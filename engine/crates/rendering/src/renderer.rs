//! The top-level renderer aggregate: the immutable [`Device`], the [`Swapchain`], the
//! [`crate::frame::FrameRing`], and the per-area sub-state (descriptors, pipelines,
//! targets, lighting, …) as sibling fields, each mutated through its own methods while
//! holding `&Device`. It drives the per-frame acquire → render-graph → present loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ash::vk;
use saffron_geometry::glam::{Mat4, Vec3, Vec4};

use crate::budget::{BudgetController, BudgetStep};
use crate::ddgi::DDGI_RAYS_PER_PROBE;
use crate::descriptors::Descriptors;
use crate::device::SurfaceSource;
use crate::draw_list::{RenderStats, SceneDrawList};
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
    /// The froxel volumetric-fog volume: the integrated in-scatter + fog opacity, visualized by the
    /// fog composite pass (requires `fog.mode == volumetric`, where the froxel grid is populated).
    Fog,
    /// Raw volumetric cloud density integrated by the dedicated cloud debug pass.
    CloudDensity,
    /// Virtual-shadow page visualization: the directional sampler's resolved
    /// (level, page) as a stable colour, dimmed where no page is resident.
    ShadowPages,
}

impl ViewMode {
    /// The debug-shading channel the mesh fragment outputs instead of full shading;
    /// folded into the light UBO's `point_shadow_meta.w`. `0` is full shading
    /// (`Lit`, `Wireframe`, `LitWireframe`, `MotionVectors`, and `Fog` — the last three
    /// being produced by dedicated passes).
    fn debug_channel(self) -> u32 {
        match self {
            ViewMode::Lit
            | ViewMode::Wireframe
            | ViewMode::LitWireframe
            | ViewMode::MotionVectors
            // Fog is visualized by the fog composite pass, not the mesh debug channel.
            | ViewMode::Fog
            | ViewMode::CloudDensity => 0,
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
            ViewMode::ShadowPages => 14,
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
    /// The offscreen thumbnail-render view (slot `2`). Never shm-published and not selectable
    /// over the wire — a background render target for material/texture preview tiles, isolated
    /// from the Scene/AssetPreview size + temporal state so a tile can render while a viewport is
    /// live. Its readback goes straight to a PNG, never the presenter ring.
    Thumbnail,
}

/// The number of editor render views (scene + asset-preview + offscreen thumbnail).
pub const VIEW_COUNT: usize = 3;

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
            ViewId::Thumbnail => 2,
        }
    }

    /// Stable world identity reserved for this renderer-owned view.
    pub fn gpu_scene_world(self) -> crate::GpuSceneWorldId {
        crate::GpuSceneWorldId(self.index() as u64)
    }

    /// Stable temporal-view identity reserved for this renderer-owned view.
    pub fn gpu_scene_view(self) -> crate::GpuSceneViewId {
        crate::GpuSceneViewId(self.index() as u64)
    }

    /// The [`ViewId`] for a dense slot index, the inverse of [`ViewId::index`].
    pub fn from_index(index: usize) -> Self {
        match index {
            1 => ViewId::AssetPreview,
            2 => ViewId::Thumbnail,
            _ => ViewId::Scene,
        }
    }

    /// The control-plane / shm wire token, FROZEN end-to-end with the presenter's reader
    /// (`editor/shell/src/presenter.rs`). Exactly `"scene"` / `"assetPreview"`; the
    /// `Thumbnail` view is offscreen-only (`"thumbnail"`) and never reaches the presenter.
    pub fn wire(self) -> &'static str {
        match self {
            ViewId::Scene => "scene",
            ViewId::AssetPreview => "assetPreview",
            ViewId::Thumbnail => "thumbnail",
        }
    }

    /// Parses a wire token into a [`ViewId`]; `None` for an unknown token. The offscreen
    /// `Thumbnail` view is intentionally **not** parseable — `set-active-view` cannot select it.
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
    /// Last completed frame's virtual-shadow residency activity.
    pub vsm: crate::VsmCounters,
    /// Wall-clock render-thread frame time (ms); `0` until the run loop records it.
    pub frame_ms: f32,
    /// Frames per second derived from `frame_ms` (`0` when `frame_ms` is `0`).
    pub fps: f32,
    /// GPU frame time (ms); `0` until the profiler runs.
    pub gpu_ms: f32,
    /// CPU busy time (ms).
    pub cpu_frame_ms: f32,
    /// CPU time spent gathering the static and skinned scene draw list (ms).
    pub scene_gather_ms: f32,
    /// Fence-wait time (ms).
    pub cpu_wait_ms: f32,
    /// Instances published into the active frame TLAS.
    pub rt_instances: u32,
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
    /// The scene-linear color grade folded into the tonemap pass (white balance, contrast,
    /// saturation, ASC-CDL).
    pub color_grade: ColorGrade,
    /// Whether the pre-tonemap bloom pyramid is enabled.
    pub bloom_enabled: bool,
    /// The energy-conserving bloom composite weight.
    pub bloom_intensity: f32,
    /// The bloom tent-upsample scatter radius (UV units).
    pub bloom_scatter: f32,
    /// The bloom tint (multiplies the composited bloom).
    pub bloom_tint: [f32; 3],
    /// The bloom soft-knee prefilter threshold (`0.0` = thresholdless).
    pub bloom_threshold: f32,
    /// The lens-dirt mask asset id (`0` = none).
    pub bloom_dirt_texture: u64,
    /// The lens-dirt mix fraction.
    pub bloom_dirt_intensity: f32,
    /// The lens-dirt tint.
    pub bloom_dirt_tint: [f32; 3],
    /// Whether the anamorphic streak runs.
    pub bloom_anamorphic_enabled: bool,
    /// The anamorphic horizontal squeeze.
    pub bloom_anamorphic_ratio: f32,
    /// The anamorphic streak tint.
    pub bloom_anamorphic_tint: [f32; 3],
    /// The anamorphic streak add weight.
    pub bloom_anamorphic_intensity: f32,
}

/// The PSOs one offscreen frame needs, resolved up front (each request borrows
/// `&mut Pipelines`) so the render-graph build borrows the rest of the renderer
/// immutably. A `None` arms nothing — that pass is skipped this frame.
struct FramePipelines {
    depth_prepass: Option<Arc<crate::Pipeline>>,
    /// The tessellation seam's vertex-input pass PSOs, resolved only when the frame
    /// carries tess draws.
    depth_prepass_tess: Option<Arc<crate::Pipeline>>,
    gbuffer_tess: Option<Arc<crate::Pipeline>>,
    motion_tess: Option<Arc<crate::Pipeline>>,
    cull: Option<Arc<crate::Pipeline>>,
    /// The compute skinning PSO, resolved when the frame has skinned dispatches.
    skin: Option<Arc<crate::Pipeline>>,
    /// The compute morph PSO, resolved when the frame has morph dispatches.
    morph: Option<Arc<crate::Pipeline>>,
    /// The executor depth PSO the virtual-shadow page passes rasterize with.
    shadow: Option<Arc<crate::Pipeline>>,
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
    /// The bloom pyramid compute PSO, resolved only when bloom is enabled this frame.
    bloom: Option<Arc<crate::Pipeline>>,
    /// The mandatory tonemap compute PSO (always resolved unless its build fails).
    tonemap: Option<Arc<crate::Pipeline>>,
    /// The analytic height-fog composite compute PSO, resolved only while fog is enabled this frame.
    fog: Option<Arc<crate::Pipeline>>,
    /// The weather-map refill compute PSO, resolved only while the authored map is dirty.
    cloud_weather: Option<Arc<crate::Pipeline>>,
    /// The unlit cloud-density compute PSO, resolved only in CloudDensity view mode.
    cloud_debug: Option<Arc<crate::Pipeline>>,
    /// The adaptive lit cloud raymarch PSO.
    cloud_raymarch: Option<Arc<crate::Pipeline>>,
    /// The reduced cloud temporal reconstruction PSO.
    cloud_reconstruct: Option<Arc<crate::Pipeline>>,
    /// The bilateral upscale and HDR cloud composite PSO.
    cloud_upscale: Option<Arc<crate::Pipeline>>,
    /// The cascaded density-integrated cloud-shadow fill PSO.
    cloud_shadow: Option<Arc<crate::Pipeline>>,
    /// The froxel fog-inject / fog-integrate compute PSOs, resolved only while volumetric fog is on.
    fog_inject: Option<Arc<crate::Pipeline>>,
    fog_integrate: Option<Arc<crate::Pipeline>>,
    /// The aerial-perspective fill compute PSO, resolved only while an active atmosphere + authored AP
    /// arm the fill this frame.
    aerial: Option<Arc<crate::Pipeline>>,
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
    /// The transition-reactive graphics PSO: re-draws the opaque buckets through the
    /// degenerate-collapse vertex path so blades and representation transitions mark the
    /// reactive mask too.
    reactive_transition: Option<Arc<crate::Pipeline>>,
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

struct RecordedSceneGraph {
    batches: Vec<RgRecordedBatch>,
    tail: vk::CommandBuffer,
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

/// One level of the transient bloom mip pyramid: its graph resource (barrier tracking), its image
/// view (bound into the per-pass descriptor sets), and its extent (the dispatch group count).
struct BloomMip {
    res: RgResource,
    view: vk::ImageView,
    extent: vk::Extent2D,
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
    /// The porous-occupancy volume resources + their external slots, mirroring the
    /// distance cascades.
    occupancy: Option<[RgResource; crate::GDF_CASCADES as usize]>,
    occupancy_slots: [Option<usize>; crate::GDF_CASCADES as usize],
    /// The lite albedo cache resource (the DDGI trace's `SampledRead`) + its external slot. `Some`
    /// only when the composite ran this frame (it writes the cache GENERAL).
    albedo: Option<RgResource>,
    albedo_slot: Option<usize>,
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

/// The authored analytic height/distance fog, resolved from `scene.environment.fog` and pushed to
/// the renderer each frame. A plain mirror of the scene [`saffron_scene::FogSettings`] block (the
/// rendering crate does not depend on the scene, so the conversion lives in the assets layer). The
/// broad layer plus an optional ground layer sum into one closed-form optical depth.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogRenderSettings {
    /// Whether the fog composite runs this frame.
    pub enabled: bool,
    /// Broad-layer sigma at `height`.
    pub density: f32,
    /// In-scatter tint (multiplied by the sky-view LUT when the atmosphere is live).
    pub albedo: Vec3,
    /// World-up reference height of the broad layer.
    pub height: f32,
    /// Exponential density falloff with world-up distance.
    pub height_falloff: f32,
    /// Fog begins this far from the eye.
    pub start_distance: f32,
    /// Clamps `1 - transmittance`.
    pub max_opacity: f32,
    /// Constant in-medium emission.
    pub emissive: Vec3,
    /// Sun-through-haze lobe color.
    pub directional_color: Vec3,
    /// Lobe sharpness.
    pub directional_exponent: f32,
    /// Ground-haze layer sigma; `0` disables it.
    pub layer2_density: f32,
    /// Ground-haze exponential density falloff.
    pub layer2_falloff: f32,
    /// Ground-haze world-up reference height.
    pub layer2_height: f32,
    /// The froxel volumetric path runs instead of the analytic closed form. The analytic height
    /// density is injected as the froxel base medium — never applied twice.
    pub volumetric: bool,
    /// Constant scattering-medium extinction floor (added to the analytic height density).
    pub base_density: f32,
    /// Single-scattering albedo (`sigma_s = albedo * sigma_t`).
    pub scatter_albedo: f32,
    /// Henyey-Greenstein phase anisotropy (`g`), forward-scattering for `g > 0`.
    pub phase_g: f32,
    /// The froxel-grid quality tier (grid dimensions) for the volumetric path.
    pub quality: crate::FroxelQuality,
    /// Temporal reprojection blend: the fresh-sample weight per frame (`0.05` default).
    pub history_blend: f32,
    /// Clamp the reprojected history to a band of the fresh sample (firefly / ghost suppression).
    pub neighborhood_clamp: bool,
    /// Cap each light's per-froxel in-scatter before accumulation (`0` = off).
    pub light_clamp: f32,
    /// Fill + composite the Hillaire-2020 aerial-perspective volume (needs a live atmosphere; the
    /// renderer gates the AP fill on the baked atmosphere, so this is a no-op without one).
    pub aerial_perspective: bool,
    /// Aerial-perspective in-scatter strength multiplier.
    pub aerial_intensity: f32,
}

impl Default for FogRenderSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            density: 0.02,
            albedo: Vec3::new(0.5, 0.6, 0.7),
            height: 0.0,
            height_falloff: 0.2,
            start_distance: 0.0,
            max_opacity: 1.0,
            emissive: Vec3::ZERO,
            directional_color: Vec3::new(1.0, 0.9, 0.7),
            directional_exponent: 8.0,
            layer2_density: 0.0,
            layer2_falloff: 0.5,
            layer2_height: 0.0,
            volumetric: false,
            base_density: 0.02,
            scatter_albedo: 0.9,
            phase_g: 0.6,
            quality: crate::FroxelQuality::Medium,
            history_blend: 0.05,
            neighborhood_clamp: false,
            light_clamp: 0.0,
            aerial_perspective: false,
            aerial_intensity: 1.0,
        }
    }
}

/// The height-fog compute pass's uniform, matching `height_fog.slang`'s `FogParams`. `layer0` /
/// `layer1` pack `(density, heightFalloff, height, pad)`; the trailing `_pad0` keeps the 16-byte
/// std140 rows aligned.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FogParams {
    inv_view_proj: [[f32; 4]; 4],
    camera_pos: [f32; 3],
    max_opacity: f32,
    albedo: [f32; 3],
    start_distance: f32,
    emissive: [f32; 3],
    dir_exponent: f32,
    sun_dir: [f32; 3],
    use_sky_lut: f32,
    dir_color: [f32; 3],
    _pad0: f32,
    layer0: [f32; 4],
    layer1: [f32; 4],
    /// `x` = mode (0 analytic, 1 volumetric), `y` = froxel near, `z` = froxel far (the Z
    /// distribution the composite W mapping inverts), `w` = fog debug view (1 = output the froxel
    /// in-scatter + opacity directly).
    froxel: [f32; 4],
    /// The camera view direction (world), for the froxel W depth projection.
    cam_forward: [f32; 3],
    /// `1` when the analytic/volumetric fog contributes this frame, `0` when the composite runs only
    /// to apply aerial perspective (fog is disabled but AP is live) — then `T_fog = 1`, `inScatter_fog = 0`.
    fog_enabled: f32,
    /// Aerial perspective: `x` = enabled (atmosphere live + AP authored), `y` = AP near, `z` = AP far
    /// (the exponential-Z distribution the composite's AP W mapping inverts), `w` unused.
    aerial: [f32; 4],
    /// `x` = full-resolution cloud scatter/transmittance is live.
    cloud: [f32; 4],
    /// Atmosphere physical and camera block shared with the AP volume fill.
    ap: crate::AerialParamsUbo,
}

/// One world's wind sway record buffer: one [`crate::GpuWindInstanceRecord`] per
/// instance slot, recreated (behind an idle wait) when the world's instance
/// capacity outgrows it.
struct WindDeformRecords {
    buffer: crate::Buffer,
    capacity: u32,
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
    /// Whether the anamorphic streak (a horizontally-squeezed blur, added over the radial bloom)
    /// runs. Off by default.
    bloom_anamorphic_enabled: bool,
    /// The anamorphic horizontal squeeze (`~2.0`). Default `2.0`.
    bloom_anamorphic_ratio: f32,
    /// The anamorphic streak tint (cool by default).
    bloom_anamorphic_tint: [f32; 3],
    /// The anamorphic streak add weight (`0.0` = no streak). Default `0.0`.
    bloom_anamorphic_intensity: f32,
    /// The optional per-upsample-step tint stack (identity `1,1,1` when off). Fanned out one tint
    /// per progressive upsample pass on the CPU, so the push carries one at a time.
    bloom_mip_tint: Vec<[f32; 3]>,
    /// The authored analytic height/distance fog, resolved from `scene.environment.fog` each frame
    /// and composited into the scene-linear HDR offscreen before bloom. Off by default.
    fog: FogRenderSettings,
    /// The froxel volumetric-fog volumes + compute sets. Filled by the inject/integrate passes and
    /// sampled by the composite when `fog.mode == Volumetric`.
    froxel: crate::FroxelFog,
    /// The Hillaire-2020 aerial-perspective volume + fill set. Filled from the atmosphere LUTs and
    /// folded into the fog composite on the shared transmittance ledger while AP is live.
    aerial: crate::AerialPerspective,
    /// Persistent channel-packed cloud noise, curl, and single resolved weather map.
    clouds: crate::Clouds,
    /// This frame's local `FogVolume` records, baked from the scene's `submit_fog_volumes`, uploaded
    /// into the inject SSBO and looped per froxel during injection.
    fog_volumes: Vec<crate::FogVolumeGpu>,
    /// A wrapping scene clock (seconds) driving the fog-volume noise wind advection, accumulated in
    /// [`Renderer::observe_frame_delta`].
    fog_time: f32,
    /// The directional light's travel direction (normalized; the way the sun's light goes), captured
    /// on the scene-lighting write so the fog pass can point its sun-inscatter lobe toward the sun.
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

    /// The debug render-output mode. Transient; drives the
    /// wireframe PSO permutation + the mesh fragment's debug-channel output.
    view_mode: ViewMode,
    /// Whether the GPU compute-skinning path runs. Off falls
    /// back to bind-pose meshes. Defaults on.
    skinning_enabled: bool,

    /// Whether the GPU compute-displacement path runs. Off leaves displacement-enabled meshes
    /// undisplaced (the base geometry). Defaults on.
    displacement_enabled: bool,

    /// The runtime-tunable tessellation-quality budget fed to each displaced instance's `TessBucket`
    /// (hard dice cap, minimum per-edge factor, target screen-space edge length in pixels). Driven by
    /// the `set-tessellation-quality` control command; defaults to the `TESS_DEFAULT_*` consts.
    tess_factor_cap: f32,
    tess_min_factor: f32,
    tess_edge_length_target: f32,

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
    /// The last static + skinned draw-list gather time (ms).
    scene_gather_ms: f32,
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
    /// Wall-clock ns of the last [`Renderer::finalize_frame_telemetry`], for the alarm tick's
    /// irregular-interval dt.
    last_frame_ns: u64,

    /// The per-frame editor-overlay geometry (gizmo handles + entity billboards),
    /// uploaded into a grow-only per-frame vertex buffer and composited after tonemap.
    overlay: OverlayState,

    submissions: Vec<RenderFn>,
    scene_draw_list: SceneDrawList,
    stats: RenderStats,

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

    /// The scene-linear color grade (default neutral identity), folded into the tonemap pass before
    /// the view/display transform and reported in stats.
    color_grade: ColorGrade,

    /// The always-bound neutral identity creative LUT (2×2×2 ramp) bound at binding 2 of every view's
    /// tonemap set when no creative look is assigned.
    default_lut: Arc<crate::GpuLut>,
    /// The assigned display-space creative look-up table, sampled tetrahedrally after the view
    /// transform. `None` binds [`Renderer::default_lut`] (the identity ramp).
    creative_lut: Option<Arc<crate::GpuLut>>,
    /// The creative-LUT asset id for read-back (`0` = none), mirroring [`Renderer::creative_lut`].
    creative_lut_id: u64,
    /// The creative-LUT look intensity in `[0, 1]` (`0` = neutral), carried in the grade UBO.
    creative_lut_intensity: f32,
    /// The bound creative LUT's resolution per axis (`2` when none), carried in the grade UBO so the
    /// tetrahedral sample scales `[0,1] → [0, n-1]` correctly.
    creative_lut_size: u32,

    /// The reactive-loop observability mirror: the host pushes the idle/converged/reasons snapshot
    /// each frame (the verdict lives above this crate), and the editor sets the power state; both
    /// surface in `render-stats`, and the host reads the power state back to suppress a hidden view.
    reactive: ReactiveState,

    /// The anti-aliasing selection (MSAA / FXAA / TAA, mutually exclusive). The frame
    /// graph branches the scene output on this; the temporal targets live per-view.
    aa: crate::Aa,

    /// The runtime TAA resolve tuning (feedback range, velocity rejection, clip gamma,
    /// sharpen). Read into the resolve push each frame; live-tunable over the control plane.
    taa_params: crate::TaaParams,
    /// The active camera's `(near, far)` planes, mirrored from the last [`Renderer::set_cluster_camera`]
    /// so the TAA resolve can linearize `motionDepth` for its disocclusion test.
    camera_near_far: (f32, f32),
    /// The last camera the host set, mirrored so the adaptive-tessellation factor pass can derive its
    /// screen-space metric (world position via `inverse(view)`, `tan(½fov)` from the projection).
    cluster_camera: ClusterCamera,

