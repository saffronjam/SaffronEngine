use crate::VegetationRuntimeFacetBytesDto;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Wall-clock microseconds one vegetation synchronization spent, split by stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationStageTimesDto {
    /// Residency scheduling.
    pub residency_us: u64,
    /// Promotion transitions committing.
    pub promotion_us: u64,
    /// Collision residency.
    pub collision_us: u64,
    /// The navigation seam.
    pub navigation_us: u64,
    /// Ecology ticks.
    pub ecology_us: u64,
    /// The whole synchronization.
    pub total_us: u64,
}

/// Work the vegetation runtime did since the world was bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationWorkCountersDto {
    /// Synchronization points executed.
    pub synchronizations: String,
    /// Spatial queries answered.
    pub queries: String,
    /// Plants those queries returned.
    pub query_hits: String,
    /// Mutation transactions reduced.
    pub mutations: String,
    /// Canonical bytes those transactions carried.
    pub mutation_bytes: String,
    /// State snapshots exported.
    pub snapshots: String,
    /// Bytes those snapshots carried.
    pub snapshot_bytes: String,
    /// Ecology ticks executed.
    pub ecology_ticks: String,
}

/// One artifact that failed verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationArtifactFaultDto {
    /// Path relative to the artifact-store root.
    pub path: String,
    /// `absent` when the manifest names it and the store lacks it, `corrupt` when its bytes do not
    /// hash to the identity its name claims.
    pub fault: String,
}

/// Verifies every artifact the current generations name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationVerifyParams {
    /// Whether to remove corrupt artifacts so the next cook republishes them.
    #[serde(default)]
    pub repair: bool,
}

/// What one verification pass found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationVerifyResult {
    /// Artifacts rehashed.
    pub checked: String,
    /// Corrupt artifacts removed.
    pub repaired: String,
    /// Faults found, in canonical path order.
    pub faults: Vec<VegetationArtifactFaultDto>,
}

/// The published initial persistent-state baseline for one cooked generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationStateBaselineResult {
    /// The generation the baseline belongs to.
    pub manifest_identity: String,
    /// Bytes the baseline occupies.
    pub bytes: String,
    /// Cells the baseline carries persistent state for.
    pub cells: String,
}

/// What the cook queue has done, and what is live in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationCookQueueDto {
    /// Jobs live right now, queued or running.
    pub live: String,
    /// Jobs accepted.
    pub submitted: String,
    /// Jobs that published.
    pub completed: String,
    /// Jobs a caller cancelled.
    pub cancelled: String,
    /// Jobs a newer request superseded.
    pub superseded: String,
    /// Jobs that failed.
    pub failed: String,
    /// Microseconds from acceptance to a terminal state, summed over every job that reached one.
    pub latency_us: String,
}

/// The vegetation runtime's compact telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationTelemetryResult {
    /// Stage times of the last completed synchronization.
    pub last: VegetationStageTimesDto,
    /// Exponentially averaged stage times.
    pub average: VegetationStageTimesDto,
    /// Counters since the world was bound.
    pub work: VegetationWorkCountersDto,
    /// Resident bytes by facet, which is the memory side of the same picture.
    pub resident_bytes: VegetationRuntimeFacetBytesDto,
    /// Bodies the batched collision residency holds.
    pub collision_bodies: String,
    /// Contributions the navigation seam publishes.
    pub navigation_contributions: String,
    /// Plants a promoted entity view owns.
    pub promoted: String,
    /// The cook queue's own counters.
    pub cook_queue: VegetationCookQueueDto,
}
