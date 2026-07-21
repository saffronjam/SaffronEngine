//! Device-independent clustered geometry, aggregate voxels, and page hierarchy cooking.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use optimesh::clusterizer::{
    Meshlet, MeshletBuffers, Positions, build_meshlets, build_meshlets_bound,
};
use optimesh::meshletutils::compute_meshlet_bounds;
use optimesh::simplifier::{
    SIMPLIFY_LOCK_BORDER, SIMPLIFY_REGULARIZE, SimplifyTarget, VertexData, simplify,
};
use optimesh::vcacheoptimizer::optimize_vertex_cache;
use saffron_spatial::{DecisionScalar, UnitInterval};

use crate::binary::{BinaryReader, BinaryWriter};
use crate::{
    AlphaClassification, Error, MaterialSurface, NormalizedPlantFamily, NormalizedPlantMesh,
    NormalizedPlantSkin, PlantFamilyAsset, PlantPartSemantic, PlantSourceRole, PlantSourceSelector,
    Result, VoxelMaterialMoments,
};

/// Portable upper bound shared by mesh and indexed-compute execution paths.
pub const PORTABLE_CLUSTER_MAX_VERTICES: usize = 64;
/// Portable primitive bound shared by mesh and indexed-compute execution paths.
pub const PORTABLE_CLUSTER_MAX_TRIANGLES: usize = 124;
/// Canonical aggregate brick edge in voxels.
pub const PORTABLE_VOXEL_BRICK_EDGE: u8 = 8;

const TRIANGLE_DOMAIN: &[u8] = b"saffron-anima/splantc/triangle-hierarchy/v1";
const VOXEL_DOMAIN: &[u8] = b"saffron-anima/splantc/voxel-hierarchy/v1";
const DEFORMATION_DOMAIN: &[u8] = b"saffron-anima/splantc/deformation/v1";
const PAGE_DOMAIN: &[u8] = b"saffron-anima/splantc/page-directory/v1";
const RAY_TRACING_DOMAIN: &[u8] = b"saffron-anima/splantc/ray-tracing/v1";

/// Stable render classification retained by clusters, voxels, PSO bins, and RT metadata.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum VirtualMaterialClass {
    /// Conventional fully covered material.
    #[default]
    Opaque = 0,
    /// Conventional canonical-coverage material.
    Masked = 1,
    /// Conventional partial-transmission material.
    Transmissive = 2,
    /// Energy-conserving two-sided thin-sheet foliage.
    ThinSheet = 3,
}

impl VirtualMaterialClass {
    fn from_tag(tag: u8, format: &'static str) -> Result<Self> {
        match tag {
            0 => Ok(Self::Opaque),
            1 => Ok(Self::Masked),
            2 => Ok(Self::Transmissive),
            3 => Ok(Self::ThinSheet),
            _ => Err(format_error(format, "materialClass")),
        }
    }
}

/// Complete material contribution needed by hierarchy, aggregate, and RT derivation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtualHierarchyMaterial {
    /// Family material slot.
    pub slot: u32,
    /// Shared surface/coverage classification.
    pub class: VirtualMaterialClass,
    /// Aggregate moments sampled by voxel representations.
    pub moments: VoxelMaterialMoments,
    /// Optional opacity-micromap derivation is permitted for RT acceleration.
    pub opacity_micromap: bool,
}

impl VirtualHierarchyMaterial {
    /// Derives the portable contract from one resolved native material surface.
    #[must_use]
    pub fn from_surface(slot: u32, surface: &MaterialSurface) -> Self {
        match surface {
            MaterialSurface::Standard => Self {
                slot,
                class: VirtualMaterialClass::Opaque,
                moments: standard_moments(),
                opacity_micromap: false,
            },
            MaterialSurface::ThinSheetFoliage(parameters) => Self {
                slot,
                class: VirtualMaterialClass::ThinSheet,
                moments: parameters.voxel_moments,
                opacity_micromap: parameters.opacity_micromap.enabled,
            },
        }
    }

    /// Overrides the standard material's canonical alpha classification.
    #[must_use]
    pub fn with_alpha_classification(mut self, classification: AlphaClassification) -> Self {
        if self.class != VirtualMaterialClass::ThinSheet {
            self.class = match classification {
                AlphaClassification::Opaque => VirtualMaterialClass::Opaque,
                AlphaClassification::Masked => VirtualMaterialClass::Masked,
                AlphaClassification::Transmissive => VirtualMaterialClass::Transmissive,
            };
        }
        self
    }
}

/// Quantized cluster-local vertex usable by mesh shaders or compute expansion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableClusterVertex {
    /// Cluster-AABB-relative unsigned normalized position.
    pub position_unorm: [u16; 3],
    /// Octahedrally encoded normal.
    pub normal_oct: [i16; 2],
    /// Octahedrally encoded tangent.
    pub tangent_oct: [i16; 2],
    /// Tangent-frame handedness.
    pub tangent_handedness: i8,
    /// Canonical Q15.16 UV0.
    pub uv_bits: [i32; 2],
}

/// Appearance-aware error used for triangle, voxel, and representation transitions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppearanceError {
    /// Geometry/silhouette error in Q15.16 local metres.
    pub silhouette: u32,
    /// Projected coverage-density error.
    pub coverage: u32,
    /// Transmitted-energy error.
    pub transmission: u32,
    /// Albedo/roughness variation error.
    pub material: u32,
    /// Normal-distribution error.
    pub normal_distribution: u32,
    /// Saturating sum used by cut selection.
    pub total: u32,
}

impl AppearanceError {
    fn new(
        silhouette: u32,
        coverage: u32,
        transmission: u32,
        material: u32,
        normal_distribution: u32,
    ) -> Self {
        Self {
            silhouette,
            coverage,
            transmission,
            material,
            normal_distribution,
            total: silhouette
                .saturating_add(coverage)
                .saturating_add(transmission)
                .saturating_add(material)
                .saturating_add(normal_distribution),
        }
    }

    fn max(self, other: Self) -> Self {
        Self::new(
            self.silhouette.max(other.silhouette),
            self.coverage.max(other.coverage),
            self.transmission.max(other.transmission),
            self.material.max(other.material),
            self.normal_distribution.max(other.normal_distribution),
        )
    }
}

/// Conservative family-local bounds in Q15.16 metres.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableBounds {
    /// Inclusive minimum.
    pub min_bits: [i32; 3],
    /// Inclusive maximum.
    pub max_bits: [i32; 3],
}

impl PortableBounds {
    fn union(self, other: Self) -> Self {
        Self {
            min_bits: std::array::from_fn(|axis| self.min_bits[axis].min(other.min_bits[axis])),
            max_bits: std::array::from_fn(|axis| self.max_bits[axis].max(other.max_bits[axis])),
        }
    }

    fn expanded(self, padding: i32) -> Self {
        Self {
            min_bits: self.min_bits.map(|value| value.saturating_sub(padding)),
            max_bits: self.max_bits.map(|value| value.saturating_add(padding)),
        }
    }
}

/// One optimized portable triangle cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableTriangleCluster {
    /// Stable cluster index.
    pub id: u32,
    /// Shared source-geometry prototype.
    pub prototype: u32,
    /// Family material slot.
    pub material_slot: u32,
    /// Shared material classification.
    pub material_class: VirtualMaterialClass,
    /// Optional OMM construction is permitted for derived RT acceleration.
    pub opacity_micromap: bool,
    /// Cluster-local quantized vertices.
    pub vertices: Vec<PortableClusterVertex>,
    /// Compact local triangle corners.
    pub local_indices: Vec<u8>,
    /// Exact source-vertex bounds.
    pub bounds: PortableBounds,
    /// Conservative structural-deformation bounds.
    pub deformed_bounds: PortableBounds,
    /// Bounding sphere center and radius in Q15.16.
    pub sphere_bits: [i32; 4],
    /// Quantized cone axis and conservative cutoff.
    pub cone: [i8; 4],
    /// Structural joint indices influencing this cluster.
    pub deformation_joints: Vec<u16>,
    /// Parent-before-child content page.
    pub page: u32,
    /// Error of this drawable representation.
    pub appearance_error: AppearanceError,
    /// Parent representation's error.
    pub parent_appearance_error: AppearanceError,
}

/// Shared normalized source mesh retained once for repeated semantic assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeometryPrototype {
    /// Stable prototype index.
    pub id: u32,
    /// Source identity.
    pub source: u128,
    /// Canonical selector identity hash.
    pub selector_hash: [u8; 32],
    /// First cluster owned by the prototype.
    pub first_cluster: u32,
    /// Number of fine clusters.
    pub cluster_count: u32,
    /// Source bounds.
    pub bounds: PortableBounds,
}

/// One semantic-part use of a shared prototype.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MicroInstance {
    /// Authored semantic part identity.
    pub part: u128,
    /// Shared prototype.
    pub prototype: u32,
    /// Q15.16 row-major local transform.
    pub transform_bits: [i32; 16],
}

/// Portable indexed aggregate voxel vertex.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortableVoxelVertex {
    /// Family-local Q15.16 position.
    pub position_bits: [i32; 3],
    /// Octahedrally encoded outward normal.
    pub normal_oct: [i16; 2],
}

/// One aggregate voxel brick plus a portable indexed-surface fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableVoxelBrick {
    /// Stable brick index.
    pub id: u32,
    /// Canonical dimensions.
    pub dimensions: [u8; 3],
    /// Brick bounds.
    pub bounds: PortableBounds,
    /// Conservative structural-deformation bounds.
    pub deformed_bounds: PortableBounds,
    /// Bit-packed occupancy, X-major then Y then Z.
    pub occupancy: Vec<u8>,
    /// Aggregate material and normal moments.
    pub moments: VoxelMaterialMoments,
    /// Shared material classification used by raster and RT expansion.
    pub material_class: VirtualMaterialClass,
    /// Optional OMM construction is permitted for derived RT acceleration.
    pub opacity_micromap: bool,
    /// Portable indexed surface vertices.
    pub vertices: Vec<PortableVoxelVertex>,
    /// Portable indexed surface triangles.
    pub indices: Vec<u32>,
    /// Parent-before-child content page.
    pub page: u32,
    /// Triangle-to-voxel appearance error.
    pub appearance_error: AppearanceError,
}

