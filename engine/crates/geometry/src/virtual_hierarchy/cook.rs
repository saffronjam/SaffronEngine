//! Cooking one device-independent hierarchy from format-neutral adapter input.

use std::collections::{BTreeSet, VecDeque};

use optimesh::clusterizer::{
    Meshlet, MeshletBuffers, Positions, build_meshlets, build_meshlets_bound,
};
use optimesh::meshletutils::compute_meshlet_bounds;
use optimesh::simplifier::{
    SIMPLIFY_ERROR_ABSOLUTE, SIMPLIFY_LOCK_BORDER, SIMPLIFY_REGULARIZE, SIMPLIFY_SPARSE,
    SimplifyTarget, VertexData, simplify,
};
use optimesh::vcacheoptimizer::optimize_vertex_cache;

use crate::{Error, Result};

use super::support::{
    bounds_contains, bounds_for_indexed, bounds_for_positions, bounds_for_vertex_indices,
    error_bits, fixed_bits, format_error, oct_encode, u32_len,
};
use super::types::{
    AppearanceError, GeometryPrototype, HierarchyRepresentation, PORTABLE_CLUSTER_MAX_TRIANGLES,
    PORTABLE_CLUSTER_MAX_VERTICES, PORTABLE_HIERARCHY_MAX_CHILDREN, PortableAggregationMode,
    PortableBounds, PortableClusterVertex, PortableHierarchyInput, PortableHierarchyNode,
    PortableHierarchyPage, PortableRayTracingRecord, PortableSourceMesh, PortableSourceSkin,
    PortableTriangleCluster, PortableVirtualHierarchy, VirtualHierarchyMaterial,
    VirtualMaterialClass,
};
use super::validate::validate_portable_virtual_hierarchy;
use super::voxel::{build_coarse_root_brick, build_voxel_brick};

fn validate_hierarchy_input(input: &PortableHierarchyInput) -> Result<()> {
    let mask_words = input.micro_instances.len().div_ceil(32);
    for combination in &input.combinations {
        if combination.active_words.len() != mask_words {
            return Err(format_error(
                "portable hierarchy input",
                "combination.activeWords",
            ));
        }
    }
    if input.deformation_padding < 0
        || (0..3).any(|axis| input.bounds.min_bits[axis] > input.bounds.max_bits[axis])
    {
        return Err(format_error("portable hierarchy input", "bounds"));
    }
    let mut identities = BTreeSet::new();
    for mesh in &input.meshes {
        if mesh.vertices.is_empty()
            || mesh.indices.is_empty()
            || !mesh.indices.len().is_multiple_of(3)
            || (!mesh.skin.is_empty() && mesh.skin.len() != mesh.vertices.len())
            || !identities.insert((mesh.source, mesh.selector_hash))
            || mesh
                .indices
                .iter()
                .any(|index| *index as usize >= mesh.vertices.len())
        {
            return Err(format_error("portable hierarchy input", "mesh"));
        }
        for submesh in &mesh.submeshes {
            let end = u64::from(submesh.first_index) + u64::from(submesh.index_count);
            if submesh.index_count == 0
                || !submesh.index_count.is_multiple_of(3)
                || end > mesh.indices.len() as u64
            {
                return Err(format_error("portable hierarchy input", "submesh"));
            }
        }
    }
    if input
        .micro_instances
        .iter()
        .any(|instance| instance.prototype as usize >= input.meshes.len())
        || input
            .deformation
            .windows(2)
            .any(|pair| pair[0].part >= pair[1].part)
        || input
            .deformation
            .iter()
            .any(|region| !bounds_contains(region.swept_bounds, region.static_bounds))
    {
        return Err(format_error("portable hierarchy input", "adapterRecords"));
    }
    Ok(())
}

