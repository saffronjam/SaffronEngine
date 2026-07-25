//! Generated-wire source types for vegetation assets, points, layers, manifests, and mutations.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::{AssetTypeDto, Uuid};

fn is_canonical_guid(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// An opaque canonical plant identity encoded as 32 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct PlantId(pub String);

impl<'de> Deserialize<'de> for PlantId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if is_canonical_guid(&value) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "plant ID must be 32 lowercase hexadecimal digits",
            ))
        }
    }
}

impl JsonSchema for PlantId {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("PlantId")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{32}$",
            "minLength": 32,
            "maxLength": 32
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// A stable 128-bit authored GUID encoded as a lowercase 32-digit hexadecimal string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, TS)]
#[serde(transparent)]
#[ts(export, type = "string")]
pub struct VegetationGuid(pub String);

impl<'de> Deserialize<'de> for VegetationGuid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if is_canonical_guid(&value) {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "vegetation GUID must be 32 lowercase hexadecimal digits",
            ))
        }
    }
}

impl JsonSchema for VegetationGuid {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("VegetationGuid")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{32}$",
            "minLength": 32,
            "maxLength": 32
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

/// One canonical hierarchical world-cell key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorldCellDto {
    /// Signed coordinates are strings so the complete i64 range survives JavaScript.
    pub coordinates: [String; 3],
    pub level: u8,
}

/// One exact half-open world bound in global quantized ticks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WorldBoundsDto {
    pub min_ticks: [String; 3],
    pub max_ticks_exclusive: [String; 3],
}

/// Canonical biological lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantLifecycleDto {
    Seed,
    Sprout,
    Juvenile,
    Mature,
    Senescent,
    Dead,
    Stump,
    Removed,
}

/// Default gameplay interaction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum InteractionPolicyDto {
    Decorative,
    Interactive,
    Harvestable,
    Structural,
}

/// Shared authoritative field-channel kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FieldChannelKindDto {
    Altitude,
    Slope,
    Curvature,
    Concavity,
    Drainage,
    Moisture,
    Temperature,
    Precipitation,
    Sunlight,
    Exposure,
    WaterDistance,
    WaterDepth,
    SignedBlocker,
    SplineDistance,
    User,
}

/// Shared authoritative field channel, including the registered user namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FieldChannelDto {
    pub kind: FieldChannelKindDto,
    pub user: Option<String>,
}

/// Stable surface attachment carried by a plant point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SurfaceAttachmentDto {
    pub provider: String,
    pub primitive: String,
    pub barycentric: [u16; 3],
    pub revision: String,
}

/// One row in the schema-hashed canonical vegetation point vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantPointDto {
    pub id: PlantId,
    pub owner: WorldCellDto,
    pub local_position: [u32; 3],
    pub orientation: [i16; 4],
    pub scale_bits: [i32; 3],
    pub bounds: WorldBoundsDto,
    pub family: Uuid,
    pub variation: u32,
    pub lifecycle: PlantLifecycleDto,
    pub phenotype: u32,
    pub representation_class: u32,
    pub deterministic_key: VegetationGuid,
    pub candidate: String,
    pub parent: Option<PlantId>,
    pub colony: Option<PlantId>,
    pub ecology_tick: String,
    pub health: u16,
    pub moisture: u16,
    pub fuel: u16,
    pub phenology: u16,
    pub flags: u32,
    pub interaction_policy: InteractionPolicyDto,
    pub provenance: u32,
    pub attachment: Option<SurfaceAttachmentDto>,
    pub surface_projection_bits: [i32; 3],
}

/// Expanded lineage behind a point's compact provenance handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProvenanceDto {
    pub map: Uuid,
    pub layer: VegetationGuid,
    pub biome: Uuid,
    pub decision: u32,
    pub candidate: String,
    pub family: Option<Uuid>,
    pub plant: Option<PlantId>,
    pub variation: u32,
}

/// Stable outcome of one candidate decision in the shared provenance DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProvenanceDecisionOutcomeDto {
    Produced,
    Retained,
    Accepted,
    Rejected,
}

/// One node in an accepted or rejected candidate's provenance decision DAG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProvenanceDecisionDto {
    pub handle: u32,
    pub parents: Vec<u32>,
    pub subgraph_path: Vec<VegetationGuid>,
    pub node: VegetationGuid,
    pub operator: String,
    pub candidate: String,
    pub outcome: ProvenanceDecisionOutcomeDto,
}

/// Closed reason an authoritative candidate was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationCandidateRejectionReasonDto {
    SurfaceMiss,
    Threshold,
    WeightedElimination,
    PriorityExclusion,
    Competition,
    ForeignOwner,
    NoSpecies,
}

/// Complete expanded provenance for one accepted plant or rejected candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ProvenanceExplanationDto {
    pub handle: u32,
    pub record: ProvenanceDto,
    pub decisions: Vec<ProvenanceDecisionDto>,
    pub rejection_reason: Option<VegetationCandidateRejectionReasonDto>,
}

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

/// Complete admitted work and evaluator-controlled allocation contract for one evaluation job.
///
/// Shared graph, provider, and executor internals plus allocator over-allocation, guard pages, and
/// platform, libstd, and kernel thread bookkeeping are outside this boundary.
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

/// Two-sided normal behavior for a thin foliage sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ThinSheetNormalBehaviorDto {
    Preserve,
    FaceForwardBack,
    Symmetric,
}

/// Canonical alpha/coverage source shared by raster, voxel, and ray tracing derivations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[ts(export)]
pub enum CoverageSourceDto {
    AlbedoAlpha,
    Texture { texture: Uuid },
    ModeledGeometry,
}

/// Conservative classification of the canonical coverage source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AlphaClassificationDto {
    Opaque,
    Masked,
    Transmissive,
}

/// Coverage-preserving mip derivation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CoverageMipMetadataDto {
    pub reference_cutoff: u16,
    pub source_extent: [u32; 2],
    pub spatial_hash_salt: String,
    pub classification: AlphaClassificationDto,
    pub mip_hashes: Vec<String>,
}

/// Aggregate voxel material moments derived from canonical coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VoxelMaterialMomentsDto {
    pub occupancy: u16,
    pub albedo_mean_bits: [i32; 3],
    pub roughness_mean: u16,
    pub transmission_mean_bits: [i32; 3],
    pub thickness_mean_bits: i32,
    pub normal_second_moments_bits: [i32; 6],
}

/// Optional opacity-micromap derivation metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct OpacityMicromapDerivationDto {
    pub enabled: bool,
    pub max_subdivision: u8,
    pub transparent_threshold: u16,
    pub opaque_threshold: u16,
}

/// Complete energy-conserving thin-sheet foliage response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ThinSheetFoliageParametersDto {
    pub front_albedo_response: u16,
    pub back_albedo_response: u16,
    pub thickness_bits: i32,
    pub absorption_color_bits: [i32; 3],
    pub transmission_color_bits: [i32; 3],
    pub roughness: u16,
    pub normal_behavior: ThinSheetNormalBehaviorDto,
    pub coverage_source: CoverageSourceDto,
    pub coverage: CoverageMipMetadataDto,
    pub voxel_moments: VoxelMaterialMomentsDto,
    pub opacity_micromap: OpacityMicromapDerivationDto,
    pub energy_limit: u16,
}

/// Exactly one material surface family and its complete typed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "model",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum MaterialSurfaceDto {
    Standard,
    ThinSheetFoliage {
        parameters: ThinSheetFoliageParametersDto,
    },
}

