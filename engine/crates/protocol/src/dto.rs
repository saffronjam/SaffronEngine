//! The control-plane DTOs: 236 structs + 17 enums, in field declaration order (the
//! positional-CLI-argument / OpenRPC-`required` order).
//!
//! Conventions, applied uniformly:
//! - `#[serde(rename_all = "camelCase")]` on every struct — the wire keys are camelCase, so this
//!   only normalizes the Rust `snake_case` fields back to the frozen wire spelling.
//! - `Option<T>` fields carry `skip_serializing_if = "Option::is_none"` so an absent value is
//!   a *missing key*, not `null`.
//! - The 17 enums are kebab-case strings; an unknown value is a `Deserialize` error.
//! - The wire-helpers `EntitySelector` / `AssetSelector` and every `Json`-typed field are
//!   opaque `serde_json::Value` — their shape is owned by the scene component registry, not
//!   here. `WireUuid` maps to the [`Uuid`](crate::Uuid) newtype.

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::Uuid;

/// Coercing readers for the wire-contract bool fields: a bool field on the
/// wire accepts a JSON bool, a number (`!= 0`), or a string (everything but `"0"`/`"false"`/`"off"`
/// is true). The `sa` CLI and the editor pass `1`/`0` and `"on"`/`"off"` for toggles, so an
/// input bool param must accept those forms or the toggle silently no-ops. Applied via
/// `#[serde(deserialize_with = ...)]` on every *params* bool field; result bools serialize as
/// plain JSON bools and need no coercion.
mod coerce {
    use super::{Deserialize, Deserializer, Value};

    /// Coerce one JSON value to a bool: a bool, a number (`!= 0`), or a string
    /// (anything but `"0"`/`"false"`/`"off"` is true).
    fn from_value<E: serde::de::Error>(value: &Value) -> Result<bool, E> {
        match value {
            Value::Bool(b) => Ok(*b),
            Value::Number(n) => Ok(n.as_f64().is_some_and(|f| f != 0.0)),
            Value::String(s) => Ok(!(s == "0" || s == "false" || s == "off")),
            other => Err(serde::de::Error::custom(format!(
                "expected a boolean, got {other}"
            ))),
        }
    }

    /// `#[serde(deserialize_with)]` for a required bool field.
    pub fn boolean<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
        let value = Value::deserialize(deserializer)?;
        from_value(&value)
    }

    /// `#[serde(deserialize_with, default)]` for an optional bool field.
    pub fn opt_boolean<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<bool>, D::Error> {
        match Option::<Value>::deserialize(deserializer)? {
            Some(Value::Null) | None => Ok(None),
            Some(value) => from_value(&value).map(Some),
        }
    }
}

/// An opaque entity selector blob (a uuid, a name, or `{}` for the current selection),
/// resolved by the runtime — not a typed sub-DTO.
pub type EntitySelector = Value;

/// An opaque asset selector blob (a uuid or a path), resolved by the runtime.
pub type AssetSelector = Value;

// Wire-helper structs (`EntityRef`, `Vec3`, `Vec4`) and the 17 enums.

/// A `{ x, y, z }` vector on the wire (a JSON object, not an array).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// A `{ x, y, z, w }` vector on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Vec4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

/// A resolved entity reference (the selection echo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityRef {
    pub id: Uuid,
    pub name: String,
}

/// The standalone app manifest: the window identity + present options the exported
/// `saffron-player` boots with. It is the project's persisted `app` config block (set in the
/// editor's Export dialog), written verbatim to `app.json` beside the player binary at export.
/// `#[serde(default)]` lets a partial or absent `app.json` fall back field-by-field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AppManifest {
    /// The window title (and the default export folder name).
    pub title: String,
    /// The initial window width in pixels.
    pub width: u32,
    /// The initial window height in pixels.
    pub height: u32,
    /// Whether the window starts fullscreen.
    pub fullscreen: bool,
    /// Whether to present with vsync.
    pub vsync: bool,
}

impl Default for AppManifest {
    fn default() -> Self {
        Self {
            title: "Saffron App".to_string(),
            width: 1280,
            height: 720,
            fullscreen: false,
            vsync: true,
        }
    }
}

/// `export-app` params: cook the open project into a standalone app folder at `outputDir`, using
/// `app` as the runtime manifest (written to `app.json` beside the player binary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportAppParams {
    /// The destination directory for the staged app (created if absent).
    pub output_dir: String,
    /// The runtime manifest to write into the staged `app.json`.
    pub app: AppManifest,
}

/// `export-app` result: the staged app directory and any non-fatal warnings raised during cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportAppResult {
    /// The staged app directory (the folder containing the player binary + data).
    pub path: String,
    /// Non-fatal warnings (e.g. a material that failed to pre-bake), for the editor to surface.
    pub warnings: Vec<String>,
}

/// The `add-entity` preset selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AddEntityPreset {
    Empty,
    Cube,
    Plane,
    Sphere,
    PointLight,
    SpotLight,
    DirectionalLight,
    Camera,
    ReflectionProbe,
}

/// What a viewport pick resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PickKind {
    Billboard,
    Mesh,
}