/// Cooks the single device-independent hierarchy from format-neutral adapter input.
pub fn cook_portable_virtual_hierarchy(
    input: &PortableHierarchyInput,
) -> Result<PortableVirtualHierarchy> {
    validate_hierarchy_input(input)?;
    let padding = input.deformation_padding;
    let mut cooked = PortableVirtualHierarchy {
        micro_instances: input.micro_instances.clone(),
        combinations: input.combinations.clone(),
        deformation: input.deformation.clone(),
        ..PortableVirtualHierarchy::default()
    };
    let mut mesh_roots = Vec::new();

    for (prototype_index, mesh) in input.meshes.iter().enumerate() {
        let prototype_id = u32::try_from(prototype_index).map_err(|_| Error::NumericOverflow)?;
        let first_fine_cluster = u32_len(cooked.triangle_clusters.len())?;
        let mesh_bounds = bounds_for_positions(&mesh.vertices)?;
        let positions = mesh_positions(mesh);
        let mut submesh_roots = Vec::new();
        for (submesh_index, submesh) in mesh.submeshes.iter().enumerate() {
            let source_submesh =
                u32::try_from(submesh_index).map_err(|_| Error::NumericOverflow)?;
            let begin = usize::try_from(submesh.first_index).map_err(|_| Error::NumericOverflow)?;
            let count = usize::try_from(submesh.index_count).map_err(|_| Error::NumericOverflow)?;
            let end = begin.checked_add(count).ok_or(Error::NumericOverflow)?;
            let source_indices = mesh
                .indices
                .get(begin..end)
                .ok_or_else(|| format_error("portable hierarchy", "submesh.indices"))?;
            let source = SubmeshSource {
                mesh,
                positions: &positions,
                prototype: prototype_id,
                submesh: source_submesh,
                material: submesh.material,
                padding,
            };
            if let Some(root) = build_submesh_subtree(&mut cooked, &source, source_indices)? {
                submesh_roots.push(root);
            }
        }

        let prototype_end = u32_len(cooked.triangle_clusters.len())?;
        cooked.prototypes.push(GeometryPrototype {
            id: prototype_id,
            source: mesh.source,
            selector_hash: mesh.selector_hash,
            vertex_count: u32_len(mesh.vertices.len())?,
            submesh_count: u32_len(mesh.submeshes.len())?,
            first_cluster: first_fine_cluster,
            cluster_count: prototype_end.saturating_sub(first_fine_cluster),
            bounds: mesh_bounds,
        });
        if submesh_roots.is_empty() {
            continue;
        }
        let bounded = bound_sibling_count(&mut cooked, submesh_roots, padding)?;
        mesh_roots.push(if let [only] = bounded[..] {
            only
        } else {
            aggregate_node(&mut cooked, &bounded, padding)?
        });
    }

    let root_children = bound_sibling_count(&mut cooked, mesh_roots, padding)?;
    let family_bounds = input.bounds;
    let root_brick = build_coarse_root_brick(
        u32_len(cooked.voxel_bricks.len())?,
        family_bounds,
        input.root_material,
        padding,
    )?;
    let root_error =
        child_appearance_error(&cooked, &root_children)?.max(root_brick.appearance_error);
    let root_brick_id = root_brick.id;
    cooked.voxel_bricks.push(root_brick);
    let root_node = u32_len(cooked.nodes.len())?;
    cooked.nodes.push(PortableHierarchyNode {
        id: root_node,
        representation: HierarchyRepresentation::Voxel {
            brick: root_brick_id,
        },
        parent: None,
        children: root_children.clone(),
        page: u32::MAX,
        bounds: family_bounds,
        deformed_bounds: family_bounds.expanded(padding),
        appearance_error: root_error,
    });
    set_parent(&mut cooked.nodes, &root_children, root_node)?;
    cooked.roots.push(root_node);
    close_subtree_bounds(&mut cooked)?;
    assign_pages(&mut cooked)?;
    cooked.ray_tracing = build_ray_tracing(&cooked)?;
    validate_portable_virtual_hierarchy(&cooked)?;
    Ok(cooked)
}

/// Everything one submesh's subtree is cooked from, carried together because every level of
/// the grouping shares it.
#[derive(Clone, Copy)]
struct SubmeshSource<'a> {
    mesh: &'a PortableSourceMesh,
    /// The whole prototype's positions, unpacked once per mesh rather than per level.
    positions: &'a [f32],
    prototype: u32,
    submesh: u32,
    material: VirtualHierarchyMaterial,
    padding: i32,
}

/// One node of the level currently being grouped, with the triangles its parent simplifies
/// or voxelizes.
struct LevelNode {
    node: u32,
    /// Prototype-relative triangle indices: the node's own drawable geometry for a
    /// simplified chain, the geometry it stands in for once the chain aggregates to voxels.
    indices: Vec<u32>,
}

