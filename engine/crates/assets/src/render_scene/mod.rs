//! `render_scene` — the engine's highest-coupling driver — and its read-side twin
//! [`pick_scene_surface`].
//!
//! [`render_scene`] translates a scene + camera into the renderer's draw list plus every
//! per-frame lighting / shadow / GI / sky / RT / cluster / SSAO setter; [`pick_scene_surface`]
//! ray-casts the same scene to find the clicked entity. Both read the same last-frame
//! world-transform flatten the draw loop writes ([`Scene::update_world_transforms`]),
//! rebuilding the joint palette fresh.
//!
//! The driver takes `&mut Scene`, `&mut AssetServer` and `&mut R: SceneRenderer` — three
//! distinct values, so the borrows stay disjoint without interior mutability. Because
//! `for_each` borrows the ECS world mutably while the world-transform readers borrow it
//! immutably, each loop first gathers entity handles plus their `Copy` component data
//! through `for_each`, then reads the cached world transforms in a second pass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use saffron_geometry::glam::{Mat4, Vec2, Vec3, Vec4};
use saffron_geometry::{
    Ray, Vertex, ray_aabb_slab, ray_triangle_coordinates, world_aabb_from_corners,
};
use saffron_rendering::{
    CloudRenderSettings, ClusterCamera, CoverageSourceKind, EnvSource, FOG_SHAPE_BOX,
    FOG_SHAPE_SPHERE, FogRenderSettings, FogVolumeUpload, GpuLight, GpuMesh, MAX_FOG_VOLUMES,
    MAX_REFLECTION_PROBES, ReflectionProbeUpload, SceneLighting, SkyRenderSettings, SkygenParams,
};
use saffron_scene::{
    AtmosphereRole, Camera, CameraView, DirectionalLight, Entity, FogShape, FogVolume, IdComponent,
    MaterialSet, MaterialSlot, Mesh as MeshComponent, MorphComponent, MorphWeightOverride,
    PlantOrigin, PointLight, PreviewGhost, ReflectionProbe, Relationship, Scene, SkinnedMesh,
    SkyMode, SpotLight, Transform, camera_projection,
};
use saffron_spatial::{
    FieldChannel, FieldDerivative, FieldSample, SurfaceCapabilities, SurfaceCoordinates,
    SurfaceField, SurfaceFrame, SurfaceHit, SurfaceProviderDescriptor, SurfaceProviderId,
    SurfaceRay, SurfaceRevision, SurfaceTagId, UnitInterval, WeightedSurfaceTag, WorldBounds,
    WorldPosition,
};
use saffron_vegetation::{AlphaClassification, CoverageSource, MaterialSurface};

use crate::gpu::GpuUploader;
use crate::time_of_day::{
    CelestialTime, dir_from_az_el, eval_monotone_curve, julian_date, lunar_position,
    solar_position, world_from_equatorial,
};
use crate::{
    AssetServer, CanonicalCpuCoverage, MaterialAsset, RenderSceneOptions, StaticMeshSurfaceInput,
    StaticMeshSurfaceProvider,
};

mod celestial;
mod frame;
mod gather;
mod pick;

#[cfg(test)]
mod test_support;

pub use frame::render_scene;
pub use pick::{
    pick_scene_surface, query_scene_surface_ray, sample_scene_surface_field,
    scene_surface_field_snapshots, scene_surface_providers, viewport_ray,
};

pub(crate) use gather::{gpu_point_light, gpu_spot_light};

use celestial::{CelestialDirectionOverrides, drive_time_of_day};
use gather::{
    DirectionalResolved, FrameSceneBuild, drive_env_bake, gather_directional_lights,
    gather_fog_volumes, gather_punctual_lights, gather_reflection_probes,
    gather_skinned_frame_facts, gather_static_frame_facts,
};

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

/// A `glm::lookAt`-equivalent view matrix (right-handed, the engine's GLM convention).
fn look_at(eye: Vec3, center: Vec3, up: Vec3) -> Mat4 {
    Mat4::look_at_rh(eye, center, up)
}

