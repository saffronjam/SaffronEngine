//! The live [`ControlRenderer`] the host hands the control plane each frame.
//!
//! The control crate defines the trait but cannot implement it for the bare
//! [`Renderer`]: the GPU-upload seam ([`ControlRenderer::with_gpu_uploader`]) needs the
//! host-owned one-off [`Uploader`] (the renderer owns none — the host constructs one
//! alongside it, `layer.rs`). So the concrete impl lives here, on a wrapper that bundles
//! `&mut Renderer` with `&Uploader` for one frame's control drain and is dropped at the
//! end of it.
//!
//! The render-domain query/toggle methods delegate straight to [`Renderer`]; the
//! view-select / screenshot / wait-idle methods route the matching `Renderer` entry
//! points; and [`ControlRenderer::with_gpu_uploader`] builds a transient
//! [`RendererUploader`] over the bundled uploader + the renderer's descriptors and hands
//! it to the asset loaders (`import_texture`, `load_mesh_asset`, `resolve_material_asset`,
//! `pick_entity`, …) for the call's duration.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use saffron_assets::{GpuUploader, RendererUploader, ThumbnailGpu, ThumbnailPng};
use saffron_control::ControlRenderer;
use saffron_geometry::{Mesh, VertexSkin};
use saffron_rendering::{
    ActiveAlarm, AlarmDrain, CaptureMode, CaptureState, Descriptors, Device, FrameHistoryStats,
    FrameSample, GpuMesh, GpuQueue, GpuTexture, PassTiming, PerfConfig, PngTransfer,
    ProfileCapture, ProfilerMode, ReflectionProbe, RenderStatsFull, Renderer, SubmeshMaterial,
    ThumbnailRenderer, Uploader, ViewId, ViewMode,
};
use serde_json::Value;

/// The host's live renderer seam: the renderer plus the host-owned uploader, bundled for
/// one control-plane drain.
///
/// `skinning_enabled` is captured once at construction so the upload seam reports the same
/// gate the scene render uses this frame.
pub struct HostControlRenderer<'a> {
    renderer: &'a mut Renderer,
    uploader: &'a Uploader,
    skinning_enabled: bool,
}

impl<'a> HostControlRenderer<'a> {
    /// Bundles the renderer + the host-owned uploader for a control drain.
    pub fn new(renderer: &'a mut Renderer, uploader: &'a Uploader) -> Self {
        let skinning_enabled = renderer.skinning_enabled();
        Self {
            renderer,
            uploader,
            skinning_enabled,
        }
    }
}

