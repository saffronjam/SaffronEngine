use crate::{
    PlantId, Uuid, VegetationCandidateRejectionReasonDto, VegetationGuid, WorldBoundsDto,
    WorldCellDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The catalog scope used to compile a standalone biome or map-local instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "scope",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationCompileTargetDto {
    Asset {
        biome: Uuid,
    },
    Instance {
        map: Uuid,
        biome_instance: VegetationGuid,
    },
}

/// Params for compiling one biome graph through the catalog-backed resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCompileBiomeParams {
    pub target: VegetationCompileTargetDto,
}

/// Graph-local planning estimates independent of concrete region inputs and worker scheduling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphEstimateDto {
    /// Maximum candidates emitted by one symbolic graph scope.
    pub candidates: String,
    /// Maximum accepted points emitted by one symbolic graph scope.
    pub accepted: String,
    /// Maximum quantized micro samples emitted by one symbolic graph scope.
    pub micro_samples: String,
    /// Maximum graph-local live bytes; concrete job admission uses evaluation preflight.
    pub memory_bytes: String,
    /// Maximum graph-local execution-boundary transfer bytes.
    pub transfer_bytes: String,
}

/// Explicit evaluator hard caps; exceeding one aborts without reducing quality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphLimitsDto {
    pub workers: u16,
    pub output_cells: String,
    pub global_stage_tiles: String,
    pub input_tiles: String,
    pub candidates: String,
    pub macro_points: String,
    pub micro_samples: String,
    /// Maximum comprehensive evaluator-owned memory admitted for one concrete job.
    pub memory_bytes: String,
    pub transfer_bytes: String,
    pub module_depth: u16,
    pub time_ms: String,
}

/// One immutable dependency fingerprint in a compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphDependencyDto {
    pub kind: String,
    pub identity: String,
    pub content_hash: String,
}

/// Result of strict current-version biome compilation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCompileBiomeResult {
    pub biome: Uuid,
    pub biome_instance: Option<VegetationGuid>,
    pub graph_identity: String,
    pub required_halo_bits: i32,
    pub estimate: VegetationGraphEstimateDto,
    pub limits: VegetationGraphLimitsDto,
    pub dependencies: Vec<VegetationGraphDependencyDto>,
}

/// Every closed biome-graph operator available to authored graphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationGraphOperatorDto {
    InterfaceInput,
    RegionInput,
    SplineInput,
    SpeciesInput,
    CommunityInput,
    ExplicitAnchors,
    StratifiedCoverage,
    BlueNoisePoisson,
    SurfaceProjection,
    FieldSample,
    PaintedTile,
    Noise,
    Gradient,
    Curve,
    Remap,
    Combine,
    Clamp,
    DistanceField,
    WeightedElimination,
    VariableSpacing,
    FieldImportance,
    ClusterPatchColony,
    SplineFollow,
    RecursiveCompanion,
    Transform,
    PriorityExclusion,
    BoundsOverlap,
    Competition,
    Suitability,
    CommunityBlend,
    SuccessionInput,
    MacroOutput,
    MicroOutput,
    DiagnosticOutput,
    ModuleCall,
}

/// Optional operator filter for node-schema inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNodeSchemaParams {
    pub operator: Option<VegetationGraphOperatorDto>,
}

/// One typed graph pin schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphPinDto {
    pub name: String,
    pub domain: String,
    pub required: bool,
}

/// One typed graph parameter schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphParameterDto {
    pub name: String,
    pub kind: String,
    pub required: bool,
}

/// Complete authoring schema for one biome-graph operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNodeSchemaDto {
    pub operator: VegetationGraphOperatorDto,
    pub inputs: Vec<VegetationGraphPinDto>,
    pub outputs: Vec<VegetationGraphPinDto>,
    pub parameters: Vec<VegetationGraphParameterDto>,
    pub seed_namespaces: Vec<String>,
    pub slang_compute: bool,
}

/// Node schemas in stable operator order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNodeSchemaResult {
    pub nodes: Vec<VegetationNodeSchemaDto>,
}

/// Assembles, comprehensively bounds, and prepares one vegetation evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationPreflightRegionParams {
    pub map: Uuid,
    pub biome_instance: VegetationGuid,
    pub bounds: WorldBoundsDto,
    pub level: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ecology_tick: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<u16>,
}

/// Lifecycle state of one asynchronous evaluation job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationEvaluationJobStateDto {
    Prepared,
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// Admitted work and evaluator-controlled allocation for one evaluation job. Shared graph,
/// provider, and executor internals sit outside the boundary, as does allocator and thread
/// bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluationPreflightDto {
    /// Partitioned output cells admitted by the job.
    pub output_cells: String,
    /// Unique compiler-owned global-stage tiles admitted by the job.
    pub global_stage_tiles: String,
    /// Total admitted caller-supplied and evaluator-generated input tiles.
    pub input_tiles: String,
    /// Caller-supplied immutable input allocations retained by the exact job.
    pub retained_input_bytes: String,
    /// Canonical input allocations created during preparation before replay.
    pub generated_input_bytes: String,
    /// Conservative total candidate-stream peak across every evaluated scope.
    pub candidate_count: String,
    /// Conservative total accepted macro points.
    pub accepted_count: String,
    /// Exact total quantized micro samples.
    pub micro_samples: String,
    /// Peak evaluator-controlled application allocation during symbolic admission.
    pub preflight_peak_bytes: String,
    /// Peak controlled allocation during execution, including requested worker-stack sizes.
    pub execution_peak_bytes: String,
    /// Greater of `preflight_peak_bytes` and `execution_peak_bytes`.
    pub memory_bytes: String,
    /// Exact resident-program transfer bytes for the selected execution plan.
    pub transfer_bytes: String,
    /// Bounded cell workers participating in the job.
    pub worker_count: u16,
    /// Maximum wall-clock duration admitted for execution.
    pub time_limit_ms: String,
    /// Complete hard limits enforced by preflight and the matching evaluator run.
    pub limits: VegetationGraphLimitsDto,
}

