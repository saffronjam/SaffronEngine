//! `render_scene` — the engine's highest-coupling driver — and its read-side twin
//! [`pick_entity`].
//!
//! [`render_scene`] translates a scene + camera into the renderer's draw list plus every
//! per-frame lighting / shadow / GI / sky / RT / cluster / SSAO setter; [`pick_entity`]
//! ray-casts the same scene to find the clicked entity. Both read the same last-frame
//! world-transform flatten the draw loop writes ([`Scene::update_world_transforms`]),
//! rebuilding the joint palette fresh.
//!
//! # The borrow shape (no `RefCell`)
//!
//! The driver takes three distinct values by distinct borrows: `&mut Scene` (the `for_each`
//! query needs `&mut`, the world reads need `&`), `&mut AssetServer` (mutable for the
//! on-demand cache fill), and `&mut R: SceneRenderer` (mutable for the ~30 setters). They
//! are three different values, so the three borrows are disjoint and the borrow checker is
//! satisfied without interior mutability. The renderer's per-frame setters are methods on
//! the [`SceneRenderer`] trait, which a recording stub also implements — so the driver is
//! unit-tested without a Vulkan device.
//!
//! Because `for_each` borrows the ECS world mutably while the world-transform readers
//! (`world_matrix`/`world_rotation`/`world_translation`) borrow it immutably, each loop
//! first gathers the entity handles + their `Copy` component data through `for_each`, then
//! reads the cached world transforms in a second pass — the codebase's standard split for a
//! query that also reads the hierarchy.

use std::sync::Arc;

use saffron_geometry::glam::{Mat3, Mat4, Vec2, Vec3, Vec4};
use saffron_geometry::{Ray, ray_aabb_slab, ray_triangle, world_aabb_from_corners};
use saffron_rendering::{
    ClusterCamera, DrawItem, EnvSource, GpuLight, GpuMesh, MAX_REFLECTION_PROBES, Material,
    ReflectionProbeUpload, SceneLighting, SdfInstance, SkyRenderSettings, SkygenParams,
};
use saffron_scene::{
    Camera, CameraView, DirectionalLight, Entity, Mesh as MeshComponent, MorphComponent,
    MorphWeightOverride, PointLight, PreviewGhost, ReflectionProbe, Scene, SkinnedMesh, SkyMode,
    SpotLight, Transform, camera_projection,
};

use crate::gpu::GpuUploader;
use crate::{AssetServer, RenderSceneOptions, SystemMeshVisual};

/// The per-frame renderer operations [`render_scene`] drives, plus the upload + skinning
/// seam it inherits from [`GpuUploader`].
///
/// The ~30 per-frame setters are trait methods, so the live renderer ([`RendererScene`])
/// and a recording test stub both satisfy the same contract. The mesh/texture upload +
/// `skinning_enabled`
/// gate ride the [`GpuUploader`] supertrait, so one handle serves both the resolve path
/// (immutable, `&self`) and the setter path (mutable, `&mut self`).
pub trait SceneRenderer: GpuUploader {
    /// The active offscreen viewport width in pixels. `0` early-outs.
    fn viewport_width(&self) -> u32;
    /// The active offscreen viewport height in pixels. `0` early-outs.
    fn viewport_height(&self) -> u32;
    /// This frame's sub-pixel TAA jitter offset in NDC (zero when TAA is inactive). Applied to
    /// the scene view-projection only; clustered lighting / SSAO / picking stay un-jittered.
    fn jitter_offset(&self) -> Vec2;

    /// Arms the spot shadow pass.
    fn set_spot_shadow(&mut self, light_view_proj: Mat4, light_index: u32, casting: bool);
    /// Arms the point shadow pass. `content_key` hashes the light + every caster's world
    /// transform (camera-independent), so the renderer re-renders the 6-face cube only when it
    /// changes — a moving camera over a static light + casters reuses the cached cube.
    fn set_point_shadow(
        &mut self,
        light_pos: Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
        content_key: u64,
    );
    /// Arms the directional shadow pass.
    fn set_directional_shadow(&mut self, light_view_proj: Mat4, casting: bool);
    /// Captures the frame's static RT instances.
    fn set_rt_scene(&mut self, models: Vec<Mat4>, meshes: Vec<Arc<GpuMesh>>);
    /// Snaps the camera-centered DDGI probe clipmap to the camera + passes the sun/sky for the
    /// trace (which sphere-marches the per-mesh MDF + the Global SDF, the sky on miss).
    fn set_ddgi_scene(
        &mut self,
        cam_pos: Vec3,
        sun_dir: Vec3,
        sun_color: Vec3,
        sun_intensity: f32,
        sky_color: Vec3,
    );
    /// Uploads this frame's per-static-instance SDF list (the lighting cone-trace iterates
    /// it for directional sky occlusion).
    fn set_sdf_scene(&mut self, instances: &[SdfInstance]);
    /// Folds the frame's reflection-probe uploads in.
    fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]);
    /// Writes the per-frame light UBO/SSBO.
    ///
    /// # Errors
    ///
    /// Returns a [`saffron_rendering::Error`] if growing the punctual SSBO fails.
    fn set_scene_lighting(&mut self, scene: &SceneLighting) -> saffron_rendering::Result<()>;
    /// Re-arms the IBL environment bake.
    fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<saffron_rendering::GpuTexture>>,
        params: SkygenParams,
    );
    /// Writes the cluster-cull camera params.
    fn set_cluster_camera(&mut self, camera: ClusterCamera);
    /// Writes the screen-space camera + sun direction.
    fn set_ssao_camera(&mut self, view: Mat4, proj: Mat4, sun_direction_world: Vec3);
    /// Toggles the ground-grid debug overlay this frame.
    fn set_show_grid(&mut self, enabled: bool);
    /// Builds the frame's draw list + concatenated joint palette.
    ///
    /// # Errors
    ///
    /// Returns a [`saffron_rendering::Error`] if an SSBO / deformed-buffer grow or upload fails.
    fn submit_draw_list(
        &mut self,
        view_proj: Mat4,
        items: &[DrawItem],
        joints: &[Mat4],
    ) -> saffron_rendering::Result<()>;
    /// Folds the visible-sky settings in.
    fn submit_sky(&mut self, settings: &SkyRenderSettings);
}

/// The nearest rendered-surface hit for a viewport ray.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneSurfaceHit {
    /// The entity whose mesh was hit.
    pub entity: Entity,
    /// The world-space hit point.
    pub point: Vec3,
    /// The ray distance to the hit point.
    pub distance: f32,
}

/// The live-renderer [`SceneRenderer`]: a `&mut Renderer` (the setter target) plus a
/// borrowed [`Uploader`](saffron_rendering::Uploader) for the asset-resolve uploads.
///
/// The renderer owns no [`Uploader`] (the host constructs one alongside it), so the adapter
/// carries both. The upload methods read through `&self` (the uploader + the renderer's
/// descriptors); the setters mutate through `&mut self` — disjoint in time, so one handle
/// drives the whole frame.
pub struct RendererScene<'a> {
    renderer: &'a mut saffron_rendering::Renderer,
    uploader: &'a saffron_rendering::Uploader,
    skinning_enabled: bool,
}

impl<'a> RendererScene<'a> {
    /// Wraps the renderer + its uploader for the scene driver. `skinning_enabled` gates the
    /// skinned draw path (off = byte-identical to a no-skinning build).
    pub fn new(
        renderer: &'a mut saffron_rendering::Renderer,
        uploader: &'a saffron_rendering::Uploader,
        skinning_enabled: bool,
    ) -> Self {
        Self {
            renderer,
            uploader,
            skinning_enabled,
        }
    }
}

impl GpuUploader for RendererScene<'_> {
    fn upload_mesh(
        &self,
        mesh: &saffron_geometry::Mesh,
        skin: &[saffron_geometry::VertexSkin],
        morph: Option<&saffron_geometry::MorphData>,
        sdf_bake: Option<&saffron_rendering::SdfBake>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        self.uploader
            .upload_mesh(self.renderer.descriptors(), mesh, skin, morph, sdf_bake)
    }

    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuTexture>> {
        self.uploader
            .upload_texture(self.renderer.descriptors(), rgba, width, height, srgb)
    }

    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuTexture>> {
        self.uploader
            .upload_texture_float(self.renderer.descriptors(), rgba, width, height)
    }

    fn skinning_enabled(&self) -> bool {
        self.skinning_enabled
    }
}

