//! Shared cook fixtures for the hierarchy unit tests.

use glam::{Vec2, Vec3};

use crate::{Mesh, Submesh, Vertex};

use super::types::{
    MicroInstance, PortableAggregationMode, PortableBounds, PortableHierarchyInput,
    PortableSourceMesh, PortableSourceSubmesh, PortableSourceVertex, VirtualHierarchyMaterial,
    identity_transform_bits,
};

/// A UV sphere of `radius` metres, dense enough to clusterize into far more leaves than one
/// node may carry.
pub(super) fn uv_sphere_input(radius: f32) -> PortableHierarchyInput {
    const RINGS: u32 = 48;
    const SEGMENTS: u32 = 64;
    let mut vertices = Vec::new();
    for ring in 0..=RINGS {
        let v = f64::from(ring) / f64::from(RINGS);
        let theta = v * std::f64::consts::PI;
        for segment in 0..=SEGMENTS {
            let u = f64::from(segment) / f64::from(SEGMENTS);
            let phi = u * std::f64::consts::TAU;
            let normal = Vec3::new(
                (theta.sin() * phi.cos()) as f32,
                theta.cos() as f32,
                (theta.sin() * phi.sin()) as f32,
            );
            vertices.push(Vertex {
                position: normal * radius,
                normal,
                uv0: Vec2::new(u as f32, v as f32),
                ..Vertex::default()
            });
        }
    }
    let mut indices = Vec::new();
    for ring in 0..RINGS {
        for segment in 0..SEGMENTS {
            let top = ring * (SEGMENTS + 1) + segment;
            let bottom = top + SEGMENTS + 1;
            indices.extend([top, bottom, top + 1, top + 1, bottom, bottom + 1]);
        }
    }
    let index_count = indices.len() as u32;
    let mesh = Mesh {
        vertices,
        indices,
        submeshes: vec![Submesh {
            first_index: 0,
            index_count,
            vertex_offset: 0,
            material_slot: 0,
        }],
    };
    PortableHierarchyInput::from_mesh(&mesh, &[]).expect("uv sphere input")
}

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
