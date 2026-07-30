use saffron_assets::{AssetServer, BUILTIN_SPHERE_MESH_ID, model_render_aabb};
use saffron_core::Uuid;
use saffron_protocol::{PlayStateResult, Uuid as WireUuid, Vec3, Vec4};
use saffron_rendering::ViewId;
use saffron_scene::{
    DirectionalLight, Entity, IdComponent, MaterialSet, MaterialSlot, Mesh, Scene, SkyMode,
    TextureRole, Transform,
};
use saffron_sceneedit::{OrbitState, SceneEditCamera};
use serde_json::json;

use super::*;
use crate::registry::EngineContext;

/// Make the asset-preview view the active one on a *fresh* enter: stash the authored camera /
/// selection / overlay / exposure, size the preview view to the viewport, and switch the renderer's
/// active view. A swap (already previewing) keeps the authored stash and the active view.
pub(crate) fn activate_asset_preview_view(ctx: &mut EngineContext<'_>) {
    if ctx.scene_edit.previewing() {
        return;
    }
    ctx.scene_edit.saved_camera = ctx.scene_edit.camera;
    ctx.scene_edit.saved_selection = ctx.scene_edit.selected;
    ctx.scene_edit.saved_overlay = ctx.scene_edit.skeleton_overlay;
    ctx.scene_edit.saved_exposure = ctx.renderer.exposure_ev();
    ctx.scene_edit.preview_active_view = true;
    let (w, h) = (
        ctx.renderer.viewport_width(),
        ctx.renderer.viewport_height(),
    );
    let _ = ctx
        .renderer
        .set_view_desired_size(ViewId::AssetPreview, w, h);
    ctx.renderer.set_active_view(ViewId::AssetPreview);
}

/// Installs an already-furnished preview `scene` as the active preview subject: switch the view,
/// store the scene + framed camera + floor + rig state, select the root, and bump the versions.
/// Returns `(root uuid, framing)` for the wire result the caller assembles (with its own bones).
pub(crate) fn install_preview_scene(
    ctx: &mut EngineContext<'_>,
    scene: Scene,
    root: Entity,
    preview_asset: Uuid,
    furnish: PreviewFurnish,
    bone_by_node: Vec<Uuid>,
    overlay_show: bool,
) -> (u64, PreviewFraming) {
    let root_uuid = scene
        .component::<IdComponent>(root)
        .map(|c| c.id.value())
        .unwrap_or(0);
    activate_asset_preview_view(ctx);
    ctx.scene_edit.preview_scene = Some(scene);
    ctx.scene_edit.preview_asset = preview_asset;
    ctx.scene_edit.preview_root_entity = root;
    ctx.scene_edit.preview_bone_by_node = bone_by_node;
    ctx.scene_edit.preview_floor_entity = furnish.floor;
    ctx.scene_edit.skeleton_overlay.show = overlay_show;
    ctx.scene_edit.skeleton_overlay.highlight_joint = -1;
    ctx.scene_edit.camera = furnish.camera;
    ctx.scene_edit.set_selection(root);
    ctx.scene_edit.scene_version += 1;
    ctx.scene_edit.animation_version += 1;
    (root_uuid, furnish.framing)
}

