//! Import-time **watertight conditioning** of a base mesh: welded-vertex identity, edge
//! adjacency, UV-seam detection, and a per-welded displacement direction + seam-consistent
//! tangent seed — plus the per-height-texture min-max pyramid.
//!
//! These are the data the adaptive tessellator (Phase 3 factors, Phase 4 dice/weld) consumes to
//! amplify a base triangle into displaced micro-geometry **without cracks**: a shared edge diced by
//! both incident triangles resolves to one [`Edge`] (so both read the same tessellation factor), a
//! welded vertex carries one agreed displacement direction (so every base copy of a seam vertex
//! lands on one 3D point), and a seam edge is flagged so the two islands sample an equal height.
//!
//! Everything here is a **pure function of the mesh** (the seam *sampling mode* aside, which the
//! material import may refine over the height image), so it is baked into every `.smesh` v5 and
//! verified entirely on the CPU — no GPU, no dependence on the later dicer.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glam::{Vec2, Vec3};

use crate::types::Mesh;

/// Edge flag: referenced by exactly one triangle — an open mesh boundary.
pub const EDGE_BOUNDARY: u32 = 1 << 0;
/// Edge flag: a UV seam — the two incident triangles assign different UVs to the shared endpoints,
/// so watertight displacement requires both to read an *equal* height value there.
pub const EDGE_SEAM: u32 = 1 << 1;
/// Edge flag: referenced by more than two triangles — non-manifold, factored by a clamped
/// world-space fallback at Phase 3 rather than cracking.
pub const EDGE_NON_MANIFOLD: u32 = 1 << 2;
/// Edge flag (only meaningful with [`EDGE_SEAM`]): sample this seam's height in object space /
/// triplanar so both islands read one value independent of UV. Clear ⇒ the default seam-aware
/// dilation path ([`EDGE_SEAM`] alone), which the material import reconciles + verifies on the image.
pub const EDGE_SEAM_OBJECT_SPACE: u32 = 1 << 3;

/// One unique mesh edge, keyed on its **welded** endpoint pair (canonical `v0 < v1`).
///
/// Exactly 16 bytes, `#[repr(C)]` Pod: serializes into the `.smesh` conditioning section and uploads
/// as a storage buffer with no re-pack. Both triangles sharing this edge resolve to the same record,
/// so a per-edge tessellation factor is indexable identically by either.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct Edge {
    /// Lower welded endpoint id.
    pub v0: u32,
    /// Higher welded endpoint id.
    pub v1: u32,
    /// `EDGE_BOUNDARY | EDGE_SEAM | EDGE_NON_MANIFOLD | EDGE_SEAM_OBJECT_SPACE`.
    pub flags: u32,
    /// Pad to 16 bytes, always 0.
    pub _pad: u32,
}

/// A triangle's three edge indices into the unique-edge list, parallel to
/// `indices.chunks_exact(3)`. `e[k]` is the edge between welded corner `k` and corner `(k+1)%3`.
///
/// Exactly 16 bytes, `#[repr(C)]` Pod.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct TriEdges {
    /// Edge indices for corners (0-1, 1-2, 2-0).
    pub e: [u32; 3],
    /// Pad to 16 bytes, always 0.
    pub _pad: u32,
}

/// The position + shading/displacement basis of one welded vertex.
///
/// Exactly 64 bytes, `#[repr(C)]` Pod, std430-clean (four 16-byte vectors). `position.xyz` is the
/// shared object-space position (every base copy of this welded vertex coincides here — the factor
/// kernel reads endpoint positions from it, indexed by an [`Edge`]'s welded ids, and the dicer
/// interpolates the base triangle from it). `direction.xyz` is the unit displacement direction (the
/// averaged, renormalized geometric normal, so every copy displaces to one point). `tangent.xyz` is
/// the seam-consistent tangent seed (Gram-Schmidt against the direction), `tangent.w` the ±1
/// handedness. `uv.xy` is the representative base UV (`zw` = 0), stored last-write like `position` so
/// both incident triangles of a shared edge read one agreed UV — the factor kernel samples the
/// per-height min/max pyramid over each edge's UV span from it, giving per-region detail-adaptive LOD
/// while staying a pure function of the shared endpoints (crack-free).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct WeldedVertex {
    /// Shared object-space position in `xyz`; `w` is 0 (std430 padding).
    pub position: [f32; 4],
    /// Unit displacement direction in `xyz`; `w` is 0 (std430 padding).
    pub direction: [f32; 4],
    /// Seam-consistent tangent seed in `xyz`; `w` is the ±1 bitangent handedness.
    pub tangent: [f32; 4],
    /// Representative base UV in `xy`; `zw` are 0 (std430 padding).
    pub uv: [f32; 4],
}

