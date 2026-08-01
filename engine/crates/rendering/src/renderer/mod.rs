//! The renderer aggregate: device, swapchain, frame ring, and the per-area sub-state. It drives
//! the per-frame acquire → render-graph → present loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3, Vec4};

use crate::budget::{BudgetController, BudgetStep};
use crate::ddgi::DDGI_RAYS_PER_PROBE;
use crate::descriptors::Descriptors;
use crate::device::SurfaceSource;
use crate::draw_list::{FrameDeformation, RenderStats};
use crate::frame::{FrameRing, FrameTimelinePoint};
use crate::frame_history::{
    ActiveAlarm, AlarmDrain, AlarmInputs, AlarmState, FrameHistory, FrameHistoryStats, FrameSample,
    PerfConfig,
};
use crate::ibl::{
    EnvSource, Ibl, LUNAR_ILLUMINANCE_FULL, ReflectionProbeUpload, ReflectionProbes,
    SOLAR_ILLUMINANCE_TOA, Sky, SkyRenderSettings, SkygenParams, sun_transmittance,
};
use crate::instancing::Instancing;
use crate::lighting::{ClusterCamera, Lighting, SceneLighting, SceneWind};
use crate::nested_scopes::NestedScopeRecorder;
use crate::overlay::{
    ColorGrade, GradeUniform, GridPush, OverlayDraw, OverlayState, OverlayVertex, TonemapMode,
    TonemapPush,
};
use crate::pipelines::Pipelines;
use crate::present::PresentSync;
use crate::profiler::{
    CaptureMode, CaptureRecorder, CaptureState, CpuProfiler, GpuProfiler, PassTiming,
    ProfileCapture, ProfilerMode, cpu_now_ns,
};
use crate::quality::RenderQuality;
use crate::reactive::{PowerState, ReactiveState};
use crate::render_graph::{
    RenderGraph, RgAttachment, RgBatchCommandBuffers, RgPass, RgQueueAssignment, RgRecordedBatch,
    RgResource, RgUsage,
};
use crate::resources::BindlessFreeList;
use crate::scene_pass::record_executor_depth_family;
use crate::skinning::Skinning;
use crate::ssao::Ssao;
use crate::tessellation::Tessellation;
use crate::transient::RenderGraphResources;
use crate::view_target::ViewTarget;
use crate::{Device, Error, Result, Swapchain, checked};

mod antialiasing_passes;
mod bloom_pass;
mod capture;
mod cloud_passes;
mod composite_passes;
mod fog_passes;
mod frame;
mod gi_passes;
mod gpu_scene;
mod graph_util;
mod graph_writeback;
mod init;
mod lighting;
mod micro_rt;
mod present;
mod raytracing;
mod scene_graph;
mod screen_space_passes;
mod selection_pick;
mod settings;
mod telemetry;
mod tessellation_prep;
#[cfg(test)]
mod tests;
mod view;
mod visibility_frame;
mod vsm_passes;
mod wind_frame;

use antialiasing_passes::{writeback_history_layout, writeback_lock_layout};
use graph_util::{
    GraphCommandSubmission, capture_barrier, color_load_store, depth_clear_store,
    depth_load_readonly, depth_load_store, merge_timeline_point, plant_validation_error,
    submit_graph_command,
};
use screen_space_passes::{writeback_dfao_history_layout, writeback_ssgi_history_layout};
use tessellation_prep::access_displaced_arena;

pub(crate) use fog_passes::FogParams;
pub use fog_passes::FogRenderSettings;
pub use lighting::InteractionFieldCapture;
pub use telemetry::RenderStatsFull;
pub use view::{VIEW_COUNT, ViewId, ViewMode};

/// A closure recorded into the current frame's scene pass after the executor draws.
type RenderFn = Box<dyn FnOnce(vk::CommandBuffer)>;

/// Maximum SDF occluder instances the per-frame SDF instance SSBO holds; overflow clamps + logs.
pub(crate) const MAX_SDF_INSTANCES: u32 = 4096;

/// Bytes of one frame slot's SDF-scatter meta slice: occluders written, culled against
/// the reach window, dropped to capacity, and a reserved word.
pub(crate) const SDF_META_SLOT_BYTES: u64 = 16;

/// Frames of frame-timing telemetry dropped after a project load, while the pipeline warms up.
const TELEMETRY_WARMUP_FRAMES: u32 = 12;