/// Drawable representation owned by one hierarchy node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HierarchyRepresentation {
    /// One or more optimized triangle clusters.
    Triangles { first: u32, count: u32 },
    /// One portable aggregate voxel brick.
    Voxel { brick: u32 },
}

/// One parent/child hierarchy node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableHierarchyNode {
    /// Stable node index.
    pub id: u32,
    /// Independently drawable payload.
    pub representation: HierarchyRepresentation,
    /// Parent node; roots have none.
    pub parent: Option<u32>,
    /// Complete child set needed for hole-free refinement.
    pub children: Vec<u32>,
    /// Parent-before-child content page.
    pub page: u32,
    /// Exact static bounds.
    pub bounds: PortableBounds,
    /// Conservative deformed bounds.
    pub deformed_bounds: PortableBounds,
    /// Appearance-aware cut error.
    pub appearance_error: AppearanceError,
}

/// One structural deformation region and its swept bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableDeformationRegion {
    /// Semantic part identity.
    pub part: u128,
    /// Structural role.
    pub semantic: PlantPartSemantic,
    /// Referenced spine/joint identities.
    pub influences: Vec<u128>,
    /// Static family-local bounds.
    pub static_bounds: PortableBounds,
    /// Conservative swept bounds.
    pub swept_bounds: PortableBounds,
}

/// One immutable content page and its parent dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortableHierarchyPage {
    /// Stable page number. Parents always have smaller numbers.
    pub id: u32,
    /// Parent page required before this page becomes visible.
    pub dependency: Option<u32>,
    /// Drawable hierarchy node stored by this page.
    pub node: u32,
    /// Static payload bounds.
    pub bounds: PortableBounds,
    /// Swept payload bounds.
    pub deformed_bounds: PortableBounds,
    /// Representation transition threshold.
    pub transition_error: AppearanceError,
    /// Root pages remain drawable under every pressure condition.
    pub guaranteed_root: bool,
}

/// RT derivation for one triangle cluster or aggregate brick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortableRayTracingRecord {
    /// Hierarchy node owning the geometry.
    pub node: u32,
    /// Shared material classification.
    pub material_class: VirtualMaterialClass,
    /// Optional OMM construction is permitted.
    pub opacity_micromap: bool,
    /// Any-hit coverage remains required when OMM is unavailable.
    pub requires_any_hit: bool,
}

/// Complete final hierarchy cook split across five independently addressable `.splantc` sections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortableVirtualHierarchy {
    /// Shared source prototypes.
    pub prototypes: Vec<GeometryPrototype>,
    /// Repeated semantic-part assembly uses.
    pub micro_instances: Vec<MicroInstance>,
    /// Optimized triangle payloads.
    pub triangle_clusters: Vec<PortableTriangleCluster>,
    /// Aggregate voxel payloads.
    pub voxel_bricks: Vec<PortableVoxelBrick>,
    /// Mixed triangle/voxel cut hierarchy.
    pub nodes: Vec<PortableHierarchyNode>,
    /// Structural deformation regions.
    pub deformation: Vec<PortableDeformationRegion>,
    /// Parent-before-child page directory.
    pub pages: Vec<PortableHierarchyPage>,
    /// Guaranteed drawable root nodes.
    pub roots: Vec<u32>,
    /// RT/coverage derivation metadata.
    pub ray_tracing: Vec<PortableRayTracingRecord>,
}

/// Cooks the device-independent virtual hierarchy from the sole normalized `.splant` source.
pub fn cook_portable_virtual_hierarchy(
    asset: &PlantFamilyAsset,
    family: &NormalizedPlantFamily,
    materials: &[VirtualHierarchyMaterial],
) -> Result<PortableVirtualHierarchy> {
    let geometry = family
        .meshes
        .iter()
        .filter(|mesh| mesh.role == PlantSourceRole::Geometry)
        .collect::<Vec<_>>();
    let material_map = materials
        .iter()
        .map(|material| (material.slot, *material))
        .collect::<BTreeMap<_, _>>();
    let padding = deformation_padding(asset);
    let mut cooked = PortableVirtualHierarchy::default();
    let mut mesh_roots = Vec::new();

    for (prototype_index, mesh) in geometry.into_iter().enumerate() {
        let prototype_id = u32::try_from(prototype_index).map_err(|_| Error::NumericOverflow)?;
        let first_fine_cluster = u32_len(cooked.triangle_clusters.len())?;
        let mesh_bounds = bounds_for_positions(&mesh.vertices)?;
        let semantic = semantic_for_source(asset, mesh.source);
        let mut submesh_roots = Vec::new();
        for submesh in &mesh.submeshes {
            let begin = usize::try_from(submesh.first_index).map_err(|_| Error::NumericOverflow)?;
            let count = usize::try_from(submesh.index_count).map_err(|_| Error::NumericOverflow)?;
            let end = begin.checked_add(count).ok_or(Error::NumericOverflow)?;
            let source_indices = mesh
                .indices
                .get(begin..end)
                .ok_or_else(|| format_error("portable hierarchy", "submesh.indices"))?;
            let material = material_map
                .get(&submesh.material_slot)
                .copied()
                .unwrap_or_else(|| VirtualHierarchyMaterial {
                    slot: submesh.material_slot,
                    ..VirtualHierarchyMaterial::default()
                });
            let leaf_start = u32_len(cooked.triangle_clusters.len())?;
            let mut leaf_clusters = build_clusters(
                mesh,
                source_indices,
                prototype_id,
                material,
                padding,
                AppearanceError::default(),
            )?;
            if leaf_clusters.is_empty() {
                continue;
            }
            assign_cluster_ids(&mut leaf_clusters, leaf_start)?;
            cooked.triangle_clusters.extend(leaf_clusters);
            let leaf_end = u32_len(cooked.triangle_clusters.len())?;
            let mut leaves = Vec::new();
            for cluster in leaf_start..leaf_end {
                let id = u32_len(cooked.nodes.len())?;
                let payload = &cooked.triangle_clusters[cluster as usize];
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
                leaves.push(id);
            }

            let root = if leaves.len() == 1 {
                leaves[0]
            } else if semantic_is_disconnected_foliage(semantic) {
                let bounds = bounds_for_indexed(mesh, source_indices)?;
                let brick_id = u32_len(cooked.voxel_bricks.len())?;
                let brick =
                    build_voxel_brick(brick_id, bounds, mesh, source_indices, material, padding)?;
                let error = brick.appearance_error;
                cooked.voxel_bricks.push(brick);
                let node_id = u32_len(cooked.nodes.len())?;
                cooked.nodes.push(PortableHierarchyNode {
                    id: node_id,
                    representation: HierarchyRepresentation::Voxel { brick: brick_id },
                    parent: None,
                    children: leaves.clone(),
                    page: u32::MAX,
                    bounds,
                    deformed_bounds: bounds.expanded(padding),
                    appearance_error: error,
                });
                set_parent(&mut cooked.nodes, &leaves, node_id)?;
                node_id
            } else {
                let (simplified, simplification_error) = simplify_contiguous(mesh, source_indices)?;
                let parent_error =
                    AppearanceError::new(error_bits(simplification_error), 0, 0, 0, 0);
                let coarse_start = u32_len(cooked.triangle_clusters.len())?;
                let mut coarse = build_clusters(
                    mesh,
                    &simplified,
                    prototype_id,
                    material,
                    padding,
                    parent_error,
                )?;
                assign_cluster_ids(&mut coarse, coarse_start)?;
                cooked.triangle_clusters.extend(coarse);
                let coarse_count = u32_len(cooked.triangle_clusters.len())?
                    .checked_sub(coarse_start)
                    .ok_or(Error::NumericOverflow)?;
                let node_id = u32_len(cooked.nodes.len())?;
                let bounds = bounds_for_indexed(mesh, &simplified)?;
                cooked.nodes.push(PortableHierarchyNode {
                    id: node_id,
                    representation: HierarchyRepresentation::Triangles {
                        first: coarse_start,
                        count: coarse_count,
                    },
                    parent: None,
                    children: leaves.clone(),
                    page: u32::MAX,
                    bounds,
                    deformed_bounds: bounds.expanded(padding),
                    appearance_error: parent_error,
                });
                set_parent(&mut cooked.nodes, &leaves, node_id)?;
                node_id
            };
            submesh_roots.push(root);
        }

        let prototype_end = u32_len(cooked.triangle_clusters.len())?;
        cooked.prototypes.push(GeometryPrototype {
            id: prototype_id,
            source: mesh.source,
            selector_hash: selector_hash(mesh),
            first_cluster: first_fine_cluster,
            cluster_count: prototype_end.saturating_sub(first_fine_cluster),
            bounds: mesh_bounds,
        });
        add_micro_instances(
            asset,
            mesh.source,
            prototype_id,
            &mut cooked.micro_instances,
        );
        if submesh_roots.is_empty() {
            continue;
        }
        mesh_roots.push(if submesh_roots.len() == 1 {
            submesh_roots[0]
        } else {
            aggregate_node(&mut cooked, &submesh_roots, padding)?
        });
    }

    let family_bounds = PortableBounds {
        min_bits: family.dimensions.local_bounds_min.map(DecisionScalar::bits),
        max_bits: family.dimensions.local_bounds_max.map(DecisionScalar::bits),
    };
    let root_material = aggregate_material(materials);
    let root_brick = build_coarse_root_brick(
        u32_len(cooked.voxel_bricks.len())?,
        family_bounds,
        root_material,
        padding,
    )?;
    let root_error = mesh_roots
        .iter()
        .try_fold(root_brick.appearance_error, |error, node| {
            Ok::<_, Error>(
                error.max(
                    cooked
                        .nodes
                        .get(*node as usize)
                        .ok_or_else(|| format_error("portable hierarchy", "root.children"))?
                        .appearance_error,
                ),
            )
        })?;
    let root_brick_id = root_brick.id;
    cooked.voxel_bricks.push(root_brick);
    let root_node = u32_len(cooked.nodes.len())?;
    cooked.nodes.push(PortableHierarchyNode {
        id: root_node,
        representation: HierarchyRepresentation::Voxel {
            brick: root_brick_id,
        },
        parent: None,
        children: mesh_roots.clone(),
        page: u32::MAX,
        bounds: family_bounds,
        deformed_bounds: family_bounds.expanded(padding),
        appearance_error: root_error,
    });
    set_parent(&mut cooked.nodes, &mesh_roots, root_node)?;
    cooked.roots.push(root_node);
    assign_pages(&mut cooked)?;
    cooked.deformation = build_deformation(asset, family_bounds, padding);
    cooked.ray_tracing = build_ray_tracing(&cooked, materials)?;
    validate_portable_virtual_hierarchy(&cooked)?;
    Ok(cooked)
}

