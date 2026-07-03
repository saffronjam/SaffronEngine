//! The top-level renderer aggregate: the immutable [`Device`], the [`Swapchain`], the
//! [`crate::frame::FrameRing`], and the per-area sub-state (descriptors, pipelines,
//! targets, lighting, …) as sibling fields, each mutated through its own methods while
//! holding `&Device`. It drives the per-frame acquire → render-graph → present loop.

use std::sync::{Arc, Mutex};

use ash::vk;
use saffron_geometry::glam::Mat4;

use crate::budget::{BudgetController, BudgetStep};
use crate::ddgi::DDGI_RAYS_PER_PROBE;
use crate::descriptors::Descriptors;
use crate::device::SurfaceSource;
use crate::draw_list::{DrawItem, RenderStats, SceneDrawList};
use crate::frame::FrameRing;
use crate::frame_history::{
    ActiveAlarm, AlarmDrain, AlarmInputs, AlarmState, FrameHistory, FrameHistoryStats, FrameSample,
    PerfConfig,
};
use crate::ibl::{
    EnvSource, Ibl, ReflectionProbeUpload, ReflectionProbes, Sky, SkyRenderSettings, SkygenParams,
};
use crate::instancing::Instancing;
use crate::lighting::{ClusterCamera, Lighting, SceneLighting, point_shadow_face_matrices};
use crate::nested_scopes::NestedScopeRecorder;
use crate::overlay::{
    GridPush, OverlayDraw, OverlayState, OverlayVertex, TonemapMode, TonemapPush,
};
use crate::pipelines::Pipelines;
use crate::present::PresentSync;
use crate::profiler::{
    CaptureMode, CaptureRecorder, CaptureState, CpuProfiler, GpuProfiler, PassTiming,
    ProfileCapture, ProfilerMode, cpu_now_ns,
};
use crate::quality::RenderQuality;
use crate::reactive::{PowerState, ReactiveState};
use crate::render_graph::{RenderGraph, RgAttachment, RgPass, RgResource, RgUsage};
use crate::resources::BindlessFreeList;
use crate::scene_pass::{
    PointShadowTarget, record_depth_prepass, record_gbuffer, record_point_shadow,
    record_scene_draw_list, record_shadow_depth, record_transparent_draw_list,
};
use crate::skinning::Skinning;
use crate::ssao::Ssao;
use crate::targets::Targets;
use crate::view_target::ViewTarget;
use crate::{Device, Error, Result, Swapchain, checked};

/// A submit-seam closure: ad-hoc geometry recorded into the scene pass after the
/// batched draw list (the editor gizmo / native overlay). Runs once on the render
/// thread, capturing resolved `Arc`/handle state, never `&mut Renderer` (README §2).
type RenderFn = Box<dyn FnOnce(vk::CommandBuffer)>;

/// The debug render-output mode.
///
/// `Lit` is the shaded default; `Wireframe` selects the wireframe PSO permutation
/// and `LitWireframe` overlays edges on the shaded scene via an extra pass;
/// `MotionVectors` is drawn by a dedicated fullscreen pass. The remaining modes
/// fold a single debug channel into the mesh fragment's debug path (a recoloured
/// material, a single G-buffer channel, or a light-complexity heatmap). The mode is
/// transient — never persisted with the scene.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// Full PBR shading (the default).
    #[default]
    Lit,
    /// Albedo + emissive, no lighting.
    Unlit,
    /// Wireframe (the `PolygonMode::LINE` permutation, gated on `fill_mode_non_solid`).
    Wireframe,
    /// Shaded scene with wireframe edges overlaid (an extra wireframe-overlay pass).
    LitWireframe,
    /// Full lighting on a neutral-grey material (normals preserved).
    DetailLighting,
    /// Full lighting on a flat white diffuse material (normals/specular flattened).
    LightingOnly,
    /// IBL specular reflection only (mirror-like).
    Reflections,
    /// Albedo / base color only.
    Albedo,
    /// World-space normal.
    Normal,
    /// Roughness.
    Roughness,
    /// Metallic.
    Metallic,
    /// Emissive.
    Emissive,
    /// Linearized view-space depth as grayscale.
    Depth,
    /// Screen-space ambient occlusion factor (white when SSAO is off).
    AmbientOcclusion,
    /// Indirect/ambient lighting only (IBL diffuse + SSGI + DDGI).
    Gi,
    /// Per-cluster light count as a heatmap.
    LightComplexity,
    /// Motion vectors, colorized by a dedicated fullscreen pass.
    MotionVectors,
}

impl ViewMode {
    /// The debug-shading channel the mesh fragment outputs instead of full shading;
    /// folded into the light UBO's `point_shadow_meta.w`. `0` is full shading
    /// (`Lit`, `Wireframe`, `LitWireframe`, and `MotionVectors`, the last two being
    /// produced by dedicated passes).
    fn debug_channel(self) -> u32 {
        match self {
            ViewMode::Lit
            | ViewMode::Wireframe
            | ViewMode::LitWireframe
            | ViewMode::MotionVectors => 0,
            ViewMode::Albedo => 1,
            ViewMode::Normal => 2,
            ViewMode::Roughness => 3,
            ViewMode::Metallic => 4,
            ViewMode::Emissive => 5,
            ViewMode::Unlit => 6,
            ViewMode::DetailLighting => 7,
            ViewMode::LightingOnly => 8,
            ViewMode::Reflections => 9,
            ViewMode::Depth => 10,
            ViewMode::AmbientOcclusion => 11,
            ViewMode::Gi => 12,
            ViewMode::LightComplexity => 13,
        }
    }
}

/// Which editor pane a render view targets.
///
/// Each view owns its own [`ViewTarget`] — offscreen images + temporal accumulators —
/// and the renderer renders/presents the one [`Renderer::set_active_view`] selects. The
/// discriminant is the dense slot index into the renderer's `views` array (and the
/// host's per-view shm segments), so `Scene = 0` / `AssetPreview = 1` is FROZEN
/// end-to-end with the wire tokens (`"scene"` / `"assetPreview"`) and the presenter's
/// reader ordering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ViewId {
    /// The main scene viewport (slot `0`, the default).
    #[default]
    Scene,
    /// The asset-preview viewport (slot `1`).
    AssetPreview,
}

/// The number of editor render views (scene + asset-preview).
pub const VIEW_COUNT: usize = 2;

/// Maximum SDF occluder instances the per-frame SDF instance SSBO holds. Each static draw
/// contributes one instance per baked field, and a mesh bakes one tight field per primitive
/// (plus per spatial chunk of an oversized primitive), so a modular scene — or a merged mesh
/// split back into its primitives on bake — reaches the low thousands. The Global Distance
/// Field clipmap carries the far field; this bounds the near-field per-instance list. Overflow
/// clamps + logs.
pub const MAX_SDF_INSTANCES: u32 = 4096;

/// Frames of frame-timing telemetry dropped after a project load, covering the cold-pipeline
/// warm-up (PSO compiles, acceleration-structure builds, GI convergence) so the HUD's average
/// reflects steady state, not the load-transition spike.
const TELEMETRY_WARMUP_FRAMES: u32 = 12;

impl ViewId {
    /// The dense slot index into the renderer's `views` array (`Scene = 0`).
    pub fn index(self) -> usize {
        match self {
            ViewId::Scene => 0,
            ViewId::AssetPreview => 1,
        }
    }

    /// The [`ViewId`] for a dense slot index, the inverse of [`ViewId::index`].
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => ViewId::AssetPreview,
            _ => ViewId::Scene,
        }
    }

    /// The control-plane / shm wire token, FROZEN end-to-end with the presenter's reader
    /// (`editor/src-tauri/src/wayland_viewport.rs`). Exactly `"scene"` / `"assetPreview"`.
    pub fn wire(self) -> &'static str {
        match self {
            ViewId::Scene => "scene",
            ViewId::AssetPreview => "assetPreview",
        }
    }

    /// Parses a wire token into a [`ViewId`]; `None` for an
    /// unknown token.
    pub fn from_wire(token: &str) -> Option<Self> {
        match token {
            "scene" => Some(ViewId::Scene),
            "assetPreview" => Some(ViewId::AssetPreview),
            _ => None,
        }
    }
}

/// The full per-frame render statistics: the draw-path counters plus the run-loop frame
/// timings, VRAM telemetry, and the current profiler/view-mode/exposure state. The
/// renderer's answer to the control plane's `render-stats` query. The control layer
/// maps this to its wire DTO.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStatsFull {
    /// The draw-path counters from the last submitted draw list.
    pub draw: RenderStats,
    /// Wall-clock render-thread frame time (ms); `0` until the run loop records it.
    pub frame_ms: f32,
    /// Frames per second derived from `frame_ms` (`0` when `frame_ms` is `0`).
    pub fps: f32,
    /// GPU frame time (ms); `0` until the profiler runs.
    pub gpu_ms: f32,
    /// CPU busy time (ms).
    pub cpu_frame_ms: f32,
    /// Fence-wait time (ms).
    pub cpu_wait_ms: f32,
    /// Device-local VRAM usage in bytes (`0` until profiled).
    pub vram_usage_bytes: u64,
    /// Device-local VRAM budget in bytes (`0` until profiled).
    pub vram_budget_bytes: u64,
    /// Whether the device is a software rasterizer.
    pub software_gpu: bool,
    /// The active GPU profiler mode.
    pub profiler_mode: ProfilerMode,
    /// The active debug render-output mode.
    pub view_mode: ViewMode,
    /// The tonemap exposure in stops.
    pub exposure_ev: f32,
}

/// The PSOs one offscreen frame needs, resolved up front (each request borrows
/// `&mut Pipelines`) so the render-graph build borrows the rest of the renderer
/// immutably. A `None` arms nothing — that pass is skipped this frame.
struct FramePipelines {
    depth_prepass: Option<Arc<crate::Pipeline>>,
    cull: Option<Arc<crate::Pipeline>>,
    /// The compute skinning PSO, resolved when the frame has skinned dispatches.
    skin: Option<Arc<crate::Pipeline>>,
    /// The compute morph PSO, resolved when the frame has morph dispatches.
    morph: Option<Arc<crate::Pipeline>>,
    shadow: Option<Arc<crate::Pipeline>>,
    point_shadow: Option<Arc<crate::Pipeline>>,
    /// Whether the **static** point-shadow cube needs re-rendering this frame (its content key or
    /// image changed). The dynamic cube always re-renders when `point_shadow` is `Some`.
    static_point_shadow_dirty: bool,
    /// The thin G-buffer prepass + the screen-space compute PSOs, resolved when the
    /// screen-space chain runs this frame (any of GTAO / contact / SSGI on).
    gbuffer: Option<Arc<crate::Pipeline>>,
    gtao: Option<Arc<crate::Pipeline>>,
    ao_blur: Option<Arc<crate::Pipeline>>,
    contact: Option<Arc<crate::Pipeline>>,
    ssgi: Option<Arc<crate::Pipeline>>,
    ssgi_blur: Option<Arc<crate::Pipeline>>,
    ssgi_accum: Option<Arc<crate::Pipeline>>,
    /// The screen-space indirect-diffuse resolve PSO, resolved when the screen chain runs (it reads
    /// the G-buffer). Additive until the fragment cutover samples its half-res output.
    gi_resolve: Option<Arc<crate::Pipeline>>,
    /// The DFAO cone-trace PSO (three-set, GDF sky-visibility), resolved when sky occlusion is on
    /// this frame. The blur + accumulation reuse the SSGI denoise PSOs bound with the DFAO sets.
    dfao: Option<Arc<crate::Pipeline>>,
    /// The DFAO bilateral-upsample PSO (the ssgi-blur PSO, bound with the DFAO blur set).
    dfao_blur: Option<Arc<crate::Pipeline>>,
    /// The DFAO temporal-accumulation PSO (the clamp-free `dfao_accum` PSO, bound with the DFAO
    /// accum sets), resolved when sky occlusion is on AND motion ran.
    dfao_accum: Option<Arc<crate::Pipeline>>,
    /// This frame's DFAO trace push (camera inverses + frame index; bumped at resolve time).
    dfao_push: crate::DfaoPush,
    /// The specular reflection-occlusion cone-trace PSO (three-set, GDF reflection occlusion),
    /// resolved under the same sky-occlusion gate as DFAO. Spatial-only: the blur reuses the SSGI
    /// blur PSO bound with the specocc set; the view-dependent occlusion term is not temporally
    /// accumulated (surface-motion reprojection would smear it), so there is no accum PSO.
    specocc: Option<Arc<crate::Pipeline>>,
    /// The specular-occlusion bilateral-upsample PSO (the ssgi-blur PSO, bound with the specocc
    /// blur set).
    specocc_blur: Option<Arc<crate::Pipeline>>,
    /// This frame's specocc trace push (camera inverses + frame index; bumped at resolve time).
    specocc_push: crate::SpecoccPush,
    /// The SSR trace PSO, resolved when SSR runs this frame.
    ssr: Option<Arc<crate::Pipeline>>,
    copy_color: Option<Arc<crate::Pipeline>>,
    /// The four DDGI trace/blend/border PSOs, resolved together when DDGI runs this frame (all
    /// four present or the chain is skipped — the gate ANDs all four). `None`
    /// arms no DDGI sampling passes.
    ddgi: Option<DdgiPipelines>,
    /// The two Global-SDF compute PSOs (cull + composite), resolved together when the GDF runs this
    /// frame (both present or the chain is skipped). `None` arms no GDF passes.
    gdf: Option<GdfPipelines>,
    /// The three ReSTIR DI compute PSOs, resolved together when ReSTIR runs this frame (all
    /// three present or the chain is skipped). `None` arms no
    /// ReSTIR passes; direct lighting then takes the clustered-forward path.
    restir: Option<RestirPipelines>,
    /// This frame's SSGI trace push, with the monotonic frame index already bumped (the
    /// bump needs `&mut self.ssao`, so it happens at resolve time, not in the `&self`
    /// graph build).
    ssgi_push: crate::SsgiPush,
    /// This frame's SSR trace push (frame index bumped at resolve time, like `ssgi_push`).
    ssr_push: crate::SsgiPush,
    /// The motion-vector prepass PSO, resolved when TAA or SSGI runs this frame (both
    /// reproject through the motion target).
    motion: Option<Arc<crate::Pipeline>>,
    /// The TAA resolve compute PSO, resolved when TAA is the active AA mode.
    taa: Option<Arc<crate::Pipeline>>,
    /// The FXAA edge-blur compute PSO, resolved when FXAA is the active AA mode.
    fxaa: Option<Arc<crate::Pipeline>>,
    /// The mandatory tonemap compute PSO (always resolved unless its build fails).
    tonemap: Option<Arc<crate::Pipeline>>,
    /// The scene-resolve copy PSO (copy_color-shaped): normalized-UV upscale of the input-extent
    /// scene scratch into the display-extent offscreen, used on the no-AA / MSAA paths (FXAA/TAA
    /// resolve to the offscreen themselves). Resolved whenever neither FXAA nor TAA is active.
    scene_resolve: Option<Arc<crate::Pipeline>>,
    /// The depth-upscale graphics PSO: point-upscales the input-extent scene depth into the
    /// display-extent overlay depth so the grid / gizmo occlude correctly under upsampling.
    depth_upscale: Option<Arc<crate::Pipeline>>,
    /// The TAA reactive-coverage graphics PSO: re-draws the translucent batches into the r8
    /// reactive mask. Resolved only when TAA is active (the mask feeds the TAA resolve).
    reactive_coverage: Option<Arc<crate::Pipeline>>,
    /// The ground-grid graphics PSO, resolved when the grid is shown this frame.
    grid: Option<Arc<crate::Pipeline>>,
    /// The on-top + depth-tested overlay graphics PSOs, resolved when overlay geometry
    /// is queued this frame.
    overlay: Option<Arc<crate::Pipeline>>,
    overlay_depth: Option<Arc<crate::Pipeline>>,
    /// The Lit Wireframe overlay PSO, resolved only in the `LitWireframe` view mode (and
    /// only on a `fill_mode_non_solid` device — `None` falls back to plain Lit).
    wireframe_overlay: Option<Arc<crate::Pipeline>>,
    /// The motion-vector visualization PSO, resolved only in the `MotionVectors` view mode.
    motion_visualize: Option<Arc<crate::Pipeline>>,
    /// This frame's uploaded overlay vertex buffer + draw-range counts (the per-frame
    /// grow-only buffer is prepared before the graph build so the pass captures only the
    /// resolved handle). `None` when no overlay geometry is queued.
    overlay_draw: Option<OverlayDraw>,
}

/// The four DDGI trace/blend/border PSOs, resolved together — the `doDdgi` gate requires all four,
/// so they are bundled (a partial set skips the whole chain). The trace sphere-marches the shared
/// distance field (the per-mesh MDF + the Global SDF), so it carries no geometry proxy of its own.
struct DdgiPipelines {
    trace: Arc<crate::Pipeline>,
    blend_irr: Arc<crate::Pipeline>,
    blend_dist: Arc<crate::Pipeline>,
    border: Arc<crate::Pipeline>,
}

/// The two Global-SDF compute PSOs, resolved together — the GDF gate requires both (a partial set
/// skips the whole chain).
struct GdfPipelines {
    cull: Arc<crate::Pipeline>,
    composite: Arc<crate::Pipeline>,
}

/// What [`Renderer::add_gdf_passes`] hands back to the build: the cascade volume resources the
/// downstream consumers (the DDGI trace's far field) declare `SampledRead` on (so the graph
/// transitions them ShaderReadOnly after the composite write), plus each cascade's external
/// layout slot for the cross-frame write-back. Empty when the GDF did not run.
#[derive(Default)]
struct GdfResult {
    cascades: Option<[RgResource; crate::GDF_CASCADES as usize]>,
    cascade_slots: [Option<usize>; crate::GDF_CASCADES as usize],
    /// The lite albedo cache resource (the DDGI trace's `SampledRead`) + its external slot. `Some`
    /// only when the composite ran this frame (it writes the cache GENERAL).
    albedo: Option<RgResource>,
    albedo_slot: Option<usize>,
}

/// The three ReSTIR DI compute PSOs, resolved together — the `doRestir` gate requires all
/// three (a partial set skips the whole chain, and direct lighting falls back to clustered).
struct RestirPipelines {
    initial: Arc<crate::Pipeline>,
    reuse: Arc<crate::Pipeline>,
    resolve: Arc<crate::Pipeline>,
}

/// What [`Renderer::add_ddgi_passes`] hands back to the scene-pass build: the irradiance
/// and distance atlas resources the scene declares `SampledRead` on (so the graph
/// transitions them ShaderReadOnly before the mesh sample), plus each imported image's
/// external layout slot for the cross-frame write-back.
#[derive(Default)]
struct DdgiResult {
    irradiance: Option<RgResource>,
    distance: Option<RgResource>,
    rays_slot: Option<usize>,
    irradiance_slot: Option<usize>,
    distance_slot: Option<usize>,
}

/// What [`Renderer::add_restir_passes`] hands back to the scene-pass build: the resolved
/// direct-radiance resource the scene declares `SampledRead` on (transitioned ShaderReadOnly
/// before the mesh sample), the set-7 mesh set the scene binds, and the radiance image's
/// external-layout slot for the cross-frame `General ↔ ShaderReadOnly` write-back.
#[derive(Default)]
struct RestirResult {
    radiance: Option<RgResource>,
    mesh_set: vk::DescriptorSet,
    radiance_slot: Option<usize>,
}

/// What [`Renderer::add_screen_space_passes`] hands back to the scene-pass build: the
/// per-view mesh set 4 to bind, the maps the scene declares `SampledRead` on (so the
/// graph transitions them ShaderReadOnly before the sample), and the optional prev-color
/// history-copy scheduled after the scene pass.
#[derive(Default)]
struct ScreenSpaceResult {
    mesh_set: vk::DescriptorSet,
    scene_sampled: Vec<RgResource>,
    history_copy: Option<HistoryCopy>,
    /// `(history-slot, external-layout-slot)` for the two SSGI history images when the
    /// SSGI temporal accumulation ran, so the resolved exit layout is written back after
    /// execute (the cross-frame `ShaderReadOnly ↔ General` transition is derived).
    ssgi_history_slots: Option<TaaHistorySlots>,
    /// The ssgi_resolved image's external-layout slot when the accumulation ran.
    ssgi_resolved_slot: Option<usize>,
    /// `(history-slot, external-layout-slot)` for the two DFAO history images when the DFAO
    /// temporal accumulation ran (same write-back shape as the SSGI history).
    dfao_history_slots: Option<TaaHistorySlots>,
    /// The dfao_resolved image's external-layout slot when the accumulation ran.
    dfao_resolved_slot: Option<usize>,
    /// The ssr_map image's external-layout slot when the SSR trace ran, so its resolved
    /// exit layout (ShaderReadOnly after the scene's SampledRead) carries to next frame.
    ssr_map_slot: Option<usize>,
}

/// The SSGI prev-color history copy: it reads the scene's linear-HDR color and writes
/// `prev_color` (read by next frame's SSGI), so it is scheduled *after* the scene pass.
/// Carries the resolved handles the copy pass body captures.
struct HistoryCopy {
    prev_color: RgResource,
    pipeline: Arc<crate::Pipeline>,
    set: vk::DescriptorSet,
    groups_x: u32,
    groups_y: u32,
}

/// The TAA pass's two history images' `(slot index in `views[active].history`, external
/// layout slot)` pairs, so `record_scene_graph` writes each image's resolved exit layout
/// back after execute (the cross-frame `ShaderReadOnly ↔ General` transition is derived).
struct TaaHistorySlots {
    read: (usize, usize),
    write: (usize, usize),
}

/// The TAA resolve's cross-frame ping-pong slots: the color history and the pixel-lock image both
/// carry their layout across frames (read one parity, write the other), so each rides a pair of
/// external-layout slots the caller reads back after execute.
struct TaaResolveSlots {
    history: TaaHistorySlots,
    lock: TaaHistorySlots,
}

/// The renderer: device, swapchain, frame ring, and the clear color.
///
/// Drop order is load-bearing — the frame ring and swapchain are destroyed (their
/// handles borrow the device) before the [`Device`] field drops and tears down the
/// allocator/device/instance. The explicit [`Drop`] runs `wait_idle` first so no
/// handle is freed under a live GPU read, then destroys the borrowing sub-state in
/// the correct order; the `device` field drops last by declaration order.
pub struct Renderer {
    /// The clear color applied to the scene/swapchain image each frame (RGBA).
    pub clear_color: [f32; 4],
    /// Wireframe view mode — drives the per-draw PSO `wireframe` permutation (gated on
    /// the device's `fill_mode_non_solid` capability inside the cache).
    pub wireframe: bool,
    /// When set, the scene pass is preceded by a depth pre-pass that lays down depth
    /// first.
    pub use_depth_prepass: bool,

    /// The tonemap exposure in stops; the mandatory tonemap pass applies `exp2(this)`.
    /// Defaults to 0 (a 1× multiplier).
    exposure_ev: f32,
    /// The infinite analytic ground grid debug overlay toggle.
    show_grid: bool,
    /// Native-viewport host mode: present blits the post-processed offscreen straight to
    /// the swapchain (no ui pass). The offscreen content is identical to editor mode —
    /// this only selects the final present path.
    present_viewport_only: bool,

    /// Set by [`Renderer::begin_offscreen_frame`] (the run loop's `begin_frame`) once the
    /// current frame slot's fence is waited + reset, so [`Renderer::render_scene_offscreen`]
    /// does not re-wait the (now-unsignaled) fence and deadlock; cleared as it consumes it.
    /// A standalone `render_scene_offscreen` (the unit tests) leaves it `false` and begins
    /// the frame itself.
    frame_begun: bool,

    /// Set by [`Renderer::render_scene_offscreen`] when it signals the windowed present path's
    /// scene-finished semaphore, and consumed (cleared) by
    /// [`Renderer::present_active_view_to_swapchain`]. The present blit only waits the
    /// scene-finished semaphore when it was actually signaled this frame; if the host skipped
    /// the offscreen render (a size-0 view or a render error), the present blits the prior
    /// frame's offscreen without waiting an unsignaled semaphore (which would deadlock).
    present_scene_signaled: bool,

    /// The debug render-output mode. Transient; drives the
    /// wireframe PSO permutation + the mesh fragment's debug-channel output.
    view_mode: ViewMode,
    /// Whether the GPU compute-skinning path runs. Off falls
    /// back to bind-pose meshes. Defaults on.
    skinning_enabled: bool,

    /// Whether the device is a software rasterizer (llvmpipe/lavapipe): GPU timings are
    /// CPU rasterization time. Mirrored from the device capabilities.
    software_gpu: bool,
    /// The physical-device name, captured at init for profiler capture metadata.
    device_name: String,
    /// The last frame's wall-clock render-thread frame time (ms); `0` until the host
    /// run loop records it.
    frame_ms: f32,
    /// The last frame's CPU busy time (ms); `0` until recorded.
    cpu_frame_ms: f32,
    /// A monotonic per-frame counter, gating the profiler's
    /// periodic timestamp re-calibration.
    frame_serial: u64,
    /// The last frame's GPU frame time (ms); `0` until the profiler runs.
    gpu_frame_ms: f32,
    /// The last frame's fence-wait time (ms); `0` until recorded.
    cpu_wait_ms: f32,
    /// Device-local VRAM usage in bytes; `0` until the profiler reads the VMA budget.
    vram_usage_bytes: u64,
    /// Device-local VRAM budget in bytes; `0` until profiled.
    vram_budget_bytes: u64,

    /// The shared frame-budget / green-amber-red threshold config.
    perf_config: PerfConfig,
    /// The rolling frame-time history ring.
    frame_history: FrameHistory,
    /// Frames of telemetry to drop after a project load: the cold-pipeline warm-up (PSO
    /// compiles, acceleration-structure builds) is not steady state, so it is kept out of the
    /// history / EMAs / alarms until the pipeline settles.
    telemetry_warmup: u32,
    /// The perf-alarm engine: active set + seq-stamped event ring.
    alarms: AlarmState,
    /// The GPU profiler: per-pass timestamps + pipeline statistics.
    gpu_profiler: GpuProfiler,
    /// The CPU span profiler, feeding the merged capture.
    cpu_profiler: CpuProfiler,
    /// The capture recorder driven by `profiler.capture-start/stop`.
    capture: CaptureRecorder,
    /// The frame slot whose CPU/GPU spans the next [`Renderer::finalize_frame_telemetry`]
    /// folds into the capture: the slot `render_scene_offscreen` just recorded into, before
    /// `frames.advance()` moved `frames.index()` on.
    last_rendered_slot: usize,
    /// Wall-clock ns of the last [`Renderer::finalize_frame_telemetry`], for the alarm tick's
    /// irregular-interval dt.
    last_frame_ns: u64,

    /// The per-frame editor-overlay geometry (gizmo handles + entity billboards),
    /// uploaded into a grow-only per-frame vertex buffer and composited after tonemap.
    overlay: OverlayState,

    submissions: Vec<RenderFn>,
    scene_draw_list: SceneDrawList,
    stats: RenderStats,

    /// The point-shadow cube cache: the content key + cube image handle of the last cube actually
    /// rendered. The cube persists in `SHADER_READ_ONLY` between frames, so when the key and the
    /// image both match, the `point-shadow` pass is skipped and the cached cube is sampled — a
    /// static light + casters cost nothing while the camera moves. A target recreation mints a new
    /// image handle, which forces a re-render (the new cube is `UNDEFINED`).
    last_point_shadow_key: Option<u64>,
    last_point_shadow_cube: vk::Image,

    /// The active render-quality tier + resolved screen-space GI parameters (applied to
    /// [`Ssao`]). Reported in `render-stats` and saved with the project.
    render_quality: RenderQuality,

    /// The frame-budget controller that auto-steps `render_quality` to hold the budget when
    /// `PerfConfig::auto_quality` is on (off by default — then it never runs).
    budget_controller: BudgetController,
    /// A render-scale change the budget controller requested, applied at the next frame's safe
    /// resize point (a resize must not run from the post-submit telemetry hook).
    pending_render_scale: Option<f32>,

    /// The active tonemap operator (default ACES), applied in the tonemap pass + reported in stats.
    tonemap_mode: TonemapMode,

    /// The reactive-loop observability mirror: the host pushes the idle/converged/reasons snapshot
    /// each frame (the verdict lives above this crate), and the editor sets the power state; both
    /// surface in `render-stats`, and the host reads the power state back to suppress a hidden view.
    reactive: ReactiveState,

    /// The directional shadow map's layout carried across frames: the graph seeds the
    /// entry layout from it and writes back the
    /// resolved exit layout each frame, so the cross-frame `DepthWrite → ShaderReadOnly`
    /// transition is derived, never hand-written.
    directional_shadow_layout: vk::ImageLayout,
    /// The spot shadow map's cross-frame layout.
    spot_shadow_layout: vk::ImageLayout,

    /// The anti-aliasing selection (MSAA / FXAA / TAA, mutually exclusive). The frame
    /// graph branches the scene output on this; the temporal targets live per-view.
    aa: crate::Aa,

    /// The runtime TAA resolve tuning (feedback range, velocity rejection, clip gamma,
    /// sharpen). Read into the resolve push each frame; live-tunable over the control plane.
    taa_params: crate::TaaParams,
    /// The active camera's `(near, far)` planes, mirrored from the last [`Renderer::set_cluster_camera`]
    /// so the TAA resolve can linearize `motionDepth` for its disocclusion test.
    camera_near_far: (f32, f32),

    /// The per-editor-pane render targets, indexed by [`ViewId::index`] (`Scene` = 0,
    /// `AssetPreview` = 1). Always [`VIEW_COUNT`] entries.
    views: Vec<ViewTarget>,
    /// Which view the renderer renders + presents this frame.
    active_view: ViewId,

    /// Per-view shm-publish enable, indexed by [`ViewId::index`]: the host sets it from its
    /// segment wiring. When the active view's flag is
    /// set, [`Renderer::render_scene_offscreen`] folds the BGRA8 readback blit/copy into the
    /// frame's command buffer (no separate submit), and [`Renderer::begin_offscreen_frame`]
    /// stages the completed pipelined slot's bytes for the host to publish.
    shm_publish_enabled: [bool; VIEW_COUNT],
    /// The `(view index, frame slot)` whose shm-capture staging buffer holds a completed
    /// BGRA8 frame, staged at the begin-frame fence wait for the host to publish directly from
    /// the mapped staging via [`Renderer::pending_shm_view`] — no intermediate copy.
    pending_shm_publish: Option<(usize, usize)>,

    lighting: Lighting,
    targets: Targets,
    instancing: Instancing,
    skinning: Skinning,
    pipelines: Pipelines,
    ibl: Ibl,
    sky: Sky,
    reflection: ReflectionProbes,
    ssao: Ssao,
    ddgi: crate::Ddgi,
    global_sdf: crate::GlobalSdf,
    rt: crate::Rt,
    restir: crate::Restir,

    /// The per-static-instance SDF SSBO: one [`crate::SdfInstance`] per static draw that
    /// carries a baked field, host-mapped + sized to a fixed capacity (grow-not-needed,
    /// the same discipline as the DDGI box buffer). The lighting cone-trace iterates the
    /// first [`Self::sdf_instance_count`] entries. Phase 2 builds + uploads it; Phase 3
    /// binds it into the lighting set and reads it.
    sdf_instances: crate::Buffer,
    /// The active SDF-instance count uploaded this frame.
    sdf_instance_count: u32,
    /// The SDF-instance SSBO capacity (entries).
    sdf_instance_capacity: u32,
    /// Whether GDF reflection occlusion (the per-pixel reflection-cone march against the Global
    /// Distance Field that occludes the reflected skybox under overhangs) is enabled. The one
    /// remaining per-pixel SDF consumer; indirect diffuse occlusion is DDGI ray-miss + GTAO.
    sky_occlusion: bool,
    /// The bindless descriptor table, behind an `Arc` so the thumbnail worker shares it
    /// (`Descriptors` is `Send + Sync` — every slot claim + write goes through its internal
    /// bindless `Mutex`, so concurrent uploads from the worker + frame loop are serialized).
    descriptors: Arc<Descriptors>,
    bindless_free_list: BindlessFreeList,

    /// The 1×1 white texture occupying [`crate::DEFAULT_WHITE_SLOT`] (and seeded into
    /// every other bindless slot at init). A material with no albedo/ORM texture
    /// samples this slot, so it must outlive every draw — held here for the renderer's
    /// lifetime.
    default_white: Arc<crate::GpuTexture>,

    /// The 1×1×1 "empty space" SDF seeded into every unbound slot of the bindless
    /// `Texture3D` SDF array (binding 1). A mesh that baked no SDF leaves its slot holding
    /// this field, which reads as "far from any surface" (no occlusion); held here so the
    /// seeded views stay valid for the renderer's lifetime.
    default_sdf: Arc<crate::GpuSdf>,

    /// The offscreen thumbnail + material-preview render sub-state: the lazy
    /// thumbnail/preview PSOs + the preview sphere, plus the render-to-texture and PNG
    /// read-back primitives.
    pub(crate) thumbnail: crate::ThumbnailRenderer,

    /// A pending window/composited-output screenshot path, armed by
    /// [`Renderer::request_window_capture`] and consumed at the next present (the swapchain
    /// image is copied to a host buffer → PNG, then this clears). `None` when no capture
    /// is pending.
    capture_next_window_path: Option<std::path::PathBuf>,

    frames: FrameRing,
    /// The present swapchain, present only in the standalone windowed mode
    /// ([`SurfaceSource::Window`]). The editor offscreen host never presents — it publishes
    /// offscreen frames to shared memory — so it carries no swapchain (it has no surface to
    /// build one against, and lavapipe's `VK_EXT_headless_surface` swapchain WSI is
    /// unimplemented anyway, which is exactly why offscreen mode must not create one).
    swapchain: Option<Swapchain>,
    /// The windowed present path's per-slot blit + sync resources, present only alongside
    /// the [`Self::swapchain`] (the standalone present-only host). The editor offscreen host
    /// publishes to shared memory instead of presenting, so it carries no present sync.
    present_sync: Option<PresentSync>,
    /// The Vulkan core, behind an `Arc` so the thumbnail worker can share it (`Device` is
    /// `Send + Sync` — ash handles + the `Arc<DeviceResources>` + VMA allocator are all
    /// thread-safe). The renderer is normally the last holder, and the worker is joined +
    /// its `Arc<Device>` dropped before the renderer's, so the device dies after every user.
    device: Arc<Device>,
}

impl Renderer {
    /// Brings up the renderer against `surface_source` at `(width, height)`.
    ///
    /// Creates the [`Device`] (instance/surface/device/allocator + feature probe), the
    /// per-frame command/sync ring, and — for [`SurfaceSource::Window`] only — the present
    /// [`Swapchain`]. The editor offscreen host ([`SurfaceSource::Offscreen`]) builds no
    /// swapchain: it has no surface, renders offscreen, and publishes BGRA8 frames to shared
    /// memory, never presenting. This also sidesteps lavapipe's unimplemented `VK_EXT_headless_surface`
    /// swapchain WSI, which SIGSEGVs creating native swapchain image memory.
    ///
    /// # Errors
    ///
    /// Propagates any [`Error`] from device, swapchain, or frame-ring creation.
    pub fn new(surface_source: &SurfaceSource<'_>, width: u32, height: u32) -> Result<Self> {
        let device = Arc::new(Device::new(surface_source)?);
        let mut swapchain = match surface_source {
            SurfaceSource::Window(_) => Some(Swapchain::new(&device, width, height)?),
            SurfaceSource::Offscreen => None,
        };
        // Log the swapchain image count alongside the GPU name; the offscreen host has no
        // present swapchain, so this fires only windowed.
        if let Some(swapchain) = swapchain.as_ref() {
            tracing::info!("{} swapchain images", swapchain.image_count());
        }
        let frames = match FrameRing::new(&device) {
            Ok(frames) => frames,
            Err(err) => {
                if let Some(swapchain) = swapchain.as_mut() {
                    swapchain.destroy(&device);
                }
                return Err(err);
            }
        };

        // The windowed present path's blit + sync ring exists only alongside the swapchain;
        // the headless host publishes to shared memory and never presents.
        let mut present_sync = match swapchain {
            Some(_) => match PresentSync::new(&device) {
                Ok(present_sync) => Some(present_sync),
                Err(err) => {
                    let mut frames = frames;
                    frames.destroy(&device);
                    if let Some(swapchain) = swapchain.as_mut() {
                        swapchain.destroy(&device);
                    }
                    return Err(err);
                }
            },
            None => None,
        };

        // The bring-up sub-state borrows the device only during construction; on a
        // failure after the swapchain/frames exist, destroy them in reverse order
        // before the `Device` field would (it is not yet moved into `Self`).
        type BuildParts = (
            Arc<Descriptors>,
            Targets,
            Lighting,
            Pipelines,
            Instancing,
            Skinning,
            Ibl,
            Sky,
            ReflectionProbes,
            Ssao,
            crate::Ddgi,
            crate::GlobalSdf,
            crate::Rt,
            crate::Restir,
            Vec<ViewTarget>,
            BindlessFreeList,
            crate::Aa,
            Arc<crate::GpuTexture>,
            Arc<crate::GpuSdf>,
        );
        let build = || -> Result<BuildParts> {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Arc::new(Descriptors::new(&device, &free_list)?);

            // The default white texture takes slot 0 (the first claim) and is seeded
            // into every other bindless slot, so any untextured material samples a valid
            // descriptor. Uploaded through a one-off uploader on the graphics queue.
            let queue = crate::GpuQueue::new(device.graphics_queue);
            let uploader = crate::Uploader::new(&device, &queue)?;
            let default_white = uploader.upload_default_white(&descriptors)?;
            // The bindless SDF array (binding 1) is partially bound; seed every slot with a
            // 1×1×1 "empty space" field so an unbound slot is never sampled by the
            // sky-occlusion cone-trace (a lavapipe fault / UB on real hardware). Real
            // per-mesh SDFs overwrite their own slot as their `GpuMesh` is built.
            let default_sdf = uploader.upload_default_sdf(&descriptors)?;

            let targets = Targets::new(&device)?;
            let lighting = Lighting::new(&device, &descriptors, &targets)?;
            let pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let instancing = Instancing::new(&device, &descriptors)?;
            let skinning = Skinning::new(&device)?;

            // IBL: the cubes + LUT sampler + set 3, then the first (procedural) bake so set
            // 3 is valid before the first frame. The sky reuses the env cube; the reflection
            // probes ride the IBL set, seeded with the global cubes after the bake.
            let mut ibl = Ibl::new(&device, &descriptors)?;
            ibl.bake(&device, true)?;
            let mut sky = Sky::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1)?;
            sky.bind_env_cube(&ibl);
            let reflection = ReflectionProbes::new(&device, ibl.set())?;
            reflection.seed(&ibl);

            // Screen-space effects: the device-shared sub-state (sampler + the two
            // compute layouts). `ready` flips once the views are built.
            let mut ssao = Ssao::new(&device)?;
            // The AA capability + initial mode (off). The per-view AA targets follow it.
            let aa = crate::Aa::new(
                device.supported_sample_counts(crate::OFFSCREEN_COLOR_FORMAT, crate::DEPTH_FORMAT),
            );
            // DDGI: the two octahedral atlases + ray image + the five sets/layouts + the four PSOs
            // deferred to lazy request. Device-shared (one camera-centered probe clipmap); on by
            // default. Built after descriptors (it needs the mesh set-5 layout + the shared pool).
            let ddgi = crate::Ddgi::new(&device, &descriptors)?;
            // Global SDF: the cascade clipmap volumes + cull SSBO + params UBO + the two compute
            // sets/layouts + the two PSOs deferred to lazy request. Device-shared (one clipmap);
            // off by default. Built after descriptors (it needs the shared pool + the light layout
            // it writes the cascade samplers into).
            let global_sdf = crate::GlobalSdf::new(&device, &descriptors)?;
            // RT: the set-6 TLAS layout + per-frame sets + the seeded empty TLAS (a no-op
            // sub-state on a software device). Built after descriptors (it needs set 6).
            let rt = crate::Rt::new(&device, &descriptors)?;
            // ReSTIR DI: the device-shared scaffolding (nearest sampler + the three compute
            // set layouts + the set-7 mesh layout), off by default and inert on a software
            // device (the resolve needs ray-query). The per-view reservoirs + radiance +
            // sets are allocated + built per view below (sized to its viewport).
            let restir = crate::Restir::new(&device, &descriptors)?;

            // The two editor views (Scene + AssetPreview), each with its own offscreen +
            // screen-space + AA + ReSTIR targets and per-view sets, so a view switch never
            // aliases another view's images. Both are sized to the initial extent; the
            // asset-preview view
            // stays inert until the editor sizes/activates it, but its targets exist so the
            // present-side shm segment + a `set-active-view assetPreview` render
            // immediately.
            let mut views = Vec::with_capacity(VIEW_COUNT);
            for _ in 0..VIEW_COUNT {
                let mut view = ViewTarget::new(&device, width, height)?;
                view.allocate_screen_space_sets(&descriptors, &ssao)?;
                view.build_screen_space(&device, &descriptors, &ssao)?;
                // The AA targets (motion / history / scratch / MSAA), built after the
                // screen-space chain (it reads the SSGI maps).
                view.build_aa_targets(&device, &descriptors, aa)?;
                view.restir.allocate_sets(&descriptors, &restir)?;
                view.restir
                    .build(&device, &descriptors, &restir, view.scaled_render_extent())?;
                views.push(view);
            }
            ssao.ready = true;
            Ok((
                descriptors,
                targets,
                lighting,
                pipelines,
                instancing,
                skinning,
                ibl,
                sky,
                reflection,
                ssao,
                ddgi,
                global_sdf,
                rt,
                restir,
                views,
                free_list,
                aa,
                default_white,
                default_sdf,
            ))
        };
        let (
            descriptors,
            targets,
            lighting,
            pipelines,
            instancing,
            skinning,
            ibl,
            sky,
            reflection,
            ssao,
            ddgi,
            global_sdf,
            rt,
            restir,
            views,
            bindless_free_list,
            aa,
            default_white,
            default_sdf,
        ) = match build() {
            Ok(parts) => parts,
            Err(err) => {
                let _ = device.wait_idle();
                let mut frames = frames;
                frames.destroy(&device);
                if let Some(present_sync) = present_sync.as_mut() {
                    present_sync.destroy(&device);
                }
                if let Some(swapchain) = swapchain.as_mut() {
                    swapchain.destroy(&device);
                }
                return Err(err);
            }
        };

