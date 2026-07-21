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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum PlantSourceSelectorDto {
    Whole,
    Element { id: VegetationGuid, path: String },
    Submesh { element: VegetationGuid, index: u32 },
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
    pub validation: VegetationValidationSummaryDto,
    pub provenance: Vec<VegetationSourceProvenanceDto>,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_cook: Option<VegetationCookStatisticsDto>,
}

/// Catalog/editor summary for one `.svegmap`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationMapSummaryDto {
    pub id: Uuid,
    pub name: String,
    pub version: u32,
    pub bounds: WorldBoundsDto,
    pub layer_count: u32,
    pub biome_instances: Vec<Uuid>,
    pub chunk_level: u8,
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
    pub position_ticks: [String; 3],
    pub bounds: WorldBoundsDto,
    pub family: Uuid,
    /// Decimal stable plant-tag identities.
    pub tags: Vec<String>,
    pub lifecycle: PlantLifecycleDto,
    pub phenotype: u32,
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

/// Parameters for inspecting one stable runtime plant identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationRuntimePlantInspectParams {
    pub plant: PlantId,
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
    pub persistent: Option<VegetationRuntimePlantStateDto>,
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
