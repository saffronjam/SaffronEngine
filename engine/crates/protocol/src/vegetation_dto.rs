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

/// Conservative graph counts and resource caps visible before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationGraphEstimateDto {
    pub candidates: String,
    pub accepted: String,
    pub micro_samples: String,
    pub memory_bytes: String,
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

/// Starts one bounded asynchronous vegetation evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluateRegionParams {
    pub map: Uuid,
    pub biome_instance: VegetationGuid,
    pub bounds: WorldBoundsDto,
    pub level: u8,
    pub ecology_tick: Option<String>,
    pub workers: Option<u16>,
}

/// Lifecycle state of one asynchronous evaluation job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum VegetationEvaluationJobStateDto {
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// Handle returned when a vegetation evaluation starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEvaluationJobDto {
    pub job: String,
    pub state: VegetationEvaluationJobStateDto,
    pub cells: String,
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
    pub summary: Option<VegetationEvaluationSummaryDto>,
    pub error: Option<String>,
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

/// One exact dependency in an immutable vegetation base manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationManifestDependencyDto {
    pub id: Uuid,
    pub content_hash: String,
}

/// Immutable identity binding authored sources, schemas, and cooked base cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationBaseManifestDto {
    pub version: u32,
    pub map: Uuid,
    pub map_hash: String,
    pub dependencies: Vec<VegetationManifestDependencyDto>,
    pub point_schema_hash: String,
    pub evaluator_version: u32,
    pub cooker_version: u32,
    pub identity: String,
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