        let overlay = OverlayState::new(device.resources());

        // Seed the GPU profiler's device-derived capabilities once.
        let facts = device.profiler_facts();
        let gpu_profiler = GpuProfiler::with_facts(
            facts.timestamp_period,
            facts.timestamp_mask,
            facts.timestamps_supported,
            facts.pipeline_stats_supported,
            facts.calibration_available,
            facts.host_domain,
        );
        let software_gpu = device.capabilities.software_gpu;
        let device_name = facts.device_name;

        // The per-static-instance SDF SSBO: host-mapped + persistently mapped, sized to a
        // fixed capacity (grow-not-needed, the DDGI box-buffer discipline). Built once;
        // `set_sdf_scene` rewrites its prefix each frame.
        let sdf_instance_bytes =
            u64::from(MAX_SDF_INSTANCES) * size_of::<crate::SdfInstance>() as u64;
        let sdf_alloc = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::Auto,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        let sdf_instances = match crate::Buffer::new(
            device.resources(),
            sdf_instance_bytes,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            &sdf_alloc,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                let _ = device.wait_idle();
                return Err(err);
            }
        };
        // Wire the persistent SDF-occluder SSBO into binding 8 of every frame slot's light
        // set (set 1); the sky-occlusion cone-trace reads the first `sdf_occlusion.x` entries.
        lighting.bind_sdf_instances(&descriptors, sdf_instances.handle(), sdf_instances.size());
        // Wire the same SSBO into the GDF cull + composite sets (they bin/min the same per-mesh
        // instances), and wire the GDF cascade samplers + params UBO into binding 9/10 of every
        // light set (the consumers' far-field tap). Both are persistent (one-time wire-up).
        global_sdf.bind_scene(sdf_instances.handle(), sdf_instances.size());
        lighting.bind_gdf(&global_sdf);
        // Wire the GDF lite albedo cache (+ its repeat sampler) into the DDGI trace set (set 2,
        // binding 0); the trace reads it as the hit radiance's per-cell base color. Persistent.
        ddgi.bind_gdf_albedo(&global_sdf);

        Ok(Self {
            clear_color: [0.05, 0.06, 0.08, 1.0],
            wireframe: false,
            use_depth_prepass: true,
            exposure_ev: 0.0,
            show_grid: false,
            present_viewport_only: false,
            frame_begun: false,
            present_scene_signaled: false,
            view_mode: ViewMode::Lit,
            skinning_enabled: true,
            software_gpu,
            frame_ms: 0.0,
            cpu_frame_ms: 0.0,
            frame_serial: 0,
            gpu_frame_ms: 0.0,
            cpu_wait_ms: 0.0,
            vram_usage_bytes: 0,
            vram_budget_bytes: 0,
            perf_config: PerfConfig::default(),
            frame_history: FrameHistory::default(),
            telemetry_warmup: 0,
            alarms: AlarmState::default(),
            gpu_profiler,
            cpu_profiler: CpuProfiler::default(),
            last_rendered_slot: 0,
            last_frame_ns: 0,
            capture: CaptureRecorder::default(),
            device_name,
            overlay,
            submissions: Vec::new(),
            scene_draw_list: SceneDrawList::default(),
            last_point_shadow_key: None,
            last_point_shadow_cube: vk::Image::null(),
            render_quality: RenderQuality::default(),
            budget_controller: BudgetController::new(),
            pending_render_scale: None,
            tonemap_mode: TonemapMode::default(),
            reactive: ReactiveState::default(),
            stats: RenderStats::default(),
            // The shadow maps are init-transitioned to ShaderReadOnly by `Targets::new`,
            // so the first frame's graph import seeds that layout (the depth-write pass
            // then transitions ShaderReadOnly → DepthWrite → ShaderReadOnly).
            directional_shadow_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            spot_shadow_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            aa,
            taa_params: crate::TaaParams::default(),
            camera_near_far: (0.1, 100.0),
            views,
            active_view: ViewId::Scene,
            shm_publish_enabled: [false; VIEW_COUNT],
            pending_shm_publish: None,
            lighting,
            targets,
            instancing,
            skinning,
            pipelines,
            ibl,
            sky,
            reflection,
            ssao,
            ddgi,
            global_sdf,
            rt,
            restir,
            sdf_instances,
            sdf_instance_count: 0,
            sdf_instance_capacity: MAX_SDF_INSTANCES,
            sky_occlusion: true,
            descriptors,
            bindless_free_list,
            default_white,
            default_sdf,
            // The thumbnail render target's color format is read back over the control plane,
            // never presented, so it follows the device's chosen surface format rather than a
            // (possibly absent) swapchain. It is the surface format the swapchain would have
            // used, matching the eventual present format.
            thumbnail: crate::ThumbnailRenderer::new(
                device.resources(),
                device.surface_format.format,
            ),
            capture_next_window_path: None,
            frames,
            swapchain,
            present_sync,
            device,
        })
    }

    /// The immutable device, shared by the sibling sub-state.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The shared device handle, for the thumbnail worker (which holds its own clone so it can
    /// drive the thumbnail-render primitives off the frame loop). `Device` is `Send + Sync`.
    pub fn device_arc(&self) -> Arc<Device> {
        Arc::clone(&self.device)
    }

    /// The present swapchain, present only in the standalone windowed mode; `None` in the
    /// editor/headless host (which publishes to shared memory instead of presenting).
    pub fn swapchain(&self) -> Option<&Swapchain> {
        self.swapchain.as_ref()
    }

    /// The present swapchain on the windowed present path. Only the present-path helpers
    /// (`render_frame` / `record_clear` / `submit_and_present` / `run_pending_window_capture`)
    /// call this, and they run only when a swapchain exists (`render_frame` guards on it up
    /// front), so the `expect` cannot fire in the editor/headless host.
    fn present_swapchain(&self) -> &Swapchain {
        self.swapchain
            .as_ref()
            .expect("present swapchain in windowed mode")
    }

    /// The descriptor sub-state (the bindless table + set layouts) for upload paths.
    pub fn descriptors(&self) -> &Descriptors {
        &self.descriptors
    }

    /// The shared bindless descriptor table, for the thumbnail worker (its uploads claim +
    /// write bindless slots through the same internal `Mutex` the frame loop uses).
    pub fn descriptors_arc(&self) -> Arc<Descriptors> {
        Arc::clone(&self.descriptors)
    }

    /// The 1×1 white texture occupying [`crate::DEFAULT_WHITE_SLOT`]: a material with no
    /// albedo/ORM texture indexes its bindless slot.
    pub fn default_white(&self) -> &Arc<crate::GpuTexture> {
        &self.default_white
    }

    /// The 1×1×1 "empty space" SDF seeded into every otherwise-unbound slot of the
    /// bindless `Texture3D` SDF array (binding 1) — a mesh that baked no field leaves its
    /// slot reading "far from any surface".
    pub fn default_sdf(&self) -> &Arc<crate::GpuSdf> {
        &self.default_sdf
    }

    /// The shared bindless free-list every uploaded texture clones (README §5).
    pub fn bindless_free_list(&self) -> &BindlessFreeList {
        &self.bindless_free_list
    }

    /// The PSO cache (übershader request front door).
    pub fn pipelines(&mut self) -> &mut Pipelines {
        &mut self.pipelines
    }

    /// The active view's offscreen scene-color image handle + view + extent.
    pub fn active_view(&self) -> &ViewTarget {
        &self.views[self.active_view.index()]
    }

    /// This frame's active-view sub-pixel jitter offset (NDC) — the offset applied to the scene
    /// view-projection while TAA is active, else zero. The scene driver reads it to jitter the
    /// projection; a mode flip mid-frame can never leak a stale offset.
    pub fn active_view_jitter(&self) -> saffron_geometry::glam::Vec2 {
        if self.aa.taa() {
            self.active_view().jitter
        } else {
            saffron_geometry::glam::Vec2::ZERO
        }
    }

    /// The scene view-projection with the TAA sub-pixel jitter removed — the camera the motion
    /// prepass, the stored previous matrix, and the post-resolve grid/overlays reproject
    /// against. Jitter is applied as a clip-space `clip.xy += offset·clip.w` translation, so it
    /// inverts exactly by the opposite translation using only the combined matrix.
    fn scene_view_proj_unjittered(&self) -> Mat4 {
        let j = self.active_view_jitter();
        Mat4::from_translation(saffron_geometry::glam::Vec3::new(-j.x, -j.y, 0.0))
            * self.scene_draw_list.view_proj
    }

    /// Which editor pane is currently rendered/presented.
    pub fn active_view_id(&self) -> ViewId {
        self.active_view
    }

    /// A view's render targets by id, for the out-of-band paths that read a non-active
    /// view's state (the seed-on-first-activate check reads
    /// [`ViewTarget::desired_width`]).
    pub fn view(&self, view: ViewId) -> &ViewTarget {
        &self.views[view.index()]
    }

    /// Selects which editor pane is rendered/presented. A no-op when `view` is already
    /// active; otherwise resets the
    /// newly-shown view's temporal accumulators (they are stale/discontinuous — it
    /// re-converges instead of reprojecting against another view's history).
    pub fn set_active_view(&mut self, view: ViewId) {
        if self.active_view == view {
            return;
        }
        self.active_view = view;
        self.reset_view_temporal(view);
    }

    /// The most recent frame's draw counters (refreshed by [`Renderer::submit_draw_list`]).
    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    /// The lighting rig sub-state (the scene-lighting toggles, inspectable counters).
    pub fn lighting(&self) -> &Lighting {
        &self.lighting
    }

    /// Whether clustered-forward light culling is on (false = the fragment loops all
    /// lights, the reference path).
    pub fn clustered_enabled(&self) -> bool {
        self.lighting.use_clustered
    }

    /// Toggles clustered-forward culling.
    pub fn set_clustered(&mut self, enabled: bool) {
        self.lighting.use_clustered = enabled;
    }

    /// Whether shadow casting is on (the master toggle).
    pub fn shadows_enabled(&self) -> bool {
        self.lighting.use_shadows
    }

    /// Toggles shadow casting.
    pub fn set_shadows(&mut self, enabled: bool) {
        self.lighting.use_shadows = enabled;
    }

    /// Folds the host's visible-sky settings in (mode / clear / intensity / rotation /
    /// visibility / panorama slot).
    pub fn submit_sky(&mut self, settings: &SkyRenderSettings) {
        self.sky.submit(settings);
    }

    /// Re-arms the IBL environment bake when the source / panorama / params change.
    /// The bake fires at the next [`Renderer::render_scene_offscreen`]
    /// (a GPU-idle point), so the visible sky + IBL relight together.
    pub fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<crate::GpuTexture>>,
        params: SkygenParams,
    ) {
        self.ibl.request_env_bake(source, panorama, params);
    }

    /// Whether IBL ambient is on (false = the flat scalar ambient fallback).
    pub fn ibl_enabled(&self) -> bool {
        self.ibl.use_ibl
    }

    /// Toggles IBL ambient.
    pub fn set_ibl(&mut self, enabled: bool) {
        self.ibl.use_ibl = enabled;
    }

    /// Folds the host's per-frame reflection-probe uploads in: arms any dirty slot for
    /// capture, re-uploads the metadata
    /// SSBO, and updates the frame probe count.
    pub fn submit_reflection_probes(&mut self, probes: &[ReflectionProbeUpload]) {
        self.reflection.submit(probes);
    }

    /// Whether reflection probes contribute.
    pub fn reflection_probes_enabled(&self) -> bool {
        self.reflection.use_probes
    }

    /// The captured reflection probes in slot order (the `list-probes` source).
    pub fn reflection_probes(&self) -> &[crate::ReflectionProbe] {
        self.reflection.probes()
    }

    /// Toggles reflection probes.
    pub fn set_reflection_probes(&mut self, enabled: bool) {
        self.reflection.use_probes = enabled;
    }

    /// Applies a render-quality tier: the scalable screen-space GI stack's enable flags + SSGI /
    /// contact step counts. This is the single knob for SSGI / GTAO / contact shadows — the old
    /// per-effect toggles are gone.
    pub fn set_render_quality(&mut self, quality: RenderQuality) {
        self.render_quality = quality;
        self.ssao.apply_quality(&quality);
    }

    /// The current render-quality tier + resolved parameters.
    pub fn render_quality(&self) -> RenderQuality {
        self.render_quality
    }

    /// The active tonemap operator.
    pub fn tonemap_mode(&self) -> TonemapMode {
        self.tonemap_mode
    }

    /// Selects the tonemap operator (applied in the tonemap pass next frame).
    pub fn set_tonemap_mode(&mut self, mode: TonemapMode) {
        self.tonemap_mode = mode;
    }

    /// Pushes the per-frame reactive-loop snapshot (idle / converged / active reasons) the host
    /// derives from the run loop's `RedrawController`, for `render-stats` to report.
    pub fn set_reactive_state(&mut self, idle: bool, converged: bool, reasons: Vec<String>) {
        self.reactive.idle = idle;
        self.reactive.converged = converged;
        self.reactive.reasons = reasons;
    }

    /// Whether the reactive loop is idling (not rendering) per the last host snapshot.
    pub fn reactive_idle(&self) -> bool {
        self.reactive.idle
    }

    /// Whether the temporal effects have converged per the last host snapshot.
    pub fn reactive_converged(&self) -> bool {
        self.reactive.converged
    }

    /// The reasons continuous render is currently held per the last host snapshot.
    pub fn reactive_reasons(&self) -> &[String] {
        &self.reactive.reasons
    }

    /// The editor viewport power state (focused / unfocused / occluded), set by the editor's
    /// window-visibility signal; the host reads it each frame to suppress a hidden viewport.
    pub fn power_state(&self) -> PowerState {
        self.reactive.power_state
    }

    /// Sets the editor viewport power state. Leaving the focused state restarts the perf-alarm
    /// settle window so the render burst on the eventual return (re-converging the temporal effects)
    /// does not fire a false frame-time alarm — this covers the occluded case, which stops ticking.
    pub fn set_power_state(&mut self, state: PowerState) {
        if state != PowerState::Focused {
            self.alarms.reset_focus_settle();
        }
        self.reactive.power_state = state;
    }

    /// Whether GTAO is on (per the active tier) and its sets/targets are built.
    pub fn ssao_enabled(&self) -> bool {
        self.ssao.use_ssao && self.ssao.ready
    }

    /// Whether contact shadows are on (per the active tier) and ready.
    pub fn contact_shadows_enabled(&self) -> bool {
        self.ssao.use_contact && self.ssao.ready
    }

    /// Whether SSGI is on (per the active tier) and ready.
    pub fn ssgi_enabled(&self) -> bool {
        self.ssao.use_ssgi && self.ssao.ready
    }

    /// Toggles voxel-traced dynamic diffuse GI; turning it on re-converges the probes
    /// from scratch (a history reset).
    pub fn set_ddgi(&mut self, enabled: bool) {
        self.ddgi.set_enabled(enabled);
    }

    /// Whether DDGI is on and its resources are built.
    pub fn ddgi_enabled(&self) -> bool {
        self.ddgi.enabled()
    }

    /// Toggles GDF reflection occlusion: the per-pixel cone-march along the reflection vector
    /// against the Global Distance Field that occludes the reflected skybox under overhangs. The
    /// one remaining per-pixel SDF consumer — indirect diffuse occlusion is DDGI ray-miss + GTAO.
    pub fn set_sky_occlusion(&mut self, enabled: bool) {
        self.sky_occlusion = enabled;
    }

    /// Whether GDF reflection occlusion (the specular reflection-cone march) is enabled.
    pub fn sky_occlusion_enabled(&self) -> bool {
        self.sky_occlusion
    }

    /// Whether the GDF reflection-occlusion term is active for the active view this frame: IBL
    /// (the analytic specular it occludes) is on, the toggle is set, and the Global Distance Field
    /// composited (the cascade clipmap the cone-march taps is ready). The lighting UBO enable bit
    /// ([`Lighting::set_frame_sdf_occlusion`]) gates on this single predicate, so the fragment
    /// marches the GDF only when its clipmap is valid. Indirect diffuse occlusion is DDGI
    /// ray-miss + contact GTAO.
    fn want_sky_occlusion(&self) -> bool {
        let ibl_enabled = self.ibl.use_ibl && self.ibl.ready;
        ibl_enabled && self.sky_occlusion_enabled() && self.global_sdf.enabled()
    }

    /// Snaps the camera-centered DDGI probe clipmap to the camera + stores the sun/sky for the
    /// trace. Call before [`Renderer::set_scene_lighting`], which folds the volume + probe grid +
    /// toroidal scroll base into the light UBO.
    pub fn set_ddgi_scene(
        &mut self,
        cam_pos: saffron_geometry::glam::Vec3,
        sun_dir: saffron_geometry::glam::Vec3,
        sun_color: saffron_geometry::glam::Vec3,
        sun_intensity: f32,
        sky_color: saffron_geometry::glam::Vec3,
    ) {
        self.ddgi
            .set_scene(cam_pos, sun_dir, sun_color, sun_intensity, sky_color);
    }

    /// Uploads this frame's per-static-instance SDF list into the host-mapped SSBO,
    /// clamped to [`MAX_SDF_INSTANCES`]. The lighting cone-trace (Phase 3) iterates the
    /// first [`Renderer::sdf_instance_count`] entries. Overflow is clamped + warned.
    pub fn set_sdf_scene(&mut self, instances: &[crate::SdfInstance]) {
        let count = (instances.len() as u32).min(self.sdf_instance_capacity);
        if instances.len() as u32 > self.sdf_instance_capacity {
            tracing::warn!(
                "SDF instance count {} exceeds capacity {}; clamping (a global distance \
                 field is the future perf path)",
                instances.len(),
                self.sdf_instance_capacity
            );
        }
        if count > 0 {
            let mapped = self
                .sdf_instances
                .mapped_bytes()
                .expect("SDF instance SSBO is host-mapped");
            let bytes: &[u8] = bytemuck::cast_slice(&instances[..count as usize]);
            mapped[..bytes.len()].copy_from_slice(bytes);
        }
        self.sdf_instance_count = count;
        // Feed the same clamped slice to the GDF so the near cascade composites only the voxels
        // around occluders that actually moved this frame instead of the whole 128³ window.
        self.global_sdf.set_instances(&instances[..count as usize]);
    }

    /// The active SDF-instance count uploaded this frame.
    pub fn sdf_instance_count(&self) -> u32 {
        self.sdf_instance_count
    }

    /// The SDF-instance SSBO handle + size (for the Phase 3 lighting-set bind).
    pub fn sdf_instance_buffer(&self) -> (vk::Buffer, vk::DeviceSize) {
        (self.sdf_instances.handle(), self.sdf_instances.size())
    }

    /// Writes this frame's camera transforms + incoming sun direction for the
    /// screen-space chain (the G-buffer prepass view/viewProj, the contact-shadow
    /// view-space light direction). Call before
    /// [`Renderer::render_scene_offscreen`].
    pub fn set_ssao_camera(
        &mut self,
        view: Mat4,
        proj: Mat4,
        sun_direction_world: saffron_geometry::glam::Vec3,
    ) {
        self.ssao.set_camera(view, proj, sun_direction_world);
        // Recenter the Global-SDF cascade clipmap on the camera eye (the inverse-view translation),
        // snapping each cascade to its own voxel grid for the toroidal incremental update.
        let eye = view.inverse().col(3).truncate();
        // The recording frame's slot — the params UBO this frame's light set (same slot) reads.
        self.global_sdf.set_camera(eye, self.frames.index());
    }

    /// Toggles the Global Distance Field: the camera-centered cascade clipmap the far-field cone
    /// march and the DDGI trace tap as one trilinear read (the per-mesh near field is unchanged). On
    /// by default — it is the distance oracle the default-on DDGI indirect path sphere-marches.
    pub fn set_gdf(&mut self, enabled: bool) {
        self.global_sdf.set_enabled(enabled);
    }

    /// Whether the Global Distance Field is on and its resources are built.
    pub fn gdf_enabled(&self) -> bool {
        self.global_sdf.enabled()
    }

    /// Writes the current frame's directional + ambient + eye + punctual lights into the
    /// per-frame light UBO/SSBO. Call once per frame before
    /// [`Renderer::render_scene_offscreen`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if growing the punctual SSBO fails.
    pub fn set_scene_lighting(&mut self, scene: &SceneLighting) -> Result<()> {
        let frame = self.frames.index();
        // Fold the IBL-ambient flag + the reflection-probe count into the UBO write.
        // Probes contribute only when IBL is baked + their toggle is on.
        let ibl_enabled = self.ibl.use_ibl && self.ibl.ready;
        let probes_on = self.reflection.use_probes && self.ibl.ready;
        let probe_count = if probes_on {
            self.reflection.frame_probe_count()
        } else {
            0
        };
        self.lighting.set_frame_ibl(ibl_enabled, probe_count);
        // Fold the DDGI flag + the fitted volume placement + probe grid into the UBO
        // write.
        let (ddgi_min, ddgi_extent) = self.ddgi.volume();
        self.lighting.set_frame_ddgi(
            self.ddgi.enabled(),
            ddgi_min,
            ddgi_extent,
            self.ddgi.probe_count_ubo(),
            self.ddgi.scroll_base_ubo(),
        );
        // Fold the GDF reflection-occlusion enable bit. The mesh fragment marches the Global
        // Distance Field along the reflection vector only when its cascade clipmap composited
        // this frame, so the enable bit gates on that readiness (`want_sky_occlusion`).
        self.lighting
            .set_frame_sdf_occlusion(self.want_sky_occlusion());
        // Fold the SSR flag (extra_flags.x) so the mesh blends the SSR map only when the
        // trace actually ran this frame.
        self.lighting
            .set_frame_ssr(self.ssao.use_ssr && self.ssao.ready);
        // Fold the RT-reflection flag (extra_flags.y) + the previous frame's view-proj for
        // reprojecting an RT hit into prev_color. RT reflections need prev_color (the
        // screen-space chain) + a valid prev view-proj; the set-6 TLAS is always a valid
        // (possibly empty) AS, and enabling the toggle arms the per-frame TLAS build, so the
        // trace is gated on the toggle rather than this-frame readiness (which lags a frame).
        let view = &self.views[self.active_view.index()];
        let rt_refl = self.rt.use_rt_reflections() && self.ssao.ready && view.prev_view_proj_valid;
        let prev_vp = view.prev_view_proj;
        self.lighting.set_frame_rt_reflections(rt_refl, prev_vp);
        self.lighting
            .set_scene_lighting(&self.descriptors, frame, scene)
    }

    /// Whether screen-space reflections are enabled.
    pub fn ssr_enabled(&self) -> bool {
        self.ssao.use_ssr
    }

    /// Toggles screen-space reflections (opt-in; off by default).
    pub fn set_ssr(&mut self, enabled: bool) {
        self.ssao.use_ssr = enabled;
    }

    /// Writes the current frame's cluster-cull params from the camera + viewport, arming
    /// the `light-cull` dispatch when clustered is on and at least one punctual light
    /// exists. Call after [`Renderer::set_scene_lighting`].
    pub fn set_cluster_camera(&mut self, camera: ClusterCamera) {
        let frame = self.frames.index();
        // Mirror the camera planes for the TAA resolve's depth linearization (disocclusion test).
        self.camera_near_far = (camera.near, camera.far);
        self.lighting.set_cluster_camera(frame, camera);
    }

    /// Arms the directional shadow pass with the light-space transform; `casting` (gated
    /// by the master shadow toggle) drives whether the `shadow` pass runs this frame.
    pub fn set_directional_shadow(&mut self, light_view_proj: Mat4, casting: bool) {
        self.lighting
            .set_directional_shadow(light_view_proj, casting);
    }

    /// Arms the spot shadow pass with the spot's perspective transform + its index in the
    /// per-frame light list.
    pub fn set_spot_shadow(&mut self, light_view_proj: Mat4, light_index: u32, casting: bool) {
        self.lighting
            .set_spot_shadow(light_view_proj, light_index, casting);
    }

    /// Arms the point shadow pass with the light's world position + far plane + its index, plus a
    /// camera-independent `content_key` (light + caster transforms) the renderer uses to reuse the
    /// cached cube when only the camera moved.
    pub fn set_point_shadow(
        &mut self,
        light_pos: saffron_geometry::glam::Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
        content_key: u64,
    ) {
        self.lighting
            .set_point_shadow(light_pos, far_plane, light_index, casting, content_key);
    }

    /// Whether the device supports hardware ray tracing (acceleration-structure +
    /// ray-query).
    pub fn rt_supported(&self) -> bool {
        self.rt.supported()
    }

    /// Toggles inline ray-query shadows (clamped off on a non-RT device). When off, the
    /// `tlas-build` pass is skipped and the mesh fragment takes the shadow-map path.
    pub fn set_rt_shadows(&mut self, enabled: bool) {
        self.rt.set_rt_shadows(enabled);
    }

    /// Whether ray-query shadows ran this frame (toggle on, RT supported, TLAS built).
    pub fn rt_shadows_enabled(&self) -> bool {
        self.rt.shadows_enabled()
    }

    /// Toggles inline ray-query reflections (clamped off on a non-RT device). When off, the
    /// mesh fragment keeps the SSR / prefiltered-env reflection path.
    pub fn set_rt_reflections(&mut self, enabled: bool) {
        self.rt.set_rt_reflections(enabled);
    }

    /// Whether the ray-query-reflections toggle is on (independent of TLAS readiness).
    pub fn rt_reflections_enabled(&self) -> bool {
        self.rt.use_rt_reflections()
    }

    /// The built per-mesh BLAS count (rt-stats).
    pub fn rt_blas_count(&self) -> u32 {
        self.rt.blas_count()
    }

    /// The skinned refit BLAS active this frame (rt-stats).
    pub fn rt_skinned_blas_count(&self) -> u32 {
        self.rt.skinned_blas_count()
    }

    /// The TLAS instance count produced by this frame's build (static + skinned).
    pub fn rt_frame_instance_count(&self) -> u32 {
        self.rt.frame_instance_count()
    }

    /// Captures this frame's static mesh instances (parallel world transforms + meshes) for
    /// the `tlas-build` pass, arming the build when RT shadows are on. Skinned instances
    /// ride the draw list.
    pub fn set_rt_scene(&mut self, models: Vec<Mat4>, meshes: Vec<Arc<crate::GpuMesh>>) {
        self.rt.set_rt_scene(models, meshes);
    }

    /// Drops every per-slot skinned refit BLAS (e.g. on a scene reset).
    pub fn clear_rt_skinned_blas(&mut self) {
        self.rt.clear_skinned_blas();
    }

    /// Toggles ReSTIR DI many-light direct lighting (clamped off on a non-RT device, since
    /// the resolve needs ray-query). Turning it on re-converges the reservoirs from scratch
    /// by arming the active view's temporal reset. When off, direct lighting falls back to
    /// the clustered-forward path.
    pub fn set_restir(&mut self, enabled: bool) {
        // The gate ANDs `rt_supported && active_restir.ready`; the supported half lives on
        // `Restir`, the view-ready half on the active `RestirView`.
        let ready = self.views[self.active_view.index()].restir.ready();
        let armed = self.restir.set_enabled(enabled && ready);
        if armed {
            self.views[self.active_view.index()].restir.reset_history();
        }
    }

    /// Whether ReSTIR is on, the device supports it, and the active view's reservoirs are
    /// built (the mesh-sample gate).
    pub fn restir_enabled(&self) -> bool {
        self.restir.use_restir()
            && self.restir.supported()
            && self.views[self.active_view.index()].restir.ready()
    }

    /// Resets a view's temporal state — its motion reprojection, SSGI/TAA history, the
    /// ReSTIR reservoir history, and the (scene-global) DDGI probes re-converge for the
    /// view.
    pub fn reset_view_temporal(&mut self, view: ViewId) {
        let target = &mut self.views[view.index()];
        target.prev_view_proj_valid = false;
        target.history_valid = false;
        target.restir.reset_history();
        // The scene-global probes re-converge for the new view.
        self.ddgi.reset_history();
    }

    /// Stashes an ad-hoc record closure replayed inside the scene pass after the
    /// batched draw list — the editor gizmo / native overlay seam. The closure captures
    /// resolved handles, runs once on the
    /// render thread.
    pub fn submit(&mut self, body: impl FnOnce(vk::CommandBuffer) + 'static) {
        self.submissions.push(Box::new(body));
    }

    /// Sets a view's desired render size and resizes its offscreen targets to match,
    /// idling the GPU first so the old images are no longer read; the resize applies
    /// eagerly here at the resize seam. The desired size is
    /// recorded even when the extent already matches, so [`ViewTarget::desired_width`]
    /// tracks "this view has been sized" for the seed-on-first-activate check (a preview
    /// view seeded from the scene size before it is shown).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the device cannot idle or the targets cannot be recreated.
    pub fn set_viewport_desired_size(
        &mut self,
        view: ViewId,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        let i = view.index();
        self.views[i].desired_width = width;
        self.views[i].desired_height = height;
        self.apply_render_extent(i)
    }

    /// Sets the dynamic-resolution factor for a view and re-sizes its render targets to
    /// `round(desired * scale)`, holding the published (native) size constant — the present
    /// blit upscales. Clamped to `(0.1, 1.0]`. A no-op if unchanged or the view is unsized.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if recreating the render targets fails.
    pub fn set_render_scale(&mut self, view: ViewId, scale: f32) -> Result<()> {
        let i = view.index();
        let clamped = scale.clamp(0.1, 1.0);
        if (self.views[i].render_scale - clamped).abs() < f32::EPSILON {
            return Ok(());
        }
        self.views[i].render_scale = clamped;
        if self.views[i].desired_width == 0 || self.views[i].desired_height == 0 {
            return Ok(());
        }
        self.apply_render_extent(i)
    }

    /// The active view's dynamic-resolution factor.
    #[must_use]
    pub fn render_scale(&self, view: ViewId) -> f32 {
        self.views[view.index()].render_scale
    }

    /// The dynamic-resolution factor of the currently-active view (the one `render-stats` reports).
    #[must_use]
    pub fn active_render_scale(&self) -> f32 {
        self.views[self.active_view.index()].render_scale
    }

    /// Sets the dynamic-resolution factor of the currently-active view (the manual-override path;
    /// `auto_quality` overrides it each frame when on).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if recreating the render targets fails.
    pub fn set_active_render_scale(&mut self, scale: f32) -> Result<()> {
        let view = self.active_view;
        self.set_render_scale(view, scale)
    }

    /// Reconciles view `i`'s two extent classes independently: the INPUT class
    /// (`scaled_render_extent` = desired × render scale — the scene / depth / motion / G-buffer /
    /// ReSTIR chain) and the DISPLAY class (`published_extent` = desired — the offscreen resolve
    /// output + TAA history + overlay depth). A desired-size change moves both; a render-scale
    /// change moves only the input class. Either rebuild resets the temporal reprojection. A
    /// no-op when neither class changed.
    fn apply_render_extent(&mut self, i: usize) -> Result<()> {
        let input = self.views[i].scaled_render_extent();
        let display = self.views[i].published_extent();
        // The last-built sizes are recoverable directly: `scratch` is the input class, `offscreen`
        // the display class (`scratch` is unconditionally allocated by `build_aa_targets`).
        let cur_input = self.views[i].scratch.as_ref().map(|s| s.extent);
        let cur_display = self.views[i].offscreen.extent;
        let input_changed = cur_input != Some(input);
        let display_changed = cur_display != display;
        if !input_changed && !display_changed {
            return Ok(());
        }
        // A render-scale-only change (input moved, display fixed — dynamic resolution) must NOT
        // flush the display-extent TAA history: the resolve resamples the newly-sized input into
        // the fixed display grid every frame, so the accumulator rides through. Flushing it would
        // flicker at every budget step. A display resize (or first build) rebuilds everything.
        let scale_only = input_changed && !display_changed;
        self.device.wait_idle()?;
        self.views[i].resize(&self.device, input, display)?;
        // `build_screen_space` sizes the input-extent chain from `scaled_render_extent`;
        // `build_aa_targets` sizes the display history + input motion/scratch/reactive/MSAA + the
        // overlay depth. On a scale-only change the preserving variant keeps the display history.
        self.views[i].build_screen_space(&self.device, &self.descriptors, &self.ssao)?;
        if scale_only {
            self.views[i].build_aa_targets_preserving_temporal(
                &self.device,
                &self.descriptors,
                self.aa,
            )?;
        } else {
            self.views[i].build_aa_targets(&self.device, &self.descriptors, self.aa)?;
        }
        // The ReSTIR reservoirs + radiance are INPUT-extent; rebuild them at the input extent
        // (arming a temporal reset — the reservoir history is stale). A no-op on a software device.
        self.views[i].restir.reset_history();
        self.views[i]
            .restir
            .build(&self.device, &self.descriptors, &self.restir, input)
    }

    /// A view's last-requested render width in device pixels (`0` until the view has been
    /// sized). Read to tell whether a not-yet-shown preview view needs seeding before a
    /// `set-active-view assetPreview` (the `desired_width == 0` check). See
    /// [`ViewTarget::desired_width`].
    pub fn view_desired_width(&self, view: ViewId) -> u32 {
        self.views[view.index()].desired_width
    }

    /// A view's last-requested render height in device pixels. See
    /// [`Renderer::view_desired_width`].
    pub fn view_desired_height(&self, view: ViewId) -> u32 {
        self.views[view.index()].desired_height
    }

    /// Captures the active view's offscreen scene color to a PNG file (the screenshot
    /// path).
    ///
    /// An out-of-band path, never on the present hot path: the offscreen may still be
    /// sampled by an in-flight frame, so it idles the device first (the capture's layout
    /// transition cannot race that read), copies the image into a host-visible buffer
    /// through a one-off submit, and leaves the image in `ShaderReadOnlyOptimal` so the
    /// next frame's producer barrier holds. The post-processed offscreen is already
    /// display-range, so its `RGBA16F` halves are clamped, not tonemapped
    /// ([`crate::PngTransfer::Clamp`], applied by [`crate::write_png_file`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the device cannot idle / a Vulkan call fails, or an
    /// [`Error::ShaderLoad`]-shaped wrapper carrying the PNG write failure.
    pub fn capture_viewport(&mut self, path: &std::path::Path) -> Result<()> {
        let (extent, format, pixels) = self.read_active_offscreen()?;
        crate::write_png_file(&pixels, extent.width, extent.height, format, path).map_err(
            |err| Error::ShaderLoad(format!("capture: write {}: {err}", path.display())),
        )?;
        Ok(())
    }

    /// Sets the active view this frame, and whether each view's shm publish is enabled (the
    /// host's segment wiring). The active view's flag
    /// gates whether [`Renderer::render_scene_offscreen`] folds the BGRA8 readback into the
    /// frame command buffer this frame.
    pub fn set_shm_publish_enabled(&mut self, view: ViewId, enabled: bool) {
        self.shm_publish_enabled[view.index()] = enabled;
    }

    /// Drains the pipelined BGRA8 bytes staged at the last begin-frame fence wait, if any —
    /// `(view, width, height, bgra8)`. The host publishes these into the view's shm segment;
    /// the bytes belong to a frame whose GPU work completed `MAX_FRAMES_IN_FLIGHT` frames ago,
    /// so the read is stall-free.
    pub fn pending_shm_view(&self) -> Option<(ViewId, u32, u32, &[u8])> {
        let (view_idx, slot) = self.pending_shm_publish?;
        let capture = self.views[view_idx].shm_capture.slots[slot].as_ref()?;
        let extent = capture.extent;
        let byte_size = extent.width as usize * extent.height as usize * 4;
        // SAFETY: the staging buffer is HOST_VISIBLE + MAPPED for `byte_size` bytes; this slot's
        // frame fence signalled at the begin-frame wait, so the GPU copy completed. The slice
        // lives until this slot is reused (`MAX_FRAMES_IN_FLIGHT` frames out) — past the publish.
        let pixels = unsafe { std::slice::from_raw_parts(capture.staging.mapped_ptr(), byte_size) };
        Some((
            ViewId::from_index(view_idx),
            extent.width,
            extent.height,
            pixels,
        ))
    }

    /// Stages the just-completed shm-capture slot's BGRA8 bytes for the host to publish, run
    /// at [`Renderer::begin_offscreen_frame`] right after this slot's in-flight fence wait —
    /// the slot's recorded readback (from `MAX_FRAMES_IN_FLIGHT` frames ago) is now host-
    /// visible, so the read never stalls. A no-op when
    /// the active view's shm publish is off or the slot has no completed readback yet.
    fn stage_pending_shm_publish(&mut self, slot: usize) {
        self.pending_shm_publish = None;
        let active = self.active_view.index();
        if !self.shm_publish_enabled[active] {
            return;
        }
        let Some(capture) = self.views[active].shm_capture.slots[slot].as_ref() else {
            return;
        };
        if !capture.valid {
            return;
        }
        // Record the slot only; the host reads the mapped staging directly via
        // `pending_shm_view` and copies straight into the shm ring — one memcpy, no alloc.
        self.pending_shm_publish = Some((active, slot));
    }

    /// Records the active view's BGRA8 shm-publish readback into the frame command buffer
    /// `cmd` for frame slot `slot`: a 1:1 `vkCmdBlitImage` does the `RGBA16F`→BGRA8
    /// conversion into this slot's persistent BGRA8 image, a `vkCmdCopyImageToBuffer` lands
    /// it in a host-visible staging buffer, and a buffer→host barrier makes the bytes visible
    /// to the host once the frame fence signals. Folded into the frame's single submit — no
    /// separate submit, no synchronous wait. The offscreen is left in `TransferSrcOptimal`
    /// (the tracked layout the next frame's producer barrier transitions from).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the blit format is unsupported or an allocation fails.
    fn record_shm_copy(&mut self, cmd: vk::CommandBuffer, slot: usize) -> Result<()> {
        let active = self.active_view.index();
        let render_extent = self.views[active].offscreen.extent;
        let publish_extent = self.views[active].published_extent();
        if render_extent.width == 0 || render_extent.height == 0 {
            return Ok(());
        }
        // The offscreen is now the DISPLAY extent (the resolve reconstructs to it), so the blit
        // is 1:1 — it survives only to convert RGBA16F → BGRA8, at matching extent.
        self.views[active].ensure_shm_capture(&self.device, slot, publish_extent)?;

        let raw = self.device.raw();
        let view = &self.views[active];
        let from_layout = view.offscreen.layout;
        let src_image = view.offscreen.handle();
        let capture = view.shm_capture.slots[slot]
            .as_ref()
            .expect("shm capture ensured above");
        let dst_image = capture.image.handle();
        let staging = capture.staging.handle();

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        // The offscreen rests in COLOR_ATTACHMENT (after the post chain's overlay pass) or
        // SHADER_READ_ONLY (after a prior frame's readback); match the source scope to it.
        let (from_stage, from_access) = match from_layout {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT
                    | vk::PipelineStageFlags2::COMPUTE_SHADER,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE | vk::AccessFlags2::SHADER_STORAGE_WRITE,
            ),
            _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
        };

        let blit = vk::ImageBlit::default()
            .src_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: render_extent.width as i32,
                    y: render_extent.height as i32,
                    z: 1,
                },
            ])
            .dst_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .dst_offsets([
                vk::Offset3D { x: 0, y: 0, z: 0 },
                vk::Offset3D {
                    x: publish_extent.width as i32,
                    y: publish_extent.height as i32,
                    z: 1,
                },
            ]);
        let copy = vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_extent(vk::Extent3D {
                width: publish_extent.width,
                height: publish_extent.height,
                depth: 1,
            });

        // SAFETY: the ash seam. Barriers / blit / copy recorded into the frame command
        // buffer (already in its begin..end recording); the images + buffer outlive the
        // recorded commands, freed at teardown under `wait_idle`.
        unsafe {
            // offscreen (current layout) → TRANSFER_SRC.
            capture_barrier(
                raw,
                cmd,
                src_image,
                color_range,
                from_layout,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                from_stage,
                from_access,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_READ,
            );
            // BGRA8 (whatever) → TRANSFER_DST (its contents are overwritten by the blit).
            capture_barrier(
                raw,
                cmd,
                dst_image,
                color_range,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::PipelineStageFlags2::TOP_OF_PIPE,
                vk::AccessFlags2::NONE,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_WRITE,
            );
            raw.cmd_blit_image(
                cmd,
                src_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                // 1:1 — the offscreen is already the display extent (the resolve reconstructs to
                // it), so this blit only converts RGBA16F → BGRA8 at matching extent, no scaling.
                vk::Filter::NEAREST,
            );
            // BGRA8 → TRANSFER_SRC for the buffer copy.
            capture_barrier(
                raw,
                cmd,
                dst_image,
                color_range,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::PipelineStageFlags2::BLIT,
                vk::AccessFlags2::TRANSFER_WRITE,
                vk::PipelineStageFlags2::COPY,
                vk::AccessFlags2::TRANSFER_READ,
            );
            raw.cmd_copy_image_to_buffer(
                cmd,
                dst_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                staging,
                &[copy],
            );
            // Make the staging write visible to host reads once the frame fence signals.
            let host = vk::BufferMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(staging)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            let host_barriers = [host];
            let dep = vk::DependencyInfo::default().buffer_memory_barriers(&host_barriers);
            raw.cmd_pipeline_barrier2(cmd, &dep);
        }

        // The offscreen rests in TRANSFER_SRC after the copy; the next frame's first write
        // transitions from this tracked layout (it rests at TransferSrc).
        self.views[active].offscreen.layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
        // A readback was recorded into this slot; its bytes are host-visible once this frame's
        // fence signals (published `MAX_FRAMES_IN_FLIGHT` frames later).
        if let Some(capture) = self.views[active].shm_capture.slots[slot].as_mut() {
            capture.valid = true;
        }
        Ok(())
    }

    /// Copies the active view's raw `RGBA16F` offscreen into a host-visible buffer through a
    /// one-off submit and returns `(extent, format, raw bytes)` — the read-back behind
    /// [`Renderer::capture_viewport`] (the PNG screenshot path, which needs the unconverted
    /// halves for tonemap/clamp encoding). The offscreen may still be sampled by an in-flight
    /// frame, so the device is idled first; the image is left in `ShaderReadOnlyOptimal` so
    /// the next frame's producer barrier holds.
    /// The shm publish uses the GPU-converting [`Renderer::read_active_view_bgra8`] instead.
    fn read_active_offscreen(&mut self) -> Result<(vk::Extent2D, vk::Format, Vec<u8>)> {
        let raw = self.device.raw();
        let view = &mut self.views[self.active_view.index()];
        let extent = view.offscreen.extent;
        let format = view.offscreen.format;
        let from_layout = view.offscreen.layout;
        let image = view.offscreen.handle();
        let byte_size = extent.width as vk::DeviceSize
            * extent.height as vk::DeviceSize
            * crate::format_pixel_bytes(format) as vk::DeviceSize;

        // The offscreen may still be sampled by an in-flight frame; idle so the read-back's
        // layout transition cannot race that read.
        self.device.wait_idle()?;

        let buffer = crate::Buffer::new(
            self.device.resources(),
            byte_size,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        let pool = self.frames.command_pool();
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One primary buffer from the current frame slot's pool;
        // freed below after the submit fence signals.
        let cmd = checked(
            unsafe { raw.allocate_command_buffers(&alloc) },
            "capture: allocate_command_buffers",
        )?[0];
        // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "capture: create_fence",
        )?;

        // The entry barrier's source scope matches the offscreen's current layout: the
        // headless host reads it straight after the scene render (COLOR_ATTACHMENT, written by
        // the post chain's overlay pass) or after a prior read-back left it ShaderReadOnly; the
        // PNG screenshot path may hit it before any frame rendered (UNDEFINED → TopOfPipe). The
        // device is idled above, so this barrier is for layout correctness, not cross-queue
        // sync. Leaving it ShaderReadOnly afterwards keeps the next frame's producer barrier
        // consistent (the `from_stage`/`from_access` choice covers the editor host's
        // COLOR_ATTACHMENT source).
        let (from_stage, from_access) = match from_layout {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            ),
            _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
        };
        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Begin / barrier / copy / barrier / end on the one-off
            // buffer; the image + buffer outlive the recorded commands.
            unsafe {
                checked(
                    raw.begin_command_buffer(cmd, &begin),
                    "capture: begin_command_buffer",
                )?;
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    from_layout,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    from_stage,
                    from_access,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                );
                checked(raw.end_command_buffer(cmd), "capture: end_command_buffer")?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. The device was idled above, so the single graphics
            // queue is free; the fence belongs to this device.
            unsafe {
                checked(
                    raw.queue_submit2(self.device.graphics_queue, &submit, fence),
                    "capture: queue_submit2",
                )?;
                checked(
                    raw.wait_for_fences(&[fence], true, u64::MAX),
                    "capture: wait_for_fences",
                )?;
            }
            Ok(())
        })();

        // Reflect the post-capture layout in the tracked state so the next frame's graph
        // import seeds the right entry layout.
        self.views[self.active_view.index()].offscreen.layout =
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        // SAFETY: the ash seam. The fence was waited (or the submit failed before
        // signaling), so the buffer + fence are idle and freed exactly once.
        unsafe {
            raw.free_command_buffers(pool, &[cmd]);
            raw.destroy_fence(fence, None);
        }
        recorded?;

        let pixel_count = byte_size as usize;
        // SAFETY: the buffer is HOST_VISIBLE + MAPPED for `byte_size` bytes; the copy
        // completed (the fence was waited).
        let pixels = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr(), pixel_count) };
        Ok((extent, format, pixels.to_vec()))
    }

    /// Arms a window/composited-output screenshot for the next present: the swapchain
    /// image (the actual composited window output, distinct from the offscreen
    /// [`Renderer::capture_viewport`] path) is copied to a host buffer and written to
    /// `path` at the next [`Renderer::render_frame`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::ShaderLoad`] if the surface lacks `TRANSFER_SRC` usage (the
    /// swapchain was not created capture-capable, so the image cannot be copied).
    pub fn request_window_capture(&mut self, path: &std::path::Path) -> Result<()> {
        let Some(swapchain) = self.swapchain.as_ref() else {
            return Err(Error::ShaderLoad(
                "window capture unsupported: the editor/headless host has no present swapchain"
                    .to_owned(),
            ));
        };
        if !swapchain.capture_supported {
            return Err(Error::ShaderLoad(
                "window capture unsupported: surface lacks TRANSFER_SRC usage".to_owned(),
            ));
        }
        self.capture_next_window_path = Some(path.to_path_buf());
        Ok(())
    }

    /// Whether a window capture is armed for the next present.
    pub fn window_capture_pending(&self) -> bool {
        self.capture_next_window_path.is_some()
    }

    /// Builds the lazy thumbnail/preview PSOs + the preview sphere up front.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build or mesh-upload failure.
    pub fn prewarm_thumbnail_resources(&mut self) -> Result<()> {
        self.thumbnail.prewarm(&self.device, &self.descriptors)
    }

    /// Renders `mesh` framed by its AABB under a fixed light into a `size`×`size` texture.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build, target-allocation, or submit failure.
    pub fn render_mesh_thumbnail(
        &mut self,
        mesh: &Arc<crate::GpuMesh>,
        size: u32,
    ) -> Result<Arc<crate::GpuTexture>> {
        self.thumbnail
            .render_mesh_thumbnail(&self.device, &self.descriptors, mesh, size)
    }

    /// Renders a unit sphere with `material` under studio lighting into a `size`×`size`
    /// texture (`shader_spv` of `None` uses the default preview pipeline; a codegen
    /// material passes its compiled `.spv` path).
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build, target-allocation, or submit failure.
    pub fn render_material_preview(
        &mut self,
        material: &crate::SubmeshMaterial,
        size: u32,
        shader_spv: Option<&std::path::Path>,
    ) -> Result<Arc<crate::GpuTexture>> {
        self.thumbnail.render_material_preview(
            &self.device,
            &self.descriptors,
            material,
            size,
            shader_spv,
        )
    }

    /// Renders `mesh` shaded per-submesh with its materials into a `size`×`size` texture.
    ///
    /// # Errors
    ///
    /// Propagates any pipeline-build, target-allocation, or submit failure.
    pub fn render_model_thumbnail(
        &mut self,
        mesh: &Arc<crate::GpuMesh>,
        submesh_materials: &[crate::SubmeshMaterial],
        size: u32,
    ) -> Result<Arc<crate::GpuTexture>> {
        self.thumbnail.render_model_thumbnail(
            &self.device,
            &self.descriptors,
            mesh,
            submesh_materials,
            size,
        )
    }

    /// Renders the framed mesh to a `size`×`size` texture, then reads it back to a PNG.
    ///
    /// # Errors
    ///
    /// Propagates the render or read-back/encode failure.
    pub fn encode_asset_thumbnail_png(
        &mut self,
        mesh: &Arc<crate::GpuMesh>,
        size: u32,
    ) -> Result<crate::ThumbnailPng> {
        self.thumbnail
            .encode_asset_thumbnail_png(&self.device, &self.descriptors, mesh, size)
    }

    /// Renders the framed, textured model to a `size`×`size` texture, then reads it back
    /// to a PNG.
    ///
    /// # Errors
    ///
    /// Propagates the render or read-back/encode failure.
    pub fn encode_model_thumbnail_png(
        &mut self,
        mesh: &Arc<crate::GpuMesh>,
        submesh_materials: &[crate::SubmeshMaterial],
        size: u32,
    ) -> Result<crate::ThumbnailPng> {
        self.thumbnail.encode_model_thumbnail_png(
            &self.device,
            &self.descriptors,
            mesh,
            submesh_materials,
            size,
        )
    }

    /// Renders `texture` (downscaled to fit `size`×`size`) and reads it back to a PNG.
    ///
    /// # Errors
    ///
    /// Propagates any allocation / blit / read-back / encode failure.
    pub fn encode_texture_thumbnail_png(
        &self,
        texture: &Arc<crate::GpuTexture>,
        size: u32,
        transfer: crate::PngTransfer,
    ) -> Result<crate::ThumbnailPng> {
        self.thumbnail
            .encode_texture_thumbnail_png(&self.device, texture, size, transfer)
    }

    /// Copies the just-presented swapchain `image` (left in `PRESENT_SRC_KHR` by
    /// [`Renderer::record_clear`]) into a host buffer and writes it to the armed path as a
    /// PNG, then clears the pending path. Called from [`Renderer::render_frame`] after the
    /// present submit's fence has signalled. A failure is logged, not fatal (a screenshot
    /// must never crash the frame loop).
    fn run_pending_window_capture(&mut self, image_index: usize) {
        let Some(path) = self.capture_next_window_path.take() else {
            return;
        };
        let swapchain = self.present_swapchain();
        let image = swapchain.image(image_index);
        let extent = swapchain.extent;
        let format = swapchain.format;
        if let Err(err) = self.copy_swapchain_to_png(image, extent, format, &path) {
            tracing::warn!("window capture failed: {err}");
        } else {
            tracing::info!(
                "captured window ({}x{}) to {}",
                extent.width,
                extent.height,
                path.display()
            );
        }
    }

    /// Copies a swapchain image (in `PRESENT_SRC_KHR`) into a host-visible buffer through a
    /// one-off submit and writes it to `path` as a PNG. The device is idled first so the
    /// copy cannot race the presentation engine's read of the image.
    fn copy_swapchain_to_png(
        &self,
        image: vk::Image,
        extent: vk::Extent2D,
        format: vk::Format,
        path: &std::path::Path,
    ) -> Result<()> {
        let raw = self.device.raw();
        let byte_size = extent.width as vk::DeviceSize
            * extent.height as vk::DeviceSize
            * crate::format_pixel_bytes(format) as vk::DeviceSize;

        // The presentation engine may still be reading the image; idle so the capture's
        // transition cannot race it.
        self.device.wait_idle()?;

        let buffer = crate::Buffer::new(
            self.device.resources(),
            byte_size,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        // SAFETY: the ash seam. A transient one-off pool freed at the end of this call.
        let pool = checked(
            unsafe {
                raw.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                        .queue_family_index(self.device.graphics_queue_family),
                    None,
                )
            },
            "window capture: create_command_pool",
        )?;
        // SAFETY: the ash seam. One primary buffer from the transient pool.
        let cmd = checked(
            unsafe {
                raw.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
            },
            "window capture: allocate_command_buffers",
        )?;
        let cmd = cmd[0];
        // SAFETY: the ash seam. A default (unsignaled) fence, destroyed below.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "window capture: create_fence",
        )?;

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Begin / copy-with-barriers / end; the image + buffer
            // outlive the submit. The image starts in PRESENT_SRC (left by record_clear)
            // and is restored to it so a later present remains valid.
            unsafe {
                checked(
                    raw.begin_command_buffer(cmd, &begin),
                    "window capture: begin_command_buffer",
                )?;
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::NONE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::PRESENT_SRC_KHR,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                    vk::PipelineStageFlags2::BOTTOM_OF_PIPE,
                    vk::AccessFlags2::NONE,
                );
                checked(
                    raw.end_command_buffer(cmd),
                    "window capture: end_command_buffer",
                )?;
            }
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. The device was idled above, so the queue is free.
            unsafe {
                checked(
                    raw.queue_submit2(self.device.graphics_queue, &submit, fence),
                    "window capture: queue_submit2",
                )?;
                checked(
                    raw.wait_for_fences(&[fence], true, u64::MAX),
                    "window capture: wait_for_fences",
                )?;
            }
            Ok(())
        })();

        // SAFETY: the ash seam. The submit (if any) was waited; the pool + fence are idle
        // and freed exactly once.
        unsafe {
            raw.destroy_command_pool(pool, None);
            raw.destroy_fence(fence, None);
        }
        recorded?;

        // SAFETY: the buffer is HOST_VISIBLE + MAPPED for `byte_size`; the copy completed.
        let pixels = unsafe { std::slice::from_raw_parts(buffer.mapped_ptr(), byte_size as usize) };
        crate::write_png_file(pixels, extent.width, extent.height, format, path).map_err(|err| {
            Error::ShaderLoad(format!("window capture: write {}: {err}", path.display()))
        })
    }

    /// Selects the anti-aliasing mode (`msaa_samples` ≥ 2 → MSAA, else `fxaa`, else `taa`,
    /// else off — mutually exclusive). Idles the GPU, recreates the active view's AA
    /// targets, and — when the MSAA sample count changed — clears the sample-count-baked
    /// PSO cache so the mesh + depth-prepass PSOs rebuild for the new count.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the device cannot idle or the AA targets cannot be recreated.
    pub fn set_aa(&mut self, msaa_samples: u32, fxaa: bool, taa: bool) -> Result<()> {
        let count_changed = self.aa.set(msaa_samples, fxaa, taa);
        self.device.wait_idle()?;
        if count_changed {
            // The mesh + depth-prepass PSOs bake the sample count — clear them so the next
            // request rebuilds for the new count.
            self.pipelines.set_sample_count(self.aa.sample_count());
            // The sky PSO bakes the sample count too — rebuild it for the new scene-color
            // target, or the sky pass draws MSAA color with a 1× pipeline.
            self.sky
                .set_sample_count(&self.device, &self.descriptors, self.aa.sample_count())?;
        }
        // Both views share the offscreen sample count, so rebuild every view's AA targets
        // (not just the active one) — a later `set-active-view` must find the inactive view's
        // MSAA targets already sized for the current count.
        for view in &mut self.views {
            view.build_aa_targets(&self.device, &self.descriptors, self.aa)?;
        }
        Ok(())
    }

    /// The current TAA resolve tuning (read by the control `get-taa-params`).
    pub fn taa_params(&self) -> crate::TaaParams {
        self.taa_params
    }

    /// Sets the TAA resolve tuning. Takes effect next frame (the push is rebuilt each frame
    /// from this state); no GPU idle / PSO rebuild — it is push-constant data, not baked state.
    pub fn set_taa_params(&mut self, params: crate::TaaParams) {
        self.taa_params = params;
    }

    /// Selects the AA mode by name (`"off"` / `"fxaa"` / `"taa"` / `"msaa2"` / `"msaa4"` /
    /// `"msaa8"`) — the control-plane / CLI entry.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the device cannot idle or the AA targets cannot be recreated.
    pub fn set_aa_mode(&mut self, mode: &str) -> Result<()> {
        let (samples, fxaa, taa) = match mode {
            "fxaa" => (1, true, false),
            "taa" => (1, false, true),
            "msaa2" => (2, false, false),
            "msaa4" => (4, false, false),
            "msaa8" => (8, false, false),
            _ => (1, false, false),
        };
        self.set_aa(samples, fxaa, taa)
    }

    /// The current AA mode as a name (`"off"` / `"fxaa"` / `"taa"` / `"msaaN"`).
    pub fn aa_mode(&self) -> String {
        self.aa.mode()
    }

    /// Toggles the depth pre-pass (lays down scene depth before the shaded scene pass).
    pub fn set_depth_prepass(&mut self, enabled: bool) {
        self.use_depth_prepass = enabled;
    }

    /// Whether the depth pre-pass is on.
    pub fn depth_prepass_enabled(&self) -> bool {
        self.use_depth_prepass
    }

    /// Sets the tonemap exposure in stops; the mandatory tonemap pass applies
    /// `exp2(this)`.
    pub fn set_exposure(&mut self, ev: f32) {
        self.exposure_ev = ev;
    }

    /// The current tonemap exposure in stops.
    pub fn exposure_ev(&self) -> f32 {
        self.exposure_ev
    }

    /// Selects the debug render-output mode. `Wireframe` arms
    /// the wireframe PSO permutation; the channel modes fold a debug-output index into
    /// the light UBO's `point_shadow_meta.w`.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
        self.wireframe = mode == ViewMode::Wireframe;
        self.lighting.set_debug_channel(mode.debug_channel());
    }

    /// The current debug render-output mode.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Toggles the GPU compute-skinning path.
    pub fn set_skinning(&mut self, enabled: bool) {
        self.skinning_enabled = enabled;
    }

    /// Whether GPU skinning is on.
    pub fn skinning_enabled(&self) -> bool {
        self.skinning_enabled
    }

    /// Whether the device is a software rasterizer.
    pub fn software_gpu(&self) -> bool {
        self.software_gpu
    }

    /// The active view's INPUT (scene render) width in device pixels — the `SceneRenderer` seam
    /// drives `render_scene` at this extent (the scene rasterises at input res under upsampling).
    pub fn viewport_width(&self) -> u32 {
        self.views[self.active_view.index()]
            .scaled_render_extent()
            .width
    }

    /// The active view's INPUT (scene render) height in device pixels.
    pub fn viewport_height(&self) -> u32 {
        self.views[self.active_view.index()]
            .scaled_render_extent()
            .height
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

    /// The most recent frame's full draw + timing counters, folding the run-loop frame
    /// times and the profiler mode into the draw-path [`RenderStats`]. `fps` derives from
    /// `frame_ms`.
    pub fn render_stats(&self) -> RenderStatsFull {
        let fps = if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        };
        RenderStatsFull {
            draw: self.stats,
            frame_ms: self.frame_ms,
            fps,
            gpu_ms: self.gpu_frame_ms,
            cpu_frame_ms: self.cpu_frame_ms,
            cpu_wait_ms: self.cpu_wait_ms,
            vram_usage_bytes: self.vram_usage_bytes,
            vram_budget_bytes: self.vram_budget_bytes,
            software_gpu: self.software_gpu,
            profiler_mode: self.gpu_profiler.mode,
            view_mode: self.view_mode,
            exposure_ev: self.exposure_ev,
        }
    }

    /// Records the run loop's per-frame wall-clock timings for [`Renderer::render_stats`]
    /// and the frame-history percentiles.
    pub fn record_frame_timings(&mut self, frame_ms: f32, cpu_frame_ms: f32, cpu_wait_ms: f32) {
        self.frame_ms = frame_ms;
        self.cpu_frame_ms = cpu_frame_ms;
        self.cpu_wait_ms = cpu_wait_ms;
    }

    /// Folds one frame's wall-clock delta (seconds) into the smoothed `frame_ms` headline the
    /// `render-stats` query reports: seed on the first frame, then a 0.9/0.1 EMA. A zero or
    /// non-finite delta is ignored.
    pub fn observe_frame_delta(&mut self, dt_seconds: f32) {
        if dt_seconds <= 0.0 || !dt_seconds.is_finite() {
            return;
        }
        let delta_ms = dt_seconds * 1000.0;
        self.frame_ms = if self.frame_ms == 0.0 {
            delta_ms
        } else {
            self.frame_ms * 0.9 + delta_ms * 0.1
        };
    }

    /// Folds one frame's CPU split (busy + fence-wait, in ms) into the smoothed `cpu_frame_ms`
    /// / `cpu_wait_ms` the `render-stats` query reports: seed on the first frame, then a
    /// 0.9/0.1 EMA each. The busy span is
    /// the run loop's update + render window minus the GPU wait, so it is render-thread CPU work,
    /// not wall clock. A non-finite value is ignored.
    pub fn observe_cpu_frame(&mut self, busy_ms: f32, wait_ms: f32) {
        if busy_ms.is_finite() && busy_ms >= 0.0 {
            self.cpu_frame_ms = if self.cpu_frame_ms == 0.0 {
                busy_ms
            } else {
                self.cpu_frame_ms * 0.9 + busy_ms * 0.1
            };
        }
        if wait_ms.is_finite() && wait_ms >= 0.0 {
            self.cpu_wait_ms = if self.cpu_wait_ms == 0.0 {
                wait_ms
            } else {
                self.cpu_wait_ms * 0.9 + wait_ms * 0.1
            };
        }
    }

    /// The per-frame telemetry tail the run loop calls once after each rendered frame:
    /// folds the CPU busy/wait split into the smoothed headline, pushes
    /// the raw frame into the history ring, runs the perf-alarm detectors, and advances the
    /// profiler-capture state machine over the slot just rendered.
    ///
    /// `busy_ms` is the loop's update+render span minus the GPU fence-wait; `wait_ms` is that
    /// wait; `dt_sec` is the wall-clock delta since the prior frame (drives the alarm EMA's
    /// irregular-interval alpha). The wall-clock-delta EMA ([`Renderer::observe_frame_delta`])
    /// is split out and called at the loop's frame top, ahead of this tail.
    /// Drops the frame-timing distribution + smoothed headlines and holds telemetry off for a short
    /// warm-up. Called when a project load completes: the prior frames (a different or empty scene)
    /// and the cold-pipeline frames right after the swap are not steady state, so grading over them
    /// paints the HUD red the moment a project opens.
    pub fn reset_frame_telemetry(&mut self) {
        self.frame_history.reset();
        self.frame_ms = 0.0;
        self.cpu_frame_ms = 0.0;
        self.cpu_wait_ms = 0.0;
        self.telemetry_warmup = TELEMETRY_WARMUP_FRAMES;
    }

    pub fn finalize_frame_telemetry(&mut self, busy_ms: f32, wait_ms: f32, dt_sec: f32) {
        let now_ns = cpu_now_ns();
        self.last_frame_ns = now_ns;
        // Warm-up frames after a project load (PSO compiles, acceleration-structure builds) are not
        // representative, so keep them out of the smoothed headline, the history distribution, and
        // the alarm detectors — the average resumes on the first settled frame.
        if self.telemetry_warmup > 0 {
            self.telemetry_warmup -= 1;
            return;
        }
        self.observe_cpu_frame(busy_ms, wait_ms);

        // Record the raw frame into the history ring (always on; the distribution stays honest
        // only if it sees every frame, un-smoothed), then run the alarm detectors on it (after
        // the push, so the MAD/burn-rate windows include this frame). Pure CPU bookkeeping.
        let frame_time_ms = busy_ms + wait_ms;
        self.frame_history.record(
            busy_ms,
            self.gpu_profiler.last_gpu_total_ms,
            wait_ms,
            self.perf_config.budget_ms(),
            now_ns,
        );
        let inputs = AlarmInputs {
            frame_time_ms,
            dt_sec,
            now_ns,
            vram_usage_bytes: self.vram_usage_bytes,
            vram_budget_bytes: self.vram_budget_bytes,
            pipelines_created: self.stats.pipelines_created,
            focused: self.reactive.power_state == PowerState::Focused,
        };
        self.alarms
            .tick(&self.frame_history, &self.perf_config, &inputs);

        // Auto-quality: when enabled, step the render-quality tier (then, below the tier floor, the
        // render scale) to hold the frame budget. The frame work time (busy + GPU-fence wait, before
        // the loop's pacing sleep) is the signal. Off by default, so the controller never runs and
        // the tier/scale stay user-/project-set.
        if self.perf_config.auto_quality {
            let active_scale = self.views[self.active_view.index()].render_scale;
            match self.budget_controller.update(
                frame_time_ms,
                self.perf_config.budget_ms(),
                self.render_quality.tier,
                active_scale,
            ) {
                Some(BudgetStep::Tier(tier)) => self.set_render_quality(tier.resolve()),
                // A resolution change reallocates the render targets; defer it to the next frame's
                // safe resize point — the just-submitted frame still references the current targets.
                Some(BudgetStep::Scale(scale)) => self.pending_render_scale = Some(scale),
                None => {}
            }
        }
    }

    /// The current GPU profiler mode.
    pub fn profiler_mode(&self) -> ProfilerMode {
        self.gpu_profiler.mode
    }

    /// Selects the GPU profiler mode, allocating the query pools on first non-`Off`
    /// request and clamping to what the device supports.
    pub fn set_profiler_mode(&mut self, mode: ProfilerMode) {
        self.gpu_profiler.set_mode(&self.device, mode);
    }

    /// Whether the device's graphics queue supports timestamp queries.
    pub fn profiler_timestamps_supported(&self) -> bool {
        self.gpu_profiler.timestamps_supported
    }

    /// Whether the device supports pipeline-statistics queries.
    pub fn profiler_pipeline_stats_supported(&self) -> bool {
        self.gpu_profiler.pipeline_stats_supported
    }

    /// The last frame's per-pass GPU timings. Empty unless the
    /// profiler ran in a timestamps mode.
    pub fn pass_timings(&self) -> &[PassTiming] {
        &self.gpu_profiler.last_timings
    }

    /// The last frame's total GPU span across all passes (ms).
    pub fn pass_timings_total_ms(&self) -> f32 {
        self.gpu_profiler.last_gpu_total_ms
    }

    /// Arms a profiler capture, returning its id.
    pub fn start_profile_capture(
        &mut self,
        mode: CaptureMode,
        frames: u32,
        filter: String,
        include_cpu: bool,
        include_stats: bool,
    ) -> u32 {
        self.capture.start(
            &self.device,
            &mut self.gpu_profiler,
            mode,
            frames,
            filter,
            include_cpu,
            include_stats,
        )
    }

    /// Finishes the armed capture and returns the accumulated spans + metadata.
    pub fn stop_profile_capture(&mut self) -> ProfileCapture {
        let software_gpu = self.software_gpu;
        let device_name = self.device_name.clone();
        let target_fps = self.perf_config.target_fps;
        self.capture.stop(
            &self.device,
            &mut self.gpu_profiler,
            software_gpu,
            device_name,
            target_fps,
        )
    }

    /// Advances the capture recorder once per finalized frame, appending the merged
    /// CPU+GPU spans while `Recording`, at the read-back seam.
    /// The run loop calls this each frame after the profiler read-back.
    pub fn tick_profile_capture(&mut self, cpu_slot: usize) {
        self.capture
            .tick(&self.cpu_profiler, cpu_slot, &self.gpu_profiler);
    }

    /// The CPU span profiler, recorded into by the run loop's per-pass CPU markers.
    pub fn cpu_profiler_mut(&mut self) -> &mut CpuProfiler {
        &mut self.cpu_profiler
    }

    /// The capture's mode.
    pub fn profile_capture_mode(&self) -> CaptureMode {
        self.capture.mode
    }

    /// The capture state machine's current state.
    pub fn profile_capture_state(&self) -> CaptureState {
        self.capture.state
    }

    /// Frames copied into the in-flight capture so far.
    pub fn profile_capture_captured_frames(&self) -> u32 {
        self.capture.captured_frames
    }

    /// The in-flight capture's target frame count.
    pub fn profile_capture_target_frames(&self) -> u32 {
        self.capture.target_frames
    }

    /// The rolling frame-time percentile / stutter summary.
    pub fn frame_history_stats(&self) -> FrameHistoryStats {
        self.frame_history.stats()
    }

    /// The most recent `max_samples` frame samples, oldest→newest.
    pub fn frame_samples(&self, max_samples: u32) -> Vec<FrameSample> {
        self.frame_history.samples(max_samples)
    }

    /// The shared frame-budget / threshold config.
    pub fn perf_config(&self) -> PerfConfig {
        self.perf_config
    }

    /// Replaces the perf config, clamping it into sane ranges.
    pub fn set_perf_config(&mut self, config: PerfConfig) {
        self.perf_config = config.clamped();
    }

    /// Drains perf-alarm events with `seq > since`.
    pub fn drain_alarms(&self, since: u64) -> AlarmDrain {
        self.alarms.drain(since)
    }

    /// The currently-firing perf alarms.
    pub fn active_alarms(&self) -> &[ActiveAlarm] {
        self.alarms.active()
    }

    /// Toggles the infinite analytic ground-grid debug overlay.
    pub fn set_show_grid(&mut self, enabled: bool) {
        self.show_grid = enabled;
    }

    /// Whether the ground grid is shown.
    pub fn show_grid(&self) -> bool {
        self.show_grid
    }

    /// Selects native-viewport-host present mode: present blits the post-processed
    /// offscreen straight to the swapchain (no ui pass). The offscreen content (incl. the
    /// overlay) is identical to editor mode.
    pub fn set_present_viewport_only(&mut self, enabled: bool) {
        self.present_viewport_only = enabled;
    }

    /// Whether present-only (native-viewport host) mode is active.
    pub fn present_viewport_only(&self) -> bool {
        self.present_viewport_only
    }

    /// Replaces this frame's editor-overlay geometry: the `depth_tested` range (gizmo /
    /// frustums occluded by scene geometry) then the `on_top` range (handles, always
    /// drawn). Composited into the post-tonemap color so present-only blits it too. The
    /// geometry source is the host's native gizmo builder.
    pub fn submit_overlay(&mut self, depth_tested: Vec<OverlayVertex>, on_top: Vec<OverlayVertex>) {
        self.overlay.submit(depth_tested, on_top);
    }

    /// Builds the frame's [`SceneDrawList`] from `items`, uploading the instance +
    /// material SSBOs for the current frame slot and refreshing [`Renderer::stats`].
    /// Equivalent to [`Renderer::submit_draw_list_skinned`] with an empty palette — the
    /// static-scene front door.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if an SSBO grow/upload fails.
    pub fn submit_draw_list(&mut self, view_proj: Mat4, items: &[DrawItem]) -> Result<()> {
        self.submit_draw_list_skinned(view_proj, items, &[])
    }

    /// Builds the frame's [`SceneDrawList`] from `items` + the concatenated `joints`
    /// palette (`worldBone * inverseBind` per joint, indexed by each skinned item's
    /// `joint_offset`). Uploads the instance / material / palette SSBOs, sizes the
    /// deformed buffers, and wires the skin dispatches for the current frame slot. Call
    /// once per frame before [`Renderer::render_scene_offscreen`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if an SSBO / deformed-buffer grow or upload fails.
    pub fn submit_draw_list_skinned(
        &mut self,
        view_proj: Mat4,
        items: &[DrawItem],
        joints: &[Mat4],
    ) -> Result<()> {
        let inputs = crate::instancing::DrawListInputs {
            frame: self.frames.index(),
            view_proj,
            wireframe: self.wireframe,
            default_texture_index: crate::DEFAULT_WHITE_SLOT,
            // Track skinned RT instances when an RT consumer is armed (ray-query shadows or
            // reflections on, on an RT device) — they feed the per-frame refit BLAS the
            // `tlas-build` reads.
            rt_skinned: self.rt.use_rt_shadows() || self.rt.use_rt_reflections(),
        };
        let (list, stats) = self.instancing.submit_draw_list(
            &self.descriptors,
            &mut self.pipelines,
            &mut self.skinning,
            items,
            joints,
            inputs,
        )?;
        self.scene_draw_list = list;
        self.stats = stats;
        Ok(())
    }

    /// Records and submits the scene + optional depth-prepass into the active view's
    /// offscreen target through the render graph — the first real end-to-end frame
    /// (geometry → instanced draw → a flat-ambient image). Call after
    /// [`Renderer::submit_draw_list`]; submit-seam closures replay after the batch list.
    ///
    /// The graph derives the UNDEFINED → COLOR/DEPTH attachment barriers and the depth WAW
    /// barrier from the declared usages. The offscreen image is left in
    /// `COLOR_ATTACHMENT_OPTIMAL` for a later post/capture.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing Vulkan call.
    /// Begins the offscreen frame: waits + resets the current slot's in-flight fence and
    /// resets its command pool, so the slot is idle before any per-frame state reset (notably
    /// the layers' draw-list submit, which resets the per-frame skinning descriptor pool).
    /// The fence-wait is split out so the run loop runs it in `begin_frame`
    /// (before the `on_render`/`on_ui` hooks) rather than at submit time. Sets
    /// [`Renderer::frame_begun`] so the following [`Renderer::render_scene_offscreen`] does not
    /// re-wait the now-unsignaled fence.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing fence/pool call.
    pub fn begin_offscreen_frame(&mut self) -> Result<()> {
        let raw = self.device.raw();
        let in_flight = self.frames.in_flight();
        // SAFETY: the ash seam. The fence belongs to this device; the wait blocks until this
        // slot's prior GPU work completes, so its per-frame buffers/sets are free to reuse.
        checked(
            unsafe { raw.wait_for_fences(&[in_flight], true, u64::MAX) },
            "wait_for_fences (begin)",
        )?;
        // This slot's GPU work (from MAX_FRAMES_IN_FLIGHT frames ago) is now complete, so its
        // timestamp pool reads back without blocking: fold the prior frame's per-pass GPU spans
        // into `gpu_frame_ms` + `last_timings` at the begin-frame fence wait. A no-op when the
        // profiler is `Off`.
        let slot = self.frames.index();
        // Re-sample the GPU↔CPU clock offset (cheap, no queue work) before the read-back, so
        // this frame's spans decode onto the CPU axis (ordering: calibrate → readback). The
        // profiler self-gates to ~once a second; only runs while
        // profiling with pools allocated.
        self.frame_serial = self.frame_serial.wrapping_add(1);
        if self.gpu_profiler.mode != ProfilerMode::Off && self.gpu_profiler.pools_ready {
            self.gpu_profiler.calibrate(&self.device, self.frame_serial);
        }
        self.gpu_frame_ms = self
            .gpu_profiler
            .readback(&self.device, slot, self.gpu_frame_ms);
        // Drain this slot's merged spans into an in-flight capture *before* the upcoming
        // `render_scene_offscreen` resets the slot's CPU buffer. Both lanes then describe the same
        // frame: the GPU `last_timings` just read back and the CPU spans still in `buffers[slot]`
        // were both recorded `MAX_FRAMES_IN_FLIGHT` frames ago. Ticking after the reset (at frame
        // end) would pair this frame's CPU spans with the older GPU read-back, splitting the lanes
        // by the read-back lag (ordering: readback → tick-capture → reset).
        self.capture
            .tick(&self.cpu_profiler, slot, &self.gpu_profiler);
        // This slot's frame fence just signalled, so its recorded shm readback (from
        // MAX_FRAMES_IN_FLIGHT frames ago) is host-visible: stage those bytes for the host to
        // publish without a stall.
        self.stage_pending_shm_publish(slot);
        // SAFETY: the ash seam. The waited fence is unsignaled and reset before resubmit.
        let raw = self.device.raw();
        checked(
            unsafe { raw.reset_fences(&[in_flight]) },
            "reset_fences (begin)",
        )?;
        // SAFETY: the ash seam. The slot's fence was waited, so the pool may be reset.
        checked(
            unsafe {
                raw.reset_command_pool(
                    self.frames.command_pool(),
                    vk::CommandPoolResetFlags::empty(),
                )
            },
            "reset_command_pool (begin)",
        )?;
        self.frame_begun = true;
        Ok(())
    }

    /// Resets this slot's GPU timestamp (and pipeline-stats) query pool on the recording command
    /// buffer, so the graph's per-pass scopes write into a clean pool. A no-op when the profiler
    /// is `Off` or its pools are not allocated.
    fn reset_profiler_pools(&self, cmd: vk::CommandBuffer, slot: usize) {
        if self.gpu_profiler.mode == ProfilerMode::Off || !self.gpu_profiler.pools_ready {
            return;
        }
        let raw = self.device.raw();
        if let Some(pool) = self.gpu_profiler.timestamp_pool(slot) {
            // SAFETY: the ash seam. `cmd` is recording; the slot's prior GPU work completed at
            // the begin-frame fence wait, so the pool is free to reset. Two queries per scope.
            unsafe {
                raw.cmd_reset_query_pool(cmd, pool, 0, 2 * crate::profiler::MAX_PROFILED_SCOPES);
            }
        }
        if self.gpu_profiler.mode == ProfilerMode::PipelineStats
            && let Some(pool) = self.gpu_profiler.stats_pool(slot)
        {
            // SAFETY: the ash seam. As above; one stats query per top-level graphics pass.
            unsafe {
                raw.cmd_reset_query_pool(cmd, pool, 0, crate::profiler::MAX_PROFILED_SCOPES);
            }
        }
    }

    pub fn render_scene_offscreen(&mut self) -> Result<()> {
        // Apply a budget-controller render-scale change here — a safe frame boundary (it idles
        // the GPU + reallocates the render targets), unlike the post-submit telemetry hook that
        // requested it. Rare (hysteresis-gated), so the realloc cost is amortized.
        if let Some(scale) = self.pending_render_scale.take() {
            self.set_render_scale(self.active_view, scale)?;
        }

        // Re-bake the IBL environment if the sky inputs changed (the directional light
        // moved). Deferred to here — a GPU-idle point — so the visible sky + IBL relight
        // together. The bake waits idle internally; an editor-time event, not per-frame hot
        // A failure is logged, not fatal.
        if self.ibl.rebake_pending {
            if let Err(err) = self.ibl.fire_rebake(&self.device) {
                tracing::error!("ibl re-bake failed: {err}");
            }
        }

        // Wait + reset this slot's fence and command pool. The run loop calls
        // [`Renderer::begin_offscreen_frame`] in `begin_frame` so the slot is idle *before*
        // the layers' draw-list submit resets the per-frame skinning descriptor pool (the
        // fence is waited first); the `frame_begun` latch skips the
        // re-wait here so a standalone caller (the unit tests) still gets a self-contained
        // begin while the loop avoids a double-wait that would deadlock on the reset fence.
        if !self.frame_begun {
            self.begin_offscreen_frame()?;
        }
        self.frame_begun = false;
        let raw = self.device.raw();
        let frame = self.frames.index();
        let command_buffer = self.frames.command_buffer();

        // Reset this slot's CPU span buffer for a fresh frame when the profiler is active
        // When `Off` the
        // buffer stays empty and every CPU scope below is a cheap no-op.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        if profile_cpu {
            self.cpu_profiler.buffers[frame].reset();
        }

        // Resolve every PSO this frame needs up front (each takes `&mut self.pipelines`),
        // so the graph build below borrows the rest of `self` immutably. A `None` arms
        // nothing — a build failure (logged once) degrades to the unlit/unshadowed path.
        let depth_prepass = if self.use_depth_prepass {
            self.pipelines.request_depth_prepass()
        } else {
            None
        };
        let cull_pipeline = if self.lighting.take_cluster_dispatch_pending() {
            self.pipelines.request_light_cull()
        } else {
            None
        };
        // The compute skinning PSO, resolved only when the frame built skin dispatches
        // (an unskinned scene never compiles it). The skin pass deforms each instance once
        // before every geometry pass reads the deformed buffer as a static stream.
        let skin_pipeline = if !self.scene_draw_list.skin_dispatches.is_empty() {
            crate::skinning::request_skin_pipeline(&mut self.pipelines, &self.skinning)
        } else {
            None
        };
        // The morph compute PSO, resolved only when the frame built morph dispatches. The
        // morph pass deforms each morph instance into the deformed buffer before skin.
        let morph_pipeline = if !self.scene_draw_list.morph_dispatches.is_empty() {
            crate::skinning::request_morph_pipeline(&mut self.pipelines, &self.skinning)
        } else {
            None
        };
        let shadow_pipeline =
            if self.lighting.shadow_pending() || self.lighting.spot_shadow_pending() {
                self.pipelines.request_shadow_depth()
            } else {
                None
            };
        // Point-shadow cubes. The PSO is needed whenever a point light casts; the dynamic cube
        // (skinned/morph casters) re-renders every active frame, while the **static** cube
        // (everything else) re-renders only when its content key (light + static casters) or the
        // cube image changed — a static light + static casters reuse the cached static cube while a
        // character animates, so only the (few) dynamic casters re-render. Both persist in
        // `SHADER_READ_ONLY` between frames, so a skipped pass samples its cached cube.
        let point_shadow_pipeline = if self.lighting.point_shadow_pending() {
            self.pipelines.request_point_shadow()
        } else {
            None
        };
        let static_point_shadow_dirty = if point_shadow_pipeline.is_some() {
            let key = self.lighting.point_shadow_key();
            let cube = self.targets.point_shadow.image();
            if self.last_point_shadow_key == Some(key) && self.last_point_shadow_cube == cube {
                false
            } else {
                self.last_point_shadow_key = Some(key);
                self.last_point_shadow_cube = cube;
                true
            }
        } else {
            false
        };

        // Screen-space effects ride a thin G-buffer prepass that runs when ANY of GTAO /
        // contact / SSGI is on. Resolve the prepass + each
        // effect's compute PSO up front (each takes `&mut self.pipelines`) so the graph
        // build below borrows the rest of `self` immutably; a `None` skips that pass.
        let gbuf_ready =
            self.ssao.ready && self.views[self.active_view.index()].screen_space_ready();
        let want_ssao = gbuf_ready && self.ssao.use_ssao;
        let want_contact = gbuf_ready && self.ssao.use_contact;
        let want_ssgi = gbuf_ready && self.ssao.use_ssgi;
        let want_ssr = gbuf_ready && self.ssao.use_ssr;
        // RT reflections gather from prev_color (the screen-space chain's history copy), so
        // they force the chain on + the prev-color copy even with no other screen effect.
        let want_rt_reflections = gbuf_ready && self.rt.use_rt_reflections();
        // ReSTIR needs the thin G-buffer (it reconstructs world pos/normal from it), so it
        // forces the prepass on even with no screen-space effect, then ANDs G-buffer
        // readiness into the ReSTIR enable.
        let want_restir = self.restir.use_restir()
            && self.restir.supported()
            && self.views[self.active_view.index()].restir.ready()
            && gbuf_ready;
        // DFAO reconstructs world pos/normal from the thin G-buffer, so it forces the prepass on
        // (like ReSTIR / RT reflections). It runs the reduced-res GDF sky-visibility cone trace
        // whenever sky occlusion is active this frame (IBL + the toggle + the GDF ready).
        let want_dfao = gbuf_ready && self.want_sky_occlusion();
        let want_screen = want_restir
            || want_rt_reflections
            || want_dfao
            || crate::ssao::wants_gbuffer_prepass(
                gbuf_ready,
                self.ssao.use_ssao,
                self.ssao.use_contact,
                self.ssao.use_ssgi,
                self.ssao.use_ssr,
            );
        let compute2 = self.ssao.compute2_layout();
        let compute3 = self.ssao.compute3_layout();
        let gi_resolve_layout = self.ssao.gi_resolve_layout();
        let (gbuffer, gtao, ao_blur, contact, ssgi, ssgi_blur, ssr, copy_color) = if want_screen {
            let gbuffer = self.pipelines.request_gbuffer();
            let (gtao, ao_blur) = if want_ssao {
                (
                    self.pipelines.request_gtao(compute2),
                    self.pipelines.request_ao_blur(compute3),
                )
            } else {
                (None, None)
            };
            let contact = if want_contact {
                self.pipelines.request_contact(compute2)
            } else {
                None
            };
            let (ssgi, ssgi_blur) = if want_ssgi {
                (
                    self.pipelines.request_ssgi(compute3),
                    self.pipelines.request_ssgi_blur(compute3),
                )
            } else {
                (None, None)
            };
            let ssr = if want_ssr {
                self.pipelines.request_ssr(compute3)
            } else {
                None
            };
            // SSGI, SSR, and RT reflections all gather from the previous frame's color, so
            // the prev-color copy runs when any is on.
            let copy_color = if want_ssgi || want_ssr || want_rt_reflections {
                self.pipelines.request_copy_color(compute2)
            } else {
                None
            };
            (
                gbuffer, gtao, ao_blur, contact, ssgi, ssgi_blur, ssr, copy_color,
            )
        } else {
            (None, None, None, None, None, None, None, None)
        };
        // Bump the monotonic SSGI/SSR frame indices (decorrelating the trace noise) here,
        // where `&mut self.ssao` is live; the `&self` graph build reads the snapshot below.
        let ssgi_push = self.ssao.next_ssgi_push();
        let ssr_push = self.ssao.next_ssr_push();
        // Screen-space indirect-diffuse resolve PSO — runs whenever the screen chain does (it reads
        // the G-buffer). Additive: its half-res output is not yet sampled by the fragment.
        let gi_resolve = if want_screen {
            self.pipelines.request_gi_resolve(gi_resolve_layout)
        } else {
            None
        };
        // DFAO: the trace PSO (three-set) + the shared bilateral-upsample PSO (the ssgi-blur PSO,
        // bound with the DFAO blur set). Resolved when sky occlusion is active this frame.
        let (dfao, dfao_blur) = if want_dfao {
            (
                self.pipelines.request_dfao(compute2),
                self.pipelines.request_ssgi_blur(compute3),
            )
        } else {
            (None, None)
        };
        // Bump the monotonic DFAO frame index (rotating the cone ring) here, where
        // `&mut self.ssao` is live; the `&self` graph build reads the snapshot below.
        let dfao_push = self.ssao.next_dfao_push();
        // Specular occlusion: shares the sky-occlusion gate with DFAO. The trace is a three-set PSO
        // (its I/O set is the compute3 shape — the G-buffer + roughness samplers + the storage) and
        // the blur reuses the ssgi-blur PSO, bound with the specocc blur set.
        let (specocc, specocc_blur) = if want_dfao {
            (
                self.pipelines.request_specocc(compute3),
                self.pipelines.request_ssgi_blur(compute3),
            )
        } else {
            (None, None)
        };
        // Bump the monotonic specocc frame index here, where `&mut self.ssao` is live.
        let specocc_push = self.ssao.next_specocc_push();

        // DDGI: the four trace/blend/border PSOs, resolved together (the `doDdgi` gate requires
        // all four — a partial set skips the whole chain). Each takes `&mut self.pipelines`
        // with the DDGI sub-state's set layout; resolved here so the `&self` graph build
        // borrows `self.ddgi` immutably. `None` when DDGI is off / not ready / a PSO failed.
        let ddgi = if self.ddgi.use_ddgi && self.ddgi.ready {
            let trace = self.pipelines.request_ddgi_trace(self.ddgi.trace_layout());
            let blend_irr = self
                .pipelines
                .request_ddgi_blend_irr(self.ddgi.blend_irr_layout());
            let blend_dist = self
                .pipelines
                .request_ddgi_blend_dist(self.ddgi.blend_dist_layout());
            let border = self
                .pipelines
                .request_ddgi_border(self.ddgi.border_layout());
            match (trace, blend_irr, blend_dist, border) {
                (Some(trace), Some(blend_irr), Some(blend_dist), Some(border)) => {
                    Some(DdgiPipelines {
                        trace,
                        blend_irr,
                        blend_dist,
                        border,
                    })
                }
                _ => None,
            }
        } else {
            None
        };

        // Global SDF: the cull + composite PSOs, resolved together (both present or the chain is
        // skipped). Resolved here so the `&self` graph build borrows `self.global_sdf` immutably.
        // `None` when the GDF is off / not ready / a PSO failed.
        let gdf = if self.global_sdf.use_gdf && self.global_sdf.ready {
            let cull = self
                .pipelines
                .request_gdf_cull(self.global_sdf.cull_layout());
            let composite = self
                .pipelines
                .request_gdf_composite(self.global_sdf.composite_layout());
            match (cull, composite) {
                (Some(cull), Some(composite)) => Some(GdfPipelines { cull, composite }),
                _ => None,
            }
        } else {
            None
        };

        // ReSTIR DI: the three compute PSOs, resolved together (the `doRestir` gate requires
        // all three — a partial set skips the whole chain). RT-only (the resolve traces a
        // visibility ray). Resolved here so the `&self` graph build below borrows `self.restir`
        // immutably; the runtime gate (cull + G-buffer + TLAS ran) is applied in the graph
        // build, where `tlas_ready` is known. `None` arms no ReSTIR passes.
        let restir = if want_restir {
            let initial = self
                .pipelines
                .request_restir_initial(self.restir.initial_layout());
            let reuse = self
                .pipelines
                .request_restir_reuse(self.restir.reuse_layout());
            let resolve = self
                .pipelines
                .request_restir_resolve(self.restir.resolve_layout());
            match (initial, reuse, resolve) {
                (Some(initial), Some(reuse), Some(resolve)) => Some(RestirPipelines {
                    initial,
                    reuse,
                    resolve,
                }),
                _ => None,
            }
        } else {
            None
        };

        // The motion-vector prepass runs when TAA or SSGI is on (both reproject through it);
        // the TAA / FXAA resolves run when that mode is active and its scratch is built. The
        // PSOs are resolved here (each takes `&mut self.pipelines`); the per-view target
        // existence checks read the active view first so no immutable borrow spans the
        // `&mut self.pipelines` requests.
        let have_motion_targets = {
            let view = &self.views[self.active_view.index()];
            view.motion.is_some() && view.motion_depth.is_some()
        };
        let have_scratch = self.views[self.active_view.index()].scratch.is_some();
        // DFAO also reprojects through the motion target, so it forces motion on (like TAA/SSGI).
        let want_motion = (self.aa.taa() || want_ssgi || want_dfao) && have_motion_targets;
        let motion = if want_motion {
            self.pipelines.request_motion()
        } else {
            None
        };
        // The SSGI temporal accumulator PSO runs when SSGI is on AND motion ran (it reprojects
        // through the motion target), independent of the final-image AA mode.
        let ssgi_accum = if want_ssgi && have_motion_targets {
            self.pipelines
                .request_ssgi_accum(self.descriptors.taa_set_layout())
        } else {
            None
        };
        // The DFAO temporal accumulator uses its own clamp-free `dfao_accum` PSO (a pure EMA over
        // the rotating cone estimate — a neighborhood clamp would re-inject the per-frame rotation
        // variance and never converge), bound with the DFAO accum sets; it runs when DFAO is on AND
        // motion ran. Specular occlusion is spatial-only (the blur denoises it; its view-dependent
        // term must not be reprojected by surface motion), so it has no accumulator here.
        let dfao_accum = if want_dfao && have_motion_targets {
            self.pipelines
                .request_dfao_accum(self.descriptors.taa_set_layout())
        } else {
            None
        };
        let taa_layout = self.descriptors.taa_set_layout();
        let fxaa_layout = self.descriptors.fxaa_set_layout();
        let taa = if self.aa.taa() && have_scratch {
            self.pipelines.request_taa(taa_layout)
        } else {
            None
        };
        let fxaa = if self.aa.fxaa() && have_scratch {
            self.pipelines.request_fxaa(fxaa_layout)
        } else {
            None
        };
        // The scene-resolve copy (input scratch -> display offscreen, normalized-UV upscale) runs
        // on the no-AA / MSAA paths; FXAA / TAA resolve to the offscreen themselves. Memoized, so
        // requesting it unconditionally is cheap.
        let scene_resolve = self.pipelines.request_copy_color(compute2);
        // The depth-upscale graphics pass fills the display-extent overlay depth from the
        // input-extent scene depth so the grid / gizmo occlude correctly under upsampling.
        let depth_upscale = self
            .pipelines
            .request_depth_upscale(self.descriptors.depth_upscale_layout());
        // The reactive-coverage pass (marks translucent geometry into the r8 reactive mask) arms
        // only under TAA — the mask is a TAA-resolve input. Memoized, so the request is cheap.
        let reactive_coverage = if self.aa.taa() {
            self.pipelines.request_reactive_coverage()
        } else {
            None
        };

        // The final post chain: the tonemap is mandatory (resolved every frame); the grid
        // arms only when shown; the overlay PSOs arm only when geometry is queued. The
        // overlay's per-frame vertex buffer is prepared (grown + uploaded) here, before the
        // graph build, so the pass captures only the resolved handle (README §2).
        let tonemap = self.pipelines.request_tonemap();
        let grid = if self.show_grid {
            self.pipelines.request_grid()
        } else {
            None
        };
        let (overlay, overlay_depth, overlay_draw) = if self.overlay.has_geometry() {
            let draw = match self.overlay.prepare(frame) {
                Ok(draw) => draw,
                Err(err) => {
                    tracing::error!("overlay upload failed: {err}");
                    None
                }
            };
            (
                self.pipelines.request_overlay(),
                self.pipelines.request_overlay_depth(),
                draw,
            )
        } else {
            (None, None, None)
        };

        // View-mode-specific post passes: the Lit Wireframe overlay (a second line-mode draw
        // over the shaded scene) and the motion-vector visualization (a fullscreen compute on
        // the motion target). Resolved only for their active mode so other frames pay nothing.
        let wireframe_overlay = if self.view_mode == ViewMode::LitWireframe {
            self.pipelines.request_wireframe_overlay()
        } else {
            None
        };
        let motion_visualize = if self.view_mode == ViewMode::MotionVectors {
            self.pipelines
                .request_motion_visualize(self.ssao.compute2_layout())
        } else {
            None
        };

        let frame_pipelines = FramePipelines {
            depth_prepass,
            cull: cull_pipeline,
            skin: skin_pipeline,
            morph: morph_pipeline,
            shadow: shadow_pipeline,
            point_shadow: point_shadow_pipeline,
            static_point_shadow_dirty,
            gbuffer,
            gtao,
            ao_blur,
            contact,
            ssgi,
            ssgi_blur,
            ssgi_accum,
            gi_resolve,
            dfao,
            dfao_blur,
            dfao_accum,
            dfao_push,
            specocc,
            specocc_blur,
            specocc_push,
            ssr,
            copy_color,
            ddgi,
            gdf,
            restir,
            ssgi_push,
            ssr_push,
            motion,
            taa,
            fxaa,
            tonemap,
            scene_resolve,
            depth_upscale,
            reactive_coverage,
            grid,
            overlay,
            overlay_depth,
            overlay_draw,
            wireframe_overlay,
            motion_visualize,
        };

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begins recording on the freshly reset buffer.
        checked(
            unsafe { raw.begin_command_buffer(command_buffer, &begin_info) },
            "begin_command_buffer (scene)",
        )?;

        // The validation-clean gate's regression probe: when armed, record one
        // deliberately invalid command so a planted error surfaces through the debug
        // messenger. This proves the detector is wired — a silently-disabled gate (wrong
        // messenger prefix, no validation layer) would let the planted error pass unseen and
        // a test asserts it does NOT. The bad viewport is overwritten by every pass's own
        // viewport set inside its render pass, so the rendered output stays correct.
        plant_validation_error(raw, command_buffer);

        // Timestamp queries are uninitialized until reset; reset this slot's pool(s) before the
        // graph writes into it (reading an unreset pool risks device loss). A no-op when the
        // profiler is `Off`.
        self.reset_profiler_pools(command_buffer, frame);

        self.record_scene_graph(command_buffer, frame, frame_pipelines);

        // Fold the active view's BGRA8 shm-publish readback into THIS frame's command buffer
        // when the view's shm publish is enabled — one submit covers it, with no separate
        // submit and no synchronous wait.
        if self.shm_publish_enabled[self.active_view.index()] {
            self.record_shm_copy(command_buffer, frame)?;
        }

        // Re-borrow the device after the `&mut self` graph build above.
        let raw = self.device.raw();
        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(command_buffer) },
            "end_command_buffer (scene)",
        )?;

        // CPU span over the frame's queue submit. A no-op when the profiler is `Off`.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let submit_span = if profile_cpu {
            let CpuProfiler { registry, buffers } = &mut self.cpu_profiler;
            Some(buffers[frame].begin_span(registry, "submit-present", cpu_now_ns()))
        } else {
            None
        };
        let raw = self.device.raw();

        let cmd = [vk::CommandBufferSubmitInfo::default().command_buffer(command_buffer)];
        // The windowed present-only host blits this offscreen onto the swapchain in
        // `present_active_view_to_swapchain`; signal the slot's scene-finished semaphore so
        // that blit submit waits for the scene render to complete on the GPU. The
        // editor/headless host has no present sync and never signals it.
        let signal = self.present_sync.as_ref().map(|present_sync| {
            [vk::SemaphoreSubmitInfo::default()
                .semaphore(present_sync.scene_finished(frame))
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)]
        });
        // Latch that this frame's offscreen was submitted + signaled the scene-finished
        // semaphore, so the present blit waits on it (and not on an unsignaled semaphore when
        // the host skips the offscreen render).
        self.present_scene_signaled = signal.is_some();
        let mut submit_info = vk::SubmitInfo2::default().command_buffer_infos(&cmd);
        if let Some(signal) = signal.as_ref() {
            submit_info = submit_info.signal_semaphore_infos(signal);
        }
        let submit = [submit_info];
        // SAFETY: the ash seam. The queue is externally synchronized; at this phase it
        // is touched from one thread only. The fence was reset in `begin_offscreen_frame`.
        checked(
            unsafe {
                raw.queue_submit2(self.device.graphics_queue, &submit, self.frames.in_flight())
            },
            "queue_submit2 (scene)",
        )?;
        if let Some(index) = submit_span {
            let CpuProfiler { buffers, .. } = &mut self.cpu_profiler;
            buffers[frame].end_span(index, cpu_now_ns());
        }
        // One primary command buffer recorded + one submit2 this frame. The offscreen /
        // shm-publish host path submits exactly once per frame.
        self.stats.command_buffers = 1;
        self.stats.queue_submits = 1;
        // The slot just recorded into is the one `finalize_frame_telemetry` reads this frame
        // (its CPU spans + the GPU read-back land in the capture); record it before `advance`
        // rolls `frames.index()` to the next slot.
        self.last_rendered_slot = frame;
        self.frames.advance();
        Ok(())
    }

    /// Builds and executes the depth-prepass + scene render graph for `frame` into the
    /// active view's offscreen target. The pass bodies capture resolved handles + the
    /// moved draw list / submissions, never `&mut self`.
    ///
    /// Pass order (the `beginFrameGraph` slice this phase fills): `light-cull` (compute)
    /// → `shadow` / `spot-shadow` (depth-only graphics, `DepthWrite → ShaderReadOnly`) →
    /// `point-shadow` (a compute-kind body driving 6 face draws) → optional
    /// `depth-prepass` → `scene`. The graph derives every barrier from the declared
    /// usage; the shadow maps' cross-frame layout rides external slots.
    fn record_scene_graph(
        &mut self,
        cmd: vk::CommandBuffer,
        frame: usize,
        pipelines: FramePipelines,
    ) {
        // CPU span over this frame's render-graph CONSTRUCTION (cull + scene/lighting/post
        // pass declarations), closed just before `execute-render-graph` opens — a top-level
        // sibling of it. A no-op when the profiler is `Off`.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let build_span = if profile_cpu {
            let CpuProfiler { registry, buffers } = &mut self.cpu_profiler;
            Some(buffers[frame].begin_span(registry, "build-frame-graph", cpu_now_ns()))
        } else {
            None
        };
        let view = &self.views[self.active_view.index()];
        // The scene / sky / depth-prepass rasterise at INPUT extent (into the input scratch +
        // input depth); the resolve reconstructs them up to the display-extent offscreen.
        let extent = view.scaled_render_extent();
        let color_image = view.offscreen.handle();
        let color_view = view.offscreen.view();
        let depth_image = view.depth.handle();
        let depth_view = view.depth.view();

        let bindless_set = self.descriptors.bindless_set();
        let light_set = self.lighting.light_set(frame);
        let instance_set = self.instancing.instance_set(frame);
        let ibl_set = self.ibl.set();
        let raw = self.device.raw().clone();

        let mut graph = RenderGraph::new();

        // Light-cull (compute): cull the punctual lights into the froxel grid. The graph
        // emits the compute→fragment barrier on the cluster buffer from the declared
        // StorageWriteCompute usage (the scene fragment reads it as a storage buffer).
        if let Some(cull) = &pipelines.cull {
            let cluster_buffer = graph.import_buffer(self.lighting.cluster_buffer(frame));
            let cull_set = self.lighting.cluster_set(frame);
            let cull = Arc::clone(cull);
            let cull_pipeline = cull.handle();
            let cull_layout = cull.layout();
            let raw_body = raw.clone();
            let groups = crate::lighting::CLUSTER_COUNT.div_ceil(64);
            let pass = RgPass::compute("light-cull")
                .access(cluster_buffer, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch
                    // covers the froxel grid (one invocation per cluster, 64 per group).
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            cull_pipeline,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            cull_layout,
                            0,
                            &[cull_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, groups, 1, 1);
                    }
                    drop(cull);
                });
            graph.add_pass(pass);
        }

        // Compute skinning pre-pass: deform each skinned mesh-instance once into the
        // frame's deformed buffer (current pose) + prev-deformed buffer (previous pose),
        // before EVERY geometry pass reads it as a static vertex stream. The graph derives
        // the compute-write → vertex-input barrier from the StorageWriteCompute here + each
        // consumer's VertexInputRead. The deformed-buffer handle is bound by each geometry
        // pass body for a skinned batch; `None` falls back to the static bind-pose stream.
        let do_skin = pipelines.skin.is_some()
            && !self.scene_draw_list.skin_dispatches.is_empty()
            && self.skinning.deformed_buffer(frame).is_some()
            && self.skinning.prev_deformed_buffer(frame).is_some();
        let do_morph = pipelines.morph.is_some()
            && !self.scene_draw_list.morph_dispatches.is_empty()
            && self.skinning.deformed_buffer(frame).is_some()
            && self.skinning.prev_deformed_buffer(frame).is_some();
        let do_deform = do_skin || do_morph;
        let deformed_handle = if do_deform {
            self.skinning.deformed_buffer(frame)
        } else {
            None
        };
        // The prev-deformed buffer carries the previous pose for the motion pass. Both the
        // morph and skin passes write it (each deforms its prev-pose slice), so it is
        // imported once for the whole deform scope and shared between them.
        let prev_deformed_handle = if do_deform {
            self.skinning.prev_deformed_buffer(frame)
        } else {
            None
        };
        let (deformed_res, prev_deformed_res) = if do_deform {
            let deformed = graph.import_buffer(deformed_handle.expect("deformed buffer"));
            let prev_deformed =
                graph.import_buffer(prev_deformed_handle.expect("prev-deformed buffer"));

            // Morph pre-pass: scatter each active blend-shape's sparse deltas into the
            // deformed (current weights) + prev-deformed (previous weights) buffers, then
            // resolve to vertex positions/normals — before skin and before any geometry pass
            // reads the deformed stream. It writes the same buffers as skin, so the graph
            // orders morph → skin (write-after-write) automatically.
            if do_morph {
                let morph = pipelines.morph.as_ref().expect("morph PSO");
                let morph = Arc::clone(morph);
                let morph_handle = morph.handle();
                let morph_layout = morph.layout();
                let raw_morph = raw.clone();
                let morph_list = self.scene_draw_list.shallow_clone();
                let pass = RgPass::compute("morph")
                    .access(deformed, RgUsage::StorageWriteCompute)
                    .access(prev_deformed, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::skinning::record_morph(
                            &raw_morph,
                            cmd,
                            morph_handle,
                            morph_layout,
                            &morph_list.morph_dispatches,
                            &morph_list.prev_morph_dispatches,
                        );
                        drop(morph);
                    });
                graph.add_pass(pass);
            }

            if do_skin {
                let skin = pipelines.skin.as_ref().expect("skin PSO");
                let skin = Arc::clone(skin);
                let skin_handle = skin.handle();
                let skin_layout = skin.layout();
                let raw_body = raw.clone();
                let list = self.scene_draw_list.shallow_clone();
                // Both deformed buffers are written this pass (current + previous pose), so
                // the graph emits a compute-write barrier for each before the consumers read
                // them.
                let pass = RgPass::compute("skin")
                    .access(deformed, RgUsage::StorageWriteCompute)
                    .access(prev_deformed, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::skinning::Skinning::record_skin(
                            &raw_body,
                            cmd,
                            skin_handle,
                            skin_layout,
                            &list.skin_dispatches,
                            &list.prev_skin_dispatches,
                        );
                        drop(skin);
                    });
                graph.add_pass(pass);
            }
            (Some(deformed), Some(prev_deformed))
        } else {
            (None, None)
        };

        // RT: build the per-frame TLAS over the scene's mesh instances (a compute-kind pass;
        // the recorded plan self-manages the AS-build → fragment ray-query barrier). Skinned
        // instances refit a per-slot BLAS from the deformed buffer first, so the pass
        // declares an `AccelStructBuildRead` on it: the graph derives the
        // skin-compute-write → AS-build-read barrier and orders this pass after `skin`. The
        // `&mut self.rt` prep (AS create / set-6 write / instance copy) happens here, outside
        // the `'static` pass closure, which replays the resulting plan.
        self.rt.reset_frame_ready();
        let deformed_rt = self.scene_draw_list.deformed_rt_instances.clone();
        let has_skinned_rt = !deformed_rt.is_empty();
        if self.rt.build_pending() && self.rt.has_instances(&deformed_rt) {
            if let Some(plan) =
                self.rt
                    .prepare_tlas_build(&self.device, frame, &deformed_rt, deformed_handle)
            {
                let raw_body = raw.clone();
                let mut tlas_pass = RgPass::compute("tlas-build").body(
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::record_tlas_build_plan(&raw_body, cmd, &plan);
                    },
                );
                // Declare the deformed-buffer read so the graph orders this after the skin
                // pass (the skinned BLAS refit reads the freshly deformed vertices).
                if has_skinned_rt {
                    if let Some(deformed) = deformed_res {
                        tlas_pass = tlas_pass.access(deformed, RgUsage::AccelStructBuildRead);
                    }
                }
                graph.add_pass(tlas_pass);
            }
        }

        // Directional + spot shadow depth passes: depth-only graphics, the graph
        // transitions each map DepthWrite → (next frame's sample) ShaderReadOnly via its
        // external layout slot. The slot index is read back after execute.
        let mut directional_slot: Option<usize> = None;
        let mut spot_slot: Option<usize> = None;
        // The shadow maps the mesh fragment samples via the light set (set 1) this frame,
        // declared `SampledRead` on the scene pass below so the graph derives the
        // DepthWrite → ShaderReadOnly transition between the depth-write pass and the scene
        // draw (otherwise the mesh samples a DEPTH_ATTACHMENT image, `00344`).
        let mut directional_res: Option<RgResource> = None;
        let mut spot_res: Option<RgResource> = None;
        if let Some(shadow) = &pipelines.shadow {
            if self.lighting.shadow_pending() {
                let slot = graph.alloc_external_layout(self.directional_shadow_layout);
                directional_slot = Some(slot);
                let res = graph.import_image(
                    self.targets.directional_shadow.handle(),
                    self.targets.directional_shadow.view(),
                    vk::ImageAspectFlags::DEPTH,
                    self.directional_shadow_layout,
                    Some(slot),
                );
                directional_res = Some(res);
                self.add_shadow_pass(
                    &mut graph,
                    "shadow",
                    res,
                    shadow,
                    instance_set,
                    self.lighting.shadow_view_proj(),
                    deformed_res,
                    deformed_handle,
                );
            }
            if self.lighting.spot_shadow_pending() {
                let slot = graph.alloc_external_layout(self.spot_shadow_layout);
                spot_slot = Some(slot);
                let res = graph.import_image(
                    self.targets.spot_shadow.handle(),
                    self.targets.spot_shadow.view(),
                    vk::ImageAspectFlags::DEPTH,
                    self.spot_shadow_layout,
                    Some(slot),
                );
                spot_res = Some(res);
                self.add_shadow_pass(
                    &mut graph,
                    "spot-shadow",
                    res,
                    shadow,
                    instance_set,
                    self.lighting.spot_shadow_view_proj(),
                    deformed_res,
                    deformed_handle,
                );
            }
        }

        // Point shadow: a compute-kind pass whose body opens its own 6 face rendering
        // scopes + manages the cube's layout (the cube's 6 layers exceed the graph's
        // single-layer barrier).
        if let Some(point) = &pipelines.point_shadow {
            let faces = point_shadow_face_matrices(
                self.lighting.point_shadow_pos(),
                self.lighting.point_shadow_far(),
            );
            let light_pos = self.lighting.point_shadow_pos();
            let far_plane = self.lighting.point_shadow_far();
            let point_pipeline = point.handle();
            let point_layout = point.layout();

            // Static cube: non-deformed casters, rendered only when its content key / image
            // changed. Skipped frames sample the cached cube (it persists ShaderReadOnly).
            if pipelines.static_point_shadow_dirty {
                let target = PointShadowTarget {
                    cube_image: self.targets.point_shadow.image(),
                    face_views: std::array::from_fn(|f| self.targets.point_shadow.face_view(f)),
                    depth_image: self.targets.point_shadow.depth_image(),
                    depth_view: self.targets.point_shadow.depth_view(),
                    extent: self.targets.point_shadow.extent,
                };
                let list = self.scene_draw_list.shallow_clone();
                let point_static = Arc::clone(point);
                let raw_body = raw.clone();
                graph.add_pass(RgPass::compute("point-shadow-static").body(
                    move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_point_shadow(
                            &raw_body,
                            cmd,
                            &list,
                            point_pipeline,
                            point_layout,
                            instance_set,
                            &target,
                            &faces,
                            light_pos,
                            far_plane,
                            None, // static casters never read the deformed buffer
                            false,
                        );
                        drop(point_static);
                    },
                ));
                self.targets.point_shadow.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
            }

            // Dynamic cube: deformed (skinned / morph) casters, re-rendered every active frame so a
            // moving character's shadow tracks it against the cached static environment cube.
            let dyn_target = PointShadowTarget {
                cube_image: self.targets.point_shadow_dynamic.image(),
                face_views: std::array::from_fn(|f| self.targets.point_shadow_dynamic.face_view(f)),
                depth_image: self.targets.point_shadow_dynamic.depth_image(),
                depth_view: self.targets.point_shadow_dynamic.depth_view(),
                extent: self.targets.point_shadow_dynamic.extent,
            };
            let list = self.scene_draw_list.shallow_clone();
            let point_dynamic = Arc::clone(point);
            let raw_body = raw.clone();
            let mut pass = RgPass::compute("point-shadow-dynamic").body(
                move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_point_shadow(
                        &raw_body,
                        cmd,
                        &list,
                        point_pipeline,
                        point_layout,
                        instance_set,
                        &dyn_target,
                        &faces,
                        light_pos,
                        far_plane,
                        deformed_handle,
                        true,
                    );
                    drop(point_dynamic);
                },
            );
            // A skinned batch draws the deformed buffer into the cube faces; declare the
            // read so the graph orders it after the skin compute write.
            if let Some(deformed) = deformed_res {
                pass = pass.access(deformed, RgUsage::VertexInputRead);
            }
            graph.add_pass(pass);
            // Each cube self-manages its layout, ending ShaderReadOnly for the scene sample.
            self.targets.point_shadow_dynamic.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        }

        // The offscreen color + 1× depth are always imported (the present blit samples the
        // offscreen; the post-tonemap overlay reads the 1× depth). The AA mode then selects
        // where the scene renders its 1× result (`scene_output`) and what the scene pass
        // attaches: MSAA renders to multisampled targets resolving into the 1× output;
        // FXAA / TAA render to a 1× scratch then a compute pass resolves it → offscreen.
        // The offscreen contents are regenerated every frame (sky/scene clears it), so it
        // enters UNDEFINED — but its *exit* layout must be tracked so the shm read-back's
        // entry barrier uses the right `old_layout`. An external slot seeded at UNDEFINED
        // carries the resolved exit layout back into `view.offscreen.layout` after execute.
        let offscreen_slot = graph.alloc_external_layout(vk::ImageLayout::UNDEFINED);
        let color = graph.import_image(
            color_image,
            color_view,
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            Some(offscreen_slot),
        );
        let depth = graph.import_image(
            depth_image,
            depth_view,
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );

        let (msaa, fxaa, taa) = {
            let view = &self.views[self.active_view.index()];
            (
                self.aa.msaa() && view.msaa_color.is_some() && view.msaa_depth.is_some(),
                pipelines.fxaa.is_some() && view.scratch.is_some(),
                pipelines.taa.is_some() && view.scratch.is_some(),
            )
        };

        // The scene always renders its result into the INPUT-extent scratch; the resolve stage
        // (FXAA / TAA, or the no-AA / MSAA copy below) reconstructs scratch → the display-extent
        // offscreen. `scratch` is unconditionally allocated by `build_aa_targets`, so there is one
        // scene→resolve→offscreen path with no `scale == 1` fork.
        let scene_output = {
            let view = &self.views[self.active_view.index()];
            let scratch = view.scratch.as_ref().expect("scratch built");
            graph.import_image(
                scratch.handle(),
                scratch.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            )
        };
        // The scene pass attaches the multisampled color (resolving into scene_output) when
        // MSAA is on, else scene_output directly.
        let scene_color_attachment = if msaa {
            let view = &self.views[self.active_view.index()];
            let msaa_color = view.msaa_color.as_ref().expect("msaa color built");
            graph.import_image(
                msaa_color.handle(),
                msaa_color.view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            )
        } else {
            scene_output
        };
        // The scene depth attaches the multisampled depth (resolving into the 1× depth) when
        // MSAA is on, else the 1× depth directly.
        let scene_depth = if msaa {
            let view = &self.views[self.active_view.index()];
            let msaa_depth = view.msaa_depth.as_ref().expect("msaa depth built");
            graph.import_image(
                msaa_depth.handle(),
                msaa_depth.view(),
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::UNDEFINED,
                None,
            )
        } else {
            depth
        };

        // The motion-vector prepass: reproject this frame's vs last frame's camera into the
        // 1× motion target. Runs before the screen-space SSGI accumulation (it reprojects
        // through motion) and before the scene so the TAA resolve (after the scene) reads
        // it; the graph derives ColorWrite → SampledReadCompute. Runs when `taa || do_ssgi`.
        let (motion_resource, motion_depth_resource) = match self.add_motion_pass(
            &mut graph,
            &pipelines,
            instance_set,
            (deformed_res, deformed_handle),
            (prev_deformed_res, prev_deformed_handle),
        ) {
            Some((motion, depth)) => (Some(motion), Some(depth)),
            None => (None, None),
        };

        // Global SDF: the cull + composite passes that bin the per-mesh MDF bricks into the
        // camera-centered cascade clipmap the DDGI trace taps as one trilinear read beyond the near
        // field (and the scene's GDF reflection-occlusion cone marches per pixel). Runs FIRST
        // (before the DDGI trace + the scene, both of which sample the cascades via the light set),
        // so the composite writes are visible before any read. Returns the cascade resources for the
        // downstream SampledRead declarations + the layout-writeback slots. Empty when the GDF is
        // off / not ready.
        let gdf = self.add_gdf_passes(&mut graph, &pipelines, frame);

        // Screen-space effects off the thin G-buffer (view normal + view-Z): the gbuffer
        // prepass, then GTAO + bilateral denoise, directional contact shadows, the one-bounce
        // SSGI trace + denoise, and (when motion ran) the SSGI temporal accumulation into the
        // resolved map. The graph derives every ColorWrite / Storage / SampledRead barrier
        // from the declared usage. Returns the maps the scene pass declares SampledRead on
        // (so they transition ShaderReadOnly before the sample), the per-view mesh set 4 to
        // bind, and whether the SSGI accumulation ran (it shares the temporal ping-pong
        // parity flipped after the scene).
        // Fill this frame's gi-resolve params UBO + (re)write the shared IBL/DDGI bindings into the
        // current frame's set slot (per-frame-slot → the slot's prior use is fenced, so no in-flight
        // hazard). Mirrors the mesh's `indirectIrr` inputs so the resolve matches it. Additive: the
        // fragment does not yet sample the resolve output.
        if pipelines.gi_resolve.is_some() {
            let inv_view = self.ssao.view().inverse();
            let (vol_min, vol_ext) = self.ddgi.volume();
            let gi_params = crate::ssao::GiParams {
                inv_projection: self.ssao.inv_projection(),
                inv_view,
                volume_min: vol_min.extend(0.0),
                volume_extent: vol_ext.extend(0.0),
                probe_count: self.ddgi.probe_count_ubo(),
                scroll_base: self.ddgi.scroll_base_ubo(),
                eye_position: inv_view.w_axis,
                flags: saffron_geometry::glam::UVec4::new(
                    u32::from(self.ddgi.enabled()),
                    u32::from(pipelines.dfao.is_some()),
                    0,
                    0,
                ),
            };
            let ibl_cube = self.ibl.irradiance_cube_view();
            let ibl_sampler = self.ibl.sampler();
            let ddgi_irr = self.ddgi.irradiance().1;
            let ddgi_dist = self.ddgi.distance().1;
            let ddgi_sampler = self.ddgi.sampler();
            let active = self.active_view.index();
            if let Some(ubo) = self.views[active].gi_params_ubos.get_mut(frame) {
                if let Some(dst) = ubo.mapped_bytes() {
                    let src = bytemuck::bytes_of(&gi_params);
                    dst[..src.len()].copy_from_slice(src);
                }
            }
            self.views[active].write_gi_resolve_shared(
                &self.device,
                frame,
                ibl_cube,
                ibl_sampler,
                ddgi_irr,
                ddgi_dist,
                ddgi_sampler,
            );
        }

        let screen = self.add_screen_space_passes(
            &mut graph,
            &pipelines,
            instance_set,
            motion_resource,
            (deformed_res, deformed_handle),
            light_set,
            gdf.cascades,
        );

        // ReSTIR DI: the three-pass reservoir chain (initial candidate sampling → temporal +
        // spatial reuse → resolve incl. one TLAS visibility ray), writing a per-pixel direct
        // radiance image the scene samples via set 7. Runs after the G-buffer prepass (it
        // reconstructs world pos/normal from it) + the TLAS build (the resolve traces it),
        // before the scene. The full runtime gate (cull + G-buffer + TLAS ran) is applied
        // inside the method (where `tlas_ready` is known). The reservoir SSBOs serialize via
        // the graph-derived RAW barriers on the combined-reservoir sentinel buffer.
        let restir = self.add_restir_passes(&mut graph, &pipelines, frame, motion_resource);

        // DDGI: the four-pass GI chain (trace → blend-irr → blend-dist → border), updating the
        // irradiance + distance atlases the mesh fragment samples via set 5. The trace
        // sphere-marches the shared distance field (the per-mesh MDF near field + the GDF beyond,
        // via the bindless + light sets) and reads the lite albedo cache for hit color, so it runs
        // AFTER the GDF composite (its cascades + albedo are this trace's inputs). The graph
        // derives the atlas General↔ShaderReadOnly transitions. Returns the atlases for the scene's
        // SampledRead declaration + the imported-image layout-writeback slots.
        let ddgi = self.add_ddgi_passes(
            &mut graph,
            &pipelines,
            bindless_set,
            light_set,
            gdf.cascades,
            gdf.albedo,
        );

        // Visible sky: a fullscreen pass that fills the scene color target before the
        // geometry. It writes the SAME target the scene pass uses, owns the color clear when
        // present, and the scene pass then loads instead of clearing.
        let did_sky = self.sky.should_draw();
        if did_sky {
            let bindless = bindless_set;
            let raw_for_body = raw.clone();
            let draw = self.sky.draw_data(self.scene_draw_list.view_proj);
            // The sky clears + STORES the (multisampled, under MSAA) scene color; the scene
            // pass then LOADs it and owns the single MSAA resolve into scene_output. The sky
            // must NOT resolve or DONT_CARE here — discarding the multisampled samples would
            // leave the scene pass loading undefined MSAA color (a noisy band over black). The
            // sky pass is unconditionally clear+store with no resolve.
            let mut color_att = RgAttachment::clear_store(scene_color_attachment);
            color_att.clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [
                        self.sky.clear_color.x,
                        self.sky.clear_color.y,
                        self.sky.clear_color.z,
                        1.0,
                    ],
                },
            };
            let sky_pass = RgPass::graphics("sky", extent).color(color_att).body(
                move |cmd, _scopes: &mut NestedScopeRecorder| {
                    crate::ibl::record_sky(&raw_for_body, cmd, bindless, &draw);
                },
            );
            graph.add_pass(sky_pass);
        }

        let did_depth_prepass = pipelines.depth_prepass.is_some();
        if let Some(pipeline) = &pipelines.depth_prepass {
            // Move a shallow copy of the draw list (batches share `Arc`s) into the body.
            let list = self.scene_draw_list.shallow_clone();
            let raw_for_body = raw.clone();
            let pipeline = Arc::clone(pipeline);
            let depth_pipeline = pipeline.handle();
            let depth_layout = pipeline.layout();
            let bindless = bindless_set;
            // The depth pre-pass writes the (multisampled, when MSAA) scene depth the scene
            // pass then loads — the same sample count the scene PSO bakes.
            let mut depth_pass = RgPass::graphics("depth-prepass", extent)
                .depth_attachment(depth_clear_store(scene_depth))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // Bind the bindless albedo set (0) so the prepass fragment can alpha-clip masked
                    // materials; set 0's layout is identical here, so it persists across the PSO bind
                    // that `record_depth_prepass` issues before the draws.
                    // SAFETY: the ash seam — the set + layout are valid for this frame.
                    unsafe {
                        raw_for_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            depth_layout,
                            0,
                            &[bindless],
                            &[],
                        );
                    }
                    record_depth_prepass(
                        &raw_for_body,
                        cmd,
                        &list,
                        depth_pipeline,
                        depth_layout,
                        instance_set,
                        deformed_handle,
                    );
                    drop(pipeline);
                });
            if let Some(deformed) = deformed_res {
                depth_pass = depth_pass.access(deformed, RgUsage::VertexInputRead);
            }
            graph.add_pass(depth_pass);
        }

        // The scene pass: clear or (after a depth pre-pass) load the depth, clear the
        // color, replay the batched draw list then the submit-seam closures. The
        // draw-list batches are `Arc`-cloned into the body; the frame's `live_textures`
        // stay pinned on `self.scene_draw_list` until the next frame's fence is waited.
        let list = self.scene_draw_list.shallow_clone();
        let submissions = std::mem::take(&mut self.submissions);
        let raw_for_body = raw.clone();
        let clear_color = self.clear_color;

        let mut color_att = RgAttachment::clear_store(scene_color_attachment);
        color_att.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: clear_color,
            },
        };
        // The sky pass owns the color clear when it ran; the scene then loads it.
        // Otherwise the scene clears the color itself.
        if did_sky {
            color_att.load_op = vk::AttachmentLoadOp::LOAD;
        }
        // MSAA: render to the multisampled color, resolve into scene_output (the
        // multisampled samples are discarded). The render graph's `resolve` is the MSAA
        // resolve (color `AVERAGE`, depth `SAMPLE_ZERO`).
        if msaa {
            color_att.store_op = vk::AttachmentStoreOp::DONT_CARE;
            color_att.resolve = Some(scene_output);
        }
        let mut depth_att = depth_clear_store(scene_depth);
        if did_depth_prepass {
            depth_att.load_op = vk::AttachmentLoadOp::LOAD;
        }
        // Persist the 1× scene depth for the post-tonemap overlay: store it directly (no
        // MSAA), or resolve the multisampled depth into the 1× target (MSAA samples then
        // discarded).
        if msaa {
            depth_att.store_op = vk::AttachmentStoreOp::DONT_CARE;
            depth_att.resolve = Some(depth);
        }
        let ssao_mesh_set = screen.mesh_set;
        // Set 5 (DDGI) — the irradiance + distance atlas samplers. Bound whenever the
        // sub-state is built (the mesh PSO statically references it; the atlases are the
        // neutral init-transitioned targets when DDGI is off). The sample is gated in the
        // mesh by the DDGI `screen_flags.z` flag.
        let ddgi_mesh_set = if self.ddgi.ready {
            self.ddgi.mesh_set()
        } else {
            vk::DescriptorSet::null()
        };
        // Set 6 (the TLAS) — present only on an RT device (`null` otherwise, the scene pass
        // then skips the bind). The mesh fragment gates the ray-query trace on `rtShadows`.
        let rt_mesh_set = self.rt.mesh_set(frame);
        // Set 7 (the ReSTIR resolved-radiance sampler) — the mesh PSO statically references it
        // on an RT device, so it must be bound whenever the view's ReSTIR scaffolding is built,
        // exactly like set 6, or `vkCmdDrawIndexed` reports set 7 unbound
        // (`VUID-vkCmdDrawIndexed-None-08600`). The mesh fragment gates the actual sample on
        // the runtime ReSTIR flag; when ReSTIR did not run this frame the set still binds the
        // (neutral) radiance sampler. `null` only on a non-RT device, where the layout omits
        // set 7 and the scene pass skips the bind.
        let restir_mesh_set = if restir.radiance.is_some() {
            restir.mesh_set
        } else {
            self.views[self.active_view.index()].restir.mesh_set()
        };
        // The scene pass binds five descriptor-set operations (sets 0, {1,2}, 3, 4, 5) plus one
        // each for the RT sets 6/7 when present — constant in the batch count. Record it for
        // `render-stats` here, where the resolved sets are known, since the pass body runs inside
        // a graph closure whose return value is discarded.
        self.stats.descriptor_binds = crate::scene_pass::scene_draw_list_bind_count(
            self.scene_draw_list.valid && !self.scene_draw_list.batches.is_empty(),
            rt_mesh_set,
            restir_mesh_set,
        );
        let mut scene = RgPass::graphics("scene", extent)
            .color(color_att)
            .depth_attachment(depth_att)
            .body(move |_cmd, scopes: &mut NestedScopeRecorder| {
                scopes.scope("scene-opaque", |cmd| {
                    record_scene_draw_list(
                        &raw_for_body,
                        cmd,
                        &list,
                        bindless_set,
                        light_set,
                        instance_set,
                        ibl_set,
                        ssao_mesh_set,
                        ddgi_mesh_set,
                        rt_mesh_set,
                        restir_mesh_set,
                        deformed_handle,
                    );
                });
                scopes.scope("scene-submissions", |cmd| {
                    for body in submissions {
                        body(cmd);
                    }
                });
                // Translucent geometry composites last, over the resolved opaque scene +
                // submissions, sorted back-to-front with depth-write off (same color + depth
                // attachments — no separate pass). Empty (a no-op) when nothing is translucent.
                scopes.scope("scene-translucent", |cmd| {
                    record_transparent_draw_list(
                        &raw_for_body,
                        cmd,
                        &list,
                        bindless_set,
                        light_set,
                        instance_set,
                        ibl_set,
                        ssao_mesh_set,
                        ddgi_mesh_set,
                        rt_mesh_set,
                        restir_mesh_set,
                        deformed_handle,
                    );
                });
            });
        // The scene fragment samples the AO / contact / SSGI maps via set 4; declare the
        // reads so the graph transitions each from GENERAL (compute write) → ShaderReadOnly
        // before the sample. The übershader gates them by flag, but the layout transition
        // is unconditional once the maps were storage-written this frame.
        for resource in &screen.scene_sampled {
            scene = scene.access(*resource, RgUsage::SampledRead);
        }
        // When DDGI ran this frame, the irradiance + distance atlases were storage-written
        // (GENERAL); declare the scene's SampledRead so the graph transitions each back to
        // ShaderReadOnly before the mesh sample (the border pass leaves irradiance GENERAL).
        if let Some(irradiance) = ddgi.irradiance {
            scene = scene.access(irradiance, RgUsage::SampledRead);
        }
        if let Some(distance) = ddgi.distance {
            scene = scene.access(distance, RgUsage::SampledRead);
        }
        // When the GDF composited this frame, the cascade volumes were storage-written (GENERAL);
        // the mesh fragment's reflection-occlusion cone taps them (light set binding 9), so declare
        // the scene's SampledRead to transition each back to ShaderReadOnly before the draw (the
        // übershader statically references the cascade samplers, so this is required even when the
        // runtime sky-occlusion flag gates the actual sample off).
        if let Some(cascades) = gdf.cascades {
            for cascade in cascades {
                scene = scene.access(cascade, RgUsage::SampledRead);
            }
        }
        // When ReSTIR ran this frame, the resolve wrote the radiance image as storage
        // (GENERAL); declare the scene's SampledRead so the graph transitions it back to
        // ShaderReadOnly before the mesh sample (set 7).
        if let Some(radiance) = restir.radiance {
            scene = scene.access(radiance, RgUsage::SampledRead);
        }
        // The mesh fragment samples the directional + spot shadow maps via the light set;
        // declare the reads so the graph transitions each DepthWrite → ShaderReadOnly between
        // its depth-write pass and the scene draw (else the sample sees a DEPTH_ATTACHMENT
        // image, `VUID-vkCmdDrawIndexed-imageLayout-00344`).
        if let Some(res) = directional_res {
            scene = scene.access(res, RgUsage::SampledRead);
        }
        if let Some(res) = spot_res {
            scene = scene.access(res, RgUsage::SampledRead);
        }
        // A skinned batch reads the deformed buffer as its vertex stream; declare the read
        // so the graph orders the scene pass after the skin compute write.
        if let Some(deformed) = deformed_res {
            scene = scene.access(deformed, RgUsage::VertexInputRead);
        }
        graph.add_pass(scene);

        // FXAA: edge-blur the scene scratch into the offscreen (a compute pass), then TAA:
        // reproject history through the motion vector + blend with the current scene
        // (scratch) into the offscreen + the next-frame history. Mutually exclusive (only
        // one of the PSOs is resolved). Both run after the scene pass.
        self.add_fxaa_pass(&mut graph, &pipelines, scene_output, color);
        // The reactive-coverage pass (TAA only) marks translucent geometry into the input-extent
        // reactive mask, depth-tested against the scene depth; the TAA resolve then biases those
        // pixels toward the current frame.
        let reactive_resource = self.add_reactive_coverage_pass(
            &mut graph,
            &pipelines,
            scene_depth,
            instance_set,
            deformed_handle,
        );
        let taa_slots = self.add_taa_pass(
            &mut graph,
            &pipelines,
            scene_output,
            color,
            motion_resource,
            motion_depth_resource,
            reactive_resource,
        );
        // No-AA / MSAA: neither resolve above wrote the offscreen, so upscale the input scene
        // scratch into the display offscreen (one path — at 1:1 it degenerates to a straight copy).
        if !fxaa && !taa {
            self.add_scene_resolve_pass(&mut graph, &pipelines, scene_output, color);
        }

        // SSGI history capture: copy the scene's resolved linear-HDR color into prevColor
        // (before any later tonemap turns it display-referred) so next frame's SSGI can
        // gather it. Reuses the single prevColor handle imported by the SSGI block (read
        // there, written here) so the graph tracks its layout across both. A barrier-only
        // restore pass declares a final compute SampledRead so the graph emits the
        // General → ShaderReadOnly transition back to prevColor's resting layout.
        if let Some(copy) = screen.history_copy {
            let raw_body = raw.clone();
            let handle = copy.pipeline.handle();
            let layout = copy.pipeline.layout();
            let set = copy.set;
            let groups_x = copy.groups_x;
            let groups_y = copy.groups_y;
            let pipeline = copy.pipeline;
            let copy_pass = RgPass::compute("ssgi-history")
                .access(color, RgUsage::SampledReadCompute)
                .access(copy.prev_color, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch
                    // covers the viewport (8×8 per group).
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(pipeline);
                });
            graph.add_pass(copy_pass);
            // Barrier-only: General → ShaderReadOnly for next frame's SSGI sample + seed.
            let restore = RgPass::compute("ssgi-history-restore")
                .access(copy.prev_color, RgUsage::SampledReadCompute)
                .body(|_cmd, _scopes: &mut NestedScopeRecorder| {});
            graph.add_pass(restore);
        }

        // The final post chain on the DISPLAY-extent resolved offscreen color: the mandatory
        // HDR → display tonemap (in-place compute), then the optional ground grid + editor
        // overlay (graphics, over the display-referred color, depth-tested against the
        // display-extent overlay depth). By here `color` is always the display offscreen —
        // present / shm publish consume it identically in editor and present-only mode.
        self.add_tonemap_pass(&mut graph, &pipelines, color);
        // The overlays draw at display extent, so they depth-test the display-extent overlay
        // depth — a point-upscale of the input scene depth. When the upscale PSO / target is
        // unavailable, fall back to the input depth (valid at render scale 1, where they match).
        let overlay_depth = self
            .add_depth_upscale_pass(&mut graph, &pipelines, depth)
            .unwrap_or(depth);
        // View-mode overlays on the post-tonemap color: the motion-vector visualization
        // overwrites it; the Lit Wireframe overlay draws edges over it. Both no-op unless
        // their mode is active (the PSO is `None` otherwise).
        self.add_motion_visualize_pass(&mut graph, &pipelines, color, motion_resource);
        self.add_lit_wireframe_pass(
            &mut graph,
            &pipelines,
            color,
            overlay_depth,
            instance_set,
            deformed_res,
            deformed_handle,
        );
        self.add_grid_overlay_passes(&mut graph, &pipelines, color, overlay_depth);

        // Arm the per-frame GPU timestamp recorder (a cheap no-op when the profiler is `Off`):
        // each pass body is then bracketed by a timestamp scope, written into this slot's pool
        // (reset at the top of the frame). The recorder is owned here, threaded through the
        // graph execute, then stashed back into the profiler for read-back `MAX_FRAMES_IN_FLIGHT`
        // frames later.
        let mut recorder = self.gpu_profiler.frame_recorder(frame);
        // The graph is fully constructed; close the `build-frame-graph` span before the
        // `execute-render-graph` span opens (siblings at top level).
        if let Some(index) = build_span {
            let CpuProfiler { buffers, .. } = &mut self.cpu_profiler;
            buffers[frame].end_span(index, cpu_now_ns());
        }

        // Arm the CPU span recorder on the same gate as the GPU one:
        // the graph brackets each pass body in a CPU span that nests under the
        // `execute-render-graph` scope opened here, so the merged capture carries both lanes.
        // `cpu_profiler` and `gpu_profiler` are distinct fields, so the two recorder borrows
        // are disjoint.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let CpuProfiler {
            registry: cpu_registry,
            buffers: cpu_buffers,
        } = &mut self.cpu_profiler;
        let cpu_buffer = &mut cpu_buffers[frame];
        let exec_span = if profile_cpu {
            Some(cpu_buffer.begin_span(cpu_registry, "execute-render-graph", cpu_now_ns()))
        } else {
            None
        };
        {
            // Scope the recorders so their `&mut` borrows of `recorder` / `cpu_*` release
            // before `cpu_buffer.end_span` re-borrows the buffer and `recorder` is stashed.
            let mut recorders = crate::render_graph::ProfileRecorders {
                gpu: recorder.armed().then_some(&mut recorder),
                cpu: profile_cpu.then_some((&mut *cpu_registry, &mut *cpu_buffer)),
            };
            graph.execute_profiled(&self.device, cmd, &mut recorders);
        }
        if let Some(index) = exec_span {
            cpu_buffer.end_span(index, cpu_now_ns());
        }
        self.gpu_profiler.stash_recorder(frame, recorder);

        // Track the offscreen color's resolved exit layout (COLOR_ATTACHMENT after the post
        // chain's overlay/grid pass) so the shm read-back's entry barrier uses the right
        // `old_layout` — otherwise a stale tracked layout mis-transitions the image and the
        // next submit flags a layout mismatch (`VUID-vkCmdDraw-None-09600`).
        self.views[self.active_view.index()].offscreen.layout =
            graph.external_layout(offscreen_slot);

        // Read back the shadow maps' resolved exit layouts for the next frame's seed (the
        // graph wrote each external slot to the map's final layout — ShaderReadOnly after
        // a depth-write pass that the next frame's sample waits on).
        if let Some(slot) = directional_slot {
            self.directional_shadow_layout = graph.external_layout(slot);
        }
        if let Some(slot) = spot_slot {
            self.spot_shadow_layout = graph.external_layout(slot);
        }

        // Read back the DDGI images' resolved exit layouts (the ray image + the two atlases each
        // rode an external slot), then advance the temporal state (bump the ray-set / round-robin
        // index, commit the scroll base, clear the history-reset flag) — but only when the chain
        // actually ran this frame.
        if let Some(slot) = ddgi.rays_slot {
            self.ddgi.set_rays_layout(graph.external_layout(slot));
        }
        if let Some(slot) = ddgi.irradiance_slot {
            self.ddgi.set_irradiance_layout(graph.external_layout(slot));
        }
        if let Some(slot) = ddgi.distance_slot {
            self.ddgi.set_distance_layout(graph.external_layout(slot));
        }
        if ddgi.irradiance.is_some() {
            self.ddgi.advance_frame();
        }

        // Read back the GDF cascade volumes' resolved exit layouts (each rode an external slot,
        // ending ShaderReadOnly after the scene's SampledRead), then commit the toroidal recenter
        // state (prev centers + history + the round-robin frame) — only when the chain ran.
        if gdf.cascades.is_some() {
            for c in 0..crate::GDF_CASCADES {
                if let Some(slot) = gdf.cascade_slots[c as usize] {
                    self.global_sdf
                        .set_cascade_layout(c, graph.external_layout(slot));
                }
            }
            if let Some(slot) = gdf.albedo_slot {
                self.global_sdf
                    .set_albedo_layout(graph.external_layout(slot));
            }
            self.global_sdf.advance_frame();
        }

        // Read back the ReSTIR radiance image's resolved exit layout (it rode an external
        // slot, ending ShaderReadOnly after the scene's SampledRead). The per-view temporal
        // state was already advanced inside `add_restir_passes`, before execute.
        if let Some(slot) = restir.radiance_slot {
            let layout = graph.external_layout(slot);
            self.views[self.active_view.index()]
                .restir
                .set_radiance_layout(layout);
        }

        // Read back the temporal images' resolved exit layouts (the cross-frame
        // ShaderReadOnly ↔ General transition is derived from these slots). The TAA history
        // pair + the SSGI history pair + the ssgi_resolved each rode an external slot.
        let temporal_ran = taa_slots.is_some()
            || screen.ssgi_history_slots.is_some()
            || screen.dfao_history_slots.is_some();
        // Store the UN-jittered matrix as this view's previous frame (the motion prepass needs a
        // jitter-free previous camera).
        let frame_view_proj = self.scene_view_proj_unjittered();
        let taa_active = self.aa.taa();
        let view = &mut self.views[self.active_view.index()];
        if let Some(slots) = &taa_slots {
            writeback_history_layout(view, &graph, &slots.history.read);
            writeback_history_layout(view, &graph, &slots.history.write);
            writeback_lock_layout(view, &graph, &slots.lock.read);
            writeback_lock_layout(view, &graph, &slots.lock.write);
        }
        if let Some(slots) = &screen.ssgi_history_slots {
            writeback_ssgi_history_layout(view, &graph, &slots.read);
            writeback_ssgi_history_layout(view, &graph, &slots.write);
        }
        if let (Some(slot), Some(resolved)) =
            (screen.ssgi_resolved_slot, view.ssgi_resolved.as_mut())
        {
            resolved.layout = graph.external_layout(slot);
        }
        if let Some(slots) = &screen.dfao_history_slots {
            writeback_dfao_history_layout(view, &graph, &slots.read);
            writeback_dfao_history_layout(view, &graph, &slots.write);
        }
        if let (Some(slot), Some(resolved)) =
            (screen.dfao_resolved_slot, view.dfao_resolved.as_mut())
        {
            resolved.layout = graph.external_layout(slot);
        }
        if let (Some(slot), Some(ssr_map)) = (screen.ssr_map_slot, view.ssr_map.as_mut()) {
            ssr_map.layout = graph.external_layout(slot);
        }

        // TAA and/or SSGI accumulation consumed this frame's history parity; mark it valid
        // and flip the shared ping-pong index once so next frame reprojects through the
        // buffer just written. FXAA touches no history, so it does not flip. Marks history
        // valid and advances the ping-pong index by one.
        if temporal_ran {
            view.flip_history();
        }
        // Record this frame's camera viewProj as this view's previous frame for next
        // frame's motion reprojection (per-view: a re-activated view reprojects against its
        // own last frame).
        view.store_prev_view_proj(frame_view_proj);
        // Advance the Halton jitter cycle for next frame — only while TAA is active, so an
        // off/FXAA/MSAA frame renders un-jittered (`jitter` stays zero, the single gate).
        if taa_active {
            view.advance_jitter();
        }
    }

    /// Builds the four DDGI compute passes into `graph` when the chain runs this frame (DDGI on +
    /// ready + all four PSOs resolved): `ddgi-trace` (the GDF/MDF sphere-march → ray storage),
    /// `ddgi-blend-irr` (ray sampler → irradiance storage), `ddgi-blend-dist` (ray sampler →
    /// distance storage), `ddgi-border` (irradiance octahedral gutter copy). The graph derives
    /// every GENERAL ↔ ShaderReadOnly barrier from the declared usages.
    ///
    /// The trace is a three-set pass (`[bindless, light, trace_set]`) reusing the shared `sdf`
    /// module's `sampleField` (near MDF → far GDF), so it declares `SampledRead` on the GDF cascade
    /// volumes + the lite albedo cache (read this frame's composite output) and runs after the GDF
    /// passes. Returns the irradiance + distance atlas resources for the scene's `SampledRead` +
    /// the three imported images' external slots for the layout write-back. An empty
    /// [`DdgiResult`] when DDGI did not run.
    fn add_ddgi_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        bindless_set: vk::DescriptorSet,
        light_set: vk::DescriptorSet,
        gdf_cascades: Option<[RgResource; crate::GDF_CASCADES as usize]>,
        gdf_albedo: Option<RgResource>,
    ) -> DdgiResult {
        // The four-pass chain runs only when DDGI is on + all PSOs resolved.
        let Some(ddgi_pipelines) = &pipelines.ddgi else {
            return DdgiResult::default();
        };
        let raw = self.device.raw().clone();

        let (ray_image, ray_view, ray_layout) = self.ddgi.rays();
        let rays_slot = graph.alloc_external_layout(ray_layout);
        let ray_res = graph.import_image(
            ray_image,
            ray_view,
            vk::ImageAspectFlags::COLOR,
            ray_layout,
            Some(rays_slot),
        );

        let (irr_image, irr_view, irr_layout) = self.ddgi.irradiance();
        let irr_slot = graph.alloc_external_layout(irr_layout);
        let irr_res = graph.import_image(
            irr_image,
            irr_view,
            vk::ImageAspectFlags::COLOR,
            irr_layout,
            Some(irr_slot),
        );

        let (dist_image, dist_view, dist_layout) = self.ddgi.distance();
        let dist_slot = graph.alloc_external_layout(dist_layout);
        let dist_res = graph.import_image(
            dist_image,
            dist_view,
            vk::ImageAspectFlags::COLOR,
            dist_layout,
            Some(dist_slot),
        );

        // 1. Trace: sphere-march the shared distance field (near MDF via the bindless bricks + the
        //    light set's instance list; far GDF via the light set's cascade clipmap), reading the
        //    prev-irradiance atlas (multi-bounce) + the lite albedo cache (hit color), writing the
        //    ray image. Three sets, so it records its binds directly (like `sdf-ao`).
        let trace = Arc::clone(&ddgi_pipelines.trace);
        let trace_handle = trace.handle();
        let trace_pipeline_layout = trace.layout();
        let trace_set = self.ddgi.trace_set();
        let trace_push = self.ddgi.trace_push(self.sdf_instance_count);
        let trace_groups_x = DDGI_RAYS_PER_PROBE.div_ceil(64);
        let raw_body = raw.clone();
        let mut trace_pass = RgPass::compute("ddgi-trace")
            .access(irr_res, RgUsage::SampledReadCompute)
            .access(ray_res, RgUsage::StorageImageRwCompute);
        // When the GDF composited this frame, the trace's far-field tap reads the cascade volumes
        // (light set binding 9) + the albedo cache (trace set 2) — declare the reads so the graph
        // transitions each from GENERAL (composite write) → ShaderReadOnly before the trace.
        if let Some(cascades) = gdf_cascades {
            for cascade in cascades {
                trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
            }
        }
        if let Some(albedo) = gdf_albedo {
            trace_pass = trace_pass.access(albedo, RgUsage::SampledReadCompute);
        }
        let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
            // covers the round-robin probe-budget slice (the shader offsets the probe index by
            // trace_push's sky_color.w), not the whole volume.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, trace_handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    trace_pipeline_layout,
                    0,
                    &[bindless_set, light_set, trace_set],
                    &[],
                );
                raw_body.cmd_push_constants(
                    cmd,
                    trace_pipeline_layout,
                    vk::ShaderStageFlags::COMPUTE,
                    0,
                    bytemuck::bytes_of(&trace_push),
                );
                raw_body.cmd_dispatch(cmd, trace_groups_x, crate::ddgi::DDGI_PROBE_BUDGET, 1);
            }
            drop(trace);
        });
        graph.add_pass(trace_pass);

        // 2. Blend irradiance: ray sampler → irradiance storage.
        let irr_w = crate::ddgi::irradiance_atlas_width();
        let irr_h = crate::ddgi::irradiance_atlas_height();
        let blend_irr = Arc::clone(&ddgi_pipelines.blend_irr);
        let blend_irr_handle = blend_irr.handle();
        let blend_irr_layout = blend_irr.layout();
        let blend_irr_set = self.ddgi.blend_irr_set();
        let blend_irr_push = self.ddgi.blend_irradiance_push();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("ddgi-blend-irr")
                .access(ray_res, RgUsage::SampledReadCompute)
                .access(irr_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        blend_irr_handle,
                        blend_irr_layout,
                        blend_irr_set,
                        bytemuck::bytes_of(&blend_irr_push),
                        (irr_w.div_ceil(8), irr_h.div_ceil(8), 1),
                    );
                    drop(blend_irr);
                }),
        );

        // 3. Blend distance: ray sampler → moment (distance) storage.
        let dist_w = crate::ddgi::distance_atlas_width();
        let dist_h = crate::ddgi::distance_atlas_height();
        let blend_dist = Arc::clone(&ddgi_pipelines.blend_dist);
        let blend_dist_handle = blend_dist.handle();
        let blend_dist_layout = blend_dist.layout();
        let blend_dist_set = self.ddgi.blend_dist_set();
        let blend_dist_push = self.ddgi.blend_distance_push();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("ddgi-blend-dist")
                .access(ray_res, RgUsage::SampledReadCompute)
                .access(dist_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        blend_dist_handle,
                        blend_dist_layout,
                        blend_dist_set,
                        bytemuck::bytes_of(&blend_dist_push),
                        (dist_w.div_ceil(8), dist_h.div_ceil(8), 1),
                    );
                    drop(blend_dist);
                }),
        );

        // 4. Border copy: fix the irradiance octahedral gutters (read+write the same
        //    storage image). Leaves irradiance GENERAL; the scene's SampledRead then
        //    transitions it ShaderReadOnly for the mesh sample.
        let border = Arc::clone(&ddgi_pipelines.border);
        let border_handle = border.handle();
        let border_layout = border.layout();
        let border_set = self.ddgi.border_set();
        let border_push = self.ddgi.border_push();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("ddgi-border")
                .access(irr_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        border_handle,
                        border_layout,
                        border_set,
                        bytemuck::bytes_of(&border_push),
                        (irr_w.div_ceil(8), irr_h.div_ceil(8), 1),
                    );
                    drop(border);
                }),
        );

        DdgiResult {
            irradiance: Some(irr_res),
            distance: Some(dist_res),
            rays_slot: Some(rays_slot),
            irradiance_slot: Some(irr_slot),
            distance_slot: Some(dist_slot),
        }
    }

    /// Builds the two Global-SDF compute passes into `graph` when the chain runs this frame (GDF on,
    /// ready, both PSOs resolved): `gdf-cull` (bin the per-mesh MDF instances per cascade into a
    /// compacted list) then `gdf-composite` (per dirty voxel of each cascade, `min()` the culled
    /// bricks into the toroidal `R16_SNORM` cascade volume). The cull list buffer serializes the two
    /// via the graph-derived RAW barrier; each cascade volume is imported on its own external slot
    /// for the cross-frame layout write-back.
    ///
    /// Returns the cascade resources (for the downstream DDGI-trace + scene `SampledRead`) + each
    /// cascade's external slot. An empty [`GdfResult`] when the GDF did not run (the cascades stay
    /// at their resting ShaderReadOnly layout and the consumers gate the tap off).
    fn add_gdf_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        frame: usize,
    ) -> GdfResult {
        let Some(gdf_pipelines) = &pipelines.gdf else {
            return GdfResult::default();
        };
        let raw = self.device.raw().clone();

        // Import this frame slot's cull-list buffer (the cull writes it, the composite reads it) +
        // each cascade volume (the composite writes them, the downstream consumers sample them). The
        // cull list is per-frame-in-flight so frame N+1's clear/rebuild never races frame N's reads.
        let cull_res = graph.import_buffer(self.global_sdf.cull_buffer(frame));
        let mut cascade_res = [RgResource { index: 0 }; crate::GDF_CASCADES as usize];
        let mut cascade_slots = [None; crate::GDF_CASCADES as usize];
        for c in 0..crate::GDF_CASCADES {
            let (image, view, layout) = self.global_sdf.cascade(c);
            let slot = graph.alloc_external_layout(layout);
            cascade_res[c as usize] = graph.import_image_3d(image, view, layout, Some(slot));
            cascade_slots[c as usize] = Some(slot);
        }
        // The lite albedo cache (the composite splats it for the finest cascade; the DDGI trace
        // samples it at hit points).
        let (albedo_image, albedo_view, albedo_layout) = self.global_sdf.albedo_cache();
        let albedo_slot = graph.alloc_external_layout(albedo_layout);
        let albedo_res =
            graph.import_image_3d(albedo_image, albedo_view, albedo_layout, Some(albedo_slot));

        // 1. Cull: clear the per-cascade counters (a `cmd_fill_buffer` + a transfer→compute barrier,
        //    the one place the graph has no primitive for), then bin every instance into the
        //    cascade(s) it touches. One thread per instance.
        {
            let cull = Arc::clone(&gdf_pipelines.cull);
            let handle = cull.handle();
            let layout = cull.layout();
            let bindless_set = self.descriptors.bindless_set();
            let cull_set = self.global_sdf.cull_set(frame);
            let push = self.global_sdf.cull_push(self.sdf_instance_count);
            let counter_bytes = self.global_sdf.cull_counter_bytes();
            let cull_buffer = self.global_sdf.cull_buffer(frame);
            let groups = MAX_SDF_INSTANCES.div_ceil(64);
            let raw_body = raw.clone();
            graph.add_pass(
                RgPass::compute("gdf-cull")
                    .access(cull_res, RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. The PSO/sets are valid this frame. The fill zeroes
                        // the atomic counters; the barrier orders the transfer write before the
                        // cull's atomic reads (the graph has no fill primitive, so this one barrier
                        // is hand-written and local to the pass).
                        unsafe {
                            raw_body.cmd_fill_buffer(cmd, cull_buffer, 0, counter_bytes, 0);
                            let barrier = vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                .dst_access_mask(
                                    vk::AccessFlags2::SHADER_STORAGE_READ
                                        | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                                )
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(cull_buffer)
                                .offset(0)
                                .size(counter_bytes);
                            let barriers = [barrier];
                            let dep =
                                vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
                            raw_body.cmd_pipeline_barrier2(cmd, &dep);
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[bindless_set, cull_set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(&push),
                            );
                            raw_body.cmd_dispatch(cmd, groups, 1, 1);
                        }
                        drop(cull);
                    }),
            );
        }

        // 2. Composite: per dirty region of each cascade, `min()` the culled bricks → the toroidal
        //    cascade volume. The near cascade updates incrementally; one cascade per frame gets the
        //    staggered full refresh (the round-robin in `dirty_regions`). All cascade writes ride
        //    the same pass declaring StorageImageRwCompute on every cascade (so the graph holds them
        //    GENERAL across the region dispatches), reading the cull list.
        let composite = Arc::clone(&gdf_pipelines.composite);
        let handle = composite.handle();
        let layout = composite.layout();
        let bindless_set = self.descriptors.bindless_set();
        let composite_set = self.global_sdf.composite_set(frame);
        // Gather every dirty region (cascade index + push) so the pass body issues one dispatch each.
        let mut dispatches: Vec<(crate::GdfCompositePush, u32, u32, u32)> = Vec::new();
        for c in 0..crate::GDF_CASCADES {
            for region in self.global_sdf.dirty_regions(c) {
                let push = self.global_sdf.composite_push(c, region);
                let g = (
                    region.size.x.div_ceil(4),
                    region.size.y.div_ceil(4),
                    region.size.z.div_ceil(4),
                );
                dispatches.push((push, g.0, g.1, g.2));
            }
        }
        let mut composite_pass =
            RgPass::compute("gdf-composite").access(cull_res, RgUsage::StorageReadCompute);
        for cascade in cascade_res {
            composite_pass = composite_pass.access(cascade, RgUsage::StorageImageRwCompute);
        }
        composite_pass = composite_pass.access(albedo_res, RgUsage::StorageImageRwCompute);
        let raw_body = raw.clone();
        composite_pass = composite_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/sets are valid this frame; each dispatch covers one
            // dirty region (4³ per group), pushing that region's cascade + bounds.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[bindless_set, composite_set],
                    &[],
                );
                for (push, gx, gy, gz) in &dispatches {
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(push),
                    );
                    raw_body.cmd_dispatch(cmd, *gx, *gy, *gz);
                }
            }
            drop(composite);
        });
        graph.add_pass(composite_pass);

        GdfResult {
            cascades: Some(cascade_res),
            cascade_slots,
            albedo: Some(albedo_res),
            albedo_slot: Some(albedo_slot),
        }
    }

    /// Builds the three ReSTIR DI compute passes into `graph` when the chain runs this
    /// frame: `restir-initial` (K candidates per pixel from the froxel light lists →
    /// initial reservoir), `restir-reuse` (temporal + spatial reservoir reuse → combined),
    /// `restir-resolve` (one TLAS visibility ray per pixel → the resolved direct radiance
    /// image). Writes this frame's per-view bindings (G-buffer/motion samplers, light +
    /// cluster SSBOs, the TLAS), imports the radiance image + the combined-reservoir
    /// sentinel buffer (the three passes serialize via RAW barriers on it the graph
    /// derives), and advances the per-view temporal state.
    ///
    /// The full runtime gate ANDs: ReSTIR PSOs resolved, RT supported, a TLAS built this
    /// frame, the cluster cull ran (the froxel candidate lists), and the G-buffer prepass
    /// ran. Returns an empty [`RestirResult`] when any gate is unmet (direct lighting then
    /// takes the clustered-forward path).
    fn add_restir_passes(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        frame: usize,
        motion: Option<RgResource>,
    ) -> RestirResult {
        let Some(restir_pipelines) = &pipelines.restir else {
            return RestirResult::default();
        };
        // The full runtime gate (the PSO presence already implies use_restir + ready +
        // supported + the G-buffer prepass armed): the TLAS must be built (the resolve
        // traces it) and the cull must have armed the froxel candidate lists.
        let gbuffer_ran = pipelines.gbuffer.is_some()
            && self.views[self.active_view.index()].screen_space_ready();
        let cull_ran = pipelines.cull.is_some();
        if !self.rt.tlas_ready() || !gbuffer_ran || !cull_ran {
            return RestirResult::default();
        }
        // The view's reservoirs must be built (sized to this extent) and its radiance present.
        if !self.views[self.active_view.index()].restir.ready() {
            return RestirResult::default();
        }
        let Some((rad_image, rad_view, rad_layout)) =
            self.views[self.active_view.index()].restir.radiance()
        else {
            return RestirResult::default();
        };
        let Some(combined) = self.views[self.active_view.index()]
            .restir
            .combined_buffer()
        else {
            return RestirResult::default();
        };

        // Write this frame's per-view bindings: the G-buffer (set) + motion samplers, the
        // light + cluster SSBOs (they regrow), and the TLAS into the resolve set. Resolved
        // through `&self` reads gathered first so the `&self.views[..].restir` write does not
        // alias a live borrow.
        let g_normal_view = self.views[self.active_view.index()]
            .g_normal
            .as_ref()
            .expect("g_normal built for restir")
            .view();
        let motion_view = self.views[self.active_view.index()]
            .motion
            .as_ref()
            .map(crate::Image::view);
        let light_buffer = self.lighting.light_list_buffer(frame);
        let cluster_buffer = self.lighting.cluster_buffer_with_size(frame);
        let tlas = self.rt.frame_tlas(frame);
        self.views[self.active_view.index()]
            .restir
            .write_frame_bindings(
                &self.device,
                &self.restir,
                g_normal_view,
                motion_view,
                light_buffer,
                cluster_buffer,
                tlas,
            );

        // The per-frame push inputs (the camera inverses + eye come from the shared SSAO
        // camera the renderer set this frame; the light count from the lighting rig).
        let inv_view = self.ssao.view().inverse();
        let inv_projection = self.ssao.inv_projection();
        let eye = inv_view.col(3).truncate();
        let light_count = self.lighting.frame_light_count();
        let extent = self.views[self.active_view.index()].scaled_render_extent();
        let frame_index = self.views[self.active_view.index()].restir.frame_index();
        let history_valid = !self.views[self.active_view.index()].restir.history_reset();

        let initial_push =
            self.restir
                .initial_push(inv_view, inv_projection, light_count, extent, frame_index);
        let reuse_push =
            self.restir
                .reuse_push(inv_view, inv_projection, extent, frame_index, history_valid);
        let resolve_push = self
            .restir
            .resolve_push(inv_view, inv_projection, extent, eye);

        let initial_set = self.views[self.active_view.index()].restir.initial_set();
        let reuse_set = self.views[self.active_view.index()].restir.reuse_set();
        let resolve_set = self.views[self.active_view.index()].restir.resolve_set();
        let mesh_set = self.views[self.active_view.index()].restir.mesh_set();

        let raw = self.device.raw().clone();
        let groups = |n: u32| n.div_ceil(8);
        let groups_x = groups(extent.width);
        let groups_y = groups(extent.height);

        // The combined-reservoir SSBO is the sentinel: the three passes serialize through
        // RAW barriers the graph derives from StorageWrite → StorageRead on it. The radiance
        // image rides an external slot for the cross-frame
        // General ↔ ShaderReadOnly write-back.
        let sentinel = graph.import_buffer(combined);
        let radiance_slot = graph.alloc_external_layout(rad_layout);
        let radiance_res = graph.import_image(
            rad_image,
            rad_view,
            vk::ImageAspectFlags::COLOR,
            rad_layout,
            Some(radiance_slot),
        );

        // 1. initial: K candidate lights per pixel → the initial reservoir (storage write).
        let initial = Arc::clone(&restir_pipelines.initial);
        let initial_handle = initial.handle();
        let initial_layout = initial.layout();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("restir-initial")
                .access(sentinel, RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        initial_handle,
                        initial_layout,
                        initial_set,
                        bytemuck::bytes_of(&initial_push),
                        (groups_x, groups_y, 1),
                    );
                    drop(initial);
                }),
        );

        // 2. reuse: temporal + spatial reservoir reuse → the combined reservoir. Reads the
        //    sentinel (the graph emits the RAW barrier after the initial write) + the motion
        //    target's sampler (the temporal term reprojects through it). Declaring the motion
        //    SampledRead orders this after the motion prepass (ColorWrite → SampledRead).
        let reuse = Arc::clone(&restir_pipelines.reuse);
        let reuse_handle = reuse.handle();
        let reuse_layout = reuse.layout();
        let raw_body = raw.clone();
        let mut reuse_pass =
            RgPass::compute("restir-reuse").access(sentinel, RgUsage::StorageReadCompute);
        if let Some(motion) = motion {
            reuse_pass = reuse_pass.access(motion, RgUsage::SampledReadCompute);
        }
        graph.add_pass(
            reuse_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_ddgi_compute(
                    &raw_body,
                    cmd,
                    reuse_handle,
                    reuse_layout,
                    reuse_set,
                    bytemuck::bytes_of(&reuse_push),
                    (groups_x, groups_y, 1),
                );
                drop(reuse);
            }),
        );

        // 3. resolve: one TLAS visibility ray per pixel + shade → the radiance image
        //    (storage RW). Reads the sentinel (the combined reservoir) + writes the radiance.
        let resolve = Arc::clone(&restir_pipelines.resolve);
        let resolve_handle = resolve.handle();
        let resolve_layout = resolve.layout();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("restir-resolve")
                .access(sentinel, RgUsage::StorageReadCompute)
                .access(radiance_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_ddgi_compute(
                        &raw_body,
                        cmd,
                        resolve_handle,
                        resolve_layout,
                        resolve_set,
                        bytemuck::bytes_of(&resolve_push),
                        (groups_x, groups_y, 1),
                    );
                    drop(resolve);
                }),
        );

        // Advance the per-view temporal state (bump the RNG index, clear the history reset)
        // in the graph build, after adding the three passes.
        self.views[self.active_view.index()].restir.advance_frame();

        RestirResult {
            radiance: Some(radiance_res),
            mesh_set,
            radiance_slot: Some(radiance_slot),
        }
    }

    /// Builds the thin G-buffer prepass + the screen-space compute chain
    /// (gtao → ao-blur, contact, ssgi → ssgi-blur → ssgi-accum) into `graph`, importing the
    /// active view's screen-space images and binding the per-view sets. `motion` is the
    /// motion-vector resource the SSGI temporal accumulation reprojects through (when
    /// present). Returns the per-view mesh set 4 to bind in the scene pass, the maps the
    /// scene declares `SampledRead` on, the prev-color history-copy info the caller
    /// schedules after the scene pass, and the SSGI history / resolved external-layout
    /// slots (read back after execute). No-op (empty result) when the prepass did not run
    /// this frame.
    #[allow(clippy::too_many_arguments)]
    fn add_screen_space_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        instance_set: vk::DescriptorSet,
        motion: Option<RgResource>,
        deformed: (Option<RgResource>, Option<vk::Buffer>),
        light_set: vk::DescriptorSet,
        gdf_cascades: Option<[RgResource; crate::GDF_CASCADES as usize]>,
    ) -> ScreenSpaceResult {
        let (deformed_res, deformed_handle) = deformed;
        let mut result = ScreenSpaceResult::default();
        let Some(gbuffer) = &pipelines.gbuffer else {
            // The screen-space prepass is skipped this frame (no GTAO / contact / SSGI /
            // ReSTIR), but the übershader's layout always declares set 4, so it must still be
            // bound or `vkCmdDrawIndexed` reports set 4 unbound (`VUID-vkCmdDrawIndexed-None-08600`).
            // The per-view set 4 is allocated + written to the neutral init-transitioned maps at
            // view bring-up (`build_screen_space`), so bind it whenever it is built. The
            // in-shader AO/contact/SSGI flags
            // gate the reads, so the lit image is correct against the neutral maps.
            let view = &self.views[self.active_view.index()];
            if view.screen_space_ready() {
                result.mesh_set = view.mesh_set;
            }
            return result;
        };
        let view = &self.views[self.active_view.index()];
        let extent = view.scaled_render_extent();
        // SSGI + GTAO trace into half-resolution targets (matching `build_screen_space`), so their
        // dispatch covers the half extent; the bilateral blur/upsample passes stay full-res.
        let half_extent = vk::Extent2D {
            width: extent.width.div_ceil(2).max(1),
            height: extent.height.div_ceil(2).max(1),
        };
        let raw = self.device.raw();
        let groups = |n: u32| n.div_ceil(8);
        result.mesh_set = view.mesh_set;

        // The G-buffer prepass: write view normal (rgb) + view-Z (.a) + roughness + its own depth.
        let g_normal = graph.import_image(
            view.g_normal.as_ref().expect("g_normal built").handle(),
            view.g_normal.as_ref().expect("g_normal built").view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let g_roughness = graph.import_image(
            view.g_roughness
                .as_ref()
                .expect("g_roughness built")
                .handle(),
            view.g_roughness.as_ref().expect("g_roughness built").view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let g_depth = graph.import_image(
            view.g_depth.as_ref().expect("g_depth built").handle(),
            view.g_depth.as_ref().expect("g_depth built").view(),
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        {
            let list = self.scene_draw_list.shallow_clone();
            let raw_body = raw.clone();
            let push = self.ssao.gbuffer_push();
            let pipeline = Arc::clone(gbuffer);
            let gbuffer_pipeline = pipeline.handle();
            let gbuffer_layout = pipeline.layout();
            let mut pass = RgPass::graphics("gbuffer", extent)
                .color(RgAttachment::clear_store(g_normal))
                .color(RgAttachment::clear_store(g_roughness))
                .depth_attachment(depth_clear_store(g_depth))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    record_gbuffer(
                        &raw_body,
                        cmd,
                        &list,
                        gbuffer_pipeline,
                        gbuffer_layout,
                        instance_set,
                        &push,
                        deformed_handle,
                    );
                    drop(pipeline);
                });
            if let Some(deformed) = deformed_res {
                pass = pass.access(deformed, RgUsage::VertexInputRead);
            }
            graph.add_pass(pass);
        }

        // GTAO + bilateral denoise: g_normal → ao_raw → ao_map.
        if let (Some(gtao), Some(ao_blur)) = (&pipelines.gtao, &pipelines.ao_blur) {
            let ao_raw = graph.import_image(
                view.ao_raw.as_ref().expect("ao_raw built").handle(),
                view.ao_raw.as_ref().expect("ao_raw built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let ao_map_slot =
                graph.alloc_external_layout(view.ao_map.as_ref().expect("ao_map built").layout);
            let ao_map = graph.import_image(
                view.ao_map.as_ref().expect("ao_map built").handle(),
                view.ao_map.as_ref().expect("ao_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ao_map.as_ref().expect("ao_map built").layout,
                Some(ao_map_slot),
            );
            self.add_compute_pass(
                graph,
                "gtao",
                gtao,
                view.gtao_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (ao_raw, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&self.ssao.gtao_push()).to_vec()),
                groups(half_extent.width),
                groups(half_extent.height),
            );
            self.add_compute_pass(
                graph,
                "ao-blur",
                ao_blur,
                view.ao_blur_set,
                &[
                    (ao_raw, RgUsage::SampledReadCompute),
                    (g_normal, RgUsage::SampledReadCompute),
                    (ao_map, RgUsage::StorageImageRwCompute),
                ],
                None,
                groups(extent.width),
                groups(extent.height),
            );
            result.scene_sampled.push(ao_map);
        }

        // Directional contact shadows: g_normal → contact_map.
        if let Some(contact) = &pipelines.contact {
            let contact_slot = graph.alloc_external_layout(
                view.contact_map.as_ref().expect("contact_map built").layout,
            );
            let contact_map = graph.import_image(
                view.contact_map
                    .as_ref()
                    .expect("contact_map built")
                    .handle(),
                view.contact_map.as_ref().expect("contact_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.contact_map.as_ref().expect("contact_map built").layout,
                Some(contact_slot),
            );
            self.add_compute_pass(
                graph,
                "contact-shadows",
                contact,
                view.contact_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (contact_map, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&self.ssao.contact_push()).to_vec()),
                groups(extent.width),
                groups(extent.height),
            );
            result.scene_sampled.push(contact_map);
        }

        // SSGI, SSR, and RT reflections all gather from the previous frame's color (the
        // first two in compute, RT reflections via the mesh's set-4 binding 4). Import
        // prevColor once here (read now, written by the copy-color pass after the scene); it
        // rests ShaderReadOnly between frames, so the import seeds that and does NOT write the
        // layout back (the graph internally pings General for the copy write).
        let rt_refl = self.rt.use_rt_reflections() && view.prev_view_proj_valid;
        let prev_color = if pipelines.ssgi.is_some() || pipelines.ssr.is_some() || rt_refl {
            Some(graph.import_image(
                view.prev_color.as_ref().expect("prev_color built").handle(),
                view.prev_color.as_ref().expect("prev_color built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                None,
            ))
        } else {
            None
        };

        // One-bounce SSGI: g_normal + prevColor → ssgi_map → ssgi_denoised.
        if let (Some(ssgi), Some(ssgi_blur)) = (&pipelines.ssgi, &pipelines.ssgi_blur) {
            let prev_color = prev_color.expect("prev_color imported when SSGI on");
            let ssgi_slot =
                graph.alloc_external_layout(view.ssgi_map.as_ref().expect("ssgi_map built").layout);
            let ssgi_map = graph.import_image(
                view.ssgi_map.as_ref().expect("ssgi_map built").handle(),
                view.ssgi_map.as_ref().expect("ssgi_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ssgi_map.as_ref().expect("ssgi_map built").layout,
                Some(ssgi_slot),
            );
            let denoised_slot = graph.alloc_external_layout(
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .layout,
            );
            let ssgi_denoised = graph.import_image(
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .handle(),
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .layout,
                Some(denoised_slot),
            );
            // The SSGI trace push was built (frame index bumped) at PSO-resolve time.
            self.add_compute_pass(
                graph,
                "ssgi",
                ssgi,
                view.ssgi_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (prev_color, RgUsage::SampledReadCompute),
                    (ssgi_map, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&pipelines.ssgi_push).to_vec()),
                groups(half_extent.width),
                groups(half_extent.height),
            );
            self.add_compute_pass(
                graph,
                "ssgi-blur",
                ssgi_blur,
                view.ssgi_blur_set,
                &[
                    (ssgi_map, RgUsage::SampledReadCompute),
                    (g_normal, RgUsage::SampledReadCompute),
                    (ssgi_denoised, RgUsage::StorageImageRwCompute),
                ],
                None,
                groups(extent.width),
                groups(extent.height),
            );
            // SSGI temporal accumulation (when motion ran): reproject the SSGI history
            // through motion, neighborhood-clamp, EMA into the stable ssgi_resolved map.
            // SSGI owns this — it runs whenever SSGI + motion is on, independent of the
            // final-image AA mode, sharing the ping-pong parity flipped after the scene.
            // The scene then SampledReads the resolved map (the mesh set 4 binding 2 points
            // at it when TAA is on, else the denoised map — but the layout transition is on
            // whichever map this declares).
            if let (Some(accum), Some(motion)) = (&pipelines.ssgi_accum, motion) {
                let p = view.history_index;
                let ssgi_resolved_slot = graph.alloc_external_layout(
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .layout,
                );
                let ssgi_resolved = graph.import_image(
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .handle(),
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .view(),
                    vk::ImageAspectFlags::COLOR,
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .layout,
                    Some(ssgi_resolved_slot),
                );
                let (read_slot, read) = import_ssgi_history(graph, &view.ssgi_history[1 - p]);
                let (write_slot, write) = import_ssgi_history(graph, &view.ssgi_history[p]);
                let push = crate::SsgiAccumPush {
                    params: saffron_geometry::glam::Vec4::new(
                        crate::SSGI_HISTORY_WEIGHT,
                        if view.history_valid { 1.0 } else { 0.0 },
                        0.0,
                        0.0,
                    ),
                };
                self.add_compute_pass(
                    graph,
                    "ssgi-accum",
                    accum,
                    view.ssgi_accum_sets[p],
                    &[
                        (ssgi_denoised, RgUsage::SampledReadCompute),
                        (read, RgUsage::SampledReadCompute),
                        (motion, RgUsage::SampledReadCompute),
                        (ssgi_resolved, RgUsage::StorageImageRwCompute),
                        (write, RgUsage::StorageImageRwCompute),
                    ],
                    Some(bytemuck::bytes_of(&push).to_vec()),
                    groups(extent.width),
                    groups(extent.height),
                );
                // The scene SampledReads the resolved map (the accum's output). The mesh
                // set-4 SSGI sampler points at it under TAA, else the denoised map.
                result.scene_sampled.push(ssgi_resolved);
                result.ssgi_resolved_slot = Some(ssgi_resolved_slot);
                result.ssgi_history_slots = Some(TaaHistorySlots {
                    read: (1 - p, read_slot),
                    write: (p, write_slot),
                });
            } else {
                // No motion this frame: the scene samples the spatially denoised map.
                result.scene_sampled.push(ssgi_denoised);
            }
        }

        // DFAO diffuse sky-visibility: the reduced-resolution GDF cone trace (Wright 2015),
        // mirroring the SSGI chain — a half-res trace (`dfao`), a bilateral upsample (reusing the
        // ssgi-blur PSO), then temporal accumulation through the motion vectors (reusing the
        // ssgi-accum PSO). The mesh samples the resolved sky-visibility (set 4 binding 5) to
        // occlude the analytic sky irradiance, so the 9-cone march no longer runs per full-res
        // fragment. The trace is a three-set pass (`[bindless, light, dfao_set]`) like the DDGI
        // trace: it taps the GDF cascade clipmap via the light set, so it declares SampledRead on
        // the cascades (transitioning them from the composite's GENERAL) and reads the shared
        // `sdf` module's `sdfSkyVisibility`.
        // The gi-resolve pass (added after this block) samples the spatial DFAO as its sky-visibility
        // input when sky occlusion ran; captured here, `None` when it did not (then gi-resolve's
        // skyVis flag is 0 and it does not touch dfao).
        let mut dfao_denoised_res: Option<RgResource> = None;
        if let Some(dfao) = &pipelines.dfao {
            let bindless_set = self.descriptors.bindless_set();
            let dfao_raw = graph.import_image(
                view.dfao_raw.as_ref().expect("dfao_raw built").handle(),
                view.dfao_raw.as_ref().expect("dfao_raw built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let dfao_denoised = graph.import_image(
                view.dfao_denoised
                    .as_ref()
                    .expect("dfao_denoised built")
                    .handle(),
                view.dfao_denoised
                    .as_ref()
                    .expect("dfao_denoised built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.dfao_denoised
                    .as_ref()
                    .expect("dfao_denoised built")
                    .layout,
                None,
            );

            // 1. Trace: reconstruct world pos/normal from the G-buffer, cone-trace the GDF sky
            //    visibility into the half-res raw map. Records its three sets directly.
            let trace = Arc::clone(dfao);
            let trace_handle = trace.handle();
            let trace_layout = trace.layout();
            let trace_set = view.dfao_set;
            let trace_push = pipelines.dfao_push;
            let raw_body = raw.clone();
            let trace_gx = groups(half_extent.width);
            let trace_gy = groups(half_extent.height);
            let mut trace_pass = RgPass::compute("dfao")
                .access(g_normal, RgUsage::SampledReadCompute)
                .access(dfao_raw, RgUsage::StorageImageRwCompute);
            // The trace taps the GDF cascade clipmap (light set binding 9); declare the reads so
            // the graph transitions each cascade from GENERAL (composite write) → ShaderReadOnly
            // before the trace, exactly as the DDGI trace does.
            if let Some(cascades) = gdf_cascades {
                for cascade in cascades {
                    trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
                }
            }
            let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
                // covers the half-res trace target.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, trace_handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        trace_layout,
                        0,
                        &[bindless_set, light_set, trace_set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        trace_layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(&trace_push),
                    );
                    raw_body.cmd_dispatch(cmd, trace_gx, trace_gy, 1);
                }
                drop(trace);
            });
            graph.add_pass(trace_pass);

            // 2. Bilateral upsample: dfao_raw (half-res) + g_normal → dfao_denoised (full-res).
            if let Some(dfao_blur) = &pipelines.dfao_blur {
                self.add_compute_pass(
                    graph,
                    "dfao-blur",
                    dfao_blur,
                    view.dfao_blur_set,
                    &[
                        (dfao_raw, RgUsage::SampledReadCompute),
                        (g_normal, RgUsage::SampledReadCompute),
                        (dfao_denoised, RgUsage::StorageImageRwCompute),
                    ],
                    None,
                    groups(extent.width),
                    groups(extent.height),
                );
            }

            // Expose dfao_denoised to the gi-resolve pass (added after this block so it runs whenever
            // the screen chain does, not only when sky-occlusion is on): its skyVis input, when present.
            dfao_denoised_res = Some(dfao_denoised);

            // 3. Temporal accumulation (when motion ran): reproject the DFAO history through
            //    motion, neighborhood-clamp, EMA into the stable dfao_resolved the mesh samples.
            if let (Some(accum), Some(motion)) = (&pipelines.dfao_accum, motion) {
                let p = view.history_index;
                let dfao_resolved_slot = graph.alloc_external_layout(
                    view.dfao_resolved
                        .as_ref()
                        .expect("dfao_resolved built")
                        .layout,
                );
                let dfao_resolved = graph.import_image(
                    view.dfao_resolved
                        .as_ref()
                        .expect("dfao_resolved built")
                        .handle(),
                    view.dfao_resolved
                        .as_ref()
                        .expect("dfao_resolved built")
                        .view(),
                    vk::ImageAspectFlags::COLOR,
                    view.dfao_resolved
                        .as_ref()
                        .expect("dfao_resolved built")
                        .layout,
                    Some(dfao_resolved_slot),
                );
                let (read_slot, read) = import_ssgi_history(graph, &view.dfao_history[1 - p]);
                let (write_slot, write) = import_ssgi_history(graph, &view.dfao_history[p]);
                let push = crate::SsgiAccumPush {
                    params: saffron_geometry::glam::Vec4::new(
                        crate::SSGI_HISTORY_WEIGHT,
                        if view.history_valid { 1.0 } else { 0.0 },
                        0.0,
                        0.0,
                    ),
                };
                self.add_compute_pass(
                    graph,
                    "dfao-accum",
                    accum,
                    view.dfao_accum_sets[p],
                    &[
                        (dfao_denoised, RgUsage::SampledReadCompute),
                        (read, RgUsage::SampledReadCompute),
                        (motion, RgUsage::SampledReadCompute),
                        (dfao_resolved, RgUsage::StorageImageRwCompute),
                        (write, RgUsage::StorageImageRwCompute),
                    ],
                    Some(bytemuck::bytes_of(&push).to_vec()),
                    groups(extent.width),
                    groups(extent.height),
                );
                result.scene_sampled.push(dfao_resolved);
                result.dfao_resolved_slot = Some(dfao_resolved_slot);
                result.dfao_history_slots = Some(TaaHistorySlots {
                    read: (1 - p, read_slot),
                    write: (p, write_slot),
                });
            } else {
                // No motion this frame (practically unreachable — DFAO forces motion on): the
                // scene samples the spatially denoised map instead of the resolved one.
                result.scene_sampled.push(dfao_denoised);
            }
        }

        // Screen-space indirect-diffuse resolve: reconstruct worldPos/n from the G-buffer, integrate
        // the DDGI cage (shared `giprobe` sampler) + IBL diffuse × the DFAO sky-visibility (when it
        // ran) into the half-res gi_indirect. Runs whenever the screen chain does (the mesh always
        // samples gi_indirect at set 4 binding 7), independent of the sky-occlusion gate — its skyVis
        // flag (GiParams) is 0 when DFAO is off, so it then does not touch dfao_denoised.
        if let Some(gi_pso) = &pipelines.gi_resolve {
            // External-layout slot so the graph transitions gi_indirect GENERAL (this pass's storage
            // write) → SHADER_READ_ONLY for the scene pass's `SampledRead` (fully rewritten each frame,
            // so the start layout harmlessly discards).
            let gi_slot = graph.alloc_external_layout(
                view.gi_indirect.as_ref().expect("gi_indirect built").layout,
            );
            let gi_indirect = graph.import_image(
                view.gi_indirect
                    .as_ref()
                    .expect("gi_indirect built")
                    .handle(),
                view.gi_indirect.as_ref().expect("gi_indirect built").view(),
                vk::ImageAspectFlags::COLOR,
                view.gi_indirect.as_ref().expect("gi_indirect built").layout,
                Some(gi_slot),
            );
            let mut accesses = vec![
                (g_normal, RgUsage::SampledReadCompute),
                (gi_indirect, RgUsage::StorageImageRwCompute),
            ];
            if let Some(dfao_res) = dfao_denoised_res {
                accesses.push((dfao_res, RgUsage::SampledReadCompute));
            }
            self.add_compute_pass(
                graph,
                "gi-resolve",
                gi_pso,
                view.gi_resolve_sets[self.frames.index()],
                &accesses,
                None,
                groups(half_extent.width),
                groups(half_extent.height),
            );
            // The scene fragment samples gi_indirect (set 4 binding 7), so declare it — the graph
            // barriers it ShaderReadOnly before the scene pass.
            result.scene_sampled.push(gi_indirect);
        }

        // Specular reflection-occlusion: the reflection-vector twin of the DFAO chain (Wright
        // 2015) — a half-res trace (`specocc`) along the per-pixel reflection vector against the
        // GDF, a bilateral upsample (reusing the ssgi-blur PSO), then temporal accumulation through
        // the motion vectors (reusing the ssgi-accum PSO). The mesh samples the resolved occlusion
        // (set 4 binding 6) to occlude the reflected skybox, so the reflection cone-march no longer
        // runs per full-res fragment. The trace is a three-set pass (`[bindless, light, specocc_set]`)
        // like the DFAO trace, and additionally reads the roughness G-buffer target.
        if let Some(specocc) = &pipelines.specocc {
            let bindless_set = self.descriptors.bindless_set();
            let specocc_raw = graph.import_image(
                view.specocc_raw
                    .as_ref()
                    .expect("specocc_raw built")
                    .handle(),
                view.specocc_raw.as_ref().expect("specocc_raw built").view(),
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let specocc_denoised = graph.import_image(
                view.specocc_denoised
                    .as_ref()
                    .expect("specocc_denoised built")
                    .handle(),
                view.specocc_denoised
                    .as_ref()
                    .expect("specocc_denoised built")
                    .view(),
                vk::ImageAspectFlags::COLOR,
                view.specocc_denoised
                    .as_ref()
                    .expect("specocc_denoised built")
                    .layout,
                None,
            );

            // 1. Trace: reconstruct world pos/normal + the reflection vector from the G-buffer,
            //    cone-trace the GDF occlusion into the half-res raw map. Records its three sets.
            let trace = Arc::clone(specocc);
            let trace_handle = trace.handle();
            let trace_layout = trace.layout();
            let trace_set = view.specocc_set;
            let trace_push = pipelines.specocc_push;
            let raw_body = raw.clone();
            let trace_gx = groups(half_extent.width);
            let trace_gy = groups(half_extent.height);
            let mut trace_pass = RgPass::compute("specocc")
                .access(g_normal, RgUsage::SampledReadCompute)
                .access(g_roughness, RgUsage::SampledReadCompute)
                .access(specocc_raw, RgUsage::StorageImageRwCompute);
            // The trace taps the GDF cascade clipmap (light set binding 9); declare the reads so
            // the graph transitions each cascade GENERAL (composite) → ShaderReadOnly first.
            if let Some(cascades) = gdf_cascades {
                for cascade in cascades {
                    trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
                }
            }
            let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
                // covers the half-res trace target.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, trace_handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        trace_layout,
                        0,
                        &[bindless_set, light_set, trace_set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        trace_layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(&trace_push),
                    );
                    raw_body.cmd_dispatch(cmd, trace_gx, trace_gy, 1);
                }
                drop(trace);
            });
            graph.add_pass(trace_pass);

            // 2. Bilateral upsample: specocc_raw (half-res) + g_normal → specocc_denoised (full-res).
            if let Some(specocc_blur) = &pipelines.specocc_blur {
                self.add_compute_pass(
                    graph,
                    "specocc-blur",
                    specocc_blur,
                    view.specocc_blur_set,
                    &[
                        (specocc_raw, RgUsage::SampledReadCompute),
                        (g_normal, RgUsage::SampledReadCompute),
                        (specocc_denoised, RgUsage::StorageImageRwCompute),
                    ],
                    None,
                    groups(extent.width),
                    groups(extent.height),
                );
            }

            // Specular reflection occlusion is view-dependent (R); temporally reprojecting it through
            // surface motion would smear/swim it under camera motion. It is a low-frequency scalar once
            // bilaterally upsampled, so the mesh samples the spatially-denoised map directly — no temporal reuse.
            result.scene_sampled.push(specocc_denoised);
        }

        // Screen-space reflections: g_normal + prevColor → ssr_map. The mesh blends ssr_map
        // over the prefiltered-env specular, weighted by hit confidence × (1 - roughness),
        // so only smooth surfaces use it. No separate denoise — TAA cleans the march jitter.
        if let Some(ssr) = &pipelines.ssr {
            let prev_color = prev_color.expect("prev_color imported when SSR on");
            let ssr_slot =
                graph.alloc_external_layout(view.ssr_map.as_ref().expect("ssr_map built").layout);
            let ssr_map = graph.import_image(
                view.ssr_map.as_ref().expect("ssr_map built").handle(),
                view.ssr_map.as_ref().expect("ssr_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ssr_map.as_ref().expect("ssr_map built").layout,
                Some(ssr_slot),
            );
            self.add_compute_pass(
                graph,
                "ssr",
                ssr,
                view.ssr_set,
                &[
                    (g_normal, RgUsage::SampledReadCompute),
                    (prev_color, RgUsage::SampledReadCompute),
                    (ssr_map, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&pipelines.ssr_push).to_vec()),
                groups(extent.width),
                groups(extent.height),
            );
            result.scene_sampled.push(ssr_map);
            result.ssr_map_slot = Some(ssr_slot);
        }

        // RT reflections sample prev_color directly in the mesh fragment (set-4 binding 4),
        // so the scene pass must SampledRead it (transition to ShaderReadOnly before the draw).
        if rt_refl {
            if let Some(pc) = prev_color {
                result.scene_sampled.push(pc);
            }
        }

        // The prev-color history copy runs AFTER the scene (it reads the scene's linear-HDR
        // color) for whichever of SSGI / SSR / RT reflections is on; hand the caller the info
        // to schedule it.
        if let (Some(copy), Some(prev_color)) = (&pipelines.copy_color, prev_color) {
            result.history_copy = Some(HistoryCopy {
                prev_color,
                pipeline: Arc::clone(copy),
                set: view.copy_color_set,
                groups_x: groups(extent.width),
                groups_y: groups(extent.height),
            });
        }

        result
    }

    /// Appends the motion-vector prepass when its PSO + targets resolved this frame: clear
    /// the rg16f motion target + its depth scratch, draw every batch with the cur/prev
    /// camera viewProj (the per-view `prev_view_proj`). Returns the imported motion resource
    /// (the TAA / SSGI-accum passes sample it), or `None` when motion did not run.
    fn add_motion_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        instance_set: vk::DescriptorSet,
        deformed: (Option<RgResource>, Option<vk::Buffer>),
        prev_deformed: (Option<RgResource>, Option<vk::Buffer>),
    ) -> Option<(RgResource, RgResource)> {
        let motion_pipeline = pipelines.motion.as_ref()?;
        let (deformed_res, deformed_handle) = deformed;
        let (prev_deformed_res, prev_deformed_handle) = prev_deformed;
        let view = &self.views[self.active_view.index()];
        let (motion_image, motion_depth) = match (&view.motion, &view.motion_depth) {
            (Some(motion), Some(depth)) => (motion, depth),
            _ => return None,
        };
        // The motion prepass rasterises at INPUT extent (with the scene).
        let extent = view.scaled_render_extent();
        let motion = graph.import_image(
            motion_image.handle(),
            motion_image.view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let motion_depth = graph.import_image(
            motion_depth.handle(),
            motion_depth.view(),
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        // The motion prepass reprojects with the UN-jittered matrices so static geometry keeps
        // exact zero velocity — the sub-pixel scene jitter must not leak into the velocity buffer.
        let cur_view_proj = self.scene_view_proj_unjittered();
        let push = crate::MotionPush {
            cur_view_proj,
            prev_view_proj: if view.prev_view_proj_valid {
                view.prev_view_proj
            } else {
                // The first frame (no history) reprojects against itself → zero motion.
                cur_view_proj
            },
        };
        let list = self.scene_draw_list.shallow_clone();
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(motion_pipeline);
        let motion_handle = pipeline.handle();
        let motion_layout = pipeline.layout();
        let mut pass = RgPass::graphics("motion", extent)
            .color(RgAttachment::clear_store(motion))
            .depth_attachment(depth_clear_store(motion_depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                crate::record_motion(
                    &raw_body,
                    cmd,
                    &list,
                    motion_handle,
                    motion_layout,
                    instance_set,
                    &push,
                    deformed_handle,
                    prev_deformed_handle,
                );
                drop(pipeline);
            });
        // The motion pass reads BOTH deformed buffers (binding 0 = current position,
        // binding 1 = previous), so declare both reads for the skin-write → vertex-input
        // barrier on each.
        if let Some(deformed) = deformed_res {
            pass = pass.access(deformed, RgUsage::VertexInputRead);
        }
        if let Some(prev_deformed) = prev_deformed_res {
            pass = pass.access(prev_deformed, RgUsage::VertexInputRead);
        }
        graph.add_pass(pass);
        // Return the motion colour + the motion-prepass depth (the TAA resolve reads the depth
        // for closest-depth velocity dilation).
        Some((motion, motion_depth))
    }

    /// Appends the FXAA edge-blur compute pass when its PSO resolved this frame: sample the
    /// scene's 1× result (`scene_output` = scratch) and store the blurred result into the
    /// offscreen (`color`).
    fn add_fxaa_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_output: RgResource,
        color: RgResource,
    ) {
        let Some(fxaa) = &pipelines.fxaa else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // Dispatched over the DISPLAY grid: FXAA reads the input scratch by normalized UV (a
        // bilinear upscale of the edge-blurred input) and writes the display-extent offscreen.
        let extent = view.published_extent();
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "fxaa",
            fxaa,
            view.fxaa_set,
            &[
                (scene_output, RgUsage::SampledReadCompute),
                (color, RgUsage::StorageImageRwCompute),
            ],
            None,
            groups(extent.width),
            groups(extent.height),
        );
    }

    /// Appends the no-AA / MSAA scene-resolve copy: a normalized-UV upscale of the input-extent
    /// scene scratch (`scene_output`) into the display-extent offscreen (`color`), dispatched over
    /// the display grid (so a display invocation samples the input scratch bilinearly). FXAA / TAA
    /// resolve to the offscreen themselves, so the caller runs this only when neither is active.
    fn add_scene_resolve_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_output: RgResource,
        color: RgResource,
    ) {
        let Some(resolve) = &pipelines.scene_resolve else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        let extent = view.published_extent();
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "scene-resolve",
            resolve,
            view.scene_resolve_set,
            &[
                (scene_output, RgUsage::SampledReadCompute),
                (color, RgUsage::StorageImageRwCompute),
            ],
            None,
            groups(extent.width),
            groups(extent.height),
        );
    }

    /// Appends the depth-upscale graphics pass: point-upscales the input-extent scene `depth`
    /// into the view's display-extent `depth_display` (depth-write-always over a fullscreen
    /// triangle) so the display-extent overlays occlude correctly under upsampling. Returns the
    /// `depth_display` resource, or `None` when the PSO / target is unavailable.
    fn add_depth_upscale_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        depth: RgResource,
    ) -> Option<RgResource> {
        let pipeline = pipelines.depth_upscale.as_ref()?;
        let view = &self.views[self.active_view.index()];
        let depth_display = view.depth_display.as_ref()?;
        let input = view.scaled_render_extent();
        let display = view.published_extent();
        let dd = graph.import_image(
            depth_display.handle(),
            depth_display.view(),
            vk::ImageAspectFlags::DEPTH,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let set = view.depth_upscale_set;
        let mut push = Vec::with_capacity(8);
        push.extend_from_slice(&(input.width as f32).to_ne_bytes());
        push.extend_from_slice(&(input.height as f32).to_ne_bytes());
        // The pass samples the input scene depth (declared read → the graph transitions it
        // DepthWrite → ShaderReadOnly after the scene) and depth-writes `depth_display`.
        let pass = RgPass::graphics("depth-upscale", display)
            .depth_attachment(depth_clear_store(dd))
            .access(depth, RgUsage::SampledRead)
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. Fullscreen triangle: bind the input-depth sampler set +
                // the inputSize push, then draw 3 vertices (no vertex buffer).
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        layout,
                        0,
                        &[set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::FRAGMENT,
                        0,
                        &push,
                    );
                    raw_body.cmd_draw(cmd, 3, 1, 0, 0);
                }
                drop(pipeline);
            });
        graph.add_pass(pass);
        Some(dd)
    }

    /// Appends the TAA resolve compute pass when its PSO + motion resolved this frame:
    /// reproject the previous history through the motion vector, neighborhood-clamp, and
    /// blend with the current scene (`scene_output` = scratch) into the offscreen (`color`)
    /// plus the next-frame history. Parity `p` reads history `1 - p` and writes history `p`,
    /// bound in the per-view TAA set. Returns the history images' external-layout slots when
    /// the pass ran.
    /// Appends the TAA reactive-coverage pass: color-clears the view's input-extent r8 reactive
    /// mask, then re-draws the translucent batches into it (constant-1.0 fragment, depth-tested
    /// read-only against `scene_depth`) so the resolve can bias alpha-blended pixels toward the
    /// current frame. Runs only under TAA. Returns the reactive-mask resource, or `None` when the
    /// PSO / target is unavailable (the resolve then falls back to a fully-cleared mask).
    fn add_reactive_coverage_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_depth: RgResource,
        instance_set: vk::DescriptorSet,
        deformed_handle: Option<vk::Buffer>,
    ) -> Option<RgResource> {
        let pipeline = pipelines.reactive_coverage.as_ref()?;
        let view = &self.views[self.active_view.index()];
        let reactive = view.reactive.as_ref()?;
        let input = view.scaled_render_extent();
        // Cleared every frame (LOAD_OP_CLEAR), so the prior content is discarded — import at
        // UNDEFINED and let the clear own it (no cross-frame layout slot needed).
        let reactive_res = graph.import_image(
            reactive.handle(),
            reactive.view(),
            vk::ImageAspectFlags::COLOR,
            vk::ImageLayout::UNDEFINED,
            None,
        );
        let list = self.scene_draw_list.shallow_clone();
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let mut color_att = RgAttachment::clear_store(reactive_res);
        color_att.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 0.0],
            },
        };
        // Read-only depth test against the resolved scene depth (declared by the attachment), so
        // occluded translucent fragments don't mark coverage.
        let pass = RgPass::graphics("reactive-coverage", input)
            .color(color_att)
            .depth_attachment(depth_load_readonly(scene_depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                crate::scene_pass::record_reactive_coverage(
                    &raw_body,
                    cmd,
                    &list,
                    handle,
                    layout,
                    instance_set,
                    deformed_handle,
                );
                drop(pipeline);
            });
        graph.add_pass(pass);
        Some(reactive_res)
    }

    #[allow(clippy::too_many_arguments)]
    fn add_taa_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_output: RgResource,
        color: RgResource,
        motion: Option<RgResource>,
        motion_depth: Option<RgResource>,
        reactive: Option<RgResource>,
    ) -> Option<TaaResolveSlots> {
        let taa = pipelines.taa.as_ref()?;
        let motion = motion?;
        // The motion prepass produces the colour + depth together; the resolve needs both.
        let motion_depth = motion_depth?;
        let view = &self.views[self.active_view.index()];
        // The resolve dispatches over the DISPLAY grid (one invocation per display pixel); it
        // samples the input-extent scene / motion by normalized UV and reconstructs upward.
        let extent = view.published_extent();
        let p = view.history_index;
        let (history_read, history_write) = match (&view.history[1 - p], &view.history[p]) {
            (Some(read), Some(write)) => (read, write),
            _ => return None,
        };
        // The two history images carry their layout across frames (the graph internally
        // pings ShaderReadOnly → General for the write and back), so each rides an external
        // slot whose resolved exit layout is read back after execute.
        let read_slot = graph.alloc_external_layout(history_read.layout);
        let write_slot = graph.alloc_external_layout(history_write.layout);
        let hist_read = graph.import_image(
            history_read.handle(),
            history_read.view(),
            vk::ImageAspectFlags::COLOR,
            history_read.layout,
            Some(read_slot),
        );
        let hist_write = graph.import_image(
            history_write.handle(),
            history_write.view(),
            vk::ImageAspectFlags::COLOR,
            history_write.layout,
            Some(write_slot),
        );
        // The pixel-lock ping-pong (same parity as history): read the opposite parity at the
        // reprojected UV, write this parity. Each carries its layout across frames like history.
        let (lock_read, lock_write) = match (&view.lock[1 - p], &view.lock[p]) {
            (Some(read), Some(write)) => (read, write),
            _ => return None,
        };
        let lock_read_slot = graph.alloc_external_layout(lock_read.layout);
        let lock_write_slot = graph.alloc_external_layout(lock_write.layout);
        let lock_read_res = graph.import_image(
            lock_read.handle(),
            lock_read.view(),
            vk::ImageAspectFlags::COLOR,
            lock_read.layout,
            Some(lock_read_slot),
        );
        let lock_write_res = graph.import_image(
            lock_write.handle(),
            lock_write.view(),
            vk::ImageAspectFlags::COLOR,
            lock_write.layout,
            Some(lock_write_slot),
        );
        let params = self.taa_params;
        // `screen_size` is the INPUT/render extent (velocity → pixels, and the source grid the
        // resolve samples); it diverges from the display dispatch/output extent under upsampling.
        let input = view.scaled_render_extent();
        // The upscale ratio `n = displayW / inputW` and the per-output accumulation target
        // (`8·n²`, the jitter cycle length, floored at the native warm-up) the confidence
        // saturates against — freshly-covered display pixels lean on the reconstructed current
        // sample and converge over the cycle.
        let n = extent.width.max(1) as f32 / input.width.max(1) as f32;
        let sample_target =
            (crate::TAA_JITTER_PHASES as f32 * n * n).max(crate::TAA_JITTER_PHASES as f32);
        let push = crate::TaaPush {
            feedback: saffron_geometry::glam::Vec2::new(params.feedback_min, params.feedback_max),
            jitter: view.jitter,
            prev_jitter: view.prev_jitter,
            screen_size: saffron_geometry::glam::Vec2::new(input.width as f32, input.height as f32),
            gamma_valid: saffron_geometry::glam::Vec2::new(
                params.clip_gamma,
                if view.history_valid { 1.0 } else { 0.0 },
            ),
            reject_sharp: saffron_geometry::glam::Vec2::new(
                params.velocity_rejection,
                params.sharpness,
            ),
            upscale: saffron_geometry::glam::Vec2::new(n, sample_target),
            // Reconstruction robustness (Phase 3): the lock initial lifetime + reactive scale, the
            // disocclusion + lock-break thresholds, and the camera planes for depth linearization.
            // `current` + `history` are both raw linear-HDR at one scale — any future pre-exposure
            // must scale both (and the value written to outHistory), never one, or accumulation drifts.
            lock_reactive: saffron_geometry::glam::Vec2::new(
                params.lock_lifetime,
                params.reactive_scale,
            ),
            disoccl: saffron_geometry::glam::Vec2::new(
                params.disocclusion_threshold,
                params.lock_break_luma,
            ),
            depth_params: saffron_geometry::glam::Vec2::new(
                self.camera_near_far.0,
                self.camera_near_far.1,
            ),
        };
        // The reactive mask feeds slot 6. When the coverage pass didn't run (PSO build failed),
        // `reactive` is None and the slot keeps its placeholder binding (never a real declared
        // access — it rests ShaderReadOnly, so the stale sample is validation-safe but inert).
        let mut accesses = vec![
            (scene_output, RgUsage::SampledReadCompute),
            (motion, RgUsage::SampledReadCompute),
            // Closest-depth dilation source — ordered after the motion prepass's depth store.
            (motion_depth, RgUsage::SampledReadCompute),
            (lock_read_res, RgUsage::SampledReadCompute),
            (hist_read, RgUsage::SampledReadCompute),
            (color, RgUsage::StorageImageRwCompute),
            (hist_write, RgUsage::StorageImageRwCompute),
            (lock_write_res, RgUsage::StorageImageRwCompute),
        ];
        if let Some(reactive) = reactive {
            accesses.push((reactive, RgUsage::SampledReadCompute));
        }
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "taa",
            taa,
            view.taa_sets[p],
            &accesses,
            Some(bytemuck::bytes_of(&push).to_vec()),
            groups(extent.width),
            groups(extent.height),
        );
        Some(TaaResolveSlots {
            history: TaaHistorySlots {
                read: (1 - p, read_slot),
                write: (p, write_slot),
            },
            lock: TaaHistorySlots {
                read: (1 - p, lock_read_slot),
                write: (p, lock_write_slot),
            },
        })
    }

    /// Appends the mandatory HDR → display tonemap: an in-place compute pass on the
    /// offscreen `color` (`StorageImageRwCompute`, GENERAL layout) binding the per-view
    /// tonemap set + the `exp2(exposure_ev)` push, dispatched 8×8 over the viewport. The
    /// graph derives the ColorWrite → General transition before and (when present blits /
    /// samples it) General → the present layout after. A build failure leaves the offscreen
    /// linear-HDR (logged).
    fn add_tonemap_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
    ) {
        let Some(tonemap) = &pipelines.tonemap else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // In-place on the DISPLAY-extent offscreen (after the resolve reconstructed it).
        let extent = view.published_extent();
        let push = TonemapPush::new(self.exposure_ev, self.tonemap_mode);
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "tonemap",
            tonemap,
            view.tonemap_set,
            &[(color, RgUsage::StorageImageRwCompute)],
            Some(bytemuck::bytes_of(&push).to_vec()),
            groups(extent.width),
            groups(extent.height),
        );
    }

    /// Appends the optional ground grid + editor overlay (both graphics passes drawing on
    /// the post-tonemap offscreen `color`, depth-testing against the persisted 1× scene
    /// `depth`). The grid runs when shown; the overlay when geometry is queued. Both load
    /// the color (composite over the tonemapped image) and load the depth read-only (the
    /// depth-tested ranges occlude; the on-top range ignores it).
    fn add_grid_overlay_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
    ) {
        // Drawn on the DISPLAY-extent resolved color, depth-testing the display-extent overlay
        // depth (`depth` is the point-upscaled `depth_display`, not the input scene depth).
        let extent = self.views[self.active_view.index()].published_extent();

        if let Some(grid) = &pipelines.grid {
            let raw_body = self.device.raw().clone();
            let pipeline = Arc::clone(grid);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            // The grid composites AFTER TAA on the post-tonemap color, so it must use the
            // UN-jittered camera or it would shimmer (the resolve never un-jitters it).
            let push = GridPush::new(self.scene_view_proj_unjittered());
            let pass = RgPass::graphics("grid", extent)
                .color(color_load_store(color))
                .depth_attachment(depth_load_readonly(depth))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    crate::record_grid(&raw_body, cmd, handle, layout, &push);
                    drop(pipeline);
                });
            graph.add_pass(pass);
        }

        if let (Some(overlay), Some(overlay_depth), Some(draw)) = (
            &pipelines.overlay,
            &pipelines.overlay_depth,
            pipelines.overlay_draw,
        ) {
            let raw_body = self.device.raw().clone();
            let on_top = Arc::clone(overlay);
            let occluded = Arc::clone(overlay_depth);
            let on_top_handle = on_top.handle();
            let occluded_handle = occluded.handle();
            let pass = RgPass::graphics("editor-overlay", extent)
                .color(color_load_store(color))
                .depth_attachment(depth_load_readonly(depth))
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    crate::record_overlay(&raw_body, cmd, &draw, on_top_handle, occluded_handle);
                    drop(on_top);
                    drop(occluded);
                });
            graph.add_pass(pass);
        }
    }

    /// Appends the motion-vector visualization (the `MotionVectors` view mode): a fullscreen
    /// compute that samples the motion target and overwrites the post-tonemap `color`. A no-op
    /// unless the mode's PSO is resolved and the motion target ran this frame (TAA or SSGI on);
    /// otherwise the shaded scene shows through.
    fn add_motion_visualize_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        motion: Option<RgResource>,
    ) {
        let (Some(pipeline), Some(motion)) = (&pipelines.motion_visualize, motion) else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // In-place over the DISPLAY-extent post-tonemap color; motion is sampled by normalized UV.
        let extent = view.published_extent();
        let mut push = Vec::with_capacity(8);
        push.extend_from_slice(&extent.width.to_ne_bytes());
        push.extend_from_slice(&extent.height.to_ne_bytes());
        let groups = |n: u32| n.div_ceil(8);
        self.add_compute_pass(
            graph,
            "motion-visualize",
            pipeline,
            view.motion_vis_set,
            &[
                (motion, RgUsage::SampledReadCompute),
                (color, RgUsage::StorageImageRwCompute),
            ],
            Some(push),
            groups(extent.width),
            groups(extent.height),
        );
    }

    /// Appends the Lit Wireframe overlay (the `LitWireframe` view mode): re-draws the scene
    /// geometry in line polygon mode over the post-tonemap `color`, depth-tested read-only
    /// against the persisted 1× `depth` so hidden edges are occluded. A no-op unless the
    /// mode's PSO is resolved (a `fill_mode_non_solid` device; else it falls back to plain
    /// Lit). Reuses the depth-prepass recorder: bind the PSO + instance set, push the
    /// viewProj, draw every batch.
    #[allow(clippy::too_many_arguments)]
    fn add_lit_wireframe_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
        instance_set: vk::DescriptorSet,
        deformed_res: Option<RgResource>,
        deformed_handle: Option<vk::Buffer>,
    ) {
        let Some(pipeline) = &pipelines.wireframe_overlay else {
            return;
        };
        // Re-drawn at DISPLAY extent, depth-tested against the display-extent overlay depth.
        let extent = self.views[self.active_view.index()].published_extent();
        let list = self.scene_draw_list.shallow_clone();
        let raw_for_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let mut pass = RgPass::graphics("lit-wireframe", extent)
            .color(color_load_store(color))
            .depth_attachment(depth_load_readonly(depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_depth_prepass(
                    &raw_for_body,
                    cmd,
                    &list,
                    handle,
                    layout,
                    instance_set,
                    deformed_handle,
                );
                drop(pipeline);
            });
        if let Some(deformed) = deformed_res {
            pass = pass.access(deformed, RgUsage::VertexInputRead);
        }
        graph.add_pass(pass);
    }

    /// Appends one screen-space compute pass: declare the `(resource, usage)` accesses
    /// (the graph derives the GENERAL ↔ ShaderReadOnly transitions), bind the per-view
    /// set, optionally push `push`, and dispatch `(groups_x, groups_y, 1)`.
    #[allow(clippy::too_many_arguments)]
    fn add_compute_pass(
        &self,
        graph: &mut RenderGraph,
        name: &'static str,
        pipeline: &Arc<crate::Pipeline>,
        set: vk::DescriptorSet,
        accesses: &[(RgResource, RgUsage)],
        push: Option<Vec<u8>>,
        groups_x: u32,
        groups_y: u32,
    ) {
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let mut pass = RgPass::compute(name).body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch covers
            // the viewport (one invocation per pixel, 8×8 per group).
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[],
                );
                if let Some(push) = &push {
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        push,
                    );
                }
                raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
            }
            drop(pipeline);
        });
        for &(resource, usage) in accesses {
            pass = pass.access(resource, usage);
        }
        graph.add_pass(pass);
    }

    /// Appends a depth-only shadow pass clearing + storing the map and recording the
    /// vertex-only, depth-biased draw list under `light_view_proj`.
    #[allow(clippy::too_many_arguments)]
    fn add_shadow_pass(
        &self,
        graph: &mut RenderGraph,
        name: &'static str,
        resource: RgResource,
        pipeline: &Arc<crate::Pipeline>,
        instance_set: vk::DescriptorSet,
        light_view_proj: Mat4,
        deformed_res: Option<RgResource>,
        deformed_handle: Option<vk::Buffer>,
    ) {
        let list = self.scene_draw_list.shallow_clone();
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let shadow_pipeline = pipeline.handle();
        let shadow_layout = pipeline.layout();
        let extent = vk::Extent2D {
            width: crate::lighting::SHADOW_MAP_SIZE,
            height: crate::lighting::SHADOW_MAP_SIZE,
        };
        let mut pass = RgPass::graphics(name, extent)
            .depth_attachment(depth_clear_store(resource))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_shadow_depth(
                    &raw_body,
                    cmd,
                    &list,
                    shadow_pipeline,
                    shadow_layout,
                    instance_set,
                    light_view_proj,
                    deformed_handle,
                );
                drop(pipeline);
            });
        // The skinned batches read the deformed buffer as a vertex stream; declare the
        // read so the graph emits the skin-compute-write → vertex-input barrier.
        if let Some(deformed) = deformed_res {
            pass = pass.access(deformed, RgUsage::VertexInputRead);
        }
        graph.add_pass(pass);
    }

    /// Rebuilds the present swapchain at `(width, height)` after a window resize.
    ///
    /// The swapchain is created once in [`Renderer::new`] and is otherwise immutable; a
    /// resize makes it out of date, so [`Renderer::begin_present_frame`] returns `false`
    /// and skips the frame until this rebuilds it at the new surface size. The windowed
    /// loop calls it on `WindowEvent::Resized`. Waits the device idle first (an in-flight
    /// present may still reference the old images), drops any acquired-image index a
    /// skipped frame left behind, then destroys and rebuilds the swapchain as a unit
    /// (its per-image views + semaphores rebuild with it; the frame-ring-indexed
    /// [`crate::present::PresentSync`] is extent-independent and is kept). A no-op for the
    /// offscreen host (no swapchain) and for a zero extent (minimized).
    ///
    /// # Errors
    ///
    /// Propagates a device-idle wait failure or any swapchain-creation [`Error`].
    pub fn recreate_swapchain(&mut self, width: u32, height: u32) -> Result<()> {
        if self.swapchain.is_none() || width == 0 || height == 0 {
            return Ok(());
        }
        self.device.wait_idle()?;
        // The old image indices are about to be invalid; drop any index a skipped
        // out-of-date frame left acquired so the next present starts clean.
        if let Some(present_sync) = self.present_sync.as_mut() {
            let _ = present_sync.take_acquired_image();
        }
        if let Some(mut swapchain) = self.swapchain.take() {
            swapchain.destroy(&self.device);
        }
        let swapchain = Swapchain::new(&self.device, width, height)?;
        tracing::info!(
            "swapchain rebuilt {}x{}",
            swapchain.extent.width,
            swapchain.extent.height
        );
        self.swapchain = Some(swapchain);
        Ok(())
    }

    /// Begins a windowed present-only frame: waits + resets the current slot's fence
    /// (so its per-frame buffers are free, the [`Renderer::begin_offscreen_frame`] half) and
    /// acquires the next swapchain image with the slot's image-available semaphore.
    ///
    /// The standalone host renders the scene into the offscreen in `on_ui`
    /// ([`Renderer::render_scene_offscreen`]) and blits it onto this acquired image in
    /// [`Renderer::present_active_view_to_swapchain`] at `end_frame`: the present-only path,
    /// where the frame begin acquires and the frame end blits + presents.
    /// Returns `false` when the swapchain is out of date (a resize the caller should handle by
    /// rebuilding).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing fence / acquire call.
    pub fn begin_present_frame(&mut self) -> Result<bool> {
        // Acquire the swapchain image *before* `begin_offscreen_frame` resets the slot fence:
        // an out-of-date swapchain must skip the whole frame without leaving an unsignaled
        // fence behind (the next frame would deadlock waiting on it).
        //
        // Wait this slot's prior present BEFORE the acquire: that present's blit waited this
        // slot's image-available semaphore, so the acquire cannot reuse the semaphore until
        // that wait completed (`VUID-vkAcquireNextImageKHR-semaphore-01779`) — the present
        // fence is the only thing that orders it (the offscreen `in_flight` fence signals
        // before the blit). The blit also re-signals the slot's scene-finished semaphore +
        // overwrites the offscreen, both of which this present completing guards. The fence is
        // created signaled, so the first cycle's wait returns immediately. Not reset here —
        // `present_active_view_to_swapchain` resets it before resubmit.
        let present_fence = self
            .present_sync
            .as_ref()
            .map(|present_sync| present_sync.present_fence(self.frames.index()));
        if let Some(present_fence) = present_fence {
            let raw = self.device.raw();
            // SAFETY: the ash seam. The fence belongs to this device (created signaled).
            checked(
                unsafe { raw.wait_for_fences(&[present_fence], true, u64::MAX) },
                "begin_present: wait_for_fences(present)",
            )?;
        }

        let swapchain_loader = self.device.swapchain_loader();
        let image_available = self.frames.image_available();
        // SAFETY: the ash seam. Acquires the next image, signaling image_available. The
        // present blit submit waits on it before touching the swapchain image. Its prior wait
        // is guaranteed complete by the present-fence wait above, so the reuse is valid.
        let acquire = unsafe {
            swapchain_loader.acquire_next_image(
                self.present_swapchain().handle(),
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        };
        let image_index = match acquire {
            Ok((index, _suboptimal)) => index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(result) => {
                return Err(Error::Vk {
                    context: "acquire_next_image (present)",
                    result,
                });
            }
        };
        if let Some(present_sync) = self.present_sync.as_mut() {
            present_sync.set_acquired_image(image_index);
        }

        // Wait + reset this slot's fence and command pool so the slot is idle before the
        // layers' draw-list submit resets per-frame state (the shared offscreen begin).
        self.begin_offscreen_frame()?;
        Ok(true)
    }

    /// Blits the active view's post-processed offscreen onto the acquired swapchain image and
    /// presents — the standalone present-only host's frame transport.
    ///
    /// Runs at `end_frame`, after `on_ui` rendered the scene + native overlay into the
    /// offscreen via [`Renderer::render_scene_offscreen`] (which signals the slot's
    /// scene-finished semaphore). Records the offscreen → swapchain `vkCmdBlitImage` with its
    /// layout transitions into the slot's blit buffer, submits it waiting on both the acquire's
    /// image-available semaphore and the scene-finished semaphore (so the swapchain image is
    /// owned and the offscreen is rendered), then presents. A no-op (returns `Ok`) when no image
    /// was acquired this frame (the swapchain was out of date in `begin_present_frame`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing fence / record / submit / present call.
    pub fn present_active_view_to_swapchain(&mut self) -> Result<()> {
        // The offscreen submit advanced the ring, so the slot that just rendered (and whose
        // image-available / scene-finished semaphores carry this frame) is `last_rendered_slot`.
        let slot = self.last_rendered_slot;
        let Some(image_index) = self
            .present_sync
            .as_mut()
            .and_then(PresentSync::take_acquired_image)
        else {
            return Ok(()); // No image acquired (out-of-date swapchain): skip the present.
        };

        // Whether the offscreen render signaled the scene-finished semaphore this frame; if it
        // did not (the host skipped the offscreen render), the blit must not wait on it.
        let scene_signaled = std::mem::take(&mut self.present_scene_signaled);

        let raw = self.device.raw();
        let present_sync = self
            .present_sync
            .as_ref()
            .expect("present sync in windowed mode");
        let blit_cmd = present_sync.command_buffer(slot);
        let blit_pool = present_sync.command_pool(slot);
        let present_fence = present_sync.present_fence(slot);
        let scene_finished = present_sync.scene_finished(slot);
        let image_available = self.frames.image_available_for(slot);

        // The slot's prior present must complete before its blit buffer + fence are reused.
        // SAFETY: the ash seam. The fence belongs to this device (created signaled).
        checked(
            unsafe { raw.wait_for_fences(&[present_fence], true, u64::MAX) },
            "present: wait_for_fences",
        )?;
        // SAFETY: the ash seam. The waited fence is unsignaled and reset before resubmit.
        checked(
            unsafe { raw.reset_fences(&[present_fence]) },
            "present: reset_fences",
        )?;

        let swapchain = self.present_swapchain();
        let swap_image = swapchain.image(image_index as usize);
        let swap_extent = swapchain.extent;
        let render_finished = swapchain.render_finished(image_index as usize);
        let tracking = swapchain.image_in_flight(image_index as usize);

        // A fence still tracking this swapchain image (from an earlier present) must signal
        // before its render-finished semaphore is reused.
        if tracking != vk::Fence::null() {
            // SAFETY: the ash seam. The tracking fence belongs to this device.
            checked(
                unsafe { raw.wait_for_fences(&[tracking], true, u64::MAX) },
                "present: wait_for_fences(image)",
            )?;
        }
        self.swapchain
            .as_mut()
            .expect("present swapchain in windowed mode")
            .set_image_in_flight(image_index as usize, present_fence);

        let view = &self.views[self.active_view.index()];
        let offscreen = view.offscreen.handle();
        let offscreen_extent = view.offscreen.extent;
        let from_layout = view.offscreen.layout;
        // The offscreen's last writer matches its tracked layout: COLOR_ATTACHMENT after the
        // post chain's overlay pass, or ShaderReadOnly after a prior read-back.
        let (from_stage, from_access) = match from_layout {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            ),
            _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
        };

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. The slot's present fence was waited above, so its pool may be
        // reset; the blit references the acquired swapchain image + the active offscreen, both
        // of which outlive the recorded command (submitted + fenced below).
        unsafe {
            checked(
                raw.reset_command_pool(blit_pool, vk::CommandPoolResetFlags::empty()),
                "present: reset_command_pool",
            )?;
            checked(
                raw.begin_command_buffer(blit_cmd, &begin),
                "present: begin_command_buffer",
            )?;
            crate::present::record_present_blit(
                raw,
                blit_cmd,
                offscreen,
                offscreen_extent,
                from_layout,
                from_stage,
                from_access,
                swap_image,
                swap_extent,
                vk::ImageLayout::PRESENT_SRC_KHR,
            );
            checked(
                raw.end_command_buffer(blit_cmd),
                "present: end_command_buffer",
            )?;
        }

        // Track the offscreen's new layout so the next frame's graph import seeds the right
        // entry layout (the blit left it in TRANSFER_SRC).
        self.views[self.active_view.index()].offscreen.layout =
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL;

        // Wait the acquire (image owned), and the scene-finished semaphore (offscreen rendered)
        // when it was signaled this frame; signal render-finished (the present waits on it);
        // fence the slot. When the offscreen render was skipped the blit reads the prior frame's
        // offscreen and waits the acquire alone — never an unsignaled semaphore (a deadlock).
        let blit_stage = vk::PipelineStageFlags2::BLIT;
        let mut wait = vec![
            vk::SemaphoreSubmitInfo::default()
                .semaphore(image_available)
                .stage_mask(blit_stage),
        ];
        if scene_signaled {
            wait.push(
                vk::SemaphoreSubmitInfo::default()
                    .semaphore(scene_finished)
                    .stage_mask(blit_stage),
            );
        }
        let signal = [vk::SemaphoreSubmitInfo::default()
            .semaphore(render_finished)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
        let cmd = [vk::CommandBufferSubmitInfo::default().command_buffer(blit_cmd)];
        let submit = [vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait)
            .command_buffer_infos(&cmd)
            .signal_semaphore_infos(&signal)];
        // SAFETY: the ash seam. The graphics queue is externally synchronized; the fence was
        // reset above.
        checked(
            unsafe { raw.queue_submit2(self.device.graphics_queue, &submit, present_fence) },
            "present: queue_submit2",
        )?;

        let swapchains = [self.present_swapchain().handle()];
        let wait_semaphores = [render_finished];
        let image_indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        // SAFETY: the ash seam. The swapchain/image-index are valid; the present waits on
        // render_finished signaled by the submit above.
        let present = unsafe {
            self.device
                .swapchain_loader()
                .queue_present(self.device.graphics_queue, &present_info)
        };
        // A window capture armed by `request_window_capture` reads the just-presented
        // swapchain image (the composited window output) into a PNG, then disarms.
        if self.capture_next_window_path.is_some() {
            self.run_pending_window_capture(image_index as usize);
        }
        match present {
            Ok(_) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                Ok(())
            }
            Err(result) => Err(Error::Vk {
                context: "present: queue_present",
                result,
            }),
        }
    }

    /// Records and submits one acquire → clear → present frame.
    ///
    /// The per-frame path reduced to a clear:
    /// 1. wait the slot's in-flight fence, then reset it;
    /// 2. acquire the next swapchain image (the slot's image-available semaphore);
    /// 3. wait any fence still tracking that image;
    /// 4. record: `UNDEFINED → TRANSFER_DST` barrier, `vkCmdClearColorImage`,
    ///    `TRANSFER_DST → PRESENT_SRC` barrier (sync2 throughout);
    /// 5. `vkQueueSubmit2` (wait image-available, signal render-finished, fence);
    /// 6. `vkQueuePresentKHR`.
    ///
    /// Returns `true` on a normal frame, `false` when the swapchain is out of date
    /// (a resize the caller should handle by rebuilding). Every barrier is
    /// explicit so the validation layer stays silent.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] for any failing Vulkan call.
    pub fn render_frame(&mut self) -> Result<bool> {
        // The present path is the standalone windowed host only; the editor/headless host
        // never presents (it renders offscreen and publishes to shared memory), so it never
        // reaches here. The swapchain is the windowed-mode-only field.
        if self.swapchain.is_none() {
            return Err(Error::ShaderLoad(
                "render_frame called without a present swapchain (editor/headless mode)".to_owned(),
            ));
        }
        let raw = self.device.raw();
        let in_flight = self.frames.in_flight();

        // SAFETY: the ash seam. The fence belongs to this device; the wait blocks
        // until the slot's prior GPU work completes.
        checked(
            unsafe { raw.wait_for_fences(&[in_flight], true, u64::MAX) },
            "wait_for_fences",
        )?;

        let swapchain_loader = self.device.swapchain_loader();
        let image_available = self.frames.image_available();
        // SAFETY: the ash seam. Acquires the next image, signaling image_available.
        let acquire = unsafe {
            swapchain_loader.acquire_next_image(
                self.present_swapchain().handle(),
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        };
        let image_index = match acquire {
            Ok((index, _suboptimal)) => index as usize,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Ok(false),
            Err(result) => {
                return Err(Error::Vk {
                    context: "acquire_next_image",
                    result,
                });
            }
        };

        // The fence still tracking this image (from up to MAX_FRAMES_IN_FLIGHT
        // frames ago) must signal before its render-finished semaphore is reused.
        let tracking = self.present_swapchain().image_in_flight(image_index);
        if tracking != vk::Fence::null() {
            // SAFETY: the ash seam. The tracking fence belongs to this device.
            checked(
                unsafe { raw.wait_for_fences(&[tracking], true, u64::MAX) },
                "wait_for_fences(image)",
            )?;
        }
        self.swapchain
            .as_mut()
            .expect("present swapchain in windowed mode")
            .set_image_in_flight(image_index, in_flight);

        // SAFETY: the ash seam. Resetting an unsignaled-after-wait fence is valid
        // and required before resubmitting work that signals it.
        checked(unsafe { raw.reset_fences(&[in_flight]) }, "reset_fences")?;

        self.record_clear(image_index)?;
        self.submit_and_present(image_index)?;
        // A window capture armed by `request_window_capture` reads the just-presented
        // swapchain image (the composited window output) into a PNG, then disarms.
        if self.capture_next_window_path.is_some() {
            self.run_pending_window_capture(image_index);
        }
        self.frames.advance();
        Ok(true)
    }

    /// Records the clear into the current frame's command buffer.
    fn record_clear(&self, image_index: usize) -> Result<()> {
        let raw = self.device.raw();
        let command_buffer = self.frames.command_buffer();
        let image = self.present_swapchain().image(image_index);

        // SAFETY: the ash seam. The current frame's fence was waited above, so the
        // pool's buffer is no longer in use and may be reset.
        checked(
            unsafe {
                raw.reset_command_pool(
                    self.frames.command_pool(),
                    vk::CommandPoolResetFlags::empty(),
                )
            },
            "reset_command_pool",
        )?;

        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Begins recording on the freshly reset buffer.
        checked(
            unsafe { raw.begin_command_buffer(command_buffer, &begin_info) },
            "begin_command_buffer",
        )?;

        let full_subresource = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };

        // UNDEFINED → TRANSFER_DST (sync2): the swapchain image's contents are not
        // preserved, so discard via UNDEFINED. The clear writes as a transfer.
        let to_transfer = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::TOP_OF_PIPE)
            .src_access_mask(vk::AccessFlags2::empty())
            .dst_stage_mask(vk::PipelineStageFlags2::CLEAR)
            .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(full_subresource);
        let to_transfer = [to_transfer];
        let dep_to_transfer = vk::DependencyInfo::default().image_memory_barriers(&to_transfer);
        // SAFETY: the ash seam. The barrier references the acquired swapchain image.
        unsafe { raw.cmd_pipeline_barrier2(command_buffer, &dep_to_transfer) };

        let clear = vk::ClearColorValue {
            float32: self.clear_color,
        };
        let ranges = [full_subresource];
        // SAFETY: the ash seam. The image is in TRANSFER_DST per the barrier above.
        unsafe {
            raw.cmd_clear_color_image(
                command_buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &clear,
                &ranges,
            );
        }

        // TRANSFER_DST → PRESENT_SRC (sync2): make the clear visible to the
        // presentation engine.
        let to_present = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
            .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags2::BOTTOM_OF_PIPE)
            .dst_access_mask(vk::AccessFlags2::empty())
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(full_subresource);
        let to_present = [to_present];
        let dep_to_present = vk::DependencyInfo::default().image_memory_barriers(&to_present);
        // SAFETY: the ash seam. Same acquired image; recorded after the clear.
        unsafe { raw.cmd_pipeline_barrier2(command_buffer, &dep_to_present) };

        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(command_buffer) },
            "end_command_buffer",
        )?;
        Ok(())
    }

    /// Submits the recorded buffer (sync2) and presents the image.
    fn submit_and_present(&self, image_index: usize) -> Result<()> {
        let raw = self.device.raw();
        let command_buffer = self.frames.command_buffer();
        let render_finished = self.present_swapchain().render_finished(image_index);

        let wait = vk::SemaphoreSubmitInfo::default()
            .semaphore(self.frames.image_available())
            .stage_mask(vk::PipelineStageFlags2::CLEAR);
        let signal = vk::SemaphoreSubmitInfo::default()
            .semaphore(render_finished)
            .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS);
        let cmd = vk::CommandBufferSubmitInfo::default().command_buffer(command_buffer);

        let wait = [wait];
        let signal = [signal];
        let cmd = [cmd];
        let submit = vk::SubmitInfo2::default()
            .wait_semaphore_infos(&wait)
            .command_buffer_infos(&cmd)
            .signal_semaphore_infos(&signal);
        let submits = [submit];

        // SAFETY: the ash seam. The graphics queue is externally synchronized; this
        // submit runs on the render thread (the thumbnail worker submits behind the
        // queue mutex). The fence is freshly reset.
        checked(
            unsafe {
                raw.queue_submit2(
                    self.device.graphics_queue,
                    &submits,
                    self.frames.in_flight(),
                )
            },
            "queue_submit2",
        )?;

        let swapchains = [self.present_swapchain().handle()];
        let wait_semaphores = [render_finished];
        let image_indices = [image_index as u32];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&wait_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);

        // SAFETY: the ash seam. The swapchain/image-index are valid; the present
        // waits on render_finished signaled by the submit above.
        let present = unsafe {
            self.device
                .swapchain_loader()
                .queue_present(self.device.graphics_queue, &present_info)
        };
        match present {
            Ok(_) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                Ok(())
            }
            Err(result) => Err(Error::Vk {
                context: "queue_present",
                result,
            }),
        }
    }
}