/// Plant-family source kind shown in catalog summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantSourceKindDto {
    Imported,
    Native,
}

/// Severity of one authored-asset or derived-artifact validation diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationValidationSeverityDto {
    Info,
    Warning,
    Error,
}

/// One stable machine-readable validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationValidationIssueDto {
    pub severity: VegetationValidationSeverityDto,
    pub code: String,
    pub path: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_selector: Option<String>,
}

/// Complete validation state for one vegetation asset or artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationValidationSummaryDto {
    pub valid: bool,
    pub issues: Vec<VegetationValidationIssueDto>,
}

/// Exact licensing and attribution retained from an imported source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationSourceProvenanceDto {
    pub source: String,
    pub source_uri: String,
    pub license_id: String,
    pub license_uri: String,
    pub author: String,
    pub attribution: String,
    pub requires_attribution: bool,
}

/// Durable location of one imported plant-family source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSourceLocatorDto {
    Asset { asset: Uuid },
    File { uri: String },
}

/// Semantic contribution supplied by one imported plant-family source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantSourceRoleDto {
    Geometry,
    Material,
    Skeleton,
    Collision,
    Navigation,
}

/// Stable selection within one imported source snapshot.
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSourceSelectorDto {
    #[default]
    Whole,
    Element {
        id: VegetationGuid,
        path: String,
    },
    Submesh {
        element: VegetationGuid,
        index: u32,
    },
}

/// Authored semantic destination of one stable imported-source selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSemanticDestinationDto {
    Part { id: VegetationGuid },
    Spine { id: VegetationGuid },
    MaterialSlot { slot: u32 },
    CollisionProxy { id: VegetationGuid },
    NavigationProxy { id: VegetationGuid },
    Phenotype { id: u32 },
}

/// Stable diagnostic category emitted by the single plant compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantCompileDiagnosticCodeDto {
    MissingSource,
    DuplicateSource,
    EmptySelection,
    InvalidGeometry,
    MissingMaterial,
    InvalidMaterial,
    InvalidSkeleton,
    MissingCoverageUv,
    InvalidLeafOrientation,
    BoundsMismatch,
    LimitExceeded,
    SourceChanged,
    OrphanedEdit,
}

/// One exact source-normalization diagnostic from the plant compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCompileDiagnosticDto {
    pub severity: VegetationValidationSeverityDto,
    pub code: PlantCompileDiagnosticCodeDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<VegetationGuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_selector: Option<PlantSourceSelectorDto>,
    pub path: String,
    pub message: String,
}

/// Why one manual semantic target cannot survive plant reimport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantReimportConflictReasonDto {
    MissingSource,
    MissingElement,
}

/// One source identity update observed by the plant compiler.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantSourceHashUpdateDto {
    pub source: VegetationGuid,
    pub previous: String,
    pub current: String,
}

/// Exact deterministic counts produced by plant source normalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCompileStatisticsDto {
    pub sources: String,
    pub meshes: String,
    pub vertices: String,
    pub indices: String,
    pub joints: String,
    pub materials: String,
    pub rejected: String,
}

/// Source coordinate units.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceUnitsDto {
    #[default]
    Meters,
    Centimeters,
    Millimeters,
    Feet,
}

/// A signed coordinate axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceAxisDto {
    PositiveX,
    NegativeX,
    #[default]
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

/// Source coordinate-system handedness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceHandednessDto {
    #[default]
    Right,
    Left,
}

/// Source front-face winding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceWindingDto {
    #[default]
    CounterClockwise,
    Clockwise,
}

/// Source UV vertical origin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SourceUvOriginDto {
    #[default]
    TopLeft,
    BottomLeft,
}

/// Tangent-frame normalization policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PlantTangentPolicyDto {
    Require,
    #[default]
    GenerateMissing,
    Regenerate,
}

/// Family origin policy after coordinate normalization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum PlantPivotDto {
    #[default]
    SourceOrigin,
    BoundsBaseCenter,
    Explicit {
        /// Position in source metres, as Q15.16 bits.
        position_bits: [i32; 3],
    },
    SemanticPart {
        /// The part identity the origin follows.
        part: VegetationGuid,
    },
}

/// How one external source is read into a plant family.
///
/// Every field defaults, so a caller states only what differs from metres, Y-up, right-handed,
/// counter-clockwise geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
#[ts(export)]
pub struct PlantImportSettingsDto {
    pub units: SourceUnitsDto,
    pub up_axis: SourceAxisDto,
    pub forward_axis: SourceAxisDto,
    pub handedness: SourceHandednessDto,
    /// Uniform post-unit scale, as Q15.16 bits, where 65536 is unchanged.
    pub scale_bits: i32,
    pub pivot: PlantPivotDto,
    pub winding: SourceWindingDto,
    pub uv_origin: SourceUvOriginDto,
    /// UV scale, as Q15.16 bits.
    pub uv_scale_bits: [i32; 2],
    /// UV offset applied after scaling, as Q15.16 bits.
    pub uv_offset_bits: [i32; 2],
    pub tangent_policy: PlantTangentPolicyDto,
}

impl Default for PlantImportSettingsDto {
    fn default() -> Self {
        Self {
            units: SourceUnitsDto::default(),
            up_axis: SourceAxisDto::PositiveY,
            forward_axis: SourceAxisDto::PositiveZ,
            handedness: SourceHandednessDto::default(),
            scale_bits: 1 << 16,
            pivot: PlantPivotDto::default(),
            winding: SourceWindingDto::default(),
            uv_origin: SourceUvOriginDto::default(),
            uv_scale_bits: [1 << 16, 1 << 16],
            uv_offset_bits: [0, 0],
            tangent_policy: PlantTangentPolicyDto::default(),
        }
    }
}

/// One external hero mesh a native family may graft over a generated element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGraftSourceDto {
    /// Source identity a graft edit names, as 32 hex digits.
    pub id: VegetationGuid,
    pub locator: PlantSourceLocatorDto,
    /// Which of the source's elements the graft takes.
    #[serde(default)]
    pub selector: PlantSourceSelectorDto,
    #[serde(default)]
    pub settings: PlantImportSettingsDto,
    pub provenance: VegetationSourceProvenanceDto,
}

/// One exact source snapshot read by plant validation and recooking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantSourceReferenceDto {
    pub id: VegetationGuid,
    pub locator: PlantSourceLocatorDto,
    pub role: PlantSourceRoleDto,
    pub selector: PlantSourceSelectorDto,
    pub content_hash: String,
    pub provenance: VegetationSourceProvenanceDto,
}

/// Catalog/editor summary for one `.splant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantAssetSummaryDto {
    pub id: Uuid,
    pub name: String,
    pub version: u32,
    pub source: PlantSourceKindDto,
    pub part_count: u32,
    pub phenotype_count: u32,
    pub material_slots: Vec<Uuid>,
    pub validation: VegetationValidationSummaryDto,
    pub provenance: Vec<VegetationSourceProvenanceDto>,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_cook: Option<VegetationCookStatisticsDto>,
}

/// Biome graph role shown in catalog summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BiomeRoleDto {
    Root,
    Module,
}

/// Catalog/editor summary for one `.sbiome`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BiomeAssetSummaryDto {
    pub id: Uuid,
    pub name: String,
    pub version: u32,
    pub role: BiomeRoleDto,
    pub plant_palette: Vec<Uuid>,
    pub modules: Vec<Uuid>,
    pub parameter_count: u32,
    /// The authored biome graph document (the versioned graph JSON the compiler reads).
    #[schemars(with = "std::collections::BTreeMap<String, Value>")]
    #[ts(type = "Record<string, unknown>")]
    pub graph: Value,
    pub validation: VegetationValidationSummaryDto,
    pub provenance: Vec<VegetationSourceProvenanceDto>,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_cook: Option<VegetationCookStatisticsDto>,
}