fn build_clusters(
    mesh: &NormalizedPlantMesh,
    source_indices: &[u32],
    prototype: u32,
    material: VirtualHierarchyMaterial,
    deformation_padding: i32,
    appearance_error: AppearanceError,
) -> Result<Vec<PortableTriangleCluster>> {
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
    let positions = mesh_positions(mesh);
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
            data: &positions,
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
            &positions,
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
            material_slot: material.slot,
            material_class: material.class,
            opacity_micromap: material.opacity_micromap,
            vertices: quantized,
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

fn simplify_contiguous(
    mesh: &NormalizedPlantMesh,
    source_indices: &[u32],
) -> Result<(Vec<u32>, f32)> {
    let positions = mesh_positions(mesh);
    let target_triangles = (source_indices.len() / 3).div_ceil(4).max(1);
    let target_index_count = target_triangles
        .checked_mul(3)
        .ok_or(Error::NumericOverflow)?;
    let mut simplified = vec![0_u32; source_indices.len()];
    let (count, error) = simplify(
        &mut simplified,
        source_indices,
        &VertexData {
            positions: &positions,
            count: mesh.vertices.len(),
            stride: 12,
        },
        &SimplifyTarget {
            target_index_count,
            target_error: 1.0,
            options: SIMPLIFY_LOCK_BORDER | SIMPLIFY_REGULARIZE,
        },
    );
    if count == 0 || !count.is_multiple_of(3) {
        return Err(format_error("portable hierarchy", "simplification"));
    }
    simplified.truncate(count);
    Ok((simplified, error))
}

fn mesh_positions(mesh: &NormalizedPlantMesh) -> Vec<f32> {
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
    mesh: &NormalizedPlantMesh,
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

fn oct_encode(vector: [f32; 3]) -> [i16; 2] {
    let length = vector[0].abs() + vector[1].abs() + vector[2].abs();
    if length <= f32::EPSILON {
        return [0, 0];
    }
    let mut x = vector[0] / length;
    let mut y = vector[1] / length;
    let z = vector[2] / length;
    if z < 0.0 {
        let previous_x = x;
        x = (1.0 - y.abs()).copysign(previous_x);
        y = (1.0 - previous_x.abs()).copysign(y);
    }
    [snorm16(x), snorm16(y)]
}

fn snorm16(value: f32) -> i16 {
    (value.clamp(-1.0, 1.0) * 32_767.0).round() as i16
}

fn bounds_for_positions(vertices: &[crate::NormalizedPlantVertex]) -> Result<PortableBounds> {
    let first = vertices
        .first()
        .ok_or_else(|| format_error("portable hierarchy", "mesh.vertices"))?;
    let mut bounds = PortableBounds {
        min_bits: first.position_bits,
        max_bits: first.position_bits,
    };
    for vertex in &vertices[1..] {
        bounds = bounds.union(PortableBounds {
            min_bits: vertex.position_bits,
            max_bits: vertex.position_bits,
        });
    }
    Ok(bounds)
}

fn bounds_for_vertex_indices(
    mesh: &NormalizedPlantMesh,
    indices: &[u32],
) -> Result<PortableBounds> {
    let first = *indices
        .first()
        .ok_or_else(|| format_error("portable hierarchy", "bounds.indices"))?;
    let position = mesh
        .vertices
        .get(first as usize)
        .ok_or_else(|| format_error("portable hierarchy", "bounds.vertex"))?
        .position_bits;
    let mut bounds = PortableBounds {
        min_bits: position,
        max_bits: position,
    };
    for &index in &indices[1..] {
        let position = mesh
            .vertices
            .get(index as usize)
            .ok_or_else(|| format_error("portable hierarchy", "bounds.vertex"))?
            .position_bits;
        bounds = bounds.union(PortableBounds {
            min_bits: position,
            max_bits: position,
        });
    }
    Ok(bounds)
}

fn bounds_for_indexed(mesh: &NormalizedPlantMesh, indices: &[u32]) -> Result<PortableBounds> {
    let unique = indices.iter().copied().collect::<BTreeSet<_>>();
    bounds_for_vertex_indices(mesh, &unique.into_iter().collect::<Vec<_>>())
}

fn add_skin_joints(joints: &mut BTreeSet<u16>, skin: NormalizedPlantSkin) {
    for (&joint, &weight) in skin.joints.iter().zip(&skin.weights) {
        if weight != 0 {
            joints.insert(joint);
        }
    }
}

fn semantic_for_source(asset: &PlantFamilyAsset, source: u128) -> PlantPartSemantic {
    asset
        .parts
        .iter()
        .find(|part| part.sources.contains(&source))
        .map_or(PlantPartSemantic::Trunk, |part| part.semantic)
}

fn semantic_is_disconnected_foliage(semantic: PlantPartSemantic) -> bool {
    matches!(
        semantic,
        PlantPartSemantic::Frond
            | PlantPartSemantic::Leaf
            | PlantPartSemantic::Flower
            | PlantPartSemantic::Fruit
            | PlantPartSemantic::Blade
    )
}

fn selector_hash(mesh: &NormalizedPlantMesh) -> [u8; 32] {
    let mut bytes = b"saffron-anima/virtual-prototype/v1\0".to_vec();
    bytes.extend_from_slice(&mesh.source.to_be_bytes());
    match &mesh.selector {
        PlantSourceSelector::Whole => bytes.push(0),
        PlantSourceSelector::Element { id, path } => {
            bytes.push(1);
            bytes.extend_from_slice(&id.to_be_bytes());
            bytes.extend_from_slice(&(path.len() as u64).to_be_bytes());
            bytes.extend_from_slice(path.as_bytes());
        }
        PlantSourceSelector::Submesh { element, index } => {
            bytes.push(2);
            bytes.extend_from_slice(&element.to_be_bytes());
            bytes.extend_from_slice(&index.to_be_bytes());
        }
    }
    crate::vegetation_content_hash(&bytes)
}

fn add_micro_instances(
    asset: &PlantFamilyAsset,
    source: u128,
    prototype: u32,
    output: &mut Vec<MicroInstance>,
) {
    let identity = [
        65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536, 0, 0, 0, 0, 65_536,
    ];
    for part in asset
        .parts
        .iter()
        .filter(|part| part.sources.contains(&source))
    {
        output.push(MicroInstance {
            part: part.id,
            prototype,
            transform_bits: identity,
        });
    }
    if !output
        .iter()
        .any(|instance| instance.prototype == prototype)
    {
        output.push(MicroInstance {
            part: source,
            prototype,
            transform_bits: identity,
        });
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
    let mut material = VirtualHierarchyMaterial::from_surface(0, &MaterialSurface::Standard);
    for child in children {
        bounds = bounds.union(hierarchy.nodes[*child as usize].bounds);
    }
    let brick_id = u32_len(hierarchy.voxel_bricks.len())?;
    material.moments.occupancy = UnitInterval::ONE;
    let brick = build_coarse_root_brick(brick_id, bounds, material, padding)?;
    let error = brick.appearance_error;
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

fn build_voxel_brick(
    id: u32,
    bounds: PortableBounds,
    mesh: &NormalizedPlantMesh,
    indices: &[u32],
    material: VirtualHierarchyMaterial,
    padding: i32,
) -> Result<PortableVoxelBrick> {
    let edge = usize::from(PORTABLE_VOXEL_BRICK_EDGE);
    let voxel_count = edge * edge * edge;
    let mut occupancy = vec![0_u8; voxel_count.div_ceil(8)];
    for triangle in indices.chunks_exact(3) {
        let coordinates = triangle
            .iter()
            .map(|index| {
                mesh.vertices
                    .get(*index as usize)
                    .map(|vertex| voxel_coordinate(vertex.position_bits, bounds, edge))
                    .ok_or_else(|| format_error("portable hierarchy", "voxel.vertex"))
            })
            .collect::<Result<Vec<_>>>()?;
        let minimum: [usize; 3] = std::array::from_fn(|axis| {
            coordinates
                .iter()
                .map(|coordinate| coordinate[axis])
                .min()
                .unwrap_or(0)
        });
        let maximum: [usize; 3] = std::array::from_fn(|axis| {
            coordinates
                .iter()
                .map(|coordinate| coordinate[axis])
                .max()
                .unwrap_or(0)
        });
        for z in minimum[2]..=maximum[2] {
            for y in minimum[1]..=maximum[1] {
                for x in minimum[0]..=maximum[0] {
                    mark_voxel(&mut occupancy, [x, y, z], edge);
                }
            }
        }
    }
    dilate_occupancy(&mut occupancy, edge);
    let (vertices, surface_indices) = voxel_surface(bounds, &occupancy, edge)?;
    let occupied = occupancy.iter().map(|byte| byte.count_ones()).sum::<u32>();
    let density = UnitInterval::from_bits(
        ((u64::from(occupied) * u64::from(u16::MAX) + voxel_count as u64 / 2) / voxel_count as u64)
            as u16,
    );
    let mut aggregate = material.moments;
    aggregate.occupancy = multiply_unit(aggregate.occupancy, density);
    let error = voxel_appearance_error(bounds, aggregate, PORTABLE_VOXEL_BRICK_EDGE);
    Ok(PortableVoxelBrick {
        id,
        dimensions: [PORTABLE_VOXEL_BRICK_EDGE; 3],
        bounds,
        deformed_bounds: bounds.expanded(padding),
        occupancy,
        moments: aggregate,
        material_class: material.class,
        opacity_micromap: material.opacity_micromap,
        vertices,
        indices: surface_indices,
        page: u32::MAX,
        appearance_error: error,
    })
}

fn build_coarse_root_brick(
    id: u32,
    bounds: PortableBounds,
    material: VirtualHierarchyMaterial,
    padding: i32,
) -> Result<PortableVoxelBrick> {
    let edge = usize::from(PORTABLE_VOXEL_BRICK_EDGE);
    let mut occupancy = vec![0_u8; (edge * edge * edge).div_ceil(8)];
    occupancy.fill(u8::MAX);
    let (vertices, indices) = voxel_surface(bounds, &occupancy, edge)?;
    let aggregate = material.moments;
    Ok(PortableVoxelBrick {
        id,
        dimensions: [PORTABLE_VOXEL_BRICK_EDGE; 3],
        bounds,
        deformed_bounds: bounds.expanded(padding),
        occupancy,
        moments: aggregate,
        material_class: material.class,
        opacity_micromap: material.opacity_micromap,
        vertices,
        indices,
        page: u32::MAX,
        appearance_error: voxel_appearance_error(bounds, aggregate, PORTABLE_VOXEL_BRICK_EDGE),
    })
}

fn multiply_unit(first: UnitInterval, second: UnitInterval) -> UnitInterval {
    UnitInterval::from_bits(
        ((u32::from(first.bits()) * u32::from(second.bits()) + u32::from(u16::MAX) / 2)
            / u32::from(u16::MAX)) as u16,
    )
}

fn voxel_coordinate(position: [i32; 3], bounds: PortableBounds, edge: usize) -> [usize; 3] {
    std::array::from_fn(|axis| {
        let extent = i64::from(bounds.max_bits[axis]) - i64::from(bounds.min_bits[axis]);
        if extent <= 0 {
            return 0;
        }
        let offset = i64::from(position[axis]) - i64::from(bounds.min_bits[axis]);
        usize::try_from((offset.clamp(0, extent) * (edge as i64 - 1)) / extent).unwrap_or(0)
    })
}

fn mark_voxel(occupancy: &mut [u8], coordinate: [usize; 3], edge: usize) {
    let index = coordinate[0] + edge * (coordinate[1] + edge * coordinate[2]);
    occupancy[index / 8] |= 1 << (index % 8);
}

fn voxel_occupied(occupancy: &[u8], coordinate: [usize; 3], edge: usize) -> bool {
    let index = coordinate[0] + edge * (coordinate[1] + edge * coordinate[2]);
    occupancy[index / 8] & (1 << (index % 8)) != 0
}

fn dilate_occupancy(occupancy: &mut [u8], edge: usize) {
    let source = occupancy.to_vec();
    for z in 0..edge {
        for y in 0..edge {
            for x in 0..edge {
                if !voxel_occupied(&source, [x, y, z], edge) {
                    continue;
                }
                for [dx, dy, dz] in [
                    [-1_i32, 0, 0],
                    [1, 0, 0],
                    [0, -1, 0],
                    [0, 1, 0],
                    [0, 0, -1],
                    [0, 0, 1],
                ] {
                    let next = [x as i32 + dx, y as i32 + dy, z as i32 + dz];
                    if next.iter().all(|value| (0..edge as i32).contains(value)) {
                        mark_voxel(
                            occupancy,
                            [next[0] as usize, next[1] as usize, next[2] as usize],
                            edge,
                        );
                    }
                }
            }
        }
    }
}

fn voxel_surface(
    bounds: PortableBounds,
    occupancy: &[u8],
    edge: usize,
) -> Result<(Vec<PortableVoxelVertex>, Vec<u32>)> {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for z in 0..edge {
        for y in 0..edge {
            for x in 0..edge {
                if !voxel_occupied(occupancy, [x, y, z], edge) {
                    continue;
                }
                for (axis, direction) in
                    [(0_usize, -1_i32), (0, 1), (1, -1), (1, 1), (2, -1), (2, 1)]
                {
                    let mut neighbor = [x as i32, y as i32, z as i32];
                    neighbor[axis] += direction;
                    let exposed = neighbor
                        .iter()
                        .any(|coordinate| !(0..edge as i32).contains(coordinate))
                        || !voxel_occupied(
                            occupancy,
                            [
                                neighbor[0].clamp(0, edge as i32 - 1) as usize,
                                neighbor[1].clamp(0, edge as i32 - 1) as usize,
                                neighbor[2].clamp(0, edge as i32 - 1) as usize,
                            ],
                            edge,
                        );
                    if exposed {
                        append_voxel_face(
                            &mut vertices,
                            &mut indices,
                            bounds,
                            [x, y, z],
                            edge,
                            axis,
                            direction,
                        )?;
                    }
                }
            }
        }
    }
    if indices.is_empty() {
        return Ok(box_surface(bounds));
    }
    Ok((vertices, indices))
}

fn append_voxel_face(
    vertices: &mut Vec<PortableVoxelVertex>,
    indices: &mut Vec<u32>,
    bounds: PortableBounds,
    voxel: [usize; 3],
    edge: usize,
    axis: usize,
    direction: i32,
) -> Result<()> {
    let tangent_axes = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    let plane = voxel[axis] + usize::from(direction > 0);
    let corners = [[0_usize, 0_usize], [1, 0], [1, 1], [0, 1]];
    let first = u32_len(vertices.len())?;
    let mut normal = [0.0_f32; 3];
    normal[axis] = direction as f32;
    for corner in corners {
        let mut grid = voxel;
        grid[axis] = plane;
        grid[tangent_axes[0]] += corner[0];
        grid[tangent_axes[1]] += corner[1];
        let position_bits = std::array::from_fn(|component| {
            let min = i64::from(bounds.min_bits[component]);
            let extent = i64::from(bounds.max_bits[component]) - min;
            let numerator = i64::try_from(grid[component]).unwrap_or(0);
            (min + (extent * numerator + edge as i64 / 2) / edge as i64) as i32
        });
        vertices.push(PortableVoxelVertex {
            position_bits,
            normal_oct: oct_encode(normal),
        });
    }
    if direction > 0 {
        indices.extend_from_slice(&[first, first + 1, first + 2, first, first + 2, first + 3]);
    } else {
        indices.extend_from_slice(&[first, first + 2, first + 1, first, first + 3, first + 2]);
    }
    Ok(())
}

fn box_surface(bounds: PortableBounds) -> (Vec<PortableVoxelVertex>, Vec<u32>) {
    let positions = [
        [0, 0, 0],
        [1, 0, 0],
        [1, 1, 0],
        [0, 1, 0],
        [0, 0, 1],
        [1, 0, 1],
        [1, 1, 1],
        [0, 1, 1],
    ];
    let vertices = positions
        .into_iter()
        .map(|corner| PortableVoxelVertex {
            position_bits: std::array::from_fn(|axis| {
                if corner[axis] == 0 {
                    bounds.min_bits[axis]
                } else {
                    bounds.max_bits[axis]
                }
            }),
            normal_oct: [0, 0],
        })
        .collect();
    let indices = vec![
        0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7, 3,
        1, 2, 6, 1, 6, 5,
    ];
    (vertices, indices)
}

fn voxel_appearance_error(
    bounds: PortableBounds,
    moments: VoxelMaterialMoments,
    edge: u8,
) -> AppearanceError {
    let maximum_extent = (0..3)
        .map(|axis| {
            i64::from(bounds.max_bits[axis])
                .saturating_sub(i64::from(bounds.min_bits[axis]))
                .unsigned_abs()
        })
        .max()
        .unwrap_or_default();
    let silhouette = u32::try_from(maximum_extent / u64::from(edge)).unwrap_or(u32::MAX);
    let coverage = u32::from(u16::MAX.saturating_sub(moments.occupancy.bits()));
    let transmission = moments
        .transmission_mean
        .iter()
        .map(|value| value.bits().unsigned_abs())
        .max()
        .unwrap_or_default();
    let material = u32::from(moments.roughness_mean.bits()) / u32::from(edge);
    let normal_distribution = moments
        .normal_second_moments
        .iter()
        .map(|value| value.bits().unsigned_abs())
        .max()
        .unwrap_or_default()
        / u32::from(edge);
    AppearanceError::new(
        silhouette,
        coverage,
        transmission,
        material,
        normal_distribution,
    )
}

fn deformation_padding(asset: &PlantFamilyAsset) -> i32 {
    let extent = (0..3)
        .map(|axis| {
            i64::from(asset.dimensions.local_bounds_max[axis].bits())
                .saturating_sub(i64::from(asset.dimensions.local_bounds_min[axis].bits()))
                .unsigned_abs()
        })
        .max()
        .unwrap_or_default();
    let bend = u64::from(asset.mechanics.bend_limit.bits());
    i32::try_from(extent.saturating_mul(bend) / u64::from(u16::MAX) / 2).unwrap_or(i32::MAX)
}

fn build_deformation(
    asset: &PlantFamilyAsset,
    family_bounds: PortableBounds,
    padding: i32,
) -> Vec<PortableDeformationRegion> {
    let mut regions = asset
        .parts
        .iter()
        .map(|part| PortableDeformationRegion {
            part: part.id,
            semantic: part.semantic,
            influences: asset
                .spines
                .iter()
                .filter(|spine| spine.part == part.id)
                .map(|spine| spine.id)
                .collect(),
            static_bounds: family_bounds,
            swept_bounds: family_bounds.expanded(padding),
        })
        .collect::<Vec<_>>();
    for region in &mut regions {
        region.influences.sort_unstable();
        region.influences.dedup();
    }
    regions.sort_unstable_by_key(|region| region.part);
    regions
}

fn aggregate_material(materials: &[VirtualHierarchyMaterial]) -> VirtualHierarchyMaterial {
    if materials.is_empty() {
        return VirtualHierarchyMaterial::from_surface(0, &MaterialSurface::Standard);
    }
    let count = i64::try_from(materials.len()).unwrap_or(i64::MAX).max(1);
    let average_decision = |values: Vec<i32>| {
        DecisionScalar::from_bits(
            i32::try_from(values.into_iter().map(i64::from).sum::<i64>() / count)
                .unwrap_or_default(),
        )
    };
    let average_unit = |value: fn(&VoxelMaterialMoments) -> u16| {
        UnitInterval::from_bits(
            u16::try_from(
                materials
                    .iter()
                    .map(|material| u64::from(value(&material.moments)))
                    .sum::<u64>()
                    / materials.len() as u64,
            )
            .unwrap_or_default(),
        )
    };
    let moments = VoxelMaterialMoments {
        occupancy: average_unit(|moments| moments.occupancy.bits()),
        albedo_mean: std::array::from_fn(|axis| {
            average_decision(
                materials
                    .iter()
                    .map(|material| material.moments.albedo_mean[axis].bits())
                    .collect(),
            )
        }),
        roughness_mean: average_unit(|moments| moments.roughness_mean.bits()),
        transmission_mean: std::array::from_fn(|axis| {
            average_decision(
                materials
                    .iter()
                    .map(|material| material.moments.transmission_mean[axis].bits())
                    .collect(),
            )
        }),
        thickness_mean: average_decision(
            materials
                .iter()
                .map(|material| material.moments.thickness_mean.bits())
                .collect(),
        ),
        normal_second_moments: std::array::from_fn(|axis| {
            average_decision(
                materials
                    .iter()
                    .map(|material| material.moments.normal_second_moments[axis].bits())
                    .collect(),
            )
        }),
    };
    let class = materials
        .iter()
        .map(|material| material.class)
        .max()
        .unwrap_or_default();
    VirtualHierarchyMaterial {
        slot: 0,
        class,
        moments,
        opacity_micromap: materials.iter().any(|material| material.opacity_micromap),
    }
}

fn standard_moments() -> VoxelMaterialMoments {
    VoxelMaterialMoments {
        occupancy: UnitInterval::ONE,
        albedo_mean: [DecisionScalar::from_bits(65_536); 3],
        roughness_mean: UnitInterval::ONE,
        transmission_mean: [DecisionScalar::from_bits(0); 3],
        thickness_mean: DecisionScalar::from_bits(0),
        normal_second_moments: [
            DecisionScalar::from_bits(21_845),
            DecisionScalar::from_bits(21_845),
            DecisionScalar::from_bits(21_846),
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(0),
            DecisionScalar::from_bits(0),
        ],
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
            transition_error: node.appearance_error,
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
        let parent_error = node.parent.map_or(node.appearance_error, |parent| {
            hierarchy.nodes[parent as usize].appearance_error
        });
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
    materials: &[VirtualHierarchyMaterial],
) -> Result<Vec<PortableRayTracingRecord>> {
    let by_slot = materials
        .iter()
        .map(|material| (material.slot, *material))
        .collect::<BTreeMap<_, _>>();
    hierarchy
        .nodes
        .iter()
        .map(|node| {
            let material = match node.representation {
                HierarchyRepresentation::Triangles { first, .. } => {
                    let cluster = hierarchy
                        .triangle_clusters
                        .get(first as usize)
                        .ok_or_else(|| format_error("portable hierarchy", "rt.cluster"))?;
                    by_slot.get(&cluster.material_slot).copied().unwrap_or(
                        VirtualHierarchyMaterial {
                            slot: cluster.material_slot,
                            class: cluster.material_class,
                            opacity_micromap: cluster.opacity_micromap,
                            ..VirtualHierarchyMaterial::default()
                        },
                    )
                }
                HierarchyRepresentation::Voxel { brick } => {
                    let brick = hierarchy
                        .voxel_bricks
                        .get(brick as usize)
                        .ok_or_else(|| format_error("portable hierarchy", "rt.brick"))?;
                    VirtualHierarchyMaterial {
                        class: brick.material_class,
                        opacity_micromap: brick.opacity_micromap,
                        ..VirtualHierarchyMaterial::default()
                    }
                }
            };
            Ok(PortableRayTracingRecord {
                node: node.id,
                material_class: material.class,
                opacity_micromap: material.opacity_micromap,
                requires_any_hit: matches!(
                    material.class,
                    VirtualMaterialClass::Masked | VirtualMaterialClass::ThinSheet
                ),
            })
        })
        .collect()
}

/// Validates hierarchy topology, compact limits, conservative bounds, page order, and roots.
pub fn validate_portable_virtual_hierarchy(hierarchy: &PortableVirtualHierarchy) -> Result<()> {
    if hierarchy.roots.is_empty() || hierarchy.pages.is_empty() {
        return Err(format_error("portable hierarchy", "roots"));
    }
    if hierarchy
        .triangle_clusters
        .iter()
        .enumerate()
        .any(|(id, cluster)| {
            cluster.id as usize != id
                || cluster.vertices.is_empty()
                || cluster.vertices.len() > PORTABLE_CLUSTER_MAX_VERTICES
                || cluster.local_indices.is_empty()
                || cluster.local_indices.len() / 3 > PORTABLE_CLUSTER_MAX_TRIANGLES
                || !cluster.local_indices.len().is_multiple_of(3)
                || cluster
                    .local_indices
                    .iter()
                    .any(|index| *index as usize >= cluster.vertices.len())
                || !bounds_contains(cluster.deformed_bounds, cluster.bounds)
                || cluster.page as usize >= hierarchy.pages.len()
        })
    {
        return Err(format_error("portable hierarchy", "triangleClusters"));
    }
    for (id, brick) in hierarchy.voxel_bricks.iter().enumerate() {
        let voxel_count = brick
            .dimensions
            .into_iter()
            .try_fold(1_usize, |total, value| {
                total.checked_mul(usize::from(value))
            });
        if brick.id as usize != id
            || voxel_count.is_none_or(|count| brick.occupancy.len() != count.div_ceil(8))
            || brick.indices.is_empty()
            || !brick.indices.len().is_multiple_of(3)
            || brick
                .indices
                .iter()
                .any(|index| *index as usize >= brick.vertices.len())
            || !bounds_contains(brick.deformed_bounds, brick.bounds)
            || brick.page as usize >= hierarchy.pages.len()
        {
            return Err(format_error("portable hierarchy", "voxelBricks"));
        }
    }
    for (id, node) in hierarchy.nodes.iter().enumerate() {
        if node.id as usize != id
            || node.page as usize >= hierarchy.pages.len()
            || !bounds_contains(node.deformed_bounds, node.bounds)
            || node.children.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(format_error("portable hierarchy", "nodes"));
        }
        match node.representation {
            HierarchyRepresentation::Triangles { first, count } => {
                if count == 0
                    || first
                        .checked_add(count)
                        .is_none_or(|end| end as usize > hierarchy.triangle_clusters.len())
                {
                    return Err(format_error("portable hierarchy", "node.triangles"));
                }
            }
            HierarchyRepresentation::Voxel { brick }
                if brick as usize >= hierarchy.voxel_bricks.len() =>
            {
                return Err(format_error("portable hierarchy", "node.voxel"));
            }
            HierarchyRepresentation::Voxel { .. } => {}
        }
        for child in &node.children {
            let child_node = hierarchy
                .nodes
                .get(*child as usize)
                .ok_or_else(|| format_error("portable hierarchy", "node.child"))?;
            if child_node.parent != Some(node.id)
                || child_node.appearance_error.total > node.appearance_error.total
            {
                return Err(format_error("portable hierarchy", "node.refinement"));
            }
        }
    }
    for (id, page) in hierarchy.pages.iter().enumerate() {
        if page.id as usize != id
            || page.node as usize >= hierarchy.nodes.len()
            || hierarchy.nodes[page.node as usize].page != page.id
            || page.dependency.is_some_and(|parent| parent >= page.id)
            || page.guaranteed_root != page.dependency.is_none()
        {
            return Err(format_error("portable hierarchy", "pages"));
        }
    }
    for root in &hierarchy.roots {
        let node = hierarchy
            .nodes
            .get(*root as usize)
            .ok_or_else(|| format_error("portable hierarchy", "root.node"))?;
        let page = &hierarchy.pages[node.page as usize];
        if node.parent.is_some()
            || !page.guaranteed_root
            || !matches!(node.representation, HierarchyRepresentation::Voxel { .. })
        {
            return Err(format_error("portable hierarchy", "root.drawable"));
        }
    }
    Ok(())
}

/// Selects a hole-free CPU reference cut from complete-child residency and appearance error.
pub fn select_portable_hierarchy_cut(
    hierarchy: &PortableVirtualHierarchy,
    resident_pages: &BTreeSet<u32>,
    maximum_error: u32,
) -> Result<Vec<u32>> {
    validate_portable_virtual_hierarchy(hierarchy)?;
    let mut cut = Vec::new();
    for root in &hierarchy.roots {
        select_node_cut(hierarchy, *root, resident_pages, maximum_error, &mut cut)?;
    }
    cut.sort_unstable();
    Ok(cut)
}

fn select_node_cut(
    hierarchy: &PortableVirtualHierarchy,
    node_id: u32,
    resident_pages: &BTreeSet<u32>,
    maximum_error: u32,
    cut: &mut Vec<u32>,
) -> Result<()> {
    let node = &hierarchy.nodes[node_id as usize];
    if !resident_pages.contains(&node.page) {
        return Err(format_error("portable hierarchy cut", "rootResidency"));
    }
    let refine = node.appearance_error.total > maximum_error
        && !node.children.is_empty()
        && node
            .children
            .iter()
            .all(|child| resident_pages.contains(&hierarchy.nodes[*child as usize].page));
    if refine {
        for child in &node.children {
            select_node_cut(hierarchy, *child, resident_pages, maximum_error, cut)?;
        }
    } else {
        cut.push(node_id);
    }
    Ok(())
}

fn bounds_contains(outer: PortableBounds, inner: PortableBounds) -> bool {
    (0..3).all(|axis| {
        outer.min_bits[axis] <= inner.min_bits[axis] && outer.max_bits[axis] >= inner.max_bits[axis]
    })
}

fn fixed_bits(value: f32) -> i32 {
    (value.clamp(-32_768.0, 32_767.999_984_74) * 65_536.0).round() as i32
}

fn error_bits(value: f32) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        (value.min(65_535.0) * 65_536.0).round() as u32
    }
}

fn u32_len(length: usize) -> Result<u32> {
    u32::try_from(length).map_err(|_| Error::NumericOverflow)
}

fn format_error(format: &'static str, field: &str) -> Error {
    Error::ArtifactFormat {
        format,
        field: field.to_owned(),
    }
}

/// Decoded triangle-hierarchy section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriangleHierarchySection {
    /// Shared prototypes.
    pub prototypes: Vec<GeometryPrototype>,
    /// Semantic micro-instances.
    pub micro_instances: Vec<MicroInstance>,
    /// Portable clusters.
    pub clusters: Vec<PortableTriangleCluster>,
}

/// Decoded aggregate-voxel section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoxelHierarchySection {
    /// Aggregate bricks and indexed surfaces.
    pub bricks: Vec<PortableVoxelBrick>,
}