/// Cooks one submesh into a bounded-branching subtree and returns its root node, or `None`
/// when the submesh clusterizes to nothing.
///
/// Leaf clusters group into fixed-size sibling sets, each set collapses to one parent, and
/// the collapse repeats until a single node remains — so every node's child count stays
/// within [`PORTABLE_HIERARCHY_MAX_CHILDREN`] and the cut has a level to stop at for every
/// factor-of-four step in detail.
fn build_submesh_subtree(
    cooked: &mut PortableVirtualHierarchy,
    source: &SubmeshSource<'_>,
    source_indices: &[u32],
) -> Result<Option<u32>> {
    let leaf_start = u32_len(cooked.triangle_clusters.len())?;
    let mut leaf_clusters = build_clusters(source, source_indices, AppearanceError::default())?;
    if leaf_clusters.is_empty() {
        return Ok(None);
    }
    assign_cluster_ids(&mut leaf_clusters, leaf_start)?;
    cooked.triangle_clusters.extend(leaf_clusters);
    let leaf_end = u32_len(cooked.triangle_clusters.len())?;

    let mut level = Vec::new();
    for cluster in leaf_start..leaf_end {
        let id = u32_len(cooked.nodes.len())?;
        let payload = &cooked.triangle_clusters[cluster as usize];
        let indices = cluster_source_indices(payload)?;
        cooked.nodes.push(PortableHierarchyNode {
            id,
            representation: HierarchyRepresentation::Triangles {
                first: cluster,
                count: 1,
            },
            parent: None,
            children: Vec::new(),
            page: u32::MAX,
            bounds: payload.bounds,
            deformed_bounds: payload.deformed_bounds,
            appearance_error: AppearanceError::default(),
        });
        level.push(LevelNode { node: id, indices });
    }

    while level.len() > 1 {
        level = collapse_level(cooked, source, level)?;
    }
    level
        .first()
        .map(|root| root.node)
        .ok_or_else(|| format_error("portable hierarchy", "submesh.root"))
        .map(Some)
}

/// Collapses one level of siblings into the next coarser level.
fn collapse_level(
    cooked: &mut PortableVirtualHierarchy,
    source: &SubmeshSource<'_>,
    level: Vec<LevelNode>,
) -> Result<Vec<LevelNode>> {
    let sizes = group_sizes(level.len(), PORTABLE_HIERARCHY_MAX_CHILDREN);
    let mut members = level.into_iter();
    let mut parents = Vec::with_capacity(sizes.len());
    for size in sizes {
        let group = members.by_ref().take(size).collect::<Vec<_>>();
        let children = group.iter().map(|member| member.node).collect::<Vec<_>>();
        let indices = group
            .into_iter()
            .flat_map(|member| member.indices)
            .collect::<Vec<_>>();
        let child_error = child_appearance_error(cooked, &children)?;
        let parent = if source.mesh.aggregation == PortableAggregationMode::Disconnected {
            aggregate_group(cooked, source, &children, child_error, indices)?
        } else {
            simplify_group(cooked, source, &children, child_error, &indices)?
        };
        set_parent(&mut cooked.nodes, &children, parent.node)?;
        parents.push(parent);
    }
    Ok(parents)
}

/// Voxelizes one sibling group into an aggregate parent, for geometry too disconnected for
/// edge collapses to coarsen without dissolving it.
fn aggregate_group(
    cooked: &mut PortableVirtualHierarchy,
    source: &SubmeshSource<'_>,
    children: &[u32],
    child_error: AppearanceError,
    indices: Vec<u32>,
) -> Result<LevelNode> {
    let bounds = bounds_for_indexed(source.mesh, &indices)?;
    let brick_id = u32_len(cooked.voxel_bricks.len())?;
    let brick = build_voxel_brick(
        brick_id,
        bounds,
        source.mesh,
        &indices,
        source.material,
        source.padding,
    )?;
    let appearance_error = child_error.max(brick.appearance_error);
    cooked.voxel_bricks.push(brick);
    let node = u32_len(cooked.nodes.len())?;
    cooked.nodes.push(PortableHierarchyNode {
        id: node,
        representation: HierarchyRepresentation::Voxel { brick: brick_id },
        parent: None,
        children: children.to_vec(),
        page: u32::MAX,
        bounds,
        deformed_bounds: bounds.expanded(source.padding),
        appearance_error,
    });
    Ok(LevelNode { node, indices })
}

/// Simplifies one sibling group into a coarser triangle parent.
fn simplify_group(
    cooked: &mut PortableVirtualHierarchy,
    source: &SubmeshSource<'_>,
    children: &[u32],
    child_error: AppearanceError,
    indices: &[u32],
) -> Result<LevelNode> {
    let (simplified, silhouette_metres) =
        simplify_contiguous(source.mesh, source.positions, indices)?;
    // The collapse error is measured against this group's own geometry, which is already a
    // stand-in for everything below it, so the levels' errors add rather than replace.
    let appearance_error = AppearanceError::new(
        child_error
            .silhouette
            .saturating_add(error_bits(silhouette_metres)),
        child_error.coverage,
        child_error.transmission,
        child_error.material,
        child_error.normal_distribution,
    );
    let first = u32_len(cooked.triangle_clusters.len())?;
    let mut coarse = build_clusters(source, &simplified, appearance_error)?;
    if coarse.is_empty() {
        return Err(format_error("portable hierarchy", "coarse.clusters"));
    }
    assign_cluster_ids(&mut coarse, first)?;
    cooked.triangle_clusters.extend(coarse);
    let count = u32_len(cooked.triangle_clusters.len())?
        .checked_sub(first)
        .ok_or(Error::NumericOverflow)?;
    let bounds = bounds_for_indexed(source.mesh, &simplified)?;
    let node = u32_len(cooked.nodes.len())?;
    cooked.nodes.push(PortableHierarchyNode {
        id: node,
        representation: HierarchyRepresentation::Triangles { first, count },
        parent: None,
        children: children.to_vec(),
        page: u32::MAX,
        bounds,
        deformed_bounds: bounds.expanded(source.padding),
        appearance_error,
    });
    Ok(LevelNode {
        node,
        indices: simplified,
    })
}

