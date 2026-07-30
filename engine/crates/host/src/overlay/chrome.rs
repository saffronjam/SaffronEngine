//! The Edit-only viewport chrome: entity billboards, camera frustums, the active gizmo, and the
//! skeleton overlay.

use glam::{Vec3, Vec4};

use saffron_rendering::OverlayVertex;
use saffron_scene::{
    Bone, Camera, CameraView, Entity, FogVolume, IdComponent, Mesh, PointLight, Relationship,
    Scene, SkinnedMesh, SpotLight, Transform, camera_projection,
};
use saffron_sceneedit::{
    NativeGizmoHandle, NativeGizmoMode, SceneEditContext, axis_color, camera_position, gizmo_axes,
    gizmo_plane_corners, ring_basis, viewport_project,
};

use super::primitives::{
    BOX_EDGES, add_box, add_bulb_icon, add_camera_icon, add_circle_fill, add_clipped_overlay_line,
    add_fog_icon, add_line_flat, add_quad,
};

/// What billboard glyph (if any) a meshless entity is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BillboardKind {
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

/// The billboard glyph an entity is drawn with: none for a mesh, otherwise by its light /
/// camera component.
pub(super) fn billboard_kind(scene: &Scene, entity: Entity) -> BillboardKind {
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
pub(super) fn build_native_gizmo(
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
pub(super) fn build_skeleton_overlay(
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
pub(super) fn build_scene_edit_billboards(
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

/// The camera-frustum overlays: a clipped 12-edge wireframe per `show_frustum` camera.
/// Depth-tested, Edit-only.
pub(super) fn build_scene_edit_camera_frustums(
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

#[cfg(test)]
mod tests {
    use super::super::testkit::test_camera;
    use super::*;
    use saffron_sceneedit::{GizmoOp, PlayState};

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
        assert!(!ctx.skeleton_overlay.show);
        build_skeleton_overlay(&mut ctx, &cam, 1280, 720, &mut v);
        assert!(v.is_empty(), "skeleton overlay off → no geometry");
        ctx.skeleton_overlay.show = true;
        let unrigged = ctx.scene.create_entity("Unrigged");
        ctx.set_selection(unrigged);
        let mut v2 = Vec::new();
        build_skeleton_overlay(&mut ctx, &cam, 1280, 720, &mut v2);
        assert!(
            v2.is_empty(),
            "skeleton overlay draws nothing for a selection with no rig in its subtree"
        );
        assert_eq!(ctx.play_state, PlayState::Edit, "it draws in every state");
    }

    /// The rig the overlay must draw rides a child mesh entity while the selection is the model's
    /// container — the shape every standard rig spawns as.
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
