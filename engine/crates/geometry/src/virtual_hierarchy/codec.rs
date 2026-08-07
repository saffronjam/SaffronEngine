//! The canonical five-section byte envelope: strict encode and strict decode.

use crate::Result;
use crate::portable_binary::{BinaryReader, BinaryWriter};

use super::support::{bounds_contains, format_error};
use super::types::{
    AppearanceError, GeometryPrototype, HierarchyRepresentation, MicroInstance,
    PORTABLE_CLUSTER_MAX_TRIANGLES, PORTABLE_CLUSTER_MAX_VERTICES,
    PORTABLE_HIERARCHY_FORMAT_VERSION, PortableBounds, PortableClusterVertex,
    PortableDeformationKind, PortableDeformationRegion, PortableHierarchyNode,
    PortableHierarchyPage, PortableMaterialMoments, PortableOpacityMicromap,
    PortableRayTracingRecord, PortableTriangleCluster, PortableUseCombination,
    PortableVirtualHierarchy, PortableVoxelBrick, PortableVoxelVertex, VirtualMaterialClass,
};
use super::validate::validate_portable_virtual_hierarchy;

const PORTABLE_HIERARCHY_MAGIC: &[u8; 4] = b"PVHR";

const TRIANGLE_DOMAIN: &[u8] = b"saffron-anima/portable/triangle-hierarchy/v1";
const VOXEL_DOMAIN: &[u8] = b"saffron-anima/portable/voxel-hierarchy/v1";
const DEFORMATION_DOMAIN: &[u8] = b"saffron-anima/portable/deformation/v1";
const PAGE_DOMAIN: &[u8] = b"saffron-anima/portable/page-directory/v1";
const RAY_TRACING_DOMAIN: &[u8] = b"saffron-anima/portable/ray-tracing/v2";

/// Decoded triangle-hierarchy section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TriangleHierarchySection {
    /// Shared prototypes.
    pub prototypes: Vec<GeometryPrototype>,
    /// Semantic micro-instances.
    pub micro_instances: Vec<MicroInstance>,
    /// Per-(variation, phenotype) active-use masks.
    pub combinations: Vec<PortableUseCombination>,
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
    /// Derived opacity micromaps, keyed by flattened submesh.
    pub opacity_micromaps: Vec<PortableOpacityMicromap>,
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
            writer.u32(prototype.vertex_count);
            writer.u32(prototype.submesh_count);
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
        writer.length(self.combinations.len())?;
        for combination in &self.combinations {
            writer.u32(combination.variation);
            writer.u32(combination.phenotype);
            writer.length(combination.active_words.len())?;
            for word in &combination.active_words {
                writer.u32(*word);
            }
        }
        writer.length(self.triangle_clusters.len())?;
        for cluster in &self.triangle_clusters {
            writer.u32(cluster.id);
            writer.u32(cluster.prototype);
            writer.u32(cluster.source_submesh);
            writer.u32(cluster.material_slot);
            writer.u8(cluster.material_class as u8);
            write_moments(&mut writer, cluster.material_moments);
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
            for source_vertex in &cluster.source_vertices {
                writer.u32(*source_vertex);
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
        writer.length(self.opacity_micromaps.len())?;
        for micromap in &self.opacity_micromaps {
            writer.u32(micromap.submesh);
            writer.length(micromap.indices.len())?;
            for &index in &micromap.indices {
                writer.u32(index as u32);
            }
            writer.length(micromap.blocks.len())?;
            for &(offset, level, format) in &micromap.blocks {
                writer.u32(offset);
                writer.u16(level);
                writer.u16(format);
            }
            writer.length(micromap.data.len())?;
            writer.bytes(&micromap.data);
            writer.length(micromap.usage.len())?;
            for &(count, level, format) in &micromap.usage {
                writer.u32(count);
                writer.u32(level);
                writer.u32(format);
            }
            writer.u64(micromap.classes.0);
            writer.u64(micromap.classes.1);
            writer.u64(micromap.classes.2);
        }
        Ok(writer.finish())
    }
}

