//! Shared `#[cfg(test)]` fixtures: an in-memory [`ControlRenderer`] stub and an
//! `EngineContext` builder, so the registry and render-command unit tests drive the
//! handlers without the un-headless-constructible concrete `Renderer`.

#![cfg(test)]

use std::path::Path;
use std::sync::Arc;

use saffron_assets::{AssetServer, GpuUploader};
use saffron_geometry::{Mesh, VertexSkin};
use saffron_rendering::{
    ActiveAlarm, AlarmDrain, AlarmEvent, CaptureMode, CaptureState, FrameHistoryStats, FrameSample,
    GpuMesh, GpuTexture, PassTiming, PerfConfig, ProfileCapture, ProfilerMode, ReflectionProbe,
    RenderStatsFull, ViewId, ViewMode,
};
use saffron_sceneedit::SceneEditContext;
use saffron_window::Window;
use serde_json::{Value, json};

use crate::registry::{ControlRenderer, EngineContext};

/// A no-op upload seam the stub hands to [`ControlRenderer::with_gpu_uploader`]: the asset-domain
/// unit tests resolve against an empty catalog (which negative-caches before reaching the GPU), so
/// the upload entry points are never actually driven.
struct StubGpu;

impl GpuUploader for StubGpu {
    fn upload_mesh(
        &self,
        _mesh: &Mesh,
        _skin: &[VertexSkin],
        _morph: Option<&saffron_geometry::MorphData>,
        _sdf_bake: Option<&saffron_rendering::SdfBake>,
    ) -> saffron_rendering::Result<Arc<GpuMesh>> {
        unreachable!("an empty catalog never reaches the stub uploader")
    }

    fn upload_texture(
        &self,
        _rgba: &[u8],
        _width: u32,
        _height: u32,
        _srgb: bool,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        unreachable!("an empty catalog never reaches the stub uploader")
    }

    fn upload_texture_float(
        &self,
        _rgba: &[f32],
        _width: u32,
        _height: u32,
    ) -> saffron_rendering::Result<Arc<GpuTexture>> {
        unreachable!("an empty catalog never reaches the stub uploader")
    }

    fn skinning_enabled(&self) -> bool {
        false
    }
}

/// Resolves a tier name to its [`saffron_rendering::RenderQuality`], `None` for an unknown name —
/// so the stub's GTAO / contact / SSGI enabled flags derive from the tier exactly like the renderer.
fn resolved_tier(name: &str) -> Option<saffron_rendering::RenderQuality> {
    saffron_rendering::QualityTier::from_name(name).map(saffron_rendering::QualityTier::resolve)
}

