//! The picking and bounds primitives: ray-triangle, ray-AABB slab, world-AABB
//! accumulation, and smooth-normal recompute.
//!
//! These are leaf math helpers with no ownership or I/O. The numeric epsilons are
//! load-bearing — they decide pick results.
//!
//! Hit tests return the payload through `Option` ("hit or miss, with data").
//! [`world_aabb_from_corners`] keeps *accumulate* semantics (it unions into
//! caller-seeded bounds), because callers union a joint palette across many calls.

use crate::types::{Mesh, Ray};
use glam::{Mat4, Vec3};

/// `|det|` below this rejects a parallel or edge-on triangle as degenerate.
const PARALLEL_EPS: f32 = 1e-8;
/// Hits at or behind the origin (`t <= FORWARD_EPS`) are rejected.
const FORWARD_EPS: f32 = 1e-5;
/// A zero-length accumulated normal (squared length at or below this) falls back
/// to world up.
const NORMAL_LENGTH_EPS: f32 = 1e-12;

/// Two-sided Möller–Trumbore ray-triangle intersection.
///
/// Returns `Some(t)` for a forward hit, where `t` is the ray parameter of the
/// intersection (the ray point is `ray.origin + t * ray.dir`), or `None` for a
/// miss. Two-sided: a back-facing strike still reports a forward hit. The forward
/// gate rejects hits at or behind the origin.
pub fn ray_triangle(ray: &Ray, v0: Vec3, v1: Vec3, v2: Vec3) -> Option<f32> {
    let edge1 = v1 - v0;
    let edge2 = v2 - v0;
    let pvec = ray.dir.cross(edge2);
    let det = edge1.dot(pvec);
    if det > -PARALLEL_EPS && det < PARALLEL_EPS {
        return None;
    }
    let inv_det = 1.0 / det;
    let tvec = ray.origin - v0;
    let u = tvec.dot(pvec) * inv_det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let qvec = tvec.cross(edge1);
    let v = ray.dir.dot(qvec) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = edge2.dot(qvec) * inv_det;
    if t <= FORWARD_EPS {
        return None;
    }
    Some(t)
}

/// The point on triangle `(a, b, c)` closest to `p` (Ericson, *Real-Time Collision
/// Detection* §5.1.5): test `p` against the three vertex Voronoi regions, the three
/// edge regions, then the interior, returning the first that contains the projection.
///
/// Used by the SDF bake's nearest-distance query; the unsigned distance is
/// `(closest - p).length()`.
pub fn closest_point_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a; // vertex region A
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b; // vertex region B
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return a + ab * v; // edge AB
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c; // vertex region C
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return a + ac * w; // edge AC
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return b + (c - b) * w; // edge BC
    }
    // Interior: barycentric combination of the three edge weights.
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    a + ab * v + ac * w
}

/// The squared distance from `p` to the axis-aligned box `[lo, hi]`, `0` when `p` is
/// inside. The BVH nearest-distance descent uses it as a node lower bound.
fn aabb_distance_sq(p: Vec3, lo: Vec3, hi: Vec3) -> f32 {
    let d = (lo - p).max(Vec3::ZERO).max(p - hi);
    d.dot(d)
}

/// Ray-AABB slab intersection.
///
/// Returns `Some((t_enter, t_exit))` when the ray hits the box, or `None` for a
/// miss. `t_enter` may be negative when the origin is inside the box. An
/// axis-aligned ray component yields an infinite slab parameter, which the
/// `min`/`max` fold handles correctly.
pub fn ray_aabb_slab(ray: &Ray, box_min: Vec3, box_max: Vec3) -> Option<(f32, f32)> {
    let inv_dir = Vec3::ONE / ray.dir;
    let t0 = (box_min - ray.origin) * inv_dir;
    let t1 = (box_max - ray.origin) * inv_dir;
    let tlo = t0.min(t1);
    let thi = t0.max(t1);
    let t_enter = tlo.x.max(tlo.y).max(tlo.z);
    let t_exit = thi.x.min(thi.y).min(thi.z);
    if t_exit >= 0.0 && t_enter <= t_exit {
        Some((t_enter, t_exit))
    } else {
        None
    }
}

