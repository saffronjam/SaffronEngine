use crate::{
    FieldChannelDto, InteractionPolicyDto, PlantId, PlantLifecycleDto, PlantPointDto,
    PlantStateOverrideDto, PlantTransformOverrideDto, Uuid, VegetationCandidateRejectionReasonDto,
    VegetationGuid, VegetationLayerDto, VegetationMapChunkKeyDto, WorldCellDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
