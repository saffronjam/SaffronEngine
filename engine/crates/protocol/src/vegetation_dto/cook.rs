use crate::{
    Uuid, VegetationBaseManifestDto, VegetationCandidateRejectionReasonDto, VegetationGuid,
    WorldBoundsDto, WorldCellDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Exact semantic versions that participate in every vegetation cook identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookVersionSetDto {
    pub schema: u32,
    pub compiler: u32,
    pub evaluator: u32,
    pub numeric: u32,
    pub simulation: u32,
}

/// Complete platform profile that can affect derived vegetation artifact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookPlatformProfileDto {
    pub target: String,
    pub content_profile: String,
    pub toolchain: String,
    pub features: Vec<String>,
    pub identity: String,
}

/// Predicted bounded work for one cook scope or output node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookWorkEstimateDto {
    pub work_units: String,
    pub peak_memory_bytes: String,
    pub input_bytes: String,
    pub output_bytes: String,
}

/// Measured execution and cache result for one cook node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookWorkActualDto {
    pub elapsed_micros: String,
    pub peak_memory_bytes: String,
    pub input_bytes: String,
    pub output_bytes: String,
    pub rejection_count: String,
    pub cache_hit: bool,
}

/// One typed rejection category accumulated by a cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookRejectionTotalDto {
    pub reason: VegetationCandidateRejectionReasonDto,
    pub count: String,
}

/// Measured work, cache behavior, and rejection totals for one cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookStatisticsDto {
    pub nodes: String,
    pub elapsed_micros: String,
    pub peak_memory_bytes: String,
    pub input_bytes: String,
    pub output_bytes: String,
    pub cache_hits: String,
    pub cache_misses: String,
    pub published_cells: String,
    pub rejections: Vec<VegetationCookRejectionTotalDto>,
}

/// Stable output address of one content-addressed vegetation cook node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationCookNodeAddressDto {
    Plant {
        family: Uuid,
    },
    GlobalStage {
        map: Uuid,
        biome_instance: VegetationGuid,
        stage: String,
        owner: WorldCellDto,
    },
    Cell {
        map: Uuid,
        cell: WorldCellDto,
    },
}

/// Exact output scope requested from the one vegetation cooker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationCookScopeDto {
    All,
    Bounds { bounds: WorldBoundsDto, level: u8 },
    Cells { cells: Vec<WorldCellDto> },
}

/// Starts one deterministic content-addressed vegetation cook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookParams {
    pub map: crate::AssetSelector,
    pub scope: VegetationCookScopeDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<u16>,
}

/// Params identifying one asynchronous vegetation cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookJobParams {
    pub job: String,
}

/// Lifecycle state of one asynchronous vegetation cook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationCookJobStateDto {
    Queued,
    Running,
    Completed,
    Cancelled,
    Superseded,
    Failed,
}

/// Monotonic progress of one asynchronous vegetation cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookProgressDto {
    pub completed_nodes: String,
    pub total_nodes: String,
    pub cache_hits: String,
    pub published_cells: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<VegetationCookNodeAddressDto>,
}

/// Handle returned when a vegetation cook starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookJobDto {
    pub job: String,
    pub state: VegetationCookJobStateDto,
    pub scope: VegetationCookScopeDto,
    pub progress: VegetationCookProgressDto,
}

/// Current state and optional terminal output of one vegetation cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookStatusDto {
    pub job: String,
    pub state: VegetationCookJobStateDto,
    pub progress: VegetationCookProgressDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statistics: Option<VegetationCookStatisticsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<VegetationBaseManifestDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::ControlFailureDto>,
}

/// Selects the current or one exact immutable base manifest for a vegetation map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestParams {
    pub map: crate::AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// Complete manifest plus observational statistics from its producing cook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestResult {
    pub manifest: VegetationBaseManifestDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_cook: Option<VegetationCookStatisticsDto>,
}

/// Selects one immutable `.svegcell` through a map manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCellInspectParams {
    pub map: crate::AssetSelector,
    pub cell: WorldCellDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
}

/// Exact known section vocabulary inside one `.svegcell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationCellSectionKindDto {
    MacroPoints,
    MicroFields,
    Provenance,
    RejectionDiagnostics,
    SurfaceAttachments,
    SurfaceDependencies,
    RenderReferences,
    RenderBounds,
    CollisionInputs,
    NavigationContributions,
    EcologyBoundary,
    EcologyCheckpoint,
}

/// Exact storage codec recorded in a vegetation artifact TOC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationArtifactSectionCodecDto {
    Raw,
    Zstd,
}

/// One validated random-access section descriptor from a `.svegcell` TOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCellSectionDto {
    pub kind: VegetationCellSectionKindDto,
    pub version: u32,
    pub codec: VegetationArtifactSectionCodecDto,
    pub alignment: u32,
    pub offset: String,
    pub stored_size: String,
    pub decoded_size: String,
    pub content_hash: String,
}

/// Complete validated header, TOC, and manifest metadata for one cooked cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCellSummaryDto {
    pub map: Uuid,
    pub manifest: String,
    pub cell: WorldCellDto,
    pub content_hash: String,
    pub cook_key: String,
    pub platform_profile: String,
    pub payload_hash: String,
    pub bounds: WorldBoundsDto,
    pub macro_points: String,
    pub micro_samples: String,
    pub sections: Vec<VegetationCellSectionDto>,
}

/// Result of inspecting one immutable cooked vegetation cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCellInspectResult {
    pub cell: VegetationCellSummaryDto,
}