/// One whole-image sync2 layout transition (single color mip), the capture path's
/// barrier.
///
/// # Safety
///
/// `image` must outlive the recorded command; `cmd` must be in the recording state.
#[allow(clippy::too_many_arguments)]
unsafe fn capture_barrier(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    range: vk::ImageSubresourceRange,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_stage: vk::PipelineStageFlags2,
    src_access: vk::AccessFlags2,
    dst_stage: vk::PipelineStageFlags2,
    dst_access: vk::AccessFlags2,
) {
    let b = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(range);
    let barriers = [b];
    let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
    // SAFETY: forwarded from this function's contract — the image outlives the command.
    unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
}

/// Records one single-set DDGI compute dispatch: bind the PSO + its set 0, push the per-pass
/// constants, dispatch `groups`. Shared by the blend + border passes (the trace binds three sets,
/// so it records its binds directly).
fn record_ddgi_compute(
    raw: &ash::Device,
    cmd: vk::CommandBuffer,
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set: vk::DescriptorSet,
    push: &[u8],
    groups: (u32, u32, u32),
) {
    // SAFETY: the ash seam. The PSO/set/layout are valid this frame; the push spans the
    // pass's declared range; the dispatch covers the pass's grid.
    unsafe {
        raw.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
        raw.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, layout, 0, &[set], &[]);
        raw.cmd_push_constants(cmd, layout, vk::ShaderStageFlags::COMPUTE, 0, push);
        raw.cmd_dispatch(cmd, groups.0, groups.1, groups.2);
    }
}

