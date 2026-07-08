//! Procedural built-in primitive meshes: cube, plane, UV sphere.
//!
//! One authoritative source for the engine's parameter-free primitives. Each returns a
//! [`Mesh`] with the same layout every importer produces — interleaved position / normal /
//! uv0, 32-bit indices, one [`Submesh`]. Tangents are *not* stored (the [`Vertex`] format
//! has none; the shaders derive the tangent frame per-fragment), so these match imported
//! meshes byte-for-byte in shape.
//!
//! Winding is **counter-clockwise as seen from outside**, matching the renderer's
//! `FrontFace::COUNTER_CLOCKWISE` + back-face cull for solid materials (and glTF's mandated
//! CCW front face). All primitives are origin-centered and unit-scaled; an entity's
//! `Transform` sizes them.

use glam::{Vec2, Vec3};

use crate::types::{Mesh, Submesh, Vertex};

/// Appends one CCW-outward quad (two triangles) to `mesh`. `half_u`/`half_v` are the face's
/// half-extent vectors; their cross product points along `normal` (so the winding faces out).
fn push_quad(mesh: &mut Mesh, center: Vec3, half_u: Vec3, half_v: Vec3, normal: Vec3) {
    let base = mesh.vertices.len() as u32;
    // (u, v) corners CCW in the face plane; the flip on the texture V keeps it upright.
    for &(a, b) in &[(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        mesh.vertices.push(Vertex {
            position: center + half_u * a + half_v * b,
            normal,
            uv0: Vec2::new((a + 1.0) * 0.5, 1.0 - (b + 1.0) * 0.5),
            ..Vertex::default()
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// Closes a primitive with a single full-range submesh over its buffers, and computes its
/// UV-aligned tangents (so a primitive carries the same frame an imported mesh does).
fn one_submesh(mesh: &mut Mesh) {
    mesh.submeshes.push(Submesh {
        first_index: 0,
        index_count: mesh.indices.len() as u32,
        vertex_offset: 0,
        material_slot: 0,
    });
    crate::compute_tangents(mesh);
}

/// A unit cube centered at the origin, edge length 1 (`±0.5`), 24 vertices (hard normals
/// per face, 4 per face) and 12 triangles.
pub fn cube() -> Mesh {
    const H: f32 = 0.5;
    let mut mesh = Mesh::default();
    // Each face: (center, u-axis, v-axis, normal) with u × v = normal (CCW-outward).
    let faces = [
        (Vec3::new(H, 0.0, 0.0), Vec3::NEG_Z, Vec3::Y, Vec3::X),
        (Vec3::new(-H, 0.0, 0.0), Vec3::Z, Vec3::Y, Vec3::NEG_X),
        (Vec3::new(0.0, H, 0.0), Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::new(0.0, -H, 0.0), Vec3::X, Vec3::Z, Vec3::NEG_Y),
        (Vec3::new(0.0, 0.0, H), Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::new(0.0, 0.0, -H), Vec3::NEG_X, Vec3::Y, Vec3::NEG_Z),
    ];
    for (center, u, v, n) in faces {
        push_quad(&mut mesh, center, u * H, v * H, n);
    }
    one_submesh(&mut mesh);
    mesh
}

/// A unit plane on the XZ axes centered at the origin (`1×1`), facing +Y — one quad,
/// 4 vertices, 2 triangles.
pub fn plane() -> Mesh {
    let mut mesh = Mesh::default();
    push_quad(&mut mesh, Vec3::ZERO, Vec3::Z * 0.5, Vec3::X * 0.5, Vec3::Y);
    one_submesh(&mut mesh);
    mesh
}

/// A unit UV sphere centered at the origin, radius 1 (normals == positions), subdivided into
/// `rings × sectors`. `uv_repeat` tiles the surface texture (`uv0` spans `0..uv_repeat`) — integer
/// components keep a tiling texture seamless across the `s = 0` / `s = sectors` wrap seam and the
/// poles. The poles collapse a ring to a point.
fn uv_sphere_with(rings: u32, sectors: u32, uv_repeat: Vec2) -> Mesh {
    let mut mesh = Mesh::default();
    for r in 0..=rings {
        let phi = std::f32::consts::PI * (r as f32) / (rings as f32);
        for s in 0..=sectors {
            let theta = 2.0 * std::f32::consts::PI * (s as f32) / (sectors as f32);
            let position = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.vertices.push(Vertex {
                position,
                normal: position,
                uv0: Vec2::new(
                    (s as f32) / (sectors as f32) * uv_repeat.x,
                    (r as f32) / (rings as f32) * uv_repeat.y,
                ),
                ..Vertex::default()
            });
        }
    }
    let stride = sectors + 1;
    for r in 0..rings {
        for s in 0..sectors {
            let a = r * stride + s;
            let b = a + stride;
            // CCW-outward: [a, a+1, b] + [a+1, b+1, b].
            mesh.indices
                .extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    one_submesh(&mut mesh);
    mesh
}

/// A unit UV sphere centered at the origin, radius 1 (normals == positions). 32 rings ×
/// 48 sectors, texture wrapping once.
pub fn uv_sphere() -> Mesh {
    uv_sphere_with(32, 48, Vec2::ONE)
}

/// The preview sphere's surface-texture tiling. A once-wrapped sphere shows only ~half its texture
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_has_six_quads() {
        let m = cube();
        assert_eq!(m.vertices.len(), 24);
        assert_eq!(m.indices.len(), 36);
        assert_eq!(m.submeshes.len(), 1);
        for v in &m.vertices {
            assert!((v.normal.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn plane_faces_up() {
        let m = plane();
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.indices.len(), 6);
        for v in &m.vertices {
            assert_eq!(v.normal, Vec3::Y);
            assert!(v.position.y.abs() < 1e-6);
        }
    }

    #[test]
    fn sphere_vertices_lie_on_unit_sphere() {
        let m = uv_sphere();
        assert!(!m.indices.is_empty());
        assert_eq!(m.submeshes[0].index_count as usize, m.indices.len());
        for v in &m.vertices {
            assert!((v.position.length() - 1.0).abs() < 1e-4);
            assert!((v.normal.length() - 1.0).abs() < 1e-4);
        }
    }

    /// Every non-degenerate triangle (the poles collapse to a point) winds CCW as seen
    /// from outside — its geometric normal agrees with the outward radial — matching the
    /// renderer's `FrontFace::COUNTER_CLOCKWISE` + back-face cull for solid materials.
    #[test]
    fn sphere_winding_is_ccw_outward() {
        let m = uv_sphere();
        let p = |i: u32| m.vertices[i as usize].position;
        let mut checked = 0;
        for tri in m.indices.chunks_exact(3) {
            let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
            let geo_normal = (b - a).cross(c - a);
            // Skip degenerate pole triangles (two coincident vertices → zero area).
            if geo_normal.length() < 1e-6 {
                continue;
            }
            let outward = (a + b + c) / 3.0;
            assert!(
                geo_normal.dot(outward) > 0.0,
                "every triangle must wind CCW-outward"
            );
            checked += 1;
        }
        assert!(checked > 0);
    }
}
