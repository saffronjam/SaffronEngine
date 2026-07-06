//! Meshlet clustering for the `VK_EXT_mesh_shader` raster front end.
//!
//! A meshlet is a small, self-contained cluster of triangles (≤ [`MESHLET_MAX_VERTICES`] unique
//! vertices, ≤ [`MESHLET_MAX_TRIANGLES`] triangles) the mesh shader emits in one workgroup, and the
//! task shader culls/LODs as a unit. The engine builds meshlets **per submesh** so a meshlet inherits
//! exactly one material slot — the mesh-shader draw records one dispatch per submesh, binding that
//! submesh's material exactly as the index-draw path does, and only the geometry front end differs.
//!
//! The layout mirrors the standard three-array meshlet form the mesh shader consumes:
//! - [`MeshletSet::meshlets`] — one [`Meshlet`] descriptor per cluster (offsets + counts + bounds).
//! - [`MeshletSet::vertices`] — flat global vertex indices; a meshlet's slice is its unique vertices.
//! - [`MeshletSet::triangles`] — flat *local* indices (into the meshlet's vertex slice), 3 per
//!   triangle, `u8` because a meshlet never references more than 64 local vertices.

use bytemuck::{Pod, Zeroable};
use glam::Vec3;

use crate::types::Mesh;

/// Max unique vertices per meshlet — the common `VK_EXT_mesh_shader` sweet spot (fits the per-workgroup
/// output-vertex budget on every desktop implementation).
pub const MESHLET_MAX_VERTICES: usize = 64;
/// Max triangles per meshlet. 124 (not 128) keeps the packed local-index array a multiple of 4 bytes
/// (`124 * 3 = 372`) and stays within the 256 primitive-output cap with headroom.
pub const MESHLET_MAX_TRIANGLES: usize = 124;

/// One meshlet descriptor: where its unique vertices and local triangles live in the flat arrays,
/// how many of each, and a bounding sphere the task shader frustum-culls against.
///
/// Exactly 32 bytes, `#[repr(C)]` Pod — uploaded verbatim as a GPU storage-buffer element.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Meshlet {
    /// First index into [`MeshletSet::vertices`] for this meshlet's unique vertices.
    pub vertex_offset: u32,
    /// First index into [`MeshletSet::triangles`] for this meshlet's local indices (3 per triangle).
    pub triangle_offset: u32,
    /// Number of unique vertices (≤ [`MESHLET_MAX_VERTICES`]).
    pub vertex_count: u32,
    /// Number of triangles (≤ [`MESHLET_MAX_TRIANGLES`]).
    pub triangle_count: u32,
    /// Object-space bounding-sphere center (task-shader cull).
    pub center: Vec3,
    /// Object-space bounding-sphere radius.
    pub radius: f32,
}

/// A mesh's full meshlet decomposition: the descriptors, the flat vertex-index and local-triangle
/// arrays, and the per-submesh meshlet ranges so the draw can bind one material per submesh.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshletSet {
    /// All meshlets, grouped by submesh in [`MeshletSet::submesh_ranges`] order.
    pub meshlets: Vec<Meshlet>,
    /// Flat global vertex indices; sliced by each meshlet's `vertex_offset`/`vertex_count`.
    pub vertices: Vec<u32>,
    /// Flat local triangle indices (3 per triangle), sliced by `triangle_offset`/`triangle_count·3`.
    pub triangles: Vec<u8>,
    /// `(first_meshlet, meshlet_count)` per submesh, parallel to [`Mesh::submeshes`].
    pub submesh_ranges: Vec<(u32, u32)>,
}

impl MeshletSet {
    /// Total meshlet count across all submeshes.
    pub fn len(&self) -> usize {
        self.meshlets.len()
    }

    /// Whether the mesh produced no meshlets (empty geometry).
    pub fn is_empty(&self) -> bool {
        self.meshlets.is_empty()
    }
}

/// A meshlet under construction: its unique global vertices in local-slot order (`locals[slot]`) plus
/// its packed local triangle indices, flushed into the [`MeshletSet`] when a triangle would overflow a
/// limit. `locals` is scanned linearly for a vertex's local slot — cheap since it never exceeds 64.
struct Builder {
    locals: Vec<u32>,
    tris: Vec<[u8; 3]>,
}