/// Splits `count` siblings into as few groups as the branching bound allows, sized within
/// one of each other so no sibling is ever left in a group of its own.
fn group_sizes(count: usize, bound: usize) -> Vec<usize> {
    let groups = count.div_ceil(bound).max(1);
    let base = count / groups;
    let remainder = count % groups;
    (0..groups)
        .map(|index| base + usize::from(index < remainder))
        .collect()
}

/// The widest error any of `children` declares.
fn child_appearance_error(
    hierarchy: &PortableVirtualHierarchy,
    children: &[u32],
) -> Result<AppearanceError> {
    children
        .iter()
        .try_fold(AppearanceError::default(), |error, child| {
            Ok(error.max(
                hierarchy
                    .nodes
                    .get(*child as usize)
                    .ok_or_else(|| format_error("portable hierarchy", "node.child"))?
                    .appearance_error,
            ))
        })
}

/// Reduces `nodes` to at most [`PORTABLE_HIERARCHY_MAX_CHILDREN`] siblings by repeatedly
/// grouping them under aggregate parents, so a prototype with many submeshes — or a family
/// with many prototypes — still refines past its root.
fn bound_sibling_count(
    cooked: &mut PortableVirtualHierarchy,
    mut nodes: Vec<u32>,
    padding: i32,
) -> Result<Vec<u32>> {
    while nodes.len() > PORTABLE_HIERARCHY_MAX_CHILDREN {
        let sizes = group_sizes(nodes.len(), PORTABLE_HIERARCHY_MAX_CHILDREN);
        let mut members = nodes.into_iter();
        let mut parents = Vec::with_capacity(sizes.len());
        for size in sizes {
            let group = members.by_ref().take(size).collect::<Vec<_>>();
            parents.push(aggregate_node(cooked, &group, padding)?);
        }
        nodes = parents;
    }
    Ok(nodes)
}

fn cluster_source_indices(cluster: &PortableTriangleCluster) -> Result<Vec<u32>> {
    cluster
        .local_indices
        .iter()
        .map(|local| {
            cluster
                .source_vertices
                .get(*local as usize)
                .copied()
                .ok_or_else(|| format_error("portable hierarchy", "cluster.localIndex"))
        })
        .collect()
}

