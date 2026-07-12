//! The native gizmo overlay geometry: the CPU builder set the editor's in-viewport chrome
//! is drawn from.
//!
//! The overlay geometry builders live here because they touch the renderer's [`OverlayVertex`]
//! / `submit_overlay`. Every builder is pure CPU geometry that pushes into a
//! `&mut Vec<OverlayVertex>` — no shared accumulator. The hit-test / projection / drag math
//! lives in `saffron-sceneedit` (the gizmo module); these builders only *consume* it to emit
//! geometry.
//!
//! [`build_scene_edit_overlay`] builds a `depth_tested` range (camera frustums, debug
//! overlays, colliders — occluded by scene geometry) and an `on_top` range (entity billboards,
//! the active gizmo, the skeleton), handing both to [`Renderer::submit_overlay`] so the overlay
//! pass draws each with its own pipeline from one buffer. `edit_chrome` gates the Edit-only
//! chrome (hidden in Play and the asset preview); colliders + the skeleton sit outside the gate
//! with their own preview guards.

use glam::{Mat4, Vec2, Vec3, Vec4};

use saffron_assets::{AssetServer, GpuUploader};
use saffron_geometry::world_aabb_from_corners;
use saffron_rendering::OverlayVertex;
use saffron_scene::{
    Bone, Camera, CameraView, Collider, Entity, FogShape, FogVolume, IdComponent, Mesh, PointLight,
    Relationship, Scene, Shape, SkinnedMesh, SpotLight, Transform, camera_projection,
};
use saffron_sceneedit::{
    NativeGizmoHandle, NativeGizmoMode, SceneEditContext, axis_color, camera_position, gizmo_axes,
    gizmo_plane_corners, pixel_to_ndc, ring_basis, viewport_project,
};

/// What billboard glyph (if any) a meshless entity is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BillboardKind {
    /// A mesh entity — no billboard.
    None,
    /// A point-light bulb glyph.
    PointLight,
    /// A spot-light bulb + aim line.
    SpotLight,
    /// A camera glyph.
    Camera,
    /// A fog-volume cloud glyph.
    FogVolume,
}

