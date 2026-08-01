//! Byte-locked device payload for one hierarchy content page.
//!
//! A resident page's bytes in the [`crate::GlobalGpuData::pages`] arena hold exactly one
//! drawable hierarchy node: a [`GpuPageNodeRecord`] header, the node's child pages as
//! resident page-table handles, per-cluster [`GpuPageClusterRecord`]s, an optional voxel
//! surface vertex block, and a u32 index blob. Triangle-cluster indices are
//! geometry-relative vertex indices (the cook's `source_vertices` resolved through
//! `local_indices`), so an indexed executor draws a cluster over the geometry's resident
//! vertex range without any per-page vertex upload; voxel indices address the page's own
//! vertex block. [`build_page_payload`] is pure and deterministic; the mirror patches the
//! child-handle table with resident page-table handles before upload.

use bytemuck::{Pod, Zeroable};
use saffron_geometry::{
    HierarchyRepresentation, PortableBounds, PortableHierarchyPage, PortableVirtualHierarchy,
};

use crate::{Error, GpuHandle, GpuRepresentation, Result};

/// Payload flag: the page is a guaranteed-resident root.
pub const GPU_PAGE_PAYLOAD_FLAG_GUARANTEED_ROOT: u32 = 1 << 0;

/// The node header at byte 0 of every page payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct GpuPageNodeRecord {
    /// [`GpuRepresentation`] discriminant of the node's drawable payload.
    pub representation: u32,
    /// Triangle clusters stored by this page (zero for a voxel page).
    pub cluster_count: u32,
    /// Child pages required for hole-free refinement.
    pub child_count: u32,
    /// Payload flags (`GPU_PAGE_PAYLOAD_FLAG_*`).
    pub flags: u32,
    /// Static payload minimum, local metres.
    pub bounds_min: [f32; 3],
    /// Saturating appearance-error total of this drawable representation.
    pub appearance_total: u32,
    /// Static payload maximum, local metres.
    pub bounds_max: [f32; 3],
    /// Representation-transition threshold total.
    pub transition_total: u32,
    /// Conservative deformed minimum, local metres.
    pub deformed_min: [f32; 3],
    /// Voxel surface vertices stored by this page (zero for a triangle page).
    pub vertex_count: u32,
    /// Conservative deformed maximum, local metres.
    pub deformed_max: [f32; 3],
    /// Total u32 indices in the page's index blob.
    pub index_count: u32,
    /// The single source prototype every drawable in this node's subtree belongs to,
    /// or [`GPU_PAGE_NODE_NO_PROTOTYPE`] when the subtree spans prototypes (the family
    /// root). The traversal forks assembly uses on it.
    pub prototype: u32,
    /// Reserved ABI words.
    pub reserved: [u32; 3],
}

/// [`GpuPageNodeRecord::prototype`]: the subtree spans prototypes (no assembly fork).
pub const GPU_PAGE_NODE_NO_PROTOTYPE: u32 = u32::MAX;

const _: () = assert!(size_of::<GpuPageNodeRecord>() == 96);

/// One triangle cluster within a page payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct GpuPageClusterRecord {
    /// First u32 within the page's index blob.
    pub first_index: u32,
    /// Index count (a multiple of three).
    pub index_count: u32,
    /// Prototype material slot.
    pub material_slot: u32,
    /// Shared source-geometry prototype.
    pub prototype: u32,
    /// Exact cluster minimum, local metres.
    pub bounds_min: [f32; 3],
    /// Saturating appearance-error total of the cluster.
    pub appearance_total: u32,
    /// Exact cluster maximum, local metres.
    pub bounds_max: [f32; 3],
    /// Quantized cone axis and conservative cutoff, packed `i8x4` little-endian.
    pub cone: u32,
    /// Conservative swept cluster minimum, local metres.
    pub deformed_min: [f32; 3],
    /// Padding: std430 starts a three-component vector on a 16-byte boundary.
    pub reserved0: u32,
    /// Conservative swept cluster maximum, local metres.
    pub deformed_max: [f32; 3],
    /// Padding: std430 starts a three-component vector on a 16-byte boundary.
    pub reserved1: u32,
}

const _: () = assert!(size_of::<GpuPageClusterRecord>() == 80);

/// One voxel-surface vertex within a page payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
#[repr(C)]
pub struct GpuPageVoxelVertex {
    /// Local-metre position.
    pub position: [f32; 3],
    /// Octahedrally encoded outward normal, packed `i16x2` little-endian.
    pub normal_oct: u32,
}

const _: () = assert!(size_of::<GpuPageVoxelVertex>() == 16);