/// Decoded structural-deformation section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeformationSection {
    /// Structural deformation regions.
    pub regions: Vec<PortableDeformationRegion>,
}

/// Decoded page-directory and mixed hierarchy section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageDirectorySection {
    /// Mixed hierarchy nodes.
    pub nodes: Vec<PortableHierarchyNode>,
    /// Parent-before-child pages.
    pub pages: Vec<PortableHierarchyPage>,
    /// Guaranteed coarse roots.
    pub roots: Vec<u32>,
}

/// Decoded RT derivation section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RayTracingSection {
    /// Per-node RT/coverage records.
    pub records: Vec<PortableRayTracingRecord>,
}

impl PortableVirtualHierarchy {
    /// Encodes optimized clusters, prototypes, and assembly tables canonically.
    pub fn triangle_hierarchy_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = section_writer(TRIANGLE_DOMAIN)?;
        writer.length(self.prototypes.len())?;
        for prototype in &self.prototypes {
            writer.u32(prototype.id);
            writer.u128(prototype.source);
            writer.bytes(&prototype.selector_hash);
            writer.u32(prototype.first_cluster);
            writer.u32(prototype.cluster_count);
            write_bounds(&mut writer, prototype.bounds);
        }
        writer.length(self.micro_instances.len())?;
        for instance in &self.micro_instances {
            writer.u128(instance.part);
            writer.u32(instance.prototype);
            for value in instance.transform_bits {
                writer.i32(value);
            }
        }
        writer.length(self.triangle_clusters.len())?;
        for cluster in &self.triangle_clusters {
            writer.u32(cluster.id);
            writer.u32(cluster.prototype);
            writer.u32(cluster.material_slot);
            writer.u8(cluster.material_class as u8);
            writer.bool(cluster.opacity_micromap);
            write_bounds(&mut writer, cluster.bounds);
            write_bounds(&mut writer, cluster.deformed_bounds);
            for value in cluster.sphere_bits {
                writer.i32(value);
            }
            writer.bytes(&cluster.cone.map(|value| value as u8));
            writer.u32(cluster.page);
            write_error(&mut writer, cluster.appearance_error);
            write_error(&mut writer, cluster.parent_appearance_error);
            writer.length(cluster.deformation_joints.len())?;
            for joint in &cluster.deformation_joints {
                writer.u16(*joint);
            }
            writer.length(cluster.vertices.len())?;
            for vertex in &cluster.vertices {
                for value in vertex.position_unorm {
                    writer.u16(value);
                }
                for value in vertex.normal_oct {
                    writer.u16(value as u16);
                }
                for value in vertex.tangent_oct {
                    writer.u16(value as u16);
                }
                writer.u8(vertex.tangent_handedness as u8);
                for value in vertex.uv_bits {
                    writer.i32(value);
                }
            }
            writer.length(cluster.local_indices.len())?;
            writer.bytes(&cluster.local_indices);
        }
        Ok(writer.finish())
    }

    /// Encodes aggregate voxels and portable indexed surfaces canonically.
    pub fn voxel_hierarchy_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = section_writer(VOXEL_DOMAIN)?;
        writer.length(self.voxel_bricks.len())?;
        for brick in &self.voxel_bricks {
            writer.u32(brick.id);
            writer.bytes(&brick.dimensions);
            write_bounds(&mut writer, brick.bounds);
            write_bounds(&mut writer, brick.deformed_bounds);
            writer.u32(brick.page);
            write_error(&mut writer, brick.appearance_error);
            writer.u8(brick.material_class as u8);
            writer.bool(brick.opacity_micromap);
            write_moments(&mut writer, brick.moments);
            writer.length(brick.occupancy.len())?;
            writer.bytes(&brick.occupancy);
            writer.length(brick.vertices.len())?;
            for vertex in &brick.vertices {
                for value in vertex.position_bits {
                    writer.i32(value);
                }
                for value in vertex.normal_oct {
                    writer.u16(value as u16);
                }
            }
            writer.length(brick.indices.len())?;
            for index in &brick.indices {
                writer.u32(*index);
            }
        }
        Ok(writer.finish())
    }

    /// Encodes structural modes and swept bounds canonically.
    pub fn deformation_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = section_writer(DEFORMATION_DOMAIN)?;
        writer.length(self.deformation.len())?;
        for region in &self.deformation {
            writer.u128(region.part);
            writer.u8(semantic_tag(region.semantic));
            writer.length(region.influences.len())?;
            for influence in &region.influences {
                writer.u128(*influence);
            }
            write_bounds(&mut writer, region.static_bounds);
            write_bounds(&mut writer, region.swept_bounds);
        }
        Ok(writer.finish())
    }

    /// Encodes the mixed hierarchy and parent-before-child page directory canonically.
    pub fn page_directory_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = section_writer(PAGE_DOMAIN)?;
        writer.length(self.nodes.len())?;
        for node in &self.nodes {
            writer.u32(node.id);
            match node.representation {
                HierarchyRepresentation::Triangles { first, count } => {
                    writer.u8(0);
                    writer.u32(first);
                    writer.u32(count);
                }
                HierarchyRepresentation::Voxel { brick } => {
                    writer.u8(1);
                    writer.u32(brick);
                    writer.u32(0);
                }
            }
            write_optional_u32(&mut writer, node.parent);
            writer.length(node.children.len())?;
            for child in &node.children {
                writer.u32(*child);
            }
            writer.u32(node.page);
            write_bounds(&mut writer, node.bounds);
            write_bounds(&mut writer, node.deformed_bounds);
            write_error(&mut writer, node.appearance_error);
        }
        writer.length(self.pages.len())?;
        for page in &self.pages {
            writer.u32(page.id);
            write_optional_u32(&mut writer, page.dependency);
            writer.u32(page.node);
            write_bounds(&mut writer, page.bounds);
            write_bounds(&mut writer, page.deformed_bounds);
            write_error(&mut writer, page.transition_error);
            writer.bool(page.guaranteed_root);
        }
        writer.length(self.roots.len())?;
        for root in &self.roots {
            writer.u32(*root);
        }
        Ok(writer.finish())
    }

    /// Encodes RT and canonical-coverage derivation metadata canonically.
    pub fn ray_tracing_bytes(&self) -> Result<Vec<u8>> {
        let mut writer = section_writer(RAY_TRACING_DOMAIN)?;
        writer.length(self.ray_tracing.len())?;
        for record in &self.ray_tracing {
            writer.u32(record.node);
            writer.u8(record.material_class as u8);
            writer.bool(record.opacity_micromap);
            writer.bool(record.requires_any_hit);
        }
        Ok(writer.finish())
    }
}