impl ControlRenderer for HostControlRenderer<'_> {
    fn render_stats(&self) -> RenderStatsFull {
        self.renderer.render_stats()
    }

    fn clustered_enabled(&self) -> bool {
        self.renderer.clustered_enabled()
    }
    fn set_clustered(&mut self, enabled: bool) {
        self.renderer.set_clustered(enabled);
    }
    fn depth_prepass_enabled(&self) -> bool {
        self.renderer.depth_prepass_enabled()
    }
    fn set_depth_prepass(&mut self, enabled: bool) {
        self.renderer.set_depth_prepass(enabled);
    }
    fn shadows_enabled(&self) -> bool {
        self.renderer.shadows_enabled()
    }
    fn set_shadows(&mut self, enabled: bool) {
        self.renderer.set_shadows(enabled);
    }
    fn ibl_enabled(&self) -> bool {
        self.renderer.ibl_enabled()
    }
    fn set_ibl(&mut self, enabled: bool) {
        self.renderer.set_ibl(enabled);
    }
    fn ssao_enabled(&self) -> bool {
        self.renderer.ssao_enabled()
    }
    fn contact_shadows_enabled(&self) -> bool {
        self.renderer.contact_shadows_enabled()
    }
    fn ssgi_enabled(&self) -> bool {
        self.renderer.ssgi_enabled()
    }
    fn render_quality_tier(&self) -> String {
        self.renderer.render_quality().tier.as_str().to_owned()
    }
    fn render_scale(&self) -> f32 {
        self.renderer.active_render_scale()
    }
    fn set_render_scale(&mut self, scale: f32) {
        if let Err(err) = self.renderer.set_active_render_scale(scale) {
            tracing::error!("set_render_scale failed: {err}");
        }
    }
    fn input_extent(&self) -> (u32, u32) {
        let e = self.renderer.active_view().scaled_render_extent();
        (e.width, e.height)
    }
    fn display_extent(&self) -> (u32, u32) {
        let e = self.renderer.active_view().published_extent();
        (e.width, e.height)
    }
    fn set_render_quality(&mut self, tier: &str) -> bool {
        match saffron_rendering::QualityTier::from_name(tier) {
            Some(tier) => {
                self.renderer.set_render_quality(tier.resolve());
                true
            }
            None => false,
        }
    }
    fn tonemap_mode(&self) -> String {
        self.renderer.tonemap_mode().as_str().to_owned()
    }
    fn set_tonemap(&mut self, mode: &str) -> bool {
        match saffron_rendering::TonemapMode::from_name(mode) {
            Some(mode) => {
                self.renderer.set_tonemap_mode(mode);
                true
            }
            None => false,
        }
    }
    fn reactive_idle(&self) -> bool {
        self.renderer.reactive_idle()
    }
    fn reactive_converged(&self) -> bool {
        self.renderer.reactive_converged()
    }
    fn redraw_reasons(&self) -> Vec<String> {
        self.renderer.reactive_reasons().to_vec()
    }
    fn power_state(&self) -> String {
        self.renderer.power_state().as_str().to_owned()
    }
    fn set_viewport_power_state(&mut self, state: &str) -> bool {
        match saffron_rendering::PowerState::from_name(state) {
            Some(state) => {
                self.renderer.set_power_state(state);
                true
            }
            None => false,
        }
    }
    fn ddgi_enabled(&self) -> bool {
        self.renderer.ddgi_enabled()
    }
    fn set_ddgi(&mut self, enabled: bool) {
        self.renderer.set_ddgi(enabled);
    }
    fn sky_occlusion_enabled(&self) -> bool {
        self.renderer.sky_occlusion_enabled()
    }
    fn set_sky_occlusion(&mut self, enabled: bool) {
        self.renderer.set_sky_occlusion(enabled);
    }
    fn gdf_enabled(&self) -> bool {
        self.renderer.gdf_enabled()
    }
    fn set_gdf(&mut self, enabled: bool) {
        self.renderer.set_gdf(enabled);
    }
    fn reflection_probes_enabled(&self) -> bool {
        self.renderer.reflection_probes_enabled()
    }
    fn set_reflection_probes(&mut self, enabled: bool) {
        self.renderer.set_reflection_probes(enabled);
    }
    fn reflection_probes(&self) -> Vec<ReflectionProbe> {
        self.renderer.reflection_probes().to_vec()
    }
    fn skinning_enabled(&self) -> bool {
        self.renderer.skinning_enabled()
    }
    fn set_skinning(&mut self, enabled: bool) {
        self.renderer.set_skinning(enabled);
    }
    fn displacement_enabled(&self) -> bool {
        self.renderer.displacement_enabled()
    }
    fn set_displacement(&mut self, enabled: bool) {
        self.renderer.set_displacement(enabled);
    }

    fn rt_supported(&self) -> bool {
        self.renderer.rt_supported()
    }
    fn rt_shadows_enabled(&self) -> bool {
        self.renderer.rt_shadows_enabled()
    }
    fn set_rt_shadows(&mut self, enabled: bool) {
        self.renderer.set_rt_shadows(enabled);
    }
    fn restir_enabled(&self) -> bool {
        self.renderer.restir_enabled()
    }
    fn set_restir(&mut self, enabled: bool) {
        self.renderer.set_restir(enabled);
    }
    fn ssr_enabled(&self) -> bool {
        self.renderer.ssr_enabled()
    }
    fn set_ssr(&mut self, enabled: bool) {
        self.renderer.set_ssr(enabled);
    }
    fn rt_reflections_enabled(&self) -> bool {
        self.renderer.rt_reflections_enabled()
    }
    fn set_rt_reflections(&mut self, enabled: bool) {
        self.renderer.set_rt_reflections(enabled);
    }
    fn rt_blas_count(&self) -> u32 {
        self.renderer.rt_blas_count()
    }

    fn pipeline_count(&self) -> u32 {
        self.renderer.pipeline_count()
    }
    fn bindless_texture_count(&self) -> u32 {
        self.renderer.bindless_texture_count()
    }
    fn bindless_free_count(&self) -> u32 {
        self.renderer.bindless_free_count()
    }

    fn view_mode(&self) -> ViewMode {
        self.renderer.view_mode()
    }
    fn set_view_mode(&mut self, mode: ViewMode) {
        self.renderer.set_view_mode(mode);
    }

    fn aa_mode(&self) -> String {
        self.renderer.aa_mode()
    }
    fn set_aa(&mut self, samples: u32, fxaa: bool, taa: bool) -> Result<(), String> {
        self.renderer
            .set_aa(samples, fxaa, taa)
            .map_err(|e| e.to_string())
    }

    fn taa_params(&self) -> saffron_protocol::TaaParamsDto {
        let p = self.renderer.taa_params();
        saffron_protocol::TaaParamsDto {
            feedback_min: p.feedback_min,
            feedback_max: p.feedback_max,
            velocity_rejection: p.velocity_rejection,
            clip_gamma: p.clip_gamma,
            sharpness: p.sharpness,
        }
    }
    fn set_taa_params(&mut self, params: saffron_protocol::TaaParamsDto) {
        // The DTO carries the five wire-exposed knobs; the reconstruction-robustness knobs
        // (lock lifetime, reactive scale, disocclusion / lock-break thresholds) are renderer-side
        // and preserved across a wire update by overlaying onto the current state.
        let mut cur = self.renderer.taa_params();
        cur.feedback_min = params.feedback_min;
        cur.feedback_max = params.feedback_max;
        cur.velocity_rejection = params.velocity_rejection;
        cur.clip_gamma = params.clip_gamma;
        cur.sharpness = params.sharpness;
        self.renderer.set_taa_params(cur);
    }

    fn exposure_ev(&self) -> f32 {
        self.renderer.exposure_ev()
    }
    fn set_exposure(&mut self, ev: f32) {
        self.renderer.set_exposure(ev);
    }

    fn profiler_mode(&self) -> ProfilerMode {
        self.renderer.profiler_mode()
    }
    fn set_profiler_mode(&mut self, mode: ProfilerMode) {
        self.renderer.set_profiler_mode(mode);
    }
    fn profiler_timestamps_supported(&self) -> bool {
        self.renderer.profiler_timestamps_supported()
    }
    fn profiler_pipeline_stats_supported(&self) -> bool {
        self.renderer.profiler_pipeline_stats_supported()
    }
    fn pass_timings(&self) -> Vec<PassTiming> {
        self.renderer.pass_timings().to_vec()
    }
    fn pass_timings_total_ms(&self) -> f32 {
        self.renderer.pass_timings_total_ms()
    }

    fn start_profile_capture(
        &mut self,
        mode: CaptureMode,
        frames: u32,
        filter: String,
        include_cpu: bool,
        include_stats: bool,
    ) -> u32 {
        self.renderer
            .start_profile_capture(mode, frames, filter, include_cpu, include_stats)
    }
    fn stop_profile_capture(&mut self) -> ProfileCapture {
        self.renderer.stop_profile_capture()
    }
    fn profile_capture_mode(&self) -> CaptureMode {
        self.renderer.profile_capture_mode()
    }
    fn profile_capture_state(&self) -> CaptureState {
        self.renderer.profile_capture_state()
    }
    fn profile_capture_captured_frames(&self) -> u32 {
        self.renderer.profile_capture_captured_frames()
    }
    fn profile_capture_target_frames(&self) -> u32 {
        self.renderer.profile_capture_target_frames()
    }

    fn frame_history_stats(&self) -> FrameHistoryStats {
        self.renderer.frame_history_stats()
    }
    fn frame_samples(&self, max_samples: u32) -> Vec<FrameSample> {
        self.renderer.frame_samples(max_samples)
    }
    fn reset_frame_telemetry(&mut self) {
        self.renderer.reset_frame_telemetry();
    }
    fn perf_config(&self) -> PerfConfig {
        self.renderer.perf_config()
    }
    fn set_perf_config(&mut self, config: PerfConfig) {
        self.renderer.set_perf_config(config);
    }

    fn drain_alarms(&self, since: u64) -> AlarmDrain {
        self.renderer.drain_alarms(since)
    }
    fn active_alarms(&self) -> Vec<ActiveAlarm> {
        self.renderer.active_alarms().to_vec()
    }

    fn viewport_width(&self) -> u32 {
        self.renderer.viewport_width()
    }
    fn viewport_height(&self) -> u32 {
        self.renderer.viewport_height()
    }

    fn software_gpu(&self) -> bool {
        self.renderer.software_gpu()
    }

    fn wait_gpu_idle(&mut self) {
        let _ = self.renderer.device().wait_idle();
    }

    fn set_active_view(&mut self, view: ViewId) {
        self.renderer.set_active_view(view);
    }
    fn view_desired_size(&self, view: ViewId) -> (u32, u32) {
        (
            self.renderer.view_desired_width(view),
            self.renderer.view_desired_height(view),
        )
    }
    fn set_view_desired_size(
        &mut self,
        view: ViewId,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        self.renderer
            .set_viewport_desired_size(view, width, height)
            .map_err(|e| e.to_string())
    }

    fn capture_viewport(&mut self, path: &Path) -> Result<(), String> {
        self.renderer
            .capture_viewport(path)
            .map_err(|e| e.to_string())
    }

    fn request_window_capture(&mut self, path: &Path) -> Result<(), String> {
        // Arms the swapchain (composited window output) capture for the next present.
        // Distinct from `capture_viewport`'s offscreen path.
        self.renderer
            .request_window_capture(path)
            .map_err(|e| e.to_string())
    }

    fn with_gpu_uploader(&mut self, with: &mut dyn FnMut(&dyn GpuUploader)) {
        let gpu = RendererUploader::new(
            self.uploader,
            self.renderer.descriptors(),
            self.skinning_enabled,
        );
        with(&gpu);
    }

    fn with_thumbnail_gpu(&mut self, with: &mut dyn FnMut(&dyn ThumbnailGpu)) {
        let gpu = HostThumbnailGpu {
            renderer: RefCell::new(&mut *self.renderer),
            uploader: self.uploader,
            skinning_enabled: self.skinning_enabled,
        };
        with(&gpu);
    }

    fn render_settings_to_json(&self) -> Value {
        self.renderer.render_settings_to_json()
    }

    fn apply_render_settings(&mut self, settings: &Value) {
        self.renderer.apply_render_settings(settings);
    }

    fn sa_lua_defs(&self) -> String {
        SA_LUA_DEFS.to_owned()
    }
}