/// One built page payload: the locked bytes plus the child-handle patch table.
///
/// `bytes[child_table_offset..]` holds `child_pages.len()` zeroed [`GpuHandle`]s; the
/// mirror overwrites each with the resident page-table handle of the matching cook page
/// id before upload.
#[derive(Clone, Debug, PartialEq)]
pub struct PagePayload {
    /// The locked payload bytes (16-byte-aligned length).
    pub bytes: Vec<u8>,
    /// Cook page ids of the node's children, in `bytes` table order.
    pub child_pages: Vec<u32>,
    /// Byte offset of the child [`GpuHandle`] table within `bytes`.
    pub child_table_offset: usize,
}

impl PagePayload {
    /// Overwrites child-table entry `index` with `handle`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidUploadData`] when `index` is out of range.
    pub fn patch_child(&mut self, index: usize, handle: GpuHandle) -> Result<()> {
        if index >= self.child_pages.len() {
            return Err(Error::InvalidUploadData(
                "page payload child patch index out of range".to_owned(),
            ));
        }
        let offset = self.child_table_offset + index * size_of::<GpuHandle>();
        self.bytes[offset..offset + size_of::<GpuHandle>()]
            .copy_from_slice(bytemuck::bytes_of(&handle));
        Ok(())
    }
}

fn q15_16(bits: i32) -> f32 {
    bits as f32 / 65_536.0
}

fn bounds_min(bounds: &PortableBounds) -> [f32; 3] {
    bounds.min_bits.map(q15_16)
}

fn bounds_max(bounds: &PortableBounds) -> [f32; 3] {
    bounds.max_bits.map(q15_16)
}

/// The single source prototype every drawable in `node`'s subtree belongs to, or
/// [`GPU_PAGE_NODE_NO_PROTOTYPE`] when the subtree spans prototypes (the family root
/// over per-mesh roots). A triangle node reads its first cluster; a voxel node
/// resolves through its children (a per-mesh aggregate brick covers one prototype).
fn subtree_prototype(
    hierarchy: &PortableVirtualHierarchy,
    node: &saffron_geometry::PortableHierarchyNode,
) -> u32 {
    match node.representation {
        HierarchyRepresentation::Triangles { first, .. } => hierarchy
            .triangle_clusters
            .get(first as usize)
            .map_or(GPU_PAGE_NODE_NO_PROTOTYPE, |cluster| cluster.prototype),
        HierarchyRepresentation::Voxel { .. } => {
            let mut found: Option<u32> = None;
            for child in &node.children {
                let Some(child_node) = hierarchy.nodes.get(*child as usize) else {
                    return GPU_PAGE_NODE_NO_PROTOTYPE;
                };
                let child_prototype = subtree_prototype(hierarchy, child_node);
                match found {
                    None => found = Some(child_prototype),
                    Some(existing) if existing == child_prototype => {}
                    Some(_) => return GPU_PAGE_NODE_NO_PROTOTYPE,
                }
            }
            found.unwrap_or(GPU_PAGE_NODE_NO_PROTOTYPE)
        }
    }
}

fn payload_error(message: impl Into<String>) -> Error {
    Error::InvalidUploadData(message.into())
}

fn pad_to_16(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(16) {
        bytes.push(0);
    }
}