/// Transform the eight corners of the local box `[lo, hi]` by `model` and
/// *accumulate* their min/max into `out_min`/`out_max`.
///
/// This unions into the passed bounds rather than resetting them: callers seed
/// `out_min`/`out_max` to `+inf`/`-inf` and call this once per element to grow a
/// shared world AABB across many local boxes.
pub fn world_aabb_from_corners(
    model: &Mat4,
    lo: Vec3,
    hi: Vec3,
    out_min: &mut Vec3,
    out_max: &mut Vec3,
) {
    for corner in 0u32..8 {
        let mut p = lo;
        if corner & 1 != 0 {
            p.x = hi.x;
        }
        if corner & 2 != 0 {
            p.y = hi.y;
        }
        if corner & 4 != 0 {
            p.z = hi.z;
        }
        let world = model.transform_point3(p);
        *out_min = out_min.min(world);
        *out_max = out_max.max(world);
    }
}

/// Recompute smooth vertex normals from the mesh triangles.
///
/// Zeroes every normal, accumulates each triangle's face normal
/// (`cross(b - a, c - a)`) into its three vertices — indexing through each
/// submesh's `first_index`/`vertex_offset` so the merged buffers are addressed
/// correctly — then normalizes. A vertex whose accumulated normal is
/// (near-)zero-length falls back to world up `(0, 1, 0)`. The importer uses this
/// when a source omits normals.
pub fn generate_normals(mesh: &mut Mesh) {
    for vertex in &mut mesh.vertices {
        vertex.normal = Vec3::ZERO;
    }
    for submesh in &mesh.submeshes {
        let mut i = 0u32;
        while i + 2 < submesh.index_count {
            let base = (submesh.first_index + i) as usize;
            let offset = submesh.vertex_offset as i64;
            let a = (offset + i64::from(mesh.indices[base])) as usize;
            let b = (offset + i64::from(mesh.indices[base + 1])) as usize;
            let c = (offset + i64::from(mesh.indices[base + 2])) as usize;
            let pos_a = mesh.vertices[a].position;
            let pos_b = mesh.vertices[b].position;
            let pos_c = mesh.vertices[c].position;
            let face_normal = (pos_b - pos_a).cross(pos_c - pos_a);
            mesh.vertices[a].normal += face_normal;
            mesh.vertices[b].normal += face_normal;
            mesh.vertices[c].normal += face_normal;
            i += 3;
        }
    }
    for vertex in &mut mesh.vertices {
        if vertex.normal.dot(vertex.normal) > NORMAL_LENGTH_EPS {
            vertex.normal = vertex.normal.normalize();
        } else {
            vertex.normal = Vec3::new(0.0, 1.0, 0.0);
        }
    }
}

/// A leaf holds at most this many triangles; above it the node splits.
const BVH_LEAF_MAX: usize = 4;

/// One BVH node: a bounding box plus either a triangle range (leaf) or two child indices.
#[derive(Clone, Copy)]
struct BvhNode {
    min: Vec3,
    max: Vec3,
    /// Leaf: index of the first triangle. Internal: index of the left child node.
    first: u32,
    /// Triangle count (`> 0` ⇒ leaf). `0` ⇒ internal node.
    count: u32,
    /// Internal only: index of the right child node.
    right: u32,
}

/// A binary bounding-volume hierarchy over a mesh's triangles, in the mesh's own coordinate
/// space, for sublinear ray picking.
///
/// Built once per mesh (then cached) so the placement/selection pick descends only into boxes
/// the ray crosses — reaching the few candidate triangles in ~log(N) steps instead of scanning
/// all N. Traversal happens in mesh-local space: transform the world ray by the entity's inverse
/// world matrix, [`raycast`](MeshBvh::raycast), then map the hit point back to world.
pub struct MeshBvh {
    nodes: Vec<BvhNode>,
    /// Triangle vertices, reordered by the build so each leaf names a contiguous range.
    tris: Vec<[Vec3; 3]>,
    /// Orientation of the triangle winding: `+1` when `cross(b-a, c-a)` points outward
    /// (the signed mesh volume is positive), `-1` when the mesh is wound inward. Multiplied
    /// into the [`nearest_signed_distance`](Self::nearest_signed_distance) sign so the field
    /// is negative inside regardless of which way the source happens to wind its faces.
    winding: f32,
}