/// One local biome-graph instance bound into an authored map: the stable instance
/// identity (the address preflight/evaluation commands take) plus the referenced
/// root biome asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationBiomeInstanceRefDto {
    pub instance: VegetationGuid,
    pub biome: Uuid,
}

/// Catalog/editor summary for one `.svegmap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapSummaryDto {
    pub id: Uuid,
    pub name: String,
    pub version: u32,
    /// The root generation optimistic layer commits are based on.
    pub generation: String,
    pub bounds: WorldBoundsDto,
    pub layer_count: u32,
    pub biome_instances: Vec<VegetationBiomeInstanceRefDto>,
    pub chunk_level: u8,
    /// Layers whose authored chunks differ from what the current manifest consumed
    /// (every layer when no manifest exists) — a recook would change the output.
    pub dirty_layers: Vec<VegetationGuid>,
    pub validation: VegetationValidationSummaryDto,
    pub provenance: Vec<VegetationSourceProvenanceDto>,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_cook: Option<VegetationCookStatisticsDto>,
}

/// Summary of any vegetation-authored catalog asset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", content = "asset", rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationAssetSummaryDto {
    Plant(PlantAssetSummaryDto),
    Biome(BiomeAssetSummaryDto),
    VegetationMap(VegetationMapSummaryDto),
}

/// Coordinate space of one ordered map layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum LayerCoordinateSpaceDto {
    World,
    Surface,
    OwnerLocal,
}

/// Quantized field blending operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FieldBlendOperatorDto {
    Replace,
    Add,
    Multiply,
    Minimum,
    Maximum,
}

/// Inclusion semantics for masks and analytic shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum InclusionOperatorDto {
    Include,
    Exclude,
}

/// One canonical plant-family weight in a species layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SpeciesWeightDto {
    pub family: Uuid,
    pub weight: u16,
}

/// One complete authored transform override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantTransformOverrideDto {
    pub plant: PlantId,
    pub global_ticks: [String; 3],
    pub scale_bits: [i32; 3],
}

/// One complete authored persistent plant-state override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantStateOverrideDto {
    pub plant: PlantId,
    pub health: Option<u16>,
    pub moisture: Option<u16>,
    pub fuel: Option<u16>,
    pub interaction_policy: Option<InteractionPolicyDto>,
}

/// Every operation in the single vegetation-map layer algebra.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationLayerOperatorDto {
    ScalarField {
        channel: FieldChannelDto,
        tile_set: VegetationGuid,
        blend: FieldBlendOperatorDto,
        weight: u16,
    },
    VectorField {
        channel: FieldChannelDto,
        tile_set: VegetationGuid,
        value_bits: [i32; 3],
        blend: FieldBlendOperatorDto,
    },
    SpeciesWeights {
        weights: Vec<SpeciesWeightDto>,
    },
    Density {
        channel: FieldChannelDto,
        tile_set: VegetationGuid,
        blend: FieldBlendOperatorDto,
        weight: u16,
    },
    Mask {
        tile_set: VegetationGuid,
        operation: InclusionOperatorDto,
    },
    Volume {
        bounds: WorldBoundsDto,
        operation: InclusionOperatorDto,
        falloff_bits: i32,
    },
    Spline {
        spline: VegetationGuid,
        points: Vec<[String; 3]>,
        radius_bits: i32,
        operation: InclusionOperatorDto,
    },
    Anchors {
        plants: Vec<PlantId>,
    },
    Pins {
        plants: Vec<PlantId>,
    },
    TransformOverrides {
        overrides: Vec<PlantTransformOverrideDto>,
    },
    StateOverrides {
        overrides: Vec<PlantStateOverrideDto>,
    },
    Blocker {
        tile_set: VegetationGuid,
        categories: u32,
    },
}

/// One stable ordered vegetation-map layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationLayerDto {
    pub id: VegetationGuid,
    pub name: String,
    pub coordinate_space: LayerCoordinateSpaceDto,
    pub bounds: WorldBoundsDto,
    pub operator: VegetationLayerOperatorDto,
    pub dependencies: Vec<VegetationGuid>,
    pub order: i32,
    pub locked: bool,
    pub muted: bool,
    pub revision: String,
}

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

/// Typed payload vocabulary for one immutable authored vegetation-map object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationMapChunkKindDto {
    Field,
    AnchorOverride,
    GraphInstance,
    LayerMetadata,
    EditorMetadata,
}

/// Global or spatial tile address for one immutable authored vegetation-map object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationMapTileKeyDto {
    Global,
    Cell { cell: WorldCellDto },
}

/// Stable logical key resolved through a vegetation map's immutable object inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkKeyDto {
    pub layer: VegetationGuid,
    pub tile: VegetationMapTileKeyDto,
    pub kind: VegetationMapChunkKindDto,
}

/// Stable address of one exact input read by the vegetation cook graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationManifestDependencyAddressDto {
    SourceAsset {
        asset: Uuid,
    },
    SourceFile {
        uri: String,
    },
    MaterialCoverage {
        material: Uuid,
    },
    BiomeIr {
        map: Uuid,
        instance: VegetationGuid,
    },
    MapManifest {
        map: Uuid,
    },
    MapObject {
        map: Uuid,
        key: VegetationMapChunkKeyDto,
    },
    SurfaceProvider {
        provider: String,
        revision: String,
    },
    SurfaceTile {
        provider: String,
        revision: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<FieldChannelDto>,
        bounds: WorldBoundsDto,
    },
    Contract {
        namespace: String,
    },
    Node {
        node: VegetationCookNodeAddressDto,
    },
}

/// One exact immutable dependency in a vegetation base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestDependencyDto {
    pub address: VegetationManifestDependencyAddressDto,
    pub content_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<WorldBoundsDto>,
    pub halo_bits: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ancestor_level: Option<u8>,
}

/// One authoritative seed namespace bound into the world manifest identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationSeedNamespaceDto {
    pub name: String,
    pub namespace: VegetationGuid,
}

/// Packed element shape of one canonical vegetation point column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationPointColumnTypeDto {
    Id128,
    WorldCell,
    Orientation,
    FixedVec3,
    WorldBounds,
    AssetUuid,
    U32,
    U64,
    OptionalId128,
    Unit,
    SurfaceProjection,
    OptionalSurfaceAttachment,
    WorldPosition,
}

/// One exact point column pinned into the immutable manifest identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestPointColumnDto {
    pub id: u32,
    pub name: String,
    pub element_type: VegetationPointColumnTypeDto,
}

/// One compiled plant family addressable by cells in the base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestPlantDto {
    pub family: Uuid,
    pub tags: Vec<String>,
    pub source_hash: String,
    pub artifact_hash: String,
    pub local_bounds_min_bits: [i32; 3],
    pub local_bounds_max_bits: [i32; 3],
    pub variation_count: u32,
    pub phenotype_count: u32,
}

/// Spatial reason that one immutable cell reads another cell artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationManifestCellDependencyRoleDto {
    Neighbour,
    Halo,
    Ancestor,
    GlobalStage,
}

/// One exact inter-cell dependency in the immutable manifest directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestCellDependencyDto {
    pub cell: WorldCellDto,
    pub content_hash: String,
    pub role: VegetationManifestCellDependencyRoleDto,
    pub halo_bits: i32,
}