/// The full watertight-conditioning payload of one mesh: the unique-edge list, per-triangle edge
/// indices, per-welded-vertex basis, and the base-vertex → welded-id map.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshConditioning {
    /// Unique edges keyed on welded endpoint pairs.
    pub edges: Vec<Edge>,
    /// Per-triangle edge indices, parallel to the mesh's triangles.
    pub tri_edges: Vec<TriEdges>,
    /// Per-welded-vertex displacement direction + tangent seed.
    pub welded: Vec<WeldedVertex>,
    /// Base vertex index → welded vertex id.
    pub weld_id: Vec<u32>,
}

/// The mesh AABB (min, max); `(ZERO, ZERO)` for an empty mesh.
fn aabb(mesh: &Mesh) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for v in &mesh.vertices {
        lo = lo.min(v.position);
        hi = hi.max(v.position);
    }
    if mesh.vertices.is_empty() {
        (Vec3::ZERO, Vec3::ZERO)
    } else {
        (lo, hi)
    }
}

/// The scale-relative weld tolerance: a small fraction of the AABB diagonal, floored so a
/// degenerate (zero-extent) mesh still welds by exact position. Coincident base vertices (a UV seam
/// or a hard-normal split share one 3D position) collapse; genuinely distinct vertices do not.
fn weld_epsilon(lo: Vec3, hi: Vec3) -> f32 {
    ((hi - lo).length() * 1e-5).max(1e-6)
}

/// Quantizes a position onto the weld grid; bit-coincident positions map to one cell.
fn quantize(p: Vec3, eps: f32) -> [i64; 3] {
    [
        (p.x / eps).round() as i64,
        (p.y / eps).round() as i64,
        (p.z / eps).round() as i64,
    ]
}

/// A stable perpendicular frame from a unit direction (Duff et al. 2017) — the tangent fallback when
/// no usable UV gradient survives Gram-Schmidt.
fn branchless_tangent(n: Vec3) -> Vec3 {
    let s = if n.z >= 0.0 { 1.0 } else { -1.0 };
    let a = -1.0 / (s + n.z);
    Vec3::new(1.0 + s * n.x * n.x * a, s * n.x * n.y * a, -s * n.x).normalize_or_zero()
}