impl SceneRenderer for RendererScene<'_> {
    fn viewport_width(&self) -> u32 {
        // The scene renders at INPUT extent; the TAA resolve reconstructs it to display.
        self.renderer.active_view().scaled_render_extent().width
    }

    fn viewport_height(&self) -> u32 {
        self.renderer.active_view().scaled_render_extent().height
    }

    fn jitter_offset(&self) -> Vec2 {
        self.renderer.active_view_jitter()
    }

    fn set_spot_shadow(&mut self, light_view_proj: Mat4, light_index: u32, casting: bool) {
        self.renderer
            .set_spot_shadow(light_view_proj, light_index, casting);
    }

    fn set_point_shadow(
        &mut self,
        light_pos: Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
        content_key: u64,
    ) {
        self.renderer
            .set_point_shadow(light_pos, far_plane, light_index, casting, content_key);
    }

    fn set_directional_shadow(&mut self, light_view_proj: Mat4, casting: bool) {
        self.renderer
            .set_directional_shadow(light_view_proj, casting);
    }

    fn set_rt_scene(&mut self, models: Vec<Mat4>, meshes: Vec<Arc<GpuMesh>>) {
        self.renderer.set_rt_scene(models, meshes);
    }

    fn set_ddgi_scene(
        &mut self,
        cam_pos: Vec3,
        sun_dir: Vec3,
        sun_color: Vec3,
        sun_intensity: f32,
        sky_color: Vec3,
    ) {
        self.renderer
            .set_ddgi_scene(cam_pos, sun_dir, sun_color, sun_intensity, sky_color);
    }

    fn set_sdf_scene(&mut self, instances: &[SdfInstance]) {
        self.renderer.set_sdf_scene(instances);
    }

    fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]) {
        self.renderer.submit_reflection_probes(probes);
    }

    fn set_scene_lighting(&mut self, scene: &SceneLighting) -> saffron_rendering::Result<()> {
        self.renderer.set_scene_lighting(scene)
    }

    fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<saffron_rendering::GpuTexture>>,
        params: SkygenParams,
    ) {
        self.renderer.request_env_bake(source, panorama, params);
    }

    fn set_cluster_camera(&mut self, camera: ClusterCamera) {
        self.renderer.set_cluster_camera(camera);
    }

    fn set_ssao_camera(&mut self, view: Mat4, proj: Mat4, sun_direction_world: Vec3) {
        self.renderer
            .set_ssao_camera(view, proj, sun_direction_world);
    }

    fn set_show_grid(&mut self, enabled: bool) {
        self.renderer.set_show_grid(enabled);
    }

    fn submit_draw_list(
        &mut self,
        view_proj: Mat4,
        items: &[DrawItem],
        joints: &[Mat4],
    ) -> saffron_rendering::Result<()> {
        self.renderer
            .submit_draw_list_skinned(view_proj, items, joints)
    }

    fn submit_sky(&mut self, settings: &SkyRenderSettings) {
        self.renderer.submit_sky(settings);
    }
}

/// A gimbal-stable up vector for a `lookAt` down `dir`: switches to `+Z` when `dir` is
/// near-vertical.
fn look_at_up_for_dir(dir: Vec3) -> Vec3 {
    if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y }
}

/// The entity's stable [`IdComponent`](saffron_scene::IdComponent) uuid value, or `0` when
/// it carries no id.
fn entity_id_or_zero(scene: &Scene, entity: Entity) -> u64 {
    scene
        .component::<saffron_scene::IdComponent>(entity)
        .map_or(0, |id| id.id.value())
}

/// The morph weights driving `entity` this frame: the runtime-only [`MorphWeightOverride`]
/// (animated, present while a clip plays) when non-empty, else the durable
/// [`MorphComponent`] rest weights, else empty (not a morph mesh).
fn morph_weights_for(scene: &Scene, entity: Entity) -> Vec<f32> {
    if let Ok(weights) =
        scene.with_component::<MorphWeightOverride, _>(entity, |m| m.weights.clone())
        && !weights.is_empty()
    {
        return weights;
    }
    scene
        .with_component::<MorphComponent, _>(entity, |m| m.weights.clone())
        .unwrap_or_default()
}

/// `transpose(inverse(mat3(model)))` as a [`Mat4`] (the normal matrix).
fn normal_matrix(model: Mat4) -> Mat4 {
    Mat4::from_mat3(Mat3::from_mat4(model).inverse().transpose())
}

/// A `glm::lookAt`-equivalent view matrix (right-handed, the engine's GLM convention).
fn look_at(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    Mat4::look_at_rh(eye, center, up)
}

/// A `glm::perspective` with Vulkan `[0, 1]` clip depth (`GLM_FORCE_DEPTH_ZERO_TO_ONE`).
fn perspective(fov: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    Mat4::perspective_rh(fov, aspect, near, far)
}

/// A `glm::ortho` with Vulkan `[0, 1]` clip depth (`GLM_FORCE_DEPTH_ZERO_TO_ONE`).
fn orthographic(left: f32, right: f32, bottom: f32, top: f32, near: f32, far: f32) -> Mat4 {
    Mat4::orthographic_rh(left, right, bottom, top, near, far)
}

/// Appends the editor-camera gizmo models to `items`: one per [`Camera`] entity with
/// `show_model`, placed by its world matrix and the fixed local lens offset.
///
/// A no-op until the editor-camera mesh loads (attempted once). The visual's mesh + resolved
/// material are read out of [`AssetServer::editor_camera_model`] after the load.
fn append_editor_camera_models<R: SceneRenderer>(
    scene: &mut Scene,
    assets: &mut AssetServer,
    renderer: &R,
    items: &mut Vec<DrawItem>,
) {
    if !assets.load_editor_camera_model(renderer) {
        return;
    }
    let SystemMeshVisual {
        mesh: Some(mesh),
        submesh_materials,
        ..
    } = &assets.editor_camera_model
    else {
        return;
    };
    let mesh = Arc::clone(mesh);
    let submesh_materials = submesh_materials.clone();

    let mut cameras: Vec<Entity> = Vec::new();
    scene.for_each::<(&Transform, &Camera), _>(|entity, (_, camera)| {
        if camera.show_model {
            cameras.push(entity);
        }
    });
    for entity in cameras {
        const MODEL_SCALE: f32 = 7.5;
        const LENS_LOCAL_X: f32 = 0.080_121_7;
        let model = scene.world_matrix(entity)
            * Mat4::from_translation(Vec3::new(0.0, -0.1, 0.0))
            * Mat4::from_rotation_y(90.0_f32.to_radians())
            * Mat4::from_scale(Vec3::splat(MODEL_SCALE))
            * Mat4::from_translation(Vec3::new(-LENS_LOCAL_X, 0.0, 0.0));
        items.push(DrawItem {
            mesh: Arc::clone(&mesh),
            model,
            normal_matrix: normal_matrix(model),
            submesh_materials: submesh_materials.clone(),
            material: Material::default(),
            skinned: false,
            joint_offset: 0,
            joint_count: 0,
            morph_weights: Vec::new(),
            entity: 0,
        });
    }
}

/// Draws every renderable in `scene` through `camera`, driving the renderer's draw list and
/// every per-frame lighting / shadow / GI / sky / RT / cluster / SSAO setter.
///
/// A no-op on a zero-size viewport. `update_world_transforms` runs **once** before any
/// consumer reads the world-transform cache. The skinned draw list is gathered only when
/// `renderer.skinning_enabled()` — off, the frame is byte-identical to a build without the
/// skinned path. The static RT instances (carrying `item.model`) split from the skinned ones
/// (which ride the draw list with identity).
/// The world-space AABB of a model: the union over every renderable mesh in `root`'s subtree
/// ([`Scene::model_mesh_entities`]). Static meshes transform their rest box by the entity's
/// world matrix; skinned meshes union the bind box through each joint matrix (matching what
/// the screen shows — the skinned vertex follows the palette, not the mesh node's world
/// transform). `None` when no mesh in the subtree resolves to a loaded asset.
///
/// This is the forest-wide counterpart to a single-entity bounds probe: a multi-node model
/// frames around its whole assembled geometry, not one node.
pub fn model_render_aabb(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
    root: Entity,
) -> Option<(Vec3, Vec3)> {
    let entities = scene.model_mesh_entities(root);
    render_aabb_of(gpu, scene, assets, &entities)
}

/// The world-space AABB of every renderable mesh in the scene (the whole-scene union).
pub fn scene_render_aabb(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
) -> Option<(Vec3, Vec3)> {
    let mut entities: Vec<Entity> = Vec::new();
    scene.for_each::<&MeshComponent, _>(|entity, _| entities.push(entity));
    scene.for_each::<&SkinnedMesh, _>(|entity, _| entities.push(entity));
    render_aabb_of(gpu, scene, assets, &entities)
}

/// The shared AABB union: folds each entity's mesh box into `(min, max)`, picking the skinned
/// (joint-palette) or static (world-matrix) path per entity. The single implementation behind
/// both [`model_render_aabb`] (a subtree) and [`scene_render_aabb`] (the whole scene).
fn render_aabb_of(
    gpu: &dyn GpuUploader,
    scene: &mut Scene,
    assets: &mut AssetServer,
    entities: &[Entity],
) -> Option<(Vec3, Vec3)> {
    scene.update_world_transforms();
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    let mut found = false;
    for &entity in entities {
        if let Ok(skin) = scene.with_component::<SkinnedMesh, _>(entity, SkinnedMesh::clone) {
            let Some(mesh_ref) = assets.load_mesh_asset(gpu, skin.mesh) else {
                continue;
            };
            for joint in scene.joint_matrices(&skin) {
                world_aabb_from_corners(
                    &joint,
                    mesh_ref.bounds_min,
                    mesh_ref.bounds_max,
                    &mut min,
                    &mut max,
                );
                found = true;
            }
        } else if let Ok(mesh) = scene.component::<MeshComponent>(entity) {
            let Some(mesh_ref) = assets.load_mesh_asset(gpu, mesh.mesh) else {
                continue;
            };
            let model = scene.world_matrix(entity);
            world_aabb_from_corners(
                &model,
                mesh_ref.bounds_min,
                mesh_ref.bounds_max,
                &mut min,
                &mut max,
            );
            found = true;
        }
    }
    found.then_some((min, max))
}