/// Per-family accepted macro-point count for one cooked cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationSpeciesCountDto {
    pub family: Uuid,
    pub macro_count: String,
    pub micro_count: String,
}

/// One independently resident section recorded in the manifest cell directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestCellSectionDto {
    pub kind: VegetationCellSectionKindDto,
    pub version: u32,
    pub codec: VegetationArtifactSectionCodecDto,
    pub alignment: u32,
    pub stored_size: String,
    pub decoded_size: String,
    pub content_hash: String,
}

/// One immutable cell entry in a complete vegetation base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestCellDto {
    pub cell: WorldCellDto,
    pub bounds: WorldBoundsDto,
    pub artifact_hash: String,
    pub payload_hash: String,
    pub dependencies: Vec<VegetationManifestCellDependencyDto>,
    pub species_counts: Vec<VegetationSpeciesCountDto>,
    pub macro_count: String,
    pub micro_count: String,
    pub resident_memory_bytes: String,
    pub stored_bytes: String,
    pub estimate: VegetationCookWorkEstimateDto,
    pub actual: VegetationCookWorkActualDto,
    pub sections: Vec<VegetationManifestCellSectionDto>,
}

/// Immutable identity binding every exact input, schema, seed, plant, and cooked base cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationBaseManifestDto {
    pub version: u32,
    pub world: Uuid,
    pub map: Uuid,
    pub map_hash: String,
    pub versions: VegetationCookVersionSetDto,
    pub platform: VegetationCookPlatformProfileDto,
    pub cook_graph_hash: String,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    pub seed_namespaces: Vec<VegetationSeedNamespaceDto>,
    pub point_schema_hash: String,
    pub point_columns: Vec<VegetationManifestPointColumnDto>,
    pub plants: Vec<VegetationManifestPlantDto>,
    pub cells: Vec<VegetationManifestCellDto>,
    pub identity: String,
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

/// Closed macro-plant filters shared by every runtime vegetation query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeQueryFilterDto {
    pub families: Vec<Uuid>,
    /// Decimal stable plant-tag identities; every listed tag must match.
    pub required_tags: Vec<String>,
    pub lifecycles: Vec<PlantLifecycleDto>,
    pub interaction_policies: Vec<InteractionPolicyDto>,
}

/// One exact runtime vegetation spatial query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum VegetationRuntimeQueryDto {
    Bounds {
        bounds: WorldBoundsDto,
    },
    Radius {
        center_ticks: [String; 3],
        radius_m: f64,
    },
    Ray {
        origin_ticks: [String; 3],
        direction: [f64; 3],
        max_distance_m: f64,
    },
    Nearest {
        position_ticks: [String; 3],
        #[serde(skip_serializing_if = "Option::is_none")]
        max_distance_m: Option<f64>,
    },
}

/// Parameters for querying CPU-resident authoritative macro vegetation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeQueryParams {
    pub query: VegetationRuntimeQueryDto,
    #[serde(default)]
    pub filter: VegetationRuntimeQueryFilterDto,
    /// Optional result cap; the response reports whether matching rows were truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One immutable effective plant row returned by the runtime authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimePlantDto {
    pub plant: PlantId,
    pub cell: WorldCellDto,
    pub generation: String,
    /// Monotonic biological tick.
    pub ecology_tick: String,
    pub position_ticks: [String; 3],
    pub orientation: [i16; 4],
    pub scale_bits: [i32; 3],
    pub bounds: WorldBoundsDto,
    pub family: Uuid,
    /// Decimal stable plant-tag identities.
    pub tags: Vec<String>,
    pub lifecycle: PlantLifecycleDto,
    pub phenotype: u32,
    /// The phenotype the renderer resolves from typed lifecycle + season.
    pub rendered_phenotype: u32,
    pub interaction_policy: InteractionPolicyDto,
    pub health: u16,
    pub moisture: u16,
    pub fuel: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceDto>,
}

/// One result row, with distance present for ray and nearest queries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeQueryHitDto {
    pub plant: VegetationRuntimePlantDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
}

/// Bounded runtime macro query result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeQueryResult {
    pub matches: String,
    pub truncated: bool,
    pub hits: Vec<VegetationRuntimeQueryHitDto>,
}

/// Per-family and per-cell vegetation render population plus streaming faults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRenderStatsDto {
    /// One row per family with resident plants or field tiles.
    pub families: Vec<VegetationFamilyRenderDto>,
    /// One row per cell with resident plants or field tiles.
    pub cells: Vec<VegetationCellRenderDto>,
    /// GPU missing-page requests drained since startup.
    pub page_faults: String,
}

/// One family's resident render population.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationFamilyRenderDto {
    pub family: Uuid,
    /// Mirrored plant instances of the family.
    pub instances: u32,
    /// Resident micro field tiles of the family.
    pub field_tiles: u32,
    /// Cooked density upper bound of the family's blade candidates.
    pub micro_predicted: u64,
}

/// One cell's resident render population.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCellRenderDto {
    pub cell: WorldCellDto,
    /// Mirrored plant instances in the cell.
    pub plants: u32,
    /// Resident micro field tiles in the cell.
    pub field_tiles: u32,
}

/// One coalesced runtime cell-facet load request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimePendingCellDto {
    pub cell: WorldCellDto,
    pub facets: Vec<crate::ResidencyFacetDto>,
    pub priority: i32,
    pub source_revision: String,
}

/// Per-facet decoded-byte accounting, encoded as strings for JavaScript safety.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeFacetBytesDto {
    pub render: String,
    pub physics: String,
    pub simulation: String,
    pub editing: String,
    pub navigation: String,
    pub network: String,
}

/// Closed reason the sole runtime vegetation authority is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationRuntimeUnavailableReasonDto {
    NoProject,
    NoEnabledField,
    NoCookedManifest,
    Fault,
}

/// Runtime residency and persistent-state status for one exact cooked generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeAvailableStatusDto {
    pub world: Uuid,
    pub map: Uuid,
    pub manifest_identity: String,
    pub persistent_state_identity: String,
    pub persistent_cells: String,
    pub persistent_plants: String,
    pub prediction_count: String,
    pub source_count: String,
    pub requested_cells: String,
    pub resident_cells: String,
    pub requested_bytes: VegetationRuntimeFacetBytesDto,
    pub resident_bytes: VegetationRuntimeFacetBytesDto,
    pub budgets: VegetationRuntimeFacetBytesDto,
    pub pending: Vec<VegetationRuntimePendingCellDto>,
    pub regeneration_cells: Vec<WorldCellDto>,
    /// Collision-facet body residency; absent without a live play world.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<VegetationCollisionResidencyDto>,
    /// Promotion counters; absent without a live play world.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<VegetationPromotionReportDto>,
}

/// Batched collision-facet residency counters for the live play world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCollisionResidencyDto {
    pub resident_cells: String,
    pub resident_bodies: String,
    pub created_total: String,
    pub removed_total: String,
    pub hull_skipped_total: String,
    pub failed_families: String,
}

/// Runtime residency and persistent-state status for one exact cooked generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum VegetationRuntimeStatusDto {
    Unavailable {
        reason: VegetationRuntimeUnavailableReasonDto,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Available(Box<VegetationRuntimeAvailableStatusDto>),
}

/// Parameters for inspecting one runtime cell generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeCellParams {
    pub cell: WorldCellDto,
}