/// Encodes all five canonical hierarchy sections into one strict format-neutral envelope.
pub fn encode_portable_virtual_hierarchy(hierarchy: &PortableVirtualHierarchy) -> Result<Vec<u8>> {
    validate_portable_virtual_hierarchy(hierarchy)?;
    let sections = [
        hierarchy.triangle_hierarchy_bytes()?,
        hierarchy.voxel_hierarchy_bytes()?,
        hierarchy.deformation_bytes()?,
        hierarchy.page_directory_bytes()?,
        hierarchy.ray_tracing_bytes()?,
    ];
    let mut writer = BinaryWriter::new();
    writer.bytes(PORTABLE_HIERARCHY_MAGIC);
    writer.u32(PORTABLE_HIERARCHY_FORMAT_VERSION);
    for section in &sections {
        writer.length(section.len())?;
    }
    for section in &sections {
        writer.bytes(section);
    }
    Ok(writer.finish())
}

/// Strictly decodes the one canonical envelope and validates every cross-reference.
pub fn decode_portable_virtual_hierarchy(bytes: &[u8]) -> Result<PortableVirtualHierarchy> {
    let mut reader = BinaryReader::new(bytes, "portable hierarchy envelope");
    if reader.take(PORTABLE_HIERARCHY_MAGIC.len())? != PORTABLE_HIERARCHY_MAGIC {
        return Err(format_error("portable hierarchy envelope", "magic"));
    }
    if reader.u32()? != PORTABLE_HIERARCHY_FORMAT_VERSION {
        return Err(format_error("portable hierarchy envelope", "version"));
    }
    let lengths = [
        reader.length()?,
        reader.length()?,
        reader.length()?,
        reader.length()?,
        reader.length()?,
    ];
    let triangle = reader.take(lengths[0])?;
    let voxel = reader.take(lengths[1])?;
    let deformation = reader.take(lengths[2])?;
    let pages = reader.take(lengths[3])?;
    let ray_tracing = reader.take(lengths[4])?;
    reader.complete()?;
    decode_portable_virtual_hierarchy_sections(triangle, voxel, deformation, pages, ray_tracing)
}

/// Strictly decodes the portable triangle hierarchy.
pub fn decode_triangle_hierarchy(bytes: &[u8]) -> Result<TriangleHierarchySection> {
    let mut reader = section_reader(bytes, TRIANGLE_DOMAIN, "portable triangle hierarchy")?;
    let prototype_count = reader.count(60)?;
    let mut prototypes = Vec::with_capacity(prototype_count);
    for expected in 0..prototype_count {
        let prototype = GeometryPrototype {
            id: reader.u32()?,
            source: reader.u128()?,
            selector_hash: reader.array()?,
            vertex_count: reader.u32()?,
            submesh_count: reader.u32()?,
            first_cluster: reader.u32()?,
            cluster_count: reader.u32()?,
            bounds: read_bounds(&mut reader)?,
        };
        if prototype.id as usize != expected {
            return Err(format_error("portable triangle hierarchy", "prototype.id"));
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
                "portable triangle hierarchy",
                "instance.prototype",
            ));
        }
        micro_instances.push(MicroInstance {
            part,
            prototype,
            transform_bits,
        });
    }
    let combination_count = reader.count(12)?;
    let mask_words = instance_count.div_ceil(32);
    let mut combinations = Vec::with_capacity(combination_count);
    for _ in 0..combination_count {
        let variation = reader.u32()?;
        let phenotype = reader.u32()?;
        let word_count = reader.count(4)?;
        if word_count != mask_words {
            return Err(format_error(
                "portable triangle hierarchy",
                "combination.activeWords",
            ));
        }
        let mut active_words = Vec::with_capacity(word_count);
        for _ in 0..word_count {
            active_words.push(reader.u32()?);
        }
        combinations.push(PortableUseCombination {
            variation,
            phenotype,
            active_words,
        });
    }
    let cluster_count = reader.count(112)?;
    let mut clusters = Vec::with_capacity(cluster_count);
    for expected in 0..cluster_count {
        let id = reader.u32()?;
        let prototype = reader.u32()?;
        let source_submesh = reader.u32()?;
        let material_slot = reader.u32()?;
        let material_class =
            VirtualMaterialClass::from_tag(reader.u8()?, "portable triangle hierarchy")?;
        let material_moments = read_moments(&mut reader)?;
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
                "portable triangle hierarchy",
                "cluster.joints",
            ));
        }
        let vertex_count = reader.count(23)?;
        if vertex_count == 0 || vertex_count > PORTABLE_CLUSTER_MAX_VERTICES {
            return Err(format_error(
                "portable triangle hierarchy",
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
        let mut source_vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            source_vertices.push(reader.u32()?);
        }
        let index_count = reader.length()?;
        if index_count == 0
            || !index_count.is_multiple_of(3)
            || index_count / 3 > PORTABLE_CLUSTER_MAX_TRIANGLES
        {
            return Err(format_error(
                "portable triangle hierarchy",
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
            return Err(format_error("portable triangle hierarchy", "cluster"));
        }
        clusters.push(PortableTriangleCluster {
            id,
            prototype,
            source_submesh,
            material_slot,
            material_class,
            material_moments,
            opacity_micromap,
            vertices,
            source_vertices,
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
                "portable triangle hierarchy",
                "prototype.clusters",
            ));
        }
    }
    Ok(TriangleHierarchySection {
        prototypes,
        micro_instances,
        combinations,
        clusters,
    })
}

/// Strictly decodes aggregate voxel bricks and portable indexed surfaces.
pub fn decode_voxel_hierarchy(bytes: &[u8]) -> Result<VoxelHierarchySection> {
    let mut reader = section_reader(bytes, VOXEL_DOMAIN, "portable voxel hierarchy")?;
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
            VirtualMaterialClass::from_tag(reader.u8()?, "portable voxel hierarchy")?;
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
            return Err(format_error("portable voxel hierarchy", "brick"));
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
    let mut reader = section_reader(bytes, DEFORMATION_DOMAIN, "portable deformation")?;
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
            return Err(format_error("portable deformation", "region"));
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
        return Err(format_error("portable deformation", "region.order"));
    }
    reader.complete()?;
    Ok(DeformationSection { regions })
}

