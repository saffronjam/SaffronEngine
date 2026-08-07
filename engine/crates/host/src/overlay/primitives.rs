//! Screen- and world-space overlay primitives: lines, quads, icons, and the clipped world-space
//! line/ring/arc/box helpers every builder emits through.

use glam::{Mat4, Vec2, Vec3, Vec4};

use saffron_rendering::OverlayVertex;
use saffron_sceneedit::pixel_to_ndc;

/// Pushes a flat-colored triangle. Edge + depth are zero (no feather, on top).
pub(super) fn add_triangle(
    vertices: &mut Vec<OverlayVertex>,
    a: Vec2,
    b: Vec2,
    c: Vec2,
    color: Vec4,
) {
    vertices.push(OverlayVertex::new(a, color, Vec4::ZERO, 0.0));
    vertices.push(OverlayVertex::new(b, color, Vec4::ZERO, 0.0));
    vertices.push(OverlayVertex::new(c, color, Vec4::ZERO, 0.0));
}

/// Pushes a thick line as two triangles (six vertices) between two pixel-space endpoints.
///
/// The quad is widened 1px per side for the shader's analytic feather: `edge.x` carries the
/// signed cross-edge coordinate (±`ext/half` at the expanded rim where coverage reaches zero)
/// and `edge.z` the half-thickness, so the falloff stays ~1px at any line width. Per-endpoint
/// NDC depths drive the depth-tested range; the on-top range passes zero.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_line(
    vertices: &mut Vec<OverlayVertex>,
    a_px: Vec2,
    b_px: Vec2,
    thickness: f32,
    color: Vec4,
    width: u32,
    height: u32,
    a_depth: f32,
    b_depth: f32,
) {
    let delta = b_px - a_px;
    let len = delta.length();
    if len < 0.001 {
        return;
    }
    let half = thickness * 0.5;
    let ext = half + 1.0;
    let n = Vec2::new(-delta.y, delta.x) / len * ext;
    let edge_pos = Vec4::new(ext / half, 0.0, half, 0.0);
    let edge_neg = Vec4::new(-ext / half, 0.0, half, 0.0);
    let a0 = pixel_to_ndc(a_px + n, width, height);
    let a1 = pixel_to_ndc(a_px - n, width, height);
    let b0 = pixel_to_ndc(b_px + n, width, height);
    let b1 = pixel_to_ndc(b_px - n, width, height);
    vertices.push(OverlayVertex::new(a0, color, edge_pos, a_depth));
    vertices.push(OverlayVertex::new(b0, color, edge_pos, b_depth));
    vertices.push(OverlayVertex::new(b1, color, edge_neg, b_depth));
    vertices.push(OverlayVertex::new(a0, color, edge_pos, a_depth));
    vertices.push(OverlayVertex::new(b1, color, edge_neg, b_depth));
    vertices.push(OverlayVertex::new(a1, color, edge_neg, a_depth));
}

/// A flat 2D line at zero NDC depth (the common on-top case).
pub(super) fn add_line_flat(
    vertices: &mut Vec<OverlayVertex>,
    a_px: Vec2,
    b_px: Vec2,
    thickness: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    add_line(
        vertices, a_px, b_px, thickness, color, width, height, 0.0, 0.0,
    );
}

/// Pushes a filled quad from four pixel-space corners (a convex loop), feathered
/// analytically in both directions.
///
/// Each corner is pushed 1px outward along the quad's edge directions and carries signed
/// coords + half-extents for the shader's coverage alpha. Corner order mirrors
/// `gizmo_plane_corners`: (min,min), (min,max), (max,max), (max,min).
pub(super) fn add_quad(
    vertices: &mut Vec<OverlayVertex>,
    corners_px: [Vec2; 4],
    color: Vec4,
    width: u32,
    height: u32,
) {
    let u = (corners_px[3] - corners_px[0] + corners_px[2] - corners_px[1]) * 0.5;
    let v = (corners_px[1] - corners_px[0] + corners_px[2] - corners_px[3]) * 0.5;
    let hu = u.length() * 0.5;
    let hv = v.length() * 0.5;
    if hu < 0.5 || hv < 0.5 {
        return;
    }
    let du = u / (hu * 2.0);
    let dv = v / (hv * 2.0);
    let eu = (hu + 1.0) / hu;
    let ev = (hv + 1.0) / hv;
    let quad = [
        OverlayVertex::new(
            pixel_to_ndc(corners_px[0] - du - dv, width, height),
            color,
            Vec4::new(-eu, -ev, hu, hv),
            0.0,
        ),
        OverlayVertex::new(
            pixel_to_ndc(corners_px[1] - du + dv, width, height),
            color,
            Vec4::new(-eu, ev, hu, hv),
            0.0,
        ),
        OverlayVertex::new(
            pixel_to_ndc(corners_px[2] + du + dv, width, height),
            color,
            Vec4::new(eu, ev, hu, hv),
            0.0,
        ),
        OverlayVertex::new(
            pixel_to_ndc(corners_px[3] + du - dv, width, height),
            color,
            Vec4::new(eu, -ev, hu, hv),
            0.0,
        ),
    ];
    vertices.push(quad[0]);
    vertices.push(quad[1]);
    vertices.push(quad[2]);
    vertices.push(quad[0]);
    vertices.push(quad[2]);
    vertices.push(quad[3]);
}