/// Current immutable runtime generation for one cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimeCellResult {
    pub cell: WorldCellDto,
    pub generation: String,
    pub manifest_identity: String,
    pub resident_facets: Vec<crate::ResidencyFacetDto>,
    pub macro_plants: String,
    pub micro_tiles: String,
    pub disturbance_masks: String,
}

/// Selects one stable runtime plant identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimePlantParams {
    pub plant: PlantId,
}

/// Where one plant sits in the promotion lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum PlantPromotionStateDto {
    /// The macro row is the only representation.
    Bulk,
    /// Promotion commits at the next synchronization point.
    Promoting,
    /// A live entity view owns render, collision, and simulation.
    Promoted { entity: Uuid },
    /// Demotion commits at the next synchronization point.
    Demoting { entity: Uuid },
}

/// One plant's promotion state after a requested transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationPromotionResult {
    pub plant: PlantId,
    pub state: PlantPromotionStateDto,
}

/// What one plant contributes to navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum NavigationContributionKindDto {
    /// Passable, at a traversal-cost multiplier.
    Cost,
    /// An immovable simplified obstacle.
    StaticObstacle,
    /// An obstacle that is moving, so a consumer treats it as dynamic until it settles.
    DynamicObstacle,
}

/// One published navigation contribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNavigationContributionDto {
    pub plant: PlantId,
    pub kind: NavigationContributionKindDto,
    pub bounds: WorldBoundsDto,
    /// World-space footprint polygon, X/Z metre pairs in authored order.
    pub footprint: Vec<[f64; 2]>,
    pub height_m: f64,
    pub cost: f64,
}

/// One cell's published contributions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNavigationCellDto {
    pub cell: WorldCellDto,
    pub contributions: Vec<VegetationNavigationContributionDto>,
}

/// Reads the navigation seam: published contributions plus the dirty regions awaiting a rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNavigationParams {
    /// Take ownership of the dirty regions, clearing them from the seam. Omit to peek.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drain_dirty: Option<bool>,
}

/// The navigation seam's current publication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationNavigationResult {
    pub cells: Vec<VegetationNavigationCellDto>,
    pub dirty_regions: Vec<WorldBoundsDto>,
    pub contributions: String,
    pub obstacles: String,
    pub dynamic_obstacles: String,
    pub drained: bool,
}

/// What one committed vegetation transition did, in gameplay terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum VegetationTransitionKindDto {
    Damaged {
        amount: u16,
        health: u16,
    },
    Harvested {
        phenotype: u32,
    },
    Burned {
        phenotype: u32,
        remaining_fuel: u16,
    },
    Removed,
    Planted,
    Regrew {
        lifecycle: PlantLifecycleDto,
        phenotype: u32,
    },
    LifecycleChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<PlantLifecycleDto>,
        to: PlantLifecycleDto,
    },
    Ignited,
    Extinguished,
    Wetted {
        moisture: u16,
        fuel: u16,
    },
    StateReplaced,
    Moved,
    Disturbed {
        categories: u32,
    },
}

/// Advances biological time and catches dependency regions up to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationAdvanceEcologyParams {
    /// World biological tick to reach; never behind the clock.
    pub target_tick: String,
    /// Ticks this call may execute before reporting what it still owes.
    pub max_ticks: u32,
    /// Sampled water reaching the ground, as `UnitInterval` bits.
    pub water: u16,
    /// Sampled warmth available for growth, as `UnitInterval` bits.
    pub warmth: u16,
}

/// What one catch-up call did, and what it still owes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEcologyReportDto {
    pub world_tick: String,
    pub regions: u32,
    pub regions_caught_up: u32,
    pub regions_awaiting_residency: u32,
    pub ticks_run: String,
    pub ticks_owed: String,
    /// Hex checkpoint identity over the rule set, the clock, and every boundary summary.
    pub checkpoint: String,
}

/// One dependency region's catch-up standing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEcologyRegionDto {
    pub cells: Vec<WorldCellDto>,
    /// The tick every cell of the region stands at.
    pub tick: String,
    /// Whether the region has reached world time.
    pub caught_up: bool,
    /// Whether every cell it spans carries resident macro rows, which a tick requires.
    pub resident: bool,
}

/// One cell's published boundary summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEcologyCellDto {
    pub cell: WorldCellDto,
    pub tick: String,
    pub plants: u32,
    pub canopy: u16,
    pub health: u16,
    pub moisture: u16,
    pub fuel: u16,
}

/// Where biological time stands, region by region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEcologyStatusDto {
    pub world_tick: String,
    /// Rule set the committed state was simulated under.
    pub simulation_version: u32,
    /// Hex checkpoint identity over the rule set, the clock, and every boundary summary.
    pub checkpoint: String,
    /// Region radius in cells, the widest declared influence.
    pub region_radius_cells: u32,
    pub regions: Vec<VegetationEcologyRegionDto>,
    pub cells: Vec<VegetationEcologyCellDto>,
}

/// Samples the combustible state of a volume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCombustionParams {
    pub bounds: WorldBoundsDto,
    #[serde(default)]
    pub filter: VegetationRuntimeQueryFilterDto,
}

/// What a volume holds, for a system that needs to know whether it will burn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCombustionDto {
    pub plants: u32,
    pub ignited: u32,
    pub fuel: u16,
    pub moisture: u16,
    pub health: u16,
    pub occupancy: u16,
}

/// One sequence-stamped committed transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEventDto {
    pub seq: String,
    pub transaction: String,
    pub cell: WorldCellDto,
    /// The plant it names; absent for a cell-wide change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plant: Option<PlantId>,
    pub transition: VegetationTransitionKindDto,
}

/// Reads committed transitions newer than a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationDrainEventsParams {
    /// The caller's last-seen sequence number; omit to read the whole retained ring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// Events after the cursor plus the metadata a stale cursor needs to notice a gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationDrainEventsResult {
    pub events: Vec<VegetationEventDto>,
    pub high_water_seq: String,
    pub oldest_seq: String,
    pub overflowed: bool,
}

/// Promotion counters for the live play world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationPromotionReportDto {
    pub promoted: String,
    pub promoting: String,
    pub demoting: String,
    pub promoted_total: String,
    pub demoted_total: String,
    pub felled_total: String,
    pub failed_total: String,
    pub flushed_total: String,
}

/// Persistent overlay for one plant, independent of current facet residency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimePlantStateDto {
    pub cell: WorldCellDto,
    pub cell_revision: String,
    pub added: bool,
    pub tombstoned: bool,
    pub position_ticks: Option<[String; 3]>,
    pub lifecycle: Option<PlantLifecycleDto>,
    pub phenotype: Option<u32>,
    pub ecology_tick: Option<String>,
    pub health: Option<u16>,
    pub moisture: Option<u16>,
    pub fuel: Option<u16>,
    pub interaction_policy: Option<InteractionPolicyDto>,
    pub promoted: bool,
}

/// Resident effective row plus any persistent overlay and editor provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimePlantInspectResult {
    pub plant: PlantId,
    pub resident: Option<VegetationRuntimePlantDto>,
    /// Every persistent delta the plant carries, in canonical cell order. A plant that moved has
    /// one entry per cell that recorded state for it: its base cell plus the cell it now occupies.
    pub persistent: Vec<VegetationRuntimePlantStateDto>,
    /// Promotion lifecycle state; absent without a live play world.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PlantPromotionStateDto>,
}

/// Canonical strict runtime-state snapshot and its exact generation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationStateSnapshotDto {
    pub manifest_identity: String,
    pub content_hash: String,
    pub bytes: String,
    pub data_hex: String,
}