/// Strictly decodes the mixed hierarchy and page dependencies.
pub fn decode_page_directory(bytes: &[u8]) -> Result<PageDirectorySection> {
    let mut reader = section_reader(bytes, PAGE_DOMAIN, "portable page directory")?;
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
                    "portable page directory",
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
            return Err(format_error("portable page directory", "node"));
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
    let page_count = reader.count(82)?;
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
            return Err(format_error("portable page directory", "page"));
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
        return Err(format_error("portable page directory", "roots"));
    }
    for node in &nodes {
        if node.page as usize >= pages.len()
            || pages[node.page as usize].node != node.id
            || node
                .parent
                .is_some_and(|parent| parent as usize >= nodes.len())
        {
            return Err(format_error("portable page directory", "node.references"));
        }
        for child in &node.children {
            if *child as usize >= nodes.len()
                || nodes[*child as usize].parent != Some(node.id)
                || nodes[*child as usize].appearance_error.total > node.appearance_error.total
            {
                return Err(format_error("portable page directory", "node.children"));
            }
        }
    }
    for root in &roots {
        if *root as usize >= nodes.len()
            || nodes[*root as usize].parent.is_some()
            || !pages[nodes[*root as usize].page as usize].guaranteed_root
        {
            return Err(format_error("portable page directory", "root"));
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
    let mut reader = section_reader(bytes, RAY_TRACING_DOMAIN, "portable ray tracing")?;
    let count = reader.count(7)?;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        records.push(PortableRayTracingRecord {
            node: reader.u32()?,
            material_class: VirtualMaterialClass::from_tag(reader.u8()?, "portable ray tracing")?,
            opacity_micromap: reader.bool()?,
            requires_any_hit: reader.bool()?,
        });
    }
    let micromap_count = reader.length()?;
    let mut opacity_micromaps = Vec::with_capacity(micromap_count);
    for _ in 0..micromap_count {
        let submesh = reader.u32()?;
        let index_count = reader.length()?;
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            indices.push(reader.u32()? as i32);
        }
        let block_count = reader.length()?;
        let mut blocks = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            blocks.push((reader.u32()?, reader.u16()?, reader.u16()?));
        }
        let data_len = reader.length()?;
        let data = reader.take(data_len)?.to_vec();
        let usage_count = reader.length()?;
        let mut usage = Vec::with_capacity(usage_count);
        for _ in 0..usage_count {
            usage.push((reader.u32()?, reader.u32()?, reader.u32()?));
        }
        opacity_micromaps.push(PortableOpacityMicromap {
            submesh,
            indices,
            blocks,
            data,
            usage,
            classes: (reader.u64()?, reader.u64()?, reader.u64()?),
        });
    }
    reader.complete()?;
    if records.windows(2).any(|pair| pair[0].node >= pair[1].node) {
        return Err(format_error("portable ray tracing", "record.order"));
    }
    Ok(RayTracingSection {
        records,
        opacity_micromaps,
    })
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
        combinations: triangle.combinations,
        triangle_clusters: triangle.clusters,
        voxel_bricks: voxel.bricks,
        nodes: pages.nodes,
        deformation: deformation.regions,
        pages: pages.pages,
        roots: pages.roots,
        ray_tracing: ray_tracing.records,
        opacity_micromaps: ray_tracing.opacity_micromaps,
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