/// The PSOs one offscreen frame needs, resolved up front so the render-graph build can borrow the
/// rest of the renderer immutably. A `None` skips that pass this frame.
struct FramePipelines {
    depth_prepass: Option<Arc<crate::Pipeline>>,
    cull: Option<Arc<crate::Pipeline>>,
    skin: Option<Arc<crate::Pipeline>>,
    morph: Option<Arc<crate::Pipeline>>,
    shadow: Option<Arc<crate::Pipeline>>,
    gbuffer: Option<Arc<crate::Pipeline>>,
    gtao: Option<Arc<crate::Pipeline>>,
    ao_blur: Option<Arc<crate::Pipeline>>,
    contact: Option<Arc<crate::Pipeline>>,
    ssgi: Option<Arc<crate::Pipeline>>,
    ssgi_blur: Option<Arc<crate::Pipeline>>,
    ssgi_accum: Option<Arc<crate::Pipeline>>,
    gi_resolve: Option<Arc<crate::Pipeline>>,
    dfao: Option<DfaoPipelines>,
    /// This frame's DFAO trace push (camera inverses + frame index; bumped at resolve time).
    dfao_push: crate::DfaoPush,
    specocc: Option<Arc<crate::Pipeline>>,
    specocc_blur: Option<Arc<crate::Pipeline>>,
    /// This frame's specocc trace push (camera inverses + frame index; bumped at resolve time).
    specocc_push: crate::SpecoccPush,
    ssr: Option<Arc<crate::Pipeline>>,
    copy_color: Option<Arc<crate::Pipeline>>,
    ddgi: Option<DdgiPipelines>,
    gdf: Option<GdfPipelines>,
    restir: Option<RestirPipelines>,
    /// This frame's SSGI trace push; the frame index is bumped at resolve time (it needs `&mut self`).
    ssgi_push: crate::SsgiPush,
    /// This frame's SSR trace push (frame index bumped at resolve time, like `ssgi_push`).
    ssr_push: crate::SsgiPush,
    motion: Option<Arc<crate::Pipeline>>,
    taa: Option<Arc<crate::Pipeline>>,
    fxaa: Option<Arc<crate::Pipeline>>,
    bloom: Option<Arc<crate::Pipeline>>,
    tonemap: Option<Arc<crate::Pipeline>>,
    fog: Option<Arc<crate::Pipeline>>,
    cloud_weather: Option<Arc<crate::Pipeline>>,
    cloud_debug: Option<Arc<crate::Pipeline>>,
    cloud_raymarch: Option<Arc<crate::Pipeline>>,
    cloud_reconstruct: Option<Arc<crate::Pipeline>>,
    cloud_upscale: Option<Arc<crate::Pipeline>>,
    cloud_shadow: Option<Arc<crate::Pipeline>>,
    fog_inject: Option<Arc<crate::Pipeline>>,
    fog_integrate: Option<Arc<crate::Pipeline>>,
    aerial: Option<Arc<crate::Pipeline>>,
    scene_resolve: Option<Arc<crate::Pipeline>>,
    depth_upscale: Option<Arc<crate::Pipeline>>,
    reactive_coverage: Option<Arc<crate::Pipeline>>,
    reactive_transition: Option<Arc<crate::Pipeline>>,
    grid: Option<Arc<crate::Pipeline>>,
    overlay: Option<Arc<crate::Pipeline>>,
    overlay_depth: Option<Arc<crate::Pipeline>>,
    wireframe_overlay: Option<Arc<crate::Pipeline>>,
    motion_visualize: Option<Arc<crate::Pipeline>>,
    overlay_draw: Option<OverlayDraw>,
}

/// One optional pipeline-state object resolved for this frame; `None` skips its pass.
type PsoSlot = Option<Arc<crate::Pipeline>>;

/// The visibility / binning PSOs the frame's scene graph records with, resolved together so the
/// `&self` graph build never re-borrows the PSO cache.
struct SceneFramePsos {
    /// Cull, traversal, bin-count, bin-seed, bin-scatter.
    visibility: (PsoSlot, PsoSlot, PsoSlot, PsoSlot, PsoSlot),
    /// Micro-blade count, scan, scatter.
    micro_field: (PsoSlot, PsoSlot, PsoSlot),
    /// Transparent keys, radix histogram, radix scan, radix scatter, reorder.
    transparent_sort: (PsoSlot, PsoSlot, PsoSlot, PsoSlot, PsoSlot),
    wind_deform: PsoSlot,
    wind_interact: PsoSlot,
    /// Ray-geometry materialization for wind-deformed instances.
    rt_deform: PsoSlot,
    gi_scatter: PsoSlot,
    /// Aggregate slab occluders for the resident micro vegetation fields.
    gi_micro: PsoSlot,
    /// Ray-geometry materialization for the reconstructed micro blades.
    micro_rt: PsoSlot,
}

/// The per-world wind and interaction resources one frame publishes, plus the GPU-scene address
/// block every later pass resolves its buffers through.
struct WindFrameResources {
    address_block: crate::GpuSceneAddressBlock,
    records: RgResource,
    interaction_field: RgResource,
    interaction_address: u64,
    impulse_address: u64,
    impulse_count: u32,
}

/// What the visibility half of the frame hands the raster half.
struct VisibilityFrame {
    active: bool,
    history_valid: bool,
    buckets: Vec<crate::ExecutorBucket>,
    inputs: Option<crate::ExecutorDrawInputs>,
}

struct RecordedSceneGraph {
    batches: Vec<RgRecordedBatch>,
    tail: vk::CommandBuffer,
}

/// The four DDGI trace/blend/border PSOs, resolved together — a partial set skips the whole chain.
struct DdgiPipelines {
    trace: Arc<crate::Pipeline>,
    blend_irr: Arc<crate::Pipeline>,
    blend_dist: Arc<crate::Pipeline>,
    border: Arc<crate::Pipeline>,
}

/// The three DFAO compute PSOs, resolved together — a partial set skips the whole chain. The
/// accumulator is part of the chain rather than a polish stage: the trace rotates its cone ring
/// every frame, so only the EMA is a stable sky-visibility term, and it is the map the
/// indirect-diffuse resolve samples.
struct DfaoPipelines {
    trace: Arc<crate::Pipeline>,
    blur: Arc<crate::Pipeline>,
    accum: Arc<crate::Pipeline>,
}