/// Imports one canonical strict runtime-state snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationStateImportParams {
    pub data_hex: String,
}

/// A botanical element class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BotanicalElementDto {
    Trunk,
    Branch,
    Root,
    Vine,
    Frond,
    Leaf,
    Needle,
    Blade,
    Flower,
    Fruit,
    Bud,
    Scar,
    DeadPart,
}

/// How child attachments are arranged around a parent axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PhyllotaxisPatternDto {
    Alternate,
    Opposite,
    Whorled,
    Spiral,
}

/// Which way a tropism bends an axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum TropismKindDto {
    Phototropism,
    Gravitropism,
    Thigmotropism,
}

/// Which axes a prune rule removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PruneRuleDto {
    BelowHeight,
    ShorterThan,
    KeepStrongest,
}

/// One point of a taper curve: where along the axis, and the radius factor there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalCurvePointDto {
    /// Position along the axis, as `UnitInterval` bits.
    pub at: u16,
    /// Radius factor, as Q15.16 bits.
    pub factor_bits: i32,
}

/// One point of a hand-drawn spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalDrawnPointDto {
    /// Position in family-local metres, as Q15.16 bits.
    pub position_bits: [i32; 3],
    /// Radius there, as Q15.16 bits.
    pub radius_bits: i32,
}

/// One typed botanical operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum BotanicalOperatorDto {
    Drawn {
        element: BotanicalElementDto,
        points: Vec<BotanicalDrawnPointDto>,
    },
    Trunk {
        element: BotanicalElementDto,
        length_bits: i32,
        base_radius_bits: i32,
        taper: Vec<BotanicalCurvePointDto>,
        segments: u32,
    },
    Branch {
        element: BotanicalElementDto,
        length_ratio: u16,
        radius_ratio: u16,
        declination: u16,
        jitter: u16,
        segments: u32,
    },
    Phyllotaxis {
        pattern: PhyllotaxisPatternDto,
        count: u32,
        nodes: u32,
        start: u16,
        end: u16,
        divergence: u16,
    },
    Tropism {
        kind_of: TropismKindDto,
        strength: u16,
    },
    Prune {
        rule: PruneRuleDto,
        threshold_bits: i32,
        count: u32,
    },
    Roots {
        depth_ratio: u16,
        spread_ratio: u16,
        count: u32,
    },
    Shell {
        material_slot: u32,
        sides: u32,
    },
    Instance {
        element: BotanicalElementDto,
        material_slot: u32,
        size_bits: i32,
        jitter: u16,
    },
    Family,
}

/// One node of a botanical graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalNodeDto {
    /// Stable node GUID as a decimal `u128`.
    pub guid: String,
    pub version: u32,
    pub semantic_revision: u32,
    pub operator: BotanicalOperatorDto,
}

/// One directed typed edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalEdgeDto {
    pub from_node: String,
    pub from_pin: String,
    pub to_node: String,
    pub to_pin: String,
}

/// What one manual edit does to its target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(export)]
pub enum BotanicalEditActionDto {
    Transform {
        /// Translation in family-local metres, as Q15.16 bits.
        offset_bits: [i32; 3],
        /// Turn about the target's base, as `UnitInterval` bits.
        roll: u16,
        /// Uniform scale as Q15.16 bits, where 65536 is unchanged.
        scale_bits: i32,
    },
    Trim {
        /// Where along the axis the cut falls, as `UnitInterval` bits.
        at: u16,
    },
    Remove,
    Graft {
        /// The family graft source supplying the geometry.
        source: VegetationGuid,
        /// Which of that source's elements to take.
        selector: PlantSourceSelectorDto,
    },
}

/// One manual edit laid over what the graph grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalManualEditDto {
    /// Element identity the edit addresses, as a decimal `u128`.
    pub target: String,
    pub action: BotanicalEditActionDto,
}

/// Why an authored edit found nothing to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BotanicalEditOrphanReasonDto {
    TargetMissing,
    TargetKind,
    TargetRemoved,
}

/// One edit that did not apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalEditOrphanDto {
    /// Element identity the edit addressed, as a decimal `u128`.
    pub target: String,
    pub action: BotanicalEditActionDto,
    pub reason: BotanicalEditOrphanReasonDto,
}

/// Imports instanced points from a digital-content-creation tool into an authored map layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationImportPointsParams {
    /// The authored vegetation map to commit into.
    pub map: crate::AssetSelector,
    /// The authored layer that owns the anchors.
    pub layer: VegetationGuid,
    /// Path to the point file. A Houdini JSON `.geo` point cloud.
    pub path: String,
    /// Prototype name to plant-family catalog identity. An unnamed source uses the single entry.
    pub prototypes: Vec<VegetationPointPrototypeDto>,
    /// The map generation the caller last observed.
    pub expected_generation: String,
}

/// One prototype binding: the source's name for it, and the family it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationPointPrototypeDto {
    /// Source-facing prototype name.
    pub name: String,
    /// The plant family it resolves to.
    pub family: Uuid,
}

/// What one point import committed, and what it could not express.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationImportPointsResult {
    /// Anchors committed.
    pub anchors: u32,
    /// Tiles the anchors landed in.
    pub tiles: u32,
    /// Prototypes the source placed.
    pub prototypes: u32,
    /// Source attributes the canonical point vocabulary cannot express.
    pub unsupported: Vec<String>,
    /// The map generation after the commit.
    pub generation: String,
}

/// Exports one authored layer's anchors for a round trip through a content-creation tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationExportPointsParams {
    /// The authored vegetation map to read.
    pub map: crate::AssetSelector,
    /// The authored layer whose anchors to export.
    pub layer: VegetationGuid,
    /// Destination path for the Houdini JSON `.geo` point cloud.
    pub path: String,
}

/// What one point export wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationExportPointsResult {
    /// Instances written.
    pub instances: u32,
    /// Prototypes written.
    pub prototypes: u32,
    /// The file that was written.
    pub path: String,
}

/// One individual a graph grows: a seed, an intrinsic age, and a name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalVariationDto {
    /// Seed selecting the individual, as a decimal `u128`.
    pub seed: String,
    /// Intrinsic age as `UnitInterval` bits, where 65535 is fully grown.
    pub age: u16,
    /// Artist-facing name.
    pub name: String,
}

/// One native plant family's botanical graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalGraphDto {
    /// The individuals the graph grows, in authored order; the first is the representative one and
    /// each becomes a family variation.
    pub variations: Vec<BotanicalVariationDto>,
    pub nodes: Vec<BotanicalNodeDto>,
    pub edges: Vec<BotanicalEdgeDto>,
    /// Manual edits laid over what the nodes grow, in canonical target order.
    #[serde(default)]
    pub edits: Vec<BotanicalManualEditDto>,
}

/// Replaces one native plant family's botanical graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGraphSetParams {
    pub plant: crate::AssetSelector,
    pub graph: BotanicalGraphDto,
    /// External hero meshes the graph's grafts name, in canonical identity order.
    #[serde(default)]
    pub grafts: Vec<PlantGraftSourceDto>,
}

/// The graph a plant family carries, and what it grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGraphResult {
    pub plant: Uuid,
    pub graph: BotanicalGraphDto,
    /// External hero meshes the graph's grafts name.
    pub grafts: Vec<PlantGraftSourceDto>,
    pub growth: BotanicalGrowthDto,
}