    /// The per-editor-pane render targets, indexed by [`ViewId::index`] (`Scene` = 0,
    /// `AssetPreview` = 1). Always [`VIEW_COUNT`] entries.
    views: Vec<ViewTarget>,
    /// Device-global immutable arenas and tables shared by every registered GPU-scene world.
    global_gpu_data: crate::GlobalGpuData,
    /// Device tables of the persistent GPU scene plus its frame upload translation.
    gpu_scene_uploader: crate::GpuSceneUploader,
    /// Per-world wind sway record buffers (one record per instance slot), written by
    /// the wind deformation prepass and read through the address block.
    wind_deform_records: std::collections::HashMap<u64, WindDeformRecords>,
    /// The frame's shared wind field parameters (clouds and fog advect on the same
    /// state the light UBO and the deformation prepass carry).
    scene_wind: SceneWind,
    /// The virtual shadow map: the physical atlas + page-table ring.
    vsm_gpu: crate::vsm::VsmGpu,
    /// The VSM CPU residency authority (allocation, LRU, cooldown, dirty pages).
    vsm_residency: crate::VsmResidency,
    /// Per-directional-level page-render visibility views, built lazily by the
    /// page-render block.
    vsm_views: Vec<Option<crate::SceneVisibilityView>>,
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
    /// The point light's position + far of the pages on the atlas; a change
    /// invalidates every point-face page.
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
    /// The mirror's upper bound on emitted draw records (see
    /// [`set_live_draw_record_bound`](Self::set_live_draw_record_bound)).
    live_draw_record_bound: u32,
    /// The latest fence-completed visibility counters (visible/retest/records +
    /// overflow/pressure flags) for the active view.
    visibility_counters: [u32; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
    page_faults: u64,
    /// Resident-record stages, retirements, and arena uploads queued by the asset mirror,
    /// drained into each frame's transfer passes.
    pending_gpu_scene_uploads: crate::GpuScenePendingUploads,
    /// The resident micro-field tile directory (fields-arena byte offset + entries).
    micro_field_directory: Option<(u32, u32)>,
    /// The most recent frame's upload-translation counters.
    last_gpu_scene_upload: crate::GpuSceneUploadRunStats,
    /// Sole renderer-derived mirror for scene, preview, thumbnail, and player worlds.
    persistent_gpu_scene: crate::PersistentGpuScene,
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
    instancing: Instancing,
    skinning: Skinning,
    /// The adaptive-tessellation prep subsystem (factor/scan/finalize/args descriptor infra); records
    /// the Phase-3 prep passes in the deform scope and emits the amplified transient geometry every
    /// raster + RT consumer reads for a displaced mesh.
    tessellation: Tessellation,
    transient: RenderGraphResources,
    pipelines: Pipelines,
    ibl: Ibl,
    /// A second IBL baked once to the fixed procedural preview environment, bound only when the
    /// active view is [`ViewId::Thumbnail`]. It isolates the background thumbnail render's
    /// procedural lighting from the project `ibl`, so draining the thumbnail queue while the scene
    /// is live never thrashes the project environment bake against the preview environment.
    preview_ibl: Ibl,
    sky: Sky,
    stars: crate::StarCatalog,
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

    /// The 1×1 `(0, 0)` default min/max pyramid seeded into every slot of the bindless
    /// `heightMinMaxTextures` array (binding 4). A non-displacement texture's slot keeps this
    /// (zero local range → no extra tessellation refinement); held here so the seeded view stays
    /// valid for the renderer's lifetime.
    default_height_minmax: crate::resources::DefaultHeightMinMax,

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

fn chromatic_light(radiance: Vec3, trim: f32) -> (Vec3, f32) {
    let luminance = radiance.dot(Vec3::new(0.2126, 0.7152, 0.0722));
    if luminance <= f32::EPSILON {
        (Vec3::ZERO, 0.0)
    } else {
        (radiance / luminance, luminance * trim.max(0.0))
    }
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
            Lighting,
            Pipelines,
            Instancing,
            Skinning,
            Tessellation,
            RenderGraphResources,
            crate::GlobalGpuData,
            crate::GpuSceneUploader,
            crate::PageResidency,
            crate::Hzb,
            crate::SceneVisibility,
            crate::PersistentGpuScene,
            Ibl,
            Ibl,
            Sky,
            crate::StarCatalog,
            ReflectionProbes,
            Ssao,
            crate::Ddgi,
            crate::GlobalSdf,
            crate::Rt,
            crate::Restir,
            crate::FroxelFog,
            crate::AerialPerspective,
            crate::Clouds,
            Vec<ViewTarget>,
            BindlessFreeList,
            crate::Aa,
            Arc<crate::GpuTexture>,
            Arc<crate::GpuSdf>,
            crate::resources::DefaultHeightMinMax,
            Arc<crate::GpuLut>,
            crate::vsm::VsmGpu,
            crate::VsmDemand,
        );
        let build = || -> Result<BuildParts> {
            let free_list: BindlessFreeList = Arc::new(Mutex::new(Vec::new()));
            let descriptors = Arc::new(Descriptors::new(&device, &free_list)?);

            // The default white texture takes slot 0 (the first claim) and is seeded
            // into every other bindless slot, so any untextured material samples a valid
            // descriptor. Uploaded through a one-off uploader on the graphics queue.
            let queue = device.graphics_queue.clone();
            let uploader = crate::Uploader::new(&device, &queue)?;
            let default_white = uploader.upload_default_white(&descriptors)?;
            // The bindless SDF array (binding 1) is partially bound; seed every slot with a
            // 1×1×1 "empty space" field so an unbound slot is never sampled by the
            // sky-occlusion cone-trace (a lavapipe fault / UB on real hardware). Real
            // per-mesh SDFs overwrite their own slot as their `GpuMesh` is built.
            let default_sdf = uploader.upload_default_sdf(&descriptors)?;
            // The per-height min/max pyramid array (binding 4) is partially bound; seed every slot
            // with a 1×1 `(0, 0)` default so a non-displacement texture's slot is a valid descriptor
            // (unbound slots fault on lavapipe / are UB on real hardware). A displacement height map
            // overwrites its own slot with its real pyramid at upload.
            let default_height_minmax = uploader.upload_default_height_minmax(&descriptors)?;
            // The neutral identity creative LUT (a 2×2×2 ramp), the always-bound default at binding 2
            // of every view's tonemap set so the tonemap shader never samples an unbound descriptor and
            // never branches on look presence (intensity 0 is the neutral).
            let default_lut = uploader.upload_identity_lut()?;

            let vsm_gpu = crate::vsm::VsmGpu::new(&device)?;
            let vsm_demand = crate::VsmDemand::new(&device, &descriptors)?;
            let lighting = Lighting::new(&device, &descriptors, vsm_gpu.atlas.view())?;
            let pipelines = Pipelines::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1);
            let instancing = Instancing::new(&device, &descriptors)?;
            let skinning = Skinning::new(&device)?;
            let tessellation = Tessellation::new(&device)?;
            let transient = RenderGraphResources::new(device.resources().clone());
            let global_gpu_data = crate::GlobalGpuData::new(&device)?;
            let gpu_scene_uploader = crate::GpuSceneUploader::new(&device)?;
            let page_residency = crate::PageResidency::new(crate::PageResidencyBudgets::default());
            let hzb = crate::Hzb::new(&device)?;
            let scene_visibility = crate::SceneVisibility::new(&device)?;
            for frame in 0..crate::MAX_FRAMES_IN_FLIGHT {
                descriptors.write_uniform_buffer_at(
                    instancing.instance_set(frame),
                    3,
                    gpu_scene_uploader.address_buffer(),
                    frame as u64 * gpu_scene_uploader.address_block_stride(),
                    size_of::<crate::GpuSceneAddressBlock>() as u64,
                );
            }
            let mut persistent_gpu_scene =
                crate::PersistentGpuScene::new(crate::GpuSceneUploadLimits::default())?;
            for view in [ViewId::Scene, ViewId::AssetPreview, ViewId::Thumbnail] {
                persistent_gpu_scene.create_world(view.gpu_scene_world())?;
                persistent_gpu_scene.create_view(view.gpu_scene_view(), view.gpu_scene_world())?;
            }

            // IBL: the cubes + LUT sampler + set 3, then the first (procedural) bake so set
            // 3 is valid before the first frame. The sky reuses the env cube; the reflection
            // probes ride the IBL set, seeded with the global cubes after the bake.
            let mut ibl = Ibl::new(&device, &descriptors)?;
            ibl.bake(&device, true)?;
            let stars = crate::StarCatalog::new(
                &device,
                &descriptors,
                &uploader,
                ibl.transmittance_view(),
                ibl.sky_view_lut_view(),
                ibl.sampler(),
                vk::SampleCountFlags::TYPE_1,
            )?;
            let mut sky = Sky::new(&device, &descriptors, vk::SampleCountFlags::TYPE_1)?;
            sky.bind_env_cube(&ibl);
            sky.bind_night_sky(&ibl, &stars);
            let reflection = ReflectionProbes::new(&device)?;
            reflection.seed(&ibl);

            // The offscreen thumbnail preview IBL: a second set validated with the global procedural
            // bake now (so set 3 is bindable) and re-baked to the actual fixed preview environment on
            // the first thumbnail render (routed there by `request_env_bake` on the Thumbnail view).
            // Its probe bindings ride the same fallback seeding — thumbnails never capture probes.
            let mut preview_ibl = Ibl::new(&device, &descriptors)?;
            preview_ibl.bake(&device, true)?;
            reflection.seed(&preview_ibl);

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
            ddgi.bind_sky_sh(ibl.sh_coefficients());
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

            // The froxel volumetric-fog volumes + compute sets. Created eagerly so the composite's
            // integration sampler (fog set binding 4) is always bound — the height-fog shader
            // statically references it even in analytic mode.
            let froxel = crate::FroxelFog::new(&device)?;

            // The Hillaire-2020 aerial-perspective volume. Created eagerly so the composite's AP
            // sampler (fog set binding 5) is always bound; its LUT bindings point at the reused
            // atmosphere LUT images so they persist across bakes (the fill is gated on a live atmosphere).
            let aerial = crate::AerialPerspective::new(&device)?;
            aerial.bind_luts(
                &device,
                ibl.sampler(),
                ibl.transmittance_view(),
                ibl.multi_scatter_view(),
            );

            // Static cloud noise is authored once by GPU compute; the weather map remains a single
            // persistent image refilled only when its authoring inputs change.
            let clouds = crate::Clouds::new(&device, &pipelines, &ibl, VIEW_COUNT)?;
            lighting.bind_cloud_shadow(
                &device,
                clouds.cloud_shadow().view(),
                clouds.shadow_sampler(),
            );

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
                // Bind the identity creative LUT at binding 2 of the tonemap set. The set is allocated
                // once per view and never reallocated (resizes rewrite bindings 0/1 only), so this write
                // persists; only assigning a creative look rewrites binding 2 (idled, `set_creative_lut`).
                view.write_tonemap_lut(&device, descriptors.linear_sampler(), default_lut.view());
                // Bind the atmosphere sky-view LUT at binding 3 of the fog set. The LUT image is
                // allocated once and reused across bakes, and the fog set is never reallocated, so
                // this write persists across resizes (the fog pass gates its use by `useSkyLut`).
                view.write_fog_sky_lut(&device, ibl.sampler(), ibl.sky_view_lut_view());
                // Bind the froxel integration volume at binding 4 of the fog set (the volumetric
                // composite sample). Rewritten by `set_fog` when a quality switch reallocates it.
                view.write_fog_integration(&device, froxel.sampler(), froxel.integration_view());
                // Bind the aerial-perspective volume at binding 5 of the fog set (the AP composite
                // sample). The volume is fixed-size and never reallocated, so this write persists.
                view.write_fog_aerial(&device, aerial.sampler(), aerial.volume_view());
                view.write_fog_atmosphere_luts(
                    &device,
                    ibl.sampler(),
                    ibl.transmittance_view(),
                    ibl.multi_scatter_view(),
                );
                // The AA targets (motion / history / scratch / MSAA), built after the
                // screen-space chain (it reads the SSGI maps).
                view.build_aa_targets(&device, &descriptors, aa)?;
                view.restir.allocate_sets(&descriptors, &restir)?;
                view.restir
                    .build(&device, &descriptors, &restir, view.scaled_render_extent())?;
                views.push(view);
            }
            for (index, view) in views.iter().enumerate() {
                clouds.bind_view(
                    index,
                    crate::clouds::CloudViewBindings {
                        color: view.offscreen.view(),
                        depth: view.depth.view(),
                        motion: view.motion.as_ref().expect("cloud motion built").view(),
                        reduced: [
                            view.cloud_reduced[0]
                                .as_ref()
                                .expect("cloud reduced 0 built")
                                .view(),
                            view.cloud_reduced[1]
                                .as_ref()
                                .expect("cloud reduced 1 built")
                                .view(),
                        ],
                        reduced_depth: view
                            .cloud_reduced_depth
                            .as_ref()
                            .expect("cloud reduced depth built")
                            .view(),
                        full_color: view
                            .cloud_full_color
                            .as_ref()
                            .expect("cloud full color built")
                            .view(),
                        full_depth: view
                            .cloud_full_depth
                            .as_ref()
                            .expect("cloud full depth built")
                            .view(),
                    },
                );
            }
            ssao.ready = true;
            Ok((
                descriptors,
                lighting,
                pipelines,
                instancing,
                skinning,
                tessellation,
                transient,
                global_gpu_data,
                gpu_scene_uploader,
                page_residency,
                hzb,
                scene_visibility,
                persistent_gpu_scene,
                ibl,
                preview_ibl,
                sky,
                stars,
                reflection,
                ssao,
                ddgi,
                global_sdf,
                rt,
                restir,
                froxel,
                aerial,
                clouds,
                views,
                free_list,
                aa,
                default_white,
                default_sdf,
                default_height_minmax,
                default_lut,
                vsm_gpu,
                vsm_demand,
            ))
        };
        let (
            descriptors,
            lighting,
            pipelines,
            instancing,
            skinning,
            tessellation,
            transient,
            global_gpu_data,
            gpu_scene_uploader,
            page_residency,
            hzb,
            scene_visibility,
            persistent_gpu_scene,
            ibl,
            preview_ibl,
            sky,
            stars,
            reflection,
            ssao,
            ddgi,
            global_sdf,
            rt,
            restir,
            froxel,
            aerial,
            clouds,
            views,
            bindless_free_list,
            aa,
            default_white,
            default_sdf,
            default_height_minmax,
            default_lut,
            vsm_gpu,
            vsm_demand,
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
        // Wire the froxel integration volume into binding 11 of every light set (set 1) so the
        // forward transparent path samples the same volumetric fog the composite applies to opaque.
        // Persistent (the volume is fixed-size, never reallocated) — a one-time wire-up.
        lighting.bind_froxel_integration(&device, froxel.integration_view(), froxel.sampler());
        // Wire the GDF lite albedo cache (+ its repeat sampler) into the DDGI trace set (set 2,
        // binding 0); the trace reads it as the hit radiance's per-cell base color. Persistent.
        ddgi.bind_gdf_albedo(&global_sdf);

        let mut renderer = Self {
            clear_color: [0.05, 0.06, 0.08, 1.0],
            wireframe: false,
            use_depth_prepass: true,
            exposure_ev: 0.0,
            night_factor: 0.0,
            bloom_enabled: false,
            bloom_intensity: 0.05,
            bloom_scatter: 0.005,
            bloom_tint: [1.0, 1.0, 1.0],
            bloom_threshold: 0.0,
            bloom_dirt_texture: None,
            bloom_dirt_texture_id: 0,
            bloom_dirt_intensity: 0.0,
            bloom_dirt_tint: [1.0, 1.0, 1.0],
            bloom_anamorphic_enabled: false,
            bloom_anamorphic_ratio: 2.0,
            bloom_anamorphic_tint: [0.6, 0.8, 1.0],
            bloom_anamorphic_intensity: 0.0,
            bloom_mip_tint: Vec::new(),
            fog: FogRenderSettings::default(),
            froxel,
            aerial,
            clouds,
            fog_volumes: Vec::new(),
            fog_time: 0.0,
            sun_direction: Vec3::new(0.0, -1.0, 0.0),
            sun_color: Vec3::ONE,
            sun_intensity: 0.0,
            moon_direction: Vec3::Y,
            moon_color: Vec3::ZERO,
            moon_intensity: 0.0,
            show_grid: false,
            present_viewport_only: false,
            frame_begun: false,
            view_mode: ViewMode::Lit,
            skinning_enabled: true,
            displacement_enabled: true,
            tess_factor_cap: crate::tessellation::TESS_DEFAULT_FACTOR_CAP,
            tess_min_factor: crate::tessellation::TESS_DEFAULT_MIN_FACTOR,
            tess_edge_length_target: crate::tessellation::TESS_DEFAULT_EDGE_LENGTH_TARGET,
            software_gpu,
            frame_ms: 0.0,
            cpu_frame_ms: 0.0,
            scene_gather_ms: 0.0,
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
            last_frame_ns: 0,
            capture: CaptureRecorder::default(),
            device_name,
            overlay,
            submissions: Vec::new(),
            scene_draw_list: SceneDrawList::default(),
            render_quality: RenderQuality::default(),
            budget_controller: BudgetController::new(),
            pending_render_scale: None,
            tonemap_mode: TonemapMode::default(),
            color_grade: ColorGrade::default(),
            default_lut,
            creative_lut: None,
            creative_lut_id: 0,
            creative_lut_intensity: 0.0,
            creative_lut_size: 2,
            reactive: ReactiveState::default(),
            stats: RenderStats::default(),
            aa,
            taa_params: crate::TaaParams::default(),
            camera_near_far: (0.1, 100.0),
            cluster_camera: ClusterCamera {
                view: Mat4::IDENTITY,
                projection: Mat4::IDENTITY,
                width,
                height,
                near: 0.1,
                far: 100.0,
            },
            views,
            global_gpu_data,
            gpu_scene_uploader,
            wind_deform_records: std::collections::HashMap::new(),
            scene_wind: SceneWind::default(),
            vsm_gpu,
            vsm_residency: crate::VsmResidency::default(),
            vsm_views: (0..crate::VSM_DIRECTIONAL_LEVELS + 1 + crate::vsm::VSM_POINT_FACES)
                .map(|_| None)
                .collect(),
            vsm_render_pages: Vec::new(),
            vsm_space: crate::VsmDirectionalSpace::build(
                saffron_geometry::glam::Vec3::NEG_Y,
                saffron_geometry::glam::Vec3::ZERO,
            ),
            vsm_demand,
            vsm_demanded: Vec::new(),
            vsm_spot_matrix: [0.0; 16],
            vsm_point_key: [0.0; 4],
            interaction_fields: std::collections::HashMap::new(),
            interaction_impulses: Vec::new(),
            interaction_impulse_ring: Vec::new(),
            wind_source_ring: Vec::new(),
            page_residency,
            hzb,
            scene_visibility,
            live_executor_bins: Vec::new(),
            live_draw_record_bound: 0,
            visibility_counters: [0; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize],
            page_faults: 0,
            pending_gpu_scene_uploads: crate::GpuScenePendingUploads::default(),
            micro_field_directory: None,
            last_gpu_scene_upload: crate::GpuSceneUploadRunStats::default(),
            persistent_gpu_scene,
            active_view: ViewId::Scene,
            shm_publish_enabled: [false; VIEW_COUNT],
            pending_shm_publish: None,
            lighting,
            instancing,
            skinning,
            tessellation,
            transient,
            pipelines,
            ibl,
            preview_ibl,
            sky,
            stars,
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
            default_height_minmax,
            capture_next_window_path: None,
            frames,
            swapchain,
            present_sync,
            device,
        };
        // Seed the shared micro-blade template's index block; it drains with the
        // first frame's pending uploads.
        renderer
            .pending_gpu_scene_uploads
            .upload_arena(crate::GpuArenaUploadRequest::PageBytes {
                range: renderer.global_gpu_data.micro_blade_template,
                data: crate::micro_blade_template_indices()
                    .iter()
                    .flat_map(|index| index.to_le_bytes())
                    .collect(),
            });
        Ok(renderer)
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

    /// Device-global geometry arenas and immutable metadata tables.
    pub fn global_gpu_data(&self) -> &crate::GlobalGpuData {
        &self.global_gpu_data
    }

    /// Mutable device-global tables used by the asset delta adapter.
    pub fn global_gpu_data_mut(&mut self) -> &mut crate::GlobalGpuData {
        &mut self.global_gpu_data
    }

    /// Descriptor-ready immutable-table bindings for visibility and draw executors.
    pub fn global_gpu_table_descriptors(&self) -> crate::GlobalGpuTableDescriptors {
        self.global_gpu_data.table_descriptors(&self.device)
    }

    /// Sole persistent renderer-derived scene mirror.
    pub fn persistent_gpu_scene(&self) -> &crate::PersistentGpuScene {
        &self.persistent_gpu_scene
    }

    /// Mutable scene mirror used by typed world and asset delta adapters.
    pub fn persistent_gpu_scene_mut(&mut self) -> &mut crate::PersistentGpuScene {
        &mut self.persistent_gpu_scene
    }

    /// The latest fence-completed visibility counters for the active view:
    /// `[visible, retest, list overflow flags, records, record/bucket pressure,
    /// transparent, _, _]`.
    pub fn visibility_counters(&self) -> [u32; crate::SCENE_VISIBILITY_COUNTER_WORDS as usize] {
        self.visibility_counters
    }

    /// GPU missing-page requests drained since startup (the page-fault total).
    pub fn page_faults(&self) -> u64 {
        self.page_faults
    }

    /// Replaces the populated executor bin set (the mirror pushes it per sync).
    pub fn set_live_executor_bins(&mut self, bins: Vec<(u32, u32)>) {
        self.live_executor_bins = bins;
    }

    /// Publishes the mirror's upper bound on emitted draw records, which bounds the fixed-slice
    /// indirect draws on a device without `drawIndirectCount`.
    pub fn set_live_draw_record_bound(&mut self, bound: u32) {
        self.live_draw_record_bound = bound;
    }

    /// The page-payload residency counters (registered/resident/bytes/evictions).
    pub fn page_residency_stats(&self) -> crate::PageResidencyStats {
        self.page_residency.stats()
    }

    /// The active view's page-demand context: the eye for projected error, the
    /// projection scale (pixels per metre at unit distance), and the view-projection
    /// for frustum visibility probability.
    pub fn page_demand_view(&self) -> crate::PageDemandView {
        let view = self.ssao.view();
        let inv_projection = self.ssao.inv_projection();
        let extent = self.views[self.active_view.index()].scaled_render_extent();
        let inv_scale = inv_projection.col(1).y;
        let proj_scale = if inv_scale.abs() > f32::EPSILON {
            (1.0 / inv_scale).abs() * extent.height as f32 * 0.5
        } else {
            0.0
        };
        crate::PageDemandView {
            eye: view.inverse().col(3).truncate(),
            proj_scale,
            view_proj: inv_projection.inverse() * view,
        }
    }

    /// The GPU-scene halves the delta adapter writes in one borrow: device tables for
    /// record inserts, the persistent mirror for typed deltas, the pending-upload queue
    /// for staged bytes, retirements, and arena data, and the page-residency authority.
    pub fn gpu_scene_parts_mut(
        &mut self,
    ) -> (
        &mut crate::GlobalGpuData,
        &mut crate::PersistentGpuScene,
        &mut crate::GpuScenePendingUploads,
        &mut crate::PageResidency,
    ) {
        (
            &mut self.global_gpu_data,
            &mut self.persistent_gpu_scene,
            &mut self.pending_gpu_scene_uploads,
            &mut self.page_residency,
        )
    }

    /// The persistent GPU scene's device tables and upload translation.
    pub fn gpu_scene_uploader(&self) -> &crate::GpuSceneUploader {
        &self.gpu_scene_uploader
    }

    /// Publishes the resident micro-field tile directory (byte offset within the
    /// fields arena + entry count) the micro reconstruction pass dispatches over,
    /// or `None` while no field tiles are resident.
    pub fn set_micro_field_directory(&mut self, directory: Option<(u32, u32)>) {
        self.micro_field_directory = directory;
    }

    /// The most recent frame's GPU-scene upload-translation counters.
    pub fn gpu_scene_upload_stats(&self) -> crate::GpuSceneUploadRunStats {
        self.last_gpu_scene_upload
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

    /// The 1×1 `(0, 0)` default min/max pyramid seeded into every slot of the bindless
    /// `heightMinMaxTextures` array (binding 4) — a non-displacement texture's slot keeps it (zero
    /// local range → no extra tessellation refinement).
    pub fn default_height_minmax(&self) -> &crate::resources::DefaultHeightMinMax {
        &self.default_height_minmax
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

    /// The active view's GPU-scene world identity.
    pub fn active_gpu_scene_world(&self) -> crate::GpuSceneWorldId {
        self.active_view.gpu_scene_world()
    }

    /// The record-driven deformation frame: uploads the palettes and wires the
    /// skin/morph/tessellation work for `work` (the scene driver's per-entity
    /// deformation facts) without any draw list. The gathered outputs land on the
    /// frame's [`SceneDrawList`] deformation fields (dispatches, RT entries, tess
    /// buckets, provider-patch facts); the draw batches stay untouched.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] on buffer growth or dispatch wiring failure.
    pub fn submit_gpu_scene_deformations(
        &mut self,
        view_proj: Mat4,
        work: &[crate::DeformationWork],
        joints: &[Mat4],
    ) -> Result<()> {
        let frame = self.frames.index();
        let mut gather = crate::DeformationGather::default();
        let mut prev_joints: Vec<Mat4> = joints.to_vec();
        let params = crate::TessGatherParams {
            rt_skinned: self.rt.use_rt_shadows() || self.rt.use_rt_reflections(),
            factor_cap: self.tess_factor_cap,
            min_factor: self.tess_min_factor,
            edge_length_target: self.tess_edge_length_target,
        };
        // A displaced item's tess bucket links to its instance row via `base_instance`
        // (row `i` = the i-th displaced item, matching `upload_tess_instance_rows`).
        let mut displaced_row = 0u32;
        for item in work {
            let base_instance = if item.displace.is_some() {
                let row = displaced_row;
                displaced_row += 1;
                row
            } else {
                0
            };
            crate::gather_instance_deformation(
                &mut gather,
                &mut self.skinning,
                item,
                joints,
                &mut prev_joints,
                params,
                base_instance,
            );
        }
        let mut list = SceneDrawList {
            view_proj,
            ..SceneDrawList::default()
        };
        self.instancing.wire_gathered_deformations(
            &self.descriptors,
            &mut self.skinning,
            frame,
            gather,
            joints,
            prev_joints,
            &mut list,
        )?;
        // The tessellation seam: one instance row + mesh PSO per displaced item; the
        // tess prep resolves each draw's amplified VB/IB/args once the frame's
        // transients exist.
        let displaced: Vec<&crate::DeformationWork> =
            work.iter().filter(|item| item.displace.is_some()).collect();
        if !displaced.is_empty() {
            let rows: Vec<(u64, Mat4, &[crate::SubmeshMaterial], u32)> = displaced
                .iter()
                .map(|item| {
                    (
                        item.entity,
                        item.model,
                        item.submesh_materials.as_slice(),
                        item.parameter_index,
                    )
                })
                .collect();
            let phase = self.views[self.active_view.index()].jitter_index;
            self.instancing.upload_tess_instance_rows(
                &self.descriptors,
                &mut self.skinning,
                frame,
                &rows,
                crate::DEFAULT_WHITE_SLOT,
                phase,
                &mut list,
            )?;
            for (row, item) in displaced.iter().enumerate() {
                let Some(pso) =
                    self.pipelines
                        .request_mesh_pipeline(&item.material, false, self.wireframe)
                else {
                    continue;
                };
                let slot0 = item.submesh_materials.first();
                list.tess_draws.push(crate::TessSceneDraw {
                    pso,
                    base_instance: row as u32,
                    cull: if slot0.is_some_and(|material| material.double_sided) {
                        vk::CullModeFlags::NONE
                    } else {
                        vk::CullModeFlags::BACK
                    },
                    blend: slot0.is_some_and(|material| {
                        material.blend_mode == saffron_core::BlendMode::Blend
                    }),
                    draw: None,
                });
            }
        }
        list.valid = true;
        self.scene_draw_list = list;
        Ok(())
    }

    /// Records the retained-mesh host-byte figure the mirror reports (render stats).
    pub fn record_retained_mesh_bytes(&mut self, bytes: u64) {
        self.stats.retained_mesh_cpu_bytes = bytes;
    }

    /// The submitted frame's skinned palette/deformed offsets (the provider-params
    /// patch input).
    pub fn skinned_deformations(&self) -> &[crate::SkinnedDeformation] {
        &self.scene_draw_list.skinned_deformations
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

    /// The most recent frame's draw counters (derived from the visibility readback).
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

    /// Folds the scene's cloud-shape authoring into the persistent density resources. Weather-map
    /// inputs mark the single weather image dirty; all other shape values update the shared UBO.
    pub fn submit_clouds(&mut self, settings: crate::CloudRenderSettings) {
        self.clouds.submit(settings);
    }

    /// Folds this frame's analytic height/distance fog in. Resolved from `scene.environment.fog`
    /// each frame (the same shape as [`Renderer::submit_sky`]); the composite pass runs before the
    /// bloom pyramid only while `settings.enabled`.
    pub fn set_fog(&mut self, settings: &FogRenderSettings) {
        self.fog = *settings;
        // Switch the froxel-grid quality tier when it changed: reallocate the ping-pong history +
        // integration volumes, then rebind BOTH consumers of the integration volume to the new view —
        // every view's composite fog set (binding 4) and every light set's binding 11 (the forward
        // mesh/transparent path samples the same volume). The history reset rides inside `set_quality`.
        // A no-op when the tier is unchanged.
        match self.froxel.set_quality(&self.device, settings.quality) {
            Ok(true) => {
                let sampler = self.froxel.sampler();
                let view = self.froxel.integration_view();
                for v in &mut self.views {
                    v.write_fog_integration(&self.device, sampler, view);
                }
                self.lighting
                    .bind_froxel_integration(&self.device, view, sampler);
            }
            Ok(false) => {}
            Err(err) => tracing::error!("froxel fog set_quality: {err}"),
        }
    }

    /// The IBL the scene pass binds for the active view: the fixed procedural [`Renderer::preview_ibl`]
    /// on the offscreen [`ViewId::Thumbnail`] view, else the project [`Renderer::ibl`].
    fn scene_ibl(&self) -> &Ibl {
        if self.active_view == ViewId::Thumbnail {
            &self.preview_ibl
        } else {
            &self.ibl
        }
    }

    /// The mutable twin of [`Renderer::scene_ibl`] — routes an environment bake to the preview IBL
    /// while a thumbnail renders, so the project IBL is never touched by a thumbnail's env sync.
    fn scene_ibl_mut(&mut self) -> &mut Ibl {
        if self.active_view == ViewId::Thumbnail {
            &mut self.preview_ibl
        } else {
            &mut self.ibl
        }
    }

    /// Whether the active view's environment bake and derived lighting capture are complete.
    /// Synchronous thumbnail readback uses this to avoid capturing a partially refreshed IBL.
    pub fn active_environment_converged(&self) -> bool {
        self.scene_ibl().dynamic_lighting_converged()
    }

    /// Re-arms the IBL environment bake when the source / panorama / params change.
    /// The bake fires at the next [`Renderer::render_scene_offscreen`]
    /// (a GPU-idle point), so the visible sky + IBL relight together. Routed to the active view's
    /// IBL, so a thumbnail render's procedural env re-arms [`Renderer::preview_ibl`], not the
    /// project IBL.
    pub fn request_env_bake(
        &mut self,
        source: EnvSource,
        panorama: Option<Arc<crate::GpuTexture>>,
        params: SkygenParams,
    ) {
        self.scene_ibl_mut()
            .request_env_bake(source, panorama, params);
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

    /// Folds this frame's local fog volumes in: bakes each [`crate::FogVolumeUpload`] into its std430
    /// record for the inject pass to loop (capped at [`crate::MAX_FOG_VOLUMES`]). Resolved from the
    /// scene each frame, like [`Renderer::submit_reflection_probes`].
    pub fn submit_fog_volumes(&mut self, volumes: &[crate::FogVolumeUpload]) {
        let cap = crate::MAX_FOG_VOLUMES as usize;
        self.fog_volumes.clear();
        self.fog_volumes.extend(
            volumes
                .iter()
                .take(cap)
                .map(crate::FogVolumeGpu::from_upload),
        );
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

    /// The scene-linear color grade folded into the tonemap pass.
    pub fn color_grading(&self) -> ColorGrade {
        self.color_grade
    }

    /// Sets the scene-linear color grade (applied in the tonemap pass next frame).
    pub fn set_color_grading(&mut self, grade: ColorGrade) {
        self.color_grade = grade;
    }

    /// The assigned creative-LUT read-back: `(asset id, size, intensity)`, or `None` when no look is
    /// assigned (id `0`).
    pub fn creative_lut(&self) -> Option<(u64, u32, f32)> {
        (self.creative_lut_id != 0).then_some((
            self.creative_lut_id,
            self.creative_lut_size,
            self.creative_lut_intensity,
        ))
    }

    /// Assigns the display-space creative look-up table and its look intensity. `lut` is the resolved
    /// GPU table (the host looks the asset up in the catalog) or `None` to clear to the identity
    /// default; `id` is the asset id (`0` clears). The intensity rides the grade UBO (a per-frame
    /// write, cheap on a drag); only an asset *change* rebinds the descriptor — idled, so the
    /// tonemap set is never rewritten while an earlier frame that bound it is in flight.
    pub fn set_creative_lut_texture(
        &mut self,
        id: u64,
        lut: Option<Arc<crate::GpuLut>>,
        intensity: f32,
    ) {
        self.creative_lut_intensity = intensity.clamp(0.0, 1.0);
        if id == self.creative_lut_id {
            return;
        }
        self.creative_lut_id = id;
        self.creative_lut = lut;
        self.creative_lut_size = self.creative_lut.as_ref().map_or(2, |l| l.size());
        let _ = self.device.wait_idle();
        let view = self
            .creative_lut
            .as_ref()
            .map_or_else(|| self.default_lut.view(), |l| l.view());
        let sampler = self.descriptors.linear_sampler();
        for v in &self.views {
            v.write_tonemap_lut(&self.device, sampler, view);
        }
    }

    /// Bakes the folded look — the current grade + view transform + creative LUT — into one
    /// `33³` display-referred table over the log2 shaper (EV `[-14, +11]`), on the GPU, reusing the
    /// same shared helpers as the live tonemap pass. Returns `(size, ev_min, ev_max, rgb)` where `rgb`
    /// is red-fastest `[r, g, b]` f16 bits the host serializes into a `.slut`. Idles around the one-off
    /// dispatch (the bake is a rare, explicit `bake-look`).
    ///
    /// # Errors
    ///
    /// [`crate::Error`] if the bake PSO is unavailable or a Vulkan/VMA call fails.
    pub fn bake_look_lut(
        &mut self,
        uploader: &crate::Uploader,
    ) -> Result<(u32, f32, f32, Vec<[u16; 3]>)> {
        let pipeline = self
            .pipelines
            .request_lut_bake()
            .ok_or_else(|| crate::Error::LutBake("lut_bake PSO unavailable".to_owned()))?;
        let grade = GradeUniform::from(&self.color_grade).with_look(
            self.creative_lut_intensity,
            self.creative_lut_size,
            false,
        );
        let view = self
            .creative_lut
            .as_ref()
            .map_or_else(|| self.default_lut.view(), |l| l.view());
        let sampler = self.descriptors.linear_sampler();
        let mode = self.tonemap_mode as u32;
        self.device.wait_idle()?;
        let rgb = uploader.bake_look_lut(
            &self.descriptors,
            &pipeline,
            &grade,
            view,
            sampler,
            crate::LUT_BAKE_SIZE,
            mode,
        )?;
        Ok((
            crate::LUT_BAKE_SIZE,
            crate::LUT_SHAPER_EV_MIN,
            crate::LUT_SHAPER_EV_MAX,
            rgb,
        ))
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
        let ibl = self.scene_ibl();
        let ibl_enabled = ibl.use_ibl && ibl.ready;
        ibl_enabled && self.sky_occlusion_enabled() && self.global_sdf.enabled()
    }

    /// Snaps the camera-centered DDGI probe clipmap to the camera + stores the sun for the
    /// trace. Call before [`Renderer::set_scene_lighting`], which folds the volume + probe grid +
    /// toroidal scroll base into the light UBO.
    pub fn set_ddgi_scene(
        &mut self,
        cam_pos: saffron_geometry::glam::Vec3,
        sun_dir: saffron_geometry::glam::Vec3,
        sun_color: saffron_geometry::glam::Vec3,
        sun_intensity: f32,
    ) {
        self.ddgi
            .set_scene(cam_pos, sun_dir, sun_color, sun_intensity);
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
        self.global_sdf.prepare_frame_regions();
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
        self.prepare_vsm_frame(frame, scene.direction);
        let mut scene = scene.clone();
        if self.scene_ibl().atmosphere_live() {
            let atmosphere = self.scene_ibl().baked_atmosphere();
            let sun_to_light = -scene.direction.normalize_or_zero();
            let sun_radiance = sun_transmittance(&atmosphere, sun_to_light) * SOLAR_ILLUMINANCE_TOA;
            (scene.color, scene.intensity) = chromatic_light(sun_radiance, scene.intensity);

            let moon_to_light = -scene.moon_direction.normalize_or_zero();
            let phase = (1.0 - sun_to_light.dot(moon_to_light).clamp(-1.0, 1.0)) * 0.5;
            let moon_radiance =
                sun_transmittance(&atmosphere, moon_to_light) * (LUNAR_ILLUMINANCE_FULL * phase);
            (scene.moon_color, scene.moon_intensity) =
                chromatic_light(moon_radiance, scene.moon_intensity);
        }
        // Capture the directional-light travel direction for the fog pass's sun-inscatter lobe (it
        // points its lobe toward the sun, i.e. `-direction`).
        self.sun_direction = scene.direction.normalize_or_zero();
        self.sun_color = scene.color;
        self.sun_intensity = scene.intensity;
        self.moon_direction = scene.moon_direction.normalize_or_zero();
        self.moon_color = scene.moon_color;
        self.moon_intensity = scene.moon_intensity;
        let cloud_shadow = self.clouds.shadow_projection(
            scene.eye_position,
            -self.sun_direction,
            self.sun_intensity,
            -self.moon_direction,
            self.moon_intensity,
        );
        self.lighting.set_frame_cloud_shadow(cloud_shadow);
        // Fold the IBL-ambient flag + the reflection-probe count into the UBO write.
        // Probes contribute only when IBL is baked + their toggle is on.
        let ibl = self.scene_ibl();
        let ibl_enabled = ibl.use_ibl && ibl.ready;
        let probes_on = self.reflection.use_probes && ibl.ready;
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
        // Fold this frame's froxel volumetric-fog params so the forward transparent path samples the
        // integration volume only when volumetric fog is authored (matching the composite's gate).
        self.lighting.set_frame_froxel_fog(
            self.fog.enabled && self.fog.volumetric,
            crate::froxel_fog::FROXEL_NEAR,
            crate::FROXEL_FAR,
        );
        self.lighting
            .set_scene_lighting(&self.descriptors, frame, &scene)
    }

    /// Folds the shared wind field's frame parameters into the light UBO: the mean
    /// direction/speed/gust, the deterministic sampling parameters, and the monotonic
    /// simulation time (previous frame's time is retained for motion). Call once per
    /// frame before [`Renderer::set_scene_lighting`].
    /// The monotonically increasing frame serial (the traversal's `frameStamp`).
    pub fn frame_serial(&self) -> u64 {
        self.frame_serial
    }

    /// Stages world-space interaction impulses for this frame's field step; the
    /// staged list drains when the frame records.
    pub fn submit_interaction_impulses(&mut self, impulses: &[crate::InteractionImpulse]) {
        self.interaction_impulses.extend_from_slice(impulses);
    }

    /// Folds the frame's wind parameters and local sources into the light UBO
    /// words, the deformation pushes, and the source ring every GPU sampler reads.
    pub fn set_wind(
        &mut self,
        wind: &SceneWind,
        sources: &[saffron_wind::LocalWindSource],
    ) -> Result<()> {
        self.scene_wind = *wind;
        while self.wind_source_ring.len() < crate::MAX_FRAMES_IN_FLIGHT {
            self.wind_source_ring.push(crate::Buffer::new(
                self.device.resources(),
                64 * size_of::<crate::GpuWindSourceRecord>() as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }
        let frame = self.frames.index();
        let records: Vec<crate::GpuWindSourceRecord> = sources
            .iter()
            .take(64)
            .map(|source| crate::GpuWindSourceRecord {
                position: [
                    source.position.x as f32,
                    source.position.y as f32,
                    source.position.z as f32,
                ],
                kind: match source.kind {
                    saffron_wind::WindSourceKind::Directional => 0,
                    saffron_wind::WindSourceKind::Point => 1,
                    saffron_wind::WindSourceKind::Vortex => 2,
                    saffron_wind::WindSourceKind::Wake => 3,
                    saffron_wind::WindSourceKind::Volume => 4,
                },
                direction: source.direction.to_array(),
                strength: source.strength,
                radius: source.radius,
                falloff: source.falloff,
                reserved: [0.0; 2],
            })
            .collect();
        let ring = &self.wind_source_ring[frame];
        if !records.is_empty() {
            // SAFETY: HOST_VISIBLE + MAPPED; the frame slot's fence passed before reuse.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    records.as_ptr().cast::<u8>(),
                    ring.mapped_ptr(),
                    records.len() * size_of::<crate::GpuWindSourceRecord>(),
                );
            }
        }
        let sources_address = self.device.buffer_device_address(ring.handle());
        let radians = wind.orientation.to_radians();
        self.lighting.set_frame_wind(
            Vec4::new(radians.sin(), radians.cos(), wind.speed, wind.gust),
            Vec4::new(
                wind.turbulence_roughness,
                wind.gust_frequency,
                wind.reference_height,
                wind.height_exponent,
            ),
            saffron_geometry::glam::UVec4::new(
                wind.turbulence_octaves,
                wind.seed,
                records.len() as u32,
                0,
            ),
            wind.time_s as f32,
            sources_address,
        );
        Ok(())
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
        // Mirror the whole camera for the adaptive-tessellation factor metric.
        self.cluster_camera = camera;
        self.lighting.set_cluster_camera(frame, camera);
    }

    /// Records the Phase-3 adaptive-tessellation prep passes for this frame's displaced instances:
    /// **factor** (one fractional factor per unique base edge) → **scan** (predict + atomic-carry
    /// prefix-sum the exact dice output into packed per-instance offsets) → **finalize** (the indirect
    /// draw seed + RT prim count) → **args** (the global emit-dispatch size). Each writes a per-frame
    /// transient buffer; nothing consumes them yet (Phase 4 emit / Phase 6 raster / Phase 7 RT), so the
    /// pass chain is inert beyond its own scratch. No-op unless a displaced instance carries watertight
    /// conditioning and all four PSOs build.
    /// Records the adaptive-tessellation prep + emit passes for the frame's displaced instances. Returns
    /// the coarse RT VB/IB graph resources (Phase 10, Q2 secondary-ray coarsening) when a tessellated
    /// instance is RT-consumed this frame, so `begin_frame_graph` can declare them `AccelStructBuildRead`
    /// on the `tlas-build` pass (the emit→build barrier is then graph-derived); `None` otherwise.
    fn record_tess_prep(
        &mut self,
        graph: &mut crate::RenderGraph,
        frame: usize,
        raw: &ash::Device,
    ) -> Option<(RgResource, RgResource)> {
        if self.scene_draw_list.tess_buckets.is_empty() {
            return None;
        }

        // The per-instance inputs (Copy handles + owned params) lifted out of the draw list up front,
        // so the transient + pipeline borrows below never alias the draw-list borrow.
        struct Inst {
            welded: vk::Buffer,
            edges: vk::Buffer,
            tri_edges: vk::Buffer,
            base_vertices: vk::Buffer,
            base_indices: vk::Buffer,
            base_instance: u32,
            entity: u64,
            edge_count: u32,
            tri_count: u32,
            model: Mat4,
            factor_cap: f32,
            min_factor: f32,
            edge_length_target: f32,
            height_index: u32,
            height_scale: f32,
            vector_index: u32,
            uv_transform: [f32; 4],
        }
        let mut insts: Vec<Inst> = Vec::with_capacity(self.scene_draw_list.tess_buckets.len());
        for bucket in &self.scene_draw_list.tess_buckets {
            let Some(cond) = bucket.mesh.conditioning() else {
                continue;
            };
            insts.push(Inst {
                welded: cond.welded.0,
                edges: cond.edges.0,
                tri_edges: cond.tri_edges.0,
                base_vertices: bucket.mesh.vertex_buffer(),
                base_indices: bucket.mesh.index_buffer(),
                base_instance: bucket.base_instance,
                entity: bucket.entity,
                edge_count: cond.edge_count,
                tri_count: bucket.mesh.index_count / 3,
                model: bucket.model,
                factor_cap: bucket.factor_cap,
                min_factor: bucket.min_factor,
                edge_length_target: bucket.edge_length_target,
                height_index: bucket.height_index,
                height_scale: bucket.height_scale,
                vector_index: bucket.vector_index,
                uv_transform: bucket.uv_transform,
            });
        }
        if insts.is_empty() {
            return None;
        }

        // Hard triangle budget (Phase 10): coarsen each instance's factor cap so the *summed* worst-case
        // reservation fits `TESS_MICRO_VERTEX_BUDGET`. The reservation, scan, and emit all read the
        // adjusted `factor_cap` below, so the GPU can never write past the reserved transient arena even
        // under a dense scene — bounded VRAM without a per-instance manual cap.
        {
            let budget_input: Vec<(u32, f32, f32)> = insts
                .iter()
                .map(|i| (i.tri_count, i.factor_cap, i.min_factor))
                .collect();
            let scaled = crate::tessellation::budget_scaled_caps(
                &budget_input,
                crate::tessellation::TESS_MICRO_VERTEX_BUDGET,
            );
            for (inst, &cap) in insts.iter_mut().zip(&scaled) {
                inst.factor_cap = cap;
            }
        }

        // The tessellation camera, derived from the mirrored cluster camera (world position via the
        // inverse view; `tan(½fov)` from the projection's `y` scale).
        let view = self.cluster_camera.view;
        let proj = self.cluster_camera.projection;
        let cam = crate::TessCamera {
            view_proj: proj * view,
            cam_pos: view.inverse().w_axis.truncate(),
            viewport: [
                self.cluster_camera.width as f32,
                self.cluster_camera.height as f32,
            ],
            tan_half_fov_y: 1.0 / proj.y_axis.y.abs().max(1e-4),
            near: self.cluster_camera.near.max(1e-4),
        };

        // Per-instance placement (prefix sums) into the shared buffers, reserved worst-case at the cap.
        let mut layouts: Vec<crate::TessInstanceLayout> = Vec::with_capacity(insts.len());
        let (mut edge_cur, mut tri_cur, mut vb_cur, mut ib_cur) = (0u32, 0u32, 0u32, 0u32);
        for (row, inst) in insts.iter().enumerate() {
            let (verts, indices) =
                crate::tessellation::tess_worst_case(inst.tri_count, inst.factor_cap as u32);
            layouts.push(crate::TessInstanceLayout {
                factor_base: edge_cur,
                tri_base: tri_cur,
                instance_row: row as u32,
                vertex_base: vb_cur,
                index_base: ib_cur,
            });
            edge_cur += inst.edge_count;
            tri_cur += inst.tri_count;
            vb_cur = vb_cur.saturating_add(verts as u32);
            ib_cur = ib_cur.saturating_add(indices as u32);
        }
        let instance_rows = insts.len() as u32;

        // The Phase-5 temporal factor ping-pong (owned by `Tessellation`, not the rewound transient pool):
        // write this frame's factors into `slot[frame % 2]`, read last frame's from `slot[(frame + 1) % 2]`.
        // The prev slot serves the emit kernel's prev-position stream only when its surviving factors are
        // laid out identically to this frame's (the per-instance edge-count sequence); on frame 0 / a layout
        // change the prev binding falls back to the cur slot, so prev == cur ⇒ zero geomorph delta (no ghost).
        let edge_layout: Vec<u32> = insts.iter().map(|inst| inst.edge_count).collect();
        let cur_slot = frame % 2;
        let prev_slot = (frame + 1) % 2;
        let factors = match self.tessellation.ensure_factor_slot(cur_slot, edge_cur) {
            Ok(buffer) => buffer,
            Err(err) => {
                tracing::error!("tess prep: ensure factor slot {cur_slot}: {err}");
                return None;
            }
        };
        let prev_factors = if self
            .tessellation
            .factor_layout_matches(prev_slot, &edge_layout)
        {
            self.tessellation.factor_slot(prev_slot).unwrap_or(factors)
        } else {
            factors
        };

        // The four PSOs (set-0 layouts are Copy handles, so no `self.tessellation` borrow spans the
        // `&mut self.pipelines` request).
        let factor_layout = self.tessellation.factor_layout();
        let scan_layout = self.tessellation.scan_layout();
        let finalize_layout = self.tessellation.finalize_layout();
        let args_layout = self.tessellation.args_layout();
        let emit_layout = self.tessellation.emit_layout();
        let bindless_layout = self.descriptors.bindless_set_layout();
        let bindless_set = self.descriptors.bindless_set();
        let (Some(factor_pso), Some(scan_pso), Some(finalize_pso), Some(args_pso), Some(emit_pso)) = (
            self.pipelines
                .request_tess_factor(bindless_layout, factor_layout),
            self.pipelines.request_tess_scan(scan_layout),
            self.pipelines.request_tess_finalize(finalize_layout),
            self.pipelines.request_tess_args(args_layout),
            self.pipelines
                .request_tessellate(bindless_layout, emit_layout),
        ) else {
            return None;
        };

        // The per-frame transient scratch (grow-only, keyed). All are storage buffers; the indirect
        // seed + emit-args also carry `INDIRECT_BUFFER` for their Phase-4/6 consumers.
        let storage = vk::BufferUsageFlags::STORAGE_BUFFER;
        let indirect = storage | vk::BufferUsageFlags::INDIRECT_BUFFER;
        // Storage buffers additionally cleared each frame via `cmd_fill_buffer` (a transfer op) need
        // TRANSFER_DST: the scan's per-instance counters + global totals, and the emit's degenerate-pad
        // clear of the index stream.
        let storage_cleared = storage | vk::BufferUsageFlags::TRANSFER_DST;
        // The AS-build-input flag the RT BLAS requires on the geometry buffers it references by device
        // address (Phase 7 builds the tessellated BLAS from the transient VB/IB). The flag is valid
        // only with `VK_KHR_acceleration_structure`; without RT the buffers are raster-only.
        let accel_input = if self.rt.supported() {
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
        } else {
            vk::BufferUsageFlags::empty()
        };
        let acquire = |graph: &mut RenderGraph,
                       t: &mut RenderGraphResources,
                       key: &'static str,
                       bytes: u64,
                       usage|
         -> Option<vk::Buffer> {
            match graph.create_buffer(
                t,
                frame,
                key,
                crate::RgBufferDesc {
                    size: bytes.max(16),
                    usage,
                    lifetime: crate::RgBufferLifetime::Transient,
                },
            ) {
                Ok(resource) => Some(graph.buffer(resource)),
                Err(err) => {
                    tracing::error!("tess prep: acquire {key}: {err}");
                    None
                }
            }
        };
        let counters_bytes = instance_rows as u64 * 8;
        let (Some(pertri), Some(counters), Some(global), Some(seeds), Some(prims), Some(dispatch)) = (
            acquire(
                graph,
                &mut self.transient,
                "tess.pertri",
                tri_cur as u64 * 16,
                storage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.counters",
                counters_bytes,
                storage_cleared,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.global",
                8,
                storage_cleared,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.seeds",
                instance_rows as u64 * 20,
                indirect,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.prims",
                instance_rows as u64 * 4,
                storage,
            ),
            acquire(graph, &mut self.transient, "tess.dispatch", 12, indirect),
        ) else {
            return None;
        };
        // The amplified geometry stream: the worst-case-reserved transient VB (48 B micro-vertices) +
        // IB (u32). These are the buffers Phase 6 rasterizes and Phase 7 builds the BLAS from; here they
        // are storage-written by the emit kernel and additionally flagged VERTEX/INDEX/indirect-friendly
        // so the same handle serves those consumers without re-acquire.
        let vb_usage = storage
            | vk::BufferUsageFlags::VERTEX_BUFFER
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | accel_input;
        // The index stream is also degenerate-pad cleared (`cmd_fill_buffer` → TRANSFER_DST) and, like
        // the VB, is a BLAS build input (ACCEL_BUILD_INPUT) — the emit writes it, the raster draws it,
        // and the RT BLAS builds from it.
        let ib_usage = storage
            | vk::BufferUsageFlags::INDEX_BUFFER
            | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
            | accel_input
            | vk::BufferUsageFlags::TRANSFER_DST;
        // The prev-position stream (Phase 5): same worst-case size as `tess.vb`, storage-written by the
        // emit kernel and bound by the motion prepass as the prev vertex stream. Motion-only (no RT / no
        // indirect), so it needs neither a device address nor index usage.
        let prev_vb_usage = storage | vk::BufferUsageFlags::VERTEX_BUFFER;
        let (Some(out_vb), Some(out_ib), Some(out_prev_vb)) = (
            acquire(
                graph,
                &mut self.transient,
                "tess.vb",
                vb_cur as u64 * 48,
                vb_usage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.ib",
                ib_cur as u64 * 4,
                ib_usage,
            ),
            acquire(
                graph,
                &mut self.transient,
                "tess.vb.prev",
                vb_cur as u64 * 48,
                prev_vb_usage,
            ),
        ) else {
            return None;
        };

        // Resolve each tess-seam draw's handles now that the per-frame transients exist,
        // matched to its tess instance by `base_instance`. The finalize seed for row `r`
        // lives at byte `r * 20` in the `seeds` args buffer. Done before the raster
        // passes shallow-clone the draw list, so they pick up the indirect-draw handles.
        for (row, inst) in insts.iter().enumerate() {
            for tess_draw in self.scene_draw_list.tess_draws.iter_mut() {
                if tess_draw.base_instance == inst.base_instance {
                    tess_draw.draw = Some(crate::TessDraw {
                        vertex_buffer: out_vb,
                        prev_vertex_buffer: out_prev_vb,
                        index_buffer: out_ib,
                        args_buffer: seeds,
                        args_offset: row as u64 * 20,
                    });
                }
            }
        }

        // The tessellated RT instances' slices are filled from the SEPARATE coarse (secondary-ray) chain
        // below (Phase 10, Q2), not these fine raster buffers, so the RT BLAS builds from lower-density
        // geometry than the raster passes rasterize.

        // Wire one factor + scan + finalize + emit descriptor set per instance (shared buffers bound at
        // whole range; the pushes carry each instance's slice bases), plus the one global args set.
        let pool = self.tessellation.pool(frame);
        let mut factor_calls: Vec<(vk::DescriptorSet, crate::TessFactorPush, u32)> = Vec::new();
        let mut scan_calls: Vec<(vk::DescriptorSet, crate::TessScanPush, u32)> = Vec::new();
        let mut finalize_calls: Vec<(vk::DescriptorSet, crate::TessFinalizePush)> = Vec::new();
        let mut emit_calls: Vec<(vk::DescriptorSet, crate::TessEmitPush, u32)> = Vec::new();
        for (row, inst) in insts.iter().enumerate() {
            let layout = layouts[row];
            let (Some(factor_set), Some(scan_set), Some(finalize_set), Some(emit_set)) = (
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    factor_layout,
                    &[inst.welded, inst.edges, factors],
                ),
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    scan_layout,
                    &[inst.tri_edges, factors, pertri, counters, global],
                ),
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    finalize_layout,
                    &[counters, seeds, prims],
                ),
                crate::tessellation::wire_storage_set(
                    raw,
                    pool,
                    emit_layout,
                    &[
                        inst.base_vertices,
                        inst.base_indices,
                        pertri,
                        inst.tri_edges,
                        factors,
                        out_vb,
                        out_ib,
                        prev_factors,
                        out_prev_vb,
                    ],
                ),
            ) else {
                continue;
            };
            factor_calls.push((
                factor_set,
                crate::tessellation::factor_push(
                    &cam,
                    inst.model,
                    inst.factor_cap,
                    inst.min_factor,
                    inst.edge_length_target,
                    layout.factor_base,
                    inst.edge_count,
                    // Full local-space displacement amplitude (world `height_scale` mapped to local by
                    // the uniform world scale); the kernel scales it by the per-edge local height range
                    // sampled from the min/max pyramid at `height_index` (Phase 10 per-region factor).
                    inst.height_scale,
                    inst.height_index,
                    [inst.uv_transform[0], inst.uv_transform[1]],
                ),
                inst.edge_count.div_ceil(64).max(1),
            ));
            scan_calls.push((
                scan_set,
                crate::TessScanPush {
                    tri_count: inst.tri_count,
                    tri_base: layout.tri_base,
                    counter_base: layout.instance_row * 2,
                    factor_cap: inst.factor_cap,
                    _pad: [0; 4],
                },
                inst.tri_count.div_ceil(64).max(1),
            ));
            finalize_calls.push((
                finalize_set,
                crate::TessFinalizePush {
                    counter_base: layout.instance_row * 2,
                    seed_base: layout.instance_row * 5,
                    vertex_base: layout.vertex_base,
                    index_base: layout.index_base,
                    instance_row: layout.instance_row,
                    first_instance: inst.base_instance,
                    _pad: [0; 2],
                },
            ));
            emit_calls.push((
                emit_set,
                crate::TessEmitPush {
                    tri_base: layout.tri_base,
                    vertex_base: layout.vertex_base,
                    index_base: layout.index_base,
                    height_index: inst.height_index,
                    height_scale: inst.height_scale,
                    factor_cap: inst.factor_cap,
                    vector_index: inst.vector_index,
                    _pad0: 0,
                    uv_transform: inst.uv_transform,
                    factor_base: layout.factor_base,
                    _pad: [0; 3],
                },
                inst.tri_count,
            ));
        }
        let args_set =
            crate::tessellation::wire_storage_set(raw, pool, args_layout, &[global, dispatch])?;

        let factors_res = graph.import_buffer(factors, None);
        let pertri_res = graph.import_buffer(pertri, None);
        let counters_res = graph.import_buffer(counters, None);
        let global_res = graph.import_buffer(global, None);
        let seeds_res = graph.import_buffer(seeds, None);
        let prims_res = graph.import_buffer(prims, None);
        let dispatch_res = graph.import_buffer(dispatch, None);
        let out_vb_res = graph.import_buffer(out_vb, None);
        let out_ib_res = graph.import_buffer(out_ib, None);
        let out_prev_vb_res = graph.import_buffer(out_prev_vb, None);

        // Factor: one thread per unique base edge writes its shared fractional factor. Bindless set 0
        // (the min/max pyramid tap) is bound once; each instance binds its edge set (set 1) + push.
        {
            let pso = factor_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = factor_calls;
            let pass = crate::RgPass::compute("tess-factor")
                .access(factors_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. PSO + sets are valid this frame; each dispatch covers one
                    // instance's edges.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[bindless_set],
                            &[],
                        );
                        for (set, push, groups) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                1,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Scan: clear the atomic accumulators (transfer write → the one hand-written transfer→compute
        // barrier, as the graph has no fill primitive), then one thread per base triangle predicts +
        // prefix-sums the exact dice counts.
        {
            let pso = scan_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = scan_calls;
            let counters_buf = counters;
            let global_buf = global;
            let clear_bytes = counters_bytes.max(8);
            let pass = crate::RgPass::compute("tess-scan")
                .access(factors_res, crate::RgUsage::StorageReadCompute)
                .access(pertri_res, crate::RgUsage::StorageWriteCompute)
                .access(counters_res, crate::RgUsage::StorageWriteCompute)
                .access(global_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The fills zero the atomic accumulators; the barrier orders
                    // them before the scan's atomic reads/writes.
                    unsafe {
                        raw_body.cmd_fill_buffer(cmd, counters_buf, 0, clear_bytes, 0);
                        raw_body.cmd_fill_buffer(cmd, global_buf, 0, 8, 0);
                        let barrier = |buffer, size| {
                            vk::BufferMemoryBarrier2::default()
                                .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                .dst_access_mask(
                                    vk::AccessFlags2::SHADER_STORAGE_READ
                                        | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                                )
                                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                .buffer(buffer)
                                .offset(0)
                                .size(size)
                        };
                        let barriers = [barrier(counters_buf, clear_bytes), barrier(global_buf, 8)];
                        let dep = vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
                        raw_body.cmd_pipeline_barrier2(cmd, &dep);
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        for (set, push, groups) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Finalize: one thread per instance writes the indirect draw seed + RT prim count.
        {
            let pso = finalize_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = finalize_calls;
            let pass = crate::RgPass::compute("tess-finalize")
                .access(counters_res, crate::RgUsage::StorageReadCompute)
                .access(seeds_res, crate::RgUsage::StorageWriteCompute)
                .access(prims_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        for (set, push) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, 1, 1, 1);
                        }
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Args: one thread turns the global micro-vertex total into the Phase-4 emit dispatch size.
        {
            let pso = args_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let pass = crate::RgPass::compute("tess-args")
                .access(global_res, crate::RgUsage::StorageReadCompute)
                .access(dispatch_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[args_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, 1, 1, 1);
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // Emit: one workgroup per base triangle (dispatched per instance) dices + displaces + welds +
        // writes the amplified micro-vertices + generated index stream into the transient VB/IB at the
        // scan's predicted offsets. Reads perTri + factors (ordered after scan) + the static base
        // stream via bindless set 0's height/vector maps.
        {
            let pso = emit_pso;
            let handle = pso.handle();
            let layout = pso.layout();
            let raw_body = raw.clone();
            let calls = emit_calls;
            let ib_clear_bytes = ib_cur as u64 * 4;
            let out_ib_buf = out_ib;
            let pass = crate::RgPass::compute("tess-emit")
                .access(pertri_res, crate::RgUsage::StorageReadCompute)
                .access(factors_res, crate::RgUsage::StorageReadCompute)
                .access(out_vb_res, crate::RgUsage::StorageWriteCompute)
                .access(out_ib_res, crate::RgUsage::StorageWriteCompute)
                .access(out_prev_vb_res, crate::RgUsage::StorageWriteCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. Bindless set 0 is bound once; each instance binds its emit
                    // set (set 1) + push and dispatches one workgroup per base triangle.
                    unsafe {
                        // Zero the whole index slice first so every index past the real (GPU-packed)
                        // triangles is a degenerate `(0,0,0)` triangle — the worst-case RT BUILD floor
                        // (Phase 7) reads the full reserved range and the AS builder discards degenerates.
                        // Ordered transfer→compute before the emit dispatches overwrite the real triangles.
                        if ib_clear_bytes > 0 {
                            raw_body.cmd_fill_buffer(cmd, out_ib_buf, 0, ib_clear_bytes, 0);
                            let clear_barrier = vk::MemoryBarrier2::default()
                                .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE);
                            let cb = [clear_barrier];
                            let dep = vk::DependencyInfo::default().memory_barriers(&cb);
                            raw_body.cmd_pipeline_barrier2(cmd, &dep);
                        }
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[bindless_set],
                            &[],
                        );
                        for (set, push, workgroups) in &calls {
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                1,
                                &[*set],
                                &[],
                            );
                            raw_body.cmd_push_constants(
                                cmd,
                                layout,
                                vk::ShaderStageFlags::COMPUTE,
                                0,
                                bytemuck::bytes_of(push),
                            );
                            raw_body.cmd_dispatch(cmd, *workgroups, 1, 1);
                        }
                        // The emit output (VB/IB) + the finalize-written indirect args are consumed by
                        // the later raster passes as vertex/index/indirect input. The graph runs passes
                        // in add order (emit precedes every raster pass), but those passes don't declare
                        // these transient handles, so make the compute writes visible to the fixed-
                        // function fetch here with one global barrier (the graph has no cross-pass
                        // primitive without threading the resources into all seven consumers).
                        let to_fetch = vk::MemoryBarrier2::default()
                            .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                            .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                            .dst_stage_mask(
                                vk::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT
                                    | vk::PipelineStageFlags2::INDEX_INPUT
                                    | vk::PipelineStageFlags2::DRAW_INDIRECT,
                            )
                            .dst_access_mask(
                                vk::AccessFlags2::VERTEX_ATTRIBUTE_READ
                                    | vk::AccessFlags2::INDEX_READ
                                    | vk::AccessFlags2::INDIRECT_COMMAND_READ,
                            );
                        let barriers = [to_fetch];
                        let dep = vk::DependencyInfo::default().memory_barriers(&barriers);
                        raw_body.cmd_pipeline_barrier2(cmd, &dep);
                    }
                    drop(pso);
                });
            graph.add_pass(pass);
        }

        // RT secondary-ray coarsening (Phase 10, Q2): the RT BLAS is built from a SEPARATE, coarser run
        // of the same factor→scan→emit chain — a larger per-edge LOD target (fewer micro-edges) + a
        // smaller dice cap (a ~1/COARSEN² worst-case reservation → a far cheaper per-frame BUILD). The
        // coarse geometry is still Phong-smoothed, displaced, and watertight (the same shared-edge factor
        // snap), only lower-density; the raster path keeps the fine buffers above. Skipped entirely unless
        // a displaced instance is actually RT-consumed this frame.
        let rt_entities: std::collections::HashSet<u64> = self
            .scene_draw_list
            .deformed_rt_instances
            .iter()
            .map(|rt| rt.entity)
            .collect();
        let rt_active = insts
            .iter()
            .any(|inst| inst.entity != 0 && rt_entities.contains(&inst.entity));
        let mut tess_rt_res: Option<(RgResource, RgResource)> = None;
        if rt_active {
            // Coarse per-instance placement: identical edge/tri prefix sums (same base topology), but the
            // vertex/index reservations use the coarse cap, so the coarse arena is ~1/COARSEN² of the fine.
            let mut rt_layouts: Vec<crate::TessInstanceLayout> = Vec::with_capacity(insts.len());
            let mut rt_caps: Vec<f32> = Vec::with_capacity(insts.len());
            let (mut r_edge, mut r_tri, mut r_vb, mut r_ib) = (0u32, 0u32, 0u32, 0u32);
            for (row, inst) in insts.iter().enumerate() {
                let cap = crate::tessellation::rt_coarsen_cap(inst.factor_cap, inst.min_factor);
                rt_caps.push(cap);
                let (verts, indices) =
                    crate::tessellation::tess_worst_case(inst.tri_count, cap as u32);
                rt_layouts.push(crate::TessInstanceLayout {
                    factor_base: r_edge,
                    tri_base: r_tri,
                    instance_row: row as u32,
                    vertex_base: r_vb,
                    index_base: r_ib,
                });
                r_edge += inst.edge_count;
                r_tri += inst.tri_count;
                r_vb = r_vb.saturating_add(verts as u32);
                r_ib = r_ib.saturating_add(indices as u32);
            }

            // Coarse transient scratch (grow-only, keyed, distinct from the fine buffers). The coarse VB/IB
            // feed only the RT BLAS build (via device address), so they carry SHADER_DEVICE_ADDRESS +
            // ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY (never VERTEX/INDEX — no raster fetch). The
            // emit→build barrier is graph-derived: StorageWrite here + AccelStructBuildRead on `tlas-build`.
            // `tess.vb.rt.prev` backs the emit kernel's mandatory prev-stream write (RT has no motion
            // vectors; prev factors == cur ⇒ prev pos == cur pos) — a throwaway the BLAS never reads.
            let rt_ib_bytes = r_ib as u64 * 4;
            let rt_as_usage = storage
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS
                | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR;
            let (
                Some(factors_rt),
                Some(pertri_rt),
                Some(counters_rt),
                Some(global_rt),
                Some(out_vb_rt),
                Some(out_ib_rt),
                Some(out_prev_vb_rt),
            ) = (
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.factor.rt",
                    r_edge as u64 * 4,
                    storage,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.pertri.rt",
                    r_tri as u64 * 16,
                    storage,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.counters.rt",
                    counters_bytes,
                    storage_cleared,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.global.rt",
                    8,
                    storage_cleared,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.vb.rt",
                    r_vb as u64 * 48,
                    rt_as_usage,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.ib.rt",
                    rt_ib_bytes,
                    rt_as_usage | vk::BufferUsageFlags::TRANSFER_DST,
                ),
                acquire(
                    graph,
                    &mut self.transient,
                    "tess.vb.rt.prev",
                    r_vb as u64 * 48,
                    storage,
                ),
            )
            else {
                return None;
            };

            // Point each RT-consumed instance's slice at the coarse geometry + coarse worst case, so
            // Phase-7's `plan_tessellated_blas_builds` reads the coarse VB/IB (the IB tail is
            // degenerate-padded, so the worst-case BUILD range discards the unused triangles).
            for (row, inst) in insts.iter().enumerate() {
                if inst.entity == 0 {
                    continue;
                }
                let layout = rt_layouts[row];
                let (wc_verts, wc_indices) =
                    crate::tessellation::tess_worst_case(inst.tri_count, rt_caps[row] as u32);
                for rt in self.scene_draw_list.deformed_rt_instances.iter_mut() {
                    if rt.entity == inst.entity {
                        rt.tess = Some(crate::TessRtSlice {
                            vertex_buffer: out_vb_rt,
                            index_buffer: out_ib_rt,
                            vertex_base: layout.vertex_base,
                            index_base: layout.index_base,
                            worst_case_verts: wc_verts as u32,
                            worst_case_prims: (wc_indices / 3) as u32,
                        });
                    }
                }
            }

            // The coarse chain reuses the fine PSOs (cached; the request clones the Arc) — factor + scan +
            // emit only (RT needs neither the raster indirect-draw finalize nor the emit-dispatch args).
            let (Some(factor_pso_rt), Some(scan_pso_rt), Some(emit_pso_rt)) = (
                self.pipelines
                    .request_tess_factor(bindless_layout, factor_layout),
                self.pipelines.request_tess_scan(scan_layout),
                self.pipelines
                    .request_tessellate(bindless_layout, emit_layout),
            ) else {
                return None;
            };

            let mut factor_calls_rt: Vec<(vk::DescriptorSet, crate::TessFactorPush, u32)> =
                Vec::new();
            let mut scan_calls_rt: Vec<(vk::DescriptorSet, crate::TessScanPush, u32)> = Vec::new();
            let mut emit_calls_rt: Vec<(vk::DescriptorSet, crate::TessEmitPush, u32)> = Vec::new();
            for (row, inst) in insts.iter().enumerate() {
                let layout = rt_layouts[row];
                let cap = rt_caps[row];
                let (Some(factor_set), Some(scan_set), Some(emit_set)) = (
                    crate::tessellation::wire_storage_set(
                        raw,
                        pool,
                        factor_layout,
                        &[inst.welded, inst.edges, factors_rt],
                    ),
                    crate::tessellation::wire_storage_set(
                        raw,
                        pool,
                        scan_layout,
                        &[
                            inst.tri_edges,
                            factors_rt,
                            pertri_rt,
                            counters_rt,
                            global_rt,
                        ],
                    ),
                    crate::tessellation::wire_storage_set(
                        raw,
                        pool,
                        emit_layout,
                        &[
                            inst.base_vertices,
                            inst.base_indices,
                            pertri_rt,
                            inst.tri_edges,
                            factors_rt,
                            out_vb_rt,
                            out_ib_rt,
                            // RT has no temporal history: prev factors == cur ⇒ zero geomorph motion.
                            factors_rt,
                            out_prev_vb_rt,
                        ],
                    ),
                ) else {
                    continue;
                };
                factor_calls_rt.push((
                    factor_set,
                    crate::tessellation::factor_push(
                        &cam,
                        inst.model,
                        cap,
                        inst.min_factor,
                        // The coarsened LOD target — the sole per-edge coarsening lever (the factor kernel
                        // is otherwise identical, so a shared edge stays bit-identical → crack-free).
                        crate::tessellation::rt_coarsen_target(inst.edge_length_target),
                        layout.factor_base,
                        inst.edge_count,
                        inst.height_scale,
                        inst.height_index,
                        [inst.uv_transform[0], inst.uv_transform[1]],
                    ),
                    inst.edge_count.div_ceil(64).max(1),
                ));
                scan_calls_rt.push((
                    scan_set,
                    crate::TessScanPush {
                        tri_count: inst.tri_count,
                        tri_base: layout.tri_base,
                        counter_base: layout.instance_row * 2,
                        factor_cap: cap,
                        _pad: [0; 4],
                    },
                    inst.tri_count.div_ceil(64).max(1),
                ));
                emit_calls_rt.push((
                    emit_set,
                    crate::TessEmitPush {
                        tri_base: layout.tri_base,
                        vertex_base: layout.vertex_base,
                        index_base: layout.index_base,
                        height_index: inst.height_index,
                        height_scale: inst.height_scale,
                        factor_cap: cap,
                        vector_index: inst.vector_index,
                        _pad0: 0,
                        uv_transform: inst.uv_transform,
                        factor_base: layout.factor_base,
                        _pad: [0; 3],
                    },
                    inst.tri_count,
                ));
            }

            let factors_rt_res = graph.import_buffer(factors_rt, None);
            let pertri_rt_res = graph.import_buffer(pertri_rt, None);
            let counters_rt_res = graph.import_buffer(counters_rt, None);
            let global_rt_res = graph.import_buffer(global_rt, None);
            let out_vb_rt_res = graph.import_buffer(out_vb_rt, None);
            let out_ib_rt_res = graph.import_buffer(out_ib_rt, None);
            let out_prev_vb_rt_res = graph.import_buffer(out_prev_vb_rt, None);

            // Coarse factor: one thread per unique base edge, coarse LOD target + coarse cap.
            {
                let pso = factor_pso_rt;
                let handle = pso.handle();
                let layout = pso.layout();
                let raw_body = raw.clone();
                let calls = factor_calls_rt;
                let pass = crate::RgPass::compute("tess-factor-rt")
                    .access(factors_rt_res, crate::RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. PSO + sets are valid this frame; one dispatch per instance.
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[bindless_set],
                                &[],
                            );
                            for (set, push, groups) in &calls {
                                raw_body.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    layout,
                                    1,
                                    &[*set],
                                    &[],
                                );
                                raw_body.cmd_push_constants(
                                    cmd,
                                    layout,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(push),
                                );
                                raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                            }
                        }
                        drop(pso);
                    });
                graph.add_pass(pass);
            }

            // Coarse scan: clear the atomic accumulators, then predict + prefix-sum the coarse dice counts.
            {
                let pso = scan_pso_rt;
                let handle = pso.handle();
                let layout = pso.layout();
                let raw_body = raw.clone();
                let calls = scan_calls_rt;
                let counters_buf = counters_rt;
                let global_buf = global_rt;
                let clear_bytes = counters_bytes.max(8);
                let pass = crate::RgPass::compute("tess-scan-rt")
                    .access(factors_rt_res, crate::RgUsage::StorageReadCompute)
                    .access(pertri_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(counters_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(global_rt_res, crate::RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. The fills zero the atomic accumulators; the barrier orders
                        // them before the scan's atomic reads/writes.
                        unsafe {
                            raw_body.cmd_fill_buffer(cmd, counters_buf, 0, clear_bytes, 0);
                            raw_body.cmd_fill_buffer(cmd, global_buf, 0, 8, 0);
                            let barrier = |buffer, size| {
                                vk::BufferMemoryBarrier2::default()
                                    .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                    .dst_access_mask(
                                        vk::AccessFlags2::SHADER_STORAGE_READ
                                            | vk::AccessFlags2::SHADER_STORAGE_WRITE,
                                    )
                                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                                    .buffer(buffer)
                                    .offset(0)
                                    .size(size)
                            };
                            let barriers =
                                [barrier(counters_buf, clear_bytes), barrier(global_buf, 8)];
                            let dep =
                                vk::DependencyInfo::default().buffer_memory_barriers(&barriers);
                            raw_body.cmd_pipeline_barrier2(cmd, &dep);
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            for (set, push, groups) in &calls {
                                raw_body.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    layout,
                                    0,
                                    &[*set],
                                    &[],
                                );
                                raw_body.cmd_push_constants(
                                    cmd,
                                    layout,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(push),
                                );
                                raw_body.cmd_dispatch(cmd, *groups, 1, 1);
                            }
                        }
                        drop(pso);
                    });
                graph.add_pass(pass);
            }

            // Coarse emit: degenerate-pad the whole coarse IB, then dice + displace + weld into the coarse
            // VB/IB. No vertex/index/indirect fetch barrier — the coarse output feeds only the RT BLAS
            // build, whose emit→build barrier the graph derives from the StorageWrite accesses declared
            // here + the `AccelStructBuildRead` on `tlas-build`.
            {
                let pso = emit_pso_rt;
                let handle = pso.handle();
                let layout = pso.layout();
                let raw_body = raw.clone();
                let calls = emit_calls_rt;
                let ib_clear_bytes = rt_ib_bytes;
                let out_ib_buf = out_ib_rt;
                let pass = crate::RgPass::compute("tess-emit-rt")
                    .access(pertri_rt_res, crate::RgUsage::StorageReadCompute)
                    .access(factors_rt_res, crate::RgUsage::StorageReadCompute)
                    .access(out_vb_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(out_ib_rt_res, crate::RgUsage::StorageWriteCompute)
                    .access(out_prev_vb_rt_res, crate::RgUsage::StorageWriteCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        // SAFETY: the ash seam. Bindless set 0 is bound once; each instance binds its emit
                        // set (set 1) + push and dispatches one workgroup per base triangle.
                        unsafe {
                            // Zero the whole coarse index slice so every index past the GPU-packed tail is
                            // a degenerate `(0,0,0)` triangle — the worst-case RT BUILD range reads the full
                            // reserved span and the AS builder discards the degenerates (watertight floor).
                            if ib_clear_bytes > 0 {
                                raw_body.cmd_fill_buffer(cmd, out_ib_buf, 0, ib_clear_bytes, 0);
                                let clear_barrier = vk::MemoryBarrier2::default()
                                    .src_stage_mask(vk::PipelineStageFlags2::CLEAR)
                                    .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                                    .dst_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                                    .dst_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE);
                                let cb = [clear_barrier];
                                let dep = vk::DependencyInfo::default().memory_barriers(&cb);
                                raw_body.cmd_pipeline_barrier2(cmd, &dep);
                            }
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[bindless_set],
                                &[],
                            );
                            for (set, push, workgroups) in &calls {
                                raw_body.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    layout,
                                    1,
                                    &[*set],
                                    &[],
                                );
                                raw_body.cmd_push_constants(
                                    cmd,
                                    layout,
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(push),
                                );
                                raw_body.cmd_dispatch(cmd, *workgroups, 1, 1);
                            }
                        }
                        drop(pso);
                    });
                graph.add_pass(pass);
            }

            tess_rt_res = Some((out_vb_rt_res, out_ib_rt_res));
        }

        // The factor pass above is committed to write `slot[cur_slot]` this frame with `edge_layout`;
        // stamp that so next frame's prev-stream lookup can tell whether the slot is layout-aligned.
        self.tessellation.set_factor_layout(cur_slot, edge_layout);
        tess_rt_res
    }

    /// Arms the directional virtual-shadow sampling; `casting` (gated by the master
    /// shadow toggle) drives whether the sun shadows this frame.
    pub fn set_directional_shadow(&mut self, casting: bool) {
        self.lighting.set_directional_shadow(casting);
    }

    /// Arms the spot's virtual-shadow space with its perspective transform + its index
    /// in the per-frame light list.
    pub fn set_spot_shadow(&mut self, light_view_proj: Mat4, light_index: u32, casting: bool) {
        self.lighting
            .set_spot_shadow(light_view_proj, light_index, casting);
    }

    /// Arms the point light's six virtual face spaces with its world position + far
    /// plane + its index.
    pub fn set_point_shadow(
        &mut self,
        light_pos: saffron_geometry::glam::Vec3,
        far_plane: f32,
        light_index: u32,
        casting: bool,
    ) {
        self.lighting
            .set_point_shadow(light_pos, far_plane, light_index, casting);
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
    pub fn set_rt_scene(&mut self, instances: Vec<crate::RtInstanceInput>) {
        self.rt.set_rt_scene(instances);
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
        // Rebuild the view's HZB pyramids at the new input extent under the idle wait,
        // returning the old build sets to the pool.
        if let Some(mut old_pyramid) = self.views[i].hzb_pyramid.take() {
            old_pyramid.free_sets(&self.descriptors);
        }
        self.views[i].hzb_pyramid =
            match crate::HzbPyramid::new(&self.device, &self.descriptors, &self.hzb, input) {
                Ok(pyramid) => Some(pyramid),
                Err(err) => {
                    tracing::error!("hzb pyramid rebuild: {err}");
                    None
                }
            };
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
            .build(&self.device, &self.descriptors, &self.restir, input)?;
        self.clouds.bind_view(
            i,
            crate::clouds::CloudViewBindings {
                color: self.views[i].offscreen.view(),
                depth: self.views[i].depth.view(),
                motion: self.views[i]
                    .motion
                    .as_ref()
                    .expect("cloud motion built")
                    .view(),
                reduced: [
                    self.views[i].cloud_reduced[0]
                        .as_ref()
                        .expect("cloud reduced 0 built")
                        .view(),
                    self.views[i].cloud_reduced[1]
                        .as_ref()
                        .expect("cloud reduced 1 built")
                        .view(),
                ],
                reduced_depth: self.views[i]
                    .cloud_reduced_depth
                    .as_ref()
                    .expect("cloud reduced depth built")
                    .view(),
                full_color: self.views[i]
                    .cloud_full_color
                    .as_ref()
                    .expect("cloud full color built")
                    .view(),
                full_depth: self.views[i]
                    .cloud_full_depth
                    .as_ref()
                    .expect("cloud full depth built")
                    .view(),
            },
        );
        Ok(())
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

    /// Reads the active view's post-processed offscreen back and encodes it to PNG bytes **in
    /// memory** — the bytes-returning twin of [`Renderer::capture_viewport`], for a background
    /// thumbnail render whose result ships over the control protocol rather than to a file. The
    /// caller selects the active view first (via [`Renderer::set_active_view`]). The offscreen is
    /// already display-range, so its `RGBA16F` halves are clamped ([`PngTransfer::Clamp`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vk`] if the read-back's device idle / Vulkan calls fail, or an
    /// [`Error::ShaderLoad`]-shaped wrapper carrying a PNG encode failure.
    pub fn encode_active_offscreen_png(&mut self) -> Result<crate::ThumbnailPng> {
        let (extent, format, pixels) = self.read_active_offscreen()?;
        let bytes = crate::encode_to_png(
            &pixels,
            extent.width,
            extent.height,
            format,
            crate::PngTransfer::Clamp,
        )
        .map_err(|err| Error::ShaderLoad(format!("thumbnail: encode png: {err}")))?;
        Ok(crate::ThumbnailPng {
            bytes,
            width: extent.width,
            height: extent.height,
        })
    }

    /// Restores the active view **without** resetting its temporal state, unlike
    /// [`Renderer::set_active_view`]. A background thumbnail render makes a brief
    /// `Scene → Thumbnail → Scene` excursion each drained frame; going back through
    /// `set_active_view` would wipe the Scene view's accumulated TAA / SSGI / ReSTIR / DDGI
    /// history every time and re-converge it visibly. This leaves that history intact.
    pub fn restore_active_view_no_reset(&mut self, view: ViewId) {
        self.active_view = view;
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
                self.device.graphics_queue.submit2(
                    raw,
                    &submit,
                    fence,
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
                self.device.graphics_queue.submit2(
                    raw,
                    &submit,
                    fence,
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
            self.stars
                .set_sample_count(&self.device, self.aa.sample_count())?;
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

    /// Sets low-light rod/cone adaptation strength for the next tonemap pass.
    pub fn set_night_factor(&mut self, factor: f32) {
        self.night_factor = factor.clamp(0.0, 1.0);
    }

    /// The current low-light adaptation strength.
    pub fn night_factor(&self) -> f32 {
        self.night_factor
    }

    /// Sets the bloom parameters: the enable flag, the energy-conserving composite `intensity`,
    /// the tent-upsample `scatter` radius (UV units), the `tint`, and the soft-knee `threshold`
    /// (`0.0` = thresholdless). Applied to the next frame's pre-tonemap bloom pass.
    pub fn set_bloom(
        &mut self,
        enabled: bool,
        intensity: f32,
        scatter: f32,
        tint: [f32; 3],
        threshold: f32,
    ) {
        self.bloom_enabled = enabled;
        self.bloom_intensity = intensity.max(0.0);
        self.bloom_scatter = scatter.clamp(0.0, 1.0);
        self.bloom_tint = tint;
        self.bloom_threshold = threshold.max(0.0);
    }

    /// Whether the scene-linear bloom pyramid runs before the tonemap.
    pub fn bloom_enabled(&self) -> bool {
        self.bloom_enabled
    }

    /// The energy-conserving bloom composite weight.
    pub fn bloom_intensity(&self) -> f32 {
        self.bloom_intensity
    }

    /// The bloom tent-upsample scatter radius in UV units.
    pub fn bloom_scatter(&self) -> f32 {
        self.bloom_scatter
    }

    /// The bloom tint (multiplies the composited bloom).
    pub fn bloom_tint(&self) -> [f32; 3] {
        self.bloom_tint
    }

    /// The bloom soft-knee prefilter threshold (`0.0` = thresholdless).
    pub fn bloom_threshold(&self) -> f32 {
        self.bloom_threshold
    }

    /// Sets the lens-dirt mask texture (`id`/`texture` together; `id == 0` + `None` clears it) that
    /// the bloom composite multiplies the accumulated pyramid by. An absent texture binds the 1×1
    /// white fallback (mask = 1 ⇒ identity).
    pub fn set_bloom_dirt_texture(&mut self, id: u64, texture: Option<Arc<crate::GpuTexture>>) {
        self.bloom_dirt_texture_id = id;
        self.bloom_dirt_texture = texture;
    }

    /// Sets the lens-dirt mix (`0.0` = no dirt, clamped to `[0, 1]`) and its tint.
    pub fn set_bloom_dirt_params(&mut self, intensity: f32, tint: [f32; 3]) {
        self.bloom_dirt_intensity = intensity.clamp(0.0, 1.0);
        self.bloom_dirt_tint = tint;
    }

    /// The lens-dirt mask asset id (`0` = none).
    pub fn bloom_dirt_texture(&self) -> u64 {
        self.bloom_dirt_texture_id
    }

    /// The lens-dirt mix fraction.
    pub fn bloom_dirt_intensity(&self) -> f32 {
        self.bloom_dirt_intensity
    }

    /// The lens-dirt tint.
    pub fn bloom_dirt_tint(&self) -> [f32; 3] {
        self.bloom_dirt_tint
    }

    /// Sets the anamorphic streak: the `enabled` toggle, the horizontal `ratio` squeeze (`~2`), the
    /// streak `tint`, and the `intensity` add weight. `ratio` is clamped `≥ 1`, `intensity ≥ 0`.
    pub fn set_bloom_anamorphic(
        &mut self,
        enabled: bool,
        ratio: f32,
        tint: [f32; 3],
        intensity: f32,
    ) {
        self.bloom_anamorphic_enabled = enabled;
        self.bloom_anamorphic_ratio = ratio.max(1.0);
        self.bloom_anamorphic_tint = tint;
        self.bloom_anamorphic_intensity = intensity.max(0.0);
    }

    /// Whether the anamorphic streak runs.
    pub fn bloom_anamorphic_enabled(&self) -> bool {
        self.bloom_anamorphic_enabled
    }

    /// The anamorphic horizontal squeeze.
    pub fn bloom_anamorphic_ratio(&self) -> f32 {
        self.bloom_anamorphic_ratio
    }

    /// The anamorphic streak tint.
    pub fn bloom_anamorphic_tint(&self) -> [f32; 3] {
        self.bloom_anamorphic_tint
    }

    /// The anamorphic streak add weight.
    pub fn bloom_anamorphic_intensity(&self) -> f32 {
        self.bloom_anamorphic_intensity
    }

    /// Sets the per-upsample-step tint stack (identity `1,1,1` when a level is absent). An empty
    /// vector disables per-mip tinting entirely.
    pub fn set_bloom_mip_tint(&mut self, tint: Vec<[f32; 3]>) {
        self.bloom_mip_tint = tint;
    }

    /// The per-upsample-step tint stack.
    pub fn bloom_mip_tint(&self) -> Vec<[f32; 3]> {
        self.bloom_mip_tint.clone()
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

    /// Toggles the GPU compute-displacement path.
    pub fn set_displacement(&mut self, enabled: bool) {
        self.displacement_enabled = enabled;
    }

    /// Whether GPU displacement is on.
    pub fn displacement_enabled(&self) -> bool {
        self.displacement_enabled
    }

    /// Sets the tessellation-quality budget for displaced instances. `None` for a field leaves it
    /// unchanged; the values are clamped to a sane range (cap ∈ [1, 64], min ∈ [1, cap], edge target
    /// ≥ 1 px) so a control caller cannot drive the tessellator into a degenerate or runaway state.
    pub fn set_tessellation_quality(
        &mut self,
        factor_cap: Option<f32>,
        min_factor: Option<f32>,
        edge_length_target: Option<f32>,
    ) {
        if let Some(cap) = factor_cap {
            // Integer cap (the snap/scan clamp grids on it) with the split pass's expressible ceiling;
            // the micro-vertex budget — not this — is the real bound on dense scenes.
            self.tess_factor_cap = cap.round().clamp(1.0, 2048.0);
        }
        if let Some(min) = min_factor {
            self.tess_min_factor = min.clamp(1.0, self.tess_factor_cap);
        }
        // A cap change can leave the min above it — re-clamp so `min ≤ cap` always holds.
        self.tess_min_factor = self.tess_min_factor.min(self.tess_factor_cap);
        if let Some(target) = edge_length_target {
            self.tess_edge_length_target = target.max(1.0);
        }
    }

    /// The current tessellation-quality budget `(factor_cap, min_factor, edge_length_target)`.
    pub fn tessellation_quality(&self) -> (f32, f32, f32) {
        (
            self.tess_factor_cap,
            self.tess_min_factor,
            self.tess_edge_length_target,
        )
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
            vsm: self.vsm_residency.counters(),
            frame_ms: self.frame_ms,
            fps,
            gpu_ms: self.gpu_frame_ms,
            cpu_frame_ms: self.cpu_frame_ms,
            scene_gather_ms: self.scene_gather_ms,
            cpu_wait_ms: self.cpu_wait_ms,
            rt_instances: self.rt.frame_instance_count(),
            vram_usage_bytes: self.vram_usage_bytes,
            vram_budget_bytes: self.vram_budget_bytes,
            software_gpu: self.software_gpu,
            profiler_mode: self.gpu_profiler.mode,
            view_mode: self.view_mode,
            exposure_ev: self.exposure_ev,
            color_grade: self.color_grade,
            bloom_enabled: self.bloom_enabled,
            bloom_intensity: self.bloom_intensity,
            bloom_scatter: self.bloom_scatter,
            bloom_tint: self.bloom_tint,
            bloom_threshold: self.bloom_threshold,
            bloom_dirt_texture: self.bloom_dirt_texture_id,
            bloom_dirt_intensity: self.bloom_dirt_intensity,
            bloom_dirt_tint: self.bloom_dirt_tint,
            bloom_anamorphic_enabled: self.bloom_anamorphic_enabled,
            bloom_anamorphic_ratio: self.bloom_anamorphic_ratio,
            bloom_anamorphic_tint: self.bloom_anamorphic_tint,
            bloom_anamorphic_intensity: self.bloom_anamorphic_intensity,
        }
    }

    /// Records the run loop's per-frame wall-clock timings for [`Renderer::render_stats`]
    /// and the frame-history percentiles.
    pub fn record_frame_timings(&mut self, frame_ms: f32, cpu_frame_ms: f32, cpu_wait_ms: f32) {
        self.frame_ms = frame_ms;
        self.cpu_frame_ms = cpu_frame_ms;
        self.cpu_wait_ms = cpu_wait_ms;
    }

    /// Records the CPU duration of the scene driver's static + skinned draw-list gather.
    pub fn record_scene_gather(&mut self, elapsed: Duration) {
        self.scene_gather_ms = elapsed.as_secs_f32() * 1000.0;
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
        // Accumulate a wrapping scene clock (seconds) for the fog-volume noise wind advection. The
        // 3600 s wrap keeps the float precise while the drift stays continuous across the seam.
        self.fog_time = (self.fog_time + dt_seconds) % 3600.0;
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

    /// Records and submits the scene + optional depth-prepass into the active view's
    /// offscreen target through the render graph. Call after
    /// [`Renderer::submit_gpu_scene_deformations`]; submit-seam closures replay after
    /// the executor draws.
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
        // The wait is unbounded, so a frame whose GPU work never completes blocks here forever.
        // Registering it names that frame from the watchdog thread instead of hanging silently.
        let _watch = crate::watchdog::watch("frame", self.frame_serial());
        // SAFETY: the ash seam. The fence belongs to this device; the wait blocks until this
        // slot's prior GPU work completes, so its per-frame buffers/sets are free to reuse.
        checked(
            unsafe { raw.wait_for_fences(&[in_flight], true, u64::MAX) },
            "wait_for_fences (begin)",
        )?;
        // The slot's prior GPU work is done, so its transient scratch allocations are free to
        // recycle: rewind the pool's acquire cursors for this frame index.
        self.transient.begin_frame(self.frames.index());
        // The tessellation prep descriptor pool recycles on the same fence: reset this slot's sets
        // before the deform scope wires the frame's factor/scan/finalize passes.
        self.tessellation.begin_frame(self.frames.index());
        // This slot's GPU work (from MAX_FRAMES_IN_FLIGHT frames ago) is now complete, so its
        // timestamp pool reads back without blocking: fold the prior frame's per-pass GPU spans
        // into `gpu_frame_ms` + `last_timings` at the begin-frame fence wait. A no-op when the
        // profiler is `Off`.
        let slot = self.frames.index();
        self.global_gpu_data.begin_frame(slot)?;
        self.gpu_scene_uploader.begin_frame(slot)?;
        self.persistent_gpu_scene.begin_frame(slot)?;
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
        self.frames.reset_command_pools(&self.device)?;
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

        // Advance fence-owned IBL refreshes at the frame boundary. A completed back set is
        // descriptor-committed here; in-flight frames continue sampling the front set.
        match self.ibl.update_refresh(&self.device) {
            Ok(true) => {
                self.sky.bind_env_cube(&self.ibl);
                self.reflection.refresh_fallbacks(&self.ibl);
            }
            Ok(false) => {}
            Err(err) => tracing::error!("ibl refresh failed: {err}"),
        }
        match self.preview_ibl.update_refresh(&self.device) {
            Ok(true) => self
                .reflection
                .refresh_secondary_fallbacks(&self.preview_ibl),
            Ok(false) => {}
            Err(err) => tracing::error!("preview ibl refresh failed: {err}"),
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
        let raw = self.device.raw().clone();
        let frame = self.frames.index();
        let command_buffer = self.frames.command_buffer();
        self.reflection.prepare_frame(frame);

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
            self.pipelines.request_depth_prepass_executor()
        } else {
            None
        };
        // The tessellation seam's vertex-input pass PSOs, resolved only when the frame
        // carries tess draws (displaced instances draw their amplified transient
        // geometry after each pass's executor buckets).
        let has_tess_draws = !self.scene_draw_list.tess_draws.is_empty();
        let depth_prepass_tess = if has_tess_draws && self.use_depth_prepass {
            self.pipelines.request_depth_prepass()
        } else {
            None
        };
        let gbuffer_tess = if has_tess_draws {
            self.pipelines.request_gbuffer()
        } else {
            None
        };
        let motion_tess = if has_tess_draws {
            self.pipelines.request_motion()
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
        let shadow_pipeline = if self.vsm_render_pages.is_empty() {
            None
        } else {
            self.pipelines.request_shadow_depth_executor()
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
            let gbuffer = self.pipelines.request_gbuffer_executor();
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
        let want_cloud_motion =
            self.clouds.settings().enabled && self.view_mode != ViewMode::CloudDensity;
        let want_motion =
            (self.aa.taa() || want_ssgi || want_dfao || want_cloud_motion) && have_motion_targets;
        let motion = if want_motion {
            self.pipelines.request_motion_executor()
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
        // Bloom composites into scene-linear `color` before the tonemap; resolve its PSO only when
        // enabled so a disabled bloom pays nothing.
        let bloom = if self.bloom_enabled {
            self.pipelines
                .request_bloom(self.descriptors.bloom_set_layout())
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
        let (reactive_coverage, reactive_transition) = if self.aa.taa() {
            (
                self.pipelines.request_reactive_coverage(),
                self.pipelines.request_reactive_transition(),
            )
        } else {
            (None, None)
        };

        // The final post chain: the tonemap is mandatory (resolved every frame); the grid
        // arms only when shown; the overlay PSOs arm only when geometry is queued. The
        // overlay's per-frame vertex buffer is prepared (grown + uploaded) here, before the
        // graph build, so the pass captures only the resolved handle (README §2).
        let tonemap = self.pipelines.request_tonemap();
        // Aerial perspective fills + composites only when authored AND the atmosphere baked its LUTs
        // (no LUTs → nothing to march); that gate is the atmosphere's own `enabled`, not a second flag.
        let ap_active = self.fog.aerial_perspective && self.scene_ibl().atmosphere_live();
        let cloud_active = self.clouds.settings().enabled;
        // Height fog is the one composite for fog, aerial perspective, and lit clouds.
        let fog = if self.view_mode != ViewMode::CloudDensity
            && (self.fog.enabled || ap_active || cloud_active)
        {
            self.pipelines.request_fog()
        } else {
            None
        };
        let cloud_weather = if cloud_active && self.clouds.weather_dirty() {
            self.pipelines
                .request_cloud_weather(self.clouds.weather_layout())
        } else {
            None
        };
        let cloud_debug = if cloud_active && self.view_mode == ViewMode::CloudDensity {
            self.pipelines
                .request_cloud_debug(self.clouds.debug_layout())
        } else {
            None
        };
        let (cloud_raymarch, cloud_reconstruct, cloud_upscale) =
            if cloud_active && self.view_mode != ViewMode::CloudDensity {
                (
                    self.pipelines
                        .request_cloud_raymarch(self.clouds.raymarch_layout()),
                    self.pipelines
                        .request_cloud_reconstruct(self.clouds.reconstruct_layout()),
                    self.pipelines
                        .request_cloud_upscale(self.clouds.upscale_layout()),
                )
            } else {
                (None, None, None)
            };
        let cloud_shadow = if cloud_active && self.clouds.settings().cast_cloud_shadows {
            self.pipelines
                .request_cloud_shadow(self.clouds.shadow_layout())
        } else {
            None
        };
        // The froxel inject/integrate PSOs arm only when volumetric fog is authored this frame.
        let (fog_inject, fog_integrate) = if self.fog.enabled && self.fog.volumetric {
            let volume_layout = self.froxel.inject_volume_layout();
            let integrate_layout = self.froxel.integrate_layout();
            (
                self.pipelines.request_fog_inject(volume_layout),
                self.pipelines.request_fog_integrate(integrate_layout),
            )
        } else {
            (None, None)
        };
        // The aerial-perspective fill PSO arms only while AP is live this frame.
        let aerial = if ap_active {
            let fill_layout = self.aerial.fill_layout();
            self.pipelines.request_aerial(fill_layout)
        } else {
            None
        };
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
            depth_prepass_tess,
            gbuffer_tess,
            motion_tess,
            cull: cull_pipeline,
            skin: skin_pipeline,
            morph: morph_pipeline,
            shadow: shadow_pipeline,
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
            bloom,
            tonemap,
            fog,
            cloud_weather,
            cloud_debug,
            cloud_raymarch,
            cloud_reconstruct,
            cloud_upscale,
            cloud_shadow,
            fog_inject,
            fog_integrate,
            aerial,
            scene_resolve,
            depth_upscale,
            reactive_coverage,
            reactive_transition,
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
        plant_validation_error(&raw, command_buffer);

        // Timestamp queries are uninitialized until reset; reset this slot's pool(s) before the
        // graph writes into it (reading an unreset pool risks device loss). A no-op when the
        // profiler is `Off`.
        self.reset_profiler_pools(command_buffer, frame);

        // This prefix owns query-pool resets and the validation probe. Every async-compute
        // batch waits for its timeline point before writing timestamps.
        checked(
            unsafe { raw.end_command_buffer(command_buffer) },
            "end_command_buffer (scene prefix)",
        )?;

        let recorded = self.record_scene_graph(frame, frame_pipelines)?;

        checked(
            unsafe { raw.begin_command_buffer(recorded.tail, &begin_info) },
            "begin_command_buffer (scene tail)",
        )?;

        // Fold the active view's BGRA8 shm-publish readback into THIS frame's command buffer
        // when the view's shm publish is enabled — one submit covers it, with no separate
        // submit and no synchronous wait.
        if self.shm_publish_enabled[self.active_view.index()] {
            self.record_shm_copy(recorded.tail, frame)?;
        }

        // Re-borrow the device after the `&mut self` graph build above.
        let raw = self.device.raw();
        // SAFETY: the ash seam. Ends the recording opened above.
        checked(
            unsafe { raw.end_command_buffer(recorded.tail) },
            "end_command_buffer (scene tail)",
        )?;

        // CPU span over the frame's queue submit. A no-op when the profiler is `Off`.
        let profile_cpu = self.gpu_profiler.mode != ProfilerMode::Off;
        let submit_span = if profile_cpu {
            let CpuProfiler { registry, buffers } = &mut self.cpu_profiler;
            Some(buffers[frame].begin_span(registry, "submit-present", cpu_now_ns()))
        } else {
            None
        };
        let queue_submits = self.submit_scene_graph(frame, command_buffer, recorded)?;
        if let Some(index) = submit_span {
            let CpuProfiler { buffers, .. } = &mut self.cpu_profiler;
            buffers[frame].end_span(index, cpu_now_ns());
        }
        self.stats.command_buffers = queue_submits;
        self.stats.queue_submits = queue_submits;
        self.frames.advance();
        Ok(())
    }

    fn submit_scene_graph(
        &mut self,
        frame: usize,
        prefix: vk::CommandBuffer,
        recorded: RecordedSceneGraph,
    ) -> Result<u32> {
        let prefix_point = self.frames.reserve_timeline(RgQueueAssignment::Graphics)?;
        submit_graph_command(
            &self.device,
            GraphCommandSubmission {
                queue: RgQueueAssignment::Graphics,
                command_buffer: prefix,
                waits: &[],
                signals: &[prefix_point],
                binary_signal: None,
                fence: vk::Fence::null(),
                context: "queue_submit2 (scene prefix)",
            },
        )?;

        let mut batch_points = Vec::with_capacity(recorded.batches.len());
        for batch in &recorded.batches {
            let point = self.frames.reserve_timeline(batch.queue)?;
            let mut waits = Vec::new();
            if batch.queue == RgQueueAssignment::AsyncCompute {
                merge_timeline_point(&mut waits, prefix_point);
            }
            for &source_batch in &batch.wait_for_batches {
                let source = *batch_points.get(source_batch).ok_or_else(|| {
                    Error::InvalidUploadData(
                        "render-graph batch dependency does not precede its consumer".into(),
                    )
                })?;
                merge_timeline_point(&mut waits, source);
            }
            submit_graph_command(
                &self.device,
                GraphCommandSubmission {
                    queue: batch.queue,
                    command_buffer: batch.command_buffer,
                    waits: &waits,
                    signals: &[point],
                    binary_signal: None,
                    fence: vk::Fence::null(),
                    context: "queue_submit2 (render-graph batch)",
                },
            )?;
            batch_points.push(point);
        }

        let mut tail_waits = Vec::new();
        if let Some(point) =
            recorded
                .batches
                .iter()
                .zip(&batch_points)
                .rev()
                .find_map(|(batch, point)| {
                    (batch.queue == RgQueueAssignment::AsyncCompute).then_some(*point)
                })
        {
            merge_timeline_point(&mut tail_waits, point);
        }
        let present_signal = match self.present_sync.as_ref() {
            Some(present_sync) => present_sync.scene_finished_to_signal(frame)?,
            None => None,
        };
        submit_graph_command(
            &self.device,
            GraphCommandSubmission {
                queue: RgQueueAssignment::Graphics,
                command_buffer: recorded.tail,
                waits: &tail_waits,
                signals: &[],
                binary_signal: present_signal,
                fence: self.frames.in_flight(),
                context: "queue_submit2 (scene tail)",
            },
        )?;
        if present_signal.is_some()
            && let Some(present_sync) = self.present_sync.as_mut()
        {
            present_sync.mark_scene_finished_signaled(frame)?;
        }

        u32::try_from(recorded.batches.len().saturating_add(2)).map_err(|_| {
            Error::InvalidUploadData("render-graph submit count exceeds u32".to_owned())
        })
    }

    /// Builds and records the depth-prepass + scene render graph for `frame` into the
    /// active view's offscreen target. The pass bodies capture resolved handles + the
    /// moved draw list / submissions, never `&mut self`.
    ///
    /// The frame's directional virtual-shadow step, before the light UBO write:
    /// rebuild the snapped space, invalidate levels whose windows moved, seed the
    /// conservative camera-centred demand, stage this frame's dirty pages, and
    /// publish the page table the sampler reads.
    /// Marks every resident virtual-shadow page a swept world AABB overlaps as
    /// dirty, across the directional levels and the armed spot/point spaces. The
    /// projection is conservative (bounds sphere through each space); marking an
    /// absent page is a no-op.
    fn dirty_vsm_swept_bounds(&mut self, min: [f32; 3], max: [f32; 3]) {
        use saffron_geometry::glam::{Vec3, Vec4};
        let center = Vec3::new(
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        );
        let half_diag = Vec3::new(max[0] - min[0], max[1] - min[1], max[2] - min[2]).length() * 0.5;
        let light_center = self.vsm_space.basis.transform_point3(center);
        for level in 0..crate::VSM_DIRECTIONAL_LEVELS {
            let window = &self.vsm_space.levels[level as usize];
            if window.extent_m <= 0.0 {
                continue;
            }
            let page_m = window.extent_m / crate::VSM_LEVEL_PAGES as f32;
            let lo = [
                (light_center.x - half_diag - window.origin_light[0]) / page_m,
                (light_center.y - half_diag - window.origin_light[1]) / page_m,
            ];
            let hi = [
                (light_center.x + half_diag - window.origin_light[0]) / page_m,
                (light_center.y + half_diag - window.origin_light[1]) / page_m,
            ];
            if hi[0] < 0.0
                || hi[1] < 0.0
                || lo[0] >= crate::VSM_LEVEL_PAGES as f32
                || lo[1] >= crate::VSM_LEVEL_PAGES as f32
            {
                continue;
            }
            let x0 = lo[0].max(0.0) as u32;
            let y0 = lo[1].max(0.0) as u32;
            let x1 = (hi[0].min(crate::VSM_LEVEL_PAGES as f32 - 1.0)) as u32;
            let y1 = (hi[1].min(crate::VSM_LEVEL_PAGES as f32 - 1.0)) as u32;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    self.vsm_residency
                        .mark_dirty(crate::VsmPageKey::Directional { level, x, y });
                }
            }
        }
        let projective = |matrix: saffron_geometry::glam::Mat4,
                          pages: u32,
                          residency: &mut crate::VsmResidency,
                          key: &dyn Fn(u32, u32) -> crate::VsmPageKey| {
            let clip = matrix * center.extend(1.0);
            if clip.w <= 0.0 {
                return;
            }
            // Row norms of the upper 3x3 bound how fast NDC moves per world metre.
            let rx = Vec3::new(matrix.x_axis.x, matrix.y_axis.x, matrix.z_axis.x).length();
            let ry = Vec3::new(matrix.x_axis.y, matrix.y_axis.y, matrix.z_axis.y).length();
            let ndc = clip.truncate() / clip.w;
            let radius = Vec4::new(rx, ry, 0.0, 0.0) * half_diag / clip.w;
            let lo = [
                ((ndc.x - radius.x) * 0.5 + 0.5) * pages as f32,
                ((ndc.y - radius.y) * 0.5 + 0.5) * pages as f32,
            ];
            let hi = [
                ((ndc.x + radius.x) * 0.5 + 0.5) * pages as f32,
                ((ndc.y + radius.y) * 0.5 + 0.5) * pages as f32,
            ];
            if hi[0] < 0.0 || hi[1] < 0.0 || lo[0] >= pages as f32 || lo[1] >= pages as f32 {
                return;
            }
            let x0 = lo[0].max(0.0) as u32;
            let y0 = lo[1].max(0.0) as u32;
            let x1 = (hi[0].min(pages as f32 - 1.0)) as u32;
            let y1 = (hi[1].min(pages as f32 - 1.0)) as u32;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    residency.mark_dirty(key(x, y));
                }
            }
        };
        if self.lighting.spot_shadow_pending() {
            projective(
                self.lighting.spot_shadow_view_proj(),
                crate::vsm::VSM_SPOT_PAGES,
                &mut self.vsm_residency,
                &|x, y| crate::VsmPageKey::Spot { x, y },
            );
        }
        if self.lighting.point_shadow_pending() {
            let faces = crate::point_shadow_face_matrices(
                self.lighting.point_shadow_pos(),
                self.lighting.point_shadow_far(),
            );
            for (face, matrix) in faces.iter().enumerate() {
                let face = face as u32;
                projective(
                    *matrix,
                    crate::vsm::VSM_POINT_FACE_PAGES,
                    &mut self.vsm_residency,
                    &|x, y| crate::VsmPageKey::PointFace { face, x, y },
                );
            }
        }
    }

    fn prepare_vsm_frame(&mut self, frame: usize, sun_direction: saffron_geometry::glam::Vec3) {
        // One global atlas serves the scene view; preview/thumbnail lighting keeps
        // its own fixed behaviour without thrashing the residency.
        if self.active_view.index() != 0 {
            return;
        }
        // The master shadow toggle: off publishes a disabled table (every sampler
        // reads unshadowed via the `vsmParams.z` gate, so the demand marker writes
        // nothing) and stages no pages.
        if !self.lighting.use_shadows {
            self.vsm_render_pages.clear();
            self.lighting.set_frame_vsm(
                saffron_geometry::glam::Mat4::IDENTITY,
                [saffron_geometry::glam::Vec4::ZERO; crate::VSM_DIRECTIONAL_LEVELS as usize],
                saffron_geometry::glam::Vec4::ZERO,
                0,
            );
            return;
        }
        // Pages staged last frame that the graph never rasterized stay dirty.
        for page in std::mem::take(&mut self.vsm_render_pages) {
            self.vsm_residency.mark_dirty(page.key);
        }
        let space = crate::VsmDirectionalSpace::build(sun_direction, self.page_demand_view().eye);
        let serial = self.frame_serial;
        self.vsm_residency.begin_frame(serial);
        for level in 0..crate::VSM_DIRECTIONAL_LEVELS {
            if space.levels[level as usize].snap != self.vsm_space.levels[level as usize].snap {
                self.vsm_residency
                    .invalidate_directional_level(level, serial);
            }
        }
        self.vsm_space = space;
        // Receiver-driven demand from the drained GPU requests, plus a coarse
        // bootstrap ring so the first frames (and un-marked regions) fall back to
        // the outermost level instead of nothing.
        {
            let bootstrap = crate::VSM_LEVEL_PAGES / 2;
            for y in (bootstrap - 4)..(bootstrap + 4) {
                for x in (bootstrap - 4)..(bootstrap + 4) {
                    let _ = self.vsm_residency.demand(
                        crate::VsmPageKey::Directional {
                            level: crate::VSM_DIRECTIONAL_LEVELS - 1,
                            x,
                            y,
                        },
                        serial,
                    );
                }
            }
        }
        // A moved/re-aimed spot invalidates its whole space (the projective pages
        // are meaningless under the new transform).
        let spot_matrix = self.lighting.spot_shadow_view_proj().to_cols_array();
        if self.vsm_spot_matrix != spot_matrix {
            self.vsm_spot_matrix = spot_matrix;
            self.vsm_residency.invalidate_spot(serial);
        }
        // A moved or re-ranged point light stales all six face spaces.
        let point_key = self
            .lighting
            .point_shadow_pos()
            .extend(self.lighting.point_shadow_far())
            .to_array();
        if self.vsm_point_key != point_key {
            self.vsm_point_key = point_key;
            self.vsm_residency.invalidate_point(serial);
        }
        // Dynamic content re-dirties the pages it overlaps; static pages stay
        // cached. Discrete movers arrive as swept bounds from the persistent
        // scene's instance deltas; continuous wind sway re-dirties the levels
        // fine enough to resolve it (the render budget paces the churn).
        let (moved, moved_overflow) = self.persistent_gpu_scene.take_moved_bounds();
        let wind_dynamic = self.scene_wind.speed > 0.0
            && self
                .wind_deform_records
                .contains_key(&self.active_view.gpu_scene_world().0);
        if wind_dynamic || moved_overflow {
            self.vsm_residency
                .mark_dynamic_dirty(crate::vsm::VSM_DYNAMIC_MAX_LEVEL);
        }
        for (min, max) in moved {
            self.dirty_vsm_swept_bounds(min, max);
        }
        for &index in &self.vsm_demanded {
            if let Some(key) = crate::vsm::vsm_demand_key(index) {
                let _ = self.vsm_residency.demand(key, serial);
            }
        }
        self.vsm_render_pages = self.vsm_residency.take_render_pages(64);
        let table = self
            .vsm_gpu
            .publish_table(&self.device, frame, &self.vsm_residency);
        let mut levels =
            [saffron_geometry::glam::Vec4::ZERO; crate::VSM_DIRECTIONAL_LEVELS as usize];
        for (slot, level) in levels.iter_mut().zip(self.vsm_space.levels.iter()) {
            *slot = saffron_geometry::glam::Vec4::new(
                level.origin_light[0],
                level.origin_light[1],
                level.extent_m,
                0.0,
            );
        }
        self.lighting.set_frame_vsm(
            self.vsm_space.basis,
            levels,
            saffron_geometry::glam::Vec4::new(
                self.vsm_space.center_forward,
                crate::vsm::VSM_DIRECTIONAL_HALF_DEPTH_M,
                1.0,
                0.0,
            ),
            table,
        );
    }

    /// The active view's world wind sway record buffer, once a frame has created it.
    /// Executor raster passes declare their device-address read on it through this.
    fn wind_records_handle(&self) -> Option<vk::Buffer> {
        self.wind_deform_records
            .get(&self.active_view.gpu_scene_world().0)
            .map(|records| records.buffer.handle())
    }

    /// Pass order (the `beginFrameGraph` slice this phase fills): `light-cull` (compute)
    /// → the virtual-shadow page passes (per-space cull/bin chains + atlas raster) →
    /// optional `depth-prepass` → `scene`. The graph derives every barrier from the
    /// declared usage; the atlas's cross-frame layout rides its external slot.
    fn record_scene_graph(
        &mut self,
        frame: usize,
        pipelines: FramePipelines,
    ) -> Result<RecordedSceneGraph> {
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
        let offscreen_state = view.offscreen.graph_state();
        let depth_image = view.depth.handle();
        let depth_view = view.depth.view();

        let bindless_set = self.descriptors.bindless_set();
        let light_set = self.lighting.light_set(frame);
        let instance_set = self.instancing.instance_set(frame);
        let ibl_set = self.scene_ibl().set(frame);
        let raw = self.device.raw().clone();
        // The tessellation seam owns displaced instances this frame: the traversal
        // skips their records and the tess indirect draws render the amplified
        // geometry.
        let tess_seam = u32::from(!self.scene_draw_list.tess_buckets.is_empty());
        // The mesh fragments read the packed material-parameter blocks (set 2,
        // binding 2) from the global arena the mirror uploads into; the arena's
        // buffer changes on growth, so the binding rewrites every frame.
        self.descriptors.write_storage_buffer(
            self.instancing.instance_set(frame),
            2,
            self.global_gpu_data.material_parameters.buffer(),
            vk::WHOLE_SIZE,
        );

        let mut graph = RenderGraph::new();
        // Page residency runs before the transfer drain: the slot's fence has completed,
        // so its GPU missing-page requests are readable, and any ready payload publishes
        // into this frame's pending queue (parent-before-child inside publish_ready).
        self.page_residency.begin_frame();
        {
            let demanded = self.vsm_demand.drain(frame);
            if !demanded.is_empty() {
                self.vsm_demanded = demanded;
            }
        }
        for slot in self.gpu_scene_uploader.drain_page_requests(frame) {
            self.page_faults += 1;
            self.page_residency.demand_slot(slot, u64::MAX / 2);
        }
        self.page_residency.publish_ready(
            &mut self.global_gpu_data,
            &mut self.pending_gpu_scene_uploads,
        )?;
        // The GPU-scene transfer passes lead the frame: pending resident-record stages,
        // retirements, and arena bytes from the asset mirror, then the persistent scene's
        // coalesced slot writes, all before any pass that could consume the tables.
        crate::gpu_scene_upload::record_pending_global_uploads(
            &mut self.pending_gpu_scene_uploads,
            &self.device,
            &mut graph,
            &mut self.global_gpu_data,
            frame,
        )?;
        self.last_gpu_scene_upload = self.gpu_scene_uploader.record_frame(
            &self.device,
            &mut graph,
            &mut self.global_gpu_data,
            &mut self.persistent_gpu_scene,
            frame,
        )?;
        // The frame's instance-upload traffic is the GPU-scene table bytes staged this
        // frame — (near-)zero on a steady scene, the O(changes) guarantee.
        self.stats.instance_upload_bytes = self.last_gpu_scene_upload.table_bytes;
        // The wind deformation prepass output buffer: one sway record per instance
        // slot, (re)created behind an idle wait before the address block captures its
        // address. A fresh buffer is zero-filled in this frame's graph before any read.
        let wind_world = self.active_view.gpu_scene_world();
        let wind_capacity = self
            .gpu_scene_uploader
            .world_instance_capacity(wind_world)
            .max(1);
        let wind_records_created = self
            .wind_deform_records
            .get(&wind_world.0)
            .is_none_or(|records| records.capacity < wind_capacity);
        if wind_records_created {
            self.device.wait_idle()?;
            let buffer = crate::Buffer::new(
                self.device.resources(),
                u64::from(wind_capacity) * size_of::<crate::GpuWindInstanceRecord>() as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::AutoPreferDevice,
                    ..Default::default()
                },
            )?;
            self.wind_deform_records.insert(
                wind_world.0,
                WindDeformRecords {
                    buffer,
                    capacity: wind_capacity,
                },
            );
        }
        let wind_records_handle = self.wind_deform_records[&wind_world.0].buffer.handle();
        let wind_records_address = self.device.buffer_device_address(wind_records_handle);
        // The world interaction field: fixed size, created once per world; the
        // impulse ring holds this frame's staged impulses for the step pass.
        let interaction_created = !self.interaction_fields.contains_key(&wind_world.0);
        if interaction_created {
            let buffer = crate::Buffer::new(
                self.device.resources(),
                crate::GPU_INTERACTION_FIELD_BYTES,
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_DST
                    | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::AutoPreferDevice,
                    ..Default::default()
                },
            )?;
            self.interaction_fields.insert(wind_world.0, buffer);
        }
        let interaction_handle = self.interaction_fields[&wind_world.0].handle();
        let interaction_address = self.device.buffer_device_address(interaction_handle);
        while self.interaction_impulse_ring.len() < crate::MAX_FRAMES_IN_FLIGHT {
            self.interaction_impulse_ring.push(crate::Buffer::new(
                self.device.resources(),
                256 * size_of::<crate::InteractionImpulse>() as u64,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &vk_mem::AllocationCreateInfo {
                    usage: vk_mem::MemoryUsage::Auto,
                    flags: vk_mem::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE
                        | vk_mem::AllocationCreateFlags::MAPPED,
                    ..Default::default()
                },
            )?);
        }
        self.interaction_impulses.truncate(256);
        let interaction_impulse_count = self.interaction_impulses.len() as u32;
        if interaction_impulse_count > 0 {
            let ring = &self.interaction_impulse_ring[frame];
            // SAFETY: HOST_VISIBLE + MAPPED; the frame slot's fence passed before reuse.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.interaction_impulses.as_ptr().cast::<u8>(),
                    ring.mapped_ptr(),
                    interaction_impulse_count as usize * size_of::<crate::InteractionImpulse>(),
                );
            }
        }
        let interaction_impulse_address = self
            .device
            .buffer_device_address(self.interaction_impulse_ring[frame].handle());
        self.interaction_impulses.clear();
        let address_block = self.gpu_scene_uploader.build_address_block(
            &self.device,
            &self.global_gpu_data,
            self.active_view.gpu_scene_world(),
            frame,
            self.skinning.frame_deformed_addresses(frame, &self.device),
            wind_records_address,
            interaction_address,
            self.views[self.active_view.index()].jitter_index,
        );
        self.gpu_scene_uploader
            .write_address_block(frame, address_block);
        let wind_records_res = graph.import_buffer(wind_records_handle, None);
        let interaction_field_res = graph.import_buffer(interaction_handle, None);
        if wind_records_created || interaction_created {
            let raw = self.device.raw().clone();
            let clear_records = wind_records_created.then_some(wind_records_handle);
            let clear_field = interaction_created.then_some(interaction_handle);
            let mut clear = crate::RgPass::compute("wind-clear");
            if clear_records.is_some() {
                clear = clear.access(wind_records_res, crate::RgUsage::TransferWrite);
            }
            if clear_field.is_some() {
                clear = clear.access(interaction_field_res, crate::RgUsage::TransferWrite);
            }
            graph.add_pass(clear.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. Both buffers are TRANSFER_DST.
                unsafe {
                    if let Some(handle) = clear_records {
                        raw.cmd_fill_buffer(cmd, handle, 0, vk::WHOLE_SIZE, 0);
                    }
                    if let Some(handle) = clear_field {
                        raw.cmd_fill_buffer(cmd, handle, 0, vk::WHOLE_SIZE, 0);
                    }
                }
            }));
        }

        // Instance visibility, first half: swap the HZB ping-pong for this frame, size
        // the per-view lists to the world's instance capacity (an idle wait on growth),
        // write the frame bindings, and record the clear + cull passes — established
        // instances test the previous pyramid with previous transforms.
        // The shared blade-template words every binning scatter needs: index count +
        // first index within the pages arena (u32 units).
        let micro_template = (
            crate::MICRO_BLADE_INDEX_COUNT,
            self.global_gpu_data.micro_blade_template.first / 4,
        );
        let micro_field_psos = (
            self.pipelines
                .request_scene_micro_count(self.scene_visibility.micro_layout()),
            self.pipelines
                .request_scene_micro_scan(self.scene_visibility.micro_layout()),
            self.pipelines
                .request_scene_micro_scatter(self.scene_visibility.micro_layout()),
        );
        let wind_deform_pso = self
            .pipelines
            .request_wind_deform(self.scene_visibility.layout());
        let wind_interact_pso = self
            .pipelines
            .request_wind_interact(self.scene_visibility.layout());
        let vsm_demand_pso = self
            .pipelines
            .request_vsm_demand(self.vsm_demand.mark_layout());
        let vsm_compact_pso = self
            .pipelines
            .request_vsm_demand_compact(self.vsm_demand.compact_layout());
        let visibility_psos = (
            self.pipelines
                .request_scene_visibility(self.scene_visibility.layout()),
            self.pipelines
                .request_scene_traversal(self.scene_visibility.traversal_layout()),
            self.pipelines
                .request_scene_bin_count(self.scene_visibility.bin_count_layout()),
            self.pipelines
                .request_scene_bin_seed(self.scene_visibility.bin_seed_layout()),
            self.pipelines
                .request_scene_bin_scatter(self.scene_visibility.bin_scatter_layout()),
        );
        let transparent_sort_psos = (
            self.pipelines
                .request_transparent_keys(self.scene_visibility.transparent_keys_layout()),
            self.pipelines
                .request_radix_histogram(self.scene_visibility.radix_histogram_layout()),
            self.pipelines
                .request_radix_scan(self.scene_visibility.radix_scan_layout()),
            self.pipelines
                .request_radix_scatter(self.scene_visibility.radix_scatter_layout()),
            self.pipelines
                .request_transparent_reorder(self.scene_visibility.transparent_reorder_layout()),
        );
        let mut visibility_active = false;
        let mut visibility_history_valid = false;
        let mut executor_buckets: Vec<crate::ExecutorBucket> = Vec::new();
        let mut executor_inputs: Option<crate::ExecutorDrawInputs> = None;
        if self.views[self.active_view.index()].hzb_pyramid.is_none() {
            // Views that never pass through the resize hook (a fixed-size offscreen
            // boot) build their pyramids on first use.
            let extent = self.views[self.active_view.index()].scaled_render_extent();
            self.views[self.active_view.index()].hzb_pyramid =
                match crate::HzbPyramid::new(&self.device, &self.descriptors, &self.hzb, extent) {
                    Ok(pyramid) => Some(pyramid),
                    Err(err) => {
                        tracing::error!("hzb pyramid bring-up: {err}");
                        None
                    }
                };
        }
        if let (Some(cull_pso), Some(_), Some(_), Some(_), Some(_)) = (
            &visibility_psos.0,
            &visibility_psos.1,
            &visibility_psos.2,
            &visibility_psos.3,
            &visibility_psos.4,
        ) && self.views[self.active_view.index()].hzb_pyramid.is_some()
        {
            let instance_capacity = address_block.instance_capacity.max(1);
            // The frame's draw buckets derive from the mirror's live (shader, class)
            // pairs alone; the blend subset sizes the sorted transparent stream (one
            // full-length slice per blend bucket), so a new blend bucket going live
            // rebuilds the view's lists exactly like instance-capacity growth.
            let (frame_buckets, bucket_table) = crate::build_executor_buckets(
                &self.live_executor_bins,
                crate::SCENE_VISIBILITY_RECORD_CAPACITY,
            );
            let blend_keys: Vec<u32> = frame_buckets
                .iter()
                .filter(|bucket| {
                    crate::bucket_material(&self.global_gpu_data.executor_shaders, **bucket).blend
                })
                .map(|bucket| (bucket.shader_index << 16) | (bucket.pso_bin & 0xFFFF))
                .collect();
            let blend_group_count = (blend_keys.len() as u32).max(1);
            let needs_lists = self.views[self.active_view.index()]
                .visibility_view
                .as_ref()
                .is_none_or(|lists| {
                    lists.capacity() < instance_capacity
                        || lists.transparent_group_capacity() < blend_group_count
                });
            if needs_lists {
                self.device.wait_idle()?;
                if let Some(mut old_lists) =
                    self.views[self.active_view.index()].visibility_view.take()
                {
                    old_lists.free_sets(&self.descriptors);
                }
                self.views[self.active_view.index()].visibility_view =
                    match crate::SceneVisibilityView::new(
                        &self.device,
                        &self.descriptors,
                        &self.scene_visibility,
                        instance_capacity,
                        crate::SCENE_VISIBILITY_RECORD_CAPACITY,
                        blend_group_count,
                    ) {
                        Ok(lists) => Some(lists),
                        Err(err) => {
                            tracing::error!("visibility lists rebuild: {err}");
                            None
                        }
                    };
            }
            if let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_mut() {
                pyramid.begin_frame();
            }
            if let Some(lists) = self.views[self.active_view.index()]
                .visibility_view
                .as_ref()
            {
                // The slot's fence completed before this frame reused it, so its
                // readback words are last use's final counters.
                self.visibility_counters = lists.read_counters(frame);
                // The GPU decides the frame's draws; the stats mirror the slot's
                // last-use readback: emitted records = indirect draw commands,
                // visible instances, and the traversal's rasterized-triangle count.
                self.stats.draw_calls =
                    self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_RECORDS];
                self.stats.instances =
                    self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_VISIBLE];
                self.stats.triangles =
                    self.visibility_counters[crate::SCENE_VISIBILITY_COUNTER_TRIANGLES];
            }
            let view_index = self.active_view.index();
            let (previous_view, previous_image, previous_layout, previous_valid) = {
                let pyramid = self.views[view_index]
                    .hzb_pyramid
                    .as_ref()
                    .expect("pyramid checked above");
                let (image, view) = pyramid.previous();
                (
                    view,
                    image,
                    pyramid.previous_layout(),
                    pyramid.previous_valid(),
                )
            };
            visibility_history_valid =
                previous_valid && self.views[view_index].prev_view_proj_valid;
            if let Some(lists) = self.views[view_index].visibility_view.as_ref() {
                let hzb_pyramid = self.views[view_index]
                    .hzb_pyramid
                    .as_ref()
                    .expect("pyramid checked above");
                let address_slice = (
                    self.gpu_scene_uploader.address_buffer(),
                    frame as u64 * self.gpu_scene_uploader.address_block_stride(),
                    size_of::<crate::GpuSceneAddressBlock>() as u64,
                );
                lists.write_frame_bindings(
                    &self.device,
                    &self.scene_visibility,
                    frame,
                    previous_view,
                    hzb_pyramid.current().1,
                    address_slice,
                );
                // The executor vertex path indexes the record stream through the
                // instance set (binding 4).
                self.descriptors.write_storage_buffer(
                    self.instancing.instance_set(frame),
                    4,
                    lists.records(frame),
                    u64::from(lists.record_capacity()) * size_of::<crate::GpuDrawRecord>() as u64,
                );
                // The kernels look records up in the bucket table, so it publishes
                // before the binning passes execute.
                lists.write_bucket_table(frame, &bucket_table);
                executor_buckets = frame_buckets;
                executor_inputs =
                    Some(lists.executor_draw_inputs(frame, self.live_draw_record_bound));
                let camera_view = self.ssao.view();
                let camera_proj = self.ssao.inv_projection().inverse();
                let view_proj = (camera_proj * camera_view).to_cols_array();
                let prev_view_proj = if visibility_history_valid {
                    self.views[view_index].prev_view_proj.to_cols_array()
                } else {
                    view_proj
                };
                let previous_res = graph.import_image(
                    previous_image,
                    previous_view,
                    vk::ImageAspectFlags::COLOR,
                    previous_layout,
                    None,
                );
                // The interaction-field step runs first (scroll reset + impulse
                // splat + damped integration), then the wind deformation prepass
                // samples it into the sway records the cull and every raster pass
                // read.
                if let Some(interact_pso) = &wind_interact_pso {
                    let wind = self.lighting.wind_deform_push();
                    let eye = self.page_demand_view().eye;
                    let center_for = |cascade: u32| {
                        let texel = 0.25_f32 * (1u32 << (2 * cascade)) as f32;
                        [
                            (eye.x / texel).floor() as i32,
                            (eye.z / texel).floor() as i32,
                        ]
                    };
                    lists.add_wind_interact_pass(
                        &self.device,
                        &mut graph,
                        interact_pso,
                        frame,
                        interaction_field_res,
                        crate::WindInteractPush {
                            field: interaction_address,
                            impulses: interaction_impulse_address,
                            center0: center_for(0),
                            center1: center_for(1),
                            impulse_count: interaction_impulse_count,
                            dt: (wind.time_current - wind.time_previous).max(0.0),
                            reserved: [0; 2],
                        },
                    );
                }
                if let Some(wind_pso) = &wind_deform_pso {
                    lists.add_wind_deform_pass(
                        &self.device,
                        &mut graph,
                        wind_pso,
                        frame,
                        wind_records_res,
                        interaction_field_res,
                        instance_capacity,
                        self.lighting.wind_deform_push(),
                    );
                }
                lists.add_cull_pass(
                    &self.device,
                    &mut graph,
                    cull_pso,
                    frame,
                    previous_res,
                    wind_records_res,
                    instance_capacity,
                    crate::SceneVisibilityPush {
                        view_proj,
                        prev_view_proj,
                        hzb_extent: [hzb_pyramid.extent().width, hzb_pyramid.extent().height],
                        hzb_mip_count: hzb_pyramid.mip_count(),
                        pass_kind: crate::SCENE_VISIBILITY_PASS_CULL,
                        history_valid: u32::from(visibility_history_valid),
                        list_capacity: lists.capacity(),
                        reserved: [0; 2],
                    },
                );
                visibility_active = true;
                // Stages 2-3: traverse the culled instances (classified against the
                // previous pyramid) into the record stream, then bin into indirect
                // commands — the provisional cut the raster passes consume.
                if let Some(traversal_pso) = &visibility_psos.1 {
                    let demand = self.page_demand_view();
                    lists.add_traversal_pass(
                        &self.device,
                        &mut graph,
                        traversal_pso,
                        frame,
                        crate::SceneTraversalPush {
                            eye: demand.eye.to_array(),
                            proj_scale: demand.proj_scale,
                            error_threshold_px: 1.0,
                            record_capacity: lists.record_capacity(),
                            list_capacity: lists.capacity(),
                            survivor: 0,
                            tess_seam,
                            transition_frames: crate::GPU_TRANSITION_FRAMES,
                            frame_stamp: self.frame_serial as u32,
                            reserved0: 0,
                        },
                    );
                    // Micro-field reconstruction appends blade records to the same
                    // stream before binning; the binning re-reads the total.
                    if let (
                        Some((directory_offset, directory_count)),
                        (Some(micro_count), Some(micro_scan), Some(micro_scatter)),
                    ) = (
                        self.micro_field_directory,
                        (
                            micro_field_psos.0.as_ref(),
                            micro_field_psos.1.as_ref(),
                            micro_field_psos.2.as_ref(),
                        ),
                    ) {
                        lists.add_micro_field_passes(
                            &self.device,
                            &mut graph,
                            (micro_count, micro_scan, micro_scatter),
                            frame,
                            self.global_gpu_data.micro_candidates.handle(),
                            interaction_field_res,
                            {
                                let wind = self.lighting.wind_deform_push();
                                crate::SceneMicroFieldPush {
                                    view_proj,
                                    eye: demand.eye.to_array(),
                                    max_distance: 96.0,
                                    directory_offset,
                                    directory_count,
                                    record_capacity: lists.record_capacity(),
                                    candidate_capacity: crate::SCENE_MICRO_CANDIDATE_CAPACITY,
                                    frame_base: frame as u32
                                        * crate::SCENE_MICRO_CANDIDATE_CAPACITY,
                                    reserved: [0; 3],
                                    wind_dir_speed_gust: wind.dir_speed_gust,
                                    wind_params: wind.params,
                                    wind_octaves: wind.octaves,
                                    wind_seed: wind.seed,
                                    wind_time_current: wind.time_current,
                                    wind_time_previous: wind.time_previous,
                                    wind_sources: wind.sources,
                                    wind_source_count: wind.source_count,
                                    wind_reserved: 0,
                                }
                            },
                        );
                    }
                }
                if let (Some(bin_count), Some(bin_scan), Some(bin_scatter)) =
                    (&visibility_psos.2, &visibility_psos.3, &visibility_psos.4)
                {
                    lists.add_binning_passes(
                        &self.device,
                        &mut graph,
                        (bin_count, bin_scan, bin_scatter),
                        frame,
                        false,
                        micro_template,
                    );
                }
                // The transparent back-to-front sort follows the binning: per blend
                // bucket, the sorted command slice the scene pass's translucent scope
                // draws. Nothing blend-routed this frame skips the sort outright.
                if let (Some(keys), Some(histogram), Some(scan), Some(scatter), Some(reorder)) = (
                    &transparent_sort_psos.0,
                    &transparent_sort_psos.1,
                    &transparent_sort_psos.2,
                    &transparent_sort_psos.3,
                    &transparent_sort_psos.4,
                ) && !blend_keys.is_empty()
                {
                    let camera_view = self.ssao.view();
                    let row2 = camera_view.row(2);
                    lists.add_transparent_sort_passes(
                        &self.device,
                        &mut graph,
                        crate::TransparentSortPipelines {
                            keys,
                            histogram,
                            scan,
                            scatter,
                            reorder,
                        },
                        frame,
                        [row2.x, row2.y, row2.z, row2.w],
                        &blend_keys,
                    );
                }
            }
        }
        // F1: resolve each frame bucket's executor mesh PSO for the pass bodies (the
        // borrow of `self.pipelines` must not overlap the visibility block's `lists`).
        let executor_draws: Vec<(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)> =
            executor_buckets
                .iter()
                .filter_map(|bucket| {
                    let material =
                        crate::bucket_material(&self.global_gpu_data.executor_shaders, *bucket);
                    self.pipelines
                        .request_executor_mesh_pipeline(&material, self.wireframe)
                        .map(|pso| (*bucket, material.blend, pso))
                })
                .collect();
        // The frame's live draw buckets, plus the tess seam's amplified draws on the
        // draw-call stat (each is one real recorded indirect draw in the scene pass).
        self.stats.batches = executor_draws.len() as u32;
        self.stats.draw_calls = self
            .stats
            .draw_calls
            .saturating_add(self.scene_draw_list.tess_draws.len() as u32);
        // Fold the executor buckets into the shadow draw-call stat: one recorded
        // counted-indirect draw per non-blend bucket per shadow pass this frame.
        let non_blend_buckets = executor_draws
            .iter()
            .filter(|(_, blend, _)| !*blend)
            .count() as u32;
        let shadow_passes = u32::try_from(self.vsm_render_pages.len()).unwrap_or(u32::MAX);
        self.stats.shadow_draw_calls = shadow_passes.saturating_mul(non_blend_buckets);
        let ibl_live = self.scene_ibl_mut().add_live_capture_passes(&mut graph);
        let ddgi_sh = if self.active_view == ViewId::Thumbnail {
            graph.import_buffer(self.ibl.sh_coefficients().handle(), None)
        } else {
            ibl_live.sh
        };

        // Light-cull (compute): cull the punctual lights into the froxel grid. The graph
        // emits the compute→fragment barrier on the cluster buffer from the declared
        // StorageWriteCompute usage (the scene fragment reads it as a storage buffer).
        if let Some(cull) = &pipelines.cull {
            let cluster_buffer = graph.import_buffer(self.lighting.cluster_buffer(frame), None);
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
        // The executor vertex paths read the micro-blade candidates through their
        // device address; every raster pass declares the read so the graph orders it
        // after the micro pass's compute write.
        let micro_candidates_res =
            graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
        let (deformed_res, prev_deformed_res) = if do_deform {
            let deformed = graph.import_buffer(deformed_handle.expect("deformed buffer"), None);
            let prev_deformed =
                graph.import_buffer(prev_deformed_handle.expect("prev-deformed buffer"), None);

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

        // Adaptive-tessellation prep (factor/scan/finalize/args): independent of the deform buffers
        // above (it writes its own transient scratch), so it records after the deform scope on the
        // same graph. Inert until Phase 4 emits from its predicted offsets. Returns the coarse RT VB/IB
        // graph resources (Phase 10, Q2) — declared `AccelStructBuildRead` on `tlas-build` below so the
        // graph derives the coarse-emit → BLAS-build barrier — when a displaced instance is RT-consumed.
        let tess_rt_res = self.record_tess_prep(&mut graph, frame, &raw);

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
        if self.rt.build_pending()
            && self.rt.has_instances(&deformed_rt)
            && let Some(plan) =
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
            if has_skinned_rt && let Some(deformed) = deformed_res {
                tlas_pass = tlas_pass.access(deformed, RgUsage::AccelStructBuildRead);
            }
            // The tessellated BLAS builds over the coarse (secondary-ray) VB/IB the tess-emit-rt pass
            // wrote; declaring the read here lets the graph derive the compute-write → AS-build-read
            // barrier and orders this pass after the coarse emit.
            if let Some((vb_rt, ib_rt)) = tess_rt_res {
                tlas_pass = tlas_pass
                    .access(vb_rt, RgUsage::AccelStructBuildRead)
                    .access(ib_rt, RgUsage::AccelStructBuildRead);
            }
            graph.add_pass(tlas_pass);
        }

        // Virtual-shadow pages: each dirty page rasterizes into its atlas tile
        // behind its space's own cull/traversal/bin chain. The scene pass declares
        // the returned atlas resource `SampledRead` so the graph derives the
        // DepthWrite -> ShaderReadOnly transition before the mesh samples it.
        let mut vsm_atlas_res: Option<RgResource> = None;
        if let (
            Some(shadow),
            Some(cull_pso),
            Some(traversal_pso),
            Some(bin_count),
            Some(bin_seed),
            Some(bin_scatter),
        ) = (
            &pipelines.shadow,
            &visibility_psos.0,
            &visibility_psos.1,
            &visibility_psos.2,
            &visibility_psos.3,
            &visibility_psos.4,
        ) {
            let vsm_capacity = address_block.instance_capacity.max(1);
            vsm_atlas_res = self.add_vsm_page_passes(
                &mut graph,
                frame,
                shadow,
                bindless_set,
                instance_set,
                deformed_res,
                &executor_draws,
                wind_records_res,
                cull_pso,
                traversal_pso,
                (bin_count, bin_seed, bin_scatter),
                micro_template,
                vsm_capacity,
            )?;
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
        let offscreen_slot = graph.alloc_external_state(offscreen_state);
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
            bindless_set,
            instance_set,
            (deformed_res, deformed_handle),
            (prev_deformed_res, prev_deformed_handle),
            executor_inputs,
            &executor_draws,
        ) {
            Some((motion, depth)) => (Some(motion), Some(depth)),
            None => (None, None),
        };

        // Resolve weather and fill the one camera-snapped cloud-shadow cascade array before any
        // mesh, cloud, or froxel consumer reads it.
        let cloud_frame = self.prepare_cloud_frame(&mut graph, &pipelines, frame);

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
            let sky_sh = self.scene_ibl().sh_coefficients();
            let sky_sh_handle = sky_sh.handle();
            let sky_sh_size = sky_sh.size();
            let ddgi_irr = self.ddgi.irradiance().1;
            let ddgi_dist = self.ddgi.distance().1;
            let ddgi_sampler = self.ddgi.sampler();
            let active = self.active_view.index();
            if let Some(ubo) = self.views[active].gi_params_ubos.get_mut(frame)
                && let Some(dst) = ubo.mapped_bytes()
            {
                let src = bytemuck::bytes_of(&gi_params);
                dst[..src.len()].copy_from_slice(src);
            }
            self.views[active].write_gi_resolve_shared(
                &self.device,
                frame,
                sky_sh_handle,
                sky_sh_size,
                ddgi_irr,
                ddgi_dist,
                ddgi_sampler,
            );
        }

        let screen = self.add_screen_space_passes(
            &mut graph,
            &pipelines,
            bindless_set,
            instance_set,
            motion_resource,
            (deformed_res, deformed_handle),
            light_set,
            gdf.cascades,
            gdf.occupancy,
            ibl_live.sh,
            executor_inputs,
            &executor_draws,
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
            &gdf,
            ddgi_sh,
        );

        // Visible sky: a fullscreen pass that fills the scene color target before the
        // geometry. It writes the SAME target the scene pass uses, owns the color clear when
        // present, and the scene pass then loads instead of clearing.
        let did_sky = self.sky.should_draw();
        if did_sky {
            let bindless = bindless_set;
            let raw_for_body = raw.clone();
            // The offscreen thumbnail view draws a fixed studio gradient instead of the scene's
            // sky, so the backdrop never samples the IBL cube — a subject (incl. a chrome ball
            // reflecting a dark HDRI) always has silhouette contrast. Interactive views keep their
            // submitted sky.
            let sky_mode_override = (self.active_view == ViewId::Thumbnail)
                .then_some(crate::ibl::SKY_MODE_THUMBNAIL_GRADIENT);
            let draw = self
                .sky
                .draw_data(self.scene_draw_list.view_proj, sky_mode_override);
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

            if self.active_view != ViewId::Thumbnail && self.sky.night().star_intensity > 0.0 {
                let draw =
                    self.stars
                        .draw_data(self.scene_draw_list.view_proj, extent, self.sky.night());
                let raw_for_body = raw.clone();
                let stars_pass = RgPass::graphics("stars", extent)
                    .color(color_load_store(scene_color_attachment))
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        crate::record_stars(&raw_for_body, cmd, &draw);
                    });
                graph.add_pass(stars_pass);
            }
        }

        let did_depth_prepass = pipelines.depth_prepass.is_some() && executor_inputs.is_some();
        if did_depth_prepass {
            let pipeline = pipelines
                .depth_prepass
                .as_ref()
                .expect("prepass PSO gated above");
            let inputs = executor_inputs.expect("executor inputs gated above");
            let raw_for_body = raw.clone();
            let pipeline = Arc::clone(pipeline);
            let prepass_handle = pipeline.handle();
            let prepass_layout = pipeline.layout();
            let bindless = bindless_set;
            let draws = executor_draws.clone();
            let push = self.scene_draw_list.view_proj;
            let pages_buffer = self.global_gpu_data.pages.buffer();
            let draw_count_supported = self.device.capabilities.draw_indirect_count;
            let tess_pso = pipelines.depth_prepass_tess.clone();
            let tess_draws = self.scene_draw_list.tess_draws.clone();
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            // The depth pre-pass writes the (multisampled, when MSAA) scene depth the scene
            // pass then loads — the same sample count the scene PSO bakes. Draws come from
            // the frame's binned executor commands over the pages-arena index stream.
            let mut depth_pass = RgPass::graphics("depth-prepass", extent)
                .depth_attachment(depth_clear_store(scene_depth))
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
            depth_pass = access_tess_draws(&mut graph, depth_pass, &tess_draws, false);
            let mut depth_pass = depth_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                record_executor_depth_family(
                    &raw_for_body,
                    cmd,
                    (prepass_handle, prepass_layout),
                    vk::ShaderStageFlags::VERTEX,
                    bytemuck::bytes_of(&push),
                    bindless,
                    instance_set,
                    inputs,
                    pages_buffer,
                    draw_count_supported,
                    &draws,
                    false,
                );
                if let Some(tess_pso) = &tess_pso {
                    crate::scene_pass::record_tess_depth_draws(
                        &raw_for_body,
                        cmd,
                        (tess_pso.handle(), tess_pso.layout()),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&push),
                        bindless,
                        instance_set,
                        &tess_draws,
                        false,
                    );
                }
                drop(pipeline);
            });
            depth_pass = depth_pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
            depth_pass = depth_pass.access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
            if let Some(deformed) = deformed_res {
                depth_pass = depth_pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
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

        // The survivor raster (visibility stage 6) redraws the retest survivors over
        // the provisional scene with both attachments LOADed, then the final HZB
        // rebuild publishes next frame's previous pyramid. Whether it runs decides the
        // scene pass's MSAA store ops: the multisampled samples must survive to the
        // survivor pass, which then owns the final resolve.
        let hzb_copy_pso = self.pipelines.request_hzb_copy(self.hzb.copy_layout());
        let hzb_reduce_pso = self.pipelines.request_hzb_reduce(self.hzb.reduce_layout());
        let survivor_planned = visibility_active
            && hzb_copy_pso.is_some()
            && hzb_reduce_pso.is_some()
            && visibility_psos.0.is_some()
            && visibility_psos.1.is_some()
            && visibility_psos.2.is_some()
            && visibility_psos.3.is_some()
            && visibility_psos.4.is_some()
            && executor_inputs.is_some();

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
        // multisampled samples are discarded, unless the survivor raster still draws
        // over them). The render graph's `resolve` is the MSAA resolve (color
        // `AVERAGE`, depth `SAMPLE_ZERO`).
        if msaa {
            color_att.store_op = if survivor_planned {
                vk::AttachmentStoreOp::STORE
            } else {
                vk::AttachmentStoreOp::DONT_CARE
            };
            color_att.resolve = Some(scene_output);
        }
        let mut depth_att = depth_clear_store(scene_depth);
        if did_depth_prepass {
            depth_att.load_op = vk::AttachmentLoadOp::LOAD;
        }
        // Persist the 1× scene depth for the post-tonemap overlay: store it directly (no
        // MSAA), or resolve the multisampled depth into the 1× target (MSAA samples
        // kept only for the survivor raster).
        if msaa {
            depth_att.store_op = if survivor_planned {
                vk::AttachmentStoreOp::STORE
            } else {
                vk::AttachmentStoreOp::DONT_CARE
            };
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
        // The scene pass binds the mesh roster once (sets 0, {1,2}, 3, 4, 5 plus one
        // each for the RT sets 6/7 when present) — constant in the draw count. Record
        // it for `render-stats` here, where the resolved sets are known, since the
        // pass body runs inside a graph closure whose return value is discarded.
        self.stats.descriptor_binds = crate::scene_pass::scene_draw_list_bind_count(
            executor_inputs.is_some() && !executor_draws.is_empty(),
            rt_mesh_set,
            restir_mesh_set,
        );
        let scene_sets = crate::MeshPassSets {
            bindless: bindless_set,
            light: light_set,
            instance: instance_set,
            ibl: ibl_set,
            ssao_mesh: ssao_mesh_set,
            ddgi_mesh: ddgi_mesh_set,
            rt_mesh: rt_mesh_set,
            restir_mesh: restir_mesh_set,
        };
        let scene_view_proj = self.scene_draw_list.view_proj;
        let scene_pages_buffer = self.global_gpu_data.pages.buffer();
        let scene_draw_count_supported = self.device.capabilities.draw_indirect_count;
        let scene_draws = executor_draws.clone();
        let scene_tess_draws = self.scene_draw_list.tess_draws.clone();
        let scene_transparent_commands = self.views[self.active_view.index()]
            .visibility_view
            .as_ref()
            .map(|lists| lists.transparent_commands(frame));
        let mut scene = RgPass::graphics("scene", extent)
            .color(color_att)
            .depth_attachment(depth_att)
            .body(move |_cmd, scopes: &mut NestedScopeRecorder| {
                let _ = &list;
                scopes.scope("scene-opaque", |cmd| {
                    if let Some(inputs) = executor_inputs {
                        crate::record_executor_buckets(
                            &raw_for_body,
                            cmd,
                            scene_view_proj,
                            scene_sets,
                            inputs,
                            scene_pages_buffer,
                            scene_draw_count_supported,
                            &scene_draws,
                            false,
                        );
                    }
                    // The tessellation seam: displaced instances' amplified draws
                    // composite with the opaque cut.
                    crate::record_tess_scene_draws(
                        &raw_for_body,
                        cmd,
                        scene_view_proj,
                        scene_sets,
                        &scene_tess_draws,
                        false,
                    );
                });
                scopes.scope("scene-submissions", |cmd| {
                    for body in submissions {
                        body(cmd);
                    }
                });
                // Translucent geometry composites last, over the resolved opaque scene +
                // submissions: per blend bucket, the GPU-sorted back-to-front command
                // slice with that bucket's blend PSO (depth-write off; same color +
                // depth attachments — no separate pass). A no-op when nothing is
                // translucent.
                scopes.scope("scene-translucent", |cmd| {
                    if let (Some(inputs), Some(transparent_commands)) =
                        (executor_inputs, scene_transparent_commands)
                    {
                        crate::record_executor_transparent_stream(
                            &raw_for_body,
                            cmd,
                            scene_view_proj,
                            scene_sets,
                            inputs,
                            scene_pages_buffer,
                            transparent_commands,
                            scene_draw_count_supported,
                            &scene_draws,
                        );
                    }
                    crate::record_tess_scene_draws(
                        &raw_for_body,
                        cmd,
                        scene_view_proj,
                        scene_sets,
                        &scene_tess_draws,
                        true,
                    );
                });
            });
        scene = access_tess_draws(&mut graph, scene, &self.scene_draw_list.tess_draws, false);
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(scene_pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            scene = scene
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
            if let Some(transparent_commands) = scene_transparent_commands {
                let transparent_res = graph.import_buffer(transparent_commands, None);
                scene = scene.access(transparent_res, RgUsage::IndirectCommandRead);
            }
        }
        // The scene fragment samples the AO / contact / SSGI maps via set 4; declare the
        // reads so the graph transitions each from GENERAL (compute write) → ShaderReadOnly
        // before the sample. The übershader gates them by flag, but the layout transition
        // is unconditional once the maps were storage-written this frame.
        for resource in &screen.scene_sampled {
            scene = scene.access(*resource, RgUsage::SampledRead);
        }
        scene = scene
            .access(ibl_live.sh, RgUsage::StorageReadFragment)
            .access(ibl_live.prefiltered, RgUsage::SampledRead);
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
        if let Some(occupancy) = gdf.occupancy {
            for volume in occupancy {
                scene = scene.access(volume, RgUsage::SampledRead);
            }
        }
        // When ReSTIR ran this frame, the resolve wrote the radiance image as storage
        // (GENERAL); declare the scene's SampledRead so the graph transitions it back to
        // ShaderReadOnly before the mesh sample (set 7).
        if let Some(radiance) = restir.radiance {
            scene = scene.access(radiance, RgUsage::SampledRead);
        }
        // The mesh fragment samples the virtual-shadow atlas via the light set;
        // declare the read so the graph transitions DepthWrite → ShaderReadOnly between
        // the page passes and the scene draw (else the sample sees a DEPTH_ATTACHMENT
        // image, `VUID-vkCmdDrawIndexed-imageLayout-00344`).
        if let Some(res) = vsm_atlas_res {
            scene = scene.access(res, RgUsage::SampledRead);
        }
        if let Some(cloud) = cloud_frame {
            scene = scene.access(cloud.shadow, RgUsage::SampledRead);
        }
        // A skinned draw pulls the deformed buffer through its device address; declare
        // the read so the graph orders the scene pass after the skin compute write.
        scene = scene.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
        scene = scene.access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
        if let Some(deformed) = deformed_res {
            scene = scene.access(deformed, RgUsage::ShaderDeviceAddressRead);
        }
        graph.add_pass(scene);

        // The HZB build follows the scene pass: seed mip 0 from the resolved 1x depth,
        // then the per-mip max reduction. Next frame's visibility reads this pyramid as
        // its previous; a missing PSO invalidates instead so tests bypass a stale pyramid.
        if let (Some(hzb_copy), Some(hzb_reduce)) = (hzb_copy_pso, hzb_reduce_pso) {
            // Stage-5 prologue: snapshot the provisional visible/record counts into
            // counter words 6/7 and zero the bucket counts, BEFORE the retest appends
            // survivors. Every consumer of the provisional cut is declared above, so
            // the graph orders their reads ahead of the clear.
            if survivor_planned
                && let Some(lists) = self.views[self.active_view.index()]
                    .visibility_view
                    .as_ref()
            {
                lists.add_survivor_snapshot_pass(&self.device, &mut graph, frame);
            }
            let hzb_depth_view = self.views[self.active_view.index()].depth.view();
            let current_hzb_res = self.views[self.active_view.index()]
                .hzb_pyramid
                .as_mut()
                .map(|pyramid| {
                    pyramid.write_depth_binding(&self.device, &self.hzb, hzb_depth_view);
                    pyramid.add_build_passes(
                        &self.device,
                        &mut graph,
                        (&hzb_copy, &hzb_reduce),
                        depth,
                    )
                });
            // Receiver demand for the virtual shadow map: mark needed pages from
            // the freshly seeded pyramid's full-resolution depth, then compact the
            // bitmap into the frame's request ring for the fence-time drain.
            if self.active_view.index() == 0
                && let (Some(mark_pso), Some(compact_pso), Some(hzb_res)) =
                    (&vsm_demand_pso, &vsm_compact_pso, current_hzb_res)
                && let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_ref()
            {
                let (_, current_view) = pyramid.current();
                self.vsm_demand.write_frame(
                    &self.device.raw().clone(),
                    frame,
                    self.lighting.frame_ubo(frame),
                    current_view,
                    self.scene_visibility.hzb_sampler(),
                );
                let extent = self.views[self.active_view.index()].scaled_render_extent();
                let inv_view_proj = self.scene_view_proj_unjittered().inverse();
                let (mark_set, compact_set) = self.vsm_demand.sets(frame);
                let raw_mark = self.device.raw().clone();
                let mark = Arc::clone(mark_pso);
                let mark_push = crate::VsmDemandPush {
                    inv_view_proj: inv_view_proj.to_cols_array(),
                    extent: [extent.width, extent.height],
                    reserved: [0; 2],
                };
                let groups = (extent.width.div_ceil(8), extent.height.div_ceil(8));
                graph.add_pass(
                    RgPass::compute("vsm-demand")
                        .access(hzb_res, RgUsage::StorageImageRwCompute)
                        .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                            // SAFETY: the ash seam; the PSO/set are valid this frame.
                            unsafe {
                                raw_mark.cmd_bind_pipeline(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    mark.handle(),
                                );
                                raw_mark.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    mark.layout(),
                                    0,
                                    &[mark_set],
                                    &[],
                                );
                                raw_mark.cmd_push_constants(
                                    cmd,
                                    mark.layout(),
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(&mark_push),
                                );
                                raw_mark.cmd_dispatch(cmd, groups.0, groups.1, 1);
                            }
                        }),
                );
                let raw_compact = self.device.raw().clone();
                let compact = Arc::clone(compact_pso);
                let compact_push = crate::VsmCompactPush {
                    capacity: crate::VSM_DEMAND_CAPACITY,
                    reserved: [0; 3],
                };
                let bitmap_res = graph.import_buffer(self.vsm_demand.bitmap_handle(), None);
                let ring_res = graph.import_buffer(self.vsm_demand.ring_handle(frame), None);
                graph.add_pass(
                    RgPass::compute("vsm-demand-compact")
                        .access(bitmap_res, RgUsage::StorageReadWriteCompute)
                        .access(ring_res, RgUsage::StorageReadWriteCompute)
                        .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                            // SAFETY: the ash seam; the PSO/set are valid this frame.
                            unsafe {
                                raw_compact.cmd_bind_pipeline(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    compact.handle(),
                                );
                                raw_compact.cmd_bind_descriptor_sets(
                                    cmd,
                                    vk::PipelineBindPoint::COMPUTE,
                                    compact.layout(),
                                    0,
                                    &[compact_set],
                                    &[],
                                );
                                raw_compact.cmd_push_constants(
                                    cmd,
                                    compact.layout(),
                                    vk::ShaderStageFlags::COMPUTE,
                                    0,
                                    bytemuck::bytes_of(&compact_push),
                                );
                                raw_compact.cmd_dispatch(cmd, 8, 1, 1);
                            }
                        }),
                );
            }

            // Instance visibility, stage 5: retest the occluded-established list
            // against the freshly built pyramid; survivors merge into the visible
            // list for the survivor traversal.
            if visibility_active && let Some(current_hzb_res) = current_hzb_res {
                let view_index = self.active_view.index();
                let camera_view = self.ssao.view();
                let camera_proj = self.ssao.inv_projection().inverse();
                let view_proj = (camera_proj * camera_view).to_cols_array();
                let lists = self.views[view_index]
                    .visibility_view
                    .as_ref()
                    .expect("visibility lists active");
                let hzb_pyramid = self.views[view_index]
                    .hzb_pyramid
                    .as_ref()
                    .expect("pyramid active");
                if let Some(cull_pso) = &visibility_psos.0 {
                    lists.add_retest_pass(
                        &self.device,
                        &mut graph,
                        cull_pso,
                        frame,
                        current_hzb_res,
                        wind_records_res,
                        crate::SceneVisibilityPush {
                            view_proj,
                            prev_view_proj: view_proj,
                            hzb_extent: [hzb_pyramid.extent().width, hzb_pyramid.extent().height],
                            hzb_mip_count: hzb_pyramid.mip_count(),
                            pass_kind: crate::SCENE_VISIBILITY_PASS_RETEST,
                            history_valid: u32::from(visibility_history_valid),
                            list_capacity: lists.capacity(),
                            reserved: [0; 2],
                        },
                    );
                }
                // Stage 6: traverse the retest survivors (the visible-list tail past
                // counter word 6) into records past word 7, re-bin them into the
                // re-seeded command slices, redraw them over the provisional scene
                // with both attachments LOADed, then rebuild the HZB so next frame's
                // previous pyramid holds the complete cut.
                if survivor_planned {
                    if let Some(traversal_pso) = &visibility_psos.1 {
                        let demand = self.page_demand_view();
                        lists.add_traversal_pass(
                            &self.device,
                            &mut graph,
                            traversal_pso,
                            frame,
                            crate::SceneTraversalPush {
                                eye: demand.eye.to_array(),
                                proj_scale: demand.proj_scale,
                                error_threshold_px: 1.0,
                                record_capacity: lists.record_capacity(),
                                list_capacity: lists.capacity(),
                                survivor: 1,
                                tess_seam,
                                transition_frames: crate::GPU_TRANSITION_FRAMES,
                                frame_stamp: self.frame_serial as u32,
                                reserved0: 0,
                            },
                        );
                    }
                    if let (Some(bin_count), Some(bin_scan), Some(bin_scatter)) =
                        (&visibility_psos.2, &visibility_psos.3, &visibility_psos.4)
                    {
                        lists.add_binning_passes(
                            &self.device,
                            &mut graph,
                            (bin_count, bin_scan, bin_scatter),
                            frame,
                            true,
                            micro_template,
                        );
                    }
                    if let Some(inputs) = executor_inputs {
                        let raw_for_body = raw.clone();
                        let survivor_draws = executor_draws.clone();
                        let survivor_pages = self.global_gpu_data.pages.buffer();
                        let survivor_count = self.device.capabilities.draw_indirect_count;
                        let survivor_view_proj = self.scene_draw_list.view_proj;
                        let mut survivor_color = color_load_store(scene_color_attachment);
                        let mut survivor_depth = depth_load_store(scene_depth);
                        if msaa {
                            survivor_color.store_op = vk::AttachmentStoreOp::DONT_CARE;
                            survivor_color.resolve = Some(scene_output);
                            survivor_depth.store_op = vk::AttachmentStoreOp::DONT_CARE;
                            survivor_depth.resolve = Some(depth);
                        }
                        let mut survivor_pass = RgPass::graphics("scene-survivors", extent)
                            .color(survivor_color)
                            .depth_attachment(survivor_depth)
                            .body(move |_cmd, scopes: &mut NestedScopeRecorder| {
                                scopes.scope("scene-survivors", |cmd| {
                                    crate::record_executor_buckets(
                                        &raw_for_body,
                                        cmd,
                                        survivor_view_proj,
                                        scene_sets,
                                        inputs,
                                        survivor_pages,
                                        survivor_count,
                                        &survivor_draws,
                                        false,
                                    );
                                });
                            });
                        let pages_res = graph.import_buffer(survivor_pages, None);
                        let commands_res = graph.import_buffer(inputs.commands, None);
                        let counters_res = graph.import_buffer(inputs.counters, None);
                        let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
                        survivor_pass = survivor_pass
                            .access(pages_res, RgUsage::IndexInputRead)
                            .access(commands_res, RgUsage::IndirectCommandRead)
                            .access(counters_res, RgUsage::IndirectCountRead)
                            .access(bucket_counts_res, RgUsage::IndirectCountRead);
                        if let Some(deformed) = deformed_res {
                            survivor_pass =
                                survivor_pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
                        }
                        survivor_pass = survivor_pass
                            .access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead)
                            .access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
                        graph.add_pass(survivor_pass);
                    }
                    // Restore the complete cut for the passes declared after this
                    // block (reactive coverage, the wireframe overlay): clear the
                    // survivor-only bucket counts and re-bin every record.
                    if let (Some(bin_count), Some(bin_scan), Some(bin_scatter)) =
                        (&visibility_psos.2, &visibility_psos.3, &visibility_psos.4)
                    {
                        lists.add_bucket_count_clear_pass(&self.device, &mut graph, frame);
                        lists.add_binning_passes(
                            &self.device,
                            &mut graph,
                            (bin_count, bin_scan, bin_scatter),
                            frame,
                            false,
                            micro_template,
                        );
                    }
                }
                lists.add_counters_readback_pass(&self.device, &mut graph, frame);
            }
            // The final HZB rebuild reads the survivor-updated depth over the same
            // imported pyramid resource, publishing the complete cut as next frame's
            // previous pyramid.
            if survivor_planned
                && let Some(current_hzb_res) = current_hzb_res
                && let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_mut()
            {
                pyramid.add_rebuild_passes(
                    &self.device,
                    &mut graph,
                    (&hzb_copy, &hzb_reduce),
                    depth,
                    current_hzb_res,
                );
            }
        } else if let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_mut() {
            pyramid.invalidate();
        }

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
            bindless_set,
            instance_set,
            executor_inputs,
            &executor_draws,
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

        // Froxel volumetric fog: inject the shadowed per-froxel in-scatter + extinction over the
        // clustered light list, then front-to-back energy-conserving integration into the volume the
        // composite samples. Runs only in `fog.mode == volumetric` (the PSOs are resolved then).
        let froxel_slots = self.add_froxel_fog_passes(
            &mut graph,
            &pipelines,
            cloud_frame.map(|resources| resources.shadow),
            frame,
        );

        // Aerial perspective: ray-march the atmosphere LUTs into the 32³ AP volume the composite folds
        // onto the shared ledger. Independent of the fog inject/integrate (it only reads the LUTs and
        // writes its own volume), scheduled here so it completes before the composite reads it. Runs
        // only while the atmosphere is live + AP is authored (the PSO is resolved then).
        let aerial_slot = self.add_aerial_perspective_pass(&mut graph, &pipelines);

        // Clouds first produce full-resolution premultiplied scatter/transmittance + front depth.
        // The CloudDensity view remains an isolated unlit visualizer that overwrites color directly.
        let cloud_slots = self.add_cloud_passes(
            &mut graph,
            &pipelines,
            CloudGraphInputs {
                color,
                depth,
                motion: motion_resource,
                sky_sh: ddgi_sh,
            },
            cloud_frame,
        );

        // Height fog is the sole fog/AP/cloud composite. It folds the already-upscaled cloud tuple
        // onto the shared transmittance ledger before bloom.
        self.add_fog_pass(&mut graph, &pipelines, color, depth, &cloud_slots, frame);

        // Scene-linear bloom: an energy-conserving mip pyramid composited into `color` while it is
        // still unbounded HDR radiance, immediately before the tonemap so the chosen view transform
        // rolls the bloomed highlights off for free. The mip chain comes from the per-frame-in-flight
        // transient pool, so nothing outlives the frame; skipped entirely when bloom is disabled.
        if pipelines.bloom.is_some() {
            let mips = self.acquire_bloom_mips(&mut graph, frame);
            if !mips.is_empty() {
                let streak = self.acquire_bloom_streak(&mut graph, frame, mips[0].extent);
                self.add_bloom_pass(
                    &mut graph, &pipelines, color, color_view, frame, &mips, &streak,
                );
            }
        }

        // The final post chain on the DISPLAY-extent resolved offscreen color: the mandatory
        // HDR → display tonemap (in-place compute), then the optional ground grid + editor
        // overlay (graphics, over the display-referred color, depth-tested against the
        // display-extent overlay depth). By here `color` is always the display offscreen —
        // present / shm publish consume it identically in editor and present-only mode.
        // Write this frame's scene-linear grade into the view's grade UBO slice; the tonemap pass
        // binds it by the matching dynamic offset (neutral grade → mathematical identity).
        let grade = GradeUniform::from(&self.color_grade).with_look(
            self.creative_lut_intensity,
            self.creative_lut_size,
            false,
        );
        self.views[self.active_view.index()].write_grade(frame, &grade);
        self.add_tonemap_pass(&mut graph, &pipelines, color, frame);
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
            bindless_set,
            instance_set,
            deformed_res,
            executor_inputs,
            &executor_draws,
        );
        self.add_grid_overlay_passes(&mut graph, &pipelines, color, overlay_depth);

        let plan = graph.submission_plan(self.device.render_graph_queue_families());
        let commands = self.frames.prepare_graph_commands(
            &self.device,
            plan.graphics_batch_count(),
            plan.compute_batch_count(),
        )?;

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
        let recorded_batches = {
            // Scope the recorders so their `&mut` borrows of `recorder` / `cpu_*` release
            // before `cpu_buffer.end_span` re-borrows the buffer and `recorder` is stashed.
            let mut recorders = crate::render_graph::ProfileRecorders {
                gpu: recorder.armed().then_some(&mut recorder),
                cpu: profile_cpu.then_some((&mut *cpu_registry, &mut *cpu_buffer)),
            };
            graph.record_submission_plan_profiled(
                &self.device,
                plan,
                RgBatchCommandBuffers {
                    graphics: &commands.graphics,
                    compute: &commands.compute,
                },
                &mut recorders,
            )?
        };
        if let Some(index) = exec_span {
            cpu_buffer.end_span(index, cpu_now_ns());
        }
        self.gpu_profiler.stash_recorder(frame, recorder);
        self.transient.resolve_buffer_states(&graph);

        // Track the offscreen color's resolved exit layout (COLOR_ATTACHMENT after the post
        // chain's overlay/grid pass) so the shm read-back's entry barrier uses the right
        // `old_layout` — otherwise a stale tracked layout mis-transitions the image and the
        // next submit flags a layout mismatch (`VUID-vkCmdDraw-None-09600`).
        self.views[self.active_view.index()]
            .offscreen
            .set_graph_state(graph.external_state(offscreen_slot));

        // Read back the DDGI images' resolved exit layouts (the ray image + the two atlases each
        // rode an external slot), then advance the temporal state (bump the ray-set / round-robin
        // index, commit the scroll base, clear the history-reset flag) — but only when the chain
        // actually ran this frame.
        if let Some(slot) = ddgi.rays_slot {
            self.ddgi.set_rays_state(graph.external_state(slot));
        }
        if let Some(slot) = ddgi.irradiance_slot {
            self.ddgi.set_irradiance_state(graph.external_state(slot));
        }
        if let Some(slot) = ddgi.distance_slot {
            self.ddgi.set_distance_state(graph.external_state(slot));
        }
        if ddgi.irradiance.is_some() {
            self.ddgi.advance_frame();
        }
        self.scene_ibl_mut().resolve_live_layouts(&graph, ibl_live);

        // Read back the GDF cascade volumes' resolved exit layouts (each rode an external slot,
        // ending ShaderReadOnly after the scene's SampledRead), then commit the toroidal recenter
        // state (prev centers + history + the round-robin frame) — only when the chain ran.
        if gdf.cascades.is_some() {
            for c in 0..crate::GDF_CASCADES {
                if let Some(slot) = gdf.cascade_slots[c as usize] {
                    self.global_sdf
                        .set_cascade_layout(c, graph.external_state(slot).layout);
                }
                if let Some(slot) = gdf.occupancy_slots[c as usize] {
                    self.global_sdf
                        .set_occupancy_layout(c, graph.external_state(slot).layout);
                }
            }
            if let Some(slot) = gdf.albedo_slot {
                self.global_sdf
                    .set_albedo_layout(graph.external_state(slot).layout);
            }
            self.global_sdf.advance_frame();
        }

        // Read back the froxel fog volumes' resolved exit layouts (the ping-pong write ends GENERAL,
        // the history ends ShaderReadOnly after its sampled read; the integration volume ends
        // ShaderReadOnly for the composite sample) and advance the ping-pong write index for next
        // frame — the just-written volume becomes next frame's reprojection history.
        if let Some((write_slot, history_slot, integration_slot)) = froxel_slots {
            self.froxel
                .set_scatter_write_layout(graph.external_state(write_slot).layout);
            self.froxel
                .set_scatter_history_layout(graph.external_state(history_slot).layout);
            self.froxel
                .set_integration_layout(graph.external_state(integration_slot).layout);
            self.froxel.advance_frame();
        }

        // Read back the aerial-perspective volume's resolved exit layout (it rode an external slot,
        // ending ShaderReadOnly after the composite's sampled read).
        if let Some(slot) = aerial_slot {
            self.aerial
                .set_volume_layout(graph.external_state(slot).layout);
        }

        if let Some(slot) = cloud_slots.base {
            self.clouds
                .set_base_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.detail {
            self.clouds
                .set_detail_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.curl {
            self.clouds
                .set_curl_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.weather {
            self.clouds
                .set_weather_layout(graph.external_state(slot).layout);
        }
        if let Some(slot) = cloud_slots.shadow {
            self.clouds
                .set_shadow_layout(graph.external_state(slot).layout);
        }
        {
            let view = &mut self.views[self.active_view.index()];
            for (index, slot) in cloud_slots.reduced.iter().enumerate() {
                if let Some(slot) = slot {
                    view.cloud_reduced[index]
                        .as_mut()
                        .expect("cloud reduced built")
                        .set_graph_state(graph.external_state(*slot));
                }
            }
            if let Some(slot) = cloud_slots.reduced_depth {
                view.cloud_reduced_depth
                    .as_mut()
                    .expect("cloud reduced depth built")
                    .set_graph_state(graph.external_state(slot));
            }
            if let Some(slot) = cloud_slots.full_depth {
                view.cloud_full_depth
                    .as_mut()
                    .expect("cloud full depth built")
                    .set_graph_state(graph.external_state(slot));
            }
            if let Some(slot) = cloud_slots.full_color {
                view.cloud_full_color
                    .as_mut()
                    .expect("cloud full color built")
                    .set_graph_state(graph.external_state(slot));
            }
        }

        // Read back the ReSTIR radiance image's resolved exit layout (it rode an external
        // slot, ending ShaderReadOnly after the scene's SampledRead). The per-view temporal
        // state was already advanced inside `add_restir_passes`, before execute.
        if let Some(slot) = restir.radiance_slot {
            let layout = graph.external_state(slot).layout;
            self.views[self.active_view.index()]
                .restir
                .set_radiance_layout(layout);
        }

        // Read back the temporal images' resolved exit layouts (the cross-frame
        // ShaderReadOnly ↔ General transition is derived from these slots). The TAA history
        // pair + the SSGI history pair + the ssgi_resolved each rode an external slot.
        let temporal_ran = taa_slots.is_some()
            || screen.ssgi_history_slots.is_some()
            || screen.dfao_history_slots.is_some()
            || cloud_slots.temporal;
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
            resolved.set_graph_state(graph.external_state(slot));
        }
        if let Some(slots) = &screen.dfao_history_slots {
            writeback_dfao_history_layout(view, &graph, &slots.read);
            writeback_dfao_history_layout(view, &graph, &slots.write);
        }
        if let (Some(slot), Some(resolved)) =
            (screen.dfao_resolved_slot, view.dfao_resolved.as_mut())
        {
            resolved.set_graph_state(graph.external_state(slot));
        }
        if let (Some(slot), Some(ssr_map)) = (screen.ssr_map_slot, view.ssr_map.as_mut()) {
            ssr_map.set_graph_state(graph.external_state(slot));
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
        if taa_active || cloud_slots.temporal {
            view.advance_jitter();
        }
        Ok(RecordedSceneGraph {
            batches: recorded_batches,
            tail: commands.tail,
        })
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
        gdf: &GdfResult,
        sky_sh: RgResource,
    ) -> DdgiResult {
        // The four-pass chain runs only when DDGI is on + all PSOs resolved.
        let Some(ddgi_pipelines) = &pipelines.ddgi else {
            return DdgiResult::default();
        };
        let raw = self.device.raw().clone();

        let (ray_image, ray_view, ray_state) = self.ddgi.rays();
        let rays_slot = graph.alloc_external_state(ray_state);
        let ray_res = graph.import_image(
            ray_image,
            ray_view,
            vk::ImageAspectFlags::COLOR,
            ray_state.layout,
            Some(rays_slot),
        );

        let (irr_image, irr_view, irr_state) = self.ddgi.irradiance();
        let irr_slot = graph.alloc_external_state(irr_state);
        let irr_res = graph.import_image(
            irr_image,
            irr_view,
            vk::ImageAspectFlags::COLOR,
            irr_state.layout,
            Some(irr_slot),
        );

        let (dist_image, dist_view, dist_state) = self.ddgi.distance();
        let dist_slot = graph.alloc_external_state(dist_state);
        let dist_res = graph.import_image(
            dist_image,
            dist_view,
            vk::ImageAspectFlags::COLOR,
            dist_state.layout,
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
            .access(ray_res, RgUsage::StorageImageRwCompute)
            .access(sky_sh, RgUsage::StorageReadCompute);
        // When the GDF composited this frame, the trace's far-field tap reads the cascade volumes
        // (light set binding 9) + the albedo cache (trace set 2) — declare the reads so the graph
        // transitions each from GENERAL (composite write) → ShaderReadOnly before the trace.
        if let Some(cascades) = gdf.cascades {
            for cascade in cascades {
                trace_pass = trace_pass.access(cascade, RgUsage::SampledReadCompute);
            }
        }
        if let Some(occupancy) = gdf.occupancy {
            for volume in occupancy {
                trace_pass = trace_pass.access(volume, RgUsage::SampledReadCompute);
            }
        }
        if let Some(albedo) = gdf.albedo {
            trace_pass = trace_pass.access(albedo, RgUsage::SampledReadCompute);
        }
        let trace_pass = trace_pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO + three sets are valid this frame; the dispatch
            // covers the round-robin probe-budget slice (the shader offsets the probe index by
            // trace_push's budget offset), not the whole volume.
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
        let cull_res = graph.import_buffer(self.global_sdf.cull_buffer(frame), None);
        let mut cascade_res = [RgResource { index: 0 }; crate::GDF_CASCADES as usize];
        let mut cascade_slots = [None; crate::GDF_CASCADES as usize];
        let mut occupancy_res = [RgResource { index: 0 }; crate::GDF_CASCADES as usize];
        let mut occupancy_slots = [None; crate::GDF_CASCADES as usize];
        for c in 0..crate::GDF_CASCADES {
            let (image, view, layout) = self.global_sdf.cascade(c);
            let slot = graph.alloc_external_state(crate::RgExternalState::new(layout));
            cascade_res[c as usize] = graph.import_image_3d(image, view, layout, Some(slot));
            cascade_slots[c as usize] = Some(slot);
            let (image, view, layout) = self.global_sdf.occupancy_cascade(c);
            let slot = graph.alloc_external_state(crate::RgExternalState::new(layout));
            occupancy_res[c as usize] = graph.import_image_3d(image, view, layout, Some(slot));
            occupancy_slots[c as usize] = Some(slot);
        }
        // The lite albedo cache (the composite splats it for the finest cascade; the DDGI trace
        // samples it at hit points).
        let (albedo_image, albedo_view, albedo_layout) = self.global_sdf.albedo_cache();
        let albedo_slot = graph.alloc_external_state(crate::RgExternalState::new(albedo_layout));
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
        for occupancy in occupancy_res {
            composite_pass = composite_pass.access(occupancy, RgUsage::StorageImageRwCompute);
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
            occupancy: Some(occupancy_res),
            occupancy_slots,
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
        let address_block = (
            self.gpu_scene_uploader.address_buffer(),
            frame as u64 * self.gpu_scene_uploader.address_block_stride(),
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );
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
                address_block,
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
        let sentinel = graph.import_buffer(combined, None);
        let radiance_slot = graph.alloc_external_state(crate::RgExternalState::new(rad_layout));
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
        //    Binds set 1 = the bindless texture array for the non-opaque candidate coverage
        //    confirmation.
        let resolve = Arc::clone(&restir_pipelines.resolve);
        let resolve_handle = resolve.handle();
        let resolve_layout = resolve.layout();
        let bindless_set = self.descriptors.bindless_set();
        let raw_body = raw.clone();
        graph.add_pass(
            RgPass::compute("restir-resolve")
                .access(sentinel, RgUsage::StorageReadCompute)
                .access(radiance_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/sets/layout are valid this frame; the
                    // push spans the declared range; the dispatch covers the view grid.
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            resolve_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            resolve_layout,
                            0,
                            &[resolve_set, bindless_set],
                            &[],
                        );
                        raw_body.cmd_push_constants(
                            cmd,
                            resolve_layout,
                            vk::ShaderStageFlags::COMPUTE,
                            0,
                            bytemuck::bytes_of(&resolve_push),
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
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
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        motion: Option<RgResource>,
        deformed: (Option<RgResource>, Option<vk::Buffer>),
        light_set: vk::DescriptorSet,
        gdf_cascades: Option<[RgResource; crate::GDF_CASCADES as usize]>,
        gdf_occupancy: Option<[RgResource; crate::GDF_CASCADES as usize]>,
        sky_sh: RgResource,
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) -> ScreenSpaceResult {
        let (deformed_res, _deformed_handle) = deformed;
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
            let raw_body = raw.clone();
            let push = self.ssao.gbuffer_push();
            let pipeline = Arc::clone(gbuffer);
            let gbuffer_pipeline = pipeline.handle();
            let gbuffer_layout = pipeline.layout();
            let draws = executor_draws.to_vec();
            let pages_buffer = self.global_gpu_data.pages.buffer();
            let draw_count_supported = self.device.capabilities.draw_indirect_count;
            let tess_pso = pipelines.gbuffer_tess.clone();
            let tess_draws = self.scene_draw_list.tess_draws.clone();
            let mut pass = RgPass::graphics("gbuffer", extent)
                .color(RgAttachment::clear_store(g_normal))
                .color(RgAttachment::clear_store(g_roughness))
                .depth_attachment(depth_clear_store(g_depth));
            pass = access_tess_draws(graph, pass, &tess_draws, false);
            let mut pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                if let Some(inputs) = executor_inputs {
                    record_executor_depth_family(
                        &raw_body,
                        cmd,
                        (gbuffer_pipeline, gbuffer_layout),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&push),
                        bindless_set,
                        instance_set,
                        inputs,
                        pages_buffer,
                        draw_count_supported,
                        &draws,
                        false,
                    );
                    if let Some(tess_pso) = &tess_pso {
                        crate::scene_pass::record_tess_depth_draws(
                            &raw_body,
                            cmd,
                            (tess_pso.handle(), tess_pso.layout()),
                            vk::ShaderStageFlags::VERTEX,
                            bytemuck::bytes_of(&push),
                            bindless_set,
                            instance_set,
                            &tess_draws,
                            false,
                        );
                    }
                }
                drop(pipeline);
            });
            if let Some(inputs) = executor_inputs {
                let pages_res = graph.import_buffer(pages_buffer, None);
                let commands_res = graph.import_buffer(inputs.commands, None);
                let counters_res = graph.import_buffer(inputs.counters, None);
                let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
                pass = pass
                    .access(pages_res, RgUsage::IndexInputRead)
                    .access(commands_res, RgUsage::IndirectCommandRead)
                    .access(counters_res, RgUsage::IndirectCountRead)
                    .access(bucket_counts_res, RgUsage::IndirectCountRead);
            }
            let micro_candidates_res =
                graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
            pass = pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
            if let Some(wind_records) = self.wind_records_handle() {
                let wind_records_res = graph.import_buffer(wind_records, None);
                pass = pass.access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
            }
            if let Some(deformed) = deformed_res {
                pass = pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
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
            let ao_map_slot = graph
                .alloc_external_state(view.ao_map.as_ref().expect("ao_map built").graph_state());
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
                1,
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
                1,
            );
            result.scene_sampled.push(ao_map);
        }

        // Directional contact shadows: g_normal → contact_map.
        if let Some(contact) = &pipelines.contact {
            let contact_slot = graph.alloc_external_state(
                view.contact_map
                    .as_ref()
                    .expect("contact_map built")
                    .graph_state(),
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
                1,
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
            let ssgi_slot = graph.alloc_external_state(
                view.ssgi_map
                    .as_ref()
                    .expect("ssgi_map built")
                    .graph_state(),
            );
            let ssgi_map = graph.import_image(
                view.ssgi_map.as_ref().expect("ssgi_map built").handle(),
                view.ssgi_map.as_ref().expect("ssgi_map built").view(),
                vk::ImageAspectFlags::COLOR,
                view.ssgi_map.as_ref().expect("ssgi_map built").layout,
                Some(ssgi_slot),
            );
            let denoised_slot = graph.alloc_external_state(
                view.ssgi_denoised
                    .as_ref()
                    .expect("ssgi_denoised built")
                    .graph_state(),
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
                1,
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
                1,
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
                let ssgi_resolved_slot = graph.alloc_external_state(
                    view.ssgi_resolved
                        .as_ref()
                        .expect("ssgi_resolved built")
                        .graph_state(),
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
                    1,
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
            if let Some(occupancy) = gdf_occupancy {
                for volume in occupancy {
                    trace_pass = trace_pass.access(volume, RgUsage::SampledReadCompute);
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
                    1,
                );
            }

            // Expose dfao_denoised to the gi-resolve pass (added after this block so it runs whenever
            // the screen chain does, not only when sky-occlusion is on): its skyVis input, when present.
            dfao_denoised_res = Some(dfao_denoised);

            // 3. Temporal accumulation (when motion ran): reproject the DFAO history through
            //    motion, neighborhood-clamp, EMA into the stable dfao_resolved the mesh samples.
            if let (Some(accum), Some(motion)) = (&pipelines.dfao_accum, motion) {
                let p = view.history_index;
                let dfao_resolved_slot = graph.alloc_external_state(
                    view.dfao_resolved
                        .as_ref()
                        .expect("dfao_resolved built")
                        .graph_state(),
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
                    1,
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
            let gi_slot = graph.alloc_external_state(
                view.gi_indirect
                    .as_ref()
                    .expect("gi_indirect built")
                    .graph_state(),
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
                (sky_sh, RgUsage::StorageReadCompute),
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
                1,
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
            if let Some(occupancy) = gdf_occupancy {
                for volume in occupancy {
                    trace_pass = trace_pass.access(volume, RgUsage::SampledReadCompute);
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
                    1,
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
            let ssr_slot = graph
                .alloc_external_state(view.ssr_map.as_ref().expect("ssr_map built").graph_state());
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
                1,
            );
            result.scene_sampled.push(ssr_map);
            result.ssr_map_slot = Some(ssr_slot);
        }

        // RT reflections sample prev_color directly in the mesh fragment (set-4 binding 4),
        // so the scene pass must SampledRead it (transition to ShaderReadOnly before the draw).
        if rt_refl && let Some(pc) = prev_color {
            result.scene_sampled.push(pc);
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
    #[allow(clippy::too_many_arguments)]
    fn add_motion_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        deformed: (Option<RgResource>, Option<vk::Buffer>),
        prev_deformed: (Option<RgResource>, Option<vk::Buffer>),
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) -> Option<(RgResource, RgResource)> {
        let motion_pipeline = pipelines.motion.as_ref()?;
        let (deformed_res, _) = deformed;
        let (prev_deformed_res, _) = prev_deformed;
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
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(motion_pipeline);
        let motion_handle = pipeline.handle();
        let motion_layout = pipeline.layout();
        let draws = executor_draws.to_vec();
        let pages_buffer = self.global_gpu_data.pages.buffer();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let tess_pso = pipelines.motion_tess.clone();
        let tess_draws = self.scene_draw_list.tess_draws.clone();
        let mut pass = RgPass::graphics("motion", extent)
            .color(RgAttachment::clear_store(motion))
            .depth_attachment(depth_clear_store(motion_depth));
        pass = access_tess_draws(graph, pass, &tess_draws, true);
        let mut pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            if let Some(inputs) = executor_inputs {
                record_executor_depth_family(
                    &raw_body,
                    cmd,
                    (motion_handle, motion_layout),
                    vk::ShaderStageFlags::VERTEX,
                    bytemuck::bytes_of(&push),
                    bindless_set,
                    instance_set,
                    inputs,
                    pages_buffer,
                    draw_count_supported,
                    &draws,
                    false,
                );
                if let Some(tess_pso) = &tess_pso {
                    crate::scene_pass::record_tess_depth_draws(
                        &raw_body,
                        cmd,
                        (tess_pso.handle(), tess_pso.layout()),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&push),
                        bindless_set,
                        instance_set,
                        &tess_draws,
                        true,
                    );
                }
            }
            drop(pipeline);
        });
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
        }
        // The motion executor pulls BOTH deformed buffers through their device
        // addresses (current + previous position), so declare both reads for the
        // skin-write → pull barrier on each. The micro-blade candidates pull the same
        // way (micro-pass-write → pull).
        let micro_candidates_res =
            graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
        pass = pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
        if let Some(wind_records) = self.wind_records_handle() {
            let wind_records_res = graph.import_buffer(wind_records, None);
            pass = pass.access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
        }
        if let Some(deformed) = deformed_res {
            pass = pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
        }
        if let Some(prev_deformed) = prev_deformed_res {
            pass = pass.access(prev_deformed, RgUsage::ShaderDeviceAddressRead);
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
            1,
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
            1,
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
    #[allow(clippy::too_many_arguments)]
    fn add_reactive_coverage_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        scene_depth: RgResource,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) -> Option<RgResource> {
        let pipeline = pipelines.reactive_coverage.as_ref()?;
        let transition_keepalive = pipelines.reactive_transition.clone();
        let transition_pipeline = transition_keepalive
            .as_ref()
            .map(|pipeline| (pipeline.handle(), pipeline.layout()));
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
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let view_proj = self.scene_draw_list.view_proj;
        let draws = executor_draws.to_vec();
        let pages_buffer = self.global_gpu_data.pages.buffer();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let mut color_att = RgAttachment::clear_store(reactive_res);
        color_att.clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 0.0],
            },
        };
        // Read-only depth test against the resolved scene depth (declared by the attachment), so
        // occluded translucent fragments don't mark coverage. The blend buckets' binned
        // commands are the translucent draws; order is irrelevant for a coverage mask.
        let mut pass = RgPass::graphics("reactive-coverage", input)
            .color(color_att)
            .depth_attachment(depth_load_readonly(scene_depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                if let Some(inputs) = executor_inputs {
                    record_executor_depth_family(
                        &raw_body,
                        cmd,
                        (handle, layout),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&view_proj),
                        bindless_set,
                        instance_set,
                        inputs,
                        pages_buffer,
                        draw_count_supported,
                        &draws,
                        true,
                    );
                    // The opaque buckets re-walk through the degenerate-collapse entry:
                    // only blades and records mid representation-transition rasterize.
                    if let Some((transition_handle, transition_layout)) = transition_pipeline {
                        record_executor_depth_family(
                            &raw_body,
                            cmd,
                            (transition_handle, transition_layout),
                            vk::ShaderStageFlags::VERTEX,
                            bytemuck::bytes_of(&view_proj),
                            bindless_set,
                            instance_set,
                            inputs,
                            pages_buffer,
                            draw_count_supported,
                            &draws,
                            false,
                        );
                    }
                }
                drop(pipeline);
                drop(transition_keepalive);
            });
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
        }
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
        let read_slot = graph.alloc_external_state(history_read.graph_state());
        let write_slot = graph.alloc_external_state(history_write.graph_state());
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
        let lock_read_slot = graph.alloc_external_state(lock_read.graph_state());
        let lock_write_slot = graph.alloc_external_state(lock_write.graph_state());
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
            1,
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

    /// Acquires this frame's transient bloom mip chain (half-res first, halving each level) sized
    /// off the display-extent `published_extent`, and imports each level into `graph`. The live
    /// level count is `floor(log2(min(w, h))) - 3` clamped to `[1, MAX_BLOOM_MIPS]` (≈6 at 1080p, 7
    /// at 1440p+). Returns empty (bloom skipped) when the viewport is too small to pyramid or an
    /// allocation fails.
    fn acquire_bloom_mips(&mut self, graph: &mut RenderGraph, frame: usize) -> Vec<BloomMip> {
        let published = self.views[self.active_view.index()].published_extent();
        let min_dim = published.width.min(published.height);
        if min_dim < 8 {
            return Vec::new();
        }
        let levels = ((min_dim as f32).log2().floor() as i32 - 3)
            .clamp(1, crate::descriptors::MAX_BLOOM_MIPS as i32) as usize;
        let mut mips = Vec::with_capacity(levels);
        for i in 0..levels {
            let extent = vk::Extent2D {
                width: (published.width >> (i + 1)).max(1),
                height: (published.height >> (i + 1)).max(1),
            };
            let desc = crate::resources::ImageDesc::color_2d(
                extent,
                crate::OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            );
            let (image, view) = match self.transient.acquire_image(
                frame,
                crate::transient::BLOOM_MIP_KEYS[i],
                &desc,
            ) {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::error!("bloom mip {i} acquire failed: {err}");
                    return Vec::new();
                }
            };
            let res = graph.import_image(
                image,
                view,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            mips.push(BloomMip { res, view, extent });
        }
        mips
    }

    /// Acquires the two ping-pong anamorphic streak buffers at `extent` (the bloom mip0 resolution)
    /// and imports each into `graph`. Returns empty (streak skipped) when anamorphic is off or an
    /// allocation fails, so the composite falls back to the white streak view (no added energy).
    fn acquire_bloom_streak(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
        extent: vk::Extent2D,
    ) -> Vec<BloomMip> {
        if !self.bloom_anamorphic_enabled {
            return Vec::new();
        }
        let mut buffers = Vec::with_capacity(crate::transient::BLOOM_STREAK_KEYS.len());
        for key in crate::transient::BLOOM_STREAK_KEYS {
            let desc = crate::resources::ImageDesc::color_2d(
                extent,
                crate::OFFSCREEN_COLOR_FORMAT,
                vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::SAMPLED,
            );
            let (image, view) = match self.transient.acquire_image(frame, key, &desc) {
                Ok(pair) => pair,
                Err(err) => {
                    tracing::error!("bloom streak '{key}' acquire failed: {err}");
                    return Vec::new();
                }
            };
            let res = graph.import_image(
                image,
                view,
                vk::ImageAspectFlags::COLOR,
                vk::ImageLayout::UNDEFINED,
                None,
            );
            buffers.push(BloomMip { res, view, extent });
        }
        buffers
    }

    /// Appends the scene-linear bloom pyramid on `color`, in place, before the tonemap: a Karis-
    /// averaged 13-tap downsample chain (`color → mip0 → … → mipN`, Karis only on the first step), a
    /// progressive 9-tap tent upsample-add back down to `mip0`, then an energy-conserving
    /// `lerp(hdr, bloom * tint, intensity)` composite into `color`. Each pass declares its
    /// `(resource, usage)` so the graph derives every `GENERAL ↔ SHADER_READ_ONLY` transition; the
    /// per-pass descriptor sets are rewritten for this frame slot first (they bind the transient
    /// mip views). One PSO drives all four pass kinds via the push `pass`/`karis` fields.
    #[allow(clippy::too_many_arguments)]
    fn add_bloom_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        color_view: vk::ImageView,
        frame: usize,
        mips: &[BloomMip],
        streak: &[BloomMip],
    ) {
        let Some(bloom) = &pipelines.bloom else {
            return;
        };
        const DOWN: [&str; crate::descriptors::MAX_BLOOM_MIPS] = [
            "bloom-downsample-0",
            "bloom-downsample-1",
            "bloom-downsample-2",
            "bloom-downsample-3",
            "bloom-downsample-4",
            "bloom-downsample-5",
            "bloom-downsample-6",
        ];
        const UP: [&str; crate::descriptors::MAX_BLOOM_MIPS] = [
            "bloom-upsample-0",
            "bloom-upsample-1",
            "bloom-upsample-2",
            "bloom-upsample-3",
            "bloom-upsample-4",
            "bloom-upsample-5",
            "bloom-upsample-6",
        ];
        let view = &self.views[self.active_view.index()];
        let published = view.published_extent();
        let levels = mips.len();
        // The streak buffer the composite adds — its last ping-pong stage when armed, else nothing.
        let streak_buffer = streak.last();

        // The source/target view pairs in graph order (downsamples, upsamples, streak, composite),
        // rewritten into this frame slot's sets before the passes reference them. Only the last
        // (composite) pass samples the dirt + streak; the earlier passes get the white fallback.
        let white = self.default_white.view();
        let mut pairs: Vec<(vk::ImageView, vk::ImageView)> = Vec::with_capacity(2 * levels + 3);
        for i in 0..levels {
            let src = if i == 0 { color_view } else { mips[i - 1].view };
            pairs.push((src, mips[i].view));
        }
        for j in (0..levels.saturating_sub(1)).rev() {
            pairs.push((mips[j + 1].view, mips[j].view));
        }
        // Streak ping-pong: mip0 → streak0, streak0 → streak1 (each a wider horizontal blur).
        for (s, buffer) in streak.iter().enumerate() {
            let src = if s == 0 {
                mips[0].view
            } else {
                streak[s - 1].view
            };
            pairs.push((src, buffer.view));
        }
        pairs.push((mips[0].view, color_view));
        let dirt_view = self.bloom_dirt_texture.as_ref().map_or(white, |t| t.view());
        let streak_view = streak_buffer.map_or(white, |b| b.view);
        view.write_bloom_sets(
            &self.device,
            &self.descriptors,
            frame,
            &pairs,
            crate::view_target::BloomCompositeBindings {
                dirt: dirt_view,
                streak: streak_view,
                fallback: white,
            },
        );

        let groups = |n: u32| n.div_ceil(8);
        let scatter = self.bloom_scatter;
        let mut pass_idx = 0usize;

        // Downsample: color → mip0 (Karis), then mip[i-1] → mip[i].
        for i in 0..levels {
            let src_res = if i == 0 { color } else { mips[i - 1].res };
            let push = crate::BloomPush {
                filter_radius: scatter,
                intensity: self.bloom_intensity,
                threshold: self.bloom_threshold,
                pass: 0,
                karis: u32::from(i == 0),
                ..crate::BloomPush::identity()
            };
            self.add_compute_pass(
                graph,
                DOWN[i],
                bloom,
                view.bloom_set(frame, pass_idx),
                &[
                    (src_res, RgUsage::SampledReadCompute),
                    (mips[i].res, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(mips[i].extent.width),
                groups(mips[i].extent.height),
                1,
            );
            pass_idx += 1;
        }

        // Tent upsample-add: mip[j+1] → mip[j], coarse to fine. The added contribution carries the
        // per-mip tint for level `j` (identity when the stack is off or shorter than the pyramid).
        for j in (0..levels.saturating_sub(1)).rev() {
            let push = crate::BloomPush {
                filter_radius: scatter,
                intensity: self.bloom_intensity,
                threshold: self.bloom_threshold,
                pass: 1,
                mip_tint: self.bloom_mip_tint.get(j).copied().unwrap_or([1.0; 3]),
                ..crate::BloomPush::identity()
            };
            self.add_compute_pass(
                graph,
                UP[j],
                bloom,
                view.bloom_set(frame, pass_idx),
                &[
                    (mips[j + 1].res, RgUsage::SampledReadCompute),
                    (mips[j].res, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(mips[j].extent.width),
                groups(mips[j].extent.height),
                1,
            );
            pass_idx += 1;
        }

        // Anamorphic streak ping-pong: a horizontally-squeezed blur widened across two passes.
        for (s, buffer) in streak.iter().enumerate() {
            let src_res = if s == 0 {
                mips[0].res
            } else {
                streak[s - 1].res
            };
            let push = crate::BloomPush {
                pass: 3,
                filter_radius: scatter,
                anamorphic_ratio: self.bloom_anamorphic_ratio,
                ..crate::BloomPush::identity()
            };
            self.add_compute_pass(
                graph,
                crate::transient::BLOOM_STREAK_KEYS[s],
                bloom,
                view.bloom_set(frame, pass_idx),
                &[
                    (src_res, RgUsage::SampledReadCompute),
                    (buffer.res, RgUsage::StorageImageRwCompute),
                ],
                Some(bytemuck::bytes_of(&push).to_vec()),
                groups(buffer.extent.width),
                groups(buffer.extent.height),
                1,
            );
            pass_idx += 1;
        }

        // Energy-conserving composite: attenuate mip0 by the dirt mask, add the streak, then
        // lerp(hdr, bloom * tint, intensity) in place on color. When the streak is off, the streak
        // binding is the white fallback and `anamorphic_intensity` is 0, so no streak energy adds.
        let mut inputs: Vec<(RgResource, RgUsage)> = vec![
            (mips[0].res, RgUsage::SampledReadCompute),
            (color, RgUsage::StorageImageRwCompute),
        ];
        if let Some(buffer) = streak_buffer {
            inputs.push((buffer.res, RgUsage::SampledReadCompute));
        }
        let push = crate::BloomPush {
            tint: self.bloom_tint,
            filter_radius: scatter,
            intensity: self.bloom_intensity,
            threshold: self.bloom_threshold,
            pass: 2,
            dirt_intensity: self.bloom_dirt_intensity,
            dirt_tint: self.bloom_dirt_tint,
            anamorphic_intensity: if streak_buffer.is_some() {
                self.bloom_anamorphic_intensity
            } else {
                0.0
            },
            anamorphic_tint: self.bloom_anamorphic_tint,
            ..crate::BloomPush::identity()
        };
        self.add_compute_pass(
            graph,
            "bloom-composite",
            bloom,
            view.bloom_set(frame, pass_idx),
            &inputs,
            Some(bytemuck::bytes_of(&push).to_vec()),
            groups(published.width),
            groups(published.height),
            1,
        );
    }

    /// Appends the froxel volumetric-fog inject + integrate compute passes: `fog_inject` fills the
    /// scatter volume (per-froxel extinction + shadowed in-scatter over the clustered light list, HG
    /// phase), `fog_integrate` marches it front-to-back into the integration volume the composite
    /// samples, and a barrier-only pass rests the integration volume in ShaderReadOnly. Runs only in
    /// volumetric mode (the PSOs are resolved then). Returns the scatter + integration external slots
    /// for the post-execute layout write-back, or `None` when the passes did not run.
    fn add_froxel_fog_passes(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        cloud_shadow: Option<RgResource>,
        frame: usize,
    ) -> Option<(usize, usize, usize)> {
        let (Some(inject), Some(integrate)) = (&pipelines.fog_inject, &pipelines.fog_integrate)
        else {
            return None;
        };

        // This frame's froxel-grid UBO: the froxel-center reconstruction matrices + the active-tier
        // grid dims + the exponential-Z near/far the composite's W mapping inverts, plus the temporal
        // reprojection state (previous view-proj, the shared TAA jitter, the blend + clamp knobs).
        let view_m = self.ssao.view();
        let inv_view = view_m.inverse();
        let inv_proj = (self.scene_view_proj_unjittered() * inv_view).inverse();
        let extent = self.views[self.active_view.index()].scaled_render_extent();
        let (gx, gy, gz) = self.froxel.grid();
        let active_view = &self.views[self.active_view.index()];
        let prev_view_proj = active_view.prev_view_proj;
        let jitter_index = active_view.jitter_index;
        // History is reusable only once the previous frame's camera transform is valid (no cut) AND
        // the history volume carries content (not the first frame after a reset / quality switch).
        let history_valid = active_view.prev_view_proj_valid && self.froxel.history_ready();
        let jitter = self.active_view_jitter();
        let f = self.fog;
        let grid_params = crate::FogGridParams {
            inverse_projection: inv_proj,
            inverse_view: inv_view,
            prev_view_proj,
            grid_size: saffron_geometry::glam::UVec4::new(gx, gy, gz, 0),
            screen_size: saffron_geometry::glam::Vec4::new(
                extent.width as f32,
                extent.height as f32,
                0.0,
                0.0,
            ),
            z_planes: saffron_geometry::glam::Vec4::new(
                crate::froxel_fog::FROXEL_NEAR,
                crate::FROXEL_FAR,
                crate::FROXEL_FAR,
                0.0,
            ),
            temporal: saffron_geometry::glam::Vec4::new(
                f.history_blend,
                if history_valid { 1.0 } else { 0.0 },
                if f.neighborhood_clamp { 1.0 } else { 0.0 },
                f.light_clamp,
            ),
            jitter: saffron_geometry::glam::Vec4::new(
                jitter.x,
                jitter.y,
                jitter_index as f32,
                self.fog_time,
            ),
        };
        self.froxel.update_grid(&grid_params);

        // Upload this frame's local fog volumes into the inject SSBO; the count rides the push so the
        // injection loop bounds itself without a separate uniform.
        let volume_count = self.froxel.update_fog_volumes(&self.fog_volumes);

        // The medium push: the analytic height density (injected as the froxel base medium — never
        // applied twice at composite), the scattering albedo/phase, and the constant emission.
        let eye = inv_view.col(3).truncate();
        let push_vals: [f32; 16] = [
            eye.x,
            eye.y,
            eye.z,
            f.base_density,
            f.emissive.x,
            f.emissive.y,
            f.emissive.z,
            f.scatter_albedo,
            f.density,
            f.height_falloff,
            f.height,
            f.phase_g,
            f.layer2_density,
            f.layer2_falloff,
            f.layer2_height,
            f32::from_bits(volume_count),
        ];

        // This frame's inject target (written in GENERAL) and last frame's history (sampled in
        // ShaderReadOnly). Both scatter volumes ride external slots so their per-volume GENERAL ↔
        // ShaderReadOnly layouts survive the frame boundary (the ping-pong swaps their roles).
        let (write_img, write_view, write_layout) = self.froxel.scatter_write_import();
        let write_slot = graph.alloc_external_state(crate::RgExternalState::new(write_layout));
        let write_res =
            graph.import_image_3d(write_img, write_view, write_layout, Some(write_slot));
        let (hist_img, hist_view, hist_layout) = self.froxel.scatter_history_import();
        let hist_slot = graph.alloc_external_state(crate::RgExternalState::new(hist_layout));
        let hist_res = graph.import_image_3d(hist_img, hist_view, hist_layout, Some(hist_slot));
        let (integ_img, integ_view, integ_layout) = self.froxel.integration_import();
        let integ_slot = graph.alloc_external_state(crate::RgExternalState::new(integ_layout));
        let integ_res =
            graph.import_image_3d(integ_img, integ_view, integ_layout, Some(integ_slot));
        let cluster_res = graph.import_buffer(self.lighting.cluster_buffer(frame), None);
        let (light_buf, _) = self.lighting.light_list_buffer(frame);
        let light_res = graph.import_buffer(light_buf, None);

        let light_set = self.lighting.light_set(frame);
        let volume_set = self.froxel.inject_set();
        {
            let inject = Arc::clone(inject);
            let handle = inject.handle();
            let layout = inject.layout();
            let raw_body = self.device.raw().clone();
            let dispatch = (gx.div_ceil(8), gy.div_ceil(8), gz.div_ceil(4));
            let mut pass = RgPass::compute("fog-inject")
                .access(write_res, RgUsage::StorageImageRwCompute)
                .access(hist_res, RgUsage::SampledReadCompute)
                .access(cluster_res, RgUsage::StorageReadCompute)
                .access(light_res, RgUsage::StorageReadCompute);
            if let Some(resource) = cloud_shadow {
                pass = pass.access(resource, RgUsage::SampledReadCompute);
            }
            let pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO/sets are valid this frame; the dispatch covers the
                // froxel grid (8×8×4 per group). Set 0 is the reused mesh light set, set 1 the fog
                // volume; the 64-byte push carries the authored medium.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        layout,
                        0,
                        &[light_set, volume_set],
                        &[],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::cast_slice(&push_vals),
                    );
                    raw_body.cmd_dispatch(cmd, dispatch.0, dispatch.1, dispatch.2);
                }
                drop(inject);
            });
            graph.add_pass(pass);
        }
        {
            let integrate = Arc::clone(integrate);
            let handle = integrate.handle();
            let layout = integrate.layout();
            let integrate_set = self.froxel.integrate_set();
            let raw_body = self.device.raw().clone();
            let dispatch = (gx.div_ceil(8), gy.div_ceil(8), 1);
            let pass = RgPass::compute("fog-integrate")
                .access(write_res, RgUsage::StorageImageRwCompute)
                .access(integ_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. One thread per froxel column, serial over Z.
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[integrate_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, dispatch.0, dispatch.1, dispatch.2);
                    }
                    drop(integrate);
                });
            graph.add_pass(pass);
        }
        // Rest the integration volume in ShaderReadOnly for the composite's trilinear sample (the fog
        // set binds it at binding 4 with that layout).
        let read_pass = RgPass::compute("fog-integration-read")
            .access(integ_res, RgUsage::SampledReadCompute)
            .body(|_cmd, _scopes: &mut NestedScopeRecorder| {});
        graph.add_pass(read_pass);

        Some((write_slot, hist_slot, integ_slot))
    }

    /// Appends the aerial-perspective fill pass: one compute dispatch ray-marches the atmosphere
    /// transmittance + multiscatter LUTs into the `32³` AP volume, bounded at each froxel center's
    /// distance (Hillaire 2020). Writes the volume as a storage image (`StorageImageRwCompute`, GENERAL)
    /// then rests it in ShaderReadOnly for the composite's binding-5 sample. No-op unless the atmosphere
    /// is live + AP is authored (the PSO is `None` otherwise). Returns the volume's external slot for the
    /// post-execute layout write-back, or `None` when the pass did not run.
    fn add_aerial_perspective_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
    ) -> Option<usize> {
        let pipeline = pipelines.aerial.as_ref()?;

        let params = self.aerial_params(self.fog.aerial_intensity);
        self.aerial.update_params(&params);

        let (vol_img, vol_view, vol_layout) = self.aerial.volume_import();
        let vol_slot = graph.alloc_external_state(crate::RgExternalState::new(vol_layout));
        let vol_res = graph.import_image_3d(vol_img, vol_view, vol_layout, Some(vol_slot));

        let fill_set = self.aerial.fill_set();
        {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let raw_body = self.device.raw().clone();
            let groups = crate::AP_GRID.div_ceil(4);
            let pass = RgPass::compute("aerial-perspective")
                .access(vol_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    // SAFETY: the ash seam. The PSO/set are valid this frame; one thread per froxel over
                    // the 32³ grid (4×4×4 per group).
                    unsafe {
                        raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            layout,
                            0,
                            &[fill_set],
                            &[],
                        );
                        raw_body.cmd_dispatch(cmd, groups, groups, groups);
                    }
                    drop(pipeline);
                });
            graph.add_pass(pass);
        }
        // Rest the AP volume in ShaderReadOnly for the composite's trilinear sample (fog set binding 5).
        let read_pass = RgPass::compute("aerial-perspective-read")
            .access(vol_res, RgUsage::SampledReadCompute)
            .body(|_cmd, _scopes: &mut NestedScopeRecorder| {});
        graph.add_pass(read_pass);

        Some(vol_slot)
    }

    fn aerial_params(&self, intensity: f32) -> crate::AerialParamsUbo {
        let inverse_view = self.ssao.view().inverse();
        let inverse_projection = (self.scene_view_proj_unjittered() * inverse_view).inverse();
        let atmosphere = self.scene_ibl().baked_atmosphere();
        let (sun_direction, sun_intensity) = self.scene_ibl().baked_sun();
        crate::AerialParamsUbo {
            inverse_projection,
            inverse_view,
            sun_dir: sun_direction.normalize_or_zero().extend(sun_intensity),
            rayleigh: atmosphere
                .rayleigh_scattering
                .extend(atmosphere.rayleigh_scale_height),
            ozone: atmosphere
                .ozone_absorption
                .extend(atmosphere.mie_scattering),
            params0: Vec4::new(
                atmosphere.planet_radius,
                atmosphere.atmosphere_height,
                atmosphere.mie_scale_height,
                atmosphere.mie_anisotropy,
            ),
            params1: Vec4::new(
                atmosphere.sun_disk_angular_radius,
                atmosphere.sun_disk_intensity,
                0.0,
                intensity,
            ),
            ap_planes: Vec4::new(
                crate::froxel_fog::FROXEL_NEAR,
                crate::AP_FAR_M,
                crate::AP_GRID as f32,
                1.0e-3,
            ),
        }
    }

    fn prepare_cloud_frame(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        frame: usize,
    ) -> Option<CloudFrameResources> {
        if !self.clouds.settings().enabled {
            return None;
        }
        let view_index = self.active_view.index();
        let inv_view_proj = self.scene_view_proj_unjittered().inverse();
        let camera = self.ssao.view().inverse().col(3).truncate();
        let view = &self.views[view_index];
        let reduced_extent = view.cloud_reduced[0]
            .as_ref()
            .expect("cloud reduced built")
            .extent;
        let atmosphere = self.scene_ibl().baked_atmosphere();
        let current_view_proj = self.scene_view_proj_unjittered();
        // The cloud layer advects on the shared wind field's mean term at its own
        // altitude (the shear power law), on the monotonic simulation clock.
        let scene_wind = self.scene_wind;
        let wind_radians = scene_wind.orientation.to_radians();
        let cloud_settings = self.clouds.settings();
        let layer_mid = cloud_settings.layer_altitude + cloud_settings.layer_height * 0.5;
        let wind_speed =
            scene_wind.speed * saffron_wind::shear_factor(&scene_wind.profile(), layer_mid);
        let state = crate::clouds::CloudFrameState {
            wind_direction: saffron_geometry::glam::Vec2::new(
                wind_radians.sin(),
                wind_radians.cos(),
            ),
            wind_speed,
            wind_gust: scene_wind.gust,
            wind_time_s: scene_wind.time_s as f32,
            inv_view_proj,
            prev_view_proj: if view.prev_view_proj_valid {
                view.prev_view_proj
            } else {
                current_view_proj
            },
            camera,
            sun_direction: (-self.sun_direction).normalize_or_zero(),
            sun_color: self.sun_color,
            sun_intensity: self.sun_intensity,
            moon_direction: (-self.moon_direction).normalize_or_zero(),
            moon_color: self.moon_color,
            moon_intensity: self.moon_intensity,
            planet_radius: atmosphere.planet_radius,
            atmosphere_height: atmosphere.atmosphere_height,
            jitter_index: view.jitter_index,
            reduced_extent,
            history_valid: view.history_valid && view.prev_view_proj_valid,
            atmosphere_live: self.scene_ibl().atmosphere_live(),
        };
        self.clouds.write_params(view_index, frame, &state);
        let params_offset = self.clouds.params_offset(view_index, frame);

        let base = self.clouds.base_noise();
        let base_slot = graph.alloc_external_state(base.graph_state());
        let base_res =
            graph.import_image_3d(base.handle(), base.view(), base.layout, Some(base_slot));
        let detail = self.clouds.detail_noise();
        let detail_slot = graph.alloc_external_state(detail.graph_state());
        let detail_res = graph.import_image_3d(
            detail.handle(),
            detail.view(),
            detail.layout,
            Some(detail_slot),
        );
        let curl = self.clouds.curl_noise();
        let curl_slot = graph.alloc_external_state(curl.graph_state());
        let curl_res = graph.import_image(
            curl.handle(),
            curl.view(),
            vk::ImageAspectFlags::COLOR,
            curl.layout,
            Some(curl_slot),
        );
        let weather = self.clouds.weather_map();
        let weather_slot = graph.alloc_external_state(weather.graph_state());
        let weather_res = graph.import_image(
            weather.handle(),
            weather.view(),
            vk::ImageAspectFlags::COLOR,
            weather.layout,
            Some(weather_slot),
        );
        let shadow = self.clouds.cloud_shadow();
        let shadow_slot = graph.alloc_external_state(shadow.graph_state());
        let shadow_res = graph.import_image(
            shadow.handle(),
            shadow.view(),
            vk::ImageAspectFlags::COLOR,
            shadow.layout,
            Some(shadow_slot),
        );

        if let Some(pipeline) = &pipelines.cloud_weather {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let set = self.clouds.weather_set();
            let groups = crate::clouds::CLOUD_WEATHER_DIM.div_ceil(8);
            let raw_body = self.device.raw().clone();
            graph.add_pass(
                RgPass::compute("cloud-weather")
                    .access(weather_res, RgUsage::StorageImageRwCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[params_offset],
                            );
                            raw_body.cmd_dispatch(cmd, groups, groups, 1);
                        }
                        drop(pipeline);
                    }),
            );
            self.clouds.mark_weather_clean();
        }

        if let Some(pipeline) = &pipelines.cloud_shadow {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let set = self.clouds.shadow_set();
            let groups = crate::clouds::CLOUD_SHADOW_DIM.div_ceil(8);
            let raw_body = self.device.raw().clone();
            graph.add_pass(
                RgPass::compute("cloud-shadow")
                    .access(shadow_res, RgUsage::StorageImageRwCompute)
                    .access(base_res, RgUsage::SampledReadCompute)
                    .access(detail_res, RgUsage::SampledReadCompute)
                    .access(curl_res, RgUsage::SampledReadCompute)
                    .access(weather_res, RgUsage::SampledReadCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[params_offset],
                            );
                            raw_body.cmd_dispatch(
                                cmd,
                                groups,
                                groups,
                                crate::clouds::CLOUD_SHADOW_CASCADES,
                            );
                        }
                        drop(pipeline);
                    }),
            );
        }

        Some(CloudFrameResources {
            base: base_res,
            detail: detail_res,
            curl: curl_res,
            weather: weather_res,
            shadow: shadow_res,
            base_slot,
            detail_slot,
            curl_slot,
            weather_slot,
            shadow_slot,
            params_offset,
        })
    }

    /// Appends the weather-map refresh and unlit density debugger. Every persistent image is imported
    /// on an external layout slot so the graph owns the write/read transitions and the resolved layouts
    /// carry across frames. Weather resolves into one image for both procedural and painted sources.
    fn add_cloud_passes(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        inputs: CloudGraphInputs,
        prepared: Option<CloudFrameResources>,
    ) -> CloudGraphResult {
        let CloudGraphInputs {
            color,
            depth,
            motion,
            sky_sh,
        } = inputs;
        let lit_ready = pipelines.cloud_raymarch.is_some()
            && pipelines.cloud_reconstruct.is_some()
            && pipelines.cloud_upscale.is_some()
            && motion.is_some();
        let Some(prepared) = prepared else {
            return CloudGraphResult::default();
        };
        let view_index = self.active_view.index();
        let reduced_extent = self.views[view_index].cloud_reduced[0]
            .as_ref()
            .expect("cloud reduced built")
            .extent;
        let base_res = prepared.base;
        let detail_res = prepared.detail;
        let curl_res = prepared.curl;
        let weather_res = prepared.weather;
        let params_offset = prepared.params_offset;
        let mut result = CloudGraphResult {
            base: Some(prepared.base_slot),
            detail: Some(prepared.detail_slot),
            curl: Some(prepared.curl_slot),
            weather: Some(prepared.weather_slot),
            shadow: Some(prepared.shadow_slot),
            ..CloudGraphResult::default()
        };
        if pipelines.cloud_debug.is_none() && !lit_ready {
            graph.add_pass(
                RgPass::compute("cloud-resources-read")
                    .access(base_res, RgUsage::SampledReadCompute)
                    .access(detail_res, RgUsage::SampledReadCompute)
                    .access(curl_res, RgUsage::SampledReadCompute)
                    .access(weather_res, RgUsage::SampledReadCompute)
                    .access(prepared.shadow, RgUsage::SampledReadCompute)
                    .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
            );
            return result;
        }

        if let Some(pipeline) = &pipelines.cloud_debug {
            let pipeline = Arc::clone(pipeline);
            let handle = pipeline.handle();
            let layout = pipeline.layout();
            let set = self.clouds.debug_set(view_index);
            let extent = self.views[view_index].published_extent();
            let groups_x = extent.width.div_ceil(8);
            let groups_y = extent.height.div_ceil(8);
            let raw_body = self.device.raw().clone();
            graph.add_pass(
                RgPass::compute("cloud-density-debug")
                    .access(color, RgUsage::StorageImageRwCompute)
                    .access(depth, RgUsage::SampledReadCompute)
                    .access(base_res, RgUsage::SampledReadCompute)
                    .access(detail_res, RgUsage::SampledReadCompute)
                    .access(curl_res, RgUsage::SampledReadCompute)
                    .access(weather_res, RgUsage::SampledReadCompute)
                    .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                        unsafe {
                            raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                            raw_body.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::COMPUTE,
                                layout,
                                0,
                                &[set],
                                &[params_offset],
                            );
                            raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                        }
                        drop(pipeline);
                    }),
            );
            return result;
        }

        let motion = motion.expect("lit cloud motion resource");
        let parity = self.views[view_index].history_index;
        let view = &self.views[view_index];
        let mut reduced_resources = [None, None];
        for (index, image) in view.cloud_reduced.iter().enumerate() {
            let image = image.as_ref().expect("cloud reduced built");
            let slot = graph.alloc_external_state(image.graph_state());
            reduced_resources[index] = Some(graph.import_image(
                image.handle(),
                image.view(),
                vk::ImageAspectFlags::COLOR,
                image.layout,
                Some(slot),
            ));
            result.reduced[index] = Some(slot);
        }
        let reduced_resources = reduced_resources.map(|resource| resource.expect("cloud import"));
        let reduced_depth = view
            .cloud_reduced_depth
            .as_ref()
            .expect("cloud reduced depth built");
        let reduced_depth_slot = graph.alloc_external_state(reduced_depth.graph_state());
        let reduced_depth_res = graph.import_image(
            reduced_depth.handle(),
            reduced_depth.view(),
            vk::ImageAspectFlags::COLOR,
            reduced_depth.layout,
            Some(reduced_depth_slot),
        );
        result.reduced_depth = Some(reduced_depth_slot);
        let full_color = view
            .cloud_full_color
            .as_ref()
            .expect("cloud full color built");
        let full_color_slot = graph.alloc_external_state(full_color.graph_state());
        let full_color_res = graph.import_image(
            full_color.handle(),
            full_color.view(),
            vk::ImageAspectFlags::COLOR,
            full_color.layout,
            Some(full_color_slot),
        );
        result.full_color = Some(full_color_slot);
        result.full_color_resource = Some(full_color_res);
        let full_depth = view
            .cloud_full_depth
            .as_ref()
            .expect("cloud full depth built");
        let full_depth_slot = graph.alloc_external_state(full_depth.graph_state());
        let full_depth_res = graph.import_image(
            full_depth.handle(),
            full_depth.view(),
            vk::ImageAspectFlags::COLOR,
            full_depth.layout,
            Some(full_depth_slot),
        );
        result.full_depth = Some(full_depth_slot);
        result.full_depth_resource = Some(full_depth_res);

        let groups_x = reduced_extent.width.div_ceil(8);
        let groups_y = reduced_extent.height.div_ceil(8);
        let raymarch = Arc::clone(
            pipelines
                .cloud_raymarch
                .as_ref()
                .expect("lit cloud raymarch pipeline"),
        );
        let raymarch_handle = raymarch.handle();
        let raymarch_layout = raymarch.layout();
        let raymarch_set = self.clouds.raymarch_set(view_index, parity);
        let raw_body = self.device.raw().clone();
        graph.add_pass(
            RgPass::compute("cloud-raymarch")
                .access(reduced_resources[parity], RgUsage::StorageImageRwCompute)
                .access(reduced_depth_res, RgUsage::StorageImageRwCompute)
                .access(depth, RgUsage::SampledReadCompute)
                .access(base_res, RgUsage::SampledReadCompute)
                .access(detail_res, RgUsage::SampledReadCompute)
                .access(curl_res, RgUsage::SampledReadCompute)
                .access(weather_res, RgUsage::SampledReadCompute)
                .access(prepared.shadow, RgUsage::SampledReadCompute)
                .access(sky_sh, RgUsage::StorageReadCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            raymarch_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            raymarch_layout,
                            0,
                            &[raymarch_set],
                            &[params_offset],
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(raymarch);
                }),
        );

        let reconstruct = Arc::clone(
            pipelines
                .cloud_reconstruct
                .as_ref()
                .expect("lit cloud reconstruct pipeline"),
        );
        let reconstruct_handle = reconstruct.handle();
        let reconstruct_layout = reconstruct.layout();
        let reconstruct_set = self.clouds.reconstruct_set(view_index, parity);
        let raw_body = self.device.raw().clone();
        graph.add_pass(
            RgPass::compute("cloud-reconstruct")
                .access(reduced_resources[parity], RgUsage::StorageImageRwCompute)
                .access(reduced_resources[1 - parity], RgUsage::SampledReadCompute)
                .access(reduced_depth_res, RgUsage::SampledReadCompute)
                .access(motion, RgUsage::SampledReadCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            reconstruct_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            reconstruct_layout,
                            0,
                            &[reconstruct_set],
                            &[params_offset],
                        );
                        raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                    }
                    drop(reconstruct);
                }),
        );

        let extent = self.views[view_index].published_extent();
        let upscale = Arc::clone(
            pipelines
                .cloud_upscale
                .as_ref()
                .expect("lit cloud upscale pipeline"),
        );
        let upscale_handle = upscale.handle();
        let upscale_layout = upscale.layout();
        let upscale_set = self.clouds.upscale_set(view_index, parity);
        let raw_body = self.device.raw().clone();
        graph.add_pass(
            RgPass::compute("cloud-upscale")
                .access(full_color_res, RgUsage::StorageImageRwCompute)
                .access(reduced_resources[parity], RgUsage::SampledReadCompute)
                .access(reduced_depth_res, RgUsage::SampledReadCompute)
                .access(depth, RgUsage::SampledReadCompute)
                .access(full_depth_res, RgUsage::StorageImageRwCompute)
                .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                    unsafe {
                        raw_body.cmd_bind_pipeline(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            upscale_handle,
                        );
                        raw_body.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::COMPUTE,
                            upscale_layout,
                            0,
                            &[upscale_set],
                            &[params_offset],
                        );
                        raw_body.cmd_dispatch(
                            cmd,
                            extent.width.div_ceil(8),
                            extent.height.div_ceil(8),
                            1,
                        );
                    }
                    drop(upscale);
                }),
        );
        graph.add_pass(
            RgPass::compute("cloud-history-read")
                .access(reduced_resources[0], RgUsage::SampledReadCompute)
                .access(reduced_resources[1], RgUsage::SampledReadCompute)
                .access(reduced_depth_res, RgUsage::SampledReadCompute)
                .access(full_color_res, RgUsage::SampledReadCompute)
                .access(full_depth_res, RgUsage::SampledReadCompute)
                .body(|_cmd, _scopes: &mut NestedScopeRecorder| {}),
        );
        result.temporal = true;
        result
    }

    /// Appends the analytic height & distance fog composite: an in-place compute pass over the scene
    /// depth that blends `scene*T + inscatter*(1-T)` into the offscreen `color` before bloom. Reads
    /// `color` (`StorageImageRwCompute`, GENERAL) and `depth` (`SampledReadCompute`, DEPTH aspect →
    /// ShaderReadOnly); the sky-view LUT is bound directly on the per-view fog set. No-op unless fog
    /// is enabled this frame (the PSO is `None` otherwise). Resolves the per-frame `FogParams` UBO
    /// slice from the camera + directional light + authored settings before recording.
    fn add_fog_pass(
        &mut self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
        cloud: &CloudGraphResult,
        frame: usize,
    ) {
        let Some(pipeline) = &pipelines.fog else {
            return;
        };
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();

        // Resolve the per-frame fog params from the camera + directional light + authored settings.
        let inv_view_proj = self.scene_view_proj_unjittered().inverse();
        let inv_view = self.ssao.view().inverse();
        let eye = inv_view.col(3).truncate();
        let cam_forward = inv_view.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
        // The sun-inscatter lobe points TOWARD the sun: the directional light's travel direction
        // negated (falls back to straight down when there is no directional light).
        let sun_dir = (-self.sun_direction).normalize_or_zero();
        let atmosphere_live = self.scene_ibl().atmosphere_live();
        let use_sky_lut = if atmosphere_live { 1.0 } else { 0.0 };
        let f = self.fog;
        // Aerial perspective folds into this composite only when authored + the atmosphere baked its
        // LUTs; the fog term is neutral (`T_fog = 1`, `inScatter_fog = 0`) when fog itself is off but AP
        // keeps the composite alive.
        let ap_active = f.aerial_perspective && atmosphere_live;
        let cloud_present =
            cloud.full_color_resource.is_some() && cloud.full_depth_resource.is_some();
        let params = FogParams {
            inv_view_proj: inv_view_proj.to_cols_array_2d(),
            camera_pos: eye.to_array(),
            max_opacity: f.max_opacity,
            albedo: f.albedo.to_array(),
            start_distance: f.start_distance,
            emissive: f.emissive.to_array(),
            dir_exponent: f.directional_exponent,
            sun_dir: sun_dir.to_array(),
            use_sky_lut,
            dir_color: f.directional_color.to_array(),
            _pad0: 0.0,
            layer0: [f.density, f.height_falloff, f.height, 0.0],
            layer1: [f.layer2_density, f.layer2_falloff, f.layer2_height, 0.0],
            froxel: [
                if f.volumetric { 1.0 } else { 0.0 },
                crate::froxel_fog::FROXEL_NEAR,
                crate::FROXEL_FAR,
                // `.w` = the fog debug view mode: the composite outputs the froxel in-scatter +
                // opacity directly instead of compositing (volumetric mode only).
                if self.view_mode == ViewMode::Fog {
                    1.0
                } else {
                    0.0
                },
            ],
            cam_forward: cam_forward.to_array(),
            fog_enabled: if f.enabled { 1.0 } else { 0.0 },
            aerial: [
                if ap_active { 1.0 } else { 0.0 },
                crate::froxel_fog::FROXEL_NEAR,
                crate::AP_FAR_M,
                0.0,
            ],
            cloud: [
                if cloud_present { 1.0 } else { 0.0 },
                if atmosphere_live { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
            ap: self.aerial_params(1.0),
        };

        let vi = self.active_view.index();
        self.views[vi].write_fog(frame, &params);
        let set = self.views[vi].fog_set;
        let offset = self.views[vi].fog_ubo_offset(frame);
        let extent = self.views[vi].published_extent();
        let groups = |n: u32| n.div_ceil(8);
        let groups_x = groups(extent.width);
        let groups_y = groups(extent.height);

        let raw_body = self.device.raw().clone();
        let mut pass = RgPass::compute("height-fog")
            .access(color, RgUsage::StorageImageRwCompute)
            .access(depth, RgUsage::SampledReadCompute);
        if let Some(resource) = cloud.full_color_resource {
            pass = pass.access(resource, RgUsage::SampledReadCompute);
        }
        if let Some(resource) = cloud.full_depth_resource {
            pass = pass.access(resource, RgUsage::SampledReadCompute);
        }
        let pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch covers the
            // viewport (8×8 per group); the dynamic offset addresses this frame's `FogParams` slice.
            unsafe {
                raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                raw_body.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    layout,
                    0,
                    &[set],
                    &[offset],
                );
                raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
            }
            drop(pipeline);
        });
        graph.add_pass(pass);
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
        frame: usize,
    ) {
        let Some(tonemap) = &pipelines.tonemap else {
            return;
        };
        let view = &self.views[self.active_view.index()];
        // In-place on the DISPLAY-extent offscreen (after the resolve reconstructed it).
        let extent = view.published_extent();
        let push = TonemapPush::new(self.exposure_ev, self.tonemap_mode, self.night_factor);
        let groups = |n: u32| n.div_ceil(8);
        let groups_x = groups(extent.width);
        let groups_y = groups(extent.height);
        // The grade UBO (binding 1) is a dynamic-offset UBO — the dispatch selects this frame's slice.
        let grade_offset = view.grade_ubo_offset(frame);
        let set = view.tonemap_set;
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(tonemap);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let pass = RgPass::compute("tonemap")
            .access(color, RgUsage::StorageImageRwCompute)
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam. The PSO/set are valid this frame; the dispatch covers the
                // viewport; the dynamic offset addresses this frame's grade slice within the bound UBO.
                unsafe {
                    raw_body.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, handle);
                    raw_body.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::COMPUTE,
                        layout,
                        0,
                        &[set],
                        &[grade_offset],
                    );
                    raw_body.cmd_push_constants(
                        cmd,
                        layout,
                        vk::ShaderStageFlags::COMPUTE,
                        0,
                        bytemuck::bytes_of(&push),
                    );
                    raw_body.cmd_dispatch(cmd, groups_x, groups_y, 1);
                }
                drop(pipeline);
            });
        graph.add_pass(pass);
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
            1,
        );
    }

    /// Appends the Lit Wireframe overlay (the `LitWireframe` view mode): re-draws the scene
    /// geometry in line polygon mode over the post-tonemap `color`, depth-tested read-only
    /// against the persisted 1× `depth` so hidden edges are occluded. A no-op unless the
    /// mode's PSO is resolved (a `fill_mode_non_solid` device; else it falls back to plain
    /// Lit). One executor PSO replays every opaque/masked bucket's counted indirect draw
    /// with the camera viewProj push.
    #[allow(clippy::too_many_arguments)]
    fn add_lit_wireframe_pass(
        &self,
        graph: &mut RenderGraph,
        pipelines: &FramePipelines,
        color: RgResource,
        depth: RgResource,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        deformed_res: Option<RgResource>,
        executor_inputs: Option<crate::ExecutorDrawInputs>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
    ) {
        let Some(pipeline) = &pipelines.wireframe_overlay else {
            return;
        };
        // Re-drawn at DISPLAY extent, depth-tested against the display-extent overlay depth.
        let extent = self.views[self.active_view.index()].published_extent();
        let raw_for_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let view_proj = self.scene_draw_list.view_proj;
        let draws = executor_draws.to_vec();
        let pages_buffer = self.global_gpu_data.pages.buffer();
        let draw_count_supported = self.device.capabilities.draw_indirect_count;
        let mut pass = RgPass::graphics("lit-wireframe", extent)
            .color(color_load_store(color))
            .depth_attachment(depth_load_readonly(depth))
            .body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                if let Some(inputs) = executor_inputs {
                    record_executor_depth_family(
                        &raw_for_body,
                        cmd,
                        (handle, layout),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&view_proj),
                        bindless_set,
                        instance_set,
                        inputs,
                        pages_buffer,
                        draw_count_supported,
                        &draws,
                        false,
                    );
                }
                drop(pipeline);
            });
        if let Some(inputs) = executor_inputs {
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead);
        }
        let micro_candidates_res =
            graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
        pass = pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
        if let Some(wind_records) = self.wind_records_handle() {
            let wind_records_res = graph.import_buffer(wind_records, None);
            pass = pass.access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
        }
        if let Some(deformed) = deformed_res {
            pass = pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
        }
        graph.add_pass(pass);
    }

    /// Appends one compute pass: declare the `(resource, usage)` accesses (the graph
    /// derives the GENERAL ↔ ShaderReadOnly transitions), bind the set, optionally push
    /// `push`, and dispatch `(groups_x, groups_y, groups_z)`. Screen-space passes pass
    /// `groups_z = 1`; a 3D-grid pass (the froxel volume) passes the Z group count.
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
        groups_z: u32,
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
                raw_body.cmd_dispatch(cmd, groups_x, groups_y, groups_z);
            }
            drop(pipeline);
        });
        for &(resource, usage) in accesses {
            pass = pass.access(resource, usage);
        }
        graph.add_pass(pass);
    }

    /// Appends a compute pass whose group count is read from `args_buffer` at `args_offset` (a
    /// `VkDispatchIndirectCommand {x,y,z}`) rather than known on the CPU — the sibling of
    /// [`Renderer::add_compute_pass`] for a GPU-determined dispatch (the Phase-4 tessellator's per-frame
    /// output size). The caller lists `args_buffer`'s resource as [`RgUsage::IndirectCommandRead`] in
    /// `accesses` so the graph derives the producer→dispatch barrier.
    #[allow(dead_code, clippy::too_many_arguments)] // wired by Phase 4 (the amplifying tessellator)
    fn add_indirect_compute_pass(
        &self,
        graph: &mut RenderGraph,
        name: &'static str,
        pipeline: &Arc<crate::Pipeline>,
        set: vk::DescriptorSet,
        accesses: &[(RgResource, RgUsage)],
        push: Option<Vec<u8>>,
        args_buffer: vk::Buffer,
        args_offset: vk::DeviceSize,
    ) {
        let raw_body = self.device.raw().clone();
        let pipeline = Arc::clone(pipeline);
        let handle = pipeline.handle();
        let layout = pipeline.layout();
        let mut pass = RgPass::compute(name).body(move |cmd, _scopes: &mut NestedScopeRecorder| {
            // SAFETY: the ash seam. The PSO/set are valid this frame; the group count is read from
            // the args buffer, whose producing write the graph ordered ahead via IndirectCommandRead.
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
                raw_body.cmd_dispatch_indirect(cmd, args_buffer, args_offset);
            }
            drop(pipeline);
        });
        for &(resource, usage) in accesses {
            pass = pass.access(resource, usage);
        }
        graph.add_pass(pass);
    }

    #[allow(clippy::too_many_arguments)]
    /// Rasterizes this frame's dirty virtual-shadow pages: per directional level,
    /// the level's own small visibility view culls with the level window (no
    /// occlusion history), traversal + binning build the level's indirect stream,
    /// and one atlas pass draws every dirty page into its tile (clear rect +
    /// dynamic viewport/scissor + the page's ortho sub-window push).
    #[allow(clippy::too_many_arguments)]
    fn add_vsm_page_passes(
        &mut self,
        graph: &mut RenderGraph,
        frame: usize,
        shadow: &Arc<crate::Pipeline>,
        bindless_set: vk::DescriptorSet,
        instance_set: vk::DescriptorSet,
        deformed_res: Option<RgResource>,
        executor_draws: &[(crate::ExecutorBucket, bool, Arc<crate::Pipeline>)],
        wind_records_res: RgResource,
        cull_pso: &Arc<crate::Pipeline>,
        traversal_pso: &Arc<crate::Pipeline>,
        bin_psos: (
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
            &Arc<crate::Pipeline>,
        ),
        micro_template: (u32, u32),
        instance_capacity: u32,
    ) -> Result<Option<RgResource>> {
        if self.vsm_render_pages.is_empty() {
            return Ok(None);
        }
        let Some(pyramid) = self.views[self.active_view.index()].hzb_pyramid.as_ref() else {
            // No pyramid to bind as the (never-sampled) cull placeholder; the pages
            // stay staged and re-mark dirty at the next prepare.
            return Ok(None);
        };
        let (previous_image, previous_view) = pyramid.previous();
        let previous_layout = pyramid.previous_layout();
        let hzb_extent = [pyramid.extent().width, pyramid.extent().height];
        let hzb_mips = pyramid.mip_count();
        let pages = std::mem::take(&mut self.vsm_render_pages);
        let address_slice = (
            self.gpu_scene_uploader.address_buffer(),
            frame as u64 * self.gpu_scene_uploader.address_block_stride(),
            size_of::<crate::GpuSceneAddressBlock>() as u64,
        );
        let atlas_slot = graph.alloc_external_state(self.vsm_gpu.atlas_state);
        let atlas_res = graph.import_image(
            self.vsm_gpu.atlas.handle(),
            self.vsm_gpu.atlas.view(),
            vk::ImageAspectFlags::DEPTH,
            self.vsm_gpu.atlas_state.layout,
            Some(atlas_slot),
        );
        let demand = self.page_demand_view();
        let space = self.vsm_space;
        let spot_view_proj = self.lighting.spot_shadow_view_proj();
        let frame_stamp = self.frame_serial as u32;
        // Page groups: one per directional level, the spot space, and each point
        // cube face — every group culls with its own frustum and derives per-page
        // matrices from its own space.
        let mut groups: Vec<(usize, Mat4, Vec<crate::VsmRenderPage>)> = Vec::new();
        for level in 0..crate::VSM_DIRECTIONAL_LEVELS {
            let level_pages: Vec<crate::VsmRenderPage> = pages
                .iter()
                .copied()
                .filter(|page| {
                    matches!(page.key, crate::VsmPageKey::Directional { level: l, .. } if l == level)
                })
                .collect();
            if !level_pages.is_empty() {
                groups.push((level as usize, space.level_view_proj(level), level_pages));
            }
        }
        let spot_pages: Vec<crate::VsmRenderPage> = pages
            .iter()
            .copied()
            .filter(|page| matches!(page.key, crate::VsmPageKey::Spot { .. }))
            .collect();
        if !spot_pages.is_empty() {
            groups.push((
                crate::VSM_DIRECTIONAL_LEVELS as usize,
                spot_view_proj,
                spot_pages,
            ));
        }
        let point_faces = crate::point_shadow_face_matrices(
            self.lighting.point_shadow_pos(),
            self.lighting.point_shadow_far(),
        );
        for face in 0..crate::vsm::VSM_POINT_FACES {
            let face_pages: Vec<crate::VsmRenderPage> = pages
                .iter()
                .copied()
                .filter(|page| {
                    matches!(page.key, crate::VsmPageKey::PointFace { face: f, .. } if f == face)
                })
                .collect();
            if !face_pages.is_empty() {
                groups.push((
                    (crate::VSM_DIRECTIONAL_LEVELS + 1 + face) as usize,
                    point_faces[face as usize],
                    face_pages,
                ));
            }
        }
        for (view_slot, cull_view_proj, group_pages) in groups {
            let needs_view = self.vsm_views[view_slot]
                .as_ref()
                .is_none_or(|view| view.capacity() < instance_capacity);
            if needs_view {
                self.device.wait_idle()?;
                if let Some(mut old) = self.vsm_views[view_slot].take() {
                    old.free_sets(&self.descriptors);
                }
                self.vsm_views[view_slot] = Some(crate::SceneVisibilityView::new(
                    &self.device,
                    &self.descriptors,
                    &self.scene_visibility,
                    instance_capacity,
                    crate::SCENE_VISIBILITY_RECORD_CAPACITY,
                    1,
                )?);
            }
            let Some(view) = self.vsm_views[view_slot].as_ref() else {
                continue;
            };
            view.write_frame_bindings(
                &self.device,
                &self.scene_visibility,
                frame,
                previous_view,
                previous_view,
                address_slice,
            );
            // The frame's bucket vocabulary is shared with the camera view; the
            // level's bin passes need the same table.
            let (_, bucket_table) = crate::build_executor_buckets(
                &self.live_executor_bins,
                crate::SCENE_VISIBILITY_RECORD_CAPACITY,
            );
            view.write_bucket_table(frame, &bucket_table);
            let previous_res = graph.import_image(
                previous_image,
                previous_view,
                vk::ImageAspectFlags::COLOR,
                previous_layout,
                None,
            );
            let level_view_proj = cull_view_proj.to_cols_array();
            view.add_cull_pass(
                &self.device,
                graph,
                cull_pso,
                frame,
                previous_res,
                wind_records_res,
                instance_capacity,
                crate::SceneVisibilityPush {
                    view_proj: level_view_proj,
                    prev_view_proj: level_view_proj,
                    hzb_extent,
                    hzb_mip_count: hzb_mips,
                    pass_kind: crate::SCENE_VISIBILITY_PASS_CULL,
                    history_valid: 0,
                    list_capacity: view.capacity(),
                    reserved: [0; 2],
                },
            );
            view.add_traversal_pass(
                &self.device,
                graph,
                traversal_pso,
                frame,
                crate::SceneTraversalPush {
                    eye: demand.eye.to_array(),
                    proj_scale: demand.proj_scale,
                    error_threshold_px: 1.0,
                    record_capacity: view.record_capacity(),
                    list_capacity: view.capacity(),
                    survivor: 0,
                    tess_seam: 0,
                    // Shadow pages draw settled cuts: a nonzero value here would let
                    // every page view mutate the shared flip-state table with its own
                    // refine decisions and fabricate camera-view crossfades.
                    transition_frames: 0,
                    frame_stamp,
                    reserved0: 0,
                },
            );
            view.add_binning_passes(&self.device, graph, bin_psos, frame, false, micro_template);

            let inputs = view.executor_draw_inputs(frame, self.live_draw_record_bound);
            let raw_body = self.device.raw().clone();
            let shadow_pipeline = shadow.handle();
            let shadow_layout = shadow.layout();
            let shadow_keep = Arc::clone(shadow);
            let draws = executor_draws.to_vec();
            let pages_buffer = self.global_gpu_data.pages.buffer();
            let draw_count_supported = self.device.capabilities.draw_indirect_count;
            let render_pages = group_pages.clone();
            let extent = vk::Extent2D {
                width: crate::VSM_ATLAS_SIZE,
                height: crate::VSM_ATLAS_SIZE,
            };
            let pass = RgPass::graphics("vsm-pages", extent).depth_attachment(RgAttachment {
                resource: atlas_res,
                load_op: vk::AttachmentLoadOp::LOAD,
                store_op: vk::AttachmentStoreOp::STORE,
                clear_value: vk::ClearValue::default(),
                resolve: None,
            });
            let pass_inputs = inputs;
            let mut pass = pass.body(move |cmd, _scopes: &mut NestedScopeRecorder| {
                // SAFETY: the ash seam; `cmd` is recording inside the pass.
                unsafe {
                    raw_body.cmd_set_depth_bias(
                        cmd,
                        crate::lighting::SHADOW_DEPTH_BIAS_CONSTANT,
                        0.0,
                        crate::lighting::SHADOW_DEPTH_BIAS_SLOPE,
                    );
                }
                for page in &render_pages {
                    let tile = vk::Rect2D {
                        offset: vk::Offset2D {
                            x: ((page.tile % crate::VSM_ATLAS_TILES) * crate::VSM_PAGE_SIZE) as i32,
                            y: ((page.tile / crate::VSM_ATLAS_TILES) * crate::VSM_PAGE_SIZE) as i32,
                        },
                        extent: vk::Extent2D {
                            width: crate::VSM_PAGE_SIZE,
                            height: crate::VSM_PAGE_SIZE,
                        },
                    };
                    let viewport = vk::Viewport {
                        x: tile.offset.x as f32,
                        y: tile.offset.y as f32,
                        width: crate::VSM_PAGE_SIZE as f32,
                        height: crate::VSM_PAGE_SIZE as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    };
                    let clear = vk::ClearAttachment {
                        aspect_mask: vk::ImageAspectFlags::DEPTH,
                        color_attachment: 0,
                        clear_value: vk::ClearValue {
                            depth_stencil: vk::ClearDepthStencilValue {
                                depth: 1.0,
                                stencil: 0,
                            },
                        },
                    };
                    let clear_rect = vk::ClearRect {
                        rect: tile,
                        base_array_layer: 0,
                        layer_count: 1,
                    };
                    // SAFETY: the ash seam; the tile lies inside the atlas attachment.
                    unsafe {
                        raw_body.cmd_set_viewport(cmd, 0, &[viewport]);
                        raw_body.cmd_set_scissor(cmd, 0, &[tile]);
                        raw_body.cmd_clear_attachments(cmd, &[clear], &[clear_rect]);
                    }
                    let page_view_proj = match page.key {
                        crate::VsmPageKey::Directional { level, x, y } => {
                            space.page_view_proj(level, x, y)
                        }
                        crate::VsmPageKey::Spot { x, y } => {
                            crate::vsm::vsm_page_crop(crate::vsm::VSM_SPOT_PAGES, x, y)
                                * spot_view_proj
                        }
                        crate::VsmPageKey::PointFace { face, x, y } => {
                            crate::vsm::vsm_page_crop(crate::vsm::VSM_POINT_FACE_PAGES, x, y)
                                * point_faces[face as usize]
                        }
                    };
                    record_executor_depth_family(
                        &raw_body,
                        cmd,
                        (shadow_pipeline, shadow_layout),
                        vk::ShaderStageFlags::VERTEX,
                        bytemuck::bytes_of(&page_view_proj),
                        bindless_set,
                        instance_set,
                        pass_inputs,
                        pages_buffer,
                        draw_count_supported,
                        &draws,
                        false,
                    );
                }
                drop(shadow_keep);
            });
            let pages_res = graph.import_buffer(pages_buffer, None);
            let commands_res = graph.import_buffer(inputs.commands, None);
            let counters_res = graph.import_buffer(inputs.counters, None);
            let bucket_counts_res = graph.import_buffer(inputs.bucket_counts, None);
            pass = pass
                .access(pages_res, RgUsage::IndexInputRead)
                .access(commands_res, RgUsage::IndirectCommandRead)
                .access(counters_res, RgUsage::IndirectCountRead)
                .access(bucket_counts_res, RgUsage::IndirectCountRead)
                .access(wind_records_res, RgUsage::ShaderDeviceAddressRead);
            let micro_candidates_res =
                graph.import_buffer(self.global_gpu_data.micro_candidates.handle(), None);
            pass = pass.access(micro_candidates_res, RgUsage::ShaderDeviceAddressRead);
            if let Some(deformed) = deformed_res {
                pass = pass.access(deformed, RgUsage::ShaderDeviceAddressRead);
            }
            graph.add_pass(pass);
        }
        Ok(Some(atlas_res))
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
        // Rebuilds run between frames. An outstanding acquisition owns a binary semaphore and
        // must be presented rather than discarded, so enforce the transaction boundary.
        if let Some(present_sync) = self.present_sync.as_ref() {
            present_sync.ensure_no_acquired_frame()?;
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
            present_sync.set_acquired_frame(image_index, self.frames.index())?;
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
        let Some(acquired) = self
            .present_sync
            .as_mut()
            .and_then(PresentSync::take_acquired_frame)
        else {
            return Ok(()); // No image acquired (out-of-date swapchain): skip the present.
        };
        let slot = acquired.slot;
        let image_index = acquired.image_index;
        let scene_signaled = acquired.scene_finished_signaled;

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

        let swapchain = self.present_swapchain();
        let swap_image = swapchain.image(image_index as usize);
        let swap_extent = swapchain.extent;
        let render_finished = swapchain.render_finished(image_index as usize);
        let tracking = swapchain.image_in_flight(image_index as usize);

        // Both the slot's prior present and the acquired image's prior present must complete
        // before their resources are reused. They may be the same fence, so deduplicate and wait
        // before resetting the slot fence; resetting first would turn the alias case into an
        // infinite wait on the newly-unsignaled fence.
        let (reuse_fences, reuse_fence_count) =
            crate::present::reuse_fences(present_fence, tracking);
        // SAFETY: the ash seam. Every returned fence belongs to this device.
        checked(
            unsafe { raw.wait_for_fences(&reuse_fences[..reuse_fence_count], true, u64::MAX) },
            "present: wait_for_fences(reuse)",
        )?;
        // SAFETY: the ash seam. The slot fence was waited above and is reset before resubmit.
        checked(
            unsafe { raw.reset_fences(&[present_fence]) },
            "present: reset_fences",
        )?;
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
        self.device.graphics_queue.submit2(
            raw,
            &submit,
            present_fence,
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
        let present = self
            .device
            .graphics_queue
            .present(self.device.swapchain_loader(), &present_info);
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
        self.device.graphics_queue.submit2(
            raw,
            &submits,
            self.frames.in_flight(),
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
        let present = self
            .device
            .graphics_queue
            .present(self.device.swapchain_loader(), &present_info);
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
/// Declares one pass's reads on the tessellation seam's transient VB/IB/args (shared
/// across every tess draw), so the graph orders the pass after the emit kernel's
/// writes. `with_prev` also declares the previous micro-vertex stream (the motion
/// pass binds it). A no-op when no tess draw resolved this frame.
fn access_tess_draws(
    graph: &mut RenderGraph,
    pass: RgPass,
    draws: &[crate::TessSceneDraw],
    with_prev: bool,
) -> RgPass {
    let Some(handles) = draws.iter().find_map(|draw| draw.draw) else {
        return pass;
    };
    let vb = graph.import_buffer(handles.vertex_buffer, None);
    let ib = graph.import_buffer(handles.index_buffer, None);
    let args = graph.import_buffer(handles.args_buffer, None);
    let mut pass = pass
        .access(vb, RgUsage::VertexInputRead)
        .access(ib, RgUsage::IndexInputRead)
        .access(args, RgUsage::IndirectCommandRead);
    if with_prev {
        let prev = graph.import_buffer(handles.prev_vertex_buffer, None);
        pass = pass.access(prev, RgUsage::VertexInputRead);
    }
    pass
}

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

/// A `LOAD`-then-`STORE` depth attachment: continue depth-testing and writing over the
/// scene's laid-down depth (the survivor raster redraws over the provisional cut).
fn depth_load_store(resource: RgResource) -> RgAttachment {
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
    let slot = graph.alloc_external_state(image.graph_state());
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
        image.set_graph_state(graph.external_state(slot.1));
    }
}

/// Writes a TAA pixel-lock image's resolved exit layout back from the graph's external slot.
/// `(lock-index, slot)` selects the image in `view.lock` and the slot to read.
fn writeback_lock_layout(view: &mut ViewTarget, graph: &RenderGraph, slot: &(usize, usize)) {
    if let Some(image) = view.lock[slot.0].as_mut() {
        image.set_graph_state(graph.external_state(slot.1));
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
        image.set_graph_state(graph.external_state(slot.1));
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
        image.set_graph_state(graph.external_state(slot.1));
    }
}

fn merge_timeline_point(points: &mut Vec<FrameTimelinePoint>, point: FrameTimelinePoint) {
    if let Some(existing) = points
        .iter_mut()
        .find(|existing| existing.semaphore == point.semaphore)
    {
        existing.value = existing.value.max(point.value);
    } else {
        points.push(point);
    }
}

struct GraphCommandSubmission<'a> {
    queue: RgQueueAssignment,
    command_buffer: vk::CommandBuffer,
    waits: &'a [FrameTimelinePoint],
    signals: &'a [FrameTimelinePoint],
    binary_signal: Option<vk::Semaphore>,
    fence: vk::Fence,
    context: &'static str,
}

fn submit_graph_command(device: &Device, submission: GraphCommandSubmission<'_>) -> Result<()> {
    let wait_infos = submission
        .waits
        .iter()
        .map(|point| {
            vk::SemaphoreSubmitInfo::default()
                .semaphore(point.semaphore)
                .value(point.value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        })
        .collect::<Vec<_>>();
    let mut signal_infos = submission
        .signals
        .iter()
        .map(|point| {
            vk::SemaphoreSubmitInfo::default()
                .semaphore(point.semaphore)
                .value(point.value)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
        })
        .collect::<Vec<_>>();
    if let Some(semaphore) = submission.binary_signal {
        signal_infos.push(
            vk::SemaphoreSubmitInfo::default()
                .semaphore(semaphore)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS),
        );
    }
    let commands =
        [vk::CommandBufferSubmitInfo::default().command_buffer(submission.command_buffer)];
    let submits = [vk::SubmitInfo2::default()
        .wait_semaphore_infos(&wait_infos)
        .command_buffer_infos(&commands)
        .signal_semaphore_infos(&signal_infos)];
    let queue = match submission.queue {
        RgQueueAssignment::Graphics => &device.graphics_queue,
        RgQueueAssignment::AsyncCompute => device.compute_queue.as_ref().ok_or(
            Error::PresentState("async-compute batch has no async-compute queue"),
        )?,
    };
    queue.submit2(device.raw(), &submits, submission.fence, submission.context)
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
        self.gpu_profiler.destroy_pools(&self.device);
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
                device
                    .graphics_queue
                    .submit2(raw, &[submit], fence, "submit")?;
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
        // Binding 2 of the tonemap set is the always-bound creative LUT; bind the neutral
        // identity ramp exactly as renderer bring-up does.
        let queue = device.graphics_queue.clone();
        let uploader = crate::upload::Uploader::new(&device, &queue).expect("Uploader");
        let identity_lut = uploader.upload_identity_lut().expect("identity LUT");
        view.write_tonemap_lut(&device, descriptors.linear_sampler(), identity_lut.view());

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
        let exposure = TonemapPush::new(0.0, crate::overlay::TonemapMode::PbrNeutral, 0.0);

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
        drop(identity_lut);
        drop(uploader);
        drop(queue);
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
        assert_eq!(ViewId::Thumbnail.index(), 2);
        assert_eq!(ViewId::Scene.wire(), "scene");
        assert_eq!(ViewId::AssetPreview.wire(), "assetPreview");
        assert_eq!(ViewId::Thumbnail.wire(), "thumbnail");
        assert_eq!(ViewId::from_wire("scene"), Some(ViewId::Scene));
        assert_eq!(
            ViewId::from_wire("assetPreview"),
            Some(ViewId::AssetPreview)
        );
        assert_eq!(ViewId::from_wire("nope"), None);
        // The offscreen Thumbnail view is not wire-selectable, so its token never parses back.
        assert_eq!(ViewId::from_wire("thumbnail"), None);
        // Round-trip the two presenter-facing variants through their wire tokens.
        for view in [ViewId::Scene, ViewId::AssetPreview] {
            assert_eq!(ViewId::from_wire(view.wire()), Some(view));
        }
        // The dense index round-trips for all three, including the offscreen view.
        for view in [ViewId::Scene, ViewId::AssetPreview, ViewId::Thumbnail] {
            assert_eq!(ViewId::from_index(view.index()), view);
        }
        assert_eq!(VIEW_COUNT, 3);
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
                device
                    .graphics_queue
                    .submit2(raw, &submit, fence, "submit")?;
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

            // Tonemap (mandatory, in-place compute). Binding 1 is the dynamic-offset grade
            // UBO; the readback records one frame, so it selects frame slot 0's slice.
            let raw_tm = raw_body.clone();
            let push = exposure;
            let grade_offset = view.grade_ubo_offset(0);
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
                                &[grade_offset],
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
                device
                    .graphics_queue
                    .submit2(raw, &submit, fence, "submit")?;
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
            .submit_gpu_scene_deformations(proj * view, &[], &[])
            .expect("submit_gpu_scene_deformations");
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
            device
                .graphics_queue
                .submit2(raw, &submit, fence, "submit")
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

    /// A displaced (`HeightMode::Displacement`) instance driven through `render_scene_offscreen`
    /// exercises the FULL adaptive-tessellation path — the factor/scan/finalize/args/emit compute chain,
    /// the transient VB/IB + their `cmd_fill_buffer` clears + storage descriptor bindings, the boundary +
    /// interior geomorph, the prev-stream, and (RT armed) the coarse RT dice + `TessellatedBlas` build.
    /// The default headless smoke has NO displaced mesh, so this is the ONLY automated coverage of the
    /// tess buffers' usage flags — the exact Vulkan-validation class (`vkCmdFillBuffer` needs
    /// `TRANSFER_DST`; a storage `ByteAddressBuffer` binding needs `STORAGE_BUFFER`; a BLAS-input buffer
    /// needs `ACCEL_BUILD_INPUT`) that a non-displaced scene cannot surface. Two frames so the prev-stream
    /// ping-pong runs with real previous factors. Asserts the whole displaced frame is validation-clean.
    #[test]
    fn displaced_instance_tessellation_frame_is_validation_clean() {
        use crate::draw_list::SubmeshMaterial;
        use crate::upload::Uploader;
        use saffron_core::HeightMode;
        use saffron_geometry::glam::{Mat4, Vec2, Vec3};
        use saffron_geometry::{Mesh, Submesh, Vertex};
        use std::sync::Arc;

        let mut renderer = match Renderer::new(&SurfaceSource::Offscreen, 128, 128) {
            Ok(r) => r,
            Err(err) => {
                eprintln!("skipping: no Vulkan device obtainable ({err})");
                return;
            }
        };
        let before = validation_issue_count();

        // Arm RT so the coarse RT dice + `TessellatedBlas` build run too (the `.rt` transient buffers),
        // exercising both tess paths when the device supports ray tracing.
        if renderer.rt_supported() {
            renderer.set_rt_shadows(true);
        }

        // Upload a UV'd quad (watertight conditioning is built at upload) + a non-flat height map (whose
        // min/max pyramid drives the per-region factor) into the renderer's bindless descriptors.
        let queue = renderer.device().graphics_queue.clone();
        let uploader = Uploader::new(renderer.device(), &queue).expect("Uploader");
        let vert = |x: f32, z: f32, u: f32, w: f32| Vertex {
            position: Vec3::new(x, 0.0, z),
            normal: Vec3::new(0.0, 1.0, 0.0),
            uv0: Vec2::new(u, w),
            ..Vertex::default()
        };
        let mesh = Mesh {
            vertices: vec![
                vert(-1.0, -1.0, 0.0, 0.0),
                vert(1.0, -1.0, 1.0, 0.0),
                vert(1.0, 1.0, 1.0, 1.0),
                vert(-1.0, 1.0, 0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 6,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        let hierarchy = crate::upload::hierarchy_for_upload(&mesh, &[]).expect("cook hierarchy");
        let mesh = uploader
            .upload_mesh(renderer.descriptors(), &mesh, &hierarchy, &[], None, None)
            .expect("upload_mesh");
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            px[0] = ((i * 37) % 256) as u8; // a busy height in R so the per-region factor refines
            px[3] = 255;
        }
        let height = uploader
            .upload_height_texture(renderer.descriptors(), &rgba, 8, 8)
            .expect("upload_height_texture");

        let displaced_work = || {
            let mut material = SubmeshMaterial::defaults();
            material.height_texture = Some(Arc::clone(&height));
            material.height_mode = HeightMode::Displacement;
            material.height_scale = 0.2;
            let displace = crate::displace_info_from(std::slice::from_ref(&material))
                .expect("displaced material");
            crate::DeformationWork {
                mesh: Arc::clone(&mesh),
                entity: 7,
                skinned: false,
                joint_offset: 0,
                joint_count: 0,
                morph_weights: Vec::new(),
                model: Mat4::IDENTITY,
                displace: Some(displace),
                material: crate::Material::default(),
                submesh_materials: vec![material],
                parameter_index: 0,
            }
        };

        // A close camera so the projected factor exceeds 1 and the dice/emit actually amplify (and, at
        // the default cap, may split — exercising the subpatch path).
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), 1.0, 0.05, 100.0);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 1.5, 1.5), Vec3::ZERO, Vec3::Y);
        let view_proj = proj * view;
        for frame in 0..2 {
            renderer.submit_sky(&SkyRenderSettings::default());
            renderer
                .set_scene_lighting(&SceneLighting::default())
                .expect("set_scene_lighting");
            renderer
                .submit_gpu_scene_deformations(view_proj, &[displaced_work()], &[])
                .expect("submit_gpu_scene_deformations");
            renderer
                .render_scene_offscreen()
                .unwrap_or_else(|err| panic!("render_scene_offscreen frame {frame}: {err}"));
        }
        renderer
            .device()
            .wait_idle()
            .expect("idle after the displaced frames");

        let after = validation_issue_count();
        assert_eq!(
            before,
            after,
            "the displaced tessellation frame must be validation-clean (saw {} new issue(s))",
            after.saturating_sub(before)
        );
    }
}