/// The generated `sa.*` LuaLS type defs written into every project's `library/` on
/// create/open.
///
/// The committed `schemas/control/sa.generated.luau` is the single source — emitted by
/// `xtask gen-protocol` from the `saffron-script` binding table (the `sa.*` API surface) plus
/// the registered-component wire shapes (the `:get_component` snapshots). Embedding the
/// committed file matches the `@saffron/protocol` discipline: the gate's regen-freshness diff
/// keeps it in lockstep with the live bindings, so the def file the editor type-checks against
/// can never silently drift.
const SA_LUA_DEFS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../schemas/control/sa.generated.luau"
));

/// The host's [`ThumbnailGpu`] seam: the live upload trio (the host-owned [`Uploader`] +
/// the renderer's bindless descriptors) plus the live offscreen render-to-PNG /
/// material-preview primitives ([`Renderer`]'s `encode_*_thumbnail_png` /
/// `render_material_preview`). The material preview forwards the optional codegen
/// `_preview.spv` path the `preview-render` command compiles for a non-foldable graph.
///
/// The render methods need `&mut Renderer` (they lazily build + cache the thumbnail /
/// preview PSOs + the unit sphere), but the [`ThumbnailGpu`] trait is `&self` (the worker
/// holds a `&dyn`). The Rust host runs `request_thumbnail` inline on the single control
/// drain (no worker thread), so a [`RefCell`] gives the interior mutability without a
/// data race: each method borrows the renderer for the call only and the borrows never
/// nest (the upload trio runs before the render, and the upload trio takes a shared
/// borrow while the render takes the mutable one — sequentially).
struct HostThumbnailGpu<'a> {
    renderer: RefCell<&'a mut Renderer>,
    uploader: &'a Uploader,
    skinning_enabled: bool,
}