/// Pushes an axis-aligned filled box of `size` pixels centered at `center_px`: two triangles,
/// no feather.
pub(super) fn add_box(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    size: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    let h = size * 0.5;
    let a = pixel_to_ndc(center_px + Vec2::new(-h, -h), width, height);
    let b = pixel_to_ndc(center_px + Vec2::new(h, -h), width, height);
    let c = pixel_to_ndc(center_px + Vec2::new(h, h), width, height);
    let d = pixel_to_ndc(center_px + Vec2::new(-h, h), width, height);
    add_triangle(vertices, a, b, c, color);
    add_triangle(vertices, a, c, d, color);
}

/// Pushes a four-line rectangle outline centered at `center_px`.
pub(super) fn add_rect_outline(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    size_px: Vec2,
    color: Vec4,
    width: u32,
    height: u32,
) {
    let h = size_px * 0.5;
    let tl = center_px + Vec2::new(-h.x, -h.y);
    let tr = center_px + Vec2::new(h.x, -h.y);
    let br = center_px + Vec2::new(h.x, h.y);
    let bl = center_px + Vec2::new(-h.x, h.y);
    add_line_flat(vertices, tl, tr, 2.0, color, width, height);
    add_line_flat(vertices, tr, br, 2.0, color, width, height);
    add_line_flat(vertices, br, bl, 2.0, color, width, height);
    add_line_flat(vertices, bl, tl, 2.0, color, width, height);
}

/// Pushes a filled circle (24-segment fan) of `radius` pixels.
pub(super) fn add_circle_fill(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    radius: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    const SEGMENTS: u32 = 24;
    let center = pixel_to_ndc(center_px, width, height);
    for i in 0..SEGMENTS {
        let a0 = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let p0 = pixel_to_ndc(
            center_px + Vec2::new(a0.cos(), a0.sin()) * radius,
            width,
            height,
        );
        let p1 = pixel_to_ndc(
            center_px + Vec2::new(a1.cos(), a1.sin()) * radius,
            width,
            height,
        );
        add_triangle(vertices, center, p0, p1, color);
    }
}

/// Pushes a 32-segment circle outline of `radius` pixels.
pub(super) fn add_circle_outline(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    radius: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    const SEGMENTS: u32 = 32;
    let mut prev = center_px + Vec2::new(radius, 0.0);
    for i in 1..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let cur = center_px + Vec2::new(a.cos(), a.sin()) * radius;
        add_line_flat(vertices, prev, cur, 2.0, color, width, height);
        prev = cur;
    }
}

/// Pushes a light-bulb glyph (a filled dome over two base lines) at `center_px`.
pub(super) fn add_bulb_icon(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    color: Vec4,
    width: u32,
    height: u32,
) {
    add_circle_fill(
        vertices,
        center_px + Vec2::new(0.0, -3.0),
        7.5,
        color,
        width,
        height,
    );
    add_line_flat(
        vertices,
        center_px + Vec2::new(-4.5, 5.0),
        center_px + Vec2::new(4.5, 5.0),
        3.0,
        color,
        width,
        height,
    );
    add_line_flat(
        vertices,
        center_px + Vec2::new(-3.5, 9.0),
        center_px + Vec2::new(3.5, 9.0),
        3.0,
        color,
        width,
        height,
    );
}

