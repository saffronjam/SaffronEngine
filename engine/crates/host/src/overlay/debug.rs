//! The debug-overlay builders: wind, micro-density heatmap, rejected candidates, vegetation cells,
//! the generic debug set, plant collision proxies, and navigation contributions.

use glam::{Mat4, Vec3, Vec4};

use saffron_assets::{AssetServer, GpuUploader};
use saffron_geometry::world_aabb_from_corners;
use saffron_rendering::OverlayVertex;
use saffron_scene::{
    CameraView, Entity, FogShape, FogVolume, Mesh, PointLight, SkinnedMesh, SpotLight, Transform,
    camera_projection,
};
use saffron_sceneedit::SceneEditContext;

use super::primitives::{
    add_clipped_overlay_line, add_world_aabb, add_world_oriented_box, add_world_ring,
};

/// The wind overlay: one speed-colored arrow per sampled ground-grid point — the
/// shaft along the sampled velocity plus a short vertical tip tick. Depth-tested,
/// Edit-only.
pub(super) fn build_wind_overlay(
    wind: &[(Vec3, Vec3)],
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if wind.is_empty() || width == 0 || height == 0 {
        return;
    }
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    const CALM: Vec4 = Vec4::new(0.35, 0.6, 0.95, 0.85);
    const STORM: Vec4 = Vec4::new(0.95, 0.3, 0.2, 0.9);
    for (base, velocity) in wind {
        let speed = velocity.length();
        if speed < 0.05 {
            continue;
        }
        let color = CALM.lerp(STORM, (speed / 15.0).clamp(0.0, 1.0));
        let shaft = *velocity * (0.15_f32).min(3.0 / speed);
        let start = *base + Vec3::new(0.0, 0.15, 0.0);
        let tip = start + shaft;
        add_clipped_overlay_line(
            vertices,
            &view_projection,
            start,
            tip,
            2.0,
            color,
            width,
            height,
        );
        add_clipped_overlay_line(
            vertices,
            &view_projection,
            tip,
            tip + Vec3::new(0.0, 0.12, 0.0),
            2.0,
            color,
            width,
            height,
        );
    }
}

/// The heatmap overlay: one thin surface-hugging tile per occupied micro-density texel, ramped
/// green to red by density. Depth-tested, Edit-only.
pub(super) fn build_heatmap_overlay(
    heatmap: &[(Vec3, f32)],
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if heatmap.is_empty() || width == 0 || height == 0 {
        return;
    }
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    const LOW: Vec4 = Vec4::new(0.2, 0.8, 0.35, 0.75);
    const HIGH: Vec4 = Vec4::new(0.95, 0.25, 0.2, 0.85);
    const HALF: f32 = 0.6;
    for (position, density) in heatmap {
        let color = LOW.lerp(HIGH, density.clamp(0.0, 1.0));
        add_world_aabb(
            vertices,
            &view_projection,
            *position - Vec3::new(HALF, 0.02, HALF),
            *position + Vec3::new(HALF, 0.06, HALF),
            color,
            width,
            height,
        );
    }
}

/// The rejection overlay: one small reason-colored marker cube per rejected
/// candidate (the host caches the rows from the resident cells' rejection facets).
/// Depth-tested, Edit-only.
pub(super) fn build_rejection_overlay(
    rejections: &[(Vec3, u8)],
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if rejections.is_empty() || width == 0 || height == 0 {
        return;
    }
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    const REASON_COLORS: [Vec4; 7] = [
        Vec4::new(0.55, 0.55, 0.6, 0.85),  // surface-miss: grey
        Vec4::new(0.95, 0.85, 0.3, 0.85),  // threshold: yellow
        Vec4::new(0.95, 0.6, 0.25, 0.85),  // weighted-elimination: orange
        Vec4::new(0.9, 0.35, 0.75, 0.85),  // priority-exclusion: magenta
        Vec4::new(0.95, 0.3, 0.3, 0.85),   // competition: red
        Vec4::new(0.4, 0.65, 0.95, 0.85),  // foreign-owner: blue
        Vec4::new(0.65, 0.45, 0.95, 0.85), // no-species: violet
    ];
    const HALF: f32 = 0.12;
    for (position, reason) in rejections {
        let color = REASON_COLORS[usize::from(*reason) % REASON_COLORS.len()];
        add_world_aabb(
            vertices,
            &view_projection,
            *position - Vec3::splat(HALF),
            *position + Vec3::splat(HALF),
            color,
            width,
            height,
        );
    }
}

