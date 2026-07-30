use crate::{
    BotanicalAxisDto, BotanicalGrowthDto, BotanicalPlacementDto, PlantCompileDiagnosticDto,
    PlantCompileStatisticsDto, PlantLifecycleDto, PlantSourceHashUpdateDto,
    PlantSourceReferenceDto, Uuid, VegetationManifestDependencyDto, VegetationValidationSummaryDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    /// Stop after this many axes, for a bounded preview of a heavy graph. Absent grows it in
    /// full, which is what the cook does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_axes: Option<u32>,
    /// Stop after this many placed elements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_elements: Option<u32>,
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

/// A phenotype's semantic role, mirroring `saffron_vegetation::PhenotypeRole`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PhenotypeRoleDto {
    /// Healthy lifecycle appearance.
    Healthy,
    /// Harvested appearance.
    Harvested,
    /// Damaged appearance.
    Damaged,
    /// Burned appearance.
    Burned,
    /// Dead appearance.
    Dead,
    /// Flowering seasonal appearance.
    Flowering,
    /// Fruiting seasonal appearance.
    Fruiting,
    /// Senescent (autumn) seasonal appearance.
    Senescent,
    /// Wet appearance.
    Wet,
}

/// One authored appearance a family can render in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantPhenotypeDto {
    /// Stable family-local identity.
    pub id: u32,
    /// What the appearance means.
    pub role: PhenotypeRoleDto,
    /// The family variation it renders.
    pub variation: u32,
    /// Seasonal window `[start, end]` in per-mille of the year, wrapping through 1000. Absent
    /// derives the role's default window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_window: Option<[u32; 2]>,
    /// Material slot remaps as `[from, to]` pairs. This is how one variation renders differently
    /// in two phenotypes without growing a second set of geometry.
    #[serde(default)]
    pub material_remap: Vec<[u32; 2]>,
    /// Parts active in this phenotype as decimal strings; empty means all of them. A subset is
    /// what makes a phenotype change the SILHOUETTE rather than only the colour.
    #[serde(default)]
    pub active_parts: Vec<String>,
}

/// Params of `plant-phenotypes`: read a family's authored appearances, or replace the whole list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantPhenotypesParams {
    /// The plant family.
    pub plant: crate::AssetSelector,
    /// The complete replacement list. Omit to read. Replacing whole rather than patching is what
    /// keeps one call one semantic operation, and what lets the family validator reject a set that
    /// does not hold together instead of a field that looks fine alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phenotypes: Option<Vec<PlantPhenotypeDto>>,
}

/// Reply of `plant-phenotypes`: the family's appearances as they now stand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantPhenotypesResult {
    /// The plant family.
    pub plant: Uuid,
    /// Its authored appearances, in family order.
    pub phenotypes: Vec<PlantPhenotypeDto>,
}

/// Where one material slot's coverage landed in a family's packed atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantAtlasPlacementDto {
    /// The material slot this rectangle carries.
    pub slot: u32,
    /// Left edge in texels.
    pub x: u32,
    /// Top edge in texels.
    pub y: u32,
    /// Width in texels.
    pub width: u32,
    /// Height in texels.
    pub height: u32,
}

/// Params of `plant-atlas`: the packed coverage atlas one cooked family carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantAtlasParams {
    /// The plant family.
    pub plant: crate::AssetSelector,
    /// Mip level to return, level 0 (full size) by default. The chain is coverage-preserving, so a
    /// smaller level is what the renderer actually samples at distance — worth being able to look
    /// at, because a mip that lost alpha area is where distant foliage goes thin.
    #[serde(default)]
    pub level: u32,
}

/// Reply of `plant-atlas`: the atlas image and what occupies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantAtlasResult {
    /// The plant family.
    pub plant: Uuid,
    /// The returned level.
    pub level: u32,
    /// Levels in the cooked chain.
    pub level_count: u32,
    /// Returned level width in texels.
    pub width: u32,
    /// Returned level height in texels.
    pub height: u32,
    /// Gutter texels the layout was packed with.
    pub gutter: u32,
    /// Where each material slot landed, in level-0 texels.
    pub placements: Vec<PlantAtlasPlacementDto>,
    /// The level as a base64 PNG.
    pub base64: String,
}