/// Pushes a camera glyph (body rect + lens circle + a trapezoidal viewfinder) at `center_px`.
pub(super) fn add_camera_icon(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    color: Vec4,
    width: u32,
    height: u32,
) {
    add_rect_outline(
        vertices,
        center_px + Vec2::new(-2.0, 1.0),
        Vec2::new(20.0, 14.0),
        color,
        width,
        height,
    );
    add_circle_outline(
        vertices,
        center_px + Vec2::new(-2.0, 1.0),
        4.0,
        color,
        width,
        height,
    );
    let a = center_px + Vec2::new(8.0, -4.0);
    let b = center_px + Vec2::new(14.0, -8.0);
    let c = center_px + Vec2::new(14.0, 6.0);
    let d = center_px + Vec2::new(8.0, 2.0);
    add_line_flat(vertices, a, b, 2.0, color, width, height);
    add_line_flat(vertices, b, c, 2.0, color, width, height);
    add_line_flat(vertices, c, d, 2.0, color, width, height);
}

/// Pushes a fog cloud glyph (three overlapping puff circles + a flat base) at `center_px`.
pub(super) fn add_fog_icon(
    vertices: &mut Vec<OverlayVertex>,
    center_px: Vec2,
    color: Vec4,
    width: u32,
    height: u32,
) {
    add_circle_outline(
        vertices,
        center_px + Vec2::new(-5.0, 1.0),
        5.0,
        color,
        width,
        height,
    );
    add_circle_outline(
        vertices,
        center_px + Vec2::new(5.0, 1.0),
        5.0,
        color,
        width,
        height,
    );
    add_circle_outline(
        vertices,
        center_px + Vec2::new(0.0, -3.0),
        6.0,
        color,
        width,
        height,
    );
    add_line_flat(
        vertices,
        center_px + Vec2::new(-8.0, 5.5),
        center_px + Vec2::new(8.0, 5.5),
        2.0,
        color,
        width,
        height,
    );
}

/// Clips a clip-space line segment to the six clip planes, mutating the endpoints in place.
/// Returns `false` when the segment is fully outside.
///
/// The near plane is `z >= 0`: `camera_projection` uses the Vulkan `[0, 1]` clip-depth convention,
/// so the line clips against the same frustum the scene's depth buffer was rasterized with.
pub(super) fn clip_overlay_line(a: &mut Vec4, b: &mut Vec4) -> bool {
    let clip_plane = |a: &mut Vec4, b: &mut Vec4, distance: fn(Vec4) -> f32| -> bool {
        let da = distance(*a);
        let db = distance(*b);
        if da >= 0.0 && db >= 0.0 {
            return true;
        }
        if da < 0.0 && db < 0.0 {
            return false;
        }
        let t = da / (da - db);
        let p = *a + (*b - *a) * t;
        if da < 0.0 {
            *a = p;
        } else {
            *b = p;
        }
        true
    };
    clip_plane(a, b, |p| p.x + p.w)
        && clip_plane(a, b, |p| p.w - p.x)
        && clip_plane(a, b, |p| p.y + p.w)
        && clip_plane(a, b, |p| p.w - p.y)
        && clip_plane(a, b, |p| p.z)
        && clip_plane(a, b, |p| p.w - p.z)
}

/// A clip-space point to viewport pixels (top-left origin).
pub(super) fn clip_to_pixel(clip: Vec4, width: u32, height: u32) -> Vec2 {
    let ndc = clip.truncate() / clip.w;
    Vec2::new(
        (ndc.x * 0.5 + 0.5) * width as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * height as f32,
    )
}

/// Projects a world-space line, clips it to the near plane (and the rest of the frustum), and
/// emits a depth-tested overlay line.
///
/// A line crossing the near plane is clipped, not dropped; a line fully behind the camera (or
/// with a degenerate `w`) emits nothing. After clipping, `clip.z / clip.w` is the Vulkan
/// `[0,1]` NDC depth the rasterizer interpolates, matching the depth buffer.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_clipped_overlay_line(
    vertices: &mut Vec<OverlayVertex>,
    view_projection: &Mat4,
    a_world: Vec3,
    b_world: Vec3,
    thickness: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    let mut a_clip = *view_projection * a_world.extend(1.0);
    let mut b_clip = *view_projection * b_world.extend(1.0);
    if a_clip.w.abs() < 0.0001
        || b_clip.w.abs() < 0.0001
        || !clip_overlay_line(&mut a_clip, &mut b_clip)
    {
        return;
    }
    add_line(
        vertices,
        clip_to_pixel(a_clip, width, height),
        clip_to_pixel(b_clip, width, height),
        thickness,
        color,
        width,
        height,
        a_clip.z / a_clip.w,
        b_clip.z / b_clip.w,
    );
}