impl GpuUploader for HostThumbnailGpu<'_> {
    fn upload_mesh(
        &self,
        mesh: &Mesh,
        skin: &[VertexSkin],
        morph: Option<&saffron_geometry::MorphData>,
        sdf_bake: Option<&saffron_rendering::SdfBake>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        self.uploader.upload_mesh(
            self.renderer.borrow().descriptors(),
            mesh,
            skin,
            morph,
            sdf_bake,
        )
    }

    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader.upload_texture(
            self.renderer.borrow().descriptors(),
            rgba,
            width,
            height,
            srgb,
        )
    }

    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader.upload_texture_float(
            self.renderer.borrow().descriptors(),
            rgba,
            width,
            height,
        )
    }

    fn skinning_enabled(&self) -> bool {
        self.skinning_enabled
    }
}

impl ThumbnailGpu for HostThumbnailGpu<'_> {
    fn bind_worker_thread(&self) {}

    fn encode_texture_thumbnail_png(
        &self,
        texture: &Arc<GpuTexture>,
        size: u32,
        transfer: PngTransfer,
    ) -> saffron_rendering::Result<ThumbnailPng> {
        let png = self
            .renderer
            .borrow()
            .encode_texture_thumbnail_png(texture, size, transfer)?;
        Ok(into_assets_png(png))
    }

    fn encode_asset_thumbnail_png(
        &self,
        mesh: &Arc<GpuMesh>,
        size: u32,
    ) -> saffron_rendering::Result<ThumbnailPng> {
        let png = self
            .renderer
            .borrow_mut()
            .encode_asset_thumbnail_png(mesh, size)?;
        Ok(into_assets_png(png))
    }

    fn encode_model_thumbnail_png(
        &self,
        mesh: &Arc<GpuMesh>,
        submesh_materials: &[SubmeshMaterial],
        size: u32,
    ) -> saffron_rendering::Result<ThumbnailPng> {
        let png =
            self.renderer
                .borrow_mut()
                .encode_model_thumbnail_png(mesh, submesh_materials, size)?;
        Ok(into_assets_png(png))
    }

    fn render_material_preview(
        &self,
        material: &SubmeshMaterial,
        size: u32,
        shader_spv: Option<&Path>,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        // `shader_spv` of `None` drives the cached default studio preview pipeline; a
        // non-foldable graph material passes its compiled `_preview.spv` for a per-call
        // codegen pipeline.
        self.renderer
            .borrow_mut()
            .render_material_preview(material, size, shader_spv)
    }

    fn render_hdri_ball_preview(
        &self,
        hdri: &Arc<GpuTexture>,
        size: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.renderer
            .borrow_mut()
            .render_hdri_ball_preview(hdri, size)
    }
}