/// A `glm::perspective` with Vulkan `[0, 1]` clip depth (`GLM_FORCE_DEPTH_ZERO_TO_ONE`).
fn perspective(fov: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    Mat4::perspective_rh(fov, aspect, near, far)
}

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
    /// Arms the point shadow pass (the six-face cube re-renders every active frame
    /// from the executor stream).
    fn set_point_shadow(
        &mut self,
        light_pos: Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
    );
    /// Arms the directional shadow pass.
    fn set_directional_shadow(&mut self, casting: bool);
    /// Captures the frame's static RT instances.
    fn set_rt_scene(&mut self, instances: Arc<[saffron_rendering::RtInstanceInput]>);
    /// Snaps the camera-centered DDGI probe clipmap to the camera + passes the sun for the trace.
    fn set_ddgi_scene(&mut self, cam_pos: Vec3, sun_dir: Vec3, sun_color: Vec3, sun_intensity: f32);
    /// Records ray instances dropped for sitting outside the reachable GI window.
    fn record_rt_culled(&mut self, culled: u32);
    /// Folds the frame's reflection-probe uploads in.
    fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]);
    /// Folds the frame's local fog-volume uploads in (injected into the froxel grid).
    fn submit_fog_volumes(&mut self, volumes: &[FogVolumeUpload]);
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
    /// Records the frame scene gather duration.
    fn record_scene_gather(&mut self, elapsed: Duration, entities: u32);
    /// Whether displacement tessellation is built and on; the gather derives displace
    /// facts only when it is.
    fn displacement_enabled(&self) -> bool {
        true
    }
    /// Submits the frame's record-driven deformation work + concatenated joint palette
    /// (the executor draws come from the GPU scene's visibility traversal, not a list).
    ///
    /// # Errors
    ///
    /// Returns a [`saffron_rendering::Error`] if a palette / deformed-buffer grow or
    /// dispatch wiring fails.
    fn submit_deformations(
        &mut self,
        view_proj: Mat4,
        work: &[saffron_rendering::DeformationWork],
        joints: &[Mat4],
    ) -> saffron_rendering::Result<()>;
    /// Pushes the submitted frame's skinned palette/deformed offsets into the mirror's
    /// stable deformation-provider params (a no-op for a renderer without the GPU
    /// scene).
    fn patch_frame_deformations(&mut self, _scene: &Scene, _mirror: &crate::GpuSceneMirror) {}
    /// Folds the visible-sky settings in.
    fn submit_sky(&mut self, settings: &SkyRenderSettings);
    /// Folds the cloud shape and resolved painted-weather source in.
    fn submit_clouds(&mut self, settings: CloudRenderSettings);
    /// Sets the tonemap exposure in EV.
    fn set_exposure(&mut self, ev: f32);
    /// Sets the mesopic/scotopic adaptation strength.
    fn set_night_factor(&mut self, factor: f32);
    /// Folds the analytic height/distance fog settings in.
    fn submit_fog(&mut self, settings: &FogRenderSettings);
}

/// A shared surface hit paired with the scene entity that publishes the provider.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSurfaceHit {
    /// The entity whose mesh was hit.
    pub entity: Entity,
    /// The complete shared surface result.
    pub surface: SurfaceHit,
    /// Provider capabilities at the sampled revision.
    pub capabilities: SurfaceCapabilities,
}

/// A surface provider paired with its scene entity for inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneSurfaceProvider {
    /// The entity publishing the provider.
    pub entity: Entity,
    /// Stable provider metadata.
    pub descriptor: SurfaceProviderDescriptor,
}

/// The live-renderer [`SceneRenderer`]: a `&mut Renderer` (the setter target) plus a
/// borrowed [`Uploader`](saffron_rendering::Uploader) for the asset-resolve uploads.
///
/// The renderer owns no uploader (the host constructs one alongside it), so the adapter
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
        hierarchy: &saffron_geometry::PortableVirtualHierarchy,
        skin: &[saffron_geometry::VertexSkin],
        morph: Option<&saffron_geometry::MorphData>,
        sdf: saffron_rendering::SdfSource<'_>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        self.uploader.upload_mesh(
            self.renderer.descriptors(),
            mesh,
            hierarchy,
            skin,
            morph,
            sdf,
        )
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

    fn upload_texture_mips(
        &self,
        mips: &[saffron_rendering::TextureMipLevel<'_>],
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuTexture>> {
        self.uploader
            .upload_texture_mips(self.renderer.descriptors(), mips, srgb)
    }

    fn upload_height_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuTexture>> {
        self.uploader
            .upload_height_texture(self.renderer.descriptors(), rgba, width, height)
    }

    fn upload_lut_3d(
        &self,
        rgb: &[[f32; 3]],
        size: u32,
    ) -> saffron_rendering::Result<Arc<saffron_rendering::GpuLut>> {
        self.uploader.upload_lut_3d(rgb, size)
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
    ) {
        self.renderer
            .set_point_shadow(light_pos, far_plane, light_index, casting);
    }

    fn set_directional_shadow(&mut self, casting: bool) {
        self.renderer.set_directional_shadow(casting);
    }

    fn set_rt_scene(&mut self, instances: Arc<[saffron_rendering::RtInstanceInput]>) {
        self.renderer.set_rt_scene(instances);
    }

    fn set_ddgi_scene(
        &mut self,
        cam_pos: Vec3,
        sun_dir: Vec3,
        sun_color: Vec3,
        sun_intensity: f32,
    ) {
        self.renderer
            .set_ddgi_scene(cam_pos, sun_dir, sun_color, sun_intensity);
    }

    fn record_rt_culled(&mut self, culled: u32) {
        self.renderer.record_rt_culled(culled);
    }

    fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]) {
        self.renderer.submit_reflection_probes(probes);
    }

    fn submit_fog_volumes(&mut self, volumes: &[FogVolumeUpload]) {
        self.renderer.submit_fog_volumes(volumes);
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

    fn record_scene_gather(&mut self, elapsed: Duration, entities: u32) {
        self.renderer.record_scene_gather(elapsed, entities);
    }

    fn patch_frame_deformations(&mut self, scene: &Scene, mirror: &crate::GpuSceneMirror) {
        let world = self.renderer.active_gpu_scene_world();
        let deformations = self.renderer.skinned_deformations().to_vec();
        let (_, gpu_scene, pending, _) = self.renderer.gpu_scene_parts_mut();
        let deformed = mirror.patch_frame_deformations(world, scene, &deformations, pending);
        gpu_scene.note_instances_moved(world, &deformed);
        self.renderer
            .record_retained_mesh_bytes(mirror.retained_mesh_cpu_bytes());
    }

    fn displacement_enabled(&self) -> bool {
        self.renderer.displacement_enabled()
    }

    fn submit_deformations(
        &mut self,
        view_proj: Mat4,
        work: &[saffron_rendering::DeformationWork],
        joints: &[Mat4],
    ) -> saffron_rendering::Result<()> {
        self.renderer
            .submit_gpu_scene_deformations(view_proj, work, joints)
    }

    fn submit_sky(&mut self, settings: &SkyRenderSettings) {
        self.renderer.submit_sky(settings);
    }

    fn submit_clouds(&mut self, settings: CloudRenderSettings) {
        self.renderer.submit_clouds(settings);
    }

    fn set_exposure(&mut self, ev: f32) {
        self.renderer.set_exposure(ev);
    }

    fn set_night_factor(&mut self, factor: f32) {
        self.renderer.set_night_factor(factor);
    }

    fn submit_fog(&mut self, settings: &FogRenderSettings) {
        self.renderer.set_fog(settings);
    }
}