/// An in-memory renderer stub: every toggle round-trips through a plain field so a
/// handler's "echo the applied state" contract is exercised without a GPU. `rt_supported`
/// defaults `false`, matching a software device, so the RT-gated handlers take their
/// unsupported branch unless a test flips it.
pub struct StubRenderer {
    pub clustered: bool,
    pub depth_prepass: bool,
    pub shadows: bool,
    pub ibl: bool,
    /// The render-quality tier name; the GTAO / contact / SSGI enabled flags derive from it.
    pub quality_tier: String,
    /// The editor viewport power state name (`focused`/`unfocused`/`occluded`).
    pub power_state: String,
    /// The active tonemap operator name.
    pub tonemap: String,
    pub ddgi: bool,
    pub sky_occlusion: bool,
    pub gdf: bool,
    pub reflection_probes: bool,
    pub skinning: bool,
    pub displacement: bool,
    pub tess_factor_cap: f32,
    pub tess_min_factor: f32,
    pub tess_edge_length_target: f32,
    pub rt_supported: bool,
    pub rt_shadows: bool,
    pub restir: bool,
    pub ssr: bool,
    pub rt_reflections: bool,
    pub view_mode: ViewMode,
    pub aa_samples: u32,
    pub aa_fxaa: bool,
    pub aa_taa: bool,
    pub taa_params: saffron_protocol::TaaParamsDto,
    pub exposure_ev: f32,
    pub color_grading: saffron_protocol::SetColorGradingParams,
    pub creative_lut_id: u64,
    pub creative_lut_intensity: f32,
    pub creative_lut_size: u32,
    pub bloom_enabled: bool,
    pub bloom_intensity: f32,
    pub bloom_scatter: f32,
    pub bloom_tint: [f32; 3],
    pub bloom_threshold: f32,
    pub bloom_dirt_texture: u64,
    pub bloom_dirt_intensity: f32,
    pub bloom_dirt_tint: [f32; 3],
    pub bloom_anamorphic_enabled: bool,
    pub bloom_anamorphic_ratio: f32,
    pub bloom_anamorphic_tint: [f32; 3],
    pub bloom_anamorphic_intensity: f32,
    pub bloom_mip_tint: Vec<[f32; 3]>,
    pub profiler_mode: ProfilerMode,
    pub timestamps_supported: bool,
    pub pipeline_stats_supported: bool,
    pub software_gpu: bool,
    pub perf_config: PerfConfig,
    /// The percentile/consistency summary [`frame_history_stats`] reports.
    pub frame_stats: FrameHistoryStats,
    /// The recent raw frames [`frame_samples`] returns (truncated to the request).
    pub frame_history_samples: Vec<FrameSample>,
    /// The alarm event ring [`drain_alarms`] filters by the `since` cursor.
    pub alarm_drain: AlarmDrain,
    /// The currently-firing alarms [`active_alarms`] reports.
    pub active_alarm_list: Vec<ActiveAlarm>,
    pub capture_state: CaptureState,
    pub capture_mode: CaptureMode,
    pub capture_id: u32,
    pub width: u32,
    pub height: u32,
    /// The active view's dynamic-resolution factor `(0, 1]`; the input extent is `display × this`.
    pub render_scale: f32,
    pub probes: Vec<ReflectionProbe>,
    pub active_view: ViewId,
    /// Per-view desired offscreen size, indexed by [`ViewId::index`].
    pub view_sizes: [(u32, u32); saffron_rendering::VIEW_COUNT],
}

impl Default for StubRenderer {
    fn default() -> Self {
        Self {
            clustered: true,
            depth_prepass: false,
            shadows: true,
            ibl: true,
            quality_tier: "high".to_owned(),
            power_state: "focused".to_owned(),
            tonemap: "aces".to_owned(),
            ddgi: false,
            sky_occlusion: true,
            gdf: false,
            reflection_probes: true,
            skinning: true,
            displacement: true,
            tess_factor_cap: 16.0,
            tess_min_factor: 1.0,
            tess_edge_length_target: 12.0,
            rt_supported: false,
            rt_shadows: false,
            restir: false,
            ssr: false,
            rt_reflections: false,
            view_mode: ViewMode::Lit,
            aa_samples: 1,
            aa_fxaa: false,
            aa_taa: false,
            taa_params: saffron_protocol::TaaParamsDto {
                feedback_min: 0.88,
                feedback_max: 0.97,
                velocity_rejection: 0.025,
                clip_gamma: 1.0,
                sharpness: 0.0,
            },
            exposure_ev: 0.0,
            color_grading: saffron_protocol::SetColorGradingParams::default(),
            creative_lut_id: 0,
            creative_lut_intensity: 0.0,
            creative_lut_size: 2,
            bloom_enabled: false,
            bloom_intensity: 0.05,
            bloom_scatter: 0.005,
            bloom_tint: [1.0, 1.0, 1.0],
            bloom_threshold: 0.0,
            bloom_dirt_texture: 0,
            bloom_dirt_intensity: 0.0,
            bloom_dirt_tint: [1.0, 1.0, 1.0],
            bloom_anamorphic_enabled: false,
            bloom_anamorphic_ratio: 2.0,
            bloom_anamorphic_tint: [0.6, 0.8, 1.0],
            bloom_anamorphic_intensity: 0.0,
            bloom_mip_tint: Vec::new(),
            profiler_mode: ProfilerMode::Off,
            timestamps_supported: false,
            pipeline_stats_supported: false,
            software_gpu: true,
            perf_config: PerfConfig::default(),
            frame_stats: FrameHistoryStats::default(),
            frame_history_samples: Vec::new(),
            alarm_drain: AlarmDrain::default(),
            active_alarm_list: Vec::new(),
            capture_state: CaptureState::Idle,
            capture_mode: CaptureMode::Single,
            capture_id: 0,
            width: 1280,
            height: 720,
            render_scale: 1.0,
            probes: Vec::new(),
            active_view: ViewId::Scene,
            view_sizes: [(1280, 720); saffron_rendering::VIEW_COUNT],
        }
    }
}