/// A camera-independent hash of the inputs the point-shadow cube depends on: the light position +
/// far plane and every mesh entity's world matrix + mesh asset id.
///
/// The renderer re-renders the 6-face cube only when this changes, so panning the camera over a
/// static light + static casters reuses the cached cube; any caster (or the light) moving, or a
/// mesh added/removed/reassigned, invalidates it. (A glTF reimport that swaps a mesh's geometry
/// under the same asset id is the one case this misses — the cube refreshes on the next transform
/// change; reimport is rare and already idles the GPU + rebuilds caches.)
fn point_shadow_content_key(scene: &mut Scene, light_pos: Vec3, far_plane: f32) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    let mut fold = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    };
    for component in light_pos.to_array() {
        fold(&component.to_le_bytes());
    }
    fold(&far_plane.to_le_bytes());
    let mut casters: Vec<(Entity, u64)> = Vec::new();
    scene.for_each::<&MeshComponent, _>(|entity, mesh| casters.push((entity, mesh.mesh.0)));
    for (entity, mesh_id) in casters {
        // Skinned (deformed) casters are *dynamic* — they render into the per-frame dynamic cube,
        // so they must not key the static cube (an animating skeleton would otherwise invalidate
        // it every frame and defeat the cache). Their motion is captured by re-rendering the
        // dynamic cube each frame, not by this key.
        if scene.has_component::<SkinnedMesh>(entity) {
            continue;
        }
        for component in scene.world_matrix(entity).to_cols_array() {
            fold(&component.to_le_bytes());
        }
        fold(&mesh_id.to_le_bytes());
    }
    hash
}

/// Renders the authored scene into one frame draw list. Asset-placement preview ghosts are
/// ordinary [`PreviewGhost`](saffron_scene::PreviewGhost)-tagged entities in this scene, so they
/// render through the normal gather; nothing else special-cases them.
pub fn render_scene<R: SceneRenderer>(
    renderer: &mut R,
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    options: RenderSceneOptions,
) {
    let width = renderer.viewport_width();
    let height = renderer.viewport_height();
    if width == 0 || height == 0 {
        return;
    }
    let aspect = width as f32 / height as f32;
    let view = camera.view;
    let mut proj = camera_projection(camera, aspect);
    proj.y_axis.y *= -1.0; // flip Y into Vulkan clip space
    // Sub-pixel TAA jitter: shift NDC by the Halton offset as a clip-space translation
    // (`clip.xy += jitter · clip.w`). Applied to the scene view-projection only — `proj` stays
    // un-jittered for clustered lighting / SSAO / picking. The offset is zero unless TAA is on,
    // and the renderer reprojects the motion prepass / grid against the un-jittered matrix.
    let jitter = renderer.jitter_offset();
    let view_projection =
        Mat4::from_translation(Vec3::new(jitter.x, jitter.y, 0.0)) * (proj * view);

    // Flatten the hierarchy once per frame before any consumer reads: every loop below
    // (lights, meshes, probes) and the between-frame pick/gizmo paths read the world-
    // transform cache this writes.
    scene.update_world_transforms();

    let (light_dir, light_color, light_intensity, light_ambient) = gather_directional_light(scene);
    let (lights, point_shadow, spot_shadow) = gather_punctual_lights(scene);

    renderer.set_spot_shadow(
        spot_shadow.map_or(Mat4::IDENTITY, |s| s.view_proj),
        spot_shadow.map_or(0, |s| s.light_index),
        spot_shadow.is_some(),
    );
    let point_shadow_pos = point_shadow.map_or(Vec3::ZERO, |p| p.pos);
    let point_shadow_far = point_shadow.map_or(1.0, |p| p.far);
    renderer.set_point_shadow(
        point_shadow_pos,
        point_shadow_far,
        point_shadow.map_or(0, |p| p.light_index),
        point_shadow.is_some(),
        point_shadow_content_key(scene, point_shadow_pos, point_shadow_far),
    );

    // The camera world position is the inverse-view translation; the BRDF needs it as the
    // view-vector origin. The lighting upload happens after the draw loop, once the scene
    // AABB (hence the shadow frustum) is known.
    let eye_position = view.inverse().w_axis.truncate();

    let mut build = DrawListBuild::default();
    gather_static_draw_list(renderer, scene, assets, &mut build);
    let frame_joints = if renderer.skinning_enabled() {
        gather_skinned_draw_list(renderer, scene, assets, &mut build)
    } else {
        Vec::new()
    };
    let DrawListBuild {
        mut items,
        scene_min,
        scene_max,
        sdf_instances,
    } = build;

    // Fit an orthographic shadow frustum to the scene's world AABB, looking down the
    // directional light. A bounding sphere keeps the fit rotation-stable.
    let cast_shadow = !items.is_empty() && scene_max.x >= scene_min.x;
    let shadow_view_proj = if cast_shadow {
        let center = (scene_min + scene_max) * 0.5;
        let radius = (scene_max - scene_min).length() * 0.5 + 0.5;
        let dir = light_dir.normalize();
        let up = look_at_up_for_dir(dir);
        let eye = center - dir * (radius + 1.0);
        let light_view = look_at(eye, center, up);
        let light_proj = orthographic(-radius, radius, -radius, radius, 0.0, 2.0 * radius + 2.0);
        light_proj * light_view
    } else {
        Mat4::IDENTITY
    };
    renderer.set_directional_shadow(shadow_view_proj, cast_shadow);

    // RT: hand the frame's STATIC instance transforms + meshes to the renderer for the
    // per-frame TLAS build. Skinned instances ride the draw list (their deformed verts are
    // already world-space, referenced by an identity transform), so they are excluded here.
    {
        let mut rt_models = Vec::with_capacity(items.len());
        let mut rt_meshes = Vec::with_capacity(items.len());
        for item in &items {
            if item.skinned {
                continue;
            }
            rt_models.push(item.model);
            rt_meshes.push(Arc::clone(&item.mesh));
        }
        renderer.set_rt_scene(rt_models, rt_meshes);
    }

    // Upload the frame's per-static-instance SDF occluder list (empty when no static
    // mesh carries a baked field — that resets the count). Independent of DDGI: the
    // lighting cone-trace reads it for directional sky occlusion whether DDGI is on or
    // off.
    renderer.set_sdf_scene(&sdf_instances);

    // DDGI: snap the camera-centered probe clipmap to the camera and pass the sun/sky. The trace
    // sphere-marches the real distance field (per-mesh MDF near + Global SDF far), so it needs no
    // scene-box proxy. Done before the lighting upload, which reads the volume placement + scroll
    // base into the light UBO.
    let mut ddgi_sky = Vec3::new(0.1, 0.13, 0.2);
    if scene.environment.use_sky_for_ambient {
        ddgi_sky = scene.environment.ambient_color * scene.environment.ambient_intensity;
    }
    renderer.set_ddgi_scene(
        eye_position,
        light_dir,
        light_color,
        light_intensity,
        ddgi_sky,
    );

    let probe_uploads = gather_reflection_probes(scene);
    renderer.submit_reflection_probes(&probe_uploads);

    // Fallback ambient (used when IBL is off): the scene environment's ambient color when
    // use_sky_for_ambient, else the directional light's scalar ambient (grayscale).
    let ambient = if scene.environment.use_sky_for_ambient {
        scene.environment.ambient_color * scene.environment.ambient_intensity
    } else {
        Vec3::splat(light_ambient)
    };
    if let Err(err) = renderer.set_scene_lighting(&SceneLighting {
        direction: light_dir,
        color: light_color,
        intensity: light_intensity,
        ambient,
        eye_position,
        lights,
    }) {
        tracing::error!("set_scene_lighting: {err}");
    }

    // Drive the environment bake. Equirect (a loaded panorama) wins, then the atmosphere,
    // then the procedural gradient — the sun derived from the directional light.
    let sky_panorama = drive_env_bake(
        renderer,
        scene,
        assets,
        light_dir,
        light_color,
        light_intensity,
    );

    renderer.set_cluster_camera(ClusterCamera {
        view,
        projection: proj,
        width,
        height,
        near: camera.near_plane,
        far: camera.far_plane,
    });
    // Screen-space passes (G-buffer/GTAO/contact/SSGI) use the scene view/proj + the
    // directional light direction (for contact shadows).
    renderer.set_ssao_camera(view, proj, light_dir);
    renderer.set_show_grid(options.show_grid);

    if options.show_editor_camera_models {
        append_editor_camera_models(scene, assets, renderer, &mut items);
    }
    if let Err(err) = renderer.submit_draw_list(view_projection, &items, &frame_joints) {
        tracing::error!("submit_draw_list: {err}");
    }

    // Resolve the scene environment into the visible-sky settings.
    let env = scene.environment;
    let mut sky = SkyRenderSettings {
        mode: env.sky_mode as u32,
        clear_color: env.clear_color,
        intensity: env.sky_intensity,
        rotation: env.sky_rotation,
        visible: env.visible,
        texture_index: 0,
    };
    if env.sky_mode == SkyMode::Texture && env.sky_texture.value() != 0 {
        if let Some(panorama) = &sky_panorama {
            sky.texture_index = panorama.bindless_index();
        } else {
            sky.mode = SkyMode::Color as u32; // missing panorama -> clear color
        }
    }
    renderer.submit_sky(&sky);
}

/// The directional light's resolved direction / color / intensity / ambient, re-aimed by the
/// entity's world rotation when it carries a [`Transform`]. The first one wins; a scene with
/// no directional light keeps the default `(-0.5, -1, -0.3)`, white, intensity 1,
/// ambient 0.15.
fn gather_directional_light(scene: &mut Scene) -> (Vec3, Vec3, f32, f32) {
    let mut found: Option<(Entity, DirectionalLight)> = None;
    scene.for_each::<&DirectionalLight, _>(|entity, light| {
        if found.is_none() {
            found = Some((entity, *light));
        }
    });
    let Some((entity, light)) = found else {
        return (Vec3::new(-0.5, -1.0, -0.3), Vec3::ONE, 1.0, 0.15);
    };
    let dir = if scene.has_component::<Transform>(entity) {
        scene.world_rotation(entity) * light.direction
    } else {
        light.direction
    };
    (dir, light.color, light.intensity, light.ambient)
}