/// Creates a native plant family from the starter botanical graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCreateParams {
    /// Catalog name for the new family.
    pub name: String,
    /// Catalog folder.
    #[serde(default)]
    pub folder: String,
    /// Seed selecting which individual the starter graph grows; zero derives one from the name.
    #[serde(default)]
    pub seed: String,
    /// Material assets bound to the graph's slots, bark first.
    pub materials: Vec<Uuid>,
}

/// What one grown botanical graph produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalGrowthDto {
    /// Exact graph identity.
    pub graph: String,
    /// Which declared variation the report describes.
    pub variation: u32,
    /// Seed the individual grew from.
    pub seed: String,
    /// Intrinsic age it grew at, as `UnitInterval` bits.
    pub age: u16,
    /// Variations the graph declares.
    pub variations: u32,
    /// Grown axes.
    pub axes: u32,
    /// Attachment frames carrying a placed element.
    pub frames: u32,
    /// Swept shells.
    pub shells: u32,
    /// Instanced elements.
    pub elements: u32,
    /// Generated vertices.
    pub vertices: u32,
    /// Generated triangles.
    pub triangles: u32,
    /// Semantic parts the family declares.
    pub parts: u32,
    /// Structural spines.
    pub spines: u32,
    /// Family height in Q15.16 metres.
    pub height_bits: i32,
    /// Hero meshes grafted over generated elements. Their geometry is resolved by the cooker, so
    /// the vertex and triangle counts above are the generated surface alone.
    pub grafts: u32,
    /// Manual edits that found their target.
    pub applied_edits: u32,
    /// Manual edits that did not, and why.
    pub orphans: Vec<BotanicalEditOrphanDto>,
}

/// One created native plant family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCreateResult {
    /// Catalog identity of the new family.
    pub plant: Uuid,
    /// What its starter graph grew.
    pub growth: BotanicalGrowthDto,
}

/// Reads what one plant family's botanical graph grows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantGrowthParams {
    pub plant: crate::AssetSelector,
    /// Which declared variation to grow; zero is the representative individual.
    #[serde(default)]
    pub variation: u32,
}

/// One grown axis, as the authoring surface addresses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalAxisDto {
    /// Stable element identity as a decimal `u128`, which an edit targets.
    pub id: String,
    /// Parent axis identity, absent for a trunk or a drawn spine.
    pub parent: Option<String>,
    /// Attachment frame it grew from, absent when it grew from a base rather than a frame.
    pub frame: Option<String>,
    pub element: BotanicalElementDto,
    /// Base position in family-local metres, as Q15.16 bits.
    pub base_bits: [i32; 3],
    /// Tip position in family-local metres, as Q15.16 bits.
    pub tip_bits: [i32; 3],
    /// Radius at the base, as Q15.16 bits.
    pub base_radius_bits: i32,
    /// Rest points along the axis.
    pub points: u32,
}

/// One placed instanced element, as the authoring surface addresses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BotanicalPlacementDto {
    /// Stable element identity as a decimal `u128`, which an edit targets.
    pub id: String,
    /// Frame it sits on, as a decimal `u128`.
    pub frame: String,
    pub element: BotanicalElementDto,
    pub material_slot: u32,
    /// Position in family-local metres, as Q15.16 bits.
    pub position_bits: [i32; 3],
    /// Size in metres, as Q15.16 bits.
    pub size_bits: i32,
    /// Roll about the frame, as `UnitInterval` bits.
    pub roll: u16,
}

/// Every element of one plant family an edit can address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantElementsResult {
    pub plant: Uuid,
    /// Grown axes in canonical identity order.
    pub axes: Vec<BotanicalAxisDto>,
    /// Placed elements in canonical identity order.
    pub elements: Vec<BotanicalPlacementDto>,
}

/// Selects one authored plant family for pure validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantValidateParams {
    pub plant: crate::AssetSelector,
}

/// Validation, source provenance, and exact dependencies of one authored plant family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantValidationResult {
    pub plant: Uuid,
    pub validation: VegetationValidationSummaryDto,
    pub diagnostics: Vec<PlantCompileDiagnosticDto>,
    pub sources: Vec<PlantSourceReferenceDto>,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    pub conflicts: Vec<crate::ReimportConflictEntryDto>,
    pub source_updates: Vec<PlantSourceHashUpdateDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family_hash: Option<String>,
    pub statistics: PlantCompileStatisticsDto,
}

/// Recooks one authored plant family through its single retained source recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantRecookParams {
    pub plant: crate::AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_profile: Option<String>,
}

/// Successful normalized plant-family publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantRecookResult {
    pub plant: Uuid,
    pub family_hash: String,
    pub artifact_hash: String,
    pub cache_hit: bool,
    pub validation: VegetationValidationSummaryDto,
    pub diagnostics: Vec<PlantCompileDiagnosticDto>,
    pub sources: Vec<PlantSourceReferenceDto>,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    pub source_updates: Vec<PlantSourceHashUpdateDto>,
    pub statistics: PlantCompileStatisticsDto,
}

/// Metadata carried by every vegetation mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMutationHeaderDto {
    pub cell: WorldCellDto,
    pub transaction: VegetationGuid,
    pub authority: VegetationGuid,
    pub logical_tick: String,
    pub idempotency_key: VegetationGuid,
    pub base_revision: Option<String>,
}

/// Exact transform carried by vegetation mutation payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantTransformDto {
    pub global_ticks: [String; 3],
    pub orientation: [i16; 4],
    pub scale_bits: [i32; 3],
}

/// One typed mutation understood by the editor journal, save state, and future network envelopes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationMutationDto {
    FieldTilePatch {
        layer: VegetationGuid,
        channel: FieldChannelDto,
        tile: VegetationGuid,
        dimensions: [u32; 3],
        quantum_bits: i32,
        values: Vec<i32>,
    },
    AnchorAddition {
        point: PlantPointDto,
    },
    Tombstone {
        plant: PlantId,
    },
    TransformOverride {
        plant: PlantId,
        transform: PlantTransformDto,
    },
    StateOverride {
        plant: PlantId,
        lifecycle: Option<PlantLifecycleDto>,
        phenotype: Option<u32>,
        health: Option<u16>,
        moisture: Option<u16>,
        fuel: Option<u16>,
        interaction_policy: Option<InteractionPolicyDto>,
    },
    Planting {
        point: PlantPointDto,
    },
    Damage {
        plant: PlantId,
        amount: u16,
        phenotype: Option<u32>,
    },
    MoistureFuel {
        plant: PlantId,
        moisture: u16,
        fuel: u16,
    },
    LifecycleTransition {
        plant: PlantId,
        from: Option<PlantLifecycleDto>,
        to: PlantLifecycleDto,
        ecology_tick: String,
    },
    Harvest {
        plant: PlantId,
        phenotype: u32,
    },
    Burn {
        plant: PlantId,
        phenotype: u32,
        remaining_fuel: u16,
    },
    Ignite {
        plant: PlantId,
    },
    Extinguish {
        plant: PlantId,
    },
    Regrow {
        plant: PlantId,
        lifecycle: PlantLifecycleDto,
        phenotype: u32,
        ecology_tick: String,
    },
    PromotionOriginState {
        plant: PlantId,
        transform: PlantTransformDto,
        linear_velocity_bits: [i32; 3],
        angular_velocity_bits: [i32; 3],
    },
    DisturbanceMask {
        categories: u32,
        tile: VegetationGuid,
        values: Vec<i16>,
    },
}

/// One quantized authored field/blocker tile on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AuthoredFieldTileDto {
    pub channel: FieldChannelDto,
    pub layer: VegetationGuid,
    pub dimensions: [u32; 3],
    pub quantum_bits: i32,
    pub values: Vec<i32>,
}