impl ControlRenderer for StubRenderer {
    fn render_stats(&self) -> RenderStatsFull {
        RenderStatsFull {
            software_gpu: self.software_gpu,
            profiler_mode: self.profiler_mode,
            view_mode: self.view_mode,
            exposure_ev: self.exposure_ev,
            ..RenderStatsFull::default()
        }
    }

    fn clustered_enabled(&self) -> bool {
        self.clustered
    }
    fn set_clustered(&mut self, enabled: bool) {
        self.clustered = enabled;
    }
    fn depth_prepass_enabled(&self) -> bool {
        self.depth_prepass
    }
    fn set_depth_prepass(&mut self, enabled: bool) {
        self.depth_prepass = enabled;
    }
    fn shadows_enabled(&self) -> bool {
        self.shadows
    }
    fn set_shadows(&mut self, enabled: bool) {
        self.shadows = enabled;
    }
    fn ibl_enabled(&self) -> bool {
        self.ibl
    }
    fn set_ibl(&mut self, enabled: bool) {
        self.ibl = enabled;
    }
    fn ssao_enabled(&self) -> bool {
        resolved_tier(&self.quality_tier).is_some_and(|q| q.gtao_enabled)
    }
    fn contact_shadows_enabled(&self) -> bool {
        resolved_tier(&self.quality_tier).is_some_and(|q| q.contact_enabled)
    }
    fn ssgi_enabled(&self) -> bool {
        resolved_tier(&self.quality_tier).is_some_and(|q| q.ssgi_enabled)
    }
    fn render_quality_tier(&self) -> String {
        self.quality_tier.clone()
    }
    fn set_render_quality(&mut self, tier: &str) -> bool {
        if saffron_rendering::QualityTier::from_name(tier).is_some() {
            self.quality_tier = tier.to_owned();
            true
        } else {
            false
        }
    }
    fn render_scale(&self) -> f32 {
        self.render_scale
    }
    fn set_render_scale(&mut self, scale: f32) {
        self.render_scale = scale.clamp(0.1, 1.0);
    }
    fn input_extent(&self) -> (u32, u32) {
        let scale = self.render_scale.clamp(0.1, 1.0);
        (
            ((self.width as f32 * scale).round() as u32).max(1),
            ((self.height as f32 * scale).round() as u32).max(1),
        )
    }
    fn display_extent(&self) -> (u32, u32) {
        (self.width.max(1), self.height.max(1))
    }
    fn tonemap_mode(&self) -> String {
        self.tonemap.clone()
    }
    fn set_tonemap(&mut self, mode: &str) -> bool {
        if saffron_rendering::TonemapMode::from_name(mode).is_some() {
            self.tonemap = mode.to_owned();
            true
        } else {
            false
        }
    }
    fn reactive_idle(&self) -> bool {
        false
    }
    fn reactive_converged(&self) -> bool {
        false
    }
    fn redraw_reasons(&self) -> Vec<String> {
        Vec::new()
    }
    fn power_state(&self) -> String {
        self.power_state.clone()
    }
    fn set_viewport_power_state(&mut self, state: &str) -> bool {
        if saffron_rendering::PowerState::from_name(state).is_some() {
            self.power_state = state.to_owned();
            true
        } else {
            false
        }
    }
    fn ddgi_enabled(&self) -> bool {
        self.ddgi
    }
    fn set_ddgi(&mut self, enabled: bool) {
        self.ddgi = enabled;
    }
    fn sky_occlusion_enabled(&self) -> bool {
        self.sky_occlusion
    }
    fn set_sky_occlusion(&mut self, enabled: bool) {
        self.sky_occlusion = enabled;
    }
    fn gdf_enabled(&self) -> bool {
        self.gdf
    }
    fn set_gdf(&mut self, enabled: bool) {
        self.gdf = enabled;
    }
    fn reflection_probes_enabled(&self) -> bool {
        self.reflection_probes
    }
    fn set_reflection_probes(&mut self, enabled: bool) {
        self.reflection_probes = enabled;
    }
    fn reflection_probes(&self) -> Vec<ReflectionProbe> {
        self.probes.clone()
    }
    fn skinning_enabled(&self) -> bool {
        self.skinning
    }
    fn set_skinning(&mut self, enabled: bool) {
        self.skinning = enabled;
    }
    fn displacement_enabled(&self) -> bool {
        self.displacement
    }
    fn set_displacement(&mut self, enabled: bool) {
        self.displacement = enabled;
    }
    fn set_tessellation_quality(
        &mut self,
        factor_cap: Option<f32>,
        min_factor: Option<f32>,
        edge_length_target: Option<f32>,
    ) {
        if let Some(cap) = factor_cap {
            self.tess_factor_cap = cap.round().clamp(1.0, 2048.0);
        }
        if let Some(min) = min_factor {
            self.tess_min_factor = min.clamp(1.0, self.tess_factor_cap);
        }
        self.tess_min_factor = self.tess_min_factor.min(self.tess_factor_cap);
        if let Some(target) = edge_length_target {
            self.tess_edge_length_target = target.max(1.0);
        }
    }
    fn tessellation_quality(&self) -> (f32, f32, f32) {
        (
            self.tess_factor_cap,
            self.tess_min_factor,
            self.tess_edge_length_target,
        )
    }