impl Builder {
    fn new() -> Self {
        Self {
            locals: Vec::with_capacity(MESHLET_MAX_VERTICES),
            tris: Vec::with_capacity(MESHLET_MAX_TRIANGLES),
        }
    }

    /// Local slot of a global vertex index within this meshlet, if already present.
    fn local_of(&self, global: u32) -> Option<u8> {
        self.locals
            .iter()
            .position(|&k| k == global)
            .map(|s| s as u8)
    }

    /// How many of a triangle's three vertices are not yet in this meshlet (deduping a degenerate
    /// triangle that names the same fresh vertex more than once).
    fn fresh_count(&self, tri: [u32; 3]) -> usize {
        let mut count = 0;
        for (a, &g) in tri.iter().enumerate() {
            if self.local_of(g).is_some() || tri[..a].contains(&g) {
                continue;
            }
            count += 1;
        }
        count
    }

    /// Appends a triangle, inserting any fresh vertices and recording its local indices.
    fn push(&mut self, tri: [u32; 3]) {
        let mut local = [0u8; 3];
        for (i, &global) in tri.iter().enumerate() {
            local[i] = match self.local_of(global) {
                Some(slot) => slot,
                None => {
                    let slot = self.locals.len() as u8;
                    self.locals.push(global);
                    slot
                }
            };
        }
        self.tris.push(local);
    }

    fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Flushes this meshlet into `out` (computing its bounding sphere from `mesh`) and resets.
    fn flush(&mut self, mesh: &Mesh, out: &mut MeshletSet) {
        if self.tris.is_empty() {
            return;
        }
        let vertex_offset = out.vertices.len() as u32;
        let triangle_offset = out.triangles.len() as u32;
        let (center, radius) = bounding_sphere(mesh, &self.locals);
        out.meshlets.push(Meshlet {
            vertex_offset,
            triangle_offset,
            vertex_count: self.locals.len() as u32,
            triangle_count: self.tris.len() as u32,
            center,
            radius,
        });
        out.vertices.extend_from_slice(&self.locals);
        for tri in &self.tris {
            out.triangles.extend_from_slice(tri);
        }
        self.locals.clear();
        self.tris.clear();
    }
}

/// The object-space bounding sphere of a meshlet's vertices: the AABB center, and the radius as the
/// max vertex distance from it (a tight-enough sphere for task-shader frustum culling).
fn bounding_sphere(mesh: &Mesh, locals: &[u32]) -> (Vec3, f32) {
    if locals.is_empty() {
        return (Vec3::ZERO, 0.0);
    }
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for &g in locals {
        let p = mesh.vertices[g as usize].position;
        min = min.min(p);
        max = max.max(p);
    }
    let center = (min + max) * 0.5;
    let radius = locals
        .iter()
        .map(|&g| mesh.vertices[g as usize].position.distance(center))
        .fold(0.0_f32, f32::max);
    (center, radius)
}