/// Builds the locked device payload for `page_id` of `hierarchy`.
///
/// # Errors
///
/// Returns [`Error::InvalidUploadData`] when the page, its node, or a referenced
/// cluster/brick is missing from the cooked hierarchy.
pub fn build_page_payload(
    hierarchy: &PortableVirtualHierarchy,
    page_id: u32,
) -> Result<PagePayload> {
    let page: &PortableHierarchyPage = hierarchy
        .pages
        .iter()
        .find(|page| page.id == page_id)
        .ok_or_else(|| payload_error(format!("hierarchy has no page {page_id}")))?;
    let node = hierarchy
        .nodes
        .get(page.node as usize)
        .filter(|node| node.id == page.node)
        .ok_or_else(|| payload_error(format!("page {page_id} references missing node")))?;

    let child_pages: Vec<u32> = node
        .children
        .iter()
        .map(|child| {
            hierarchy
                .nodes
                .get(*child as usize)
                .map(|child_node| child_node.page)
                .ok_or_else(|| payload_error(format!("node {} references missing child", node.id)))
        })
        .collect::<Result<_>>()?;

    let mut header = GpuPageNodeRecord {
        cluster_count: 0,
        child_count: u32::try_from(child_pages.len())
            .map_err(|_| payload_error("page child count exceeds u32"))?,
        flags: if page.guaranteed_root {
            GPU_PAGE_PAYLOAD_FLAG_GUARANTEED_ROOT
        } else {
            0
        },
        bounds_min: bounds_min(&node.bounds),
        appearance_total: node.appearance_error.total,
        bounds_max: bounds_max(&node.bounds),
        transition_total: page.transition_error.total,
        deformed_min: bounds_min(&node.deformed_bounds),
        vertex_count: 0,
        deformed_max: bounds_max(&node.deformed_bounds),
        index_count: 0,
        prototype: subtree_prototype(hierarchy, node),
        ..Default::default()
    };

    let mut clusters = Vec::new();
    let mut voxel_vertices = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    match node.representation {
        HierarchyRepresentation::Triangles { first, count } => {
            header.representation = GpuRepresentation::TriangleCluster as u32;
            for cluster_index in first..first.saturating_add(count) {
                let cluster = hierarchy
                    .triangle_clusters
                    .get(cluster_index as usize)
                    .ok_or_else(|| {
                        payload_error(format!("node {} references missing cluster", node.id))
                    })?;
                let first_index = u32::try_from(indices.len())
                    .map_err(|_| payload_error("page index blob exceeds u32"))?;
                for local in &cluster.local_indices {
                    let vertex = cluster
                        .source_vertices
                        .get(*local as usize)
                        .ok_or_else(|| {
                            payload_error(format!(
                                "cluster {} local index outside its vertex set",
                                cluster.id
                            ))
                        })?;
                    indices.push(*vertex);
                }
                clusters.push(GpuPageClusterRecord {
                    first_index,
                    index_count: u32::try_from(cluster.local_indices.len())
                        .map_err(|_| payload_error("cluster index count exceeds u32"))?,
                    material_slot: cluster.material_slot,
                    prototype: cluster.prototype,
                    bounds_min: bounds_min(&cluster.bounds),
                    appearance_total: cluster.appearance_error.total,
                    bounds_max: bounds_max(&cluster.bounds),
                    cone: u32::from_le_bytes(cluster.cone.map(|axis| axis as u8)),
                    deformed_min: bounds_min(&cluster.deformed_bounds),
                    reserved0: 0,
                    deformed_max: bounds_max(&cluster.deformed_bounds),
                    reserved1: 0,
                });
            }
            header.cluster_count = u32::try_from(clusters.len())
                .map_err(|_| payload_error("page cluster count exceeds u32"))?;
        }
        HierarchyRepresentation::Voxel { brick } => {
            header.representation = GpuRepresentation::AggregateVoxel as u32;
            let brick = hierarchy.voxel_bricks.get(brick as usize).ok_or_else(|| {
                payload_error(format!("node {} references missing brick", node.id))
            })?;
            for vertex in &brick.vertices {
                voxel_vertices.push(GpuPageVoxelVertex {
                    position: vertex.position_bits.map(q15_16),
                    normal_oct: u32::from_le_bytes({
                        let n0 = vertex.normal_oct[0].to_le_bytes();
                        let n1 = vertex.normal_oct[1].to_le_bytes();
                        [n0[0], n0[1], n1[0], n1[1]]
                    }),
                });
            }
            header.vertex_count = u32::try_from(voxel_vertices.len())
                .map_err(|_| payload_error("brick vertex count exceeds u32"))?;
            indices.extend_from_slice(&brick.indices);
        }
    }
    header.index_count =
        u32::try_from(indices.len()).map_err(|_| payload_error("page index blob exceeds u32"))?;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(bytemuck::bytes_of(&header));
    let child_table_offset = bytes.len();
    bytes.extend(std::iter::repeat_n(
        0_u8,
        child_pages.len() * size_of::<GpuHandle>(),
    ));
    pad_to_16(&mut bytes);
    bytes.extend_from_slice(bytemuck::cast_slice(&clusters));
    bytes.extend_from_slice(bytemuck::cast_slice(&voxel_vertices));
    bytes.extend_from_slice(bytemuck::cast_slice(&indices));
    pad_to_16(&mut bytes);

    Ok(PagePayload {
        bytes,
        child_pages,
        child_table_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{
        Mesh, PortableHierarchyInput, Submesh, Vertex, cook_portable_virtual_hierarchy,
    };

    fn cooked_quad() -> PortableVirtualHierarchy {
        let vertices = vec![
            Vertex {
                position: Vec3::new(0.0, 0.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 0.0),
                ..Default::default()
            },
            Vertex {
                position: Vec3::new(1.0, 0.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(1.0, 0.0),
                ..Default::default()
            },
            Vertex {
                position: Vec3::new(0.0, 1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 1.0),
                ..Default::default()
            },
            Vertex {
                position: Vec3::new(1.0, 1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::new(1.0, 1.0),
                ..Default::default()
            },
        ];
        let mesh = Mesh {
            vertices,
            indices: vec![0, 1, 2, 1, 3, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let input = PortableHierarchyInput::from_mesh(&mesh, &[]).expect("input");
        cook_portable_virtual_hierarchy(&input).expect("cook")
    }

    #[test]
    fn payload_locks_header_children_and_geometry_relative_indices() {
        let hierarchy = cooked_quad();
        for page in &hierarchy.pages {
            let payload = build_page_payload(&hierarchy, page.id).expect("payload");
            assert_eq!(payload.bytes.len() % 16, 0, "16-byte-aligned payload");
            let header: &GpuPageNodeRecord =
                bytemuck::from_bytes(&payload.bytes[..size_of::<GpuPageNodeRecord>()]);
            let node = &hierarchy.nodes[page.node as usize];
            assert_eq!(header.child_count as usize, node.children.len());
            assert_eq!(header.transition_total, page.transition_error.total);
            assert_eq!(header.appearance_total, node.appearance_error.total);
            assert_eq!(
                header.flags & GPU_PAGE_PAYLOAD_FLAG_GUARANTEED_ROOT != 0,
                page.guaranteed_root
            );
            assert_eq!(payload.child_pages.len(), node.children.len());
            for (child, child_page) in node.children.iter().zip(&payload.child_pages) {
                assert_eq!(hierarchy.nodes[*child as usize].page, *child_page);
            }

            match node.representation {
                HierarchyRepresentation::Triangles { first, count } => {
                    assert_eq!(
                        header.representation,
                        crate::GpuRepresentation::TriangleCluster as u32
                    );
                    assert_eq!(header.vertex_count, 0);
                    let cluster_base = {
                        let mut offset = size_of::<GpuPageNodeRecord>()
                            + payload.child_pages.len() * size_of::<GpuHandle>();
                        offset = offset.div_ceil(16) * 16;
                        offset
                    };
                    let records: &[GpuPageClusterRecord] = bytemuck::cast_slice(
                        &payload.bytes[cluster_base
                            ..cluster_base
                                + header.cluster_count as usize
                                    * size_of::<GpuPageClusterRecord>()],
                    );
                    let index_base = cluster_base
                        + header.cluster_count as usize * size_of::<GpuPageClusterRecord>();
                    let indices: &[u32] = bytemuck::cast_slice(
                        &payload.bytes[index_base..index_base + header.index_count as usize * 4],
                    );
                    let mut expected = Vec::new();
                    for cluster_index in first..first + count {
                        let cluster = &hierarchy.triangle_clusters[cluster_index as usize];
                        for local in &cluster.local_indices {
                            expected.push(cluster.source_vertices[*local as usize]);
                        }
                        let record = &records[(cluster_index - first) as usize];
                        assert_eq!(
                            record.deformed_min,
                            bounds_min(&cluster.deformed_bounds),
                            "the cluster's swept minimum reaches the record"
                        );
                        assert_eq!(
                            record.deformed_max,
                            bounds_max(&cluster.deformed_bounds),
                            "the cluster's swept maximum reaches the record"
                        );
                        assert!(
                            record.deformed_min <= record.bounds_min
                                && record.deformed_max >= record.bounds_max,
                            "swept bounds enclose the cluster's static bounds"
                        );
                    }
                    assert_eq!(indices, expected, "geometry-relative index blob");
                    let total: u32 = records.iter().map(|record| record.index_count).sum();
                    assert_eq!(total, header.index_count);
                }
                HierarchyRepresentation::Voxel { brick } => {
                    assert_eq!(
                        header.representation,
                        crate::GpuRepresentation::AggregateVoxel as u32
                    );
                    assert_eq!(header.cluster_count, 0);
                    let brick = &hierarchy.voxel_bricks[brick as usize];
                    assert_eq!(header.vertex_count as usize, brick.vertices.len());
                    assert_eq!(header.index_count as usize, brick.indices.len());
                }
            }

            let rebuilt = build_page_payload(&hierarchy, page.id).expect("rebuild");
            assert_eq!(rebuilt, payload, "deterministic payload bytes");
        }
    }

    #[test]
    fn child_patching_overwrites_the_handle_table_in_place() {
        let hierarchy = cooked_quad();
        let root = hierarchy
            .pages
            .iter()
            .find(|page| page.guaranteed_root)
            .expect("root page");
        let mut payload = build_page_payload(&hierarchy, root.id).expect("payload");
        if payload.child_pages.is_empty() {
            return;
        }
        let handle = GpuHandle {
            index: 42,
            generation: 7,
        };
        payload.patch_child(0, handle).expect("patch");
        let offset = payload.child_table_offset;
        let patched: &GpuHandle =
            bytemuck::from_bytes(&payload.bytes[offset..offset + size_of::<GpuHandle>()]);
        assert_eq!(*patched, handle);
        assert!(
            payload
                .patch_child(payload.child_pages.len(), handle)
                .is_err()
        );
    }
}