    fn rt_supported(&self) -> bool {
        self.rt_supported
    }
    fn rt_shadows_enabled(&self) -> bool {
        self.rt_shadows
    }
    fn set_rt_shadows(&mut self, enabled: bool) {
        self.rt_shadows = enabled;
    }
    fn restir_enabled(&self) -> bool {
        self.restir
    }
    fn set_restir(&mut self, enabled: bool) {
        self.restir = enabled;
    }
    fn ssr_enabled(&self) -> bool {
        self.ssr
    }
    fn set_ssr(&mut self, enabled: bool) {
        self.ssr = enabled;
    }
    fn rt_reflections_enabled(&self) -> bool {
        self.rt_reflections
    }
    fn set_rt_reflections(&mut self, enabled: bool) {
        self.rt_reflections = enabled;
    }
    fn rt_blas_count(&self) -> u32 {
        0
    }

    fn pipeline_count(&self) -> u32 {
        0
    }
    fn bindless_texture_count(&self) -> u32 {
        0
    }
    fn bindless_free_count(&self) -> u32 {
        0
    }

    fn view_mode(&self) -> ViewMode {
        self.view_mode
    }
    fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
    }

    fn aa_mode(&self) -> String {
        if self.aa_samples >= 2 {
            format!("msaa{}", self.aa_samples)
        } else if self.aa_fxaa {
            "fxaa".to_owned()
        } else if self.aa_taa {
            "taa".to_owned()
        } else {
            "off".to_owned()
        }
    }
    fn set_aa(&mut self, samples: u32, fxaa: bool, taa: bool) -> Result<(), String> {
        self.aa_samples = samples;
        self.aa_fxaa = fxaa;
        self.aa_taa = taa;
        Ok(())
    }

    fn taa_params(&self) -> saffron_protocol::TaaParamsDto {
        self.taa_params.clone()
    }
    fn set_taa_params(&mut self, params: saffron_protocol::TaaParamsDto) {
        self.taa_params = params;
    }

    fn exposure_ev(&self) -> f32 {
        self.exposure_ev
    }
    fn set_exposure(&mut self, ev: f32) {
        self.exposure_ev = ev;
    }
    fn set_color_grading(&mut self, params: saffron_protocol::SetColorGradingParams) {
        self.color_grading = params;
    }
    fn color_grading(&self) -> saffron_protocol::SetColorGradingParams {
        let mut params = self.color_grading.clone();
        params.creative_lut_asset = saffron_protocol::Uuid(self.creative_lut_id);
        params.creative_lut_intensity = self.creative_lut_intensity;
        params
    }
    fn set_creative_lut_texture(
        &mut self,
        id: u64,
        _lut: Option<std::sync::Arc<saffron_rendering::GpuLut>>,
        intensity: f32,
    ) {
        self.creative_lut_id = id;
        self.creative_lut_intensity = intensity;
        self.creative_lut_size = if id == 0 { 2 } else { 33 };
    }
    fn creative_lut(&self) -> Option<(u64, u32, f32)> {
        (self.creative_lut_id != 0).then_some((
            self.creative_lut_id,
            self.creative_lut_size,
            self.creative_lut_intensity,
        ))
    }
    fn bake_look_lut(&mut self) -> std::result::Result<(u32, Vec<[u16; 3]>), String> {
        Ok((33, vec![[0, 0, 0]; 33usize.pow(3)]))
    }

    fn set_bloom(
        &mut self,
        enabled: bool,
        intensity: f32,
        scatter: f32,
        tint: [f32; 3],
        threshold: f32,
    ) {
        self.bloom_enabled = enabled;
        self.bloom_intensity = intensity;
        self.bloom_scatter = scatter;
        self.bloom_tint = tint;
        self.bloom_threshold = threshold;
    }
    fn bloom_enabled(&self) -> bool {
        self.bloom_enabled
    }
    fn bloom_intensity(&self) -> f32 {
        self.bloom_intensity
    }
    fn bloom_scatter(&self) -> f32 {
        self.bloom_scatter
    }
    fn bloom_tint(&self) -> [f32; 3] {
        self.bloom_tint
    }
    fn bloom_threshold(&self) -> f32 {
        self.bloom_threshold
    }
    fn set_bloom_dirt_texture(&mut self, id: u64, _texture: Option<std::sync::Arc<GpuTexture>>) {
        self.bloom_dirt_texture = id;
    }
    fn set_bloom_dirt_params(&mut self, intensity: f32, tint: [f32; 3]) {
        self.bloom_dirt_intensity = intensity;
        self.bloom_dirt_tint = tint;
    }
    fn bloom_dirt_texture(&self) -> u64 {
        self.bloom_dirt_texture
    }
    fn bloom_dirt_intensity(&self) -> f32 {
        self.bloom_dirt_intensity
    }
    fn bloom_dirt_tint(&self) -> [f32; 3] {
        self.bloom_dirt_tint
    }
    fn set_bloom_anamorphic(&mut self, enabled: bool, ratio: f32, tint: [f32; 3], intensity: f32) {
        self.bloom_anamorphic_enabled = enabled;
        self.bloom_anamorphic_ratio = ratio;
        self.bloom_anamorphic_tint = tint;
        self.bloom_anamorphic_intensity = intensity;
    }
    fn bloom_anamorphic_enabled(&self) -> bool {
        self.bloom_anamorphic_enabled
    }
    fn bloom_anamorphic_ratio(&self) -> f32 {
        self.bloom_anamorphic_ratio
    }
    fn bloom_anamorphic_tint(&self) -> [f32; 3] {
        self.bloom_anamorphic_tint
    }
    fn bloom_anamorphic_intensity(&self) -> f32 {
        self.bloom_anamorphic_intensity
    }
    fn set_bloom_mip_tint(&mut self, tint: Vec<[f32; 3]>) {
        self.bloom_mip_tint = tint;
    }
    fn bloom_mip_tint(&self) -> Vec<[f32; 3]> {
        self.bloom_mip_tint.clone()
    }

    fn profiler_mode(&self) -> ProfilerMode {
        self.profiler_mode
    }
    fn set_profiler_mode(&mut self, mode: ProfilerMode) {
        // Mirror the renderer clamp: unsupported modes fall back to Off.
        self.profiler_mode = if mode != ProfilerMode::Off && !self.timestamps_supported {
            ProfilerMode::Off
        } else {
            mode
        };
    }
    fn profiler_timestamps_supported(&self) -> bool {
        self.timestamps_supported
    }
    fn profiler_pipeline_stats_supported(&self) -> bool {
        self.pipeline_stats_supported
    }
    fn pass_timings(&self) -> Vec<PassTiming> {
        Vec::new()
    }
    fn pass_timings_total_ms(&self) -> f32 {
        0.0
    }

    fn start_profile_capture(
        &mut self,
        mode: CaptureMode,
        _frames: u32,
        _filter: String,
        _include_cpu: bool,
        _include_stats: bool,
    ) -> u32 {
        self.capture_mode = mode;
        self.capture_state = CaptureState::Arming;
        self.capture_id += 1;
        self.capture_id
    }
    fn stop_profile_capture(&mut self) -> ProfileCapture {
        self.capture_state = CaptureState::Idle;
        ProfileCapture::default()
    }
    fn profile_capture_mode(&self) -> CaptureMode {
        self.capture_mode
    }
    fn profile_capture_state(&self) -> CaptureState {
        self.capture_state
    }
    fn profile_capture_captured_frames(&self) -> u32 {
        0
    }
    fn profile_capture_target_frames(&self) -> u32 {
        1
    }

    fn frame_history_stats(&self) -> FrameHistoryStats {
        self.frame_stats
    }
    fn frame_samples(&self, max_samples: u32) -> Vec<FrameSample> {
        self.frame_history_samples
            .iter()
            .take(max_samples as usize)
            .copied()
            .collect()
    }
    fn perf_config(&self) -> PerfConfig {
        self.perf_config
    }
    fn set_perf_config(&mut self, config: PerfConfig) {
        self.perf_config = config.clamped();
    }

    fn drain_alarms(&self, since: u64) -> AlarmDrain {
        // Mirror the renderer's cursor contract: only events past `since` come back.
        let events: Vec<AlarmEvent> = self
            .alarm_drain
            .events
            .iter()
            .filter(|event| event.seq > since)
            .cloned()
            .collect();
        AlarmDrain {
            events,
            high_water_seq: self.alarm_drain.high_water_seq,
            oldest_seq: self.alarm_drain.oldest_seq,
            overflowed: self.alarm_drain.overflowed,
        }
    }
    fn active_alarms(&self) -> Vec<ActiveAlarm> {
        self.active_alarm_list.clone()
    }

    fn viewport_width(&self) -> u32 {
        self.width
    }
    fn viewport_height(&self) -> u32 {
        self.height
    }

    fn software_gpu(&self) -> bool {
        self.software_gpu
    }

    fn wait_gpu_idle(&mut self) {}

    fn set_active_view(&mut self, view: ViewId) {
        self.active_view = view;
        let (w, h) = self.view_sizes[view.index()];
        self.width = w;
        self.height = h;
    }
    fn view_desired_size(&self, view: ViewId) -> (u32, u32) {
        self.view_sizes[view.index()]
    }
    fn set_view_desired_size(
        &mut self,
        view: ViewId,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.view_sizes[view.index()] = (width, height);
        if view == self.active_view {
            self.width = width;
            self.height = height;
        }
        Ok(())
    }

    fn capture_viewport(&mut self, _path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn request_window_capture(&mut self, _path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn with_gpu_uploader(&mut self, with: &mut dyn FnMut(&dyn GpuUploader)) {
        with(&StubGpu);
    }

    fn create_vegetation_compute_executor(
        &self,
    ) -> Result<Option<crate::VegetationComputeExecutor>, String> {
        Ok(None)
    }

    fn render_material_preview_png(
        &mut self,
        _assets: &mut AssetServer,
        _subject: crate::commands_asset::PreviewSubject,
        _size: u32,
    ) -> std::result::Result<Vec<u8>, String> {
        // The stub renders no scene; a fixed non-empty byte string proves the seam is wired.
        Ok(b"stub-preview-png".to_vec())
    }

    fn render_settings_to_json(&self) -> Value {
        json!({})
    }

    fn apply_render_settings(&mut self, _settings: &Value) {}

    fn sa_lua_defs(&self) -> String {
        String::new()
    }

    fn reset_frame_telemetry(&mut self) {}
}

/// Runs `body` against a fresh `EngineContext` built from the cheaply constructible
/// subsystems (a headless window, a default editor context, an asset server rooted at a
/// temp dir) and the given renderer stub.
pub fn with_stub<T>(
    renderer: &mut StubRenderer,
    body: impl FnOnce(&mut EngineContext<'_>) -> T,
) -> T {
    let mut window = Window::headless();
    let mut scene_edit = SceneEditContext::new();
    let mut assets = AssetServer::new(std::env::temp_dir().join("saffron-control-test"));
    let mut spatial = saffron_spatial::ResidencyManager::new();
    let mut vegetation_jobs = crate::vegetation_jobs::VegetationEvaluationJobs::default();
    let mut vegetation_compute = None;
    let mut ctx = EngineContext {
        window: &mut window,
        renderer,
        scene_edit: &mut scene_edit,
        assets: &mut assets,
        spatial: &mut spatial,
        physics: None,
        vegetation_jobs: &mut vegetation_jobs,
        vegetation_compute: &mut vegetation_compute,
    };
    body(&mut ctx)
}