/// The 12-edge index list for an 8-corner box, the order both the AABB and the frustum use.
pub(super) const BOX_EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 0),
    (4, 5),
    (5, 6),
    (6, 7),
    (7, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

/// A world-space AABB as 12 depth-tested edges.
pub(super) fn add_world_aabb(
    vertices: &mut Vec<OverlayVertex>,
    view_projection: &Mat4,
    lo: Vec3,
    hi: Vec3,
    color: Vec4,
    width: u32,
    height: u32,
) {
    let corners = [
        Vec3::new(lo.x, lo.y, lo.z),
        Vec3::new(hi.x, lo.y, lo.z),
        Vec3::new(hi.x, hi.y, lo.z),
        Vec3::new(lo.x, hi.y, lo.z),
        Vec3::new(lo.x, lo.y, hi.z),
        Vec3::new(hi.x, lo.y, hi.z),
        Vec3::new(hi.x, hi.y, hi.z),
        Vec3::new(lo.x, hi.y, hi.z),
    ];
    for (i, j) in BOX_EDGES {
        add_clipped_overlay_line(
            vertices,
            view_projection,
            corners[i],
            corners[j],
            1.5,
            color,
            width,
            height,
        );
    }
}

/// A world-space ring of `radius` in the plane spanned by unit axes `a`, `b`.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_world_ring(
    vertices: &mut Vec<OverlayVertex>,
    view_projection: &Mat4,
    center: Vec3,
    a: Vec3,
    b: Vec3,
    radius: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    const SEGMENTS: u32 = 32;
    let mut prev = center + a * radius;
    for i in 1..=SEGMENTS {
        let t = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let cur = center + (a * t.cos() + b * t.sin()) * radius;
        add_clipped_overlay_line(
            vertices,
            view_projection,
            prev,
            cur,
            1.5,
            color,
            width,
            height,
        );
        prev = cur;
    }
}

/// A world-space arc of `radius` over `[t0, t1]` in the plane spanned by unit axes `a`, `b`.
/// Used for the capsule's pole hemispheres.
#[allow(clippy::too_many_arguments)]
pub(super) fn add_world_arc(
    vertices: &mut Vec<OverlayVertex>,
    view_projection: &Mat4,
    center: Vec3,
    a: Vec3,
    b: Vec3,
    radius: f32,
    t0: f32,
    t1: f32,
    color: Vec4,
    width: u32,
    height: u32,
) {
    const SEGMENTS: u32 = 16;
    let mut prev = center + (a * t0.cos() + b * t0.sin()) * radius;
    for i in 1..=SEGMENTS {
        let t = t0 + (t1 - t0) * i as f32 / SEGMENTS as f32;
        let cur = center + (a * t.cos() + b * t.sin()) * radius;
        add_clipped_overlay_line(
            vertices,
            view_projection,
            prev,
            cur,
            1.5,
            color,
            width,
            height,
        );
        prev = cur;
    }
}

