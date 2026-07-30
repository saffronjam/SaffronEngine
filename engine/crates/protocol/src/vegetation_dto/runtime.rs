use crate::{
    InteractionPolicyDto, PlantId, PlantLifecycleDto, ProvenanceDto, Uuid,
    VegetationPromotionReportDto, WorldBoundsDto, WorldCellDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
#[ts(
    export,
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
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
#[ts(
    export,
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
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
#[ts(
    export,
    tag = "state",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
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