fn build_clusters(
    source: &SubmeshSource<'_>,
    source_indices: &[u32],
    appearance_error: AppearanceError,
) -> Result<Vec<PortableTriangleCluster>> {
    let SubmeshSource {
        mesh,
        positions,
        prototype,
        submesh: source_submesh,
        material,
        padding: deformation_padding,
    } = *source;
    if source_indices.is_empty() {
        return Ok(Vec::new());
    }
    if !source_indices.len().is_multiple_of(3)
        || source_indices
            .iter()
            .any(|index| *index as usize >= mesh.vertices.len())
    {
        return Err(format_error("portable hierarchy", "cluster.indices"));
    }
    let mut optimized = vec![0_u32; source_indices.len()];
    optimize_vertex_cache(&mut optimized, source_indices, mesh.vertices.len());
    let bound = build_meshlets_bound(
        optimized.len(),
        PORTABLE_CLUSTER_MAX_VERTICES,
        PORTABLE_CLUSTER_MAX_TRIANGLES,
    );
    let mut meshlets = vec![Meshlet::default(); bound];
    let mut vertices = vec![0_u32; bound * PORTABLE_CLUSTER_MAX_VERTICES];
    let mut triangles = vec![0_u8; bound * PORTABLE_CLUSTER_MAX_TRIANGLES * 3];
    let count = build_meshlets(
        &mut MeshletBuffers {
            meshlets: &mut meshlets,
            vertices: &mut vertices,
            triangles: &mut triangles,
        },
        &optimized,
        &Positions {
            data: positions,
            count: mesh.vertices.len(),
            stride: 12,
        },
        PORTABLE_CLUSTER_MAX_VERTICES,
        PORTABLE_CLUSTER_MAX_TRIANGLES,
        0.65,
    );
    let mut clusters = Vec::with_capacity(count);
    for meshlet in meshlets.into_iter().take(count) {
        let vertex_begin = meshlet.vertex_offset as usize;
        let vertex_end = vertex_begin
            .checked_add(meshlet.vertex_count as usize)
            .ok_or(Error::NumericOverflow)?;
        let triangle_begin = meshlet.triangle_offset as usize;
        let triangle_end = triangle_begin
            .checked_add(meshlet.triangle_count as usize * 3)
            .ok_or(Error::NumericOverflow)?;
        let global_vertices = vertices
            .get(vertex_begin..vertex_end)
            .ok_or_else(|| format_error("portable hierarchy", "cluster.vertices"))?;
        let local_indices = triangles
            .get(triangle_begin..triangle_end)
            .ok_or_else(|| format_error("portable hierarchy", "cluster.triangles"))?
            .to_vec();
        let bounds = bounds_for_vertex_indices(mesh, global_vertices)?;
        let quantized = global_vertices
            .iter()
            .map(|index| quantize_cluster_vertex(mesh, *index, bounds))
            .collect::<Result<Vec<_>>>()?;
        let optimesh_bounds = compute_meshlet_bounds(
            global_vertices,
            &local_indices,
            meshlet.triangle_count as usize,
            positions,
            mesh.vertices.len(),
            12,
        );
        let mut joints = BTreeSet::new();
        if mesh.skin.len() == mesh.vertices.len() {
            for &index in global_vertices {
                add_skin_joints(&mut joints, mesh.skin[index as usize]);
            }
        }
        let id = u32_len(clusters.len())?;
        clusters.push(PortableTriangleCluster {
            id,
            prototype,
            source_submesh,
            material_slot: material.slot,
            material_class: material.class,
            material_moments: material.moments,
            opacity_micromap: material.opacity_micromap,
            vertices: quantized,
            source_vertices: global_vertices.to_vec(),
            local_indices,
            bounds,
            deformed_bounds: bounds.expanded(deformation_padding),
            sphere_bits: [
                fixed_bits(optimesh_bounds.center[0]),
                fixed_bits(optimesh_bounds.center[1]),
                fixed_bits(optimesh_bounds.center[2]),
                fixed_bits(optimesh_bounds.radius.max(0.0)),
            ],
            cone: [
                optimesh_bounds.cone_axis_s8[0],
                optimesh_bounds.cone_axis_s8[1],
                optimesh_bounds.cone_axis_s8[2],
                optimesh_bounds.cone_cutoff_s8,
            ],
            deformation_joints: joints.into_iter().collect(),
            page: u32::MAX,
            appearance_error,
            parent_appearance_error: appearance_error,
        });
    }
    Ok(clusters)
}