/// Strictly decodes the portable triangle hierarchy.
pub fn decode_triangle_hierarchy(bytes: &[u8]) -> Result<TriangleHierarchySection> {
    let mut reader = section_reader(bytes, TRIANGLE_DOMAIN, ".splantc triangle hierarchy")?;
    let prototype_count = reader.count(60)?;
    let mut prototypes = Vec::with_capacity(prototype_count);
    for expected in 0..prototype_count {
        let prototype = GeometryPrototype {
            id: reader.u32()?,
            source: reader.u128()?,
            selector_hash: reader.array()?,
            first_cluster: reader.u32()?,
            cluster_count: reader.u32()?,
            bounds: read_bounds(&mut reader)?,
        };
        if prototype.id as usize != expected {
            return Err(format_error(".splantc triangle hierarchy", "prototype.id"));
        }
        prototypes.push(prototype);
    }
    let instance_count = reader.count(84)?;
    let mut micro_instances = Vec::with_capacity(instance_count);
    for _ in 0..instance_count {
        let part = reader.u128()?;
        let prototype = reader.u32()?;
        let mut transform_bits = [0_i32; 16];
        for value in &mut transform_bits {
            *value = reader.i32()?;
        }
        if prototype as usize >= prototypes.len() {
            return Err(format_error(
                ".splantc triangle hierarchy",
                "instance.prototype",
            ));
        }
        micro_instances.push(MicroInstance {
            part,
            prototype,
            transform_bits,
        });
    }
    let cluster_count = reader.count(112)?;
    let mut clusters = Vec::with_capacity(cluster_count);
    for expected in 0..cluster_count {
        let id = reader.u32()?;
        let prototype = reader.u32()?;
        let material_slot = reader.u32()?;
        let material_class =
            VirtualMaterialClass::from_tag(reader.u8()?, ".splantc triangle hierarchy")?;
        let opacity_micromap = reader.bool()?;
        let bounds = read_bounds(&mut reader)?;
        let deformed_bounds = read_bounds(&mut reader)?;
        let mut sphere_bits = [0_i32; 4];
        for value in &mut sphere_bits {
            *value = reader.i32()?;
        }
        let cone = reader.array::<4>()?.map(|value| value as i8);
        let page = reader.u32()?;
        let appearance_error = read_error(&mut reader)?;
        let parent_appearance_error = read_error(&mut reader)?;
        let joint_count = reader.count(2)?;
        let mut deformation_joints = Vec::with_capacity(joint_count);
        for _ in 0..joint_count {
            deformation_joints.push(reader.u16()?);
        }
        if deformation_joints.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(format_error(
                ".splantc triangle hierarchy",
                "cluster.joints",
            ));
        }
        let vertex_count = reader.count(23)?;
        if vertex_count == 0 || vertex_count > PORTABLE_CLUSTER_MAX_VERTICES {
            return Err(format_error(
                ".splantc triangle hierarchy",
                "cluster.vertices",
            ));
        }
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            vertices.push(PortableClusterVertex {
                position_unorm: [reader.u16()?, reader.u16()?, reader.u16()?],
                normal_oct: [reader.u16()? as i16, reader.u16()? as i16],
                tangent_oct: [reader.u16()? as i16, reader.u16()? as i16],
                tangent_handedness: reader.u8()? as i8,
                uv_bits: [reader.i32()?, reader.i32()?],
            });
        }
        let index_count = reader.length()?;
        if index_count == 0
            || !index_count.is_multiple_of(3)
            || index_count / 3 > PORTABLE_CLUSTER_MAX_TRIANGLES
        {
            return Err(format_error(
                ".splantc triangle hierarchy",
                "cluster.indices",
            ));
        }
        let local_indices = reader.take(index_count)?.to_vec();
        if id as usize != expected
            || prototype as usize >= prototypes.len()
            || local_indices
                .iter()
                .any(|index| *index as usize >= vertices.len())
            || !bounds_contains(deformed_bounds, bounds)
        {
            return Err(format_error(".splantc triangle hierarchy", "cluster"));
        }
        clusters.push(PortableTriangleCluster {
            id,
            prototype,
            material_slot,
            material_class,
            opacity_micromap,
            vertices,
            local_indices,
            bounds,
            deformed_bounds,
            sphere_bits,
            cone,
            deformation_joints,
            page,
            appearance_error,
            parent_appearance_error,
        });
    }
    reader.complete()?;
    for prototype in &prototypes {
        if prototype
            .first_cluster
            .checked_add(prototype.cluster_count)
            .is_none_or(|end| end as usize > clusters.len())
        {
            return Err(format_error(
                ".splantc triangle hierarchy",
                "prototype.clusters",
            ));
        }
    }
    Ok(TriangleHierarchySection {
        prototypes,
        micro_instances,
        clusters,
    })
}