/// One level of the transient bloom mip pyramid: graph resource, image view, and dispatch extent.
struct BloomMip {
    res: RgResource,
    view: vk::ImageView,
    extent: vk::Extent2D,
}

/// The two Global-SDF compute PSOs, resolved together — a partial set skips the whole chain.
struct GdfPipelines {
    cull: Arc<crate::Pipeline>,
    composite: Arc<crate::Pipeline>,
}

/// What [`Renderer::add_gdf_passes`] hands back: the cascade volumes downstream passes sample,
/// plus each imported image's external layout slot for the cross-frame write-back.
#[derive(Default)]
struct GdfResult {
    cascades: Option<[RgResource; crate::GDF_CASCADES as usize]>,
    cascade_slots: [Option<usize>; crate::GDF_CASCADES as usize],
    occupancy: Option<[RgResource; crate::GDF_CASCADES as usize]>,
    occupancy_slots: [Option<usize>; crate::GDF_CASCADES as usize],
    albedo: Option<RgResource>,
    albedo_slot: Option<usize>,
    /// The cull-list buffer's cross-frame state slot and the frame slot it belongs to, read back
    /// so the next frame's build can be placed on the async-compute lane again.
    cull_state: Option<GdfCullState>,
}

/// Where one frame's cull-list buffer state is written back to.
#[derive(Clone, Copy)]
struct GdfCullState {
    slot: usize,
    frame: usize,
}

/// External layout slots for persistent cloud shape fields and per-view temporal products.
#[derive(Default)]
struct CloudGraphResult {
    base: Option<usize>,
    detail: Option<usize>,
    curl: Option<usize>,
    weather: Option<usize>,
    shadow: Option<usize>,
    reduced: [Option<usize>; 2],
    reduced_depth: Option<usize>,
    full_color: Option<usize>,
    full_depth: Option<usize>,
    full_color_resource: Option<RgResource>,
    full_depth_resource: Option<RgResource>,
    temporal: bool,
}

#[derive(Clone, Copy)]
struct CloudGraphInputs {
    color: RgResource,
    depth: RgResource,
    motion: Option<RgResource>,
    sky_sh: RgResource,
}

#[derive(Clone, Copy)]
struct CloudFrameResources {
    base: RgResource,
    detail: RgResource,
    curl: RgResource,
    weather: RgResource,
    shadow: RgResource,
    base_slot: usize,
    detail_slot: usize,
    curl_slot: usize,
    weather_slot: usize,
    shadow_slot: usize,
    params_offset: u32,
}

/// The three ReSTIR DI compute PSOs, resolved together — a partial set skips the chain entirely.
struct RestirPipelines {
    initial: Arc<crate::Pipeline>,
    reuse: Arc<crate::Pipeline>,
    resolve: Arc<crate::Pipeline>,
}

/// What [`Renderer::add_ddgi_passes`] hands back: the irradiance and distance atlases the scene
/// samples, plus each imported image's external layout slot for the cross-frame write-back.
#[derive(Default)]
struct DdgiResult {
    irradiance: Option<RgResource>,
    distance: Option<RgResource>,
    rays_slot: Option<usize>,
    irradiance_slot: Option<usize>,
    distance_slot: Option<usize>,
}

/// What [`Renderer::add_restir_passes`] hands back: the resolved direct-radiance resource the
/// scene samples, the set-7 mesh set it binds, and that image's external-layout slot.
#[derive(Default)]
struct RestirResult {
    radiance: Option<RgResource>,
    mesh_set: vk::DescriptorSet,
    radiance_slot: Option<usize>,
}

/// What [`Renderer::add_screen_space_passes`] hands back: the per-view mesh set 4, the maps the
/// scene samples, and the optional prev-color history copy scheduled after the scene pass.
#[derive(Default)]
struct ScreenSpaceResult {
    mesh_set: vk::DescriptorSet,
    scene_sampled: Vec<RgResource>,
    history_copy: Option<HistoryCopy>,
    /// The two SSGI history images' slots when the temporal accumulation ran.
    ssgi_history_slots: Option<TaaHistorySlots>,
    /// The ssgi_resolved image's external-layout slot when the accumulation ran.
    ssgi_resolved_slot: Option<usize>,
    /// The two DFAO history images' slots when the temporal accumulation ran.
    dfao_history_slots: Option<TaaHistorySlots>,
    /// The dfao_resolved image's external-layout slot when the accumulation ran.
    dfao_resolved_slot: Option<usize>,
    /// The ssr_map image's external-layout slot when the SSR trace ran.
    ssr_map_slot: Option<usize>,
}

/// The SSGI prev-color history copy: it reads the scene's linear-HDR color and writes
/// `prev_color`, so it is scheduled after the scene pass.
struct HistoryCopy {
    prev_color: RgResource,
    pipeline: Arc<crate::Pipeline>,
    set: vk::DescriptorSet,
    groups_x: u32,
    groups_y: u32,
}

/// A history image's `(slot index in the view's `history`, external layout slot)` pair, so the
/// resolved exit layout is written back after execute.
struct TaaHistorySlots {
    read: (usize, usize),
    write: (usize, usize),
}