fn assign_cluster_ids(clusters: &mut [PortableTriangleCluster], first: u32) -> Result<()> {
    for (offset, cluster) in clusters.iter_mut().enumerate() {
        cluster.id = first
            .checked_add(u32::try_from(offset).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(())
}

/// Collapses `source_indices` to a quarter of its triangles and reports the silhouette
/// deviation that cost, in local metres.
///
/// The triangle ratio is the sole stopping condition: an error budget tight enough to bind
/// returns a parent nearly as heavy as the children it stands in for, which buys the cut
/// nothing. Sparse addressing scales the reported error by the submesh's own extent rather
/// than the whole prototype's, so a small part of a large model declares the error it
/// actually has.
fn simplify_contiguous(
    mesh: &PortableSourceMesh,
    positions: &[f32],
    source_indices: &[u32],
) -> Result<(Vec<u32>, f32)> {
    let target_triangles = (source_indices.len() / 3).div_ceil(4).max(1);
    let target_index_count = target_triangles
        .checked_mul(3)
        .ok_or(Error::NumericOverflow)?;
    let mut simplified = vec![0_u32; source_indices.len()];
    let (count, error) = simplify(
        &mut simplified,
        source_indices,
        &VertexData {
            positions,
            count: mesh.vertices.len(),
            stride: 12,
        },
        &SimplifyTarget {
            target_index_count,
            target_error: NON_BINDING_SIMPLIFY_ERROR,
            options: SIMPLIFY_LOCK_BORDER
                | SIMPLIFY_REGULARIZE
                | SIMPLIFY_SPARSE
                | SIMPLIFY_ERROR_ABSOLUTE,
        },
    );
    if count == 0 || !count.is_multiple_of(3) {
        return Err(format_error("portable hierarchy", "simplification"));
    }
    simplified.truncate(count);
    Ok((simplified, error))
}

/// An absolute error budget wider than any local-space model, so only the triangle ratio
/// stops a collapse.
const NON_BINDING_SIMPLIFY_ERROR: f32 = 1.0e9;

fn mesh_positions(mesh: &PortableSourceMesh) -> Vec<f32> {
    mesh.vertices
        .iter()
        .flat_map(|vertex| {
            vertex
                .position_bits
                .map(|component| component as f32 / 65_536.0)
        })
        .collect()
}

fn quantize_cluster_vertex(
    mesh: &PortableSourceMesh,
    index: u32,
    bounds: PortableBounds,
) -> Result<PortableClusterVertex> {
    let vertex = mesh
        .vertices
        .get(index as usize)
        .ok_or_else(|| format_error("portable hierarchy", "cluster.vertexIndex"))?;
    let position_unorm = std::array::from_fn(|axis| {
        let extent = i64::from(bounds.max_bits[axis]) - i64::from(bounds.min_bits[axis]);
        if extent <= 0 {
            return 0;
        }
        let offset = i64::from(vertex.position_bits[axis]) - i64::from(bounds.min_bits[axis]);
        ((offset.clamp(0, extent) * i64::from(u16::MAX) + extent / 2) / extent) as u16
    });
    let normal = vertex
        .normal_snorm
        .map(|component| f32::from(component) / 32_767.0);
    let tangent = [
        f32::from(vertex.tangent_snorm[0]) / 32_767.0,
        f32::from(vertex.tangent_snorm[1]) / 32_767.0,
        f32::from(vertex.tangent_snorm[2]) / 32_767.0,
    ];
    Ok(PortableClusterVertex {
        position_unorm,
        normal_oct: oct_encode(normal),
        tangent_oct: oct_encode(tangent),
        tangent_handedness: if vertex.tangent_snorm[3] < 0 { -1 } else { 1 },
        uv_bits: vertex.uv_bits,
    })
}

fn add_skin_joints(joints: &mut BTreeSet<u16>, skin: PortableSourceSkin) {
    for (&joint, &weight) in skin.joints.iter().zip(&skin.weights) {
        if weight != 0 {
            joints.insert(joint);
        }
    }
}

fn set_parent(nodes: &mut [PortableHierarchyNode], children: &[u32], parent: u32) -> Result<()> {
    for child in children {
        let node = nodes
            .get_mut(*child as usize)
            .ok_or_else(|| format_error("portable hierarchy", "node.child"))?;
        if node.parent.replace(parent).is_some() {
            return Err(format_error("portable hierarchy", "node.parent"));
        }
    }
    Ok(())
}

fn aggregate_node(
    hierarchy: &mut PortableVirtualHierarchy,
    children: &[u32],
    padding: i32,
) -> Result<u32> {
    let mut bounds = hierarchy.nodes[children[0] as usize].bounds;
    let mut material = VirtualHierarchyMaterial::opaque(0);
    for child in children {
        bounds = bounds.union(hierarchy.nodes[*child as usize].bounds);
    }
    let brick_id = u32_len(hierarchy.voxel_bricks.len())?;
    material.moments.occupancy = u16::MAX;
    let brick = build_coarse_root_brick(brick_id, bounds, material, padding)?;
    let error = child_appearance_error(hierarchy, children)?.max(brick.appearance_error);
    hierarchy.voxel_bricks.push(brick);
    let node_id = u32_len(hierarchy.nodes.len())?;
    hierarchy.nodes.push(PortableHierarchyNode {
        id: node_id,
        representation: HierarchyRepresentation::Voxel { brick: brick_id },
        parent: None,
        children: children.to_vec(),
        page: u32::MAX,
        bounds,
        deformed_bounds: bounds.expanded(padding),
        appearance_error: error,
    });
    set_parent(&mut hierarchy.nodes, children, node_id)?;
    Ok(node_id)
}

/// Widens every node's static and deformed bounds until they enclose its whole subtree.
///
/// Hierarchical culling descends, so rejecting a node must also reject everything below it.
/// Simplification picks a coarse parent's bounds from the simplified geometry, which can sit
/// inside the children's silhouette, so the enclosure is established here rather than assumed.
fn close_subtree_bounds(hierarchy: &mut PortableVirtualHierarchy) -> Result<()> {
    // Pre-order, so reversing it visits every child before its parent without assuming
    // anything about node ordering.
    let mut order = Vec::with_capacity(hierarchy.nodes.len());
    let mut stack = hierarchy.roots.clone();
    let mut seen = BTreeSet::new();
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id) {
            return Err(format_error("portable hierarchy", "bounds.nodeCycle"));
        }
        let node = hierarchy
            .nodes
            .get(node_id as usize)
            .ok_or_else(|| format_error("portable hierarchy", "bounds.node"))?;
        stack.extend(node.children.iter().copied());
        order.push(node_id);
    }
    for node_id in order.into_iter().rev() {
        let node = &hierarchy.nodes[node_id as usize];
        let mut bounds = node.bounds;
        let mut deformed = node.deformed_bounds;
        for child in &node.children.clone() {
            let child = hierarchy
                .nodes
                .get(*child as usize)
                .ok_or_else(|| format_error("portable hierarchy", "bounds.child"))?;
            bounds = bounds.union(child.bounds);
            deformed = deformed.union(child.deformed_bounds);
        }
        let node = &mut hierarchy.nodes[node_id as usize];
        node.bounds = bounds;
        node.deformed_bounds = deformed;
    }
    Ok(())
}

