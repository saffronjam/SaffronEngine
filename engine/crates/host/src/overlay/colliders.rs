//! The collider overlay: authored [`Collider`] wireframes, drawn in Edit and Play alike.

use glam::{Mat4, Vec3, Vec4};

use saffron_assets::{AssetServer, GpuUploader};
use saffron_rendering::OverlayVertex;
use saffron_scene::{
    CameraView, Collider, Entity, Mesh, Shape, SkinnedMesh, Transform, camera_projection,
};
use saffron_sceneedit::SceneEditContext;

use super::primitives::{
    add_clipped_overlay_line, add_world_arc, add_world_oriented_box, add_world_ring,
};

/// The physics collider overlay (`set-debug-overlays {colliders}`): a world-space wireframe
/// per [`Collider`] — oriented box / sphere / capsule, or the cook-source mesh AABB for
/// hull/mesh.
///
/// Drawn SCALE-FREE to match the Jolt body: position + rotation only, with the collider offset
/// in the rotated body-local frame (never `world_matrix`, which carries entity scale). Reads
/// the authored [`Collider`], present in Edit AND Play, so it sits outside `edit_chrome` and
/// carries its own preview guard.
pub(super) fn build_collider_overlays(
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