/// The TAA resolve's cross-frame ping-pong slots: the color history and the pixel-lock image each
/// ride a pair of external-layout slots the caller reads back after execute.
struct TaaResolveSlots {
    history: TaaHistorySlots,
    lock: TaaHistorySlots,
}

/// One world's wind sway record buffer: one [`crate::GpuWindInstanceRecord`] per instance slot.
struct WindDeformRecords {
    buffer: crate::Buffer,
    capacity: u32,
}

/// The renderer: device, swapchain, frame ring, and the per-area sub-state.
///
/// Drop order is load-bearing: the explicit [`Drop`] idles the device, then destroys the
/// device-borrowing sub-state; the `device` field drops last by declaration order.
pub struct Renderer {
    /// Per-view binned-cut state the last frame left behind, replayed by the selection pick.
    selection_sources: [Option<selection_pick::SelectionSource>; crate::VIEW_COUNT],
    /// The one-texel pick targets, allocated on the first pick and reused after.
    selection_targets: Option<selection_pick::SelectionTargets>,
    /// Routes the shaded executor through `VK_EXT_mesh_shader` instead of the indexed path,
    /// wherever the device's mesh feature bits and output limits qualify
    /// ([`crate::mesh_executor_supported`]).
    mesh_executor: bool,
    /// Whether reconstructed micro-blade field passes run. On unless `SAFFRON_MICRO_FIELD=off`.
    micro_field_enabled: bool,
    /// Whether the traversal rejects a hierarchy node whose swept world bounds leave the view —
    /// dropping its subtree with it — and each surviving node's clusters on their own swept
    /// bounds. On unless `SAFFRON_NODE_CULL=off`; read once at construction.
    node_cull: bool,
    /// Pins the hierarchy cut instead of letting projected error choose it, keyed by
    /// [`crate::SceneViewClass::ordinal`] so pinning one class does not drag the others with it.
    /// `SAFFRON_CUT_OVERRIDE` sets the initial value; `set-hierarchy-cut` moves it afterwards.
    cut_override: [u32; crate::SCENE_VIEW_CLASSES],
    /// Occluders dropped from this frame's SDF list for want of capacity (rt-stats).
    sdf_instances_dropped: u32,
    /// Occluders the cascade-window gate excluded this frame (rt-stats).
    sdf_instances_culled: u32,
    /// Ray instances excluded by the cascade-window gate this frame.
    rt_instances_culled: u32,
    /// A wind edit landed since the last frame, so temporal history describes a field that no
    /// longer exists. Consumed and cleared by the frame that resets on it.
    wind_discontinuity: bool,
    /// Live local wind sources last frame, and a digest of each, for discontinuity detection.
    wind_source_count: usize,
    wind_source_digest: Vec<u64>,
    /// Digest of the authored global field, excluding the clock.
    wind_authored_digest: u64,
    /// The clear color applied to the scene/swapchain image each frame (RGBA).
    pub clear_color: [f32; 4],
    /// Wireframe view mode — drives the per-draw PSO `wireframe` permutation (gated on
    /// the device's `fill_mode_non_solid` capability inside the cache).
    pub wireframe: bool,
    /// When set, the scene pass is preceded by a depth pre-pass that lays down depth
    /// first.
    pub use_depth_prepass: bool,

    /// The tonemap exposure in stops; the tonemap pass applies `exp2(this)`.
    exposure_ev: f32,
    /// Sun-elevation-derived scotopic adaptation strength.
    night_factor: f32,
    /// Whether the scene-linear bloom pyramid runs before the tonemap pass. Off by default.
    bloom_enabled: bool,
    /// The energy-conserving bloom composite weight (`lerp(hdr, bloom, this)`). Default `0.05`.
    bloom_intensity: f32,
    /// The bloom tent-upsample scatter radius in UV units. Default `0.005`.
    bloom_scatter: f32,
    /// The bloom tint (multiplies the composited bloom). Default white.
    bloom_tint: [f32; 3],
    /// The soft-knee prefilter threshold; `0.0` (default) leaves bloom thresholdless.
    bloom_threshold: f32,
    /// The lens-dirt mask texture bound into the bloom composite set. `None` binds the 1×1 white
    /// fallback (mask = 1 ⇒ identity), so an absent dirt asset is not a special code path.
    bloom_dirt_texture: Option<Arc<crate::GpuTexture>>,
    /// The lens-dirt asset id for read-back (`0` = none), mirroring `bloom_dirt_texture`.
    bloom_dirt_texture_id: u64,
    /// The lens-dirt mix (`0.0` = no dirt); the composite lerps the mask in by this fraction.
    bloom_dirt_intensity: f32,
    /// The lens-dirt tint (multiplies the sampled mask). Default white.
    bloom_dirt_tint: [f32; 3],
    /// Whether the anamorphic streak (a horizontally-squeezed blur over the radial bloom) runs.
    bloom_anamorphic_enabled: bool,
    /// The anamorphic horizontal squeeze (`~2.0`). Default `2.0`.
    bloom_anamorphic_ratio: f32,
    /// The anamorphic streak tint (cool by default).
    bloom_anamorphic_tint: [f32; 3],
    /// The anamorphic streak add weight (`0.0` = no streak). Default `0.0`.
    bloom_anamorphic_intensity: f32,
    /// The optional per-upsample-step tint stack (identity when off), one tint per upsample pass.
    bloom_mip_tint: Vec<[f32; 3]>,
    /// The authored analytic height/distance fog, composited into the HDR offscreen before bloom.
    fog: FogRenderSettings,
    /// The froxel volumetric-fog volumes + compute sets, sampled by the composite when volumetric.
    froxel: crate::FroxelFog,
    /// The Hillaire-2020 aerial-perspective volume + fill set, folded into the fog composite.
    aerial: crate::AerialPerspective,
    /// Persistent channel-packed cloud noise, curl, and single resolved weather map.
    clouds: crate::Clouds,
    /// This frame's local `FogVolume` records, uploaded into the inject SSBO.
    fog_volumes: Vec<crate::FogVolumeGpu>,
    /// A wrapping scene clock (seconds) driving the fog-volume noise wind advection.
    fog_time: f32,
    /// The directional light's travel direction (normalized), aiming the fog sun-inscatter lobe.
    sun_direction: Vec3,
    /// The atmosphere-coupled directional-light color consumed by cloud lighting.
    sun_color: Vec3,
    /// The atmosphere-coupled directional-light intensity consumed by cloud lighting.
    sun_intensity: f32,
    /// The moon-light travel direction captured for night cloud lighting.
    moon_direction: Vec3,
    /// The atmosphere-coupled moon-light color consumed by cloud lighting.
    moon_color: Vec3,
    /// The atmosphere-coupled moon-light intensity consumed by cloud lighting.
    moon_intensity: f32,
    /// The infinite analytic ground grid debug overlay toggle.
    show_grid: bool,
    /// Native-viewport host mode: present blits the post-processed offscreen straight to the
    /// swapchain, with no ui pass.
    present_viewport_only: bool,