/// Reconciles the runtime-only editor-camera gizmo entities: every [`Camera`] with
/// `show_model` owns one [`PreviewGhost`]-tagged child rendering the reserved
/// editor-camera model mesh ([`crate::EDITOR_CAMERA_MESH_ID`], seeded on first
/// resolve); clearing the flag — or disabling the option — removes the child. A ghost
/// never serializes, lists, or picks, so the gizmo is pure runtime state that renders
/// through the ordinary GPU-scene mirror like any entity.
fn sync_editor_camera_models(scene: &mut Scene, enabled: bool) {
    const MODEL_SCALE: f32 = 7.5;
    const LENS_LOCAL_X: f32 = 0.080_121_7;

    let mut wanted: Vec<Entity> = Vec::new();
    if enabled {
        scene.for_each::<(&Transform, &Camera), _>(|entity, (_, camera)| {
            if camera.show_model {
                wanted.push(entity);
            }
        });
    }
    let mut existing: Vec<Entity> = Vec::new();
    scene.for_each::<(&PreviewGhost, &MeshComponent), _>(|entity, (_, mesh)| {
        if mesh.mesh == crate::EDITOR_CAMERA_MESH_ID {
            existing.push(entity);
        }
    });
    let mut covered: Vec<Entity> = Vec::new();
    for ghost in existing {
        let parent = scene
            .with_component::<Relationship, _>(ghost, |rel| rel.parent_handle)
            .unwrap_or(None);
        match parent {
            // One ghost per camera: extras and orphans reconcile away.
            Some(camera) if wanted.contains(&camera) && !covered.contains(&camera) => {
                covered.push(camera);
            }
            _ => scene.destroy_entity(ghost),
        }
    }
    if wanted.iter().all(|camera| covered.contains(camera)) {
        return;
    }
    // The gizmo's local offset under its camera parent, as one TRS (the rotation is a
    // pure +90° yaw, so the Euler triple is exact).
    let lens = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2).transform_vector3(Vec3::new(
        -LENS_LOCAL_X * MODEL_SCALE,
        0.0,
        0.0,
    ));
    let local = Transform {
        translation: Vec3::new(0.0, -0.1, 0.0) + lens,
        scale: Vec3::splat(MODEL_SCALE),
        rotation: Vec3::new(0.0, std::f32::consts::FRAC_PI_2, 0.0),
    };
    for camera in wanted {
        if covered.contains(&camera) {
            continue;
        }
        let ghost = scene.create_entity("Camera Model");
        let _ = scene.add_component(ghost, PreviewGhost::default());
        let _ = scene.add_component(
            ghost,
            MeshComponent {
                mesh: crate::EDITOR_CAMERA_MESH_ID,
            },
        );
        let _ = scene.add_component(
            ghost,
            MaterialSet {
                slots: vec![MaterialSlot {
                    material: crate::EDITOR_CAMERA_MATERIAL_ID,
                    ..MaterialSlot::default()
                }],
            },
        );
        let _ = scene.with_component_mut::<Transform, _>(ghost, |transform| *transform = local);
        if let Err(err) = scene.set_parent(ghost, Some(camera), false) {
            tracing::warn!("camera gizmo parent: {err}");
        }
    }
}

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