/// One explicit authored anchor row on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ExplicitPlantAnchorDto {
    pub id: PlantId,
    pub layer: VegetationGuid,
    pub family: Uuid,
    pub point: PlantPointDto,
}

/// One typed authored map-chunk payload a brush transaction writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum VegetationMapChunkPayloadDto {
    Field {
        fields: Vec<AuthoredFieldTileDto>,
        blockers: Vec<AuthoredFieldTileDto>,
    },
    AnchorOverride {
        explicit_plants: Vec<ExplicitPlantAnchorDto>,
        pins: Vec<PlantId>,
        transform_overrides: Vec<PlantTransformOverrideDto>,
        state_overrides: Vec<PlantStateOverrideDto>,
    },
}

/// One complete authored chunk replacement in a brush transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkDto {
    pub key: VegetationMapChunkKeyDto,
    pub revision: String,
    pub payload: VegetationMapChunkPayloadDto,
}

/// Parameters for one optimistic authored-map chunk transaction (a brush gesture:
/// its touched tiles and anchors across cells commit atomically).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkCommitParams {
    pub map: crate::AssetSelector,
    /// Root generation captured before the gesture began.
    pub expected_generation: String,
    pub upserts: Vec<VegetationMapChunkDto>,
    pub removals: Vec<VegetationMapChunkKeyDto>,
}

/// Result of committing an authored-map chunk transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkCommitResult {
    /// The map root generation after the commit.
    pub generation: String,
}

/// Parameters for reading one cooked cell's rejected candidates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRejectionsParams {
    pub map: crate::AssetSelector,
    pub cell: WorldCellDto,
    /// Exact manifest identity; the current manifest when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// Row cap (default 1024).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One rejected candidate: its world position, reason, and sampler ordinal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRejectionDto {
    pub reason: VegetationCandidateRejectionReasonDto,
    pub position_ticks: [String; 3],
    pub ordinal: String,
}

/// Result of reading one cell's rejected candidates (rows capped by `limit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRejectionsResult {
    pub candidates: String,
    pub accepted: String,
    pub total_rejected: String,
    pub rows: Vec<VegetationRejectionDto>,
}

/// Parameters for one per-cell topology diff between two cooked manifests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationTopologyDiffParams {
    pub map: crate::AssetSelector,
    /// The older manifest identity (the editor captures it before the edit's recook).
    pub from: String,
    /// The newer manifest identity; the current manifest when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Restricts the diff to these cells; every cell of either manifest when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<WorldCellDto>>,
}

/// One authored override whose referenced plant is absent from the newer manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationOverrideConflictDto {
    /// The authored row kind: `anchor`, `pin`, `transform-override`, or `state-override`.
    pub kind: String,
    pub plant: PlantId,
    pub layer: VegetationGuid,
}

/// One cell's topology diff: identity churn plus unresolved authored overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationTopologyCellDiffDto {
    pub cell: WorldCellDto,
    pub added: String,
    pub removed: String,
    pub moved: String,
    /// Capped id samples of each churn class (`64` per list).
    pub added_ids: Vec<PlantId>,
    pub removed_ids: Vec<PlantId>,
    pub moved_ids: Vec<PlantId>,
    pub conflicts: Vec<VegetationOverrideConflictDto>,
}

/// Result of one topology diff: the compared identities and every changed cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationTopologyDiffResult {
    pub from: String,
    pub to: String,
    pub cells: Vec<VegetationTopologyCellDiffDto>,
}

/// Parameters for reading authored map chunks by logical key (a brush gesture's
/// read-modify-write starts here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkReadParams {
    pub map: crate::AssetSelector,
    pub keys: Vec<VegetationMapChunkKeyDto>,
}

/// Result of reading authored map chunks: the map root generation (the optimistic
/// baseline for the commit that follows) and every requested chunk present in the
/// inventory — a key with no chunk contributes no row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapChunkReadResult {
    pub generation: String,
    pub chunks: Vec<VegetationMapChunkDto>,
}

/// Parameters for one optimistic authored-map layer transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapLayerCommitParams {
    pub map: crate::AssetSelector,
    /// Root generation captured before editing began (optimistic concurrency).
    pub expected_generation: String,
    /// Complete replacement rows for the listed layer ids.
    pub upserts: Vec<VegetationLayerDto>,
    /// Layer ids removed from the map.
    pub removals: Vec<VegetationGuid>,
}

/// Result of committing an authored-map layer transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapLayerCommitResult {
    /// The map root generation after the commit.
    pub generation: String,
}

/// Parameters for applying a batch of typed vegetation mutations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMutateParams {
    /// The ordered mutation records; the batch applies atomically per record order.
    pub records: Vec<VegetationMutationRecordDto>,
}

/// Result of applying a vegetation mutation batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMutateResult {
    /// Records applied.
    pub applied: u32,
}

/// A vegetation mutation with common deterministic metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMutationRecordDto {
    pub header: VegetationMutationHeaderDto,
    pub mutation: VegetationMutationDto,
}

/// Parameters for querying a vegetation asset summary through the control plane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationAssetSummaryParams {
    pub asset: crate::AssetSelector,
}

/// Result of querying a vegetation asset summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationAssetSummaryResult {
    pub r#type: AssetTypeDto,
    pub summary: VegetationAssetSummaryDto,
    pub layers: Vec<VegetationLayerDto>,
}

/// Parameters for importing one authored native vegetation asset or map package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ImportVegetationAssetParams {
    pub path: String,
    pub folder: Option<String>,
}

/// Result of importing one authored vegetation asset into the project catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ImportVegetationAssetResult {
    pub id: Uuid,
    pub name: String,
    pub r#type: AssetTypeDto,
}

/// An opaque typed extension payload reserved for registered future point columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PointExtensionColumnDto {
    pub id: u32,
    pub element_type: String,
    pub stride: u32,
    #[schemars(with = "Vec<u8>")]
    #[ts(type = "number[]")]
    pub bytes: Vec<u8>,
}

/// Future-safe editor metadata attached to a typed mutation without entering reducer truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGestureMetadataDto {
    pub gesture: VegetationGuid,
    #[schemars(with = "std::collections::BTreeMap<String, Value>")]
    #[ts(type = "Record<string, unknown>")]
    pub presentation: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plant_id_is_a_strict_opaque_string() {
        let value = "1d8b921f8252f69b6c1c88f9da096eea";
        assert_eq!(
            serde_json::to_string(&PlantId(value.to_owned())).unwrap(),
            format!("\"{value}\"")
        );
        assert!(serde_json::from_str::<PlantId>(&format!("\"{value}\"")).is_ok());
        assert!(serde_json::from_str::<PlantId>("\"1D8B921F8252F69B6C1C88F9DA096EEA\"").is_err());
        assert!(serde_json::from_str::<PlantId>("42").is_err());
    }

    #[test]
    fn vegetation_guid_is_a_strict_opaque_string() {
        let value = "0000000000000000000000000000002a";
        assert_eq!(
            serde_json::to_string(&VegetationGuid(value.to_owned())).unwrap(),
            format!("\"{value}\"")
        );
        assert!(serde_json::from_str::<VegetationGuid>(&format!("\"{value}\"")).is_ok());
        assert!(serde_json::from_str::<VegetationGuid>("\"2A\"").is_err());
        assert!(serde_json::from_str::<VegetationGuid>("42").is_err());
    }
}