/// Strictly decodes aggregate voxel bricks and portable indexed surfaces.
pub fn decode_voxel_hierarchy(bytes: &[u8]) -> Result<VoxelHierarchySection> {
    let mut reader = section_reader(bytes, VOXEL_DOMAIN, ".splantc voxel hierarchy")?;
    let brick_count = reader.count(100)?;
    let mut bricks = Vec::with_capacity(brick_count);
    for expected in 0..brick_count {
        let id = reader.u32()?;
        let dimensions = reader.array::<3>()?;
        let bounds = read_bounds(&mut reader)?;
        let deformed_bounds = read_bounds(&mut reader)?;
        let page = reader.u32()?;
        let appearance_error = read_error(&mut reader)?;
        let material_class =
            VirtualMaterialClass::from_tag(reader.u8()?, ".splantc voxel hierarchy")?;
        let opacity_micromap = reader.bool()?;
        let moments = read_moments(&mut reader)?;
        let occupancy_length = reader.length()?;
        let occupancy = reader.take(occupancy_length)?.to_vec();
        let vertex_count = reader.count(16)?;
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            vertices.push(PortableVoxelVertex {
                position_bits: [reader.i32()?, reader.i32()?, reader.i32()?],
                normal_oct: [reader.u16()? as i16, reader.u16()? as i16],
            });
        }
        let index_count = reader.count(4)?;
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(reader.u32()?);
        }
        let voxel_count = dimensions.into_iter().try_fold(1_usize, |total, value| {
            total.checked_mul(usize::from(value))
        });
        if id as usize != expected
            || dimensions.contains(&0)
            || voxel_count.is_none_or(|count| occupancy.len() != count.div_ceil(8))
            || vertices.is_empty()
            || indices.is_empty()
            || !indices.len().is_multiple_of(3)
            || indices
                .iter()
                .any(|index| *index as usize >= vertices.len())
            || !bounds_contains(deformed_bounds, bounds)
        {
            return Err(format_error(".splantc voxel hierarchy", "brick"));
        }
        bricks.push(PortableVoxelBrick {
            id,
            dimensions,
            bounds,
            deformed_bounds,
            occupancy,
            moments,
            material_class,
            opacity_micromap,
            vertices,
            indices,
            page,
            appearance_error,
        });
    }
    reader.complete()?;
    Ok(VoxelHierarchySection { bricks })
}