/// Maps the renderer's [`saffron_rendering::ThumbnailPng`] to the assets-layer
/// [`ThumbnailPng`] the worker / control reply consume (identical fields).
fn into_assets_png(png: saffron_rendering::ThumbnailPng) -> ThumbnailPng {
    ThumbnailPng {
        bytes: png.bytes,
        width: png.width,
        height: png.height,
    }
}

/// The off-frame-loop thumbnail worker's GPU seam.
///
/// The worker thread owns this `Send` object for its whole life and decodes the image bytes
/// on its own thread, then drives the GPU through it. The renderer's `Device` + bindless
/// `Descriptors` are `Send + Sync` and shared by `Arc` (every bindless slot claim + write
/// serializes through the descriptor table's internal mutex, so a worker upload races no
/// frame-loop one); the worker holds its **own** [`Uploader`] (a per-thread command pool, the
/// queue shared behind its `Arc<Mutex>`) and its **own** [`ThumbnailRenderer`] (per-thread
/// command pools + PSO cache, prewarmed on this thread so it never races a main-thread build).
/// The render methods need `&mut ThumbnailRenderer`, so it sits behind a [`RefCell`] — the
/// worker is single-threaded, so the borrows never alias.
pub struct WorkerThumbnailGpu {
    device: Arc<Device>,
    descriptors: Arc<Descriptors>,
    uploader: Uploader,
    thumbnail: RefCell<ThumbnailRenderer>,
    skinning_enabled: bool,
}