impl MeshConditioning {
    /// Conditions a base mesh: weld first (so edges key on welded identity, not attribute-split base
    /// identity — otherwise a UV seam reads as a boundary and its factor is never shared), then edge
    /// adjacency + seam detection. A pure function of the mesh; the tangent seed reads the base
    /// [`crate::Vertex::tangent`] frame [`crate::compute_tangents`] already produced.
    pub fn build(mesh: &Mesh) -> Self {
        let n = mesh.vertices.len();
        let (lo, hi) = aabb(mesh);
        let eps = weld_epsilon(lo, hi);

        // --- Weld: base vertex → welded id, averaging direction + tangent per welded vertex. ---
        let mut cell: HashMap<[i64; 3], u32> = HashMap::new();
        let mut weld_id = vec![0u32; n];
        let mut welded_count = 0u32;
        for (i, v) in mesh.vertices.iter().enumerate() {
            let key = quantize(v.position, eps);
            let id = *cell.entry(key).or_insert_with(|| {
                let id = welded_count;
                welded_count += 1;
                id
            });
            weld_id[i] = id;
        }

        let wc = welded_count as usize;
        let mut position = vec![Vec3::ZERO; wc];
        let mut uv = vec![Vec2::ZERO; wc];
        let mut dir_sum = vec![Vec3::ZERO; wc];
        let mut tan_sum = vec![Vec3::ZERO; wc];
        let mut handed_sum = vec![0.0f32; wc];
        for (i, v) in mesh.vertices.iter().enumerate() {
            let w = weld_id[i] as usize;
            // All base copies of a welded vertex coincide, so any one is the shared position; the UV
            // is representative (last-write) so both incident triangles of a shared edge agree on it.
            position[w] = v.position;
            uv[w] = v.uv0;
            dir_sum[w] += v.normal;
            tan_sum[w] += Vec3::new(v.tangent[0], v.tangent[1], v.tangent[2]);
            handed_sum[w] += v.tangent[3];
        }
        let mut welded = Vec::with_capacity(wc);
        for w in 0..wc {
            let mut direction = dir_sum[w].normalize_or_zero();
            if direction.length_squared() < 0.5 {
                direction = Vec3::Y;
            }
            let handed = if handed_sum[w] < 0.0 { -1.0 } else { 1.0 };
            // Gram-Schmidt the averaged tangent against the welded direction, so both incident
            // triangles at a seam start from one agreed frame.
            let mut tangent =
                (tan_sum[w] - direction * direction.dot(tan_sum[w])).normalize_or_zero();
            if tangent.length_squared() < 0.5 || !tangent.is_finite() {
                tangent = branchless_tangent(direction);
            }
            let p = position[w];
            let t = uv[w];
            welded.push(WeldedVertex {
                position: [p.x, p.y, p.z, 0.0],
                direction: [direction.x, direction.y, direction.z, 0.0],
                tangent: [tangent.x, tangent.y, tangent.z, handed],
                uv: [t.x, t.y, 0.0, 0.0],
            });
        }

        // --- Edge adjacency over welded ids, with per-edge UV records for seam detection. ---
        let mut edge_map: HashMap<(u32, u32), u32> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut incident: Vec<u32> = Vec::new();
        let mut edge_uv: Vec<Vec<(Vec2, Vec2)>> = Vec::new();
        let mut tri_edges: Vec<TriEdges> = Vec::with_capacity(mesh.indices.len() / 3);

        for tri in mesh.indices.chunks_exact(3) {
            let base = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            if base.iter().any(|&b| b >= n) {
                tri_edges.push(TriEdges::default());
                continue;
            }
            let w = [weld_id[base[0]], weld_id[base[1]], weld_id[base[2]]];
            let mut te = [0u32; 3];
            for k in 0..3 {
                let (a, b) = (w[k], w[(k + 1) % 3]);
                let (ba, bb) = (base[k], base[(k + 1) % 3]);
                let (key, uv_lo, uv_hi) = if a < b {
                    ((a, b), mesh.vertices[ba].uv0, mesh.vertices[bb].uv0)
                } else {
                    ((b, a), mesh.vertices[bb].uv0, mesh.vertices[ba].uv0)
                };
                let idx = *edge_map.entry(key).or_insert_with(|| {
                    let idx = edges.len() as u32;
                    edges.push(Edge {
                        v0: key.0,
                        v1: key.1,
                        flags: 0,
                        _pad: 0,
                    });
                    incident.push(0);
                    edge_uv.push(Vec::new());
                    idx
                });
                incident[idx as usize] += 1;
                edge_uv[idx as usize].push((uv_lo, uv_hi));
                te[k] = idx;
            }
            tri_edges.push(TriEdges { e: te, _pad: 0 });
        }

        // --- Finalize edge flags: boundary / non-manifold / UV seam. ---
        const UV_EPS: f32 = 1e-5;
        for (i, edge) in edges.iter_mut().enumerate() {
            match incident[i] {
                1 => edge.flags |= EDGE_BOUNDARY,
                2 => {
                    let recs = &edge_uv[i];
                    let (a0, a1) = recs[0];
                    let (b0, b1) = recs[1];
                    if (a0 - b0).length() > UV_EPS || (a1 - b1).length() > UV_EPS {
                        edge.flags |= EDGE_SEAM;
                    }
                }
                c if c > 2 => edge.flags |= EDGE_NON_MANIFOLD,
                _ => {}
            }
        }

        Self {
            edges,
            tri_edges,
            welded,
            weld_id,
        }
    }
}

/// One level of a [`build_min_max_pyramid`]: the `(min, max)` height bound per texel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MinMaxLevel {
    /// Level width in texels.
    pub width: u32,
    /// Level height in texels.
    pub height: u32,
    /// `[min, max]` height per texel, row-major.
    pub texels: Vec<[f32; 2]>,
}