    /// Whether the current frame slot's in-flight fence is reset and no submit has been made that
    /// signals it. Set by [`Renderer::begin_offscreen_frame`], cleared only by a submit that owns
    /// the fence's signal operation, so [`Renderer::render_scene_offscreen`] never re-waits an
    /// unsignaled fence and [`Renderer::finish_unsubmitted_frame`] can close the slot from any
    /// early return between the two.
    slot_fence_armed: bool,

    /// The last async-compute timeline point the current slot actually submitted, cleared with
    /// [`Renderer::slot_fence_armed`]. A slot closed after a partial submit must not signal its
    /// fence before that compute work completes — the fence is what gates resetting the pools it
    /// still reads.
    pending_compute_signal: Option<crate::frame::FrameTimelinePoint>,

    /// The debug render-output mode; drives the wireframe PSO permutation + the debug channel.
    view_mode: ViewMode,
    /// Whether the GPU compute-skinning path runs; off falls back to bind-pose meshes.
    skinning_enabled: bool,

    /// Whether the GPU compute-displacement path runs; off leaves displaced meshes undisplaced.
    displacement_enabled: bool,

    /// The tessellation-quality budget fed to each displaced instance's `TessBucket` (dice cap,
    /// minimum per-edge factor, target screen-space edge length in pixels).
    tess_factor_cap: f32,
    tess_min_factor: f32,
    tess_edge_length_target: f32,

    /// Whether the device is a software rasterizer: GPU timings are CPU rasterization time.
    software_gpu: bool,
    /// The physical-device name, captured at init for profiler capture metadata.
    device_name: String,
    /// The last frame's wall-clock render-thread frame time (ms).
    frame_ms: f32,
    /// The last frame's CPU busy time (ms); `0` until recorded.
    cpu_frame_ms: f32,
    /// The last deformation-frame build time (ms).
    scene_gather_ms: f32,
    scene_gather_entities: u32,
    /// A monotonic per-frame counter, gating the profiler's periodic timestamp re-calibration.
    frame_serial: u64,
    /// The last frame's GPU frame time (ms); `0` until the profiler runs.
    gpu_frame_ms: f32,
    /// The last frame's fence-wait time (ms); `0` until recorded.
    cpu_wait_ms: f32,
    /// Device-local VRAM occupancy in bytes, resampled from the driver's heap budgets each frame.
    vram_usage_bytes: u64,
    /// Device-local VRAM budget in bytes, from the same sample.
    vram_budget_bytes: u64,

    /// The shared frame-budget / green-amber-red threshold config.
    perf_config: PerfConfig,
    /// The rolling frame-time history ring.
    frame_history: FrameHistory,
    /// Frames of telemetry to drop after a project load, while the cold pipeline warms up.
    telemetry_warmup: u32,
    /// The perf-alarm engine: active set + seq-stamped event ring.
    alarms: AlarmState,
    /// The GPU profiler: per-pass timestamps + pipeline statistics.
    gpu_profiler: GpuProfiler,
    /// The CPU span profiler, feeding the merged capture.
    cpu_profiler: CpuProfiler,
    /// The capture recorder driven by `profiler.capture-start/stop`.
    capture: CaptureRecorder,
    /// Wall-clock ns of the last [`Renderer::finalize_frame_telemetry`], for the alarm tick's dt.
    last_frame_ns: u64,

    /// The per-frame editor-overlay geometry, uploaded into a grow-only per-frame vertex buffer.
    overlay: OverlayState,