/// Strictly decodes structural deformation modes and swept bounds.
pub fn decode_deformation(bytes: &[u8]) -> Result<DeformationSection> {
    let mut reader = section_reader(bytes, DEFORMATION_DOMAIN, ".splantc deformation")?;
    let count = reader.count(65)?;
    let mut regions = Vec::with_capacity(count);
    for _ in 0..count {
        let part = reader.u128()?;
        let semantic = semantic_from_tag(reader.u8()?)?;
        let influence_count = reader.count(16)?;
        let mut influences = Vec::with_capacity(influence_count);
        for _ in 0..influence_count {
            influences.push(reader.u128()?);
        }
        let static_bounds = read_bounds(&mut reader)?;
        let swept_bounds = read_bounds(&mut reader)?;
        if influences.windows(2).any(|pair| pair[0] >= pair[1])
            || !bounds_contains(swept_bounds, static_bounds)
        {
            return Err(format_error(".splantc deformation", "region"));
        }
        regions.push(PortableDeformationRegion {
            part,
            semantic,
            influences,
            static_bounds,
            swept_bounds,
        });
    }
    if regions.windows(2).any(|pair| pair[0].part >= pair[1].part) {
        return Err(format_error(".splantc deformation", "region.order"));
    }
    reader.complete()?;
    Ok(DeformationSection { regions })
}

