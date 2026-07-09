//! UV-seam **height value agreement** for watertight displacement.
//!
//! A UV seam is one 3D edge shared by two triangles that assign it *different* UVs (the two sides of
//! an unwrapped island border). Displacing both sides watertightly requires them to read an **equal**
//! height there — otherwise the shared edge splits and the surface cracks. Two mechanisms guarantee it,
//! chosen per seam edge (baked into the mesh's [`saffron_geometry::Edge`] flags):
//!
//! - **Dilation** ([`try_reconcile_seam`]): copy one island's edge height values onto the texels the
//!   other island reads across the seam, so both sample a bit-equal value. Reconciles only when the
//!   two islands' edge texels form a consistent correspondence; a conflict (one target texel required
//!   to hold two different source values) cannot be reconciled and falls back.
//! - **Object space** ([`SeamResolution::ObjectSpace`]): sample height at the shared *object-space*
//!   point, independent of UV, so both triangles read one value by construction — mesh-agnostic and
//!   always correct, the safety valve when dilation can't reconcile.

use std::collections::HashMap;

use saffron_geometry::glam::Vec2;

/// How a seam edge's height value agreement was resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeamResolution {
    /// Reconciled on the height image: both islands' cross-seam texels now hold an equal value.
    Dilated,
    /// Not reconcilable by dilation — sample this seam in object space instead.
    ObjectSpace,
}

/// The nearest texel index (row-major) for a UV, clamped into `[0, w) × [0, h)`.
fn texel_index(uv: Vec2, w: u32, h: u32) -> usize {
    let x = ((uv.x.clamp(0.0, 1.0) * w as f32).floor() as u32).min(w.saturating_sub(1));
    let y = ((uv.y.clamp(0.0, 1.0) * h as f32).floor() as u32).min(h.saturating_sub(1));
    (y * w + x) as usize
}

/// Attempts to reconcile a single seam edge by dilation: samples both islands' UV edges at matched
/// parameters and forces the `b`-side texels to the `a`-side height value, so both read a bit-equal
/// value across the seam. Returns [`SeamResolution::Dilated`] and mutates `height` on success, or
/// [`SeamResolution::ObjectSpace`] (leaving `height` untouched) when a `b`-texel is required to hold
/// two `a`-values differing by more than `quantum` — an irreconcilable correspondence.
///
/// `a` / `b` are the `(uv_at_v0, uv_at_v1)` the two incident triangles assign to the shared welded
/// endpoints; `height` is the decoded single-channel height image, row-major, `w × h`.
pub fn try_reconcile_seam(
    height: &mut [f32],
    w: u32,
    h: u32,
    a: (Vec2, Vec2),
    b: (Vec2, Vec2),
    quantum: f32,
) -> SeamResolution {
    if w == 0 || h == 0 || height.len() != (w as usize * h as usize) {
        return SeamResolution::ObjectSpace;
    }
    // Oversample along the edge at roughly twice its longer texel length, so every crossed texel is hit.
    let dim = Vec2::new(w as f32, h as f32);
    let len_a = ((a.1 - a.0) * dim).length();
    let len_b = ((b.1 - b.0) * dim).length();
    let steps = (len_a.max(len_b).ceil() as usize).max(1) * 2;

    // First pass: gather the required (b-texel → a-value) writes and detect a conflict before touching
    // the image, so an irreconcilable seam leaves `height` byte-identical (the caller falls to object space).
    let mut writes: HashMap<usize, f32> = HashMap::new();
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a_val = height[texel_index(a.0.lerp(a.1, t), w, h)];
        let ib = texel_index(b.0.lerp(b.1, t), w, h);
        match writes.get(&ib) {
            Some(&prev) if (prev - a_val).abs() > quantum => return SeamResolution::ObjectSpace,
            _ => {
                writes.insert(ib, a_val);
            }
        }
    }
    for (ib, a_val) in writes {
        height[ib] = a_val;
    }
    SeamResolution::Dilated
}

/// The object-space seam guarantee, made explicit for verification: two triangles sampling height at
/// the same object-space point read one value regardless of their UVs. `sample` is any object-space
/// height field; `point` is the shared 3D edge point. Returned identically for either triangle.
pub fn seam_object_space_value(sample: impl Fn([f32; 3]) -> f32, point: [f32; 3]) -> f32 {
    sample(point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dilation_makes_both_sides_read_an_equal_value() {
        // A 4×1 height strip; island A reads column 0, island B reads column 3 across the seam.
        let mut height = vec![0.10, 0.20, 0.30, 0.99];
        // A's edge sits at u≈0/4 (column 0); B's edge at u≈3/4 (column 3), same v span.
        let a = (Vec2::new(0.0, 0.0), Vec2::new(0.0, 1.0));
        let b = (Vec2::new(0.75, 0.0), Vec2::new(0.75, 1.0));
        let res = try_reconcile_seam(&mut height, 4, 1, a, b, 1e-6);
        assert_eq!(res, SeamResolution::Dilated);
        // B's edge texel (column 3) now holds A's edge value (column 0) — bit-equal across the seam.
        assert_eq!(height[3], 0.10);
        assert_eq!(height[0], 0.10, "the source island is left intact");
    }

    #[test]
    fn irreconcilable_correspondence_falls_back_to_object_space() {
        // A's edge sweeps two distinct values (0.1 → 0.9) while B's edge collapses to one texel — that
        // texel cannot hold both, so dilation cannot reconcile and the image is left untouched.
        let mut height = vec![0.1, 0.9, 0.5, 0.5];
        let before = height.clone();
        let a = (Vec2::new(0.0, 0.0), Vec2::new(0.49, 0.0)); // columns 0 → 1 across the row
        let b = (Vec2::new(0.75, 0.0), Vec2::new(0.75, 0.0)); // a single texel (column 3)
        let res = try_reconcile_seam(&mut height, 4, 1, a, b, 1e-3);
        assert_eq!(res, SeamResolution::ObjectSpace);
        assert_eq!(
            height, before,
            "an unreconcilable seam leaves the image byte-identical"
        );
    }

    #[test]
    fn object_space_value_is_identical_for_both_triangles() {
        // Object-space sampling ignores UV entirely, so both incident triangles read one value.
        let field = |p: [f32; 3]| p[0] * 2.0 + p[1];
        let point = [1.5, 0.25, -3.0];
        assert_eq!(
            seam_object_space_value(field, point),
            seam_object_space_value(field, point)
        );
        assert_eq!(seam_object_space_value(field, point), 3.25);
    }
}