    submissions: Vec<RenderFn>,
    frame_deformation: FrameDeformation,
    /// This frame's ray-geometry materialization dispatches: one per placed use whose wind
    /// deformation is written into the deformed arena for its bottom-level structure.
    rt_deform_jobs: Vec<crate::RtDeformPush>,
    /// This frame's materialized micro-field tiles: the generated blade geometry each one's
    /// per-frame bottom-level structure is rebuilt from.
    micro_rt_tiles: Vec<crate::MicroRtTile>,
    stats: RenderStats,

    /// The active render-quality tier + resolved screen-space GI parameters.
    render_quality: RenderQuality,

    /// The frame-budget controller that auto-steps `render_quality` under `PerfConfig::auto_quality`.
    budget_controller: BudgetController,
    /// A render-scale change the budget controller requested, applied at the next safe resize point.
    pending_render_scale: Option<f32>,

    /// The active tonemap operator (default ACES), applied in the tonemap pass + reported in stats.
    tonemap_mode: TonemapMode,

    /// The scene-linear color grade, folded into the tonemap pass before the view transform.
    color_grade: ColorGrade,

    /// The neutral identity creative LUT bound when no creative look is assigned.
    default_lut: Arc<crate::GpuLut>,
    /// The assigned display-space creative LUT; `None` binds [`Renderer::default_lut`].
    creative_lut: Option<Arc<crate::GpuLut>>,
    /// The creative-LUT asset id for read-back (`0` = none), mirroring [`Renderer::creative_lut`].
    creative_lut_id: u64,
    /// The creative-LUT look intensity in `[0, 1]` (`0` = neutral), carried in the grade UBO.
    creative_lut_intensity: f32,
    /// The bound creative LUT's resolution per axis (`2` when none), carried in the grade UBO.
    creative_lut_size: u32,

    /// The reactive-loop observability mirror: the host's idle/converged/reasons snapshot plus the
    /// editor's power state, both surfaced in `render-stats`.
    reactive: ReactiveState,

    /// The anti-aliasing selection (MSAA / FXAA / TAA, mutually exclusive).
    aa: crate::Aa,

    /// The runtime TAA resolve tuning, read into the resolve push each frame.
    taa_params: crate::TaaParams,
    /// The active camera's `(near, far)` planes, so the TAA resolve can linearize `motionDepth`.
    camera_near_far: (f32, f32),
    /// The last camera the host set, for the tessellation factor pass's screen-space metric.
    cluster_camera: ClusterCamera,