/// The first point light's shadow inputs (the single shadowed point in v1).
#[derive(Clone, Copy)]
struct PointShadow {
    pos: Vec3,
    far: f32,
    light_index: u32,
}

/// The first spot light's shadow inputs (the single shadowed spot in v1).
#[derive(Clone, Copy)]
struct SpotShadow {
    view_proj: Mat4,
    light_index: u32,
}

/// Gathers the punctual (point + spot) lights into the per-frame [`GpuLight`] list, tracking
/// the first point's position/range and the first spot's perspective light-space transform.
fn gather_punctual_lights(
    scene: &mut Scene,
) -> (Vec<GpuLight>, Option<PointShadow>, Option<SpotShadow>) {
    let mut points: Vec<(Entity, PointLight)> = Vec::new();
    scene.for_each::<(&Transform, &PointLight), _>(|entity, (_, light)| {
        points.push((entity, *light));
    });
    let mut spots: Vec<(Entity, SpotLight)> = Vec::new();
    scene.for_each::<(&Transform, &SpotLight), _>(|entity, (_, light)| {
        spots.push((entity, *light));
    });

    let mut lights: Vec<GpuLight> = Vec::new();
    let mut point_shadow: Option<PointShadow> = None;
    for (entity, light) in points {
        let pos = scene.world_translation(entity);
        lights.push(GpuLight {
            position_range: pos.extend(light.range),
            color_intensity: light.color.extend(light.intensity),
            direction_type: Vec4::ZERO, // type 0 = point
            spot_cos: Vec4::ZERO,
        });
        if point_shadow.is_none() {
            point_shadow = Some(PointShadow {
                pos,
                far: light.range.max(0.1),
                light_index: (lights.len() - 1) as u32,
            });
        }
    }

    let mut spot_shadow: Option<SpotShadow> = None;
    for (entity, light) in spots {
        let pos = scene.world_translation(entity);
        let dir = (scene.world_rotation(entity) * light.direction).normalize();
        let index = lights.len() as u32;
        lights.push(GpuLight {
            position_range: pos.extend(light.range),
            color_intensity: light.color.extend(light.intensity),
            direction_type: dir.extend(1.0), // type 1 = spot
            spot_cos: Vec4::new(
                light.inner_angle.to_radians().cos(),
                light.outer_angle.to_radians().cos(),
                0.0,
                0.0,
            ),
        });
        if spot_shadow.is_none() {
            // A perspective frustum down the spot cone: fov = 2 x outer angle (a small pad so
            // the penumbra sits inside the map), aspect 1, near/far from range.
            let fov = (2.0 * light.outer_angle + 2.0).min(179.0).to_radians();
            let up = look_at_up_for_dir(dir);
            let light_view = look_at(pos, pos + dir, up);
            let light_proj = perspective(fov, 1.0, 0.05, light.range.max(0.1));
            spot_shadow = Some(SpotShadow {
                view_proj: light_proj * light_view,
                light_index: index,
            });
        }
    }
    (lights, point_shadow, spot_shadow)
}

/// The accumulating draw-list + scene-AABB + SDF-occluder state built across the static and
/// skinned passes.
struct DrawListBuild {
    items: Vec<DrawItem>,
    scene_min: Vec3,
    scene_max: Vec3,
    /// One [`SdfInstance`] per static draw whose mesh carries a baked signed distance field — the
    /// lighting cone-trace's + the GDF composite's per-instance occluder list. Its base color rides
    /// the reserved `.w` of the world/local AABB corners (the GDF albedo cache's per-cell color).
    sdf_instances: Vec<SdfInstance>,
}

impl Default for DrawListBuild {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            scene_min: Vec3::splat(f32::MAX),
            scene_max: Vec3::splat(f32::MIN),
            sdf_instances: Vec::new(),
        }
    }
}

/// Gathers the static `Transform + Mesh` renderables: resolves each mesh + its materials on
/// demand, accumulates the world AABB + per-draw box proxies, and pushes a [`DrawItem`].
fn gather_static_draw_list<R: SceneRenderer>(
    renderer: &R,
    scene: &mut Scene,
    assets: &mut AssetServer,
    build: &mut DrawListBuild,
) {
    let mut meshes: Vec<(Entity, MeshComponent)> = Vec::new();
    scene.for_each::<(&Transform, &MeshComponent), _>(|entity, (_, mesh)| {
        meshes.push((entity, *mesh));
    });
    for (entity, mesh) in meshes {
        let Some(mesh_ref) = assets.load_mesh_asset(renderer, mesh.mesh) else {
            continue;
        };
        let submeshes = mesh_ref.submeshes.clone();
        let materials = assets.resolve_entity_materials(renderer, scene, entity, &submeshes);
        let model = scene.world_matrix(entity);
        let mut box_min = Vec3::splat(f32::MAX);
        let mut box_max = Vec3::splat(f32::MIN);
        world_aabb_from_corners(
            &model,
            mesh_ref.bounds_min,
            mesh_ref.bounds_max,
            &mut box_min,
            &mut box_max,
        );
        build.scene_min = build.scene_min.min(box_min);
        build.scene_max = build.scene_max.max(box_max);
        // A static instance contributes one SDF occluder per baked field — one tight field
        // per primitive (and per spatial chunk of an oversized primitive). Each carries the
        // shared world→local transform, the field's OWN world AABB (for the cone-march's
        // per-sample cull, tight so many small fields cull independently), and its bindless
        // slot + encode clamp.
        if !mesh_ref.sdfs().is_empty() {
            // The fields store distances in local (rest) units, but the cone-trace marches and
            // compares in world space. The instance's world scale (the mean basis-column length
            // of the model's upper-left 3×3) converts a sampled local distance to world units,
            // keeping the AO footprint correct under non-unit instance scale — shared by every
            // field of this mesh (they share its local frame).
            let basis = Mat3::from_mat4(model);
            let world_scale =
                (basis.x_axis.length() + basis.y_axis.length() + basis.z_axis.length()) / 3.0;
            let world_to_local = model.inverse();
            let uvec = |a: [u32; 3]| saffron_geometry::glam::UVec4::new(a[0], a[1], a[2], 0);
            // The base color rides the reserved `.w` of the world/local AABB corners (r, g, b) —
            // the GDF composite splats it into the lite albedo cache the DDGI trace reads. The
            // brick sample reads only `.xyz`, so this packing is transparent to it.
            let albedo = materials.proxy_albedo;
            for field in mesh_ref.sdfs() {
                let mut field_min = Vec3::splat(f32::MAX);
                let mut field_max = Vec3::splat(f32::MIN);
                world_aabb_from_corners(
                    &model,
                    field.bounds_min,
                    field.bounds_max,
                    &mut field_min,
                    &mut field_max,
                );
                build.sdf_instances.push(SdfInstance {
                    world_to_local,
                    world_min: field_min.extend(albedo.x),
                    world_max: field_max.extend(albedo.y),
                    local_min: field.bounds_min.extend(albedo.z),
                    local_max: field.bounds_max.extend(0.0),
                    params: Vec4::new(
                        field.bindless_index() as f32,
                        field.max_dist,
                        world_scale,
                        field.mip_count as f32,
                    ),
                    voxel_dims: uvec(field.voxel_dims),
                    indir_dims: uvec(field.indirection_dims),
                    atlas_bricks: uvec(field.atlas_bricks),
                });
            }
        }
        build.items.push(DrawItem {
            mesh: mesh_ref,
            model,
            normal_matrix: normal_matrix(model),
            submesh_materials: materials.submeshes,
            material: Material {
                shader: materials.shader,
                unlit: materials.unlit,
                // blend/masked are per-submesh now — each SubmeshMaterial carries its own blend
                // mode, and the draw-list batcher resolves the PSO + pass per submesh. The item's
                // Material is only the shared PSO base (shader + unlit).
                blend: false,
                masked: false,
            },
            skinned: false,
            joint_offset: 0,
            joint_count: 0,
            morph_weights: morph_weights_for(scene, entity),
            entity: entity_id_or_zero(scene, entity),
        });
    }
}

/// Gathers the skinned `Transform + SkinnedMesh` renderables (identity model, joint palette
/// via [`Scene::joint_matrices`]), unioning the conservative bind-AABB bounds through every
/// joint, and returns the concatenated frame joint palette. Called only when skinning is on.
fn gather_skinned_draw_list<R: SceneRenderer>(
    renderer: &R,
    scene: &mut Scene,
    assets: &mut AssetServer,
    build: &mut DrawListBuild,
) -> Vec<Mat4> {
    let mut skins: Vec<(Entity, SkinnedMesh)> = Vec::new();
    scene.for_each::<(&Transform, &SkinnedMesh), _>(|entity, (_, skin)| {
        skins.push((entity, skin.clone()));
    });

    let mut frame_joints: Vec<Mat4> = Vec::new();
    for (entity, skin) in skins {
        let Some(mesh_ref) = assets.load_mesh_asset(renderer, skin.mesh) else {
            continue;
        };
        if mesh_ref.skin_buffer().is_none() {
            continue; // baked without a skin stream
        }
        let palette = scene.joint_matrices(&skin);
        if palette.is_empty() {
            continue;
        }
        let submeshes = mesh_ref.submeshes.clone();
        let materials = assets.resolve_entity_materials(renderer, scene, entity, &submeshes);
        // Conservative bounds: union the bind-space AABB corners through every joint.
        for joint in &palette {
            world_aabb_from_corners(
                joint,
                mesh_ref.bounds_min,
                mesh_ref.bounds_max,
                &mut build.scene_min,
                &mut build.scene_max,
            );
        }
        build.items.push(DrawItem {
            mesh: mesh_ref,
            model: Mat4::IDENTITY,
            normal_matrix: Mat4::IDENTITY,
            submesh_materials: materials.submeshes,
            material: Material {
                shader: materials.shader,
                unlit: materials.unlit,
                // blend/masked are per-submesh now — each SubmeshMaterial carries its own blend
                // mode, and the draw-list batcher resolves the PSO + pass per submesh. The item's
                // Material is only the shared PSO base (shader + unlit).
                blend: false,
                masked: false,
            },
            skinned: true,
            joint_offset: frame_joints.len() as u32,
            joint_count: palette.len() as u32,
            morph_weights: morph_weights_for(scene, entity),
            entity: entity_id_or_zero(scene, entity),
        });
        frame_joints.extend_from_slice(&palette);
    }
    frame_joints
}