/// The vegetation debug overlays (`set-debug-overlays`): resident runtime cells as
/// wireframe boxes, and per-plant conservative world bounds colored by lifecycle.
/// Depth-tested, Edit-only; the plant boxes cap so a dense world stays interactive.
pub(super) fn build_vegetation_overlays(
    editor: &mut SceneEditContext,
    vegetation: Option<&saffron_runtime::VegetationWorld>,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    let opts = editor.debug_overlays;
    if width == 0 || height == 0 || (!opts.vegetation_cells && !opts.vegetation_bounds) {
        return;
    }
    let Some(world) = vegetation else {
        return;
    };
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    const CELL_COLOR: Vec4 = Vec4::new(0.35, 0.75, 0.95, 0.8);
    const MATURE_COLOR: Vec4 = Vec4::new(0.4, 0.9, 0.45, 0.85);
    const YOUNG_COLOR: Vec4 = Vec4::new(0.85, 0.95, 0.4, 0.85);
    const DECLINING_COLOR: Vec4 = Vec4::new(0.95, 0.6, 0.3, 0.85);
    const DORMANT_COLOR: Vec4 = Vec4::new(0.6, 0.6, 0.65, 0.7);
    const PLANT_BOX_CAP: usize = 4096;
    let to_meters = |ticks: [i128; 3]| {
        Vec3::new(
            ticks[0] as f32 / 4096.0,
            ticks[1] as f32 / 4096.0,
            ticks[2] as f32 / 4096.0,
        )
    };
    let mut plant_boxes = 0_usize;
    for (cell, generation) in world.resident_cells() {
        if opts.vegetation_cells {
            let bounds = cell.bounds();
            add_world_aabb(
                vertices,
                &view_projection,
                to_meters(bounds.min_ticks()),
                to_meters(bounds.max_ticks_exclusive()),
                CELL_COLOR,
                width,
                height,
            );
        }
        if opts.vegetation_bounds && plant_boxes < PLANT_BOX_CAP {
            let points = generation.macro_points();
            for index in 0..points.ids.len() {
                if plant_boxes >= PLANT_BOX_CAP {
                    break;
                }
                let color = match points.lifecycles[index] {
                    saffron_runtime::PlantLifecycle::Seed
                    | saffron_runtime::PlantLifecycle::Removed => continue,
                    saffron_runtime::PlantLifecycle::Mature => MATURE_COLOR,
                    saffron_runtime::PlantLifecycle::Sprout
                    | saffron_runtime::PlantLifecycle::Juvenile => YOUNG_COLOR,
                    saffron_runtime::PlantLifecycle::Senescent
                    | saffron_runtime::PlantLifecycle::Dead => DECLINING_COLOR,
                    saffron_runtime::PlantLifecycle::Stump => DORMANT_COLOR,
                };
                let bounds = points.bounds[index];
                add_world_aabb(
                    vertices,
                    &view_projection,
                    to_meters(bounds.min_ticks()),
                    to_meters(bounds.max_ticks_exclusive()),
                    color,
                    width,
                    height,
                );
                plant_boxes += 1;
            }
        }
    }
}