/// The five components of a hierarchy node's declared transition error, in Q15.16 local units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AppearanceErrorDto {
    /// Geometry/silhouette error.
    pub silhouette: u32,
    /// Projected coverage-density error.
    pub coverage: u32,
    /// Transmitted-energy error.
    pub transmission: u32,
    /// Albedo/roughness variation error.
    pub material: u32,
    /// Normal-distribution error.
    pub normal_distribution: u32,
    /// The saturating sum the cut selector compares against its pixel threshold.
    pub total: u32,
}

/// One node of a cooked family's virtual-geometry hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantHierarchyNodeDto {
    /// Stable node index.
    pub id: u32,
    /// Parent node, absent for a root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<u32>,
    /// Depth from the root, so a reader can see the cut's shape without walking parents.
    pub depth: u32,
    /// `triangles` or `voxel` — which representation this node draws.
    pub representation: String,
    /// Triangle clusters the node owns, or the voxel brick's triangle count.
    pub primitives: u32,
    /// The content page the payload streams from.
    pub page: u32,
    /// Children needed for hole-free refinement.
    pub child_count: u32,
    /// The declared transition error the cut selector reads.
    pub appearance_error: AppearanceErrorDto,
}

/// Params of `plant-hierarchy`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantHierarchyParams {
    /// The plant family.
    pub plant: crate::AssetSelector,
}

/// Reply of `plant-hierarchy`: the cooked cut, node by node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantHierarchyResult {
    /// The plant family.
    pub plant: Uuid,
    /// Triangle-cluster nodes in the hierarchy.
    pub triangle_nodes: u32,
    /// Aggregate-voxel nodes in the hierarchy.
    pub voxel_nodes: u32,
    /// The nodes, in id order.
    pub nodes: Vec<PlantHierarchyNodeDto>,
}

/// Params of `plant-season-phenotype`: which appearance a family renders at a point in the year.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantSeasonPhenotypeParams {
    /// The plant family.
    pub plant: crate::AssetSelector,
    /// Point in the year as per-mille, `0..1000`.
    #[schemars(range(min = 0, max = 999))]
    pub season_mille: u32,
    /// Typed lifecycle state; healthy by default. Lifecycle wins over season, so a dead plant does
    /// not turn autumnal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<PlantLifecycleDto>,
}

/// Reply of `plant-season-phenotype`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantSeasonPhenotypeResult {
    /// The plant family.
    pub plant: Uuid,
    /// The phenotype the family renders under those conditions.
    pub phenotype: u32,
    /// The variation that phenotype draws, which is what a preview binds alongside it.
    pub variation: u32,
}

/// One derived collision proxy a family carries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCollisionProxyDto {
    /// `box`, `sphere`, `capsule`, or `convexHull`.
    pub shape: PlantCollisionShapeDto,
    /// Local centre in metres.
    pub center_m: [f32; 3],
    /// Half extents, or radius and half-height for a capsule, in metres.
    pub dimensions_m: [f32; 3],
    /// Whether the proxy comes off when the plant breaks. A trunk capsule never does — breaking a
    /// trunk fells the plant rather than pruning it.
    pub breakable: bool,
}

/// The shape family of a derived collision proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum PlantCollisionShapeDto {
    /// Oriented box.
    Box,
    /// Sphere.
    Sphere,
    /// Capsule.
    Capsule,
    /// Convex hull over a named part set.
    ConvexHull,
}

/// One derived navigation proxy: a footprint and the obstacle height above it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantNavigationProxyDto {
    /// Footprint polygon as local XZ pairs in metres.
    pub footprint_m: Vec<[f32; 2]>,
    /// Obstacle height in metres.
    pub height_m: f32,
    /// Unit traversal cost; one is neutral.
    pub cost: f32,
}

/// Params of `plant-proxies`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantProxiesParams {
    /// The plant family.
    pub plant: crate::AssetSelector,
}

/// Reply of `plant-proxies`: what the family derived for collision and navigation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantProxiesResult {
    /// The plant family.
    pub plant: Uuid,
    /// Collision proxies, thickest first as the cooker ordered them.
    pub collision: Vec<PlantCollisionProxyDto>,
    /// Navigation proxies.
    pub navigation: Vec<PlantNavigationProxyDto>,
}