/// Snapshots each [`ReflectionProbe`] (positioned by its [`Transform`]) into a per-frame
/// upload list, consuming each probe's `dirty` flag (capped at [`MAX_REFLECTION_PROBES`]).
fn gather_reflection_probes(scene: &mut Scene) -> Vec<ReflectionProbeUpload> {
    let mut probes: Vec<(Entity, ReflectionProbe)> = Vec::new();
    scene.for_each::<(&Transform, &mut ReflectionProbe), _>(|entity, (_, probe)| {
        if probes.len() < MAX_REFLECTION_PROBES as usize {
            probes.push((entity, *probe));
            probe.dirty = false; // consumed; the renderer tracks capture state from here
        }
    });
    probes
        .into_iter()
        .map(|(entity, probe)| ReflectionProbeUpload {
            entity: entity_id_or_zero(scene, entity),
            origin: scene.world_translation(entity),
            influence_radius: probe.influence_radius,
            intensity: probe.intensity,
            box_projection: probe.box_projection,
            box_extent: probe.box_extent,
            dirty: probe.dirty,
        })
        .collect()
}

/// Drives the environment bake from the scene environment + the sun derived from the
/// directional light, returning the loaded sky panorama (Texture mode) for the visible-sky
/// resolve below.
fn drive_env_bake<R: SceneRenderer>(
    renderer: &mut R,
    scene: &Scene,
    assets: &mut AssetServer,
    light_dir: Vec3,
    light_color: Vec3,
    light_intensity: f32,
) -> Option<Arc<saffron_rendering::GpuTexture>> {
    let env = scene.environment;
    let at = env.atmosphere;
    let sky_bake = SkygenParams {
        sun_dir: -light_dir,
        sun_intensity: light_intensity,
        sun_color: light_color,
        atmosphere: saffron_rendering::AtmosphereParams {
            enabled: at.enabled,
            planet_radius: at.planet_radius,
            atmosphere_height: at.atmosphere_height,
            rayleigh_scattering: at.rayleigh_scattering,
            rayleigh_scale_height: at.rayleigh_scale_height,
            mie_scattering: at.mie_scattering,
            mie_scale_height: at.mie_scale_height,
            mie_anisotropy: at.mie_anisotropy,
            ozone_absorption: at.ozone_absorption,
            sun_disk_angular_radius: at.sun_disk_angular_radius,
            sun_disk_intensity: at.sun_disk_intensity,
        },
    };
    // Resolution order: a user equirect panorama wins, then the atmosphere, then the
    // gradient. Only a valid loaded panorama claims Equirect.
    let want_equirect = env.sky_mode == SkyMode::Texture && env.sky_texture.value() != 0;
    let sky_panorama = if want_equirect {
        assets.load_texture_asset(renderer, env.sky_texture)
    } else {
        None
    };
    if let Some(panorama) = &sky_panorama {
        renderer.request_env_bake(EnvSource::Equirect, Some(Arc::clone(panorama)), sky_bake);
    } else if at.enabled {
        renderer.request_env_bake(EnvSource::Atmosphere, None, sky_bake);
    } else {
        renderer.request_env_bake(EnvSource::Procedural, None, sky_bake);
    }
    sky_panorama
}

/// Picks the nearest entity the camera ray strikes: a per-mesh AABB broad-phase rejects far
/// meshes, then a ray-triangle narrow-phase finds the true surface hit (so a click through
/// the empty space inside a loose bounding box misses).
///
/// Covers static [`MeshComponent`] (rest verts transformed by the entity's world matrix) and
/// [`SkinnedMesh`] (verts CPU-skinned through a freshly-rebuilt joint palette into world
/// space, exactly as the GPU does). `ndc` is the click point in clip space `[-1, 1]` matching
/// the rendered image (Y-flipped proj). Returns [`Entity::NULL`] on a miss.
///
/// Takes the upload seam + the active `(width, height)` viewport directly rather than a
/// full [`SceneRenderer`] — picking needs only the AABB mesh upload + the aspect ratio, not
/// the per-frame render driver — so the control plane drives it through
/// `ControlRenderer::with_gpu_uploader` + the viewport-size query.
pub fn pick_entity(
    gpu: &dyn GpuUploader,
    viewport: (u32, u32),
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    ndc: Vec2,
) -> Entity {
    pick_scene_surface(gpu, viewport, scene, assets, camera, ndc)
        .map_or(Entity::NULL, |hit| hit.entity)
}

/// Picks the nearest rendered surface hit for a viewport NDC point.
pub fn pick_scene_surface(
    gpu: &dyn GpuUploader,
    viewport: (u32, u32),
    scene: &mut Scene,
    assets: &mut AssetServer,
    camera: &CameraView,
    ndc: Vec2,
) -> Option<SceneSurfaceHit> {
    let (width, height) = viewport;
    if width == 0 || height == 0 {
        return None;
    }
    let ray = viewport_ray(viewport, camera, ndc);

    // The world transforms come from the last frame's flatten (lockstep with the draw loop);
    // the joint palette is rebuilt fresh below.
    let mut statics: Vec<(Entity, MeshComponent)> = Vec::new();
    scene.for_each::<(&Transform, &MeshComponent), _>(|entity, (_, mesh)| {
        statics.push((entity, *mesh));
    });
    let mut skins: Vec<(Entity, SkinnedMesh)> = Vec::new();
    scene.for_each::<(&Transform, &SkinnedMesh), _>(|entity, (_, skin)| {
        skins.push((entity, skin.clone()));
    });

    let mut hit = None;
    let mut nearest = f32::MAX;

    for (entity, mesh) in statics {
        // A placement ghost must never be its own placement target.
        if scene.has_component::<PreviewGhost>(entity) {
            continue;
        }
        let Some(mesh_ref) = assets.load_mesh_asset(gpu, mesh.mesh) else {
            continue;
        };
        if mesh_ref.cpu_positions.is_empty() {
            continue;
        }
        let model = scene.world_matrix(entity);
        let mut world_min = Vec3::splat(f32::MAX);
        let mut world_max = Vec3::splat(f32::MIN);
        world_aabb_from_corners(
            &model,
            mesh_ref.bounds_min,
            mesh_ref.bounds_max,
            &mut world_min,
            &mut world_max,
        );
        if ray_aabb_slab(&ray, world_min, world_max).is_none() {
            continue;
        }
        // Narrowphase via the cached per-mesh BVH, traversed in mesh-local space (transform the
        // world ray by the inverse world matrix, then map the hit point back to world). This is
        // sublinear in the mesh's triangle count, unlike transforming and scanning every triangle.
        let Some(bvh) = assets.mesh_pick_bvh(mesh.mesh, &mesh_ref) else {
            continue;
        };
        let inv = model.inverse();
        let local_ray = Ray {
            origin: inv.transform_point3(ray.origin),
            dir: inv.transform_vector3(ray.dir),
        };
        if let Some(t_local) = bvh.raycast(&local_ray) {
            let world_hit = model.transform_point3(local_ray.origin + local_ray.dir * t_local);
            // `ray.dir` is unit-length in world space, so the projection is the world distance.
            let world_t = (world_hit - ray.origin).dot(ray.dir);
            if world_t > 0.0 && world_t < nearest {
                nearest = world_t;
                hit = Some(SceneSurfaceHit {
                    entity,
                    point: world_hit,
                    distance: world_t,
                });
            }
        }
    }

    for (entity, skin) in skins {
        if scene.has_component::<PreviewGhost>(entity) {
            continue;
        }
        let Some(mesh_ref) = assets.load_mesh_asset(gpu, skin.mesh) else {
            continue;
        };
        if mesh_ref.cpu_positions.is_empty() || mesh_ref.cpu_skin.is_empty() {
            continue;
        }
        let palette = scene.joint_matrices(&skin);
        if palette.is_empty() {
            continue;
        }
        // Conservative broad-phase: union the bind-pose box through every joint (mirrors the
        // skinned scene-bounds fit). Cheap enough to reject before paying for CPU skinning.
        let mut world_min = Vec3::splat(f32::MAX);
        let mut world_max = Vec3::splat(f32::MIN);
        for joint in &palette {
            world_aabb_from_corners(
                joint,
                mesh_ref.bounds_min,
                mesh_ref.bounds_max,
                &mut world_min,
                &mut world_max,
            );
        }
        if ray_aabb_slab(&ray, world_min, world_max).is_none() {
            continue;
        }
        // Skin every vertex into world space once: deformed = Σ w_k · (palette · pos);
        // matches skin.slang, so picking agrees with what the screen shows.
        let deformed: Vec<Vec3> = mesh_ref
            .cpu_positions
            .iter()
            .zip(&mesh_ref.cpu_skin)
            .map(|(&pos, inf)| {
                let mut acc = Vec3::ZERO;
                for k in 0..4 {
                    let w = inf.weights[k];
                    let j = inf.joints[k] as usize;
                    if w == 0.0 || j >= palette.len() {
                        continue;
                    }
                    acc += w * palette[j].transform_point3(pos);
                }
                acc
            })
            .collect();
        if let Some(t) = nearest_triangle(&ray, &deformed, &mesh_ref.cpu_indices)
            && t < nearest
        {
            nearest = t;
            hit = Some(SceneSurfaceHit {
                entity,
                point: ray.origin + ray.dir * t,
                distance: t,
            });
        }
    }
    hit
}