/// Pushes a flat-colored triangle. Edge + depth are zero (no feather, on top).
fn add_triangle(vertices: &mut Vec<OverlayVertex>, a: Vec2, b: Vec2, c: Vec2, color: Vec4) {
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
fn add_line(
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
fn add_line_flat(
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
fn add_quad(
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
fn add_box(
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
fn add_rect_outline(
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
fn add_circle_fill(
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
fn add_circle_outline(
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
fn add_bulb_icon(
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
fn add_camera_icon(
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
fn add_fog_icon(
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

/// The billboard glyph an entity is drawn with: none for a mesh, otherwise by its light /
/// camera component.
fn billboard_kind(scene: &Scene, entity: Entity) -> BillboardKind {
    if scene.has_component::<Mesh>(entity) {
        return BillboardKind::None;
    }
    if scene.has_component::<PointLight>(entity) {
        return BillboardKind::PointLight;
    }
    if scene.has_component::<SpotLight>(entity) {
        return BillboardKind::SpotLight;
    }
    if scene.has_component::<Camera>(entity) {
        return BillboardKind::Camera;
    }
    if scene.has_component::<FogVolume>(entity) {
        return BillboardKind::FogVolume;
    }
    BillboardKind::None
}

/// Builds the active-mode gizmo geometry for the selected entity.
///
/// Reads the projected handle positions from the `saffron-sceneedit` gizmo math; this only
/// emits geometry. A no-op when nothing transformable is selected or the origin is off-screen.
fn build_native_gizmo(
    editor: &SceneEditContext,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if editor.selected == Entity::NULL || !editor.scene.has_component::<Transform>(editor.selected)
    {
        return;
    }
    let position = editor.scene.world_translation(editor.selected);
    let axes = gizmo_axes(
        editor.scene.world_rotation(editor.selected),
        editor.native_gizmo.space,
    );
    let origin = viewport_project(cam, width, height, position);
    if !origin.visible {
        return;
    }
    let distance = (camera_position(cam) - position).length();
    let axis_len = (distance * 0.22).max(0.75);
    let handles = [
        NativeGizmoHandle::X,
        NativeGizmoHandle::Y,
        NativeGizmoHandle::Z,
    ];
    // Rotate mode shows only the rings; the straight axis lines belong to translate/scale.
    if editor.native_gizmo.mode != NativeGizmoMode::Rotate {
        for i in 0..3 {
            let end = viewport_project(cam, width, height, position + axes[i] * axis_len);
            if !end.visible {
                continue;
            }
            add_line_flat(
                vertices,
                origin.pixel,
                end.pixel,
                5.0,
                axis_color(handles[i], &editor.native_gizmo),
                width,
                height,
            );
            let box_size = if editor.native_gizmo.mode == NativeGizmoMode::Scale {
                12.0
            } else {
                8.0
            };
            add_box(
                vertices,
                end.pixel,
                box_size,
                axis_color(handles[i], &editor.native_gizmo),
                width,
                height,
            );
        }
    }
    if editor.native_gizmo.mode == NativeGizmoMode::Translate {
        // The drawn quads are the exact hit-test geometry (gizmo_plane_corners), so the
        // plane handles always sit under the cursor that activates them.
        let planes = [
            (NativeGizmoHandle::Xy, (0usize, 1usize)),
            (NativeGizmoHandle::Yz, (1usize, 2usize)),
            (NativeGizmoHandle::Xz, (0usize, 2usize)),
        ];
        for (handle, pair) in planes {
            let corners = gizmo_plane_corners(cam, width, height, position, &axes, axis_len, pair);
            if !corners[0].visible
                || !corners[1].visible
                || !corners[2].visible
                || !corners[3].visible
            {
                continue;
            }
            add_quad(
                vertices,
                [
                    corners[0].pixel,
                    corners[1].pixel,
                    corners[2].pixel,
                    corners[3].pixel,
                ],
                axis_color(handle, &editor.native_gizmo),
                width,
                height,
            );
        }
    } else if editor.native_gizmo.mode == NativeGizmoMode::Rotate {
        const SEGMENTS: u32 = 96;
        let radius = axis_len * 0.72;
        for axis in 0..3 {
            let (a, b) = ring_basis(axes[axis]);
            let mut prev = saffron_sceneedit::GizmoProjection::default();
            for i in 0..=SEGMENTS {
                let t = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let cur = viewport_project(
                    cam,
                    width,
                    height,
                    position + (a * t.cos() + b * t.sin()) * radius,
                );
                if i > 0 && prev.visible && cur.visible {
                    add_line_flat(
                        vertices,
                        prev.pixel,
                        cur.pixel,
                        3.0,
                        axis_color(handles[axis], &editor.native_gizmo),
                        width,
                        height,
                    );
                }
                prev = cur;
            }
        }
    } else {
        add_box(
            vertices,
            origin.pixel,
            13.0,
            axis_color(NativeGizmoHandle::Uniform, &editor.native_gizmo),
            width,
            height,
        );
    }
}

/// Draws a line skeleton over the selected rig: a bone segment to each joint's parent, a
/// screen-constant joint dot, and (when enabled) three short RGB axis lines per joint.
///
/// Always on top. Renders in Edit and Play so a playing clip shows its bones move; scoped to
/// the selected (or previewed root) entity to bound the vertex count.
fn build_skeleton_overlay(
    editor: &mut SceneEditContext,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if !editor.skeleton_overlay.show || width == 0 || height == 0 {
        return;
    }
    let overlay = editor.skeleton_overlay;
    let previewing = editor.preview_active_view;
    let preview_root = editor.preview_root_entity;
    // The model the overlay draws bones for: the previewed model's root while previewing
    // (so highlighting a bone via the dedicated channel never blanks the overlay, and a bone
    // has no SkinnedMesh of its own), else the selected entity in the normal scene-edit view.
    let mut target = editor.selected;
    if previewing {
        target = preview_root;
    }
    if target == Entity::NULL {
        return;
    }

    // Resolve the highlighted joint (a get-asset-model node index) to its spawned entity
    // uuid; only set while previewing, drawn in a distinct tint.
    let mut highlight_uuid = saffron_core::Uuid(0);
    if previewing
        && overlay.highlight_joint >= 0
        && (overlay.highlight_joint as usize) < editor.preview_bone_by_node.len()
    {
        highlight_uuid = editor.preview_bone_by_node[overlay.highlight_joint as usize];
    }

    let scene = editor.active_scene();
    // The SkinnedMesh rides a child mesh entity, not the selected/preview container root, so
    // resolve the rig within the model's forest. An unrigged model resolves to nothing (no
    // skeleton to draw), which is correct.
    let Some(rig) = scene.model_rig_entity(target) else {
        return;
    };
    let bone_handles = scene
        .with_component::<SkinnedMesh, _>(rig, |skin| skin.bone_handles.clone())
        .unwrap_or_default();

    const BONE_COLOR: Vec4 = Vec4::new(0.55, 0.78, 1.0, 0.95);
    const JOINT_COLOR: Vec4 = Vec4::new(1.0, 0.78, 0.18, 1.0);
    const HIGHLIGHT_COLOR: Vec4 = Vec4::new(0.30, 1.0, 0.45, 1.0);
    const AXIS_LEN: f32 = 0.08; // per-joint axis length in world units
    let axis_colors = [
        Vec4::new(1.0, 0.32, 0.32, 0.95),
        Vec4::new(0.40, 0.90, 0.40, 0.95),
        Vec4::new(0.42, 0.62, 1.0, 0.95),
    ];

    for bone in bone_handles {
        if bone == Entity::NULL {
            continue;
        }
        let world_pos = scene.world_translation(bone);
        let joint = viewport_project(cam, width, height, world_pos);
        if !joint.visible {
            continue;
        }
        // Bone segment to the parent, only when the parent is itself a joint.
        let parent_handle = scene
            .with_component::<Relationship, _>(bone, |rel| rel.parent_handle)
            .unwrap_or(None);
        if let Some(parent) = parent_handle
            && scene.has_component::<Bone>(parent)
        {
            let parent_proj = viewport_project(cam, width, height, scene.world_translation(parent));
            if parent_proj.visible {
                add_line_flat(
                    vertices,
                    parent_proj.pixel,
                    joint.pixel,
                    2.0,
                    BONE_COLOR,
                    width,
                    height,
                );
            }
        }
        // Joint dot: a constant pixel radius so the dot stays the same on-screen size at any
        // zoom. The highlighted joint draws larger in a distinct tint.
        let bone_uuid = scene
            .with_component::<IdComponent, _>(bone, |id| id.id)
            .unwrap_or(saffron_core::Uuid(0));
        let highlighted =
            highlight_uuid.value() != 0 && bone_uuid.value() == highlight_uuid.value();
        let base_radius = overlay.joint_size.max(2.5);
        let (radius, joint_color) = if highlighted {
            (base_radius * 1.8, HIGHLIGHT_COLOR)
        } else {
            (base_radius, JOINT_COLOR)
        };
        add_circle_fill(vertices, joint.pixel, radius, joint_color, width, height);
        // Optional per-joint RGB axes from the bone's world-rotation basis.
        if overlay.axes {
            let rotation = scene.world_rotation(bone);
            let basis = [rotation * Vec3::X, rotation * Vec3::Y, rotation * Vec3::Z];
            for axis in 0..3 {
                let tip = viewport_project(cam, width, height, world_pos + basis[axis] * AXIS_LEN);
                if tip.visible {
                    add_line_flat(
                        vertices,
                        joint.pixel,
                        tip.pixel,
                        1.5,
                        axis_colors[axis],
                        width,
                        height,
                    );
                }
            }
        }
    }
}

/// Colored screen-space glyphs for meshless light/camera entities.
fn build_scene_edit_billboards(
    editor: &mut SceneEditContext,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if width == 0 || height == 0 {
        return;
    }
    let selected = editor.selected;
    let scene = editor.active_scene();

    // `for_each` borrows the scene mutably, so collect the transformable entities first, then
    // do the per-entity world reads.
    let mut entities: Vec<Entity> = Vec::new();
    scene.for_each::<&Transform, _>(|entity, _| entities.push(entity));

    let selected_color = Vec4::new(1.0, 0.78, 0.18, 1.0);
    for entity in entities {
        let kind = billboard_kind(scene, entity);
        if kind == BillboardKind::None {
            continue;
        }
        let position = scene.world_translation(entity);
        let p = viewport_project(cam, width, height, position);
        if !p.visible {
            continue;
        }
        let sel = selected == entity;
        match kind {
            BillboardKind::PointLight => {
                let color = if sel {
                    selected_color
                } else {
                    Vec4::new(1.0, 0.84, 0.34, 0.95)
                };
                add_bulb_icon(vertices, p.pixel, color, width, height);
            }
            BillboardKind::SpotLight => {
                let color = if sel {
                    selected_color
                } else {
                    Vec4::new(0.45, 0.85, 1.0, 0.9)
                };
                add_bulb_icon(vertices, p.pixel, color, width, height);
                let forward = scene.world_rotation(entity) * Vec3::NEG_Z;
                let tip = viewport_project(cam, width, height, position + forward * 0.6);
                if tip.visible {
                    add_line_flat(vertices, p.pixel, tip.pixel, 3.0, color, width, height);
                }
            }
            BillboardKind::Camera => {
                let show_model = scene
                    .with_component::<Camera, _>(entity, |camera| camera.show_model)
                    .unwrap_or(false);
                if show_model {
                    continue;
                }
                let color = if sel {
                    selected_color
                } else {
                    Vec4::new(0.85, 0.87, 0.92, 0.95)
                };
                add_camera_icon(vertices, p.pixel, color, width, height);
            }
            BillboardKind::FogVolume => {
                let color = if sel {
                    selected_color
                } else {
                    Vec4::new(0.7, 0.8, 0.92, 0.9)
                };
                add_fog_icon(vertices, p.pixel, color, width, height);
            }
            BillboardKind::None => {}
        }
    }
}

/// Clips a clip-space line segment to the six clip planes, mutating the endpoints in place.
/// Returns `false` when the segment is fully outside.
///
/// The near plane is `z + w >= 0` (the GL `[-1, 1]` clip convention `camera_projection`
/// produces): the projection is `perspective_rh_gl`, so the line clips against the same frustum
/// the scene's depth buffer was rasterized with.
fn clip_overlay_line(a: &mut Vec4, b: &mut Vec4) -> bool {
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
        && clip_plane(a, b, |p| p.z + p.w)
        && clip_plane(a, b, |p| p.w - p.z)
}

/// A clip-space point to viewport pixels (top-left origin).
fn clip_to_pixel(clip: Vec4, width: u32, height: u32) -> Vec2 {
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
fn add_clipped_overlay_line(
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
const BOX_EDGES: [(usize, usize); 12] = [
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
fn add_world_aabb(
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
fn add_world_ring(
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
fn add_world_arc(
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
fn add_world_oriented_box(
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

/// The camera-frustum overlays: a clipped 12-edge wireframe per `show_frustum` camera.
/// Depth-tested, Edit-only.
fn build_scene_edit_camera_frustums(
    editor: &mut SceneEditContext,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if width == 0 || height == 0 {
        return;
    }
    const FRUSTUM_COLOR: Vec4 = Vec4::new(0.78, 0.29, 0.02, 0.95);
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    let scene = editor.active_scene();

    let mut cameras: Vec<(Entity, Camera)> = Vec::new();
    scene.for_each::<(&Transform, &Camera), _>(|entity, (_, camera)| {
        cameras.push((entity, *camera));
    });

    for (entity, camera) in cameras {
        if !camera.show_frustum {
            continue;
        }
        let near_plane = camera.near_plane.max(0.001);
        let max_distance = camera.frustum_max_distance.max(near_plane + 0.001);
        let far_plane = camera.far_plane.max(near_plane + 0.001).min(max_distance);
        let half_fov = camera.fov.clamp(1.0, 179.0).to_radians() * 0.5;
        let near_y = half_fov.tan() * near_plane;
        let near_x = near_y * aspect;
        let far_y = half_fov.tan() * far_plane;
        let far_x = far_y * aspect;
        let model = scene.world_matrix(entity);
        let local = [
            Vec3::new(-near_x, -near_y, -near_plane),
            Vec3::new(-near_x, near_y, -near_plane),
            Vec3::new(near_x, near_y, -near_plane),
            Vec3::new(near_x, -near_y, -near_plane),
            Vec3::new(-far_x, -far_y, -far_plane),
            Vec3::new(-far_x, far_y, -far_plane),
            Vec3::new(far_x, far_y, -far_plane),
            Vec3::new(far_x, -far_y, -far_plane),
        ];
        let mut world = [Vec3::ZERO; 8];
        for (i, slot) in world.iter_mut().enumerate() {
            *slot = model.transform_point3(local[i]);
        }
        for (i, j) in BOX_EDGES {
            add_clipped_overlay_line(
                vertices,
                &view_projection,
                world[i],
                world[j],
                2.0,
                FRUSTUM_COLOR,
                width,
                height,
            );
        }
    }
}

/// The viewport debug overlays (`set-debug-overlays`): per-entity bounds (the exact box
/// `pick_entity` tests, static + skinned joint-union), the whole-scene AABB the shadow fit
/// uses, and point/spot light volumes. Depth-tested, Edit-only.
fn build_debug_overlays(
    editor: &mut SceneEditContext,
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    let opts = editor.debug_overlays;
    if width == 0 || height == 0 || (!opts.bounds && !opts.scene_aabb && !opts.light_volumes) {
        return;
    }
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    const STATIC_BOUNDS_COLOR: Vec4 = Vec4::new(0.35, 0.95, 0.55, 0.9);
    const SKINNED_BOUNDS_COLOR: Vec4 = Vec4::new(0.95, 0.45, 0.95, 0.9);
    const SCENE_AABB_COLOR: Vec4 = Vec4::new(0.95, 0.85, 0.25, 0.85);
    const POINT_COLOR: Vec4 = Vec4::new(1.0, 0.84, 0.34, 0.85);
    const SPOT_COLOR: Vec4 = Vec4::new(0.45, 0.85, 1.0, 0.85);

    let mut scene_min = Vec3::splat(f32::MAX);
    let mut scene_max = Vec3::splat(f32::MIN);
    let mut have_scene = false;

    let scene = editor.active_scene();

    let mut static_meshes: Vec<(Entity, Mesh)> = Vec::new();
    scene.for_each::<(&Transform, &Mesh), _>(|entity, (_, mesh)| {
        static_meshes.push((entity, *mesh));
    });
    for (entity, mesh) in static_meshes {
        let Some(mesh_ref) = assets.load_mesh_asset(gpu, mesh.mesh) else {
            continue;
        };
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        world_aabb_from_corners(
            &scene.world_matrix(entity),
            mesh_ref.bounds_min,
            mesh_ref.bounds_max,
            &mut lo,
            &mut hi,
        );
        if opts.bounds {
            add_world_aabb(
                vertices,
                &view_projection,
                lo,
                hi,
                STATIC_BOUNDS_COLOR,
                width,
                height,
            );
        }
        scene_min = scene_min.min(lo);
        scene_max = scene_max.max(hi);
        have_scene = true;
    }

    let mut skins: Vec<(Entity, SkinnedMesh)> = Vec::new();
    scene.for_each::<(&Transform, &SkinnedMesh), _>(|entity, (_, skin)| {
        skins.push((entity, skin.clone()));
    });
    for (_, skin) in skins {
        let Some(mesh_ref) = assets.load_mesh_asset(gpu, skin.mesh) else {
            continue;
        };
        let palette = scene.joint_matrices(&skin);
        if palette.is_empty() {
            continue;
        }
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for joint in &palette {
            world_aabb_from_corners(
                joint,
                mesh_ref.bounds_min,
                mesh_ref.bounds_max,
                &mut lo,
                &mut hi,
            );
        }
        if opts.bounds {
            add_world_aabb(
                vertices,
                &view_projection,
                lo,
                hi,
                SKINNED_BOUNDS_COLOR,
                width,
                height,
            );
        }
        scene_min = scene_min.min(lo);
        scene_max = scene_max.max(hi);
        have_scene = true;
    }

    // The whole-scene AABB the directional-shadow / DDGI fit derives each frame; render_scene
    // recomputes and discards it, and this recompute intentionally mirrors that one.
    if opts.scene_aabb && have_scene {
        add_world_aabb(
            vertices,
            &view_projection,
            scene_min,
            scene_max,
            SCENE_AABB_COLOR,
            width,
            height,
        );
    }

    // Local fog volumes: a passive box/sphere wireframe of the injected bounds, in the depth-tested
    // overlay range so scene geometry occludes it. Reuses the mesh-bounds toggle.
    if opts.bounds {
        const FOG_VOLUME_COLOR: Vec4 = Vec4::new(0.6, 0.75, 0.95, 0.85);
        let mut fog_volumes: Vec<(Entity, FogVolume)> = Vec::new();
        scene.for_each::<(&Transform, &FogVolume), _>(|entity, (_, volume)| {
            fog_volumes.push((entity, *volume));
        });
        for (entity, volume) in fog_volumes {
            let model = scene.world_matrix(entity);
            match volume.shape {
                FogShape::Box => {
                    add_world_oriented_box(
                        vertices,
                        &view_projection,
                        &model,
                        volume.extents,
                        FOG_VOLUME_COLOR,
                        width,
                        height,
                    );
                }
                FogShape::Sphere => {
                    let center = model.col(3).truncate();
                    for (a, b) in [(Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z), (Vec3::X, Vec3::Z)] {
                        add_world_ring(
                            vertices,
                            &view_projection,
                            center,
                            a,
                            b,
                            volume.radius,
                            FOG_VOLUME_COLOR,
                            width,
                            height,
                        );
                    }
                }
            }
        }
    }

    if opts.light_volumes {
        let mut point_lights: Vec<(Entity, PointLight)> = Vec::new();
        scene.for_each::<(&Transform, &PointLight), _>(|entity, (_, light)| {
            point_lights.push((entity, *light));
        });
        for (entity, light) in point_lights {
            if light.range <= 0.0 {
                continue;
            }
            let center = scene.world_translation(entity);
            add_world_ring(
                vertices,
                &view_projection,
                center,
                Vec3::X,
                Vec3::Y,
                light.range,
                POINT_COLOR,
                width,
                height,
            );
            add_world_ring(
                vertices,
                &view_projection,
                center,
                Vec3::Y,
                Vec3::Z,
                light.range,
                POINT_COLOR,
                width,
                height,
            );
            add_world_ring(
                vertices,
                &view_projection,
                center,
                Vec3::X,
                Vec3::Z,
                light.range,
                POINT_COLOR,
                width,
                height,
            );
        }

        let mut spot_lights: Vec<(Entity, SpotLight)> = Vec::new();
        scene.for_each::<(&Transform, &SpotLight), _>(|entity, (_, light)| {
            spot_lights.push((entity, *light));
        });
        for (entity, light) in spot_lights {
            if light.range <= 0.0 {
                continue;
            }
            // Matches the lighting upload: dir = normalize(world_rotation * component dir).
            let apex = scene.world_translation(entity);
            let dir = (scene.world_rotation(entity) * light.direction).normalize();
            let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
            let right = dir.cross(up).normalize();
            let up2 = right.cross(dir);
            let base = apex + dir * light.range;
            let base_radius = light.range * light.outer_angle.clamp(0.5, 89.0).to_radians().tan();
            add_world_ring(
                vertices,
                &view_projection,
                base,
                right,
                up2,
                base_radius,
                SPOT_COLOR,
                width,
                height,
            );
            for i in 0..4 {
                let t = i as f32 / 4.0 * std::f32::consts::TAU;
                let rim = base + (right * t.cos() + up2 * t.sin()) * base_radius;
                add_clipped_overlay_line(
                    vertices,
                    &view_projection,
                    apex,
                    rim,
                    1.5,
                    SPOT_COLOR,
                    width,
                    height,
                );
            }
        }
    }
}

/// The physics collider overlay (`set-debug-overlays {colliders}`): a world-space wireframe
/// per [`Collider`] — oriented box / sphere / capsule, or the cook-source mesh AABB for
/// hull/mesh.
///
/// Drawn SCALE-FREE to match the Jolt body: position + rotation only, with the collider offset
/// in the rotated body-local frame (never `world_matrix`, which carries entity scale). Reads
/// the authored [`Collider`], present in Edit AND Play, so it sits outside `edit_chrome` and
/// carries its own preview guard.
fn build_collider_overlays(
    editor: &mut SceneEditContext,
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if !editor.debug_overlays.colliders || editor.previewing() || width == 0 || height == 0 {
        return;
    }
    let selected = editor.selected;
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    const COLLIDER_COLOR: Vec4 = Vec4::new(0.20, 0.95, 0.85, 0.9); // cyan: solid colliders
    const SENSOR_COLOR: Vec4 = Vec4::new(0.30, 0.90, 0.40, 0.9); // green: trigger volumes
    const SELECTED_COLOR: Vec4 = Vec4::new(1.0, 0.55, 0.1, 1.0); // orange: the selected collider

    let scene = editor.active_scene();
    let mut colliders: Vec<(Entity, Collider)> = Vec::new();
    scene.for_each::<(&Transform, &Collider), _>(|entity, (_, collider)| {
        colliders.push((entity, *collider));
    });

    for (entity, collider) in colliders {
        let color = if selected == entity {
            SELECTED_COLOR
        } else if collider.is_sensor {
            SENSOR_COLOR
        } else {
            COLLIDER_COLOR
        };
        // Scale-free body frame: T(pos) * R(rot) * T(offset) — the offset rides the rotated
        // body-local frame, the body carries no scale.
        let model = Mat4::from_translation(scene.world_translation(entity))
            * Mat4::from_quat(scene.world_rotation(entity))
            * Mat4::from_translation(collider.offset);
        let he = collider.half_extents.max(Vec3::splat(0.01));
        let center = model.w_axis.truncate();

        match collider.shape {
            Shape::Box => {
                add_world_oriented_box(
                    vertices,
                    &view_projection,
                    &model,
                    he,
                    color,
                    width,
                    height,
                );
            }
            Shape::Sphere => {
                // Sphere radius packs from half_extents.x; the three world-axis rings are
                // rotation-invariant.
                add_world_ring(
                    vertices,
                    &view_projection,
                    center,
                    Vec3::X,
                    Vec3::Y,
                    he.x,
                    color,
                    width,
                    height,
                );
                add_world_ring(
                    vertices,
                    &view_projection,
                    center,
                    Vec3::Y,
                    Vec3::Z,
                    he.x,
                    color,
                    width,
                    height,
                );
                add_world_ring(
                    vertices,
                    &view_projection,
                    center,
                    Vec3::X,
                    Vec3::Z,
                    he.x,
                    color,
                    width,
                    height,
                );
            }
            Shape::Capsule => {
                // Y-up capsule: radius from half_extents.x, half-height from half_extents.y.
                // Axes from the body rotation columns.
                let radius = he.x;
                let half_height = he.y;
                let right = model.x_axis.truncate().normalize();
                let up = model.y_axis.truncate().normalize();
                let fwd = model.z_axis.truncate().normalize();
                let top_c = center + up * half_height;
                let bot_c = center - up * half_height;
                add_world_ring(
                    vertices,
                    &view_projection,
                    top_c,
                    right,
                    fwd,
                    radius,
                    color,
                    width,
                    height,
                );
                add_world_ring(
                    vertices,
                    &view_projection,
                    bot_c,
                    right,
                    fwd,
                    radius,
                    color,
                    width,
                    height,
                );
                for side in [right, -right, fwd, -fwd] {
                    add_clipped_overlay_line(
                        vertices,
                        &view_projection,
                        top_c + side * radius,
                        bot_c + side * radius,
                        1.5,
                        color,
                        width,
                        height,
                    );
                }
                let pi = std::f32::consts::PI;
                add_world_arc(
                    vertices,
                    &view_projection,
                    top_c,
                    right,
                    up,
                    radius,
                    0.0,
                    pi,
                    color,
                    width,
                    height,
                );
                add_world_arc(
                    vertices,
                    &view_projection,
                    top_c,
                    fwd,
                    up,
                    radius,
                    0.0,
                    pi,
                    color,
                    width,
                    height,
                );
                add_world_arc(
                    vertices,
                    &view_projection,
                    bot_c,
                    right,
                    up,
                    radius,
                    pi,
                    2.0 * pi,
                    color,
                    width,
                    height,
                );
                add_world_arc(
                    vertices,
                    &view_projection,
                    bot_c,
                    fwd,
                    up,
                    radius,
                    pi,
                    2.0 * pi,
                    color,
                    width,
                    height,
                );
            }
            Shape::ConvexHull | Shape::Mesh => {
                // The documented cook-source-AABB approximation (no CPU hull edges are kept):
                // resolve the cook mesh (source_mesh, else the entity's Mesh, else SkinnedMesh)
                // and draw its bounds box, oriented by the same scale-free body frame.
                let mut mesh_id = collider.source_mesh;
                if mesh_id.value() == 0 && scene.has_component::<Mesh>(entity) {
                    mesh_id = scene
                        .with_component::<Mesh, _>(entity, |m| m.mesh)
                        .unwrap_or(saffron_core::Uuid(0));
                } else if mesh_id.value() == 0 && scene.has_component::<SkinnedMesh>(entity) {
                    mesh_id = scene
                        .with_component::<SkinnedMesh, _>(entity, |m| m.mesh)
                        .unwrap_or(saffron_core::Uuid(0));
                }
                if mesh_id.value() == 0 {
                    continue;
                }
                let Some(mesh_ref) = assets.load_mesh_asset(gpu, mesh_id) else {
                    continue;
                };
                let bounds_center = (mesh_ref.bounds_min + mesh_ref.bounds_max) * 0.5;
                let bounds_he =
                    ((mesh_ref.bounds_max - mesh_ref.bounds_min) * 0.5).max(Vec3::splat(0.01));
                let box_model = model * Mat4::from_translation(bounds_center);
                add_world_oriented_box(
                    vertices,
                    &view_projection,
                    &box_model,
                    bounds_he,
                    color,
                    width,
                    height,
                );
            }
        }
    }
}

/// Builds the editor overlay's two ranges, returned for the caller to submit once per frame.
///
/// Camera frustums + debug overlays are depth-tested against the scene (occluded by
/// geometry); billboards, the active gizmo, and the skeleton always draw on top. The gizmo +
/// billboards + frustums + debug overlays are Edit-only chrome, gated by `edit_chrome` (false
/// during play and the asset preview). Colliders + the skeleton sit outside the gate (they
/// read authored state and draw in Edit AND Play) with their own preview guards.
///
/// Returns `(depth_tested, on_top)` so the host's `render_ui` owns the renderer borrow when it
/// submits, and unit tests assert the ranges without a GPU.
#[must_use]
pub fn build_scene_edit_overlay(
    editor: &mut SceneEditContext,
    assets: &mut AssetServer,
    gpu: &dyn GpuUploader,
    cam: &CameraView,
    width: u32,
    height: u32,
    edit_chrome: bool,
) -> (Vec<OverlayVertex>, Vec<OverlayVertex>) {
    let mut depth_tested: Vec<OverlayVertex> = Vec::new();
    let mut on_top: Vec<OverlayVertex> = Vec::new();
    if edit_chrome {
        build_scene_edit_camera_frustums(editor, cam, width, height, &mut depth_tested);
        build_debug_overlays(editor, assets, gpu, cam, width, height, &mut depth_tested);
        build_scene_edit_billboards(editor, cam, width, height, &mut on_top);
        build_native_gizmo(editor, cam, width, height, &mut on_top);
    }
    // Colliders draw in Edit AND Play (they read the authored Collider), so they sit outside
    // edit_chrome like the skeleton overlay, with their own preview guard (inside the call).
    build_collider_overlays(editor, assets, gpu, cam, width, height, &mut depth_tested);
    build_skeleton_overlay(editor, cam, width, height, &mut on_top);
    (depth_tested, on_top)
}

#[cfg(test)]
mod tests {
    use super::*;
    use saffron_sceneedit::{GizmoOp, PlayState};

    /// A camera at `eye` looking at the origin, the framing the overlay tests project against.
    fn test_camera(eye: Vec3) -> CameraView {
        CameraView {
            view: Mat4::look_at_rh(eye, Vec3::ZERO, Vec3::Y),
            fov: 45.0,
            near_plane: 0.1,
            far_plane: 100.0,
        }
    }

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
        // A thick line is one quad: two triangles, six vertices.
        assert_eq!(v.len(), 6);
        // half = 2, ext = 3 → edge.x = ±1.5 for the two sides, edge.z = 2 (half-thickness).
        assert!((v[0].edge.x - 1.5).abs() < 1e-5, "positive side edge coord");
        assert!((v[2].edge.x + 1.5).abs() < 1e-5, "negative side edge coord");
        assert!(
            (v[0].edge.z - 2.0).abs() < 1e-5,
            "edge.z is the half-thickness"
        );
        // A degenerate (zero-length) line emits nothing.
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
        assert!(empty.is_empty());
    }

    #[test]
    fn primitive_vertex_counts_match() {
        // add_quad: one quad = six vertices (a degenerate quad emits none).
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

        // add_box: two triangles, six vertices.
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

        // add_circle_fill: 24 segments × 3 vertices = 72.
        let mut fill = Vec::new();
        add_circle_fill(&mut fill, Vec2::new(80.0, 80.0), 10.0, Vec4::ONE, 1280, 720);
        assert_eq!(fill.len(), 24 * 3);

        // add_circle_outline: 32 line segments × 6 vertices each = 192.
        let mut outline = Vec::new();
        add_circle_outline(
            &mut outline,
            Vec2::new(80.0, 80.0),
            10.0,
            Vec4::ONE,
            1280,
            720,
        );
        assert_eq!(outline.len(), 32 * 6);
    }

    #[test]
    fn pixel_to_ndc_roundtrip() {
        // The center pixel maps to NDC origin; the corners map to the ±1 extremes (top-left
        // origin: (0,0) → (-1,-1), (w,h) → (+1,+1)).
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

        // A line crossing the near plane (the eye is at z=5, near at world z=4.9): from a
        // point in front (z=4.0) to one nearer than the near plane (z=4.95) — clipped to the
        // near plane, not dropped, so it still emits the line quad (six vertices).
        let mut crossing = Vec::new();
        add_clipped_overlay_line(
            &mut crossing,
            &vp,
            Vec3::new(1.0, 0.5, 4.0), // off-axis, in front of the near plane
            Vec3::new(0.0, 0.0, 4.95), // nearer than the near plane (between 4.9 and the eye)
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

        // A line fully behind the camera (both endpoints past the eye at z=5) emits nothing.
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
        // Two points to the far right of clip space (x > w on both): rejected by the first plane.
        let mut a = Vec4::new(10.0, 0.0, 0.5, 1.0);
        let mut b = Vec4::new(12.0, 0.0, 0.5, 1.0);
        assert!(!clip_overlay_line(&mut a, &mut b));
        // A segment straddling the left plane is clipped in place (kept).
        let mut a = Vec4::new(-2.0, 0.0, 0.5, 1.0);
        let mut b = Vec4::new(0.5, 0.0, 0.5, 1.0);
        assert!(clip_overlay_line(&mut a, &mut b));
        assert!(
            a.x + a.w >= -1e-5,
            "the left endpoint is clipped onto the plane"
        );
    }

    /// A null GPU uploader: the overlay's mesh-resolving paths (debug/collider) negative-cache
    /// to `None` against an empty catalog before reaching it, so it is never actually invoked.
    struct NoGpu;
    impl GpuUploader for NoGpu {
        fn upload_mesh(
            &self,
            _mesh: &saffron_geometry::Mesh,
            _skin: &[saffron_geometry::VertexSkin],
            _morph: Option<&saffron_geometry::MorphData>,
            _sdf_bake: Option<&saffron_rendering::SdfBake>,
        ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuMesh>> {
            unreachable!("an empty catalog never reaches the uploader")
        }
        fn upload_texture(
            &self,
            _rgba: &[u8],
            _width: u32,
            _height: u32,
            _srgb: bool,
        ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuTexture>> {
            unreachable!()
        }
        fn upload_texture_float(
            &self,
            _rgba: &[f32],
            _width: u32,
            _height: u32,
        ) -> saffron_rendering::Result<std::sync::Arc<saffron_rendering::GpuTexture>> {
            unreachable!()
        }
        fn skinning_enabled(&self) -> bool {
            false
        }
    }

    /// A context with a transformable selection at the origin, a spot light, and a camera,
    /// plus the debug overlays toggled on — enough to exercise both overlay ranges.
    fn overlay_context() -> SceneEditContext {
        let mut ctx = SceneEditContext::new();
        // Select a transformable entity so the gizmo emits geometry.
        let target = ctx.scene.create_entity("Target");
        ctx.scene.relink_hierarchy();
        ctx.set_selection(target);
        ctx.gizmo_op = GizmoOp::Translate;
        ctx.sync_native_gizmo();
        // Add a spot light so a billboard + frustum (the seeded camera) contribute.
        let spot = ctx.scene.create_entity("Spot");
        let _ = ctx.scene.add_component(spot, SpotLight::default());
        ctx.scene.relink_hierarchy();
        ctx.scene.update_world_transforms();
        ctx
    }

    #[test]
    fn submit_scene_edit_overlay_ranges() {
        let mut ctx = overlay_context();
        let mut assets = AssetServer::new(std::env::temp_dir().join("saffron-overlay-test-edit"));
        let gpu = NoGpu;
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        let (w, h) = (1280u32, 720u32);

        // Edit chrome on: the on-top range carries the billboards + the gizmo; the depth-tested
        // range carries the seeded camera's frustum.
        let (depth, on_top) =
            build_scene_edit_overlay(&mut ctx, &mut assets, &gpu, &cam, w, h, true);
        assert!(
            !on_top.is_empty(),
            "edit chrome populates the on-top range (gizmo + billboards)"
        );
        assert!(
            !depth.is_empty(),
            "the seeded camera's frustum populates the depth-tested range"
        );

        // Edit chrome off (Play): the gizmo / billboards / frustums vanish; with the skeleton
        // off and no colliders, both ranges are empty.
        let (depth_play, on_top_play) =
            build_scene_edit_overlay(&mut ctx, &mut assets, &gpu, &cam, w, h, false);
        assert!(
            depth_play.is_empty() && on_top_play.is_empty(),
            "no edit chrome and no colliders/skeleton → both ranges empty"
        );
    }

    #[test]
    fn colliders_and_skeleton_ignore_edit_chrome() {
        let mut ctx = SceneEditContext::new();
        // A capsule collider on a transformable entity, debug-colliders on.
        let body = ctx.scene.create_entity("Body");
        let _ = ctx.scene.add_component(
            body,
            Collider {
                shape: Shape::Capsule,
                half_extents: Vec3::new(0.4, 0.8, 0.4),
                ..Collider::default()
            },
        );
        ctx.scene.relink_hierarchy();
        ctx.scene.update_world_transforms();
        ctx.debug_overlays.colliders = true;

        let mut assets =
            AssetServer::new(std::env::temp_dir().join("saffron-overlay-test-collider"));
        let gpu = NoGpu;
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        let (w, h) = (1280u32, 720u32);

        // Even with edit_chrome=false (Play), the collider draws into the depth-tested range.
        let (depth, _on_top) =
            build_scene_edit_overlay(&mut ctx, &mut assets, &gpu, &cam, w, h, false);
        assert!(
            !depth.is_empty(),
            "colliders draw in Play (outside edit_chrome)"
        );

        // The preview guard suppresses colliders while previewing.
        ctx.preview_scene = Some(Scene::new());
        ctx.preview_active_view = true;
        let (depth_preview, _) =
            build_scene_edit_overlay(&mut ctx, &mut assets, &gpu, &cam, w, h, false);
        assert!(
            depth_preview.is_empty(),
            "the collider preview guard suppresses them while previewing"
        );
    }

    #[test]
    fn billboard_kind_classifies_by_component() {
        let mut scene = Scene::new();
        let mesh = scene.create_entity("Mesh");
        let _ = scene.add_component(mesh, Mesh::default());
        let point = scene.create_entity("Point");
        let _ = scene.add_component(point, PointLight::default());
        let spot = scene.create_entity("Spot");
        let _ = scene.add_component(spot, SpotLight::default());
        let camera = scene.create_entity("Camera");
        let _ = scene.add_component(camera, Camera::default());
        let bare = scene.create_entity("Bare");

        assert_eq!(billboard_kind(&scene, mesh), BillboardKind::None);
        assert_eq!(billboard_kind(&scene, point), BillboardKind::PointLight);
        assert_eq!(billboard_kind(&scene, spot), BillboardKind::SpotLight);
        assert_eq!(billboard_kind(&scene, camera), BillboardKind::Camera);
        assert_eq!(billboard_kind(&scene, bare), BillboardKind::None);
    }

    #[test]
    fn native_gizmo_is_empty_without_a_selection() {
        let mut ctx = SceneEditContext::new();
        ctx.set_selection(Entity::NULL);
        ctx.sync_native_gizmo();
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        let mut v = Vec::new();
        build_native_gizmo(&ctx, &cam, 1280, 720, &mut v);
        assert!(v.is_empty(), "no selection → no gizmo geometry");

        // With a transformable selection at the origin and translate mode, the gizmo emits the
        // three axis lines + boxes + the plane quads.
        let target = ctx.scene.create_entity("Target");
        ctx.scene.relink_hierarchy();
        ctx.scene.update_world_transforms();
        ctx.set_selection(target);
        ctx.gizmo_op = GizmoOp::Translate;
        ctx.sync_native_gizmo();
        let mut g = Vec::new();
        build_native_gizmo(&ctx, &cam, 1280, 720, &mut g);
        assert!(
            !g.is_empty(),
            "a transformable selection emits gizmo geometry"
        );
        // The rotation rings are gone in translate mode; switching to rotate re-emits geometry.
        ctx.gizmo_op = GizmoOp::Rotate;
        ctx.sync_native_gizmo();
        let mut r = Vec::new();
        build_native_gizmo(&ctx, &cam, 1280, 720, &mut r);
        assert!(!r.is_empty(), "rotate mode emits the rings");
    }

    #[test]
    fn skeleton_overlay_off_emits_nothing() {
        let mut ctx = SceneEditContext::new();
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        let mut v = Vec::new();
        // Default: skeleton overlay off.
        assert!(!ctx.skeleton_overlay.show);
        build_skeleton_overlay(&mut ctx, &cam, 1280, 720, &mut v);
        assert!(v.is_empty(), "skeleton overlay off → no geometry");
        // On, but the selection has no rig anywhere in its forest → still nothing.
        ctx.skeleton_overlay.show = true;
        let unrigged = ctx.scene.create_entity("Unrigged");
        ctx.set_selection(unrigged);
        let mut v2 = Vec::new();
        build_skeleton_overlay(&mut ctx, &cam, 1280, 720, &mut v2);
        assert!(
            v2.is_empty(),
            "skeleton overlay draws nothing for a selection with no rig in its subtree"
        );
        // The play state never blocks the skeleton overlay (it draws in every state).
        assert_eq!(ctx.play_state, PlayState::Edit);
    }

    /// The rig the overlay must draw rides a child mesh entity, while the selection (and the
    /// preview root) is the model's container — the shape every standard rig spawns as. The
    /// overlay resolves the rig within the forest, so it emits geometry rather than blanking.
    #[test]
    fn skeleton_overlay_draws_for_a_rig_on_a_child_of_the_selected_container() {
        let mut ctx = SceneEditContext::new();
        let cam = test_camera(Vec3::new(3.0, 2.5, 6.0));
        ctx.skeleton_overlay.show = true;

        let container = ctx.scene.create_entity("Rig");
        let mesh_entity = ctx.scene.create_entity("RigMesh");
        let bone = ctx.scene.create_entity("Bone");
        ctx.scene
            .set_parent(mesh_entity, Some(container), false)
            .unwrap();
        ctx.scene
            .set_parent(bone, Some(mesh_entity), false)
            .unwrap();
        // Add the rig after the last reparent: `set_parent` relinks the hierarchy, which would
        // rebuild `bone_handles` from `skin.bones`; seeding the cache directly keeps it.
        ctx.scene
            .add_component(
                mesh_entity,
                SkinnedMesh {
                    mesh: saffron_core::Uuid(1),
                    bone_handles: vec![bone],
                    ..SkinnedMesh::default()
                },
            )
            .unwrap();
        ctx.scene.update_world_transforms();
        ctx.set_selection(container);

        let mut v = Vec::new();
        build_skeleton_overlay(&mut ctx, &cam, 1280, 720, &mut v);
        assert!(
            !v.is_empty(),
            "the overlay draws the rig that rides a child of the selected container"
        );
    }
}
