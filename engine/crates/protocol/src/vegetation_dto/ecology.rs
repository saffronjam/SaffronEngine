use crate::{
    InteractionPolicyDto, PlantId, PlantLifecycleDto, PlantPromotionOriginDto,
    PlantPromotionStateDto, VegetationRuntimePlantDto, VegetationRuntimeQueryFilterDto,
    WorldBoundsDto, WorldCellDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What one committed vegetation transition did, in gameplay terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
#[ts(
    export,
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
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
///
/// The weather, influence, and worker count come from the world's ecology clock, so an explicit step
/// and the clock's own ticks run under the same rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationAdvanceEcologyParams {
    /// World biological tick to reach; never behind the clock.
    pub target_tick: String,
    /// Ticks this call may execute before reporting what it still owes.
    pub max_ticks: u32,
}

/// Configures the world simulation clock biology advances on; every field is optional, and a request
/// with none reads the clock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEcologyClockParams {
    /// Whether simulated play time reaches the clock. A stopped clock runs no tick of its own and
    /// works off none of what a region owes.
    #[serde(default, deserialize_with = "crate::dto::coerce::opt_boolean")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<bool>,
    /// Simulated milliseconds one ecology tick spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tick_milliseconds: Option<u32>,
    /// Region ticks one synchronization point may execute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_ticks_per_sync: Option<u32>,
    /// Threads the per-region rule evaluation spreads across.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers: Option<u32>,
    /// Sampled water reaching the ground, as `UnitInterval` bits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub water: Option<u16>,
    /// Sampled warmth available for growth, as `UnitInterval` bits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warmth: Option<u16>,
}

/// The world simulation clock biology advances on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationEcologyClockDto {
    /// Whether simulated play time reaches the clock.
    pub running: bool,
    /// Simulated milliseconds one ecology tick spans.
    pub tick_milliseconds: u32,
    /// Simulated milliseconds accumulated toward the next whole tick.
    pub pending_milliseconds: String,
    /// Region ticks one synchronization point may execute.
    pub max_ticks_per_sync: u32,
    /// Threads the per-region rule evaluation spreads across.
    pub workers: u32,
    /// Sampled water reaching the ground, as `UnitInterval` bits.
    pub water: u16,
    /// Sampled warmth available for growth, as `UnitInterval` bits.
    pub warmth: u16,
    /// Ticks the world clock has yet to work off, from the last catch-up it ran. Only what a
    /// resident region owes: ticks waiting on ground to load are in the catch-up report.
    pub ticks_owed: String,
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
    /// Ticks owed by resident regions, which a following call runs.
    pub ticks_owed: String,
    /// Ticks owed by regions a cell they span is not resident, which nothing runs until that ground
    /// loads.
    pub ticks_awaiting_residency: String,
    /// Threads the call spread the per-region rule evaluation across.
    pub workers: u32,
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
    /// The world simulation clock driving biology.
    pub clock: VegetationEcologyClockDto,
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
    /// Views dropped without a write-back because another authority took the plant over.
    pub released_total: String,
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
    /// The exact state the plant's last promoted simulation returned, including the momentum a
    /// re-promotion hands back to its entity view. Absent for a plant no promotion ever wrote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion_origin: Option<PlantPromotionOriginDto>,
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