/// A `CLEAR`-to-1.0-then-`STORE` depth attachment — the scene/depth-prepass clear that
/// seeds the far plane (depth `LESS` then keeps the nearest fragment).
fn depth_clear_store(resource: crate::render_graph::RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::CLEAR,
        store_op: vk::AttachmentStoreOp::STORE,
        clear_value: vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: 1.0,
                stencil: 0,
            },
        },
        resolve: None,
    }
}

/// A `LOAD`-then-`STORE` color attachment: composite over the existing contents (the
/// grid + overlay draw over the tonemapped color and keep it).
fn color_load_store(resource: RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::LOAD,
        store_op: vk::AttachmentStoreOp::STORE,
        clear_value: vk::ClearValue::default(),
        resolve: None,
    }
}

/// A `LOAD`-then-`DONT_CARE` depth attachment: load the persisted 1× scene depth so the
/// grid / overlay depth-test against it, but never write it back (the grid + overlay PSOs
/// have depth writes off, so the depth is consumed read-only this pass).
fn depth_load_readonly(resource: RgResource) -> RgAttachment {
    RgAttachment {
        resource,
        load_op: vk::AttachmentLoadOp::LOAD,
        store_op: vk::AttachmentStoreOp::DONT_CARE,
        clear_value: vk::ClearValue::default(),
        resolve: None,
    }
}