/// The active gizmo operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GizmoOpDto {
    Translate,
    Rotate,
    Scale,
}

/// The gizmo reference frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GizmoSpaceDto {
    World,
    Local,
}

/// A gizmo pointer-interaction phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GizmoPointerPhase {
    Hover,
    Begin,
    Drag,
    End,
}

/// The anti-aliasing mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AaModeDto {
    Off,
    Fxaa,
    Taa,
    Msaa2,
    Msaa4,
    Msaa8,
}

/// The global-illumination mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum GiModeDto {
    Off,
    Ddgi,
}

/// Debug render-output mode (read back via render-stats; transient, not persisted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ViewModeDto {
    Lit,
    Unlit,
    Wireframe,
    LitWireframe,
    DetailLighting,
    LightingOnly,
    Reflections,
    Albedo,
    Normal,
    Roughness,
    Metallic,
    Emissive,
    Depth,
    AmbientOcclusion,
    Gi,
    LightComplexity,
    MotionVectors,
}

/// The asset slot an `assign-asset` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AssetSlotDto {
    Mesh,
    Albedo,
    MetallicRoughness,
    Normal,
    Occlusion,
    Emissive,
    Height,
}

/// The surface a screenshot captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ScreenshotTargetDto {
    Viewport,
    Window,
}

/// The catalog asset kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AssetTypeDto {
    Mesh,
    Texture,
    Other,
    Animation,
    Material,
    Model,
}

/// The profiler capture mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProfilerModeDto {
    Off,
    Timestamps,
    PipelineStats,
}

/// A profile span's lane (CPU vs GPU).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProfileLaneDto {
    Cpu,
    Gpu,
}

/// The capture recorder mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CaptureModeDto {
    Single,
    Frames,
    Rolling,
}

/// The capture recorder state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum CaptureStateDto {
    Idle,
    Arming,
    Recording,
    Ready,
}

/// A performance-alarm severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AlarmSeverityDto {
    Info,
    Warning,
    Critical,
}

