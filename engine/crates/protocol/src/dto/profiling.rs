use super::coerce;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AlarmEventDto {
    pub seq: i64,
    pub fingerprint: String,
    pub metric: String,
    pub pass: String,
    /// What the breach belongs to; see [`ActiveAlarmDto::owner`].
    pub owner: String,
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
pub struct ActiveAlarmDto {
    pub fingerprint: String,
    pub metric: String,
    pub pass: String,
    /// What the breach belongs to — a vegetation cell and the family that filled it, or empty for
    /// a whole-frame alarm. This is what turns "a budget broke" into "this content broke it".
    pub owner: String,
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