impl MeshBvh {
    /// Builds a BVH from a mesh's positions + triangle indices. `None` when there are no
    /// triangles (or the indices are degenerate), matching "nothing to pick".
    #[must_use]
    pub fn build(positions: &[Vec3], indices: &[u32]) -> Option<MeshBvh> {
        let mut tris: Vec<[Vec3; 3]> = Vec::with_capacity(indices.len() / 3);
        for tri in indices.chunks_exact(3) {
            let a = *positions.get(tri[0] as usize)?;
            let b = *positions.get(tri[1] as usize)?;
            let c = *positions.get(tri[2] as usize)?;
            // Drop degenerate (zero-area / sliver) triangles: they carry no surface, are
            // never a ray hit, and their unstable normal would poison the signed-distance
            // pseudonormal. `sin²θ < 1e-10` between the two edges ⇒ collinear (scale-free).
            let (e1, e2) = (b - a, c - a);
            let cross = e1.cross(e2);
            if cross.length_squared() > 1e-10 * e1.length_squared() * e2.length_squared() {
                tris.push([a, b, c]);
            }
        }
        if tris.is_empty() {
            return None;
        }
        // Signed volume (divergence theorem, ×6): positive when the faces are wound so
        // `cross(b-a, c-a)` points outward. Its sign orients the pseudonormal so an
        // inward-wound source still signs as negative inside.
        let vol6: f32 = tris.iter().map(|t| t[0].dot(t[1].cross(t[2]))).sum();
        let winding = if vol6 >= 0.0 { 1.0 } else { -1.0 };
        let centroids: Vec<Vec3> = tris.iter().map(|t| (t[0] + t[1] + t[2]) / 3.0).collect();
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes: Vec<BvhNode> = Vec::new();
        build_node(&mut nodes, &tris, &centroids, &mut order, 0, tris.len());
        let tris = order.iter().map(|&i| tris[i as usize]).collect();
        Some(MeshBvh {
            nodes,
            tris,
            winding,
        })
    }

    /// The signed distance from `point` to the nearest triangle, found by a
    /// distance-bounded BVH descent (a box farther than the running best is pruned, its
    /// children visited near-first). `point` is in the BVH's build space (mesh-local).
    ///
    /// The magnitude is the unsigned nearest-surface distance; the sign comes from the
    /// **angle-weighted pseudonormal** at the closest point — the summed unit normals of
    /// every triangle that shares that closest feature (Bærentzen & Aanæs). `point` is
    /// behind the surface (negative) when `dot(point - closestPoint, pseudonormal) < 0`.
    /// Summing the incident faces (not one triangle's normal) is what makes the sign
    /// correct at an edge or corner, where a single face normal can point the wrong way.
    /// For a watertight mesh this is the usual inside/outside SDF sign; for a
    /// non-watertight mesh it reads as "which side of the nearest surface", the sensible
    /// value for an occlusion field (and far cheaper + more robust than ray-stab parity).
    /// Returns `+∞` for an empty hierarchy.
    #[must_use]
    pub fn nearest_signed_distance(&self, point: Vec3) -> f32 {
        if self.nodes.is_empty() {
            return f32::INFINITY;
        }
        let mut best = f32::INFINITY;
        let mut best_point = point;
        // Summed unit normals of the triangles incident to the closest feature.
        let mut normal_sum = Vec3::Y;
        // (node index, lower-bound squared distance from `point` to the node's box).
        let mut stack: Vec<(u32, f32)> = vec![(0, 0.0)];
        while let Some((idx, lower)) = stack.pop() {
            // Keep boxes that could still tie the closest distance (`lower <= best`).
            if lower > best {
                continue;
            }
            let node = self.nodes[idx as usize];
            if node.count > 0 {
                for tri in &self.tris[node.first as usize..(node.first + node.count) as usize] {
                    let cp = closest_point_on_triangle(point, tri[0], tri[1], tri[2]);
                    let d = cp.distance_squared(point);
                    let n = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
                    // A relative epsilon on the squared distance gathers triangles that
                    // meet at the same vertex/edge (their closest point is that shared
                    // feature), summing their normals into the pseudonormal.
                    let tie = 1e-5 * best.max(1.0);
                    if d < best - tie {
                        best = d;
                        best_point = cp;
                        normal_sum = n;
                    } else if d <= best + tie
                        && cp.distance_squared(best_point) <= 1e-6 * best.max(1.0)
                    {
                        normal_sum += n;
                        if d < best {
                            best = d;
                            best_point = cp;
                        }
                    }
                }
            } else {
                // Push both children with their box lower bounds; visit the nearer last so
                // it pops first and tightens `best` before the farther child is reached.
                let l = node.first;
                let r = node.right;
                let dl = aabb_distance_sq(
                    point,
                    self.nodes[l as usize].min,
                    self.nodes[l as usize].max,
                );
                let dr = aabb_distance_sq(
                    point,
                    self.nodes[r as usize].min,
                    self.nodes[r as usize].max,
                );
                if dl <= dr {
                    stack.push((l, dl));
                    stack.push((r, dr));
                } else {
                    stack.push((r, dr));
                    stack.push((l, dl));
                }
            }
        }
        let dist = best.sqrt();
        if (point - best_point).dot(normal_sum) * self.winding < 0.0 {
            -dist
        } else {
            dist
        }
    }