/// The error the representation drawn in `node`'s place declares — its parent's, or its own at a
/// root.
///
/// This is what resolving `node` removes, so it is what both the page directory and the cluster
/// records price a refinement by. A node's own error answers the different question of whether to
/// refine past it, and a leaf's is legitimately zero because leaf geometry is exact.
fn parent_appearance_error(
    hierarchy: &PortableVirtualHierarchy,
    node_id: u32,
) -> Result<AppearanceError> {
    let node = hierarchy
        .nodes
        .get(node_id as usize)
        .ok_or_else(|| format_error("portable hierarchy", "node"))?;
    match node.parent {
        Some(parent) => Ok(hierarchy
            .nodes
            .get(parent as usize)
            .ok_or_else(|| format_error("portable hierarchy", "node.parent"))?
            .appearance_error),
        None => Ok(node.appearance_error),
    }
}

fn assign_pages(hierarchy: &mut PortableVirtualHierarchy) -> Result<()> {
    let mut queue = VecDeque::new();
    for root in &hierarchy.roots {
        queue.push_back((*root, None));
    }
    let mut visited = BTreeSet::new();
    while let Some((node_id, dependency)) = queue.pop_front() {
        if !visited.insert(node_id) {
            return Err(format_error("portable hierarchy", "page.nodeCycle"));
        }
        let page_id = u32_len(hierarchy.pages.len())?;
        let transition_error = parent_appearance_error(hierarchy, node_id)?;
        let node = hierarchy
            .nodes
            .get_mut(node_id as usize)
            .ok_or_else(|| format_error("portable hierarchy", "page.node"))?;
        node.page = page_id;
        let children = node.children.clone();
        hierarchy.pages.push(PortableHierarchyPage {
            id: page_id,
            dependency,
            node: node_id,
            bounds: node.bounds,
            deformed_bounds: node.deformed_bounds,
            transition_error,
            guaranteed_root: dependency.is_none(),
        });
        match node.representation {
            HierarchyRepresentation::Triangles { first, count } => {
                let end = first.checked_add(count).ok_or(Error::NumericOverflow)?;
                for cluster_id in first..end {
                    let cluster = hierarchy
                        .triangle_clusters
                        .get_mut(cluster_id as usize)
                        .ok_or_else(|| format_error("portable hierarchy", "page.cluster"))?;
                    cluster.page = page_id;
                }
            }
            HierarchyRepresentation::Voxel { brick } => {
                hierarchy
                    .voxel_bricks
                    .get_mut(brick as usize)
                    .ok_or_else(|| format_error("portable hierarchy", "page.brick"))?
                    .page = page_id;
            }
        }
        for child in children {
            queue.push_back((child, Some(page_id)));
        }
    }
    if visited.len() != hierarchy.nodes.len() {
        return Err(format_error("portable hierarchy", "page.unreachableNode"));
    }
    for node in &hierarchy.nodes {
        let parent_error = parent_appearance_error(hierarchy, node.id)?;
        if let HierarchyRepresentation::Triangles { first, count } = node.representation {
            for cluster in &mut hierarchy.triangle_clusters
                [first as usize..first.saturating_add(count) as usize]
            {
                cluster.parent_appearance_error = parent_error;
            }
        }
    }
    Ok(())
}

