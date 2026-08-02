use crate::{AssetAttributionDto, AssetSelector, EntitySelector, Uuid, Vec3, Vec4};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCreateParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCreateResult {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialAssignParams {
    pub entity: EntitySelector,
    pub material: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialAssignResult {
    pub material: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialImportParams {
    pub path: String,
    pub name: String,
    /// Optional store attribution, recorded on the imported material's catalog entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<AssetAttributionDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialImportResultDto {
    pub id: Uuid,
    pub roles: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialRefDto {
    pub id: Uuid,
    pub name: String,
    pub folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialListResult {
    pub materials: Vec<MaterialRefDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialGetParams {
    pub material: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialGetResult {
    pub id: Uuid,
    pub surface: crate::MaterialSurfaceDto,
    pub blend: String,
    pub unlit: bool,
    pub base_color: Vec4,
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: Vec3,
    pub emissive_strength: f32,
    pub height_scale: f32,
    /// The height-map technique: `bump` | `parallax` | `displacement`.
    pub height_mode: String,
    pub albedo_texture: Uuid,
    pub orm_texture: Uuid,
    pub normal_texture: Uuid,
    pub emissive_texture: Uuid,
    pub height_texture: Uuid,
    /// The tangent-space vector-displacement map (`0` = scalar-only along the normal).
    pub vector_displacement_texture: Uuid,
    pub graph: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSchemaParams {
    pub material: AssetSelector,
}

/// One exposed material parameter — its override key, type token, and default value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExposedParamDto {
    pub name: String,
    /// `scalar` | `color3` | `color4` | `vec2` | `bool` | `blend` | `texture`.
    pub kind: String,
    pub default: Value,
}

/// The exposed-parameter schema of a material — the keys a `MaterialSet` slot may override.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSchemaResult {
    pub params: Vec<ExposedParamDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialUpdateParams {
    pub material: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<crate::MaterialSurfaceDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_color: Option<Vec4>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metallic: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_scale: Option<f32>,
    /// The height-map technique: `bump` | `parallax` | `displacement`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albedo_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orm_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_texture: Option<Uuid>,
    /// The tangent-space vector-displacement map (`0` = scalar-only along the normal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_displacement_texture: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialUpdateResult {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetGraphParams {
    pub material: AssetSelector,
    pub graph: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetGraphResult {
    pub id: Uuid,
    pub foldable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCreateInstanceParams {
    pub parent: AssetSelector,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetOverrideParams {
    pub material: AssetSelector,
    pub field: String,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialSetOverrideResult {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCompileParams {
    pub material: AssetSelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCompileResult {
    pub id: Uuid,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MaterialCookResult {
    pub compiled: u32,
    pub failed: u32,
}