/// Strictly decodes the mixed hierarchy and page dependencies.
pub fn decode_page_directory(bytes: &[u8]) -> Result<PageDirectorySection> {
    let mut reader = section_reader(bytes, PAGE_DOMAIN, ".splantc page directory")?;
    let node_count = reader.count(90)?;
    let mut nodes = Vec::with_capacity(node_count);
    for expected in 0..node_count {
        let id = reader.u32()?;
        let tag = reader.u8()?;
        let payload = reader.u32()?;
        let payload_count = reader.u32()?;
        let representation = match tag {
            0 if payload_count != 0 => HierarchyRepresentation::Triangles {
                first: payload,
                count: payload_count,
            },
            1 if payload_count == 0 => HierarchyRepresentation::Voxel { brick: payload },
            _ => {
                return Err(format_error(
                    ".splantc page directory",
                    "node.representation",
                ));
            }
        };
        let parent = read_optional_u32(&mut reader)?;
        let child_count = reader.count(4)?;
        let mut children = Vec::with_capacity(child_count);
        for _ in 0..child_count {
            children.push(reader.u32()?);
        }
        let page = reader.u32()?;
        let bounds = read_bounds(&mut reader)?;
        let deformed_bounds = read_bounds(&mut reader)?;
        let appearance_error = read_error(&mut reader)?;
        if id as usize != expected
            || children.windows(2).any(|pair| pair[0] >= pair[1])
            || !bounds_contains(deformed_bounds, bounds)
        {
            return Err(format_error(".splantc page directory", "node"));
        }
        nodes.push(PortableHierarchyNode {
            id,
            representation,
            parent,
            children,
            page,
            bounds,
            deformed_bounds,
            appearance_error,
        });
    }
    let page_count = reader.count(90)?;
    let mut pages = Vec::with_capacity(page_count);
    for expected in 0..page_count {
        let id = reader.u32()?;
        let dependency = read_optional_u32(&mut reader)?;
        let node = reader.u32()?;
        let bounds = read_bounds(&mut reader)?;
        let deformed_bounds = read_bounds(&mut reader)?;
        let transition_error = read_error(&mut reader)?;
        let guaranteed_root = reader.bool()?;
        if id as usize != expected
            || dependency.is_some_and(|parent| parent >= id)
            || node as usize >= nodes.len()
            || guaranteed_root != dependency.is_none()
            || !bounds_contains(deformed_bounds, bounds)
        {
            return Err(format_error(".splantc page directory", "page"));
        }
        pages.push(PortableHierarchyPage {
            id,
            dependency,
            node,
            bounds,
            deformed_bounds,
            transition_error,
            guaranteed_root,
        });
    }
    let root_count = reader.count(4)?;
    let mut roots = Vec::with_capacity(root_count);
    for _ in 0..root_count {
        roots.push(reader.u32()?);
    }
    reader.complete()?;
    if roots.is_empty() || roots.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format_error(".splantc page directory", "roots"));
    }
    for node in &nodes {
        if node.page as usize >= pages.len()
            || pages[node.page as usize].node != node.id
            || node
                .parent
                .is_some_and(|parent| parent as usize >= nodes.len())
        {
            return Err(format_error(".splantc page directory", "node.references"));
        }
        for child in &node.children {
            if *child as usize >= nodes.len()
                || nodes[*child as usize].parent != Some(node.id)
                || nodes[*child as usize].appearance_error.total > node.appearance_error.total
            {
                return Err(format_error(".splantc page directory", "node.children"));
            }
        }
    }
    for root in &roots {
        if *root as usize >= nodes.len()
            || nodes[*root as usize].parent.is_some()
            || !pages[nodes[*root as usize].page as usize].guaranteed_root
        {
            return Err(format_error(".splantc page directory", "root"));
        }
    }
    Ok(PageDirectorySection {
        nodes,
        pages,
        roots,
    })
}

/// Strictly decodes RT/coverage derivation metadata.
pub fn decode_ray_tracing(bytes: &[u8]) -> Result<RayTracingSection> {
    let mut reader = section_reader(bytes, RAY_TRACING_DOMAIN, ".splantc ray tracing")?;
    let count = reader.count(7)?;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        records.push(PortableRayTracingRecord {
            node: reader.u32()?,
            material_class: VirtualMaterialClass::from_tag(reader.u8()?, ".splantc ray tracing")?,
            opacity_micromap: reader.bool()?,
            requires_any_hit: reader.bool()?,
        });
    }
    reader.complete()?;
    if records.windows(2).any(|pair| pair[0].node >= pair[1].node) {
        return Err(format_error(".splantc ray tracing", "record.order"));
    }
    Ok(RayTracingSection { records })
}

/// Strictly combines the five final hierarchy sections and validates all cross-references.
pub fn decode_portable_virtual_hierarchy_sections(
    triangle: &[u8],
    voxel: &[u8],
    deformation: &[u8],
    pages: &[u8],
    ray_tracing: &[u8],
) -> Result<PortableVirtualHierarchy> {
    let triangle = decode_triangle_hierarchy(triangle)?;
    let voxel = decode_voxel_hierarchy(voxel)?;
    let deformation = decode_deformation(deformation)?;
    let pages = decode_page_directory(pages)?;
    let ray_tracing = decode_ray_tracing(ray_tracing)?;
    let hierarchy = PortableVirtualHierarchy {
        prototypes: triangle.prototypes,
        micro_instances: triangle.micro_instances,
        triangle_clusters: triangle.clusters,
        voxel_bricks: voxel.bricks,
        nodes: pages.nodes,
        deformation: deformation.regions,
        pages: pages.pages,
        roots: pages.roots,
        ray_tracing: ray_tracing.records,
    };
    if hierarchy.ray_tracing.len() != hierarchy.nodes.len()
        || hierarchy
            .ray_tracing
            .iter()
            .enumerate()
            .any(|(node, record)| record.node as usize != node)
    {
        return Err(format_error("portable hierarchy", "rayTracing.nodes"));
    }
    for (node, record) in hierarchy.nodes.iter().zip(&hierarchy.ray_tracing) {
        let (class, opacity_micromap) = match node.representation {
            HierarchyRepresentation::Triangles { first, .. } => {
                let cluster = &hierarchy.triangle_clusters[first as usize];
                (cluster.material_class, cluster.opacity_micromap)
            }
            HierarchyRepresentation::Voxel { brick } => {
                let brick = &hierarchy.voxel_bricks[brick as usize];
                (brick.material_class, brick.opacity_micromap)
            }
        };
        if record.material_class != class
            || record.opacity_micromap != opacity_micromap
            || record.requires_any_hit
                != matches!(
                    class,
                    VirtualMaterialClass::Masked | VirtualMaterialClass::ThinSheet
                )
        {
            return Err(format_error("portable hierarchy", "rayTracing.material"));
        }
    }
    validate_portable_virtual_hierarchy(&hierarchy)?;
    Ok(hierarchy)
}

fn section_writer(domain: &[u8]) -> Result<BinaryWriter> {
    let mut writer = BinaryWriter::new();
    writer.length(domain.len())?;
    writer.bytes(domain);
    Ok(writer)
}

fn section_reader<'a>(
    bytes: &'a [u8],
    domain: &[u8],
    format: &'static str,
) -> Result<BinaryReader<'a>> {
    let mut reader = BinaryReader::new(bytes, format);
    let length = reader.length()?;
    if reader.take(length)? != domain {
        return Err(format_error(format, "domain"));
    }
    Ok(reader)
}

fn write_bounds(writer: &mut BinaryWriter, bounds: PortableBounds) {
    for value in bounds.min_bits.into_iter().chain(bounds.max_bits) {
        writer.i32(value);
    }
}

fn read_bounds(reader: &mut BinaryReader<'_>) -> Result<PortableBounds> {
    let bounds = PortableBounds {
        min_bits: [reader.i32()?, reader.i32()?, reader.i32()?],
        max_bits: [reader.i32()?, reader.i32()?, reader.i32()?],
    };
    if (0..3).any(|axis| bounds.min_bits[axis] > bounds.max_bits[axis]) {
        return Err(format_error("portable bounds", "range"));
    }
    Ok(bounds)
}

fn write_error(writer: &mut BinaryWriter, error: AppearanceError) {
    writer.u32(error.silhouette);
    writer.u32(error.coverage);
    writer.u32(error.transmission);
    writer.u32(error.material);
    writer.u32(error.normal_distribution);
    writer.u32(error.total);
}

fn read_error(reader: &mut BinaryReader<'_>) -> Result<AppearanceError> {
    let error = AppearanceError {
        silhouette: reader.u32()?,
        coverage: reader.u32()?,
        transmission: reader.u32()?,
        material: reader.u32()?,
        normal_distribution: reader.u32()?,
        total: reader.u32()?,
    };
    if AppearanceError::new(
        error.silhouette,
        error.coverage,
        error.transmission,
        error.material,
        error.normal_distribution,
    ) != error
    {
        return Err(format_error("portable appearance error", "total"));
    }
    Ok(error)
}

fn write_moments(writer: &mut BinaryWriter, moments: VoxelMaterialMoments) {
    writer.u16(moments.occupancy.bits());
    for value in moments.albedo_mean {
        writer.i32(value.bits());
    }
    writer.u16(moments.roughness_mean.bits());
    for value in moments.transmission_mean {
        writer.i32(value.bits());
    }
    writer.i32(moments.thickness_mean.bits());
    for value in moments.normal_second_moments {
        writer.i32(value.bits());
    }
}

fn read_moments(reader: &mut BinaryReader<'_>) -> Result<VoxelMaterialMoments> {
    Ok(VoxelMaterialMoments {
        occupancy: UnitInterval::from_bits(reader.u16()?),
        albedo_mean: [
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
        ],
        roughness_mean: UnitInterval::from_bits(reader.u16()?),
        transmission_mean: [
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
        ],
        thickness_mean: DecisionScalar::from_bits(reader.i32()?),
        normal_second_moments: [
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
            DecisionScalar::from_bits(reader.i32()?),
        ],
    })
}

fn write_optional_u32(writer: &mut BinaryWriter, value: Option<u32>) {
    writer.bool(value.is_some());
    if let Some(value) = value {
        writer.u32(value);
    }
}

fn read_optional_u32(reader: &mut BinaryReader<'_>) -> Result<Option<u32>> {
    if reader.bool()? {
        Ok(Some(reader.u32()?))
    } else {
        Ok(None)
    }
}

fn semantic_tag(semantic: PlantPartSemantic) -> u8 {
    match semantic {
        PlantPartSemantic::Trunk => 0,
        PlantPartSemantic::Branch => 1,
        PlantPartSemantic::Root => 2,
        PlantPartSemantic::Frond => 3,
        PlantPartSemantic::Leaf => 4,
        PlantPartSemantic::Flower => 5,
        PlantPartSemantic::Fruit => 6,
        PlantPartSemantic::Blade => 7,
    }
}

fn semantic_from_tag(tag: u8) -> Result<PlantPartSemantic> {
    match tag {
        0 => Ok(PlantPartSemantic::Trunk),
        1 => Ok(PlantPartSemantic::Branch),
        2 => Ok(PlantPartSemantic::Root),
        3 => Ok(PlantPartSemantic::Frond),
        4 => Ok(PlantPartSemantic::Leaf),
        5 => Ok(PlantPartSemantic::Flower),
        6 => Ok(PlantPartSemantic::Fruit),
        7 => Ok(PlantPartSemantic::Blade),
        _ => Err(format_error(".splantc deformation", "semantic")),
    }
}