/// Builds a **min-max (max-mipmap) pyramid** over a height channel: level 0 is `(h, h)` per texel,
/// each coarser level the exact per-texel `(min, max)` of its up-to-2×2 children, down to 1×1.
///
/// This is **not** a linear mip average — the prism march (Phase 9) and offset-limited parallax (D3)
/// need a *conservative* bound, so it must be point-sampled with explicit LOD, never linearly
/// filtered (linear filtering blends min with max and breaks the bound). Built CPU-side for exactness
/// and self-verifiability. Returns an empty pyramid for a zero-extent input.
pub fn build_min_max_pyramid(height: &[f32], width: u32, height_px: u32) -> Vec<MinMaxLevel> {
    if width == 0 || height_px == 0 || (width as usize * height_px as usize) != height.len() {
        return Vec::new();
    }
    let level0 = MinMaxLevel {
        width,
        height: height_px,
        texels: height.iter().map(|&h| [h, h]).collect(),
    };
    let mut levels = vec![level0];
    while levels
        .last()
        .map(|l| l.width > 1 || l.height > 1)
        .unwrap_or(false)
    {
        let prev = levels.last().unwrap();
        let nw = prev.width.div_ceil(2).max(1);
        let nh = prev.height.div_ceil(2).max(1);
        let mut texels = Vec::with_capacity(nw as usize * nh as usize);
        for y in 0..nh {
            for x in 0..nw {
                let mut mn = f32::INFINITY;
                let mut mx = f32::NEG_INFINITY;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let sx = x * 2 + dx;
                        let sy = y * 2 + dy;
                        if sx < prev.width && sy < prev.height {
                            let t = prev.texels[(sy * prev.width + sx) as usize];
                            mn = mn.min(t[0]);
                            mx = mx.max(t[1]);
                        }
                    }
                }
                texels.push([mn, mx]);
            }
        }
        levels.push(MinMaxLevel {
            width: nw,
            height: nh,
            texels,
        });
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Submesh, Vertex};

    /// A flat quad on XZ (two triangles sharing the diagonal), UVs continuous from position.
    fn quad() -> Mesh {
        let v = |x: f32, z: f32| Vertex {
            position: Vec3::new(x, 0.0, z),
            normal: Vec3::Y,
            uv0: Vec2::new(x, z),
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        Mesh {
            vertices: vec![v(0.0, 0.0), v(1.0, 0.0), v(1.0, 1.0), v(0.0, 1.0)],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        }
    }

    #[test]
    fn interior_edge_shared_by_two_triangles_boundary_by_one() {
        let c = MeshConditioning::build(&quad());
        // 5 edges: 4 outer (boundary, 1 tri each) + 1 diagonal (interior, 2 tris).
        assert_eq!(c.edges.len(), 5);
        assert_eq!(c.tri_edges.len(), 2);
        let boundary = c
            .edges
            .iter()
            .filter(|e| e.flags & EDGE_BOUNDARY != 0)
            .count();
        let interior = c
            .edges
            .iter()
            .filter(|e| e.flags & EDGE_BOUNDARY == 0)
            .count();
        assert_eq!(boundary, 4, "four outer edges, one triangle each");
        assert_eq!(interior, 1, "one shared diagonal, two triangles");
        // Every referenced edge resolves back to a canonical (v0 < v1) unique-edge record.
        for tri in &c.tri_edges {
            for &e in &tri.e {
                let edge = c.edges[e as usize];
                assert!(edge.v0 < edge.v1);
            }
        }
    }

    #[test]
    fn coincident_positions_weld_and_seam_is_detected() {
        // Two triangles meeting along x=1 (welded), but the right triangle uses a *different* UV on
        // the shared edge — a UV seam at one welded position pair.
        let v = |x: f32, z: f32, u: f32, w: f32| Vertex {
            position: Vec3::new(x, 0.0, z),
            normal: Vec3::Y,
            uv0: Vec2::new(u, w),
            tangent: [1.0, 0.0, 0.0, 1.0],
        };
        let mesh = Mesh {
            vertices: vec![
                // left tri: (0,0)-(1,0)-(1,1) with continuous uv
                v(0.0, 0.0, 0.0, 0.0),
                v(1.0, 0.0, 0.0, 1.0),
                v(1.0, 0.0, 1.0, 1.0),
                // right tri: shares the (1,0)-(1,1) edge in 3D but with a jumped uv (seam)
                v(1.0, 0.0, 0.0, 0.0), // same position as vtx 1, different uv
                v(1.0, 0.0, 1.0, 0.0), // same position as vtx 2, different uv
                v(2.0, 0.0, 0.5, 0.5),
            ],
            indices: vec![0, 1, 2, 3, 5, 4],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let c = MeshConditioning::build(&mesh);
        // Positions (1,0) and (1,1) each appear twice → collapse to one welded id apiece.
        assert_eq!(c.weld_id[1], c.weld_id[3], "coincident (1,0,0) welds");
        assert_eq!(c.weld_id[2], c.weld_id[4], "coincident (1,0,1) welds");
        // The shared edge is interior (2 triangles) and flagged a seam (divergent UVs).
        let seam = c.edges.iter().find(|e| e.flags & EDGE_SEAM != 0);
        assert!(seam.is_some(), "the divergent-UV shared edge is a seam");
        let seam = seam.unwrap();
        assert_eq!(
            seam.flags & EDGE_BOUNDARY,
            0,
            "a shared seam is not a boundary"
        );
    }

    #[test]
    fn welded_direction_and_tangent_are_unit_and_finite() {
        let c = MeshConditioning::build(&quad());
        for w in &c.welded {
            let d = Vec3::new(w.direction[0], w.direction[1], w.direction[2]);
            let t = Vec3::new(w.tangent[0], w.tangent[1], w.tangent[2]);
            assert!(
                d.is_finite() && (d.length() - 1.0).abs() < 1e-4,
                "unit direction"
            );
            assert!(
                t.is_finite() && (t.length() - 1.0).abs() < 1e-4,
                "unit tangent"
            );
            assert!(d.dot(t).abs() < 1e-3, "tangent orthogonal to direction");
            assert!(w.tangent[3] == 1.0 || w.tangent[3] == -1.0, "±1 handedness");
        }
    }

    #[test]
    fn welded_position_is_the_shared_endpoint_position() {
        let mesh = quad();
        let c = MeshConditioning::build(&mesh);
        // Each welded vertex's stored position matches every base vertex that welds to it.
        for (base, &w) in c.weld_id.iter().enumerate() {
            let wp = c.welded[w as usize].position;
            let bp = mesh.vertices[base].position;
            assert!((wp[0] - bp.x).abs() < 1e-6);
            assert!((wp[1] - bp.y).abs() < 1e-6);
            assert!((wp[2] - bp.z).abs() < 1e-6);
        }
    }

    #[test]
    fn welded_uv_is_a_representative_base_uv() {
        let mesh = quad();
        let c = MeshConditioning::build(&mesh);
        // Each welded vertex's stored UV matches a base vertex that welds to it (last-write
        // representative), so both incident triangles of a shared edge read one agreed UV.
        for (base, &w) in c.weld_id.iter().enumerate() {
            let wu = c.welded[w as usize].uv;
            let bu = mesh.vertices[base].uv0;
            // Some other base copy may have overwritten it, but the stored UV is always the uv0 of
            // *some* base copy — for the continuous quad every copy is unique, so it matches exactly.
            assert!((wu[0] - bu.x).abs() < 1e-6);
            assert!((wu[1] - bu.y).abs() < 1e-6);
            assert_eq!(wu[2], 0.0);
            assert_eq!(wu[3], 0.0);
        }
    }

    #[test]
    fn pyramid_levels_are_exact_min_max_of_children() {
        // 3×3 (odd extents) so the down-step exercises the clamped 2×2 gather.
        let w = 3u32;
        let h = 3u32;
        let data: Vec<f32> = (0..9).map(|i| i as f32).collect();
        let p = build_min_max_pyramid(&data, w, h);
        assert_eq!(p[0].width, 3);
        assert_eq!(p[0].height, 3);
        assert_eq!(p[0].texels[4], [4.0, 4.0], "level 0 is (h,h)");
        // Level 1 is 2×2.
        assert_eq!(p[1].width, 2);
        assert_eq!(p[1].height, 2);
        // texel (0,0) covers source (0,0),(1,0),(0,1),(1,1) = {0,1,3,4}.
        assert_eq!(p[1].texels[0], [0.0, 4.0]);
        // texel (1,0) covers source (2,0),(2,1) (x=3 clamped) = {2,5}.
        assert_eq!(p[1].texels[1], [2.0, 5.0]);
        // The last level is 1×1 and bounds the whole image.
        let top = p.last().unwrap();
        assert_eq!((top.width, top.height), (1, 1));
        assert_eq!(top.texels[0], [0.0, 8.0]);
    }

    #[test]
    fn pyramid_rejects_mismatched_extent() {
        assert!(build_min_max_pyramid(&[1.0, 2.0], 4, 4).is_empty());
        assert!(build_min_max_pyramid(&[], 0, 0).is_empty());
    }
}