/// The viewport debug overlays (`set-debug-overlays`): per-entity bounds (the exact box
/// `pick_entity` tests, static + skinned joint-union), the whole-scene AABB the shadow fit
/// uses, and point/spot light volumes. Depth-tested, Edit-only.
pub(super) fn build_debug_overlays(
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

/// The plant-proxy overlay: a cooked family's derived collision capsules and navigation footprints,
/// drawn over the asset preview.
///
/// This is the counterpart to [`build_collider_overlays`], which guards itself off in the preview
/// because a preview scene has no physics bodies to draw. A plant family's proxies are DERIVED — a
/// result of what grew, like its dimensions — so there is nothing in the scene to attach them to and
/// nothing else that would ever show them. They are read straight off the authored family.
///
/// Same two toggles as the scene, applied to whatever surface is in front of you: `colliders` for
/// the capsules, `vegetationNavigation` for the footprints.
pub(super) fn build_plant_proxy_overlays(
    editor: &mut SceneEditContext,
    assets: &mut AssetServer,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if !editor.previewing() || width == 0 || height == 0 {
        return;
    }
    let show_collision = editor.debug_overlays.colliders;
    let show_navigation = editor.debug_overlays.vegetation_navigation;
    if !show_collision && !show_navigation {
        return;
    }
    let subject = editor.preview_asset;
    let Ok(family) = saffron_assets::load_plant_family_asset(assets, subject) else {
        return;
    };
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    // Cyan matches the scene's solid colliders, so the two surfaces read the same way; breakable
    // proxies take the warmer tone because "this one comes off" is the property worth spotting.
    const PROXY_COLOR: Vec4 = Vec4::new(0.20, 0.95, 0.85, 0.9);
    const BREAKABLE_COLOR: Vec4 = Vec4::new(1.0, 0.72, 0.25, 0.9);
    const NAVIGATION_COLOR: Vec4 = Vec4::new(0.45, 0.65, 1.0, 0.85);
    let metres = |value: saffron_spatial::DecisionScalar| value.bits() as f32 / 65_536.0;

    if show_collision {
        for proxy in &family.collision_proxies {
            let color = if proxy.breakable {
                BREAKABLE_COLOR
            } else {
                PROXY_COLOR
            };
            let center = Vec3::new(
                metres(proxy.center[0]),
                metres(proxy.center[1]),
                metres(proxy.center[2]),
            );
            let dimensions = Vec3::new(
                metres(proxy.dimensions[0]),
                metres(proxy.dimensions[1]),
                metres(proxy.dimensions[2]),
            );
            match proxy.shape {
                saffron_runtime::PlantCollisionShape::Box
                | saffron_runtime::PlantCollisionShape::ConvexHull => {
                    // A hull has no wireframe of its own here; its bounding box is the honest
                    // stand-in and is what the batched collision residency sizes against anyway.
                    add_world_oriented_box(
                        vertices,
                        &view_projection,
                        &Mat4::from_translation(center),
                        dimensions,
                        color,
                        width,
                        height,
                    );
                }
                saffron_runtime::PlantCollisionShape::Sphere => {
                    for (right, up) in [(Vec3::X, Vec3::Y), (Vec3::Y, Vec3::Z), (Vec3::Z, Vec3::X)]
                    {
                        add_world_ring(
                            vertices,
                            &view_projection,
                            center,
                            right,
                            up,
                            dimensions.x,
                            color,
                            width,
                            height,
                        );
                    }
                }
                saffron_runtime::PlantCollisionShape::Capsule => {
                    // Y-up, matching the cooker: radius in x, half-height in y. A plant's capsules
                    // stand along the axis they were fitted to.
                    let radius = dimensions.x;
                    let half_height = dimensions.y;
                    let top = center + Vec3::Y * half_height;
                    let bottom = center - Vec3::Y * half_height;
                    for ring_center in [top, bottom] {
                        add_world_ring(
                            vertices,
                            &view_projection,
                            ring_center,
                            Vec3::X,
                            Vec3::Z,
                            radius,
                            color,
                            width,
                            height,
                        );
                    }
                    for offset in [Vec3::X * radius, Vec3::Z * radius] {
                        add_clipped_overlay_line(
                            vertices,
                            &view_projection,
                            top + offset,
                            bottom + offset,
                            1.5,
                            color,
                            width,
                            height,
                        );
                        add_clipped_overlay_line(
                            vertices,
                            &view_projection,
                            top - offset,
                            bottom - offset,
                            1.5,
                            color,
                            width,
                            height,
                        );
                    }
                }
            }
        }
    }

    if show_navigation {
        for proxy in &family.navigation_proxies {
            if proxy.footprint.len() < 2 {
                continue;
            }
            let height_m = metres(proxy.height);
            let corner = |point: &[saffron_spatial::DecisionScalar; 2], y: f32| {
                Vec3::new(metres(point[0]), y, metres(point[1]))
            };
            // The footprint is a closed ring at the ground and again at the obstacle height, with
            // uprights between — an author needs the HEIGHT as much as the outline, because that is
            // what decides whether a character walks through or around.
            for (index, point) in proxy.footprint.iter().enumerate() {
                let next = &proxy.footprint[(index + 1) % proxy.footprint.len()];
                for y in [0.0, height_m] {
                    add_clipped_overlay_line(
                        vertices,
                        &view_projection,
                        corner(point, y),
                        corner(next, y),
                        1.5,
                        NAVIGATION_COLOR,
                        width,
                        height,
                    );
                }
                add_clipped_overlay_line(
                    vertices,
                    &view_projection,
                    corner(point, 0.0),
                    corner(point, height_m),
                    1.5,
                    NAVIGATION_COLOR,
                    width,
                    height,
                );
            }
        }
    }
}

/// Draws vegetation's published navigation contributions: each footprint as a closed loop at its
/// obstacle height, colored by declaration, plus the regions awaiting a rebuild as boxes. Obstacles
/// read red, dynamic obstacles (a plant currently moving) amber, cost fields blue, and a dirty
/// region white — so a glance answers whether the seam matches what is on screen.
pub(super) fn build_navigation_overlay(
    editor: &SceneEditContext,
    navigation: Option<&saffron_runtime::VegetationNavigationSeam>,
    cam: &CameraView,
    width: u32,
    height: u32,
    vertices: &mut Vec<OverlayVertex>,
) {
    if width == 0 || height == 0 || !editor.debug_overlays.vegetation_navigation {
        return;
    }
    let Some(seam) = navigation else {
        return;
    };
    const OBSTACLE_COLOR: Vec4 = Vec4::new(0.95, 0.35, 0.3, 0.85);
    const DYNAMIC_COLOR: Vec4 = Vec4::new(0.98, 0.72, 0.25, 0.9);
    const COST_COLOR: Vec4 = Vec4::new(0.4, 0.65, 0.95, 0.75);
    const DIRTY_COLOR: Vec4 = Vec4::new(0.95, 0.95, 0.95, 0.6);
    const FOOTPRINT_CAP: usize = 2048;
    let aspect = width as f32 / height as f32;
    let view_projection = camera_projection(cam, aspect) * cam.view;
    let to_meters = |ticks: [i128; 3]| {
        Vec3::new(
            ticks[0] as f32 / 4096.0,
            ticks[1] as f32 / 4096.0,
            ticks[2] as f32 / 4096.0,
        )
    };

    let mut drawn = 0_usize;
    for (_, contributions) in seam.cells() {
        for contribution in contributions {
            if drawn >= FOOTPRINT_CAP {
                break;
            }
            let color = match contribution.kind {
                saffron_runtime::NavigationContributionKind::Cost => COST_COLOR,
                saffron_runtime::NavigationContributionKind::StaticObstacle => OBSTACLE_COLOR,
                saffron_runtime::NavigationContributionKind::DynamicObstacle => DYNAMIC_COLOR,
            };
            let base_y = to_meters(contribution.bounds.min_ticks()).y;
            let top_y = base_y + contribution.height_m as f32;
            for (index, point) in contribution.footprint.iter().enumerate() {
                let next =
                    contribution.footprint[(index + 1) % contribution.footprint.len().max(1)];
                let from = Vec3::new(point[0] as f32, base_y, point[1] as f32);
                let to = Vec3::new(next[0] as f32, base_y, next[1] as f32);
                add_clipped_overlay_line(
                    vertices,
                    &view_projection,
                    from,
                    to,
                    1.5,
                    color,
                    width,
                    height,
                );
                // One upright per vertex carries the obstacle height without drawing a full prism.
                add_clipped_overlay_line(
                    vertices,
                    &view_projection,
                    from,
                    Vec3::new(from.x, top_y, from.z),
                    1.5,
                    color,
                    width,
                    height,
                );
            }
            drawn += 1;
        }
    }
    for region in seam.dirty_regions() {
        add_world_aabb(
            vertices,
            &view_projection,
            to_meters(region.min_ticks()),
            to_meters(region.max_ticks_exclusive()),
            DIRTY_COLOR,
            width,
            height,
        );
    }
}