    /// The nearest forward triangle hit along `ray`, as the ray parameter `t` (the hit point is
    /// `ray.origin + t * ray.dir`), or `None` for a miss. `ray` is in the same space the BVH was
    /// built in (mesh-local).
    #[must_use]
    pub fn raycast(&self, ray: &Ray) -> Option<f32> {
        if self.nodes.is_empty() {
            return None;
        }
        let mut best = f32::INFINITY;
        let mut stack: Vec<u32> = vec![0];
        while let Some(idx) = stack.pop() {
            let node = self.nodes[idx as usize];
            // Skip a box the ray misses, or whose entry is already farther than the best hit.
            match ray_aabb_slab(ray, node.min, node.max) {
                Some((t_enter, _)) if t_enter <= best => {}
                _ => continue,
            }
            if node.count > 0 {
                for tri in &self.tris[node.first as usize..(node.first + node.count) as usize] {
                    if let Some(t) = ray_triangle(ray, tri[0], tri[1], tri[2])
                        && t < best
                    {
                        best = t;
                    }
                }
            } else {
                stack.push(node.first);
                stack.push(node.right);
            }
        }
        best.is_finite().then_some(best)
    }
}

/// Recursively builds BVH nodes over `order[start..end]` (a permutation of triangle indices),
/// returning the index of the node it pushes. Median-split on the axis of greatest centroid
/// spread; a degenerate split falls back to the midpoint so the recursion always shrinks.
fn build_node(
    nodes: &mut Vec<BvhNode>,
    tris: &[[Vec3; 3]],
    centroids: &[Vec3],
    order: &mut [u32],
    start: usize,
    end: usize,
) -> u32 {
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for &t in &order[start..end] {
        for v in tris[t as usize] {
            min = min.min(v);
            max = max.max(v);
        }
    }
    let idx = nodes.len() as u32;
    nodes.push(BvhNode {
        min,
        max,
        first: start as u32,
        count: (end - start) as u32,
        right: 0,
    });
    if end - start <= BVH_LEAF_MAX {
        return idx;
    }

    let mut cmin = Vec3::splat(f32::MAX);
    let mut cmax = Vec3::splat(f32::MIN);
    for &t in &order[start..end] {
        cmin = cmin.min(centroids[t as usize]);
        cmax = cmax.max(centroids[t as usize]);
    }
    let ext = cmax - cmin;
    let axis = if ext.x >= ext.y && ext.x >= ext.z {
        0
    } else if ext.y >= ext.z {
        1
    } else {
        2
    };
    let pivot = (cmin[axis] + cmax[axis]) * 0.5;

    // Partition `order[start..end]` so centroids below the pivot come first.
    let mut mid = start;
    for i in start..end {
        if centroids[order[i] as usize][axis] < pivot {
            order.swap(i, mid);
            mid += 1;
        }
    }
    if mid == start || mid == end {
        mid = start + (end - start) / 2; // degenerate spread → split by count
    }

    let left = build_node(nodes, tris, centroids, order, start, mid);
    let right = build_node(nodes, tris, centroids, order, mid, end);
    nodes[idx as usize].count = 0;
    nodes[idx as usize].first = left;
    nodes[idx as usize].right = right;
    idx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Submesh, Vertex};
    use glam::{Vec2, Vec3};
    use saffron_test_support::close;

    /// The triangle corners and ray directions the pick-math tests share.
    const A: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    const B: Vec3 = Vec3::new(1.0, 0.0, 0.0);
    const C: Vec3 = Vec3::new(0.0, 1.0, 0.0);
    const DOWN: Vec3 = Vec3::new(0.0, 0.0, -1.0);
    const UP: Vec3 = Vec3::new(0.0, 0.0, 1.0);

    #[test]
    fn ray_through_triangle_center_hits_at_one() {
        let ray = Ray {
            origin: Vec3::new(0.25, 0.25, 1.0),
            dir: DOWN,
        };
        let t = ray_triangle(&ray, A, B, C).expect("center ray must hit");
        assert!(close(t, 1.0, 1e-4), "t was {t}");
    }

    #[test]
    fn ray_through_corner_gap_misses() {
        // (0.9, 0.9) is inside the AABB but past the hypotenuse (u + v > 1): a miss.
        let ray = Ray {
            origin: Vec3::new(0.9, 0.9, 1.0),
            dir: DOWN,
        };
        assert!(ray_triangle(&ray, A, B, C).is_none());
    }

    #[test]
    fn triangle_behind_origin_misses() {
        // The whole triangle is behind the origin along the ray; the forward gate rejects it.
        let ray = Ray {
            origin: Vec3::new(0.25, 0.25, -1.0),
            dir: DOWN,
        };
        assert!(ray_triangle(&ray, A, B, C).is_none());
    }

    #[test]
    fn backface_strike_reports_forward_hit() {
        // Striking the back face from below is two-sided: still a forward hit at t ≈ 1.
        let ray = Ray {
            origin: Vec3::new(0.25, 0.25, -1.0),
            dir: UP,
        };
        let t = ray_triangle(&ray, A, B, C).expect("backface ray must hit (two-sided)");
        assert!(close(t, 1.0, 1e-4), "t was {t}");
    }

    /// Builds the 45°-about-Z rotation the AABB test uses.
    fn rot_z_45() -> Mat4 {
        let angle = std::f32::consts::FRAC_PI_4;
        let cs = angle.cos();
        let sn = angle.sin();
        // glam Mat4::from_cols takes columns in column-major order.
        Mat4::from_cols(
            glam::Vec4::new(cs, sn, 0.0, 0.0),
            glam::Vec4::new(-sn, cs, 0.0, 0.0),
            glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
            glam::Vec4::new(0.0, 0.0, 0.0, 1.0),
        )
    }

    #[test]
    fn rotated_unit_box_world_aabb_grows_in_x_keeps_z() {
        let rot = rot_z_45();
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        world_aabb_from_corners(&rot, Vec3::splat(-0.5), Vec3::splat(0.5), &mut lo, &mut hi);
        // A 45° spin grows the unit box's half-extent in x/y to √0.5; z is unchanged.
        assert!(close(hi.x, 0.5f32.sqrt(), 1e-3), "hi.x was {}", hi.x);
        assert!(close(hi.z, 0.5, 1e-3), "hi.z was {}", hi.z);
        // The accumulate is symmetric about the origin.
        assert!(close(lo.x, -0.5f32.sqrt(), 1e-3), "lo.x was {}", lo.x);
        assert!(close(lo.z, -0.5, 1e-3), "lo.z was {}", lo.z);
    }

    #[test]
    fn world_aabb_accumulates_rather_than_resets() {
        // Two boxes union into the same bounds: the second call grows, never clobbers.
        let identity = Mat4::IDENTITY;
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        world_aabb_from_corners(&identity, Vec3::ZERO, Vec3::ONE, &mut lo, &mut hi);
        world_aabb_from_corners(
            &identity,
            Vec3::new(-2.0, -2.0, -2.0),
            Vec3::new(-1.0, -1.0, -1.0),
            &mut lo,
            &mut hi,
        );
        assert_eq!(lo, Vec3::new(-2.0, -2.0, -2.0));
        assert_eq!(hi, Vec3::ONE);
    }

    #[test]
    fn slab_hit_on_overhead_ray() {
        let rot = rot_z_45();
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        world_aabb_from_corners(&rot, Vec3::splat(-0.5), Vec3::splat(0.5), &mut lo, &mut hi);
        let ray = Ray {
            origin: Vec3::new(0.0, 0.0, 2.0),
            dir: DOWN,
        };
        assert!(ray_aabb_slab(&ray, lo, hi).is_some());
    }

    #[test]
    fn slab_miss_when_ray_is_outside() {
        let rot = rot_z_45();
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        world_aabb_from_corners(&rot, Vec3::splat(-0.5), Vec3::splat(0.5), &mut lo, &mut hi);
        let ray = Ray {
            origin: Vec3::new(5.0, 5.0, 2.0),
            dir: DOWN,
        };
        assert!(ray_aabb_slab(&ray, lo, hi).is_none());
    }

    #[test]
    fn slab_t_enter_negative_when_origin_inside() {
        // Origin inside a unit box: the near plane is behind the origin.
        let ray = Ray {
            origin: Vec3::ZERO,
            dir: Vec3::new(0.0, 0.0, -1.0),
        };
        let (t_enter, t_exit) =
            ray_aabb_slab(&ray, Vec3::splat(-1.0), Vec3::splat(1.0)).expect("inside box hits");
        assert!(t_enter < 0.0, "t_enter was {t_enter}");
        assert!(t_exit > 0.0, "t_exit was {t_exit}");
    }

    fn vertex_at(p: Vec3) -> Vertex {
        Vertex {
            position: p,
            normal: Vec3::ZERO,
            uv0: Vec2::ZERO,
            ..Vertex::default()
        }
    }

    #[test]
    fn generate_normals_on_xy_quad_points_up() {
        // A flat quad in the z=0 plane, wound CCW, must yield unit +Z normals.
        let mut mesh = Mesh {
            vertices: vec![
                vertex_at(Vec3::new(0.0, 0.0, 0.0)),
                vertex_at(Vec3::new(1.0, 0.0, 0.0)),
                vertex_at(Vec3::new(1.0, 1.0, 0.0)),
                vertex_at(Vec3::new(0.0, 1.0, 0.0)),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        generate_normals(&mut mesh);
        for (i, vertex) in mesh.vertices.iter().enumerate() {
            assert!(
                close(vertex.normal.length(), 1.0, 1e-5),
                "vertex {i} normal not unit length: {}",
                vertex.normal.length()
            );
            assert!(
                vertex.normal.abs_diff_eq(Vec3::new(0.0, 0.0, 1.0), 1e-5),
                "vertex {i} normal not +Z: {:?}",
                vertex.normal
            );
        }
    }

    #[test]
    fn generate_normals_respects_vertex_offset() {
        // Two vertices precede the quad; the submesh's vertex_offset must skip them.
        let mut mesh = Mesh {
            vertices: vec![
                vertex_at(Vec3::new(9.0, 9.0, 9.0)),
                vertex_at(Vec3::new(8.0, 8.0, 8.0)),
                vertex_at(Vec3::new(0.0, 0.0, 0.0)),
                vertex_at(Vec3::new(1.0, 0.0, 0.0)),
                vertex_at(Vec3::new(1.0, 1.0, 0.0)),
                vertex_at(Vec3::new(0.0, 1.0, 0.0)),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 2,
                material_slot: 0,
            }],
        };
        generate_normals(&mut mesh);
        // The two skipped leading vertices get no face contribution: the up fallback.
        assert!(
            mesh.vertices[0]
                .normal
                .abs_diff_eq(Vec3::new(0.0, 1.0, 0.0), 1e-5)
        );
        assert!(
            mesh.vertices[1]
                .normal
                .abs_diff_eq(Vec3::new(0.0, 1.0, 0.0), 1e-5)
        );
        // The quad vertices (offset 2..) point +Z.
        for vertex in &mesh.vertices[2..] {
            assert!(vertex.normal.abs_diff_eq(Vec3::new(0.0, 0.0, 1.0), 1e-5));
        }
    }

    #[test]
    fn degenerate_triangle_normal_falls_back_to_up() {
        // A zero-area triangle (three coincident points) accumulates a zero normal,
        // which the 1e-12 guard replaces with world up.
        let mut mesh = Mesh {
            vertices: vec![
                vertex_at(Vec3::ZERO),
                vertex_at(Vec3::ZERO),
                vertex_at(Vec3::ZERO),
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        generate_normals(&mut mesh);
        for vertex in &mesh.vertices {
            assert_eq!(vertex.normal, Vec3::new(0.0, 1.0, 0.0));
        }
    }

    /// Brute-force nearest forward hit: the reference the BVH must match.
    fn brute_nearest(ray: &Ray, positions: &[Vec3], indices: &[u32]) -> Option<f32> {
        let mut best = f32::INFINITY;
        for tri in indices.chunks_exact(3) {
            let (a, b, c) = (
                positions[tri[0] as usize],
                positions[tri[1] as usize],
                positions[tri[2] as usize],
            );
            if let Some(t) = ray_triangle(ray, a, b, c) {
                best = best.min(t);
            }
        }
        best.is_finite().then_some(best)
    }

    /// A grid of stacked quads at varying depths, so a downward ray crosses several triangles and
    /// the nearest is non-trivial — exercises real BVH descent + the depth-ordered `best` prune.
    fn grid_mesh() -> (Vec<Vec3>, Vec<u32>) {
        let mut positions = Vec::new();
        let mut indices = Vec::new();
        let n = 8;
        for gx in 0..n {
            for gy in 0..n {
                let z = ((gx * 13 + gy * 7) % 5) as f32; // 0..4, deterministic spread
                let base = positions.len() as u32;
                let (x, y) = (gx as f32, gy as f32);
                positions.push(Vec3::new(x, y, z));
                positions.push(Vec3::new(x + 1.0, y, z));
                positions.push(Vec3::new(x, y + 1.0, z));
                positions.push(Vec3::new(x + 1.0, y + 1.0, z));
                indices.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
            }
        }
        (positions, indices)
    }

    /// The BVH returns the same nearest hit as the brute-force scan, across many rays — the parity
    /// that lets the pick replace the O(triangles) narrowphase with the BVH.
    #[test]
    fn bvh_raycast_matches_brute_force() {
        let (positions, indices) = grid_mesh();
        let bvh = MeshBvh::build(&positions, &indices).expect("non-empty mesh builds");
        for gx in 0..8 {
            for gy in 0..8 {
                let ray = Ray {
                    origin: Vec3::new(gx as f32 + 0.25, gy as f32 + 0.25, 100.0),
                    dir: DOWN,
                };
                let brute = brute_nearest(&ray, &positions, &indices);
                let fast = bvh.raycast(&ray);
                match (brute, fast) {
                    (Some(a), Some(b)) => assert!(close(a, b, 1e-4), "ray ({gx},{gy}): {a} vs {b}"),
                    (None, None) => {}
                    (a, b) => panic!("ray ({gx},{gy}) disagree: brute={a:?} bvh={b:?}"),
                }
            }
        }
        // A ray that misses every triangle agrees on the miss.
        let miss = Ray {
            origin: Vec3::new(-5.0, -5.0, 100.0),
            dir: DOWN,
        };
        assert_eq!(
            bvh.raycast(&miss),
            brute_nearest(&miss, &positions, &indices)
        );
    }

    /// An empty (or index-degenerate) mesh has no BVH — "nothing to pick".
    #[test]
    fn bvh_build_rejects_empty() {
        assert!(MeshBvh::build(&[], &[]).is_none());
        assert!(MeshBvh::build(&[Vec3::ZERO, Vec3::X], &[]).is_none());
    }
}