/// A performance-alarm lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AlarmStateDto {
    Firing,
    Resolved,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct PingParams {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PingResult {
    pub pong: bool,
    pub engine: String,
    pub version: String,
    pub pid: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderStatsDto {
    pub draw_calls: i32,
    pub batches: i32,
    pub instances: i32,
    pub frame_ms: f32,
    pub fps: f32,
    pub gpu_ms: f32,
    pub cpu_frame_ms: f32,
    pub gpu_frame_ms: f32,
    pub cpu_wait_ms: f32,
    pub triangles: i32,
    pub descriptor_binds: i32,
    pub command_buffers: i32,
    pub queue_submits: i32,
    pub pipelines_created: i32,
    pub vram_usage_bytes: u64,
    pub vram_budget_bytes: u64,
    pub software_gpu: bool,
    pub profiler_mode: ProfilerModeDto,
    pub clustered: bool,
    pub depth_prepass: bool,
    pub shadows: bool,
    pub ibl: bool,
    pub ssao: bool,
    pub contact_shadows: bool,
    pub ssgi: bool,
    /// The active view's dynamic-resolution factor (`(0, 1]`; `1.0` = native). The frame-budget
    /// controller lowers it below the `Low` tier floor to hold the budget; the present blit upscales.
    pub render_scale: f32,
    /// Whether the Global SDF reflection-occlusion cone is occluding the reflected skybox
    /// (specular only; indirect diffuse occlusion is DDGI ray-miss + contact GTAO).
    pub sky_occlusion: bool,
    /// The active render-quality tier (`low`/`medium`/`high`/`ultra`/`custom`) — the knob the
    /// `ssao`/`contact_shadows`/`ssgi` flags above derive from.
    pub quality: String,
    /// The active tonemap operator (`reinhard`/`aces`/`agx`/`pbr-neutral`).
    pub tonemap: String,
    /// The reactive loop is idling (skipping renders) — a static, converged, or hidden viewport.
    pub idle: bool,
    /// The temporal effects (TAA / SSGI history) have converged to their final image.
    pub converged: bool,
    /// The reasons continuous render is currently held (empty when idle), for the stats readout.
    pub redraw_reasons: Vec<String>,
    /// The editor viewport power state (`focused`/`unfocused`/`occluded`).
    pub power_state: String,
    pub ddgi: bool,
    /// Whether the Global Distance Field (the camera-centered cascade clipmap) backs the far-field
    /// cone-march tap.
    pub gdf: bool,
    pub rt_supported: bool,
    pub rt_shadows: bool,
    pub restir: bool,
    pub ssr: bool,
    pub rt_reflections: bool,
    pub blas_count: i32,
    pub pipelines: i32,
    pub bindless_textures: i32,
    pub bindless_free: i32,
    pub hdr: bool,
    pub exposure_ev: f32,
    pub aa: AaModeDto,
    pub view_mode: ViewModeDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderPassTimingDto {
    pub name: String,
    pub gpu_ms: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderPassTimingsDto {
    pub passes: Vec<RenderPassTimingDto>,
    pub gpu_total_ms: f32,
    pub software_gpu: bool,
    pub profiler_mode: ProfilerModeDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfilerSetModeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ProfilerModeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfilerModeResult {
    pub mode: ProfilerModeDto,
    pub timestamps_supported: bool,
    pub pipeline_stats_supported: bool,
    pub software_gpu: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PipelineStatsDto {
    pub input_vertices: u64,
    pub vertex_invocations: u64,
    pub clipping_invocations: u64,
    pub clipping_primitives: u64,
    pub fragment_invocations: u64,
    pub compute_invocations: u64,
    pub pixels: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileSpanDto {
    pub name: String,
    pub lane: ProfileLaneDto,
    pub start_ns: u64,
    pub end_ns: u64,
    pub parent_index: i32,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_stats: Option<PipelineStatsDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileCaptureMetadataDto {
    pub software_gpu: bool,
    pub correlated: bool,
    pub device_name: String,
    pub timestamp_period: f32,
    pub target_fps: f32,
    pub mode: ProfilerModeDto,
    pub filter: String,
    pub frame_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProfileCaptureDto {
    pub spans: Vec<ProfileSpanDto>,
    pub metadata: ProfileCaptureMetadataDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CaptureStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<CaptureModeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub include_cpu: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub include_pipeline_stats: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CaptureStartResult {
    pub capture_id: u32,
    pub ack: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CaptureStopResult {
    pub ready: bool,
    pub mode: CaptureModeDto,
    pub frame_count: u32,
    pub inlined: bool,
    pub capture: ProfileCaptureDto,
    pub chrome_trace: String,
    pub path: String,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CaptureStatusResult {
    pub state: CaptureStateDto,
    pub captured_frames: u32,
    pub target_frames: u32,
    pub mode: CaptureModeDto,
    pub pipeline_stats_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FrameSampleDto {
    pub frame_index: i64,
    pub cpu_ms: f32,
    pub gpu_ms: f32,
    pub cpu_wait_ms: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FrameHistoryParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FrameHistoryDto {
    pub p50_ms: f32,
    pub p95_ms: f32,
    pub p99_ms: f32,
    pub p999_ms: f32,
    pub max_ms: f32,
    pub mean_ms: f32,
    pub stddev_ms: f32,
    pub stutter_count: i64,
    pub sample_count: i32,
    pub budget_ms: f32,
    pub samples: Vec<FrameSampleDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PerfConfigDto {
    pub target_fps: f32,
    pub budget_ms: f32,
    pub green_budget_frac: f32,
    pub green_median_mul: f32,
    pub amber_median_mul: f32,
    pub frozen_ms: f32,
    pub vram_warn_frac: f32,
    pub vram_crit_frac: f32,
    /// Auto-quality: the frame-budget controller steps the render-quality tier to hold the budget.
    pub auto_quality: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetPerfConfigParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green_budget_frac: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green_median_mul: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amber_median_mul: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frozen_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_warn_frac: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_crit_frac: Option<f32>,
}

/// The temporal-upsampling (TAAU) render-scale + dynamic-resolution surface: the fixed input:display
/// ratio, the frame-budget-driven dynamic toggle + target, and the live input/display extents. The
/// single wire home of the render scale — a separate concern from [`SetTaaParamsParams`] (the blend /
/// sharpen look tunables).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpscaleDto {
    /// Fixed input:display render scale in `(0, 1]`; `1.0` = native (no upscaling).
    pub ratio: f32,
    /// Dynamic resolution: the frame-budget controller drives `ratio` toward `target_ms`.
    pub dynamic: bool,
    /// The per-frame budget (ms) the dynamic driver holds to (`= 1000 / target_fps`).
    pub target_ms: f32,
    /// Current input extent (scene / depth / motion render size) in device pixels.
    pub input_width: u32,
    pub input_height: u32,
    /// Fixed display extent the resolve reconstructs to (the present size), in device pixels.
    pub display_width: u32,
    pub display_height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetUpscaleResult {
    pub upscale: UpscaleDto,
}

/// Partial update of the upscale surface — every field `Option`, so an omitted field keeps its prior
/// value (`{ "ratio": 0.67 }` pins the ratio without touching `dynamic` / `target_ms`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetUpscaleParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_ms: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetUpscaleResult {
    pub upscale: UpscaleDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AlarmEventDto {
    pub seq: i64,
    pub fingerprint: String,
    pub metric: String,
    pub pass: String,
    pub severity: AlarmSeverityDto,
    pub state: AlarmStateDto,
    pub value: f32,
    pub threshold: f32,
    pub since_frame: i64,
    pub count: i32,
    pub duration_ms: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainAlarmsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainAlarmsResult {
    pub events: Vec<AlarmEventDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptStatusResult {
    pub state: String,
    pub instances: i32,
    pub error_high_water: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PhysicsStateResult {
    pub active: bool,
    pub body_count: i32,
    pub dynamic_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FitColliderParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FitColliderResult {
    pub entity: Uuid,
    pub shape: String,
    pub half_extents: Vec3,
    pub offset: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ContactEventDto {
    pub seq: i64,
    pub kind: String,
    pub entity_a: Uuid,
    pub entity_b: Uuid,
    pub sensor: bool,
    pub point: Vec3,
    pub normal: Vec3,
    pub tick: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainContactsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainContactsResult {
    pub events: Vec<ContactEventDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PhysicsBodyDto {
    pub entity: Uuid,
    pub motion: String,
    pub active: bool,
    pub position: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PhysicsBodiesResult {
    pub bodies: Vec<PhysicsBodyDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyImpulseParams {
    pub entity: EntitySelector,
    pub impulse: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyImpulseResult {
    pub velocity: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetKinematicBonesParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct KinematicBonesResult {
    pub entity: Uuid,
    pub enabled: bool,
    pub bone_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MoveCharacterParams {
    pub entity: EntitySelector,
    pub velocity: Vec3,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub jump: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MoveCharacterResult {
    pub position: Vec3,
    pub on_ground: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RaycastParams {
    pub origin: Vec3,
    pub dir: Vec3,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_dist: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ShapecastParams {
    pub origin: Vec3,
    pub dir: Vec3,
    pub radius: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_dist: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RaycastResult {
    pub hit: bool,
    pub entity: Uuid,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnableRagdollParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RagdollResult {
    pub present: bool,
    pub active: bool,
    pub body_weight: f32,
    pub bones: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRagdollParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_weight: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetRagdollParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptErrorDto {
    pub seq: i64,
    pub entity: Uuid,
    pub script: String,
    pub message: String,
    pub tick: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptErrorsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptErrorsResult {
    pub events: Vec<ScriptErrorDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptLogDto {
    pub seq: i64,
    pub entity: Uuid,
    pub message: String,
    pub epoch_ms: i64,
    pub tick: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptLogsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainScriptLogsResult {
    pub events: Vec<ScriptLogDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetScriptSchemaParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptFieldDto {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub default_value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetScriptSchemaResult {
    pub fields: Vec<ScriptFieldDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetScriptOverrideParams {
    pub entity: EntitySelector,
    pub slot: i32,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetScriptOverrideResult {
    pub script_path: String,
    pub overrides: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateScriptParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateScriptResult {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ActiveAlarmDto {
    pub fingerprint: String,
    pub metric: String,
    pub pass: String,
    pub severity: AlarmSeverityDto,
    pub value: f32,
    pub threshold: f32,
    pub since_frame: i64,
    pub count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ActiveAlarmsDto {
    pub alarms: Vec<ActiveAlarmDto>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAaParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<AaModeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAaResult {
    pub aa: AaModeDto,
}

/// The runtime TAA resolve tuning (mirrors `saffron_rendering::TaaParams`): the adaptive
/// feedback range, the velocity-rejection scale, the YCoCg variance-clip gamma, and the
/// RCAS sharpen strength.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TaaParamsDto {
    pub feedback_min: f32,
    pub feedback_max: f32,
    pub velocity_rejection: f32,
    pub clip_gamma: f32,
    pub sharpness: f32,
}

/// `get-taa-params` reply: the current TAA blend/sharpen parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetTaaParamsResult {
    pub params: TaaParamsDto,
}

/// `set-taa-params` request: a partial update — each present field overwrites, each omitted
/// field keeps its current value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTaaParamsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub velocity_rejection: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_gamma: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<f32>,
}

/// `set-taa-params` reply: the fully-resolved parameters after the merge (echo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTaaParamsResult {
    pub params: TaaParamsDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewModeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ViewModeDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewModeResult {
    pub view_mode: ViewModeDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToggleParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetClusteredResult {
    pub clustered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetIblResult {
    pub ibl: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkyOcclusionResult {
    pub sky_occlusion: bool,
}

/// Result of `set-gdf`: the resolved Global-Distance-Field enable state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGdfResult {
    pub gdf: bool,
}

/// Params for `set-render-quality`: the tier name (`low`/`medium`/`high`/`ultra`/`custom`) — the
/// single knob for the SSGI / GTAO / contact-shadow stack (replacing the per-effect toggles).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRenderQualityParams {
    pub tier: String,
}

/// Params for `set-tonemap`: the operator name (`reinhard`/`aces`/`agx`/`pbr-neutral`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTonemapParams {
    pub mode: String,
}

/// The applied tonemap operator, echoed by `set-tonemap`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TonemapResult {
    pub mode: String,
}

/// Params for `set-viewport-power-state`: the editor's window visibility
/// (`focused`/`unfocused`/`occluded`), so the host can suppress rendering a hidden viewport.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewportPowerStateParams {
    pub state: String,
}

/// The applied viewport power state, echoed by `set-viewport-power-state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewportPowerStateResult {
    pub state: String,
}

/// The active render-quality tier + the resolved per-effect state, returned by both
/// `set-render-quality` and `get-render-quality`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderQualityResult {
    /// The active tier name.
    pub tier: String,
    /// Whether screen-space one-bounce GI is on at this tier.
    pub ssgi: bool,
    /// Whether GTAO ambient occlusion is on at this tier.
    pub gtao: bool,
    /// Whether screen-space contact shadows are on at this tier.
    pub contact_shadows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRtShadowsResult {
    pub rt_shadows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRestirResult {
    pub restir: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSsrResult {
    pub ssr: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRtReflectionsResult {
    pub rt_reflections: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGiParams {
    pub mode: GiModeDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGiResult {
    pub ddgi: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetShadowsResult {
    pub shadows: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkinningResult {
    pub skinning: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetDisplacementResult {
    pub displacement: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetDepthPrepassResult {
    pub depth_prepass: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ViewportNativeInfoResult {
    pub platform: String,
    pub transport: String,
    pub status: String,
    pub control_socket: String,
    pub width: i32,
    pub height: i32,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewportSizeParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetViewportSizeResult {
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetActiveViewParams {
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetActiveViewResult {
    pub view: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectInfoDto {
    pub loaded: bool,
    pub root: String,
    pub path: String,
    pub name: String,
    pub display_name: String,
}

/// Coarse project-load lifecycle (mirrors `saffron_sceneedit::ProjectPhase`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProjectPhaseDto {
    Unloaded,
    Loading,
    Ready,
    Failed,
}

/// The current boot stage within `Loading` (mirrors `saffron_sceneedit::BootStage`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BootStageDto {
    Manifest,
    Catalog,
    Scene,
    Install,
    Assets,
    Skybox,
    Accel,
    Ready,
    Failed,
}

/// Project-load phase + progress. `total == 0` means indeterminate (spinner). `version` is
/// monotonic; the editor dedups on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProjectStatusDto {
    pub phase: ProjectPhaseDto,
    pub stage: BootStageDto,
    pub done: i32,
    pub total: i32,
    pub label: String,
    pub current_item: String,
    pub error: String,
    pub version: i64,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NewProjectParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PathParams {
    pub path: String,
}

/// The per-project asset-store enablement block: which connectors the project has
/// enabled. Non-secret only — credentials live in the OS keyring, never here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct ProjectStoresDto {
    /// Enabled connector ids (e.g. `["polyhaven", "poly-pizza"]`).
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct OptionalPathParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Where a store-imported asset came from and under what license, recorded on the
/// catalog entry so attribution travels with the asset (CC-BY / Sketchfab require it).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AssetAttributionDto {
    /// Canonical license id (`cc0`, `cc-by`, `cc-by-sa`, …).
    pub license_id: String,
    /// Whether the license requires visible attribution.
    pub requires_attribution: bool,
    /// Canonical license url.
    pub license_url: String,
    /// The asset author / creator.
    pub author: String,
    /// The asset's page on the source service.
    pub source_url: String,
    /// The connector the asset came from (`polyhaven`, `sketchfab`, …).
    pub store_id: String,
}

/// Parameters for `import-model`: a local file path plus optional store attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportModelParams {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AssetAttributionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportModelResult {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstantiateModelParams {
    pub asset: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AssetPlacementPhaseDto {
    Preview,
    Commit,
    #[default]
    Clear,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPlacementParams {
    pub phase: AssetPlacementPhaseDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlacementTransformDto {
    pub translation: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPlacementResult {
    pub active: bool,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<PlacementTransformDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExtractSubAssetParams {
    pub asset: AssetSelector,
    pub sub_asset: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ClearExtractionParams {
    pub asset: AssetSelector,
    pub sub_asset: Uuid,
}

/// Parameters for `import-texture`: a file path plus an optional colorspace hint
/// (`srgb` | `linear` | `hdr` | `auto`). `auto`/absent keeps the file-extension heuristic;
/// `linear` is for data maps (normal/roughness/metallic/AO) so they don't upload as sRGB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportTextureParams {
    pub path: String,
    /// Explicit upload colorspace override (`srgb`/`linear`/`hdr`); usually left unset and
    /// derived from `role`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colorspace: Option<String>,
    /// Semantic role hint (`albedo`/`normal`/`roughness`/…/`hdri`, or a connector map key like
    /// `nor_gl`/`arm`). Drives preview routing and, absent `colorspace`, the upload colorspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportTextureResult {
    pub texture: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetEntryDto {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: AssetTypeDto,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rigged: Option<bool>,
    /// Texture: how its bytes are interpreted on upload (`srgb`/`linear`/`hdr`/`auto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colorspace: Option<String>,
    /// Texture: its semantic role (`albedo`/`normal`/`roughness`/…/`hdri`), for preview routing.
    /// Omitted for a non-texture asset or an unrecognized (`unknown`) role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Creation time (seconds since the Unix epoch) of the asset's backing file, for sorting.
    pub created_at: i64,
    /// Store source/license, present for assets imported from a connector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AssetAttributionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetList {
    pub assets: Vec<AssetEntryDto>,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanAssetsResult {
    pub added: i32,
    pub removed: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReimportModelResult {
    pub updated: i32,
    pub added: i32,
    pub removed_from_source: i32,
    pub skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReimportModelParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelInfoParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelSubAssetDto {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelInfoResult {
    pub id: Uuid,
    pub name: String,
    pub source_path: String,
    pub source_hash: String,
    pub material_count: i32,
    pub has_skin: bool,
    pub node_count: i32,
    pub total_bytes: u64,
    pub sub_assets: Vec<ModelSubAssetDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetReferencesParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetReferencesResult {
    pub referenced_by: Vec<String>,
    pub references: Vec<String>,
    pub footprint: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanCandidateDto {
    pub id: Uuid,
    pub path: String,
    pub category: String,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanReport {
    pub candidates: Vec<CleanCandidateDto>,
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanAssetsParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteUnusedParams {
    pub ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteUnusedResult {
    pub deleted: i32,
    pub reclaimed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenameAssetParams {
    pub asset: AssetSelector,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetRef {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateAssetFolderParams {
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenameAssetFolderParams {
    pub folder: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteAssetFolderParams {
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MoveAssetParams {
    pub asset: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetUsagesParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetUsageDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_name: Option<String>,
    pub slot: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetUsagesResult {
    pub usages: Vec<AssetUsageDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetMetadataParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetMetadataDto {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: AssetTypeDto,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertex_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triangle_count: Option<u32>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteAssetParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteAssetResult {
    pub id: Uuid,
    pub name: String,
    pub cleared: Vec<AssetUsageDto>,
    pub file_deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssignAssetParams {
    pub entity: EntitySelector,
    pub slot: AssetSlotDto,
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCreateParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCreateResult {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialAssignParams {
    pub entity: EntitySelector,
    pub material: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialAssignResult {
    pub material: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialImportParams {
    pub path: String,
    pub name: String,
    /// Optional store attribution, recorded on the imported material's catalog entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AssetAttributionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialImportResultDto {
    pub id: Uuid,
    pub roles: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialRefDto {
    pub id: Uuid,
    pub name: String,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialListResult {
    pub materials: Vec<MaterialRefDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialGetParams {
    pub material: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialGetResult {
    pub id: Uuid,
    pub blend: String,
    pub unlit: bool,
    pub base_color: Vec4,
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: Vec3,
    pub emissive_strength: f32,
    pub height_scale: f32,
    /// The height-map technique: `bump` | `parallax` | `displacement`.
    pub height_mode: String,
    pub albedo_texture: Uuid,
    pub orm_texture: Uuid,
    pub normal_texture: Uuid,
    pub emissive_texture: Uuid,
    pub height_texture: Uuid,
    /// The tangent-space vector-displacement map (`0` = scalar-only along the normal).
    pub vector_displacement_texture: Uuid,
    pub graph: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSchemaParams {
    pub material: AssetSelector,
}

/// One exposed material parameter — its override key, type token, and default value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExposedParamDto {
    pub name: String,
    /// `scalar` | `color3` | `color4` | `vec2` | `bool` | `blend` | `texture`.
    pub kind: String,
    pub default: Value,
}

/// The exposed-parameter schema of a material — the keys a `MaterialSet` slot may override.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSchemaResult {
    pub params: Vec<ExposedParamDto>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialUpdateParams {
    pub material: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_color: Option<Vec4>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_scale: Option<f32>,
    /// The height-map technique: `bump` | `parallax` | `displacement`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albedo_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orm_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_texture: Option<Uuid>,
    /// The tangent-space vector-displacement map (`0` = scalar-only along the normal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_displacement_texture: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialUpdateResult {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PreviewRenderParams {
    pub material: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PreviewRenderResult {
    pub png: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetGraphParams {
    pub material: AssetSelector,
    pub graph: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetGraphResult {
    pub id: Uuid,
    pub foldable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCreateInstanceParams {
    pub parent: AssetSelector,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetOverrideParams {
    pub material: AssetSelector,
    pub field: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetOverrideResult {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCompileParams {
    pub material: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCompileResult {
    pub id: Uuid,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCookResult {
    pub compiled: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssignAssetResult {
    pub id: Uuid,
    pub name: String,
    pub slot: AssetSlotDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PathResult {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScreenshotParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ScreenshotTargetDto>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScreenshotResult {
    pub target: ScreenshotTargetDto,
    pub path: String,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailParams {
    pub asset: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailResult {
    pub id: Uuid,
    pub format: String,
    pub width: i32,
    pub height: i32,
    pub base64: String,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailCacheParams {
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailCacheResult {
    pub entries: i32,
    pub bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct QuitResult {
    pub quitting: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateEntityParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetParentParams {
    pub entity: EntitySelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntitySelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DestroyEntityResult {
    pub destroyed: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityListEntry {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityList {
    pub entities: Vec<EntityListEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ComponentList {
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ComponentParams {
    pub entity: EntitySelector,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddComponentResult {
    pub added: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoveComponentResult {
    pub removed: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentParams {
    pub entity: EntitySelector,
    pub component: String,
    pub json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentResult {
    pub set: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentOrderParams {
    pub entity: EntitySelector,
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentOrderResult {
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTransformParams {
    pub entity: EntitySelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<Vec3>,
    /// Animate the fields toward the given values over ~25ms instead of snapping
    /// (ignored when preserve-children must rebase the subtree on each write).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub smooth: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetLightParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntitySelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickResult {
    pub hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<PickKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InspectResult {
    pub id: Uuid,
    pub name: String,
    pub components: Value,
    pub component_order: Vec<String>,
}

/// The scene environment block on the wire — the bare environment object (its schema is
/// `$ref Environment`), not a wrapper. `#[serde(transparent)]` so the single
/// `value` field serializes as the object itself: `set-environment`/`get-environment`/
/// `set-atmosphere` reply with `{ skyMode, …, atmosphere: {…} }`, never `{ value: {…} }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
#[ts(export)]
pub struct EnvironmentDto {
    pub value: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetEnvironmentParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_rotation: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub visible: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub use_sky_for_ambient: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient_color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient_intensity: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAtmosphereParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planet_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atmosphere_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rayleigh_scattering: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rayleigh_scale_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mie_scattering: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mie_scale_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mie_anisotropy: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ozone_absorption: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sun_disk_angular_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sun_disk_intensity: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SelectionResult {
    pub selection_version: i32,
    pub scene_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityRef>,
    pub play_state: String,
    pub play_version: i32,
    pub animation_version: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlayStateResult {
    pub state: String,
    pub play_version: i32,
    pub scene_version: i32,
    pub has_primary_camera: bool,
    pub animation_version: i32,
    pub preview_asset: Uuid,
}

/// One animation channel (track) of a clip, carrying enough to draw a real per-channel
/// keyframe strip. The editor draws one strip per channel keyed on `times`, independent of
/// `width`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationChannelDto {
    /// `"node-translation" | "node-rotation" | "node-scale" | "morph-weights" | "bone"` — a
    /// plain wire string; serde/ts-rs handle it natively, no enum DTO row.
    pub kind: String,
    /// The display label: the resolved entity name for a node/bone binding (the raw glTF node
    /// name when the binding is unresolved — which doubles as the broken-binding signal), and
    /// the raw glTF target name for a morph-weights channel.
    pub label: String,
    /// The raw glTF binding key (node name, or morph target name) — durable, what the runtime
    /// binds on. Distinct from `label` so the editor can show the friendly name yet key on it.
    pub target_name: String,
    /// The keyframe sample times in seconds, ascending — the strip's tick positions.
    pub times: Vec<f32>,
    /// Value components per keyframe, so `values.len() == times.len() * width`: `3` for
    /// translation/scale, `4` for a rotation quaternion, `morph_count` for a morph channel.
    pub width: i32,
    /// The per-keyframe values, row-major `times.len() * width`. Translation/scale rows are
    /// `xyz`, rotation rows are quaternion `xyzw`, morph rows are the N weights.
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationClipDto {
    pub id: Uuid,
    pub name: String,
    pub duration: f32,
    /// One entry per track in the clip — the editor renders a keyframe strip per channel.
    pub channels: Vec<AnimationChannelDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BoneDto {
    pub index: i32,
    pub name: String,
    pub parent: i32,
    pub joint: bool,
}

/// What a model can do — a flat, additive capability struct read once when the asset editor
/// opens. A new capability appends a field; existing readers ignore unknown fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetCapabilitiesDto {
    pub mesh_count: i32,
    pub material_count: i32,
    pub node_count: i32,
    pub has_rig: bool,
    pub bone_count: i32,
    pub clip_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetAssetModelParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetModelResult {
    pub mesh: Uuid,
    pub name: String,
    pub capabilities: AssetCapabilitiesDto,
    pub bones: Vec<BoneDto>,
    pub clips: Vec<AnimationClipDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnterAssetPreviewParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BoneEntityDto {
    pub index: i32,
    pub entity: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPreviewResult {
    pub root_entity: Uuid,
    pub bones: Vec<BoneEntityDto>,
    pub target: Vec3,
    pub distance: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListClipsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListClipsResult {
    pub clips: Vec<AnimationClipDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlayAnimationParams {
    pub entity: EntitySelector,
    pub clip: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#loop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub paused: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SeekAnimationParams {
    pub entity: EntitySelector,
    pub time: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seek_blend: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAnimationLoopParams {
    pub entity: EntitySelector,
    pub wrap: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAnimationPlayingParams {
    pub entity: EntitySelector,
    #[serde(deserialize_with = "coerce::boolean")]
    pub playing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationStateParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationStateResult {
    pub clip: Uuid,
    pub clip_name: String,
    pub duration: f32,
    pub time: f32,
    pub playing: bool,
    pub wrap: String,
    pub speed: f32,
    pub animation_version: i32,
    /// The target's live morph weights (canonical 0..1) — always present, empty when the
    /// target has no morph mesh. The runtime override if a preview is live, else the durable
    /// component's rest weights.
    pub morph_weights: Vec<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkeletonOverlayParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub show: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub axes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joint_size: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SkeletonOverlayResult {
    pub show: bool,
    pub axes: bool,
    pub joint_size: f32,
    pub highlight_joint: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DebugOverlaysParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub bounds: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub scene_aabb: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub light_volumes: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub grid: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub colliders: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DebugOverlaysResult {
    pub bounds: bool,
    pub scene_aabb: bool,
    pub light_volumes: bool,
    pub grid: bool,
    pub colliders: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkeletonHighlightParams {
    pub joint: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickSkeletonJointParams {
    pub u: f32,
    pub v: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_px: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickSkeletonJointResult {
    pub found: bool,
    pub node_index: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAssetPreviewOptionsParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub floor: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPreviewOptionsResult {
    pub floor: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetFootIkParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_height: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetFootIkParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FootIkResult {
    pub enabled: bool,
    pub ground_height: f32,
    pub chains: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetMorphWeightsParams {
    pub entity: EntitySelector,
    /// The morph-target weights (canonical 0..1); the length must equal the target's morph
    /// count.
    pub weights: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetMorphWeightsParams {
    pub entity: EntitySelector,
}

/// The live morph weights + the durable target names, shared by `set-`/`get-morph-weights`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MorphWeightsResult {
    pub weights: Vec<f32>,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListClipBindingsParams {
    pub entity: EntitySelector,
    pub clip: AssetSelector,
}

/// A clip's channels resolved against a live entity forest — an unresolved channel surfaces
/// as a broken binding via its `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ClipBindingsResult {
    pub channels: Vec<AnimationChannelDto>,
}

/// An entity's composed world-space transform (the cached WorldTransformComponent), so a
/// caller can read a bone's world position — e.g. to verify foot IK plants on the ground.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorldTransformResult {
    pub translation: Vec3,
    pub scale: Vec3,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StepParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeselectResult {
    pub selection_version: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddEntityParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<AddEntityPreset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenameEntityParams {
    pub entity: EntitySelector,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentFieldParams {
    pub entity: EntitySelector,
    pub component: String,
    pub field: String,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentFieldResult {
    pub set: String,
    pub field: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EditorCamera {
    pub position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub fov: f32,
    pub near: f32,
    pub far: f32,
    pub move_speed: f32,
    pub look_speed: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetCameraParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fov: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub near: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub far: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub move_speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_speed: Option<f32>,
    /// The point the eye orbits (world space). Present with `distance` selects the orbit mode:
    /// the engine eases the pivot / distance / yaw / pitch toward the sample each rendered frame
    /// and derives the eye on the arc, so a fast preview drag sweeps the circle instead of
    /// cutting a chord across it. Absent, `set-camera` is a free-eye set that snaps to `position`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot: Option<Vec3>,
    /// The eye's distance from `pivot`. Present with `pivot` selects the eased orbit mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GizmoState {
    pub op: GizmoOpDto,
    pub space: GizmoSpaceDto,
    pub preserve_children: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetGizmoParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op: Option<GizmoOpDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space: Option<GizmoSpaceDto>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub preserve_children: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GizmoPointerParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<GizmoPointerPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GizmoPointerResult {
    pub hovered: String,
    pub dragging: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FlyInputParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_dx: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub look_dy: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub forward: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub back: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub left: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub right: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub up: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub down: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FlyInputResult {
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptInputParams {
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_buttons: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scroll: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScriptInputResult {
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetProbesParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetProbesResult {
    pub probes: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecaptureProbesResult {
    pub marked: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProbeRef {
    pub slot: u32,
    pub entity: Uuid,
    pub origin: Vec3,
    pub influence_radius: f32,
    pub intensity: f32,
    pub box_projection: bool,
    pub valid: bool,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListProbesResult {
    pub enabled: bool,
    pub count: u32,
    pub probes: Vec<ProbeRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetExposureParams {
    pub ev: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetExposureResult {
    pub exposure_ev: f32,
}

#[cfg(test)]
mod coerce_tests {
    use super::{SetAnimationPlayingParams, ToggleParams};

    #[test]
    fn toggle_enabled_coerces_number_string_and_bool() {
        // The bool-coercion contract: a bool param accepts a JSON bool, a number (`!= 0`),
        // or a string (anything but `"0"`/`"false"`/`"off"` is true). This is the form the
        // `sa` CLI / e2e harness send via positional `args` (`{ "enabled": 1 }`).
        let one: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": 1 })).unwrap();
        assert_eq!(one.enabled, Some(true));
        let zero: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": 0 })).unwrap();
        assert_eq!(zero.enabled, Some(false));
        let on: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": "on" })).unwrap();
        assert_eq!(on.enabled, Some(true));
        let off: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": "off" })).unwrap();
        assert_eq!(off.enabled, Some(false));
        let real: ToggleParams =
            serde_json::from_value(serde_json::json!({ "enabled": true })).unwrap();
        assert_eq!(real.enabled, Some(true));
        // Absent ⇒ None (the optional-field default), not an error.
        let absent: ToggleParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(absent.enabled, None);
    }

    #[test]
    fn required_bool_param_coerces_a_number() {
        let p: SetAnimationPlayingParams =
            serde_json::from_value(serde_json::json!({ "entity": "rig", "playing": 1 })).unwrap();
        assert!(p.playing);
    }
}