/// Builds a furnished, renderable preview scene for a sphere subject (a material by id, or a texture
/// map through an ephemeral single-slot material): the dense displacement sphere carrying the
/// subject, plus floor / key light / procedural sky / framed camera. Pure of the edit context —
/// the one builder shared by the interactive previewer and the background thumbnail render.
pub(crate) fn build_preview_scene(
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    catalog: Option<std::sync::Arc<saffron_scene::AssetCatalog>>,
    subject: PreviewSubject,
    ephemeral_material_id: Uuid,
    spec: FurnishSpec,
) -> PreviewBuild {
    let mut scene = Scene::new();
    scene.catalog = catalog;
    let root = match subject {
        PreviewSubject::Material(mid) => {
            let name = assets
                .catalog()
                .find(mid)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Material".to_owned());
            let root = scene.create_entity(&name);
            attach_preview_sphere(&mut scene, root, mid);
            root
        }
        PreviewSubject::TextureRole { tid, role } => {
            let material = preview_material_for_texture(role, tid);
            assets.seed_preview_material(ephemeral_material_id, material);
            let name = assets
                .catalog()
                .find(tid)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Texture".to_owned());
            let root = scene.create_entity(&name);
            attach_preview_sphere(&mut scene, root, ephemeral_material_id);
            root
        }
        PreviewSubject::Mesh(mesh_id) => {
            let name = assets
                .catalog()
                .find(mesh_id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Mesh".to_owned());
            let root = scene.create_entity(&name);
            let _ = scene.add_component(root, Mesh { mesh: mesh_id });
            // The default material slot (`material: Uuid(0)`) resolves to the built-in default material.
            let _ = scene.add_component(
                root,
                MaterialSet {
                    slots: vec![MaterialSlot::default()],
                },
            );
            root
        }
        PreviewSubject::Model(model_id) => {
            let name = assets
                .catalog()
                .find(model_id)
                .map(|e| e.name.clone())
                .unwrap_or_else(|| "Model".to_owned());
            match assets.instantiate_model(&mut scene, model_id, name) {
                Ok(root) => root,
                Err(err) => {
                    tracing::warn!("preview: model {model_id:?} failed to instantiate: {err}");
                    Entity::NULL
                }
            }
        }
        PreviewSubject::Plant(family) => {
            match plant_preview_root(assets, gpu, &mut scene, family) {
                Ok((root, _)) => root,
                Err(err) => {
                    tracing::warn!("preview: plant {family:?} failed to build: {err}");
                    Entity::NULL
                }
            }
        }
        PreviewSubject::Hdri(_) => {
            // A mirror ball (metallic 1 / near-zero roughness) reflecting the HDRI env — the HDRI is
            // set as the scene's sky in `furnish_preview_scene` (`PreviewEnv::Hdri`), which also
            // backs the tile and drives the IBL prefilter the ball samples.
            let root = scene.create_entity("HDRI");
            let _ = scene.add_component(
                root,
                Mesh {
                    mesh: BUILTIN_SPHERE_MESH_ID,
                },
            );
            let _ = scene.add_component(
                root,
                MaterialSet {
                    slots: vec![MaterialSlot {
                        material: Uuid(0),
                        overrides: json!({ "metallic": 1.0, "roughness": 0.04, "baseColor": [1.0, 1.0, 1.0, 1.0] }),
                    }],
                },
            );
            root
        }
    };
    let furnish = furnish_preview_scene(&mut scene, assets, gpu, root, spec);
    PreviewBuild {
        scene,
        root,
        furnish,
    }
}

/// Attach the ordinary low-poly builtin sphere + a single slot referencing `material_id`. A
/// displacement-enabled `.smat` gets its true bulged silhouette from the real tessellating path — the
/// same path a scene mesh uses — so no dense stand-in is needed and preview matches scene.
pub(crate) fn attach_preview_sphere(scene: &mut Scene, root: Entity, material_id: Uuid) {
    let _ = scene.add_component(
        root,
        Mesh {
            mesh: BUILTIN_SPHERE_MESH_ID,
        },
    );
    let _ = scene.add_component(
        root,
        MaterialSet {
            slots: vec![MaterialSlot {
                material: material_id,
                ..MaterialSlot::default()
            }],
        },
    );
}

/// The environment and framing tightness a thumbnail subject furnishes with: a material or texture
/// map frames tight over the procedural sky with displacement headroom; an HDRI ball frames tighter
/// still and backs itself with its own equirect; a model or mesh frames looser so a 3/4 view of its
/// bounding box does not clip.
pub(crate) fn thumbnail_subject_furnishing(subject: &PreviewSubject) -> (PreviewEnv, f32) {
    match subject {
        PreviewSubject::Material(_) | PreviewSubject::TextureRole { .. } => {
            (PreviewEnv::Procedural, THUMBNAIL_MATERIAL_FRAME_MARGIN)
        }
        PreviewSubject::Mesh(_) | PreviewSubject::Model(_) | PreviewSubject::Plant(_) => {
            (PreviewEnv::Procedural, THUMBNAIL_MODEL_FRAME_MARGIN)
        }
        PreviewSubject::Hdri(tid) => (PreviewEnv::Hdri(*tid), THUMBNAIL_CHROME_BALL_FRAME_MARGIN),
    }
}

pub fn build_preview_scene_for_thumbnail(
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    subject: PreviewSubject,
    ephemeral_material_id: Uuid,
) -> (Scene, Entity, SceneEditCamera) {
    let (env, frame_margin) = thumbnail_subject_furnishing(&subject);
    let spec = FurnishSpec {
        base_cam: SceneEditCamera::default(),
        env,
        show_floor: false,
        frame_margin,
    };
    let build = build_preview_scene(assets, gpu, None, subject, ephemeral_material_id, spec);
    (build.scene, build.root, build.furnish.camera)
}

/// Commits a pre-built (unfurnished) container-less preview subject (a built-in primitive or the
/// HDRI ball rig): furnishes the scene through the shared builder, then installs it. The rig-less
/// commit tail of [`enter_asset_preview`].
pub(crate) fn commit_preview_subject(
    ctx: &mut EngineContext<'_>,
    mut preview: Scene,
    root: Entity,
    preview_asset: Uuid,
    env: PreviewEnv,
) -> AssetPreviewResultWrap {
    let spec = FurnishSpec {
        base_cam: ctx.scene_edit.camera,
        env,
        show_floor: ctx.scene_edit.preview_show_floor,
        frame_margin: INTERACTIVE_FRAME_MARGIN,
    };
    let assets = &mut *ctx.assets;
    let mut furnish = None;
    ctx.renderer.with_gpu_uploader(&mut |gpu| {
        furnish = Some(furnish_preview_scene(&mut preview, assets, gpu, root, spec));
    });
    let furnish = furnish.expect("furnish ran");
    let (root_uuid, framing) = install_preview_scene(
        ctx,
        preview,
        root,
        preview_asset,
        furnish,
        Vec::new(),
        false,
    );
    AssetPreviewResultWrap(saffron_protocol::AssetPreviewResult {
        root_entity: WireUuid(root_uuid),
        bones: Vec::new(),
        target: vec3(framing.target),
        distance: framing.distance,
        plant_combinations: Vec::new(),
    })
}

/// A newtype around [`AssetPreviewResult`](saffron_protocol::AssetPreviewResult) so the
/// `enter-asset-preview` handler can use a free fn (the closure form does not infer the
/// generic). It serializes transparently to the wire DTO.
#[derive(serde::Serialize)]
#[serde(transparent)]
pub struct AssetPreviewResultWrap(pub(crate) saffron_protocol::AssetPreviewResult);

/// The preview framing pivot + orbit distance.
#[derive(Clone, Copy)]
pub(crate) struct PreviewFraming {
    pub(crate) target: saffron_geometry::glam::Vec3,
    pub(crate) distance: f32,
}

/// What [`furnish_preview_scene`] produced: the framed camera, the floor entity (or `NULL`), and
/// the orbit framing — applied into the edit context by [`install_preview_scene`], or read directly
/// by a background thumbnail render.
pub(crate) struct PreviewFurnish {
    pub(crate) framing: PreviewFraming,
    pub(crate) camera: SceneEditCamera,
    pub(crate) floor: Entity,
}

/// How to furnish a preview scene: the base camera to frame from, the environment, whether to lay a
/// floor slab, and how tightly to frame the subject. One bundle shared by the interactive previewer
/// and the thumbnail render.
#[derive(Clone, Copy)]
pub(crate) struct FurnishSpec {
    pub(crate) base_cam: SceneEditCamera,
    pub(crate) env: PreviewEnv,
    pub(crate) show_floor: bool,
    /// Camera framing margin: how far past a snug fit the camera sits (`1.0` ≈ the bounding sphere
    /// touches the frame edge). The interactive previewer leaves generous room; a thumbnail pulls in
    /// tight so the subject fills the tile.
    pub(crate) frame_margin: f32,
}

/// The interactive previewer's framing margin — generous room around the subject for orbiting.
pub(crate) const INTERACTIVE_FRAME_MARGIN: f32 = 1.3;

/// A thumbnail's framing margin for a smooth chrome-ball subject (an HDRI reflection sphere): tight —
/// the AABB half-diagonal radius already over-frames a sphere by √3 — but with a little breathing room
/// so the ball does not touch the tile edges.
pub(crate) const THUMBNAIL_CHROME_BALL_FRAME_MARGIN: f32 = 0.72;

/// A thumbnail's framing margin for a displacement-sphere subject (a material or a texture role):
/// looser than the smooth ball because displacement pushes the silhouette outward at render time,
/// *after* the frame bounds are computed from the base mesh — so the base-sphere framing needs
/// headroom for the bulge, or a displaced material spills past the tile edges.
pub(crate) const THUMBNAIL_MATERIAL_FRAME_MARGIN: f32 = 0.85;

/// A thumbnail's framing margin for a model / mesh subject: still tighter than interactive, but with
/// enough room that a 3/4 view of an arbitrary bounding box does not clip its far corners.
pub(crate) const THUMBNAIL_MODEL_FRAME_MARGIN: f32 = 1.05;

/// A preview subject built into a throwaway scene and rendered through the main forward+ graph.
/// Every asset kind maps to one of these, which is the single thumbnail render path.
pub enum PreviewSubject {
    Material(Uuid),
    TextureRole { tid: Uuid, role: TextureRole },
    Mesh(Uuid),
    Model(Uuid),
    Hdri(Uuid),
    Plant(Uuid),
}

/// A furnished, renderable preview scene built by [`build_preview_scene`].
pub(crate) struct PreviewBuild {
    pub(crate) scene: Scene,
    pub(crate) root: Entity,
    pub(crate) furnish: PreviewFurnish,
}

/// The previewed model's world-space bounding sphere from its mesh's rest-pose AABB.
pub(crate) struct PreviewBounds {
    pub(crate) center: saffron_geometry::glam::Vec3,
    pub(crate) radius: f32,
    pub(crate) min_y: f32,
}

/// The lighting/backdrop a preview subject is furnished with.
#[derive(Clone, Copy)]
pub(crate) enum PreviewEnv {
    /// A studio key light over the procedural sky (a model / a lone texture sphere).
    Procedural,
    /// An imported HDRI as both the visible backdrop and the IBL source (the environment rig).
    Hdri(Uuid),
}

/// Make the preview look like a preview: floor + key light + procedural sky, or the HDRI
/// environment; and frame the fly-cam. Pure of the edit context — operates on the passed `scene`
/// and a base camera, so both the interactive previewer and a background thumbnail render share
/// this one furnishing. Returns the framed camera, the floor entity, and the orbit framing.
pub(crate) fn furnish_preview_scene(
    scene: &mut Scene,
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    root: Entity,
    spec: FurnishSpec,
) -> PreviewFurnish {
    use saffron_geometry::glam::Vec3 as GVec3;

    let bounds = compute_preview_bounds(scene, assets, gpu, root);
    // The HDRI environment is its own backdrop — no floor slab under the rig.
    let floor = if spec.show_floor && matches!(spec.env, PreviewEnv::Procedural) {
        spawn_preview_floor(scene, assets, gpu, &bounds)
    } else {
        Entity::NULL
    };

    match spec.env {
        PreviewEnv::Procedural => {
            let light = scene.create_entity("PreviewLight");
            let _ = scene.add_component(
                light,
                DirectionalLight {
                    direction: GVec3::new(-0.4, -1.0, -0.5).normalize(),
                    color: GVec3::ONE,
                    intensity: 3.0,
                    ambient: 0.25,
                    ..Default::default()
                },
            );
            scene.environment.sky_mode = SkyMode::Procedural;
            scene.environment.use_sky_for_ambient = true;
            scene.environment.ambient_intensity = 0.3;
        }
        PreviewEnv::Hdri(id) => {
            // The HDRI both lights (IBL prefilter of the equirect) and backs the scene — no
            // directional key light, so the balls read the environment's own illumination.
            scene.environment.sky_mode = SkyMode::Texture;
            scene.environment.sky_texture = id;
            scene.environment.sky_intensity = 1.0;
            scene.environment.use_sky_for_ambient = true;
            scene.environment.ambient_intensity = 1.0;
        }
    }

    let camera = frame_preview_camera(spec.base_cam, &bounds, spec.frame_margin);
    let fovy = camera.fov.to_radians();
    PreviewFurnish {
        framing: PreviewFraming {
            target: bounds.center,
            distance: bounds.radius / (fovy * 0.5).tan() * spec.frame_margin,
        },
        camera,
        floor,
    }
}

/// The previewed model's world-space bounding sphere. Pure of the edit context.
pub(crate) fn compute_preview_bounds(
    scene: &mut Scene,
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    root: Entity,
) -> PreviewBounds {
    use saffron_geometry::glam::Vec3 as GVec3;

    let mut out = PreviewBounds {
        center: GVec3::ZERO,
        radius: 1.0,
        min_y: 0.0,
    };
    // The whole forest's world AABB — every mesh-bearing node, skinned through the joint
    // palette — not a single resolved entity's box.
    if !scene.valid(root) {
        return out;
    }
    let Some((lo, hi)) = model_render_aabb(gpu, scene, assets, root) else {
        // No resolvable mesh: fall back to the model root's position.
        out.center = scene.world_translation(root);
        out.min_y = out.center.y - 1.0;
        return out;
    };
    out.center = (lo + hi) * 0.5;
    out.radius = (hi - lo).length() * 0.5;
    out.min_y = lo.y;
    if out.radius <= 0.0001 {
        out.radius = 1.0;
    }
    out
}

/// A thin floor slab centered under the model's feet. Pure of the edit context.
pub(crate) fn spawn_preview_floor(
    scene: &mut Scene,
    assets: &mut AssetServer,
    gpu: &dyn saffron_assets::GpuUploader,
    bounds: &PreviewBounds,
) -> Entity {
    use saffron_geometry::glam::Vec3 as GVec3;

    if !assets.ensure_preview_floor_mesh(gpu) {
        return Entity::NULL;
    }
    let floor = scene.create_entity("PreviewFloor");
    let _ = scene.add_component(
        floor,
        Mesh {
            mesh: saffron_assets::PREVIEW_FLOOR_MESH_ID,
        },
    );
    let _ = scene.add_component(
        floor,
        MaterialSet {
            slots: vec![MaterialSlot {
                overrides: json!({
                    "baseColor": [0.32, 0.33, 0.35, 1.0],
                    "roughness": 0.92,
                    "metallic": 0.0,
                }),
                ..MaterialSlot::default()
            }],
        },
    );
    let span = (bounds.radius * 8.0).max(0.5);
    let thickness = (bounds.radius * 0.08).max(0.02);
    let _ = scene.with_component_mut::<Transform, _>(floor, |t| {
        t.translation = GVec3::new(
            bounds.center.x,
            bounds.min_y - thickness * 0.5,
            bounds.center.z,
        );
        t.scale = GVec3::new(span, thickness, span);
    });
    floor
}

/// Aim a fly-cam at the model: a 3/4 view fit to its bounding sphere, `margin` past a snug fit.
/// Starts from the current camera so the user's fov/near/far survive.
pub(crate) fn frame_preview_camera(
    mut cam: SceneEditCamera,
    bounds: &PreviewBounds,
    margin: f32,
) -> SceneEditCamera {
    use saffron_geometry::glam::Vec3 as GVec3;

    let fovy = cam.fov.to_radians();
    let distance = bounds.radius / (fovy * 0.5).tan() * margin;
    let eye = bounds.center + GVec3::new(1.0, 0.7, 1.0).normalize() * distance;
    let forward = (bounds.center - eye).normalize();
    cam.position = eye;
    cam.pitch = forward.y.clamp(-1.0, 1.0).asin().to_degrees();
    cam.yaw = forward.x.atan2(-forward.z).to_degrees();
    cam.far_plane = cam.far_plane.max(distance + bounds.radius * 4.0);
    cam.near_plane = (distance * 0.01).clamp(1e-4, 0.1);
    // Frame into orbit mode about the model centre so preview drags sweep the arc; the framed
    // pose shows at once (sync_target snaps pivot/distance/angles, no ease from the prior pose).
    cam.orbit = Some(OrbitState {
        pivot: bounds.center,
        distance,
        target_pivot: bounds.center,
        target_distance: distance,
    });
    cam.sync_target();
    cam
}

/// Builds the [`PlayStateResult`] from the editor state.
pub(crate) fn play_state_result(ctx: &EngineContext<'_>) -> PlayStateResult {
    let editor = &ctx.scene_edit;
    PlayStateResult {
        state: editor.play_state.name().to_owned(),
        play_version: i32::try_from(editor.play_version).unwrap_or(i32::MAX),
        scene_version: i32::try_from(editor.scene_version).unwrap_or(i32::MAX),
        has_primary_camera: editor.had_primary_camera,
        animation_version: i32::try_from(editor.animation_version).unwrap_or(i32::MAX),
        preview_asset: WireUuid(editor.preview_asset.value()),
    }
}

/// Converts a glam `Vec3` into the wire `Vec3`.
pub(crate) fn vec3(v: saffron_geometry::glam::Vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

/// Converts a glam `Vec4` into the wire `Vec4`.
pub(crate) fn vec4(v: saffron_geometry::glam::Vec4) -> Vec4 {
    Vec4 {
        x: v.x,
        y: v.y,
        z: v.z,
        w: v.w,
    }
}

/// Converts a wire `Vec3` into a glam vector.
pub(crate) fn from_vec3(v: Vec3) -> saffron_geometry::glam::Vec3 {
    saffron_geometry::glam::Vec3::new(v.x, v.y, v.z)
}

/// Converts a wire `Vec4` into a glam vector.
pub(crate) fn from_vec4(v: Vec4) -> saffron_geometry::glam::Vec4 {
    saffron_geometry::glam::Vec4::new(v.x, v.y, v.z, v.w)
}
