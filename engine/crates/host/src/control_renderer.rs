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

use std::path::Path;
use std::sync::Arc;

use saffron_assets::{AssetServer, GpuUploader, PREVIEW_THUMBNAIL_MATERIAL_ID, RendererUploader};
use saffron_control::{ControlRenderer, VegetationComputeExecutor};
use saffron_rendering::{
    ActiveAlarm, AlarmDrain, CaptureMode, CaptureState, FrameHistoryStats, FrameSample, PassTiming,
    PerfConfig, ProfileCapture, ProfilerMode, ReflectionProbe, RenderStatsFull, Renderer, Uploader,
    ViewId, ViewMode,
};
use saffron_vegetation_gpu::VulkanGraphComputeExecutor;
use serde_json::Value;

/// Maps a wire grade range onto the renderer's [`saffron_rendering::GradeRange`].
fn range_to_grade(r: &saffron_protocol::GradeRangeDto) -> saffron_rendering::GradeRange {
    saffron_rendering::GradeRange {
        slope: r.slope,
        offset: r.offset,
        power: r.power,
        saturation: r.saturation,
        contrast: r.contrast,
    }
}

/// Maps a renderer grade range back onto the wire [`saffron_protocol::GradeRangeDto`].
fn grade_to_range(r: &saffron_rendering::GradeRange) -> saffron_protocol::GradeRangeDto {
    saffron_protocol::GradeRangeDto {
        slope: r.slope,
        offset: r.offset,
        power: r.power,
        saturation: r.saturation,
        contrast: r.contrast,
    }
}

/// The host's live renderer seam: the renderer plus the host-owned uploader, bundled for
/// one control-plane drain.
///
/// `skinning_enabled` is captured once at construction so the upload seam reports the same
/// gate the scene render uses this frame.
pub struct HostControlRenderer<'a> {
    renderer: &'a mut Renderer,
    uploader: &'a Uploader,
    mirror: &'a mut saffron_assets::GpuSceneMirror,
    skinning_enabled: bool,
}

impl<'a> HostControlRenderer<'a> {
    /// Bundles the renderer, the host-owned uploader, and the GPU-scene mirror for a
    /// control drain.
    pub fn new(
        renderer: &'a mut Renderer,
        uploader: &'a Uploader,
        mirror: &'a mut saffron_assets::GpuSceneMirror,
    ) -> Self {
        let skinning_enabled = renderer.skinning_enabled();
        Self {
            renderer,
            uploader,
            mirror,
            skinning_enabled,
        }
    }
}