fn build_ray_tracing(
    hierarchy: &PortableVirtualHierarchy,
) -> Result<Vec<PortableRayTracingRecord>> {
    hierarchy
        .nodes
        .iter()
        .map(|node| {
            let (material_class, opacity_micromap) = match node.representation {
                HierarchyRepresentation::Triangles { first, .. } => {
                    let cluster = hierarchy
                        .triangle_clusters
                        .get(first as usize)
                        .ok_or_else(|| format_error("portable hierarchy", "rt.cluster"))?;
                    (cluster.material_class, cluster.opacity_micromap)
                }
                HierarchyRepresentation::Voxel { brick } => {
                    let brick = hierarchy
                        .voxel_bricks
                        .get(brick as usize)
                        .ok_or_else(|| format_error("portable hierarchy", "rt.brick"))?;
                    (brick.material_class, brick.opacity_micromap)
                }
            };
            Ok(PortableRayTracingRecord {
                node: node.id,
                material_class,
                opacity_micromap,
                requires_any_hit: matches!(
                    material_class,
                    VirtualMaterialClass::Masked | VirtualMaterialClass::ThinSheet
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_hierarchy::fixtures::{multi_page_input, uv_sphere_input};

    /// The coarsest triangle level's declared error, in Q15.16 local metres.
    fn coarsest_triangle_error(hierarchy: &PortableVirtualHierarchy) -> u32 {
        hierarchy
            .nodes
            .iter()
            .filter(|node| {
                matches!(
                    node.representation,
                    HierarchyRepresentation::Triangles { .. }
                )
            })
            .map(|node| node.appearance_error.total)
            .max()
            .expect("the sphere cooks triangle nodes")
    }

    #[test]
    fn the_declared_silhouette_error_is_metres_not_a_fraction_of_the_model() {
        // The same sphere at two sizes: eight times the model is eight times the deviation a
        // collapse costs, and the cut selector divides that by distance to get pixels. A
        // fraction of the bounding box would read identically at both sizes, so the larger
        // model would under-declare its only real LOD error eightfold.
        let small = cook_portable_virtual_hierarchy(&uv_sphere_input(0.5)).unwrap();
        let large = cook_portable_virtual_hierarchy(&uv_sphere_input(4.0)).unwrap();
        let ratio =
            f64::from(coarsest_triangle_error(&large)) / f64::from(coarsest_triangle_error(&small));
        assert!(
            (7.0..9.0).contains(&ratio),
            "an eight-times model must declare an eight-times error, got {ratio}"
        );
    }

    #[test]
    fn every_page_below_the_root_is_worth_the_error_it_removes() {
        let hierarchy = cook_portable_virtual_hierarchy(&uv_sphere_input(1.0)).unwrap();
        assert!(
            hierarchy.pages.len() > 32,
            "the fixture has to be past what one flat node could ever hold, got {}",
            hierarchy.pages.len()
        );
        for page in &hierarchy.pages {
            if page.dependency.is_none() {
                continue;
            }
            assert!(
                page.transition_error.total > 0,
                "page {} removes no error, so streaming would never ask for it",
                page.id
            );
        }
        for node in &hierarchy.nodes {
            assert!(
                node.children.len() <= PORTABLE_HIERARCHY_MAX_CHILDREN,
                "node {} fans out to {} children",
                node.id,
                node.children.len()
            );
        }

        let mut depth = vec![0_u32; hierarchy.nodes.len()];
        let mut stack = hierarchy.roots.clone();
        let mut deepest = 0;
        while let Some(node) = stack.pop() {
            deepest = deepest.max(depth[node as usize]);
            for child in &hierarchy.nodes[node as usize].children {
                depth[*child as usize] = depth[node as usize] + 1;
                stack.push(*child);
            }
        }
        assert!(
            deepest > 2,
            "bounded branching has to buy real intermediate levels, got depth {deepest}"
        );
    }

    #[test]
    fn every_cooked_node_encloses_its_subtree() {
        let hierarchy = cook_portable_virtual_hierarchy(&multi_page_input()).unwrap();
        assert!(hierarchy.nodes.len() > 2, "the tree has interior nodes");
        for node in &hierarchy.nodes {
            for child in &node.children {
                let child = &hierarchy.nodes[*child as usize];
                assert!(
                    bounds_contains(node.bounds, child.bounds),
                    "node {} must enclose child {}",
                    node.id,
                    child.id
                );
                assert!(
                    bounds_contains(node.deformed_bounds, child.deformed_bounds),
                    "node {} must sweep over child {}",
                    node.id,
                    child.id
                );
            }
        }
    }

    #[test]
    fn the_closure_widens_a_parent_that_simplification_shrank() {
        let node = |id: u32, children: Vec<u32>, half: i32| PortableHierarchyNode {
            id,
            representation: HierarchyRepresentation::Voxel { brick: 0 },
            parent: None,
            children,
            page: u32::MAX,
            bounds: PortableBounds {
                min_bits: [-half; 3],
                max_bits: [half; 3],
            },
            deformed_bounds: PortableBounds {
                min_bits: [-half; 3],
                max_bits: [half; 3],
            },
            appearance_error: AppearanceError::default(),
        };
        let mut hierarchy = PortableVirtualHierarchy {
            nodes: vec![node(0, vec![1], 65_536), node(1, Vec::new(), 262_144)],
            roots: vec![0],
            ..PortableVirtualHierarchy::default()
        };
        assert!(
            !bounds_contains(hierarchy.nodes[0].bounds, hierarchy.nodes[1].bounds),
            "the fixture starts with the child outside its parent"
        );
        close_subtree_bounds(&mut hierarchy).unwrap();
        assert_eq!(hierarchy.nodes[0].bounds.max_bits, [262_144; 3]);
        assert_eq!(hierarchy.nodes[0].deformed_bounds.min_bits, [-262_144; 3]);
        assert_eq!(
            hierarchy.nodes[1].bounds.max_bits, [262_144; 3],
            "a leaf keeps its own bounds"
        );
    }
}