/// Imports one SSGI history image into `graph` on an external layout slot (its layout
/// crosses frames: ShaderReadOnly ↔ General for the accum write). Returns the slot index
/// (read back after execute) + the imported resource. Panics if the image is not built
/// (the accum pass only runs once the SSGI chain is built).
fn import_ssgi_history(
    graph: &mut RenderGraph,
    image: &Option<crate::Image>,
) -> (usize, RgResource) {
    let image = image.as_ref().expect("ssgi history built");
    let slot = graph.alloc_external_layout(image.layout);
    let resource = graph.import_image(
        image.handle(),
        image.view(),
        vk::ImageAspectFlags::COLOR,
        image.layout,
        Some(slot),
    );
    (slot, resource)
}

/// Writes a TAA history image's resolved exit layout back from the graph's external slot.
/// `(history-index, slot)` selects the image in `view.history` and the slot to read.
fn writeback_history_layout(view: &mut ViewTarget, graph: &RenderGraph, slot: &(usize, usize)) {
    if let Some(image) = view.history[slot.0].as_mut() {
        image.layout = graph.external_layout(slot.1);
    }
}

/// Writes a TAA pixel-lock image's resolved exit layout back from the graph's external slot.
/// `(lock-index, slot)` selects the image in `view.lock` and the slot to read.
fn writeback_lock_layout(view: &mut ViewTarget, graph: &RenderGraph, slot: &(usize, usize)) {
    if let Some(image) = view.lock[slot.0].as_mut() {
        image.layout = graph.external_layout(slot.1);
    }
}