/// Builds the world-space viewport ray used by picking and placement.
pub fn viewport_ray(viewport: (u32, u32), camera: &CameraView, ndc: Vec2) -> Ray {
    let (width, height) = viewport;
    let aspect = width as f32 / height as f32;
    let mut proj = camera_projection(camera, aspect);
    proj.y_axis.y *= -1.0;
    let inv_view_proj = (proj * camera.view).inverse();
    let near_h = inv_view_proj * Vec4::new(ndc.x, ndc.y, 0.0, 1.0);
    let far_h = inv_view_proj * Vec4::new(ndc.x, ndc.y, 1.0, 1.0);
    let origin = near_h.truncate() / near_h.w;
    Ray {
        origin,
        dir: (far_h.truncate() / far_h.w - origin).normalize(),
    }
}

/// Walks a triangle soup (flat indices into `positions`, already in world space) and reports
/// the nearest forward triangle hit's `t`, if any.
fn nearest_triangle(ray: &Ray, positions: &[Vec3], indices: &[u32]) -> Option<f32> {
    let mut best = f32::MAX;
    let mut found = false;
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (
            positions[tri[0] as usize],
            positions[tri[1] as usize],
            positions[tri[2] as usize],
        );
        if let Some(t) = ray_triangle(ray, a, b, c)
            && t < best
        {
            best = t;
            found = true;
        }
    }
    found.then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    use saffron_rendering::{
        BindlessFreeList, Descriptors, Device, GpuQueue, GpuTexture, SurfaceSource, Uploader,
    };
    use saffron_scene::{AssetEntry, AssetType};

    /// One recorded setter call, in the exact order [`render_scene`] issues them, so a test
    /// asserts the byte-identical setter sequence the skinning gate must preserve.
    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        SpotShadow {
            index: u32,
            casting: bool,
        },
        PointShadow {
            index: u32,
            casting: bool,
            far: f32,
        },
        DirectionalShadow {
            casting: bool,
        },
        RtScene {
            static_count: usize,
        },
        DdgiScene,
        ReflectionProbes(usize),
        SceneLighting {
            light_count: usize,
        },
        EnvBake(EnvSource),
        ClusterCamera,
        SsaoCamera,
        ShowGrid(bool),
        DrawList {
            item_count: usize,
            joint_count: usize,
        },
        Sky {
            mode: u32,
        },
    }

    /// A recording [`SceneRenderer`]: records every setter call and optionally backs the
    /// upload path with a live [`Uploader`] so the draw-list
    /// and pick tests resolve real `Arc<GpuMesh>`. Without a GPU the upload methods are never
    /// reached by the early-out and light tests (no mesh resolves), so those run off-hardware.
    struct RecordingRenderer<'a> {
        width: u32,
        height: u32,
        skinning: bool,
        gpu: Option<(&'a Uploader, &'a Descriptors)>,
        calls: RefCell<Vec<Call>>,
        // The static RT models captured by the last `set_rt_scene`, for the split assert.
        rt_models: RefCell<Vec<Mat4>>,
        draw_items: RefCell<Vec<DrawItem>>,
    }

    impl<'a> RecordingRenderer<'a> {
        fn new(width: u32, height: u32, skinning: bool) -> Self {
            Self {
                width,
                height,
                skinning,
                gpu: None,
                calls: RefCell::new(Vec::new()),
                rt_models: RefCell::new(Vec::new()),
                draw_items: RefCell::new(Vec::new()),
            }
        }

        fn with_gpu(mut self, uploader: &'a Uploader, descriptors: &'a Descriptors) -> Self {
            self.gpu = Some((uploader, descriptors));
            self
        }

        fn calls(&self) -> Vec<Call> {
            self.calls.borrow().clone()
        }
    }

    impl GpuUploader for RecordingRenderer<'_> {
        fn upload_mesh(
            &self,
            mesh: &saffron_geometry::Mesh,
            skin: &[saffron_geometry::VertexSkin],
            morph: Option<&saffron_geometry::MorphData>,
            sdf_bake: Option<&saffron_rendering::SdfBake>,
        ) -> saffron_rendering::Result<Arc<GpuMesh>> {
            let (uploader, descriptors) = self.gpu.expect("upload_mesh needs a GPU fixture");
            uploader.upload_mesh(descriptors, mesh, skin, morph, sdf_bake)
        }

        fn upload_texture(
            &self,
            rgba: &[u8],
            width: u32,
            height: u32,
            srgb: bool,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            let (uploader, descriptors) = self.gpu.expect("upload_texture needs a GPU fixture");
            uploader.upload_texture(descriptors, rgba, width, height, srgb)
        }

        fn upload_texture_float(
            &self,
            rgba: &[f32],
            width: u32,
            height: u32,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            let (uploader, descriptors) =
                self.gpu.expect("upload_texture_float needs a GPU fixture");
            uploader.upload_texture_float(descriptors, rgba, width, height)
        }

        fn skinning_enabled(&self) -> bool {
            self.skinning
        }
    }

    impl SceneRenderer for RecordingRenderer<'_> {
        fn viewport_width(&self) -> u32 {
            self.width
        }
        fn viewport_height(&self) -> u32 {
            self.height
        }
        fn jitter_offset(&self) -> Vec2 {
            Vec2::ZERO
        }
        fn set_spot_shadow(&mut self, _view_proj: Mat4, light_index: u32, casting: bool) {
            self.calls.borrow_mut().push(Call::SpotShadow {
                index: light_index,
                casting,
            });
        }
        fn set_point_shadow(
            &mut self,
            _pos: Vec3,
            far: f32,
            light_index: u32,
            casting: bool,
            _content_key: u64,
        ) {
            self.calls.borrow_mut().push(Call::PointShadow {
                index: light_index,
                casting,
                far,
            });
        }
        fn set_directional_shadow(&mut self, _view_proj: Mat4, casting: bool) {
            self.calls
                .borrow_mut()
                .push(Call::DirectionalShadow { casting });
        }
        fn set_rt_scene(&mut self, models: Vec<Mat4>, _meshes: Vec<Arc<GpuMesh>>) {
            self.calls.borrow_mut().push(Call::RtScene {
                static_count: models.len(),
            });
            *self.rt_models.borrow_mut() = models;
        }
        fn set_ddgi_scene(
            &mut self,
            _cam_pos: Vec3,
            _sun_dir: Vec3,
            _sun_color: Vec3,
            _sun_intensity: f32,
            _sky_color: Vec3,
        ) {
            self.calls.borrow_mut().push(Call::DdgiScene);
        }
        fn set_sdf_scene(&mut self, _instances: &[SdfInstance]) {}
        fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]) {
            self.calls
                .borrow_mut()
                .push(Call::ReflectionProbes(probes.len()));
        }
        fn set_scene_lighting(&mut self, scene: &SceneLighting) -> saffron_rendering::Result<()> {
            self.calls.borrow_mut().push(Call::SceneLighting {
                light_count: scene.lights.len(),
            });
            Ok(())
        }
        fn request_env_bake(
            &mut self,
            source: EnvSource,
            _panorama: Option<Arc<GpuTexture>>,
            _params: SkygenParams,
        ) {
            self.calls.borrow_mut().push(Call::EnvBake(source));
        }
        fn set_cluster_camera(&mut self, _camera: ClusterCamera) {
            self.calls.borrow_mut().push(Call::ClusterCamera);
        }
        fn set_ssao_camera(&mut self, _view: Mat4, _proj: Mat4, _sun: Vec3) {
            self.calls.borrow_mut().push(Call::SsaoCamera);
        }
        fn set_show_grid(&mut self, enabled: bool) {
            self.calls.borrow_mut().push(Call::ShowGrid(enabled));
        }
        fn submit_draw_list(
            &mut self,
            _view_proj: Mat4,
            items: &[DrawItem],
            joints: &[Mat4],
        ) -> saffron_rendering::Result<()> {
            self.calls.borrow_mut().push(Call::DrawList {
                item_count: items.len(),
                joint_count: joints.len(),
            });
            *self.draw_items.borrow_mut() = items.to_vec();
            Ok(())
        }
        fn submit_sky(&mut self, settings: &SkyRenderSettings) {
            self.calls.borrow_mut().push(Call::Sky {
                mode: settings.mode,
            });
        }
    }

    /// A standard test camera (un-flipped projection; `render_scene` applies the Y-flip).
    fn test_camera() -> CameraView {
        CameraView {
            view: Mat4::look_at_rh(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y),
            fov: 45.0,
            near_plane: 0.1,
            far_plane: 100.0,
        }
    }

    fn scratch_server(tag: &str) -> (AssetServer, PathBuf) {
        let tmp =
            std::env::temp_dir().join(format!("saffron-render-scene-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("project").join("assets");
        (AssetServer::new(&root), tmp)
    }

    #[test]
    fn zero_viewport_early_outs_before_any_setter() {
        let mut renderer = RecordingRenderer::new(0, 0, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("zero-viewport");
        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &test_camera(),
            RenderSceneOptions::default(),
        );
        assert!(
            renderer.calls().is_empty(),
            "a zero-size viewport drives no setter"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_scene_drives_the_full_setter_sequence() {
        let mut renderer = RecordingRenderer::new(1280, 720, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("empty-scene");
        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &test_camera(),
            RenderSceneOptions::default(),
        );
        // No lights, no meshes: the shadows are all off, no items, and the procedural sky
        // bake fires (the default environment). The DDGI clipmap snaps to the camera every frame
        // regardless of scene contents (it has no scene-box proxy to gate on). The exact frozen
        // order.
        assert_eq!(
            renderer.calls(),
            vec![
                Call::SpotShadow {
                    index: 0,
                    casting: false
                },
                Call::PointShadow {
                    index: 0,
                    casting: false,
                    far: 1.0
                },
                Call::DirectionalShadow { casting: false },
                Call::RtScene { static_count: 0 },
                Call::DdgiScene,
                Call::ReflectionProbes(0),
                Call::SceneLighting { light_count: 0 },
                Call::EnvBake(EnvSource::Procedural),
                Call::ClusterCamera,
                Call::SsaoCamera,
                Call::ShowGrid(false),
                Call::DrawList {
                    item_count: 0,
                    joint_count: 0
                },
                Call::Sky { mode: 2 },
            ]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn first_lights_drive_the_single_shadow_setters() {
        let mut renderer = RecordingRenderer::new(800, 600, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("lights");

        // A directional, a point, then a spot light — the first of each drives its shadow.
        let dir = scene.create_entity("Sun");
        scene
            .add_component(dir, DirectionalLight::default())
            .unwrap();
        let point = scene.create_entity("Point");
        scene.add_component(point, PointLight::default()).unwrap();
        let spot = scene.create_entity("Spot");
        scene.add_component(spot, SpotLight::default()).unwrap();

        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &test_camera(),
            RenderSceneOptions::default(),
        );
        let calls = renderer.calls();
        // The single point + the single spot drive their shadow setters; the directional
        // shadow stays off (it gates on a non-empty scene AABB / at least one item).
        assert_eq!(
            calls[0],
            Call::SpotShadow {
                index: 1, // the spot is index 1 in the light list (point is 0)
                casting: true
            }
        );
        assert_eq!(
            calls[1],
            Call::PointShadow {
                index: 0,
                casting: true,
                far: PointLight::default().range.max(0.1)
            }
        );
        assert_eq!(calls[2], Call::DirectionalShadow { casting: false });
        // Both punctual lights are in the per-frame light list.
        assert!(calls.contains(&Call::SceneLighting { light_count: 2 }));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn skinning_gate_off_is_byte_identical_to_no_skinning() {
        // A scene carrying a SkinnedMesh entity but no resolvable mesh asset: the skinning
        // gate off must produce the same setter sequence as an empty scene (no skinned items,
        // no joints). Without a GPU the mesh never resolves anyway, so this proves the gate is
        // the only thing that changes — the skinned loop runs only when the gate is on.
        let mut renderer = RecordingRenderer::new(640, 480, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("skin-gate-off");
        let e = scene.create_entity("Rig");
        scene
            .add_component(
                e,
                SkinnedMesh {
                    mesh: saffron_core::Uuid(9000),
                    root_bone: saffron_core::Uuid(0),
                    bones: Vec::new(),
                    inverse_bind: Vec::new(),
                    bone_handles: Vec::new(),
                },
            )
            .unwrap();

        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &test_camera(),
            RenderSceneOptions::default(),
        );
        // The DrawList carries zero items + zero joints (the skinned loop never ran).
        assert!(renderer.calls().contains(&Call::DrawList {
            item_count: 0,
            joint_count: 0
        }));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A live headless GPU fixture, or `None` (no Vulkan ICD) so the GPU-backed draw/pick
    /// tests skip rather than fail off-hardware. Mirrors `load.rs`'s `gpu_or_skip`.
    struct GpuFixture {
        device: Device,
        descriptors: Descriptors,
        uploader: Uploader,
    }

    fn gpu_or_skip() -> Option<GpuFixture> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(std::sync::Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = GpuQueue::new(device.graphics_queue);
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        Some(GpuFixture {
            device,
            descriptors,
            uploader,
        })
    }

    impl GpuFixture {
        fn teardown(self, mut assets: AssetServer) {
            let GpuFixture {
                device,
                descriptors,
                uploader,
            } = self;
            device.wait_idle().expect("idle before teardown");
            assets.clear_asset_caches();
            drop(assets);
            drop(uploader);
            drop(descriptors);
            drop(device);
        }
    }

    /// Writes a standalone `.smesh` of a single forward-facing triangle (centered on the
    /// origin, +Z normal) and registers a Mesh catalog row for `id`.
    fn write_triangle_mesh(assets: &mut AssetServer, id: saffron_core::Uuid, name: &str) {
        use saffron_geometry::glam::Vec2;
        use saffron_geometry::{Mesh, Submesh, Vertex, save_mesh_to_buffer};
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.5, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let rel = format!("models/{name}.smesh");
        let full = format!("{}/{rel}", assets.root.display());
        std::fs::create_dir_all(format!("{}/models", assets.root.display())).unwrap();
        std::fs::write(&full, save_mesh_to_buffer(&mesh, &[], None).unwrap()).unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Mesh,
            path: rel,
            chunk: -1,
            ..AssetEntry::default()
        });
    }

    #[test]
    fn point_shadow_key_is_camera_independent_and_caster_sensitive() {
        // The cache key drives the point-shadow cube reuse: it must be stable for a static scene
        // (no camera term exists to perturb it) yet change when the light or a caster moves.
        let mut scene = Scene::new();
        let e = scene.create_entity("Mesh");
        scene
            .with_component_mut::<Transform, _>(e, |t| t.translation = Vec3::new(1.0, 0.0, 0.0))
            .unwrap();
        scene
            .add_component(
                e,
                MeshComponent {
                    mesh: saffron_core::Uuid(7000),
                },
            )
            .unwrap();
        scene.update_world_transforms();

        let light = Vec3::new(0.0, 5.0, 0.0);
        let k1 = point_shadow_content_key(&mut scene, light, 50.0);
        assert_eq!(
            k1,
            point_shadow_content_key(&mut scene, light, 50.0),
            "key is stable for a static scene (camera-independent)"
        );
        assert_ne!(
            k1,
            point_shadow_content_key(&mut scene, Vec3::new(0.1, 5.0, 0.0), 50.0),
            "a light move invalidates the cube"
        );
        assert_ne!(
            k1,
            point_shadow_content_key(&mut scene, light, 60.0),
            "a far-plane change invalidates the cube"
        );
        scene
            .with_component_mut::<Transform, _>(e, |t| t.translation = Vec3::new(2.0, 0.0, 0.0))
            .unwrap();
        scene.update_world_transforms();
        assert_ne!(
            k1,
            point_shadow_content_key(&mut scene, light, 50.0),
            "a caster move invalidates the cube"
        );
    }

    #[test]
    fn two_mesh_scene_records_two_draw_items_with_world_matrices() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let (mut assets, tmp) = scratch_server("two-mesh");
        write_triangle_mesh(&mut assets, saffron_core::Uuid(5000), "tri");

        let mut scene = Scene::new();
        // Two entities sharing the same mesh, at distinct positions.
        for x in [-3.0_f32, 3.0_f32] {
            let e = scene.create_entity("Mesh");
            scene
                .with_component_mut::<Transform, _>(e, |t| t.translation = Vec3::new(x, 0.0, 0.0))
                .unwrap();
            scene
                .add_component(
                    e,
                    MeshComponent {
                        mesh: saffron_core::Uuid(5000),
                    },
                )
                .unwrap();
            scene
                .add_component(
                    e,
                    saffron_scene::MaterialSet {
                        slots: vec![saffron_scene::MaterialSlot {
                            overrides: serde_json::json!({ "baseColor": [0.2, 0.4, 0.6, 1.0] }),
                            ..saffron_scene::MaterialSlot::default()
                        }],
                    },
                )
                .unwrap();
        }

        {
            let mut renderer =
                RecordingRenderer::new(1024, 768, false).with_gpu(&fx.uploader, &fx.descriptors);
            render_scene(
                &mut renderer,
                &mut scene,
                &mut assets,
                &test_camera(),
                RenderSceneOptions::default(),
            );
            // Two DrawItems, both static; the RT static split carries both models.
            assert!(renderer.calls().contains(&Call::DrawList {
                item_count: 2,
                joint_count: 0
            }));
            assert!(
                renderer
                    .calls()
                    .contains(&Call::RtScene { static_count: 2 })
            );
            // The two DrawItems carry the distinct world matrices + the resolved base color +
            // the directional shadow now casts (a non-empty scene AABB).
            let items = renderer.draw_items.borrow();
            assert_eq!(items.len(), 2);
            let xs: Vec<f32> = items.iter().map(|it| it.model.w_axis.x).collect();
            assert!(xs.contains(&-3.0) && xs.contains(&3.0));
            for it in items.iter() {
                assert!(!it.skinned);
                assert_eq!(
                    it.submesh_materials[0].base_color,
                    Vec4::new(0.2, 0.4, 0.6, 1.0)
                );
            }
            assert!(
                renderer
                    .calls()
                    .contains(&Call::DirectionalShadow { casting: true })
            );
        }

        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn viewport_ray_maps_upper_screen_to_world_up() {
        // The pick convention (viewport UV 0,0 = top-left → y-down NDC): the projection's
        // y is flipped, so an upper-screen point (ndc.y < 0) casts a ray pointing toward world
        // +Y and a lower-screen point (ndc.y > 0) toward world -Y. Guards the double y-flip
        // regression where clicking above an object hit below it (the ray mirrored about center).
        let camera = test_camera(); // eye at +Z, looking down -Z, up +Y.
        let viewport = (1024, 768);
        let up = viewport_ray(viewport, &camera, Vec2::new(0.0, -0.5));
        let center = viewport_ray(viewport, &camera, Vec2::ZERO);
        let down = viewport_ray(viewport, &camera, Vec2::new(0.0, 0.5));
        assert!(up.dir.y > 0.0, "upper screen (ndc.y<0) aims at world +Y");
        assert!(down.dir.y < 0.0, "lower screen (ndc.y>0) aims at world -Y");
        assert!(center.dir.y.abs() < 1e-4, "screen center aims level");
        // The mirrored point about center is the exact opposite tilt, never the same hemisphere.
        assert!(
            up.dir.y > 0.0 && down.dir.y < 0.0 && (up.dir.y + down.dir.y).abs() < 1e-4,
            "the ray is not mirrored about screen center"
        );
    }

    #[test]
    fn pick_hits_a_mesh_through_its_center_and_misses_empty_space() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let (mut assets, tmp) = scratch_server("pick");
        write_triangle_mesh(&mut assets, saffron_core::Uuid(5100), "tri");

        let mut scene = Scene::new();
        let e = scene.create_entity("Tri");
        scene
            .add_component(
                e,
                MeshComponent {
                    mesh: saffron_core::Uuid(5100),
                },
            )
            .unwrap();
        // Flatten the hierarchy so the world matrix the pick reads is current.
        scene.update_world_transforms();

        let renderer =
            RecordingRenderer::new(1024, 768, false).with_gpu(&fx.uploader, &fx.descriptors);
        let camera = test_camera();
        // The triangle straddles the origin; a ray through clip-space center (0,0) hits it.
        let hit = pick_entity(
            &renderer,
            (1024, 768),
            &mut scene,
            &mut assets,
            &camera,
            Vec2::ZERO,
        );
        assert_eq!(hit, e, "a click through the center hits the triangle");

        // A click far in the corner of the loose AABB but outside the triangle misses (the
        // narrow-phase ray-triangle rejects the empty corner).
        let miss = pick_entity(
            &renderer,
            (1024, 768),
            &mut scene,
            &mut assets,
            &camera,
            Vec2::new(-0.99, -0.99),
        );
        assert_eq!(miss, Entity::NULL, "a click into empty space misses");

        drop(renderer);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_scene_flattens_the_hierarchy_before_the_draw_gather() {
        // A parented child whose world matrix is only correct after `update_world_transforms`.
        // `render_scene` must run it once at the top, so reading the child's world matrix after
        // the call (and inside the draw gather, which produced its DrawItem) reflects the
        // parent. Without a GPU the mesh never resolves; the world-matrix read is the proof.
        let mut renderer = RecordingRenderer::new(800, 600, false);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("flatten");

        let parent = scene.create_entity("Parent");
        scene
            .with_component_mut::<Transform, _>(parent, |t| {
                t.translation = Vec3::new(10.0, 0.0, 0.0)
            })
            .unwrap();
        let child = scene.create_entity("Child");
        scene.set_parent(child, Some(parent), false).unwrap();
        scene
            .with_component_mut::<Transform, _>(child, |t| t.translation = Vec3::new(0.0, 2.0, 0.0))
            .unwrap();

        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &test_camera(),
            RenderSceneOptions::default(),
        );
        // The flatten ran: the child's cached world translation composes the parent's.
        assert_eq!(
            scene.world_translation(child),
            Vec3::new(10.0, 2.0, 0.0),
            "render_scene must flatten the hierarchy once before any reader"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Writes a skinned `.smesh` (a one-bone triangle, all weight on joint 0) + a Mesh row.
    fn write_skinned_triangle(assets: &mut AssetServer, id: saffron_core::Uuid, name: &str) {
        use saffron_geometry::glam::Vec2;
        use saffron_geometry::{Mesh, Submesh, Vertex, VertexSkin, save_mesh_to_buffer};
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::new(-1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(1.0, -1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::new(0.0, 1.0, 0.0),
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.5, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        // All weight on joint 0 so CPU skinning by an identity palette is the rest pose.
        let skin = vec![
            VertexSkin {
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            };
            3
        ];
        let rel = format!("models/{name}.smesh");
        std::fs::create_dir_all(format!("{}/models", assets.root.display())).unwrap();
        std::fs::write(
            format!("{}/{rel}", assets.root.display()),
            save_mesh_to_buffer(&mesh, &skin, None).unwrap(),
        )
        .unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Mesh,
            path: rel,
            chunk: -1,
            ..AssetEntry::default()
        });
    }

    /// Builds a one-bone skinned entity (bone at the origin, inverse-bind identity) and runs
    /// the relink so `bone_handles` resolves. Returns the skinned entity.
    fn spawn_one_bone_skin(scene: &mut Scene, mesh_id: saffron_core::Uuid) -> Entity {
        let bone = scene.create_entity("Bone");
        let bone_uuid = scene
            .component::<saffron_scene::IdComponent>(bone)
            .unwrap()
            .id;
        let e = scene.create_entity("Rig");
        scene
            .add_component(
                e,
                SkinnedMesh {
                    mesh: mesh_id,
                    root_bone: bone_uuid,
                    bones: vec![bone_uuid],
                    inverse_bind: vec![Mat4::IDENTITY],
                    bone_handles: Vec::new(),
                },
            )
            .unwrap();
        scene.relink_hierarchy();
        e
    }

    #[test]
    fn skinning_gate_on_produces_an_identity_model_skinned_item_split_from_rt() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let (mut assets, tmp) = scratch_server("skin-on");
        write_skinned_triangle(&mut assets, saffron_core::Uuid(5300), "rig");

        let mut scene = Scene::new();
        let skinned = spawn_one_bone_skin(&mut scene, saffron_core::Uuid(5300));
        // A static mesh too, so the RT split has one of each.
        write_triangle_mesh(&mut assets, saffron_core::Uuid(5301), "tri");
        let stat = scene.create_entity("Static");
        scene
            .add_component(
                stat,
                MeshComponent {
                    mesh: saffron_core::Uuid(5301),
                },
            )
            .unwrap();

        {
            let mut renderer =
                RecordingRenderer::new(1024, 768, true).with_gpu(&fx.uploader, &fx.descriptors);
            render_scene(
                &mut renderer,
                &mut scene,
                &mut assets,
                &test_camera(),
                RenderSceneOptions::default(),
            );
            // Two items (one static, one skinned), three joints? no — one joint (one bone).
            assert!(renderer.calls().contains(&Call::DrawList {
                item_count: 2,
                joint_count: 1
            }));
            // The RT static split carries only the one static item (the skinned one is excluded).
            assert!(
                renderer
                    .calls()
                    .contains(&Call::RtScene { static_count: 1 })
            );
            let items = renderer.draw_items.borrow();
            let skinned_item = items.iter().find(|it| it.skinned).expect("a skinned item");
            assert_eq!(
                skinned_item.model,
                Mat4::IDENTITY,
                "skinned model is identity"
            );
            assert_eq!(skinned_item.joint_count, 1);
            assert_eq!(skinned_item.joint_offset, 0);
            assert_eq!(skinned_item.entity, entity_id_or_zero(&scene, skinned));
        }

        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pick_resolves_a_skinned_mesh_against_a_fresh_joint_palette() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let (mut assets, tmp) = scratch_server("pick-skin");
        write_skinned_triangle(&mut assets, saffron_core::Uuid(5400), "rig");

        let mut scene = Scene::new();
        let e = spawn_one_bone_skin(&mut scene, saffron_core::Uuid(5400));
        scene.update_world_transforms();

        let renderer =
            RecordingRenderer::new(1024, 768, false).with_gpu(&fx.uploader, &fx.descriptors);
        let camera = test_camera();
        // The bone is at the origin with identity inverse-bind, so the rest triangle straddles
        // the origin; a center ray skins each vertex through the (identity) palette and hits.
        let hit = pick_entity(
            &renderer,
            (1024, 768),
            &mut scene,
            &mut assets,
            &camera,
            Vec2::ZERO,
        );
        assert_eq!(hit, e, "the skinned triangle picks against a fresh palette");

        drop(renderer);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The borrow-shape proof: `render_scene` takes `&Scene` (read), `&mut AssetServer`
    /// (cache fill), and `&mut R: SceneRenderer` (setters) as three disjoint borrows of three
    /// distinct values — no `RefCell`, no interior mutability on the engine state. This
    /// compiles, which *is* the assertion; the body just exercises the call once.
    #[test]
    fn disjoint_three_value_borrow_shape_compiles() {
        let mut renderer = RecordingRenderer::new(320, 240, true);
        let mut scene = Scene::new();
        let (mut assets, tmp) = scratch_server("borrow");
        // Three distinct values, three distinct mutable/shared borrows, one call.
        render_scene(
            &mut renderer,
            &mut scene,
            &mut assets,
            &test_camera(),
            RenderSceneOptions::default(),
        );
        assert!(!renderer.calls().is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
