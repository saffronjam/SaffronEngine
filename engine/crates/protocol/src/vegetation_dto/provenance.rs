use crate::{PlantId, Uuid, VegetationGuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
