use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{Mesh, Submesh, Vertex};

use crate::Device;
use crate::device::SurfaceSource;

/// Builds a headless device or skips the test (no Vulkan ICD in this toolbox).
pub(super) fn device_or_skip() -> Option<Device> {
    match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => Some(device),
        Err(err) => {
            eprintln!("skipping: no Vulkan device obtainable ({err})");
            None
        }
    }
}

/// A single-triangle mesh, the minimal valid upload input.
pub(super) fn triangle() -> Mesh {
    let v = |x: f32, y: f32| Vertex {
        position: Vec3::new(x, y, 0.0),
        normal: Vec3::new(0.0, 0.0, 1.0),
        uv0: Vec2::ZERO,
        ..Vertex::default()
    };
    Mesh {
        vertices: vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)],
        indices: vec![0, 1, 2],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 3,
            vertex_offset: 0,
            material_slot: 0,
        }],
    }
}