fn write_moments(writer: &mut BinaryWriter, moments: PortableMaterialMoments) {
    writer.u16(moments.occupancy);
    for value in moments.albedo_mean {
        writer.i32(value);
    }
    writer.u16(moments.roughness_mean);
    for value in moments.transmission_mean {
        writer.i32(value);
    }
    writer.i32(moments.thickness_mean);
    for value in moments.normal_second_moments {
        writer.i32(value);
    }
}

fn read_moments(reader: &mut BinaryReader<'_>) -> Result<PortableMaterialMoments> {
    Ok(PortableMaterialMoments {
        occupancy: reader.u16()?,
        albedo_mean: [reader.i32()?, reader.i32()?, reader.i32()?],
        roughness_mean: reader.u16()?,
        transmission_mean: [reader.i32()?, reader.i32()?, reader.i32()?],
        thickness_mean: reader.i32()?,
        normal_second_moments: [
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
            reader.i32()?,
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

fn semantic_tag(semantic: PortableDeformationKind) -> u8 {
    semantic.0
}

fn semantic_from_tag(tag: u8) -> Result<PortableDeformationKind> {
    Ok(PortableDeformationKind(tag))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_hierarchy::cook::cook_portable_virtual_hierarchy;
    use crate::virtual_hierarchy::fixtures::multi_page_input;
    use crate::virtual_hierarchy::types::{PortableHierarchyInput, VirtualHierarchyMaterial};

    #[test]
    fn envelope_decodes_multiple_parent_before_child_pages() {
        let hierarchy = cook_portable_virtual_hierarchy(&multi_page_input()).unwrap();
        assert!(hierarchy.pages.len() > 2);
        let bytes = encode_portable_virtual_hierarchy(&hierarchy).unwrap();
        assert_eq!(
            decode_portable_virtual_hierarchy(&bytes).unwrap(),
            hierarchy
        );
    }

    #[test]
    fn aggregate_only_input_cooks_a_drawable_root() {
        let input = PortableHierarchyInput {
            combinations: Vec::new(),
            meshes: Vec::new(),
            micro_instances: Vec::new(),
            deformation: Vec::new(),
            bounds: PortableBounds {
                min_bits: [-65_536; 3],
                max_bits: [65_536; 3],
            },
            root_material: VirtualHierarchyMaterial::opaque(0),
            deformation_padding: 0,
        };
        let hierarchy = cook_portable_virtual_hierarchy(&input).unwrap();
        assert!(hierarchy.prototypes.is_empty());
        assert!(hierarchy.triangle_clusters.is_empty());
        assert_eq!(hierarchy.roots.len(), 1);
        assert!(matches!(
            hierarchy.nodes[hierarchy.roots[0] as usize].representation,
            HierarchyRepresentation::Voxel { .. }
        ));
        let bytes = encode_portable_virtual_hierarchy(&hierarchy).unwrap();
        assert_eq!(
            decode_portable_virtual_hierarchy(&bytes).unwrap(),
            hierarchy
        );
    }
}