/// Clusters a mesh into meshlets, per submesh, via a greedy fill: triangles accumulate into the
/// current meshlet until one would exceed [`MESHLET_MAX_VERTICES`] or [`MESHLET_MAX_TRIANGLES`], then
/// it flushes and a fresh meshlet opens. Preserves submesh boundaries in [`MeshletSet::submesh_ranges`].
pub fn build_meshlets(mesh: &Mesh) -> MeshletSet {
    let mut out = MeshletSet::default();
    let n = mesh.vertices.len() as u32;
    let submeshes: &[crate::types::Submesh] = &mesh.submeshes;

    // A mesh with no explicit submeshes is one implicit full-range draw.
    let ranges: Vec<(u32, u32)> = if submeshes.is_empty() {
        vec![(0, mesh.indices.len() as u32)]
    } else {
        submeshes
            .iter()
            .map(|s| (s.first_index, s.index_count))
            .collect()
    };

    let mut builder = Builder::new();
    for &(first_index, index_count) in &ranges {
        let first_meshlet = out.meshlets.len() as u32;
        let end = (first_index + index_count).min(mesh.indices.len() as u32);
        let mut i = first_index;
        while i + 3 <= end {
            let tri = [
                mesh.indices[i as usize],
                mesh.indices[i as usize + 1],
                mesh.indices[i as usize + 2],
            ];
            i += 3;
            if tri[0] >= n || tri[1] >= n || tri[2] >= n {
                continue;
            }
            let fresh = builder.fresh_count(tri);
            let would_overflow = builder.locals.len() + fresh > MESHLET_MAX_VERTICES
                || builder.tris.len() + 1 > MESHLET_MAX_TRIANGLES;
            if would_overflow && !builder.is_empty() {
                builder.flush(mesh, &mut out);
            }
            builder.push(tri);
        }
        builder.flush(mesh, &mut out);
        let meshlet_count = out.meshlets.len() as u32 - first_meshlet;
        out.submesh_ranges.push((first_meshlet, meshlet_count));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Submesh, Vertex};
    use glam::{Vec2, Vec3};

    /// A grid mesh of `tris` triangles sharing a growing vertex pool (each triangle adds 3 verts).
    fn strip(tris: usize) -> Mesh {
        let mut mesh = Mesh::default();
        for t in 0..tris {
            let base = mesh.vertices.len() as u32;
            for k in 0..3 {
                mesh.vertices.push(Vertex {
                    position: Vec3::new(t as f32, k as f32, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                    ..Vertex::default()
                });
            }
            mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        mesh.submeshes.push(Submesh {
            first_index: 0,
            index_count: mesh.indices.len() as u32,
            vertex_offset: 0,
            material_slot: 0,
        });
        mesh
    }

    #[test]
    fn meshlet_struct_is_32_bytes() {
        assert_eq!(size_of::<Meshlet>(), 32);
    }

    #[test]
    fn small_mesh_is_one_meshlet() {
        let set = build_meshlets(&strip(4));
        assert_eq!(set.meshlets.len(), 1);
        assert_eq!(set.submesh_ranges, vec![(0, 1)]);
        assert_eq!(set.meshlets[0].triangle_count, 4);
        assert_eq!(set.meshlets[0].vertex_count, 12);
        // 4 triangles × 3 local indices.
        assert_eq!(set.triangles.len(), 12);
    }

    #[test]
    fn overflow_splits_into_multiple_meshlets() {
        // 30 disjoint triangles = 90 unique vertices > 64, so the vertex cap forces a split.
        let set = build_meshlets(&strip(30));
        assert!(set.meshlets.len() >= 2);
        for m in &set.meshlets {
            assert!(m.vertex_count as usize <= MESHLET_MAX_VERTICES);
            assert!(m.triangle_count as usize <= MESHLET_MAX_TRIANGLES);
        }
        let total: u32 = set.meshlets.iter().map(|m| m.triangle_count).sum();
        assert_eq!(total, 30);
    }

    #[test]
    fn local_indices_resolve_to_the_original_triangles() {
        let mesh = strip(30);
        let set = build_meshlets(&mesh);
        // Reconstruct every triangle's global indices from meshlet-local indices and compare to a
        // multiset of the source triangles (meshlet order need not match source order).
        let mut rebuilt: Vec<[u32; 3]> = Vec::new();
        for m in &set.meshlets {
            for t in 0..m.triangle_count as usize {
                let base = m.triangle_offset as usize + t * 3;
                let g = |k: usize| {
                    set.vertices[m.vertex_offset as usize + set.triangles[base + k] as usize]
                };
                rebuilt.push([g(0), g(1), g(2)]);
            }
        }
        let mut source: Vec<[u32; 3]> = mesh
            .indices
            .chunks_exact(3)
            .map(|c| [c[0], c[1], c[2]])
            .collect();
        rebuilt.sort_unstable();
        source.sort_unstable();
        assert_eq!(rebuilt, source);
    }

    #[test]
    fn per_submesh_ranges_partition_the_meshlets() {
        // Two submeshes, each 20 triangles, over one shared 120-vertex pool.
        let mut mesh = strip(40);
        mesh.submeshes.clear();
        mesh.submeshes.push(Submesh {
            first_index: 0,
            index_count: 60,
            vertex_offset: 0,
            material_slot: 0,
        });
        mesh.submeshes.push(Submesh {
            first_index: 60,
            index_count: 60,
            vertex_offset: 0,
            material_slot: 1,
        });
        let set = build_meshlets(&mesh);
        assert_eq!(set.submesh_ranges.len(), 2);
        let (f0, c0) = set.submesh_ranges[0];
        let (f1, c1) = set.submesh_ranges[1];
        assert_eq!(f0, 0);
        assert_eq!(f1, c0);
        assert_eq!((f1 + c1) as usize, set.meshlets.len());
    }
}