impl ControlRenderer for HostControlRenderer<'_> {
    fn render_stats(&self) -> RenderStatsFull {
        self.renderer.render_stats()
    }

    fn gpu_scene_mirror_stats(&self) -> saffron_assets::GpuSceneMirrorStats {
        self.mirror.stats()
    }

    fn page_residency_stats(&self) -> saffron_rendering::PageResidencyStats {
        self.renderer.page_residency_stats()
    }

    fn visibility_counters(&self) -> [u32; 16] {
        self.renderer.visibility_counters()
    }

    fn vegetation_breakdown(&self) -> saffron_assets::VegetationRenderBreakdown {
        self.mirror.vegetation_breakdown()
    }

    fn page_faults(&self) -> u64 {
        self.renderer.page_faults()
    }

    fn clustered_enabled(&self) -> bool {
        self.renderer.clustered_enabled()
    }
    fn set_clustered(&mut self, enabled: bool) {
        self.renderer.set_clustered(enabled);
    }
    fn submit_interaction_impulse(&mut self, impulse: saffron_rendering::InteractionImpulse) {
        self.renderer.submit_interaction_impulses(&[impulse]);
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
    fn set_tessellation_quality(
        &mut self,
        factor_cap: Option<f32>,
        min_factor: Option<f32>,
        edge_length_target: Option<f32>,
    ) {
        self.renderer
            .set_tessellation_quality(factor_cap, min_factor, edge_length_target);
    }
    fn tessellation_quality(&self) -> (f32, f32, f32) {
        self.renderer.tessellation_quality()
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

    fn set_color_grading(&mut self, params: saffron_protocol::SetColorGradingParams) {
        self.renderer
            .set_color_grading(saffron_rendering::ColorGrade {
                temperature: params.temperature,
                tint: params.tint,
                contrast: params.contrast,
                pivot: params.pivot,
                saturation: params.saturation,
                slope: params.slope,
                offset: params.offset,
                power: params.power,
                shadows: range_to_grade(&params.shadows),
                midtones: range_to_grade(&params.midtones),
                highlights: range_to_grade(&params.highlights),
                shadows_max: params.shadows_max,
                highlights_min: params.highlights_min,
                channel_mixer: params.channel_mixer,
                split_shadow: params.split_tone.shadow,
                split_highlight: params.split_tone.highlight,
                split_balance: params.split_tone.balance,
            });
    }
    fn color_grading(&self) -> saffron_protocol::SetColorGradingParams {
        let g = self.renderer.color_grading();
        saffron_protocol::SetColorGradingParams {
            temperature: g.temperature,
            tint: g.tint,
            contrast: g.contrast,
            pivot: g.pivot,
            saturation: g.saturation,
            slope: g.slope,
            offset: g.offset,
            power: g.power,
            shadows: grade_to_range(&g.shadows),
            midtones: grade_to_range(&g.midtones),
            highlights: grade_to_range(&g.highlights),
            shadows_max: g.shadows_max,
            highlights_min: g.highlights_min,
            channel_mixer: g.channel_mixer,
            split_tone: saffron_protocol::SplitToneDto {
                shadow: g.split_shadow,
                highlight: g.split_highlight,
                balance: g.split_balance,
            },
            creative_lut_asset: saffron_protocol::Uuid(
                self.renderer.creative_lut().map_or(0, |(id, _, _)| id),
            ),
            creative_lut_intensity: self
                .renderer
                .creative_lut()
                .map_or(0.0, |(_, _, intensity)| intensity),
        }
    }

    fn set_creative_lut_texture(
        &mut self,
        id: u64,
        lut: Option<std::sync::Arc<saffron_rendering::GpuLut>>,
        intensity: f32,
    ) {
        self.renderer.set_creative_lut_texture(id, lut, intensity);
    }
    fn creative_lut(&self) -> Option<(u64, u32, f32)> {
        self.renderer.creative_lut()
    }
    fn bake_look_lut(&mut self) -> std::result::Result<(u32, Vec<[u16; 3]>), String> {
        self.renderer
            .bake_look_lut(self.uploader)
            .map(|(size, _ev_min, _ev_max, rgb)| (size, rgb))
            .map_err(|e| e.to_string())
    }

    fn set_bloom(
        &mut self,
        enabled: bool,
        intensity: f32,
        scatter: f32,
        tint: [f32; 3],
        threshold: f32,
    ) {
        self.renderer
            .set_bloom(enabled, intensity, scatter, tint, threshold);
    }
    fn bloom_enabled(&self) -> bool {
        self.renderer.bloom_enabled()
    }
    fn bloom_intensity(&self) -> f32 {
        self.renderer.bloom_intensity()
    }
    fn bloom_scatter(&self) -> f32 {
        self.renderer.bloom_scatter()
    }
    fn bloom_tint(&self) -> [f32; 3] {
        self.renderer.bloom_tint()
    }
    fn bloom_threshold(&self) -> f32 {
        self.renderer.bloom_threshold()
    }

    fn set_bloom_dirt_texture(
        &mut self,
        id: u64,
        texture: Option<std::sync::Arc<saffron_rendering::GpuTexture>>,
    ) {
        self.renderer.set_bloom_dirt_texture(id, texture);
    }
    fn set_bloom_dirt_params(&mut self, intensity: f32, tint: [f32; 3]) {
        self.renderer.set_bloom_dirt_params(intensity, tint);
    }
    fn bloom_dirt_texture(&self) -> u64 {
        self.renderer.bloom_dirt_texture()
    }
    fn bloom_dirt_intensity(&self) -> f32 {
        self.renderer.bloom_dirt_intensity()
    }
    fn bloom_dirt_tint(&self) -> [f32; 3] {
        self.renderer.bloom_dirt_tint()
    }
    fn set_bloom_anamorphic(&mut self, enabled: bool, ratio: f32, tint: [f32; 3], intensity: f32) {
        self.renderer
            .set_bloom_anamorphic(enabled, ratio, tint, intensity);
    }
    fn bloom_anamorphic_enabled(&self) -> bool {
        self.renderer.bloom_anamorphic_enabled()
    }
    fn bloom_anamorphic_ratio(&self) -> f32 {
        self.renderer.bloom_anamorphic_ratio()
    }
    fn bloom_anamorphic_tint(&self) -> [f32; 3] {
        self.renderer.bloom_anamorphic_tint()
    }
    fn bloom_anamorphic_intensity(&self) -> f32 {
        self.renderer.bloom_anamorphic_intensity()
    }
    fn set_bloom_mip_tint(&mut self, tint: Vec<[f32; 3]>) {
        self.renderer.set_bloom_mip_tint(tint);
    }
    fn bloom_mip_tint(&self) -> Vec<[f32; 3]> {
        self.renderer.bloom_mip_tint()
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

    fn create_vegetation_compute_executor(
        &self,
    ) -> Result<Option<VegetationComputeExecutor>, String> {
        let executor = VulkanGraphComputeExecutor::new(self.renderer.device_arc())
            .map_err(|error| error.to_string())?;
        Ok(Some(Arc::new(executor)))
    }

    fn render_material_preview_png(
        &mut self,
        assets: &mut AssetServer,
        subject: saffron_control::PreviewSubject,
        size: u32,
    ) -> Result<Vec<u8>, String> {
        // Build the furnished preview scene through a transient uploader over the renderer's
        // descriptors, then render it through the main graph on the offscreen thumbnail view. The
        // uploader borrow of the renderer ends with the block, freeing it for the render pass.
        let (mut scene, _root, camera) = {
            let gpu = RendererUploader::new(
                self.uploader,
                self.renderer.descriptors(),
                self.skinning_enabled,
            );
            saffron_control::build_preview_scene_for_thumbnail(
                assets,
                &gpu,
                subject,
                PREVIEW_THUMBNAIL_MATERIAL_ID,
            )
        };
        let view = camera.view();
        crate::layer::render_preview_scene_to_png(
            self.renderer,
            self.uploader,
            self.mirror,
            &mut scene,
            assets,
            &view,
            size,
        )
        .map(|png| png.bytes)
        .map_err(|e| e.to_string())
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