    /// The per-editor-pane render targets, indexed by [`ViewId::index`]; always [`VIEW_COUNT`] long.
    views: Vec<ViewTarget>,
    /// Device-global immutable arenas and tables shared by every registered GPU-scene world.
    global_gpu_data: crate::GlobalGpuData,
    /// Device tables of the persistent GPU scene plus its frame upload translation.
    gpu_scene_uploader: crate::GpuSceneUploader,
    /// CPU spans measured outside this crate, awaiting the next frame slot to be written into.
    pending_cpu_spans: Vec<(String, u64, u64)>,
    /// Shadow pages the frame may render; lowering it is the only way to force page churn.
    vsm_page_budget: usize,
    /// Per-world wind sway record buffers, written by the wind deformation prepass.
    wind_deform_records: std::collections::HashMap<u64, WindDeformRecords>,
    /// The frame's shared wind field parameters.
    scene_wind: SceneWind,
    /// The virtual shadow map: the physical atlas + page-table ring.
    vsm_gpu: crate::vsm::VsmGpu,
    /// The owned budget breaches the frame's alarm tick reads.
    owned_budgets: Vec<crate::OwnedBudgetBreach>,
    /// Deformed instances the interaction field's scroll has reset since boot, summed over every
    /// frame's counter readback.
    wind_interaction_resets: u64,
    /// The interaction-field cascade centres this frame integrates, in absolute world texel
    /// coordinates; the wind prepass compares them against the previous frame's.
    interaction_centers: [[i32; 2]; 2],
    /// The previous frame's [`Self::interaction_centers`].
    interaction_centers_previous: [[i32; 2]; 2],
    /// The VSM CPU residency authority (allocation, LRU, cooldown, dirty pages).
    vsm_residency: crate::VsmResidency,
    /// Per-directional-level page-render visibility views, built lazily.
    vsm_views: Vec<Option<crate::SceneVisibilityView>>,
    /// The global-illumination reach view, built lazily beside the camera's.
    ///
    /// A gather reads occluders behind the eye and behind the depth pyramid, so it cannot share
    /// the camera's list.
    gi_view: Option<crate::SceneVisibilityView>,
    /// The reach view's counters from the latest completed frame.
    gi_visibility_counters: [u32; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
    /// The dirty pages this frame rasterizes.
    vsm_render_pages: Vec<crate::VsmRenderPage>,
    /// The frame's directional virtual-shadow space.
    vsm_space: crate::VsmDirectionalSpace,
    /// The GPU receiver-demand apparatus (bitmap + request rings + layouts).
    vsm_demand: crate::VsmDemand,
    /// The freshest drained receiver demand as `(level, page)` pairs.
    vsm_demanded: Vec<u32>,
    /// The spot matrix the resident spot pages were rendered under.
    vsm_spot_matrix: [f32; 16],
    /// The point light's position + far of the pages on the atlas; a change invalidates every face.
    vsm_point_key: [f32; 4],
    /// Per-world interaction field buffers (header + damped-oscillator texel cascades).
    interaction_fields: std::collections::HashMap<u64, crate::Buffer>,
    /// Impulses staged for this frame's interaction-field step.
    interaction_impulses: Vec<crate::InteractionImpulse>,
    /// Per-frame-in-flight mapped impulse upload ring.
    interaction_impulse_ring: Vec<crate::Buffer>,
    /// Per-frame-in-flight mapped local wind-source ring (cap 64 records).
    wind_source_ring: Vec<crate::Buffer>,
    /// The hierarchy page-payload residency authority (state machine, budgets, LRU).
    page_residency: crate::PageResidency,
    /// Device-shared HZB scaffolding (sampler + build set layouts).
    hzb: crate::Hzb,
    /// Device-shared instance-visibility scaffolding (set layouts + HZB sampler).
    scene_visibility: crate::SceneVisibility,
    /// The populated executor bins (shader index, material class bits) the mirror
    /// pushed after its last sync; draw sites iterate only these.
    live_executor_bins: Vec<(u32, u32)>,
    /// The mirror's upper bound on emitted draw records.
    live_draw_record_bound: u32,
    /// The latest fence-completed visibility counters for the active view.
    visibility_counters: [u32; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
    page_faults: u64,
    /// Resident-record stages, retirements, and arena uploads queued by the asset mirror.
    pending_gpu_scene_uploads: crate::GpuScenePendingUploads,
    /// The resident micro-field tile directory (fields-arena byte offset + entries).
    micro_field_directory: Option<(u32, u32)>,
    /// The most recent frame's upload-translation counters.
    last_gpu_scene_upload: crate::GpuSceneUploadRunStats,
    /// Sole renderer-derived mirror for scene, preview, thumbnail, and player worlds.
    persistent_gpu_scene: crate::PersistentGpuScene,
    /// Which view the renderer renders + presents this frame.
    active_view: ViewId,

    /// Per-view shm-publish enable, indexed by [`ViewId::index`]; the host sets it from its segment
    /// wiring.
    shm_publish_enabled: [bool; VIEW_COUNT],
    /// The `(view index, frame slot)` whose shm-capture staging buffer holds a completed BGRA8 frame,
    /// staged at the begin-frame fence wait for the host to publish from the mapped staging.
    pending_shm_publish: Option<(usize, usize)>,

    lighting: Lighting,
    instancing: Instancing,
    skinning: Skinning,
    /// The adaptive-tessellation prep subsystem: records the prep passes in the deform scope and
    /// emits the amplified transient geometry every raster + RT consumer reads.
    tessellation: Tessellation,
    /// The frame's displacement arena as pass inputs, or `None` when nothing displaces.
    displaced_frame: Option<crate::DisplacedFrameBuffers>,
    /// The same arena's device addresses, published in the frame's GPU-scene address block.
    displaced_addresses: crate::DisplacedFrameAddresses,
    transient: RenderGraphResources,
    pipelines: Pipelines,
    ibl: Ibl,
    /// A second IBL baked to the fixed procedural preview environment, bound only for
    /// [`ViewId::Thumbnail`] so a background thumbnail never thrashes the project bake.
    preview_ibl: Ibl,
    sky: Sky,
    stars: crate::StarCatalog,
    reflection: ReflectionProbes,
    ssao: Ssao,
    ddgi: crate::Ddgi,
    global_sdf: crate::GlobalSdf,
    rt: crate::Rt,
    restir: crate::Restir,

    /// The SDF-occluder SSBO the occluder scatter writes: one [`MAX_SDF_INSTANCES`]-entry region
    /// per frame slot.
    sdf_instances: crate::Buffer,
    /// The scatter's meta words, one 16-byte slice per frame slot: occluders written,
    /// culled against the reach window, dropped to capacity.
    sdf_meta: crate::Buffer,
    /// Fence-gated host copy of [`Self::sdf_meta`], read on slot reuse for render-stats.
    sdf_meta_readback: crate::Buffer,
    /// Whether GDF reflection occlusion (the per-pixel reflection-cone march that occludes the
    /// reflected skybox under overhangs) is enabled.
    sky_occlusion: bool,
    /// The bindless descriptor table, behind an `Arc` so the thumbnail worker shares it; every slot
    /// claim + write goes through its internal `Mutex`.
    descriptors: Arc<Descriptors>,
    bindless_free_list: BindlessFreeList,

    /// The 1×1 white texture at [`crate::DEFAULT_WHITE_SLOT`], seeded into every other bindless
    /// slot; held here so it outlives every draw.
    default_white: Arc<crate::GpuTexture>,

    /// The 1×1×1 empty-space SDF seeded into every unbound slot of the bindless SDF array; it reads
    /// as far from any surface, and is held here so the seeded views stay valid.
    default_sdf: Arc<crate::GpuSdf>,

    /// The unit-box brick every aggregate slab occluder is backed by, so a device-reconstructed
    /// vegetation field reaches the distance field as a real brick-backed occluder.
    slab_sdf: Arc<crate::GpuSdf>,

    /// The 1×1 `(0, 0)` min/max pyramid seeded into every slot of the bindless
    /// `heightMinMaxTextures` array; held here so the seeded view stays valid.
    default_height_minmax: crate::resources::DefaultHeightMinMax,

    /// A pending window screenshot path, armed by [`Renderer::request_window_capture`] and consumed
    /// at the next present.
    capture_next_window_path: Option<std::path::PathBuf>,

    frames: FrameRing,
    /// The present swapchain, present only in the standalone windowed mode; the editor host
    /// publishes offscreen frames to shared memory and has no surface to build one against.
    swapchain: Option<Swapchain>,
    /// The windowed present path's per-slot blit + sync resources, present only alongside the
    /// [`Self::swapchain`].
    present_sync: Option<PresentSync>,
    /// The Vulkan core, behind an `Arc` so the thumbnail worker can share it. The worker is joined
    /// and its clone dropped before the renderer's, so the device dies after every user.
    device: Arc<Device>,
}

impl Renderer {
    /// The immutable device, shared by the sibling sub-state.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The shared device handle, for the thumbnail worker driving renders off the frame loop.
    pub fn device_arc(&self) -> Arc<Device> {
        Arc::clone(&self.device)
    }

    /// The present swapchain; `None` in the editor/headless host, which publishes to shared memory.
    pub fn swapchain(&self) -> Option<&Swapchain> {
        self.swapchain.as_ref()
    }

    /// The descriptor sub-state (the bindless table + set layouts) for upload paths.
    pub fn descriptors(&self) -> &Descriptors {
        &self.descriptors
    }

    /// The shared bindless descriptor table, for the thumbnail worker.
    /// write bindless slots through the same internal `Mutex` the frame loop uses).
    pub fn descriptors_arc(&self) -> Arc<Descriptors> {
        Arc::clone(&self.descriptors)
    }

    /// The 1×1 white texture at [`crate::DEFAULT_WHITE_SLOT`], indexed by materials with no texture.
    pub fn default_white(&self) -> &Arc<crate::GpuTexture> {
        &self.default_white
    }

    /// The 1×1×1 empty-space SDF seeded into every otherwise-unbound bindless SDF slot.
    pub fn default_sdf(&self) -> &Arc<crate::GpuSdf> {
        &self.default_sdf
    }

    /// The 1×1 `(0, 0)` min/max pyramid seeded into every `heightMinMaxTextures` slot.
    pub fn default_height_minmax(&self) -> &crate::resources::DefaultHeightMinMax {
        &self.default_height_minmax
    }

    /// The shared bindless free-list every uploaded texture clones.
    pub fn bindless_free_list(&self) -> &BindlessFreeList {
        &self.bindless_free_list
    }

    /// The PSO cache (übershader request front door).
    pub fn pipelines(&mut self) -> &mut Pipelines {
        &mut self.pipelines
    }

    /// The most recent frame's draw counters (derived from the visibility readback).
    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    /// The lighting rig sub-state (the scene-lighting toggles, inspectable counters).
    pub fn lighting(&self) -> &Lighting {
        &self.lighting
    }

    /// Whether the device is a software rasterizer.
    pub fn software_gpu(&self) -> bool {
        self.software_gpu
    }

    /// The number of cached PSOs.
    pub fn pipeline_count(&self) -> u32 {
        self.pipelines.pipeline_count()
    }

    /// The high-water count of bindless texture slots claimed.
    pub fn bindless_texture_count(&self) -> u32 {
        self.descriptors.texture_count()
    }

    /// The number of reclaimed-and-free bindless slots.
    pub fn bindless_free_count(&self) -> u32 {
        self.descriptors.free_count()
    }

    /// Whether a mesh stage is available on this device.
    pub fn mesh_shader_supported(&self) -> bool {
        self.device.capabilities.mesh_shader
    }

    /// Whether the device exposes an independent compute queue family, which is what lets the
    /// render graph place an async-compute pass on its own lane.
    pub fn async_compute_queue_supported(&self) -> bool {
        self.device.compute_queue_family.is_some()
    }

    /// Whether the shaded executor is running through the mesh stage this frame.
    pub fn mesh_executor_active(&self) -> bool {
        self.mesh_executor
    }

    /// Routes the shaded executor through the mesh stage or the indexed path.
    ///
    /// Returns the executor actually in force: a device that does not qualify stays on the
    /// indexed path, so a caller cannot select a stage the device cannot run.
    pub fn set_mesh_executor(&mut self, mesh: bool) -> bool {
        self.mesh_executor = mesh && crate::mesh_executor_supported(&self.device.capabilities);
        self.mesh_executor
    }

    /// The monotonically increasing frame serial (the traversal's `frameStamp`).
    pub fn frame_serial(&self) -> u64 {
        self.frame_serial
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        let _ = self.device.wait_idle();
        self.gpu_profiler.destroy_pools(&self.device);
        self.frames.destroy(&self.device);
        // The view's shm-capture fence is a raw handle: destroy it before the view Drops its images.
        for view in &mut self.views {
            view.destroy(&self.device);
        }
        if let Some(present_sync) = self.present_sync.as_mut() {
            present_sync.destroy(&self.device);
        }
        if let Some(swapchain) = self.swapchain.as_mut() {
            swapchain.destroy(&self.device);
        }
    }
}