/// An oriented box: the 8 local ±`he` corners transformed by `model`, drawn as 12 edges.
/// Unlike [`add_world_aabb`] this keeps the box oriented.
pub(super) fn add_world_oriented_box(
    vertices: &mut Vec<OverlayVertex>,
    view_projection: &Mat4,
    model: &Mat4,
    he: Vec3,
    color: Vec4,
    width: u32,
    height: u32,
) {
    let mut corners = [Vec3::ZERO; 8];
    for (corner, slot) in corners.iter_mut().enumerate() {
        let local = Vec3::new(
            if corner & 1 != 0 { he.x } else { -he.x },
            if corner & 2 != 0 { he.y } else { -he.y },
            if corner & 4 != 0 { he.z } else { -he.z },
        );
        *slot = model.transform_point3(local);
    }
    // The oriented box uses a face-loop edge order distinct from the AABB's corner sweep.
    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (1, 3),
        (3, 2),
        (2, 0),
        (4, 5),
        (5, 7),
        (7, 6),
        (6, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (i, j) in EDGES {
        add_clipped_overlay_line(
            vertices,
            view_projection,
            corners[i],
            corners[j],
            1.5,
            color,
            width,
            height,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::test_camera;
    use super::*;
    use saffron_scene::camera_projection;

    #[test]
    fn add_line_emits_two_triangles_with_edge() {
        let mut v = Vec::new();
        add_line_flat(
            &mut v,
            Vec2::new(100.0, 100.0),
            Vec2::new(200.0, 100.0),
            4.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert_eq!(v.len(), 6);
        // half = 2, ext = 3 → edge.x = ±1.5 for the two sides, edge.z = 2 (half-thickness).
        assert!((v[0].edge.x - 1.5).abs() < 1e-5, "positive side edge coord");
        assert!((v[2].edge.x + 1.5).abs() < 1e-5, "negative side edge coord");
        assert!(
            (v[0].edge.z - 2.0).abs() < 1e-5,
            "edge.z is the half-thickness"
        );
        let mut empty = Vec::new();
        add_line_flat(
            &mut empty,
            Vec2::new(5.0, 5.0),
            Vec2::new(5.0, 5.0),
            4.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert!(empty.is_empty(), "a degenerate line emits nothing");
    }

    #[test]
    fn primitive_vertex_counts_match() {
        let mut quad = Vec::new();
        add_quad(
            &mut quad,
            [
                Vec2::new(0.0, 0.0),
                Vec2::new(0.0, 40.0),
                Vec2::new(40.0, 40.0),
                Vec2::new(40.0, 0.0),
            ],
            Vec4::ONE,
            1280,
            720,
        );
        assert_eq!(quad.len(), 6);

        let mut boxed = Vec::new();
        add_box(
            &mut boxed,
            Vec2::new(50.0, 50.0),
            16.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert_eq!(boxed.len(), 6);

        let mut fill = Vec::new();
        add_circle_fill(&mut fill, Vec2::new(80.0, 80.0), 10.0, Vec4::ONE, 1280, 720);
        assert_eq!(fill.len(), 24 * 3, "24 segments × 3 vertices");

        let mut outline = Vec::new();
        add_circle_outline(
            &mut outline,
            Vec2::new(80.0, 80.0),
            10.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert_eq!(outline.len(), 32 * 6, "32 line segments × 6 vertices");
    }

    #[test]
    fn pixel_to_ndc_roundtrip() {
        // Top-left origin: (0,0) → (-1,-1), (w,h) → (+1,+1).
        let (w, h) = (1280u32, 720u32);
        let center = pixel_to_ndc(Vec2::new(w as f32 / 2.0, h as f32 / 2.0), w, h);
        assert!(
            center.abs_diff_eq(Vec2::ZERO, 1e-5),
            "center → origin: {center:?}"
        );
        let tl = pixel_to_ndc(Vec2::ZERO, w, h);
        assert!(
            tl.abs_diff_eq(Vec2::new(-1.0, -1.0), 1e-5),
            "top-left → (-1,-1)"
        );
        let br = pixel_to_ndc(Vec2::new(w as f32, h as f32), w, h);
        assert!(
            br.abs_diff_eq(Vec2::new(1.0, 1.0), 1e-5),
            "bottom-right → (1,1)"
        );
    }

    #[test]
    fn clipped_overlay_line_near_plane() {
        let cam = test_camera(Vec3::new(0.0, 0.0, 5.0));
        let vp = camera_projection(&cam, 1280.0 / 720.0) * cam.view;

        // The eye is at z=5 and near at world z=4.9, so this segment crosses the near plane and
        // must be clipped onto it rather than dropped.
        let mut crossing = Vec::new();
        add_clipped_overlay_line(
            &mut crossing,
            &vp,
            Vec3::new(1.0, 0.5, 4.0),
            Vec3::new(0.0, 0.0, 4.95),
            2.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert_eq!(
            crossing.len(),
            6,
            "a near-plane crossing line is clipped, not dropped"
        );

        let mut behind = Vec::new();
        add_clipped_overlay_line(
            &mut behind,
            &vp,
            Vec3::new(0.0, 0.0, 20.0),
            Vec3::new(0.0, 0.0, 30.0),
            2.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert!(behind.is_empty(), "a fully-behind line emits nothing");
    }

    #[test]
    fn clip_overlay_line_rejects_fully_outside() {
        let mut a = Vec4::new(10.0, 0.0, 0.5, 1.0);
        let mut b = Vec4::new(12.0, 0.0, 0.5, 1.0);
        assert!(!clip_overlay_line(&mut a, &mut b), "both past x > w");
        let mut a = Vec4::new(-2.0, 0.0, 0.5, 1.0);
        let mut b = Vec4::new(0.5, 0.0, 0.5, 1.0);
        assert!(clip_overlay_line(&mut a, &mut b), "straddling is kept");
        assert!(
            a.x + a.w >= -1e-5,
            "the left endpoint is clipped onto the plane"
        );
    }
}
