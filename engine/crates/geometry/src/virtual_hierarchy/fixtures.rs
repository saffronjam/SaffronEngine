//! Shared cook fixtures for the hierarchy unit tests.

use super::types::{
    MicroInstance, PortableAggregationMode, PortableBounds, PortableHierarchyInput,
    PortableSourceMesh, PortableSourceSubmesh, PortableSourceVertex, VirtualHierarchyMaterial,
    identity_transform_bits,
};

pub(super) fn source_vertex(position_bits: [i32; 3], uv_bits: [i32; 2]) -> PortableSourceVertex {
    PortableSourceVertex {
        position_bits,
        normal_snorm: [0, 0, i16::MAX],
        uv_bits,
        tangent_snorm: [i16::MAX, 0, 0, i16::MAX],
    }
}

pub(super) fn multi_page_input() -> PortableHierarchyInput {
    PortableHierarchyInput {
        combinations: Vec::new(),
        meshes: vec![PortableSourceMesh {
            source: 7,
            selector_hash: [11; 32],
            vertices: vec![
                source_vertex([0, 0, 0], [0, 0]),
                source_vertex([65_536, 0, 0], [65_536, 0]),
                source_vertex([65_536, 65_536, 0], [65_536, 65_536]),
                source_vertex([0, 65_536, 0], [0, 65_536]),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![
                PortableSourceSubmesh {
                    first_index: 0,
                    index_count: 3,
                    material: VirtualHierarchyMaterial::opaque(0),
                },
                PortableSourceSubmesh {
                    first_index: 3,
                    index_count: 3,
                    material: VirtualHierarchyMaterial::opaque(1),
                },
            ],
            skin: Vec::new(),
            aggregation: PortableAggregationMode::Contiguous,
        }],
        micro_instances: vec![MicroInstance {
            part: 7,
            prototype: 0,
            transform_bits: identity_transform_bits(),
        }],
        deformation: Vec::new(),
        bounds: PortableBounds {
            min_bits: [0, 0, 0],
            max_bits: [65_536, 65_536, 0],
        },
        root_material: VirtualHierarchyMaterial::opaque(0),
        deformation_padding: 0,
    }
}
