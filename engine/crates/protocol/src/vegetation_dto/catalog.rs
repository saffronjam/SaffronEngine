use crate::{
    AssetTypeDto, PlantSourceKindDto, Uuid, VegetationCookStatisticsDto, VegetationGuid,
    VegetationLayerDto, VegetationManifestDependencyDto, VegetationSourceProvenanceDto,
    VegetationValidationSummaryDto, WorldBoundsDto,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

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