/// Handle and admitted work returned for one vegetation evaluation job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluationJobDto {
    pub job: String,
    pub state: VegetationEvaluationJobStateDto,
    pub preflight: VegetationEvaluationPreflightDto,
}

/// Params identifying one asynchronous vegetation evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluationJobParams {
    pub job: String,
}

/// Runtime domain selected by the canonical graph execution plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationExecutionDomainDto {
    ReferenceCpu,
    ParallelCpu,
    SlangCompute,
}

/// Aggregated actual work for one fully qualified node in a completed evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNodeEvaluationDiagnosticDto {
    pub module_path: Vec<VegetationGuid>,
    pub node: VegetationGuid,
    pub operator: VegetationGraphOperatorDto,
    pub symbol: String,
    pub input_candidates: String,
    pub output_candidates: String,
    pub output_bytes: String,
    pub predicted_transfer_bytes: String,
    pub elapsed_micros: String,
    pub execution_domain: VegetationExecutionDomainDto,
}

/// Canonical fully qualified address of one graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphNodeAddressDto {
    pub module_path: Vec<VegetationGuid>,
    pub node: VegetationGuid,
}

/// Evaluation result that owns one retained diagnostic value or provenance handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationDiagnosticResultSourceDto {
    Cell { cell: WorldCellDto },
    GlobalStage { stage: String, owner: WorldCellDto },
}

/// Candidate population captured by one named diagnostic output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationDiagnosticStreamScopeDto {
    GlobalSnapshot,
    CandidateLineage { lineage: VegetationGuid },
}

/// Exact retained candidate value from one diagnostic output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationDiagnosticCandidateSampleDto {
    pub source: VegetationDiagnosticResultSourceDto,
    pub identity: VegetationCandidateIdentityDto,
    pub owner: WorldCellDto,
    pub position_ticks: [String; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<Uuid>,
    pub variation: u32,
    pub priority_bits: i32,
    pub ecology_tick: String,
}

/// Exact retained scalar value from one diagnostic output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationDiagnosticScalarSampleDto {
    pub source: VegetationDiagnosticResultSourceDto,
    pub candidate: VegetationCandidateIdentityDto,
    pub value_bits: i32,
}

/// Result-scoped identifier for one retained provenance record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationDiagnosticProvenanceIdDto {
    pub source: VegetationDiagnosticResultSourceDto,
    pub handle: u32,
}

/// One rejected candidate captured by a named diagnostic output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationDiagnosticRejectionDto {
    pub candidate: VegetationCandidateIdentityDto,
    pub reason: VegetationCandidateRejectionReasonDto,
    pub provenance: VegetationDiagnosticProvenanceIdDto,
}

/// One canonical user-named stream retained from graph evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNamedDiagnosticStreamDto {
    pub node: VegetationGraphNodeAddressDto,
    pub label: String,
    pub scope: VegetationDiagnosticStreamScopeDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_samples: Option<Vec<VegetationDiagnosticCandidateSampleDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scalar_samples: Option<Vec<VegetationDiagnosticScalarSampleDto>>,
    pub rejected: Vec<VegetationDiagnosticRejectionDto>,
}

/// Aggregated actual work for one resident GPU execution group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGpuGroupEvaluationDiagnosticDto {
    pub nodes: Vec<VegetationGraphNodeAddressDto>,
    pub invocation_count: String,
    pub output_bytes: String,
    pub transfer_bytes: String,
    pub elapsed_micros: String,
}

/// Deterministic aggregate returned after a complete evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluationSummaryDto {
    pub cells: String,
    pub global_stages: String,
    pub global_resident_bytes: String,
    pub candidates: String,
    pub accepted: String,
    pub micro_tiles: String,
    pub rejected: String,
    pub canonical_hash: String,
    pub nodes: Vec<VegetationNodeEvaluationDiagnosticDto>,
    pub gpu_groups: Vec<VegetationGpuGroupEvaluationDiagnosticDto>,
    pub streams: Vec<VegetationNamedDiagnosticStreamDto>,
}

/// Current state and optional terminal result of one evaluation job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluationStatusDto {
    pub job: String,
    pub state: VegetationEvaluationJobStateDto,
    pub preflight: VegetationEvaluationPreflightDto,
    pub summary: Option<VegetationEvaluationSummaryDto>,
    pub error: Option<crate::ControlFailureDto>,
}

/// Complete pre-acceptance identity used to select a rejected candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCandidateIdentityDto {
    pub node: VegetationGuid,
    pub node_address: VegetationGuid,
    pub node_semantic_revision: u32,
    pub ordinal: String,
    pub ancestor: String,
}

/// Accepted or rejected evaluation subject to explain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationExplainSubjectDto {
    Plant {
        plant: PlantId,
    },
    Rejected {
        candidate: VegetationCandidateIdentityDto,
    },
}

/// Params for explaining one point from a completed evaluation job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationExplainPointParams {
    pub job: String,
    pub cell: WorldCellDto,
    pub subject: VegetationExplainSubjectDto,
}