// SAFETY: every field is `Send` — `Arc<Device>`/`Arc<Descriptors>` over `Send + Sync` types,
// the `Uploader` is `Send` (its pool is used only from the owning worker thread), and
// `RefCell<ThumbnailRenderer>` is `Send` because `ThumbnailRenderer` is. The object is `!Sync`
// (the `RefCell`), which is correct: only the one worker thread ever touches it.
unsafe impl Send for WorkerThumbnailGpu {}

impl WorkerThumbnailGpu {
    /// Builds the worker GPU seam from the renderer's shared device + descriptors, with its own
    /// uploader + thumbnail renderer prewarmed on the calling thread. Built on the **worker**
    /// thread: each worker owns its own resources, so prewarming on the worker thread is
    /// race-free.
    ///
    /// # Errors
    ///
    /// Returns the rendering error if the uploader's command pool or the prewarm fails.
    pub fn new(
        device: Arc<Device>,
        descriptors: Arc<Descriptors>,
        queue: GpuQueue,
        skinning_enabled: bool,
    ) -> saffron_rendering::Result<Self> {
        let uploader = Uploader::new(&device, &queue)?;
        let mut thumbnail =
            ThumbnailRenderer::new(device.resources(), device.surface_format.format);
        thumbnail.prewarm(&device, &descriptors)?;
        Ok(Self {
            device,
            descriptors,
            uploader,
            thumbnail: RefCell::new(thumbnail),
            skinning_enabled,
        })
    }
}

impl GpuUploader for WorkerThumbnailGpu {
    fn upload_mesh(
        &self,
        mesh: &Mesh,
        skin: &[VertexSkin],
        morph: Option<&saffron_geometry::MorphData>,
        sdf_bake: Option<&saffron_rendering::SdfBake>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        self.uploader
            .upload_mesh(&self.descriptors, mesh, skin, morph, sdf_bake)
    }

    fn upload_texture(
        &self,
        rgba: &[u8],
        width: u32,
        height: u32,
        srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader
            .upload_texture(&self.descriptors, rgba, width, height, srgb)
    }

    fn upload_texture_float(
        &self,
        rgba: &[f32],
        width: u32,
        height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.uploader
            .upload_texture_float(&self.descriptors, rgba, width, height)
    }

    fn skinning_enabled(&self) -> bool {
        self.skinning_enabled
    }
}

impl ThumbnailGpu for WorkerThumbnailGpu {
    fn bind_worker_thread(&self) {
        // The worker's uploader + thumbnail renderer own their command pools, so there is no
        // thread-local pool to bind.
    }

    fn encode_texture_thumbnail_png(
        &self,
        texture: &Arc<GpuTexture>,
        size: u32,
        transfer: PngTransfer,
    ) -> saffron_rendering::Result<ThumbnailPng> {
        let png = self.thumbnail.borrow().encode_texture_thumbnail_png(
            &self.device,
            texture,
            size,
            transfer,
        )?;
        Ok(into_assets_png(png))
    }

    fn encode_asset_thumbnail_png(
        &self,
        mesh: &Arc<GpuMesh>,
        size: u32,
    ) -> saffron_rendering::Result<ThumbnailPng> {
        let png = self.thumbnail.borrow_mut().encode_asset_thumbnail_png(
            &self.device,
            &self.descriptors,
            mesh,
            size,
        )?;
        Ok(into_assets_png(png))
    }

    fn encode_model_thumbnail_png(
        &self,
        mesh: &Arc<GpuMesh>,
        submesh_materials: &[SubmeshMaterial],
        size: u32,
    ) -> saffron_rendering::Result<ThumbnailPng> {
        let png = self.thumbnail.borrow_mut().encode_model_thumbnail_png(
            &self.device,
            &self.descriptors,
            mesh,
            submesh_materials,
            size,
        )?;
        Ok(into_assets_png(png))
    }

    fn render_material_preview(
        &self,
        material: &SubmeshMaterial,
        size: u32,
        shader_spv: Option<&Path>,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.thumbnail.borrow_mut().render_material_preview(
            &self.device,
            &self.descriptors,
            material,
            size,
            shader_spv,
        )
    }

    fn render_hdri_ball_preview(
        &self,
        hdri: &Arc<GpuTexture>,
        size: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        self.thumbnail.borrow_mut().render_hdri_ball_preview(
            &self.device,
            &self.descriptors,
            hdri,
            size,
        )
    }
}