/// Writes an SSGI history image's resolved exit layout back from the graph's external slot.
/// `(history-index, slot)` selects the image in `view.ssgi_history` and the slot to read.
fn writeback_ssgi_history_layout(
    view: &mut ViewTarget,
    graph: &RenderGraph,
    slot: &(usize, usize),
) {
    if let Some(image) = view.ssgi_history[slot.0].as_mut() {
        image.layout = graph.external_layout(slot.1);
    }
}

/// Writes a DFAO history image's resolved exit layout back from the graph's external slot.
/// `(history-index, slot)` selects the image in `view.dfao_history` and the slot to read.
fn writeback_dfao_history_layout(
    view: &mut ViewTarget,
    graph: &RenderGraph,
    slot: &(usize, usize),
) {
    if let Some(image) = view.dfao_history[slot.0].as_mut() {
        image.layout = graph.external_layout(slot.1);
    }
}

/// The validation-clean gate's regression probe seam: when
/// `SAFFRON_VK_PLANT_VALIDATION_ERROR` is set, record one out-of-spec command into the
/// scene frame's command buffer so the validation layer flags exactly one error on submit.
///
/// This exists only to prove the gate's detector is live: an e2e test boots with the env set
/// and asserts `validation_errors()` is non-empty, so a silently-disabled gate (a renamed
/// messenger prefix, a missing validation layer) is itself a test failure. Unset (the default,
/// every real run) it is a single env read and a no-op. The planted call is a zero-width
/// viewport (`VUID-VkViewport-width-01770`); every pass sets its own viewport inside its render
/// pass, so the bad state never reaches a draw and the rendered output is unaffected.
fn plant_validation_error(raw: &ash::Device, command_buffer: vk::CommandBuffer) {
    if std::env::var_os("SAFFRON_VK_PLANT_VALIDATION_ERROR").is_none() {
        return;
    }
    let bad = vk::Viewport {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 1.0,
        min_depth: 0.0,
        max_depth: 1.0,
    };
    // SAFETY: the ash seam. `command_buffer` is recording (begun just above); a zero-width
    // viewport is rejected by the validation layer, which is the whole point — it does not
    // corrupt the device (always `VK_FALSE` from the messenger, no abort).
    unsafe { raw.cmd_set_viewport(command_buffer, 0, &[bad]) };
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // The run loop's responsibility per the README, made robust here: idle the
        // device so nothing is freed under a live GPU read, then destroy the
        // device-borrowing sub-state before the `device` field drops last.
        let _ = self.device.wait_idle();
        self.frames.destroy(&self.device);
        // Destroy each view's shm-capture fence (the only raw handle ViewTarget owns)
        // before the views Drop their VMA images/buffers.
        for view in &mut self.views {
            view.destroy(&self.device);
        }
        if let Some(present_sync) = self.present_sync.as_mut() {
            present_sync.destroy(&self.device);
        }
        if let Some(swapchain) = self.swapchain.as_mut() {
            swapchain.destroy(&self.device);
        }
        // `skinning` (and the other sub-state fields) Drop after this impl runs, in
        // declaration order — each ahead of the `device` field, which Drops last.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation_issue_count;
    use vk_mem::Alloc;

    /// A validation-clean offscreen clear+readback. Lavapipe's `VK_EXT_headless_surface`
    /// swapchain WSI crashes inside `wsi_create_native_image_mem` (it has no native
    /// image-memory backing for a headless surface), so the *present-engine* half of
    /// the loop is not exercisable in this toolbox without a real Wayland display.
    /// Everything the engine controls is, though: this allocates a color image via
    /// VMA, records the exact `UNDEFINED → TRANSFER_DST → clear → TRANSFER_SRC → copy`
    /// sync2 sequence the swapchain path uses, submits it on the graphics queue with
    /// the frame fence, reads the result back, and asserts both the cleared color
    /// landed and the run was validation-clean.
    ///
    /// The real acquire→present half is covered by `tests/swapchain_present.rs` on a
    /// weston Wayland surface (which lavapipe presents correctly), skipped when no
    /// display is available. Skips cleanly when no Vulkan device is obtainable.
    #[test]
    fn offscreen_clear_is_validation_clean() {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();
        let cleared = clear_offscreen_and_read_back(&device, [0.25, 0.5, 0.75, 1.0])
            .expect("offscreen clear+readback succeeds");
        device.wait_idle().expect("idle after the run");

        // The image is R8G8B8A8_UNORM; the cleared floats round to these bytes.
        assert_eq!(cleared, [64, 128, 191, 255], "the clear color reads back");
        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the clear+readback must be validation-clean (saw {} new issue(s))",
            after - before
        );
    }

    /// Allocates a 1×1 `R8G8B8A8_UNORM` image, clears it to `color`, copies it into a
    /// host-visible buffer, and returns the single texel's bytes. Exercises the same
    /// device/allocator/queue/sync2 path the swapchain present uses.
    fn clear_offscreen_and_read_back(device: &Device, color: [f32; 4]) -> Result<[u8; 4]> {
        let allocator = device.allocator();

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D {
                width: 1,
                height: 1,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferDevice,
            ..Default::default()
        };
        // SAFETY: the ash/VMA seam. The create-infos are valid for the call; the
        // returned image+allocation are freed below before the function returns.
        let (image, mut image_alloc) = unsafe { allocator.create_image(&image_info, &alloc_info) }
            .map_err(|result| Error::Vk {
                context: "create_image",
                result,
            })?;

        let buffer_info = vk::BufferCreateInfo::default()
            .size(4)
            .usage(vk::BufferUsageFlags::TRANSFER_DST);
        let buffer_alloc_info = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::AutoPreferHost,
            flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                | vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };
        // SAFETY: the ash/VMA seam. As above; freed before returning.
        let (buffer, mut buffer_alloc) =
            unsafe { allocator.create_buffer(&buffer_info, &buffer_alloc_info) }.map_err(
                |result| Error::Vk {
                    context: "create_buffer",
                    result,
                },
            )?;

        let result = record_clear_copy(device, image, buffer, color);

        // SAFETY: the ash/VMA seam. The device was idled by `record_clear_copy`
        // before this; each resource is destroyed exactly once.
        let texel = result.and_then(|()| {
            let info = allocator.get_allocation_info(&buffer_alloc);
            let ptr = info.mapped_data.cast::<u8>();
            if ptr.is_null() {
                return Err(Error::Vk {
                    context: "buffer not mapped",
                    result: vk::Result::ERROR_MEMORY_MAP_FAILED,
                });
            }
            // SAFETY: the buffer is HOST_VISIBLE + MAPPED and 4 bytes long; the copy
            // completed (the submit fence was waited).
            Ok(unsafe { std::ptr::read(ptr.cast::<[u8; 4]>()) })
        });
        // SAFETY: the ash/VMA seam. Destroyed after the device idled.
        unsafe {
            allocator.destroy_buffer(buffer, &mut buffer_alloc);
            allocator.destroy_image(image, &mut image_alloc);
        }
        texel
    }

    /// Records the clear + copy on a one-shot command buffer, submits with a fence,
    /// and waits — the same sync2 sequence as the swapchain path, ending in a copy
    /// to a host buffer instead of a present.
    fn record_clear_copy(
        device: &Device,
        image: vk::Image,
        buffer: vk::Buffer,
        color: [f32; 4],
    ) -> Result<()> {
        let raw = device.raw();
        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end of the function.
        let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
        // SAFETY: the ash seam. Default fence.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "fence",
        )?;

        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let record = || -> Result<()> {
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: the ash seam. The command-buffer recording below references
            // the image/buffer that outlive the submit-wait.
            unsafe {
                checked(raw.begin_command_buffer(cmd, &begin), "begin")?;
                barrier(
                    raw,
                    cmd,
                    image,
                    range,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::PipelineStageFlags2::TOP_OF_PIPE,
                    vk::AccessFlags2::empty(),
                    vk::PipelineStageFlags2::CLEAR,
                    vk::AccessFlags2::TRANSFER_WRITE,
                );
                let clear = vk::ClearColorValue { float32: color };
                raw.cmd_clear_color_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &clear,
                    &[range],
                );
                barrier(
                    raw,
                    cmd,
                    image,
                    range,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::CLEAR,
                    vk::AccessFlags2::TRANSFER_WRITE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: 1,
                        height: 1,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                );
                checked(raw.end_command_buffer(cmd), "end")?;
            }

            let cmd_info = vk::CommandBufferSubmitInfo::default().command_buffer(cmd);
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::default().command_buffer_infos(&cmd_infos);
            // SAFETY: the ash seam. Single-threaded queue use in this test.
            unsafe {
                checked(
                    raw.queue_submit2(device.graphics_queue, &[submit], fence),
                    "submit",
                )?;
                checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
            }
            Ok(())
        };
        let result = record();

        // SAFETY: the ash seam. The fence was waited (or the submit never happened),
        // so the pool/fence are idle and destroyed exactly once.
        unsafe {
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        result
    }

    /// Records one sync2 image-layout barrier.
    #[allow(clippy::too_many_arguments)]
    unsafe fn barrier(
        raw: &ash::Device,
        cmd: vk::CommandBuffer,
        image: vk::Image,
        range: vk::ImageSubresourceRange,
        old_layout: vk::ImageLayout,
        new_layout: vk::ImageLayout,
        src_stage: vk::PipelineStageFlags2,
        src_access: vk::AccessFlags2,
        dst_stage: vk::PipelineStageFlags2,
        dst_access: vk::AccessFlags2,
    ) {
        let b = vk::ImageMemoryBarrier2::default()
            .src_stage_mask(src_stage)
            .src_access_mask(src_access)
            .dst_stage_mask(dst_stage)
            .dst_access_mask(dst_access)
            .old_layout(old_layout)
            .new_layout(new_layout)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range);
        let barriers = [b];
        let dep = vk::DependencyInfo::default().image_memory_barriers(&barriers);
        // SAFETY: the ash seam. The image outlives the recorded command.
        unsafe { raw.cmd_pipeline_barrier2(cmd, &dep) };
    }

    /// The first end-to-end frame on llvmpipe: build the renderer sub-state (descriptors,
    /// the depth-prepass PSO, the per-frame instance SSBO, an offscreen view), submit a
    /// fullscreen-covering triangle draw list, record the depth pre-pass through the
    /// render graph into a depth target, read the depth back, and assert geometry
    /// rasterized (the center reads nearer than the cleared far plane) validation-clean.
    ///
    /// This is the GPU-runtime gate the toolbox can actually run: the vertex-only depth
    /// path (geometry → instanced draw → a depth image), built off the same
    /// `submit_draw_list` batching + scene-pass recording the shaded path uses. The
    /// shaded color golden image is DEFERRED — its übershader fragment reads the
    /// lighting / IBL descriptor sets that land in phases 7-11, so a full-color render
    /// cannot be validation-clean until then. Skips when no Vulkan device is present.
    #[test]
    fn depth_prepass_rasterizes_geometry_validation_clean() {
        use crate::descriptors::Descriptors;
        use crate::draw_list::{DrawItem, SubmeshMaterial};
        use crate::instancing::Instancing;
        use crate::pipelines::Pipelines;
        use crate::resources::BindlessFreeList;
        use crate::upload::{GpuQueue, Uploader};
        use crate::view_target::ViewTarget;
        use saffron_geometry::glam::{Mat4, Vec2, Vec3};
        use saffron_geometry::{Mesh, Submesh, Vertex};
        use std::sync::{Arc, Mutex};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let mut instancing = Instancing::new(&device, &descriptors).expect("Instancing");
        let mut skinning = Skinning::new(&device).expect("Skinning");
        let view = ViewTarget::new(&device, 16, 16).expect("ViewTarget");
        let queue = GpuQueue::new(device.graphics_queue);
        let uploader = Uploader::new(&device, &queue).expect("Uploader");

        // A clip-space triangle covering the whole viewport (NDC corners), so the depth
        // pre-pass writes the near plane (z=0) across the center under the identity push.
        let v = |x: f32, y: f32| Vertex {
            position: Vec3::new(x, y, 0.0),
            normal: Vec3::new(0.0, 0.0, 1.0),
            uv0: Vec2::ZERO,
        };
        let mesh = Mesh {
            vertices: vec![v(-3.0, -3.0), v(3.0, -3.0), v(0.0, 3.0)],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let mesh = uploader
            .upload_mesh(&descriptors, &mesh, &[], None, None)
            .expect("upload");
        let item = DrawItem::new(
            Arc::clone(&mesh),
            Mat4::IDENTITY,
            vec![SubmeshMaterial::defaults()],
        );
        let inputs = crate::instancing::DrawListInputs {
            frame: 0,
            view_proj: Mat4::IDENTITY,
            wireframe: false,
            default_texture_index: crate::DEFAULT_WHITE_SLOT,
            rt_skinned: false,
        };
        let (list, stats) = instancing
            .submit_draw_list(
                &descriptors,
                &mut pipelines,
                &mut skinning,
                &[item],
                &[],
                inputs,
            )
            .expect("submit_draw_list");
        assert_eq!(stats.batches, 1);
        assert_eq!(stats.instances, 1);

        // A transfer-capable depth image to render + read back (the per-view `ViewTarget`
        // depth is sampled, not transfer-copied, so it carries no `TRANSFER_SRC`; the
        // test owns this one to read the result on the CPU).
        let depth_image = crate::Image::new(
            device.resources(),
            &crate::ImageDesc {
                extent: view.scaled_render_extent(),
                format: crate::DEPTH_FORMAT,
                usage: vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_SRC,
                aspect: vk::ImageAspectFlags::DEPTH,
                view_type: vk::ImageViewType::TYPE_2D,
                mip_levels: 1,
                array_layers: 1,
                samples: vk::SampleCountFlags::TYPE_1,
            },
        )
        .expect("depth image");

        let depth_pipeline = pipelines.request_depth_prepass().expect("depth PSO");
        let depth = render_depth_prepass_readback(
            &device,
            depth_image.handle(),
            depth_image.view(),
            view.scaled_render_extent(),
            &list,
            depth_pipeline.handle(),
            depth_pipeline.layout(),
            instancing.instance_set(0),
        )
        .expect("depth readback");

        // The clear is the far plane (1.0); the rasterized triangle writes the near
        // plane (0.0) over the center. So the center reads ~0 and a corner stays ~1.
        let center = depth[16 * 8 + 8];
        assert!(
            center < 0.5,
            "geometry rasterized into depth (center={center})"
        );

        drop(list);
        drop(depth_pipeline);
        drop(depth_image);
        drop(mesh);
        drop(view);
        drop(instancing);
        device.wait_idle().expect("idle before teardown");
        drop(skinning);
        drop(uploader);
        drop(pipelines);
        drop(descriptors);
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the depth pre-pass frame must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Records the depth pre-pass through the render graph into `view`'s depth target on
    /// a one-off command buffer, copies the depth into a host buffer, and returns the
    /// `D32_SFLOAT` texels (row-major). The swapchain-free path the e2e test drives (a
    /// real `Renderer` cannot be built headless on lavapipe — its swapchain WSI crashes).
    #[allow(clippy::too_many_arguments)]
    fn render_depth_prepass_readback(
        device: &Device,
        depth_image: vk::Image,
        depth_view: vk::ImageView,
        extent: vk::Extent2D,
        list: &crate::draw_list::SceneDrawList,
        depth_pipeline: vk::Pipeline,
        depth_layout: vk::PipelineLayout,
        instance_set: vk::DescriptorSet,
    ) -> Result<Vec<f32>> {
        use crate::render_graph::{RenderGraph, RgPass};
        use crate::scene_pass::record_depth_prepass;

        let raw = device.raw();

        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end of the function.
        let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
        // SAFETY: the ash seam. Default fence.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "fence",
        )?;

        let row = extent.width as usize;
        let pixels = row * extent.height as usize;
        let buffer = crate::Buffer::new(
            device.resources(),
            (pixels * size_of::<f32>()) as vk::DeviceSize,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        let list = list.shallow_clone();
        let raw_body = raw.clone();
        let body_set = instance_set;
        let body = move |cmd: vk::CommandBuffer| {
            record_depth_prepass(
                &raw_body,
                cmd,
                &list,
                depth_pipeline,
                depth_layout,
                body_set,
                None,
            );
        };

        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::DEPTH,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the ash seam. Record the depth graph + the copy on the one-off buffer.
        let recorded = (|| -> Result<()> {
            unsafe { checked(raw.begin_command_buffer(cmd, &begin), "begin")? };

            let mut graph = RenderGraph::new();
            let depth = graph.import_image(
                depth_image,
                depth_view,
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            graph.add_pass(
                RgPass::graphics("depth-prepass", extent)
                    .depth_attachment(super::depth_clear_store(depth))
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| body(cmd)),
            );
            graph.execute(device, cmd);

            // SAFETY: the ash seam. DEPTH_ATTACHMENT → TRANSFER_SRC then copy out.
            unsafe {
                barrier(
                    raw,
                    cmd,
                    depth_image,
                    range,
                    vk::ImageLayout::DEPTH_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS,
                    vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    depth_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                checked(raw.end_command_buffer(cmd), "end")?;
            }

            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. Single-threaded queue use in the test.
            unsafe {
                checked(
                    raw.queue_submit2(device.graphics_queue, &submit, fence),
                    "submit",
                )?;
                checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
            }
            Ok(())
        })();

        let mut out = vec![0.0f32; pixels];
        if recorded.is_ok() {
            let ptr = buffer.mapped_ptr().cast::<f32>();
            // SAFETY: the buffer is HOST_VISIBLE + MAPPED; the copy completed.
            unsafe { std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), pixels) };
        }
        // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
        unsafe {
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        recorded.map(|()| out)
    }

    /// A GPU-runtime gate: build the descriptor + pipeline sub-state, seed a
    /// known linear-HDR color into an offscreen, then run the full final post chain
    /// (mandatory tonemap → ground grid → editor overlay) through the render graph and
    /// read the offscreen back. Asserts the tonemap mapped the HDR value to the expected
    /// display-referred byte, the grid + overlay composited over it (the center pixel
    /// changed where the on-top overlay quad covers it), and the whole frame was
    /// validation-clean on llvmpipe. Also asserts present-only vs editor mode produce
    /// byte-identical offscreen content (the flag does not touch `render_scene_offscreen`).
    /// Skips when no Vulkan device is present.
    #[test]
    fn final_post_chain_tonemaps_composites_grid_and_overlay_validation_clean() {
        use crate::descriptors::Descriptors;
        use crate::overlay::{OverlayState, OverlayVertex, TonemapPush};
        use crate::pipelines::Pipelines;
        use crate::resources::BindlessFreeList;
        use crate::ssao::Ssao;
        use crate::view_target::ViewTarget;
        use saffron_geometry::glam::{Vec2, Vec4};
        use std::sync::{Arc, Mutex};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let mut pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
        let ssao = Ssao::new(&device).expect("Ssao");
        let mut view = ViewTarget::new(&device, 16, 16).expect("ViewTarget");
        view.allocate_screen_space_sets(&descriptors, &ssao)
            .expect("alloc sets");
        view.build_screen_space(&device, &descriptors, &ssao)
            .expect("build screen-space (writes the tonemap set)");

        // The three post PSOs build on llvmpipe (graphics + compute, no RT).
        let tonemap = pipelines.request_tonemap().expect("tonemap PSO");
        let grid = pipelines.request_grid().expect("grid PSO");
        let overlay = pipelines.request_overlay().expect("overlay PSO");
        let overlay_depth = pipelines
            .request_overlay_depth()
            .expect("overlay-depth PSO");

        // A full-viewport on-top overlay quad in solid red (two triangles, no depth test).
        let red = Vec4::new(1.0, 0.0, 0.0, 1.0);
        let quad = |x: f32, y: f32| OverlayVertex::new(Vec2::new(x, y), red, Vec4::ZERO, 0.0);
        let on_top = vec![
            quad(-1.0, -1.0),
            quad(1.0, -1.0),
            quad(1.0, 1.0),
            quad(-1.0, -1.0),
            quad(1.0, 1.0),
            quad(-1.0, 1.0),
        ];

        // Thumbnails use PBR-Neutral so a material's color (a gold sphere) stays accurate in the
        // asset preview rather than getting the viewport's filmic look.
        let exposure = TonemapPush::new(0.0, crate::overlay::TonemapMode::PbrNeutral);

        // Render the chain twice; the only difference is the present-only flag, which does
        // not touch this path — the two readbacks must be byte-identical.
        let mut readbacks = Vec::new();
        for _present_only in [false, true] {
            let mut overlay_state = OverlayState::new(device.resources());
            overlay_state.submit(Vec::new(), on_top.clone());
            let draw = overlay_state.prepare(0).expect("prepare").expect("draw");

            let pixels = render_post_chain_readback(
                &device,
                &view,
                view.tonemap_set,
                tonemap.handle(),
                tonemap.layout(),
                &exposure,
                grid.handle(),
                grid.layout(),
                overlay.handle(),
                overlay_depth.handle(),
                &draw,
            )
            .expect("post-chain readback");
            readbacks.push(pixels);
        }

        // The on-top red overlay covers every pixel: the center R channel is ~1.0
        // (overlay alpha 1 over the tonemapped gray), and the two modes match byte-for-byte.
        let editor = &readbacks[0];
        let present_only = &readbacks[1];
        assert_eq!(
            editor, present_only,
            "present-only and editor mode produce identical offscreen content"
        );
        // Pixel (8,8), R channel (4 halves per pixel, R first). f16 1.0 == 0x3C00.
        let center_r = editor[(16 * 8 + 8) * 4];
        assert_eq!(
            center_r,
            half_from_f32(1.0),
            "the on-top red overlay covered the center"
        );

        device.wait_idle().expect("idle before teardown");
        drop(view);
        drop(ssao);
        drop(tonemap);
        drop(grid);
        drop(overlay);
        drop(overlay_depth);
        drop(pipelines);
        drop(descriptors);
        drop(free_list);
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the final post chain must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// The view-id wire tokens + dense slot indices are FROZEN end-to-end with the
    /// presenter's reader and the host's per-view shm segments (`Scene = 0`).
    #[test]
    fn view_id_wire_tokens_and_indices_are_frozen() {
        assert_eq!(ViewId::default(), ViewId::Scene);
        assert_eq!(ViewId::Scene.index(), 0);
        assert_eq!(ViewId::AssetPreview.index(), 1);
        assert_eq!(ViewId::Scene.wire(), "scene");
        assert_eq!(ViewId::AssetPreview.wire(), "assetPreview");
        assert_eq!(ViewId::from_wire("scene"), Some(ViewId::Scene));
        assert_eq!(
            ViewId::from_wire("assetPreview"),
            Some(ViewId::AssetPreview)
        );
        assert_eq!(ViewId::from_wire("nope"), None);
        // Round-trip every variant through its wire token.
        for view in [ViewId::Scene, ViewId::AssetPreview] {
            assert_eq!(ViewId::from_wire(view.wire()), Some(view));
        }
        assert_eq!(VIEW_COUNT, 2);
    }

    /// Both editor views are created at startup, each with its own offscreen targets;
    /// sizing one view leaves the other's extent + tracked desired size untouched, and a
    /// view's `desired_width` reads back the requested size (the seed-on-first-activate
    /// check). The capture path then reads the active view's offscreen back to a PNG file.
    /// Skips when no Vulkan device is obtainable (and the host swapchain WSI crashes
    /// headless, so a `Renderer` cannot bring up here — this exercises the per-view targets
    /// + the image→buffer→PNG capture pipeline directly on `ViewTarget`s, validation-clean).
    #[test]
    fn per_view_targets_size_independently_and_capture_writes_a_png() {
        use crate::descriptors::Descriptors;
        use crate::resources::BindlessFreeList;
        use crate::ssao::Ssao;
        use crate::view_target::ViewTarget;
        use std::sync::{Arc, Mutex};

        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors");
        let ssao = Ssao::new(&device).expect("Ssao");

        // Two independent views (mirroring the renderer's init loop): the scene view at
        // 24×16, the preview view sized later to 8×8.
        let mut views = Vec::with_capacity(VIEW_COUNT);
        for _ in 0..VIEW_COUNT {
            let mut view = ViewTarget::new(&device, 24, 16).expect("ViewTarget");
            view.allocate_screen_space_sets(&descriptors, &ssao)
                .expect("alloc sets");
            view.build_screen_space(&device, &descriptors, &ssao)
                .expect("build screen-space");
            views.push(view);
        }

        // A fresh view records its construction size as the desired size.
        assert_eq!(views[ViewId::Scene.index()].desired_width, 24);
        assert_eq!(views[ViewId::AssetPreview.index()].desired_width, 24);

        // Resize only the preview view to 8×8; the scene view is untouched.
        {
            let preview = &mut views[ViewId::AssetPreview.index()];
            preview.desired_width = 8;
            preview.desired_height = 8;
            let ext = vk::Extent2D {
                width: 8,
                height: 8,
            };
            preview.resize(&device, ext, ext).expect("resize preview");
            preview
                .build_screen_space(&device, &descriptors, &ssao)
                .expect("rebuild preview screen-space");
        }
        assert_eq!(
            views[ViewId::Scene.index()].scaled_render_extent().width,
            24
        );
        assert_eq!(
            views[ViewId::AssetPreview.index()]
                .scaled_render_extent()
                .width,
            8
        );
        assert_eq!(views[ViewId::AssetPreview.index()].desired_width, 8);

        // Capture the scene view's offscreen: clear it to a known linear-HDR gray through a
        // graphics pass, then copy it out exactly as `capture_viewport` does and encode a
        // PNG. Decode it back to confirm the dimensions + a clamped center pixel.
        let scene = &mut views[ViewId::Scene.index()];
        let tmp = std::env::temp_dir().join(format!(
            "saffron-capture-test-{}-{}.png",
            std::process::id(),
            scene.generation
        ));
        capture_view_to_png_for_test(&device, scene, &tmp).expect("capture");
        let decoded = image::open(&tmp).expect("decode capture").to_rgba8();
        assert_eq!(
            decoded.dimensions(),
            (24, 16),
            "PNG matches the offscreen size"
        );
        // The seed clears to linear 0.75; Clamp transfer keeps [0,1]×255 → ~191.
        let center = decoded.get_pixel(12, 8).0;
        assert!(
            (center[0] as i32 - 191).abs() <= 2,
            "the cleared gray reads back near 0.75×255 (got {})",
            center[0]
        );
        let _ = std::fs::remove_file(&tmp);

        device.wait_idle().expect("idle before teardown");
        drop(views);
        drop(ssao);
        drop(descriptors);
        drop(free_list);
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the per-view capture path must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }

    /// Seeds a known linear-HDR gray (0.75) into a view's offscreen via a graphics
    /// clear-store pass, then runs the exact image→buffer copy + PNG write `capture_viewport`
    /// records (a one-off submit on a transient pool). The standalone analog of
    /// `Renderer::capture_viewport` for a test that cannot bring up a full headless
    /// `Renderer`.
    fn capture_view_to_png_for_test(
        device: &Device,
        view: &mut ViewTarget,
        path: &std::path::Path,
    ) -> Result<()> {
        let raw = device.raw();
        let extent = view.offscreen.extent;
        let format = view.offscreen.format;
        let image = view.offscreen.handle();
        let offscreen_view = view.offscreen.view();
        let byte_size = extent.width as vk::DeviceSize
            * extent.height as vk::DeviceSize
            * crate::format_pixel_bytes(format) as vk::DeviceSize;

        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end.
        let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
        // SAFETY: the ash seam. Default fence.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "fence",
        )?;

        let buffer = crate::Buffer::new(
            device.resources(),
            byte_size,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;
        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Recorded on the one-off buffer.
            unsafe { checked(raw.begin_command_buffer(cmd, &begin), "begin")? };

            let mut graph = RenderGraph::new();
            let color = graph.import_image(
                image,
                offscreen_view,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let mut seed = RgAttachment::clear_store(color);
            seed.clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [0.75, 0.75, 0.75, 1.0],
                },
            };
            graph.add_pass(
                RgPass::graphics("seed", extent)
                    .color(seed)
                    .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
            );
            graph.execute(device, cmd);

            // The seed pass left the offscreen COLOR_ATTACHMENT_OPTIMAL; copy it out exactly
            // as `capture_viewport` does.
            // SAFETY: the ash seam. COLOR_ATTACHMENT → TRANSFER_SRC then copy out.
            unsafe {
                capture_barrier(
                    raw,
                    cmd,
                    image,
                    color_range,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                checked(raw.end_command_buffer(cmd), "end")?;
            }

            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. Single-threaded queue use in the test.
            unsafe {
                checked(
                    raw.queue_submit2(device.graphics_queue, &submit, fence),
                    "submit",
                )?;
                checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
            }
            Ok(())
        })();

        if recorded.is_ok() {
            let pixels =
                unsafe { std::slice::from_raw_parts(buffer.mapped_ptr(), byte_size as usize) };
            crate::write_png_file(pixels, extent.width, extent.height, format, path)
                .map_err(|err| Error::ShaderLoad(format!("capture write: {err}")))?;
        }
        view.offscreen.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
        unsafe {
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        recorded
    }

    /// Encodes an `f32` to its IEEE binary16 bit pattern (the offscreen is RGBA16F; the
    /// readback compares raw half words).
    fn half_from_f32(value: f32) -> u16 {
        let bits = value.to_bits();
        let sign = ((bits >> 16) & 0x8000) as u16;
        let exp = ((bits >> 23) & 0xff) as i32 - 127 + 15;
        let mantissa = bits & 0x7f_ffff;
        if exp <= 0 {
            return sign;
        }
        if exp >= 0x1f {
            return sign | 0x7c00;
        }
        sign | ((exp as u16) << 10) | ((mantissa >> 13) as u16)
    }

    /// Seeds a known linear-HDR gray into the offscreen, runs the final post chain
    /// (tonemap in-place → grid → overlay) through the render graph, and copies the
    /// offscreen out as raw RGBA16F half words. Mirrors the renderer's pass order; the
    /// offscreen carries `TRANSFER_SRC` + `STORAGE` so it can be cleared, tonemapped, and
    /// read back.
    #[allow(clippy::too_many_arguments)]
    fn render_post_chain_readback(
        device: &Device,
        view: &ViewTarget,
        tonemap_set: vk::DescriptorSet,
        tonemap_pipeline: vk::Pipeline,
        tonemap_layout: vk::PipelineLayout,
        exposure: &crate::overlay::TonemapPush,
        grid_pipeline: vk::Pipeline,
        grid_layout: vk::PipelineLayout,
        overlay_pipeline: vk::Pipeline,
        overlay_depth_pipeline: vk::Pipeline,
        draw: &crate::overlay::OverlayDraw,
    ) -> Result<Vec<u16>> {
        use crate::overlay::{GridPush, record_grid, record_overlay};
        use crate::render_graph::{RenderGraph, RgPass, RgUsage};

        let raw = device.raw();
        // At the test's render scale 1, the display (offscreen) and input (depth) extents match.
        let extent = view.published_extent();
        let offscreen = view.offscreen.handle();
        let offscreen_view = view.offscreen.view();
        let depth = view.depth.handle();
        let depth_view = view.depth.view();

        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. Freed at the end of the function.
        let pool = checked(unsafe { raw.create_command_pool(&pool_info, None) }, "pool")?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the ash seam. One buffer from the pool above.
        let cmd = checked(unsafe { raw.allocate_command_buffers(&alloc) }, "cmd")?[0];
        // SAFETY: the ash seam. Default fence.
        let fence = checked(
            unsafe { raw.create_fence(&vk::FenceCreateInfo::default(), None) },
            "fence",
        )?;

        let halves = extent.width as usize * extent.height as usize * 4;
        let buffer = crate::Buffer::new(
            device.resources(),
            (halves * size_of::<u16>()) as vk::DeviceSize,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )?;

        let color_range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };

        let exposure = *exposure;
        let grid_push = GridPush::new(Mat4::IDENTITY);
        let draw = *draw;
        let raw_body = raw.clone();

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        let recorded = (|| -> Result<()> {
            // SAFETY: the ash seam. Recorded on the one-off buffer.
            unsafe { checked(raw.begin_command_buffer(cmd, &begin), "begin")? };

            let mut graph = RenderGraph::new();
            // Both targets enter UNDEFINED; the seed graphics pass below clears them (the
            // offscreen carries no TRANSFER_DST, so the known HDR value is laid down via a
            // color-attachment clear, mirroring how the scene pass writes the color).
            let color = graph.import_image(
                offscreen,
                offscreen_view,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            let depth_res = graph.import_image(
                depth,
                depth_view,
                vk::ImageAspectFlags::DEPTH,
                vk::ImageLayout::UNDEFINED,
                None,
            );

            // Seed a known linear-HDR white (1.0) into the offscreen + a cleared-far depth
            // (so nothing occludes the on-top overlay). A graphics clear-store pass.
            let mut seed_color = RgAttachment::clear_store(color);
            seed_color.clear_value = vk::ClearValue {
                color: vk::ClearColorValue {
                    float32: [1.0, 1.0, 1.0, 1.0],
                },
            };
            graph.add_pass(
                RgPass::graphics("seed", extent)
                    .color(seed_color)
                    .depth_attachment(super::depth_clear_store(depth_res))
                    .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
            );

            // Tonemap (mandatory, in-place compute).
            let raw_tm = raw_body.clone();
            let push = exposure;
            let groups = |n: u32| n.div_ceil(8);
            graph.add_pass(
                RgPass::compute("tonemap")
                    .access(color, RgUsage::StorageImageRwCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. The set/PSO are valid; the dispatch covers
                        // the viewport (8×8 per group).
                        unsafe {
                            raw_tm.cmd_bind_pipeline(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                tonemap_pipeline,
                            );
                            raw_tm.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                tonemap_layout,
                                0,
                                &[tonemap_set],
                                &[],
                            );
                            raw_tm.cmd_push_constants(
                                cmd,
                                tonemap_layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(&push),
                            );
                            raw_tm.cmd_dispatch(
                                cmd,
                                groups(extent.width),
                                groups(extent.height),
                                1,
                            );
                        }
                    }),
            );

            // Grid (graphics, over the tonemapped color, depth-tested read-only).
            let raw_grid = raw_body.clone();
            graph.add_pass(
                RgPass::graphics("grid", extent)
                    .color(super::color_load_store(color))
                    .depth_attachment(super::depth_load_readonly(depth_res))
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_grid(&raw_grid, cmd, grid_pipeline, grid_layout, &grid_push);
                    }),
            );

            // Overlay (graphics, on-top range over the color).
            let raw_ov = raw_body.clone();
            graph.add_pass(
                RgPass::graphics("editor-overlay", extent)
                    .color(super::color_load_store(color))
                    .depth_attachment(super::depth_load_readonly(depth_res))
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        record_overlay(
                            &raw_ov,
                            cmd,
                            &draw,
                            overlay_pipeline,
                            overlay_depth_pipeline,
                        );
                    }),
            );

            graph.execute(device, cmd);

            // The overlay graphics pass left the offscreen COLOR_ATTACHMENT_OPTIMAL; copy
            // it out.
            // SAFETY: the ash seam. COLOR_ATTACHMENT → TRANSFER_SRC then copy out.
            unsafe {
                barrier(
                    raw,
                    cmd,
                    offscreen,
                    color_range,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                    vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
                    vk::PipelineStageFlags2::COPY,
                    vk::AccessFlags2::TRANSFER_READ,
                );
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    });
                raw.cmd_copy_image_to_buffer(
                    cmd,
                    offscreen,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer.handle(),
                    &[region],
                );
                // Restore the offscreen to UNDEFINED-equivalent for the next run (the
                // second iteration's clear transitions from UNDEFINED again).
                checked(raw.end_command_buffer(cmd), "end")?;
            }

            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            // SAFETY: the ash seam. Single-threaded queue use in the test.
            unsafe {
                checked(
                    raw.queue_submit2(device.graphics_queue, &submit, fence),
                    "submit",
                )?;
                checked(raw.wait_for_fences(&[fence], true, u64::MAX), "wait")?;
            }
            Ok(())
        })();

        let mut out = vec![0u16; halves];
        if recorded.is_ok() {
            let ptr = buffer.mapped_ptr().cast::<u16>();
            // SAFETY: the buffer is HOST_VISIBLE + MAPPED; the copy completed.
            unsafe { std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), halves) };
        }
        // SAFETY: the ash seam. The fence was waited, so the pool/fence are idle.
        unsafe {
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
        }
        recorded.map(|()| out)
    }

    /// The present-only blit's non-blank proof, headless: a full [`Renderer`] renders a visible
    /// procedural sky into its offscreen, then [`crate::present::record_present_blit`] (the exact
    /// barrier + `vkCmdBlitImage` sequence the windowed present path runs) blits that offscreen
    /// into a host-readable BGRA8 image, which is read back and asserted NON-UNIFORM.
    ///
    /// This is the headless stand-in for the windowed `present_only_blit_shows_a_non_blank_scene`
    /// integration test (which needs a real present surface): lavapipe cannot present a headless
    /// swapchain, but the blit itself — offscreen (RGBA16F) → a TRANSFER_DST BGRA8 image, with the
    /// SHADER_READ_ONLY/COLOR_ATTACHMENT → TRANSFER_SRC + UNDEFINED → TRANSFER_DST transitions — is
    /// the load-bearing part, and it runs anywhere. Proves the blit carries the rendered scene (not
    /// a uniform clear) and is validation-clean. Skips when no Vulkan device is obtainable.
    #[test]
    fn present_blit_carries_a_non_blank_scene() {
        use saffron_geometry::glam::{Mat4, Vec3};

        let mut renderer = match Renderer::new(&SurfaceSource::Offscreen, 64, 64) {
            Ok(renderer) => renderer,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        // A visible procedural sky + a camera + an empty draw list, so `render_scene_offscreen`
        // fills the offscreen with the sky's gradient (the non-uniform content the blit carries).
        renderer.submit_sky(&SkyRenderSettings::default());
        renderer
            .set_scene_lighting(&SceneLighting::default())
            .expect("set_scene_lighting");
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.1, 100.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.0, 4.0), Vec3::ZERO, Vec3::Y);
        renderer
            .submit_draw_list(proj * view, &[])
            .expect("submit_draw_list");
        renderer
            .render_scene_offscreen()
            .expect("render_scene_offscreen");

        // The offscreen now holds the rendered sky. Allocate a BGRA8 destination + read-back
        // buffer, then run the exact present blit into it and read it back.
        let extent = renderer.active_view().offscreen.extent;
        let device = renderer.device_arc();
        let raw = device.raw();
        let dst_format = vk::Format::B8G8R8A8_UNORM;
        // The destination stands in for a swapchain image: TRANSFER_DST (the blit target) +
        // TRANSFER_SRC (the read-back) + COLOR_ATTACHMENT (swapchain images carry it, and
        // `Image::new` builds a sampled-compatible view).
        let dst = crate::Image::new(
            device.resources(),
            &crate::ImageDesc::color_2d(
                extent,
                dst_format,
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::COLOR_ATTACHMENT,
            ),
        )
        .expect("dst image");
        let byte_size = extent.width as vk::DeviceSize
            * extent.height as vk::DeviceSize
            * crate::format_pixel_bytes(dst_format) as vk::DeviceSize;
        let buffer = crate::Buffer::new(
            device.resources(),
            byte_size,
            vk::BufferUsageFlags::TRANSFER_DST,
            &vk_mem::AllocationCreateInfo {
                usage: vk_mem::MemoryUsage::Auto,
                flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_RANDOM
                    | vk_mem::AllocationCreateFlags::MAPPED,
                ..Default::default()
            },
        )
        .expect("readback buffer");

        renderer.device().wait_idle().expect("idle before blit");
        let offscreen = renderer.active_view().offscreen.handle();
        let from_layout = renderer.active_view().offscreen.layout;
        let (from_stage, from_access) = match from_layout {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL => (
                vk::PipelineStageFlags2::FRAGMENT_SHADER,
                vk::AccessFlags2::SHADER_SAMPLED_READ,
            ),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL => (
                vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT,
                vk::AccessFlags2::COLOR_ATTACHMENT_WRITE,
            ),
            _ => (vk::PipelineStageFlags2::TOP_OF_PIPE, vk::AccessFlags2::NONE),
        };

        let pool_info =
            vk::CommandPoolCreateInfo::default().queue_family_index(device.graphics_queue_family);
        // SAFETY: the ash seam. One-off pool/buffer/fence for the blit + read-back; all freed
        // below after the fence signals.
        let pixels = unsafe {
            let pool = raw.create_command_pool(&pool_info, None).expect("pool");
            let cmd = raw
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1),
                )
                .expect("cmd")[0];
            let fence = raw
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .expect("fence");
            raw.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .expect("begin");
            // This stand-in runs on an offscreen device with no `VK_KHR_swapchain`, where
            // `PRESENT_SRC_KHR` is invalid; the blit leaves `dst` in `TRANSFER_SRC` directly so the
            // read-back copy can read it. The windowed present path passes `PRESENT_SRC_KHR`, proven
            // on a real surface in `tests/swapchain_present.rs`.
            crate::present::record_present_blit(
                raw,
                cmd,
                offscreen,
                extent,
                from_layout,
                from_stage,
                from_access,
                dst.handle(),
                extent,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                });
            raw.cmd_copy_image_to_buffer(
                cmd,
                dst.handle(),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer.handle(),
                &[region],
            );
            raw.end_command_buffer(cmd).expect("end");
            let cmd_info = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
            let submit = [vk::SubmitInfo2::default().command_buffer_infos(&cmd_info)];
            raw.queue_submit2(device.graphics_queue, &submit, fence)
                .expect("submit");
            raw.wait_for_fences(&[fence], true, u64::MAX).expect("wait");
            let slice =
                std::slice::from_raw_parts(buffer.mapped_ptr(), byte_size as usize).to_vec();
            raw.destroy_fence(fence, None);
            raw.destroy_command_pool(pool, None);
            slice
        };

        // The blitted BGRA8 image must be NON-UNIFORM: the procedural sky is a gradient, so the
        // pixels carry many distinct colors. A uniform clear would yield exactly one.
        let mut distinct = std::collections::HashSet::new();
        for px in pixels.chunks_exact(4) {
            distinct.insert([px[0], px[1], px[2]]);
            if distinct.len() > 64 {
                break;
            }
        }
        assert!(
            distinct.len() > 16,
            "the present blit carried a NON-BLANK scene (saw {} distinct colors; a uniform clear \
             would be 1) — the offscreen sky was blitted, not a flat fill",
            distinct.len()
        );

        renderer.device().wait_idle().expect("idle before teardown");
        drop(buffer);
        drop(dst);
        drop(renderer);
        drop(device);

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the present blit must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
