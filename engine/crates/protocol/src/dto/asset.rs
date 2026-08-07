use super::coerce;
use crate::{AnimationClipDto, AssetSelector, BoneDto, EntityRef, EntitySelector, Uuid, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The asset slot an `assign-asset` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AssetSlotDto {
    Mesh,
    Albedo,
    MetallicRoughness,
    Normal,
    Occlusion,
    Emissive,
    Height,
}

/// The surface a screenshot captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ScreenshotTargetDto {
    Viewport,
    Window,
}

/// The thumbnail image encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ThumbnailFormatDto {
    Png,
}

/// The catalog asset kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AssetTypeDto {
    Mesh,
    Texture,
    Other,
    Animation,
    Material,
    Model,
    Lut,
    Environment,
    Plant,
    Biome,
    VegetationMap,
}

/// Where a store-imported asset came from and under what license, recorded on the
/// catalog entry so attribution travels with the asset (CC-BY / Sketchfab require it).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AssetAttributionDto {
    /// Canonical license id (`cc0`, `cc-by`, `cc-by-sa`, …).
    pub license_id: String,
    /// Whether the license requires visible attribution.
    pub requires_attribution: bool,
    /// Canonical license url.
    pub license_url: String,
    /// The asset author / creator.
    pub author: String,
    /// The asset's page on the source service.
    pub source_url: String,
    /// The connector the asset came from (`polyhaven`, `sketchfab`, …).
    pub store_id: String,
}

/// Parameters for `import-model`: a local file path plus optional store attribution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportModelParams {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AssetAttributionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportModelResult {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InstantiateModelParams {
    pub asset: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum AssetPlacementPhaseDto {
    Preview,
    Commit,
    #[default]
    Clear,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPlacementParams {
    pub phase: AssetPlacementPhaseDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetSelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlacementTransformDto {
    pub translation: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPlacementResult {
    pub active: bool,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<PlacementTransformDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExtractSubAssetParams {
    pub asset: AssetSelector,
    pub sub_asset: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ClearExtractionParams {
    pub asset: AssetSelector,
    pub sub_asset: Uuid,
}

/// Parameters for `import-texture`: a file path plus an optional colorspace hint
/// (`srgb` | `linear` | `hdr` | `auto`). `auto`/absent keeps the file-extension heuristic;
/// `linear` is for data maps (normal/roughness/metallic/AO) so they don't upload as sRGB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportTextureParams {
    pub path: String,
    /// Explicit upload colorspace override (`srgb`/`linear`/`hdr`); usually left unset and
    /// derived from `role`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colorspace: Option<String>,
    /// Semantic role hint (`albedo`/`normal`/`roughness`/…/`hdri`, or a connector map key like
    /// `nor_gl`/`arm`). Drives preview routing and, absent `colorspace`, the upload colorspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportTextureResult {
    pub texture: Uuid,
}

/// Params for `import-lut`: the path to a creative `.cube` look to import as a LUT asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportLutParams {
    pub path: String,
}

/// The `import-lut` result: the registered creative-LUT asset id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImportLutResult {
    pub lut: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetEntryDto {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: AssetTypeDto,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rigged: Option<bool>,
    /// Texture: how its bytes are interpreted on upload (`srgb`/`linear`/`hdr`/`auto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colorspace: Option<String>,
    /// Texture: its semantic role (`albedo`/`normal`/`roughness`/…/`hdri`), for preview routing.
    /// Omitted for a non-texture asset or an unrecognized (`unknown`) role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Creation time (seconds since the Unix epoch) of the asset's backing file, for sorting.
    pub created_at: i64,
    /// Store source/license, present for assets imported from a connector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AssetAttributionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetList {
    pub assets: Vec<AssetEntryDto>,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScanAssetsResult {
    pub added: i32,
    pub removed: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReimportModelResult {
    pub updated: i32,
    pub added: i32,
    pub removed_from_source: i32,
    pub skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ReimportModelParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelInfoParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelSubAssetDto {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ModelInfoResult {
    pub id: Uuid,
    pub name: String,
    pub source_path: String,
    pub source_hash: String,
    pub material_count: i32,
    pub has_skin: bool,
    pub node_count: i32,
    pub total_bytes: u64,
    pub sub_assets: Vec<ModelSubAssetDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetReferencesParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetReferencesResult {
    pub referenced_by: Vec<String>,
    pub references: Vec<String>,
    pub footprint: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanCandidateDto {
    pub id: Uuid,
    pub path: String,
    pub category: String,
    pub bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanReport {
    pub candidates: Vec<CleanCandidateDto>,
    pub reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CleanAssetsParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteUnusedParams {
    pub ids: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub confirm: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteUnusedResult {
    pub deleted: i32,
    pub reclaimed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenameAssetParams {
    pub asset: AssetSelector,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetRef {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateAssetFolderParams {
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenameAssetFolderParams {
    pub folder: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteAssetFolderParams {
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MoveAssetParams {
    pub asset: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetUsagesParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetUsageDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_name: Option<String>,
    pub slot: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetUsagesResult {
    pub usages: Vec<AssetUsageDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetMetadataParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetMetadataDto {
    pub id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: AssetTypeDto,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertex_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triangle_count: Option<u32>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteAssetParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeleteAssetResult {
    pub id: Uuid,
    pub name: String,
    pub cleared: Vec<AssetUsageDto>,
    pub file_deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssignAssetParams {
    pub entity: EntitySelector,
    pub slot: AssetSlotDto,
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssignAssetResult {
    pub id: Uuid,
    pub name: String,
    pub slot: AssetSlotDto,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScreenshotParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ScreenshotTargetDto>,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ScreenshotResult {
    pub target: ScreenshotTargetDto,
    pub path: String,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailParams {
    pub asset: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailResult {
    pub id: Uuid,
    pub format: ThumbnailFormatDto,
    pub width: i32,
    pub height: i32,
    pub base64: String,
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailCacheParams {
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ThumbnailCacheResult {
    pub entries: i32,
    pub bytes: i64,
}

/// What a model can do — a flat, additive capability struct read once when the asset editor
/// opens. A new capability appends a field; existing readers ignore unknown fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetCapabilitiesDto {
    pub mesh_count: i32,
    pub material_count: i32,
    pub node_count: i32,
    pub has_rig: bool,
    pub bone_count: i32,
    pub clip_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetAssetModelParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetModelResult {
    pub mesh: Uuid,
    pub name: String,
    pub capabilities: AssetCapabilitiesDto,
    pub bones: Vec<BoneDto>,
    pub clips: Vec<AnimationClipDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnterAssetPreviewParams {
    pub asset: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BoneEntityDto {
    pub index: i32,
    pub entity: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPreviewResult {
    pub root_entity: Uuid,
    pub bones: Vec<BoneEntityDto>,
    pub target: Vec3,
    pub distance: f32,
    /// The authored (variation, phenotype) combinations of a plant subject —
    /// the scrub domain for `set-asset-preview-options`; empty for every other
    /// subject kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plant_combinations: Vec<PlantCombinationDto>,
}

/// One authored assembly combination of a compiled plant family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PlantCombinationDto {
    pub variation: u32,
    pub phenotype: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAssetPreviewOptionsParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub floor: Option<bool>,
    /// Selects a plant subject's authored variation (with `phenotype`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variation: Option<u32>,
    /// Selects a plant subject's phenotype (with `variation`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phenotype: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AssetPreviewOptionsResult {
    pub floor: bool,
}
