use crate::{Uuid, VegetationGuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// Imports instanced points from a digital-content-creation tool into an authored map layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationImportPointsParams {
    /// The authored vegetation map to commit into.
    pub map: crate::AssetSelector,
    /// The authored layer that owns the anchors.
    pub layer: VegetationGuid,
    /// Path to the point file. The extension picks the reader: a Houdini JSON `.geo` point cloud, a
    /// `.usda` stage carrying `PointInstancer` prims, or a `.gltf`/`.glb` carrying
    /// `EXT_mesh_gpu_instancing` nodes.
    pub path: String,
    /// Prototype name to plant-family catalog identity. An unnamed source uses the single entry.
    pub prototypes: Vec<VegetationPointPrototypeDto>,
    /// The map generation the caller last observed.
    pub expected_generation: String,
}

/// One prototype binding: the source's name for it, and the family it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationPointPrototypeDto {
    /// Source-facing prototype name.
    pub name: String,
    /// The plant family it resolves to.
    pub family: Uuid,
}

/// What one point import committed, and what it could not express.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationImportPointsResult {
    /// Anchors committed.
    pub anchors: u32,
    /// Tiles the anchors landed in.
    pub tiles: u32,
    /// Prototypes the source placed.
    pub prototypes: u32,
    /// Source attributes the canonical point vocabulary cannot express.
    pub unsupported: Vec<String>,
    /// The map generation after the commit.
    pub generation: String,
}

/// Exports one authored layer's anchors for a round trip through a content-creation tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationExportPointsParams {
    /// The authored vegetation map to read.
    pub map: crate::AssetSelector,
    /// The authored layer whose anchors to export.
    pub layer: VegetationGuid,
    /// Destination path. The extension picks the writer: `.geo` for a Houdini point cloud, `.usda`
    /// for a USD `PointInstancer`.
    pub path: String,
}

/// What one point export wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationExportPointsResult {
    /// Instances written.
    pub instances: u32,
    /// Prototypes written.
    pub prototypes: u32,
    /// The file that was written.
    pub path: String,
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

/// Which USD stage to read skeletons from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct UsdSkeletonsParams {
    /// Path to a `.usda`/`.usd` text stage.
    pub path: String,
}

/// The `UsdSkel` skeletons one stage declares.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UsdSkeletonsResult {
    pub skeletons: Vec<UsdSkeletonDto>,
    /// Skeleton attributes the vocabulary cannot express, reported rather than dropped.
    pub unsupported: Vec<String>,
}

/// One `UsdSkel` skeleton.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UsdSkeletonDto {
    /// The `Skeleton` prim's name.
    pub name: String,
    /// The enclosing `SkelRoot`, absent when the skeleton sits outside one — which binds nothing.
    pub skel_root: Option<String>,
    pub joints: Vec<UsdJointDto>,
}

/// One joint of a `UsdSkel` skeleton.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UsdJointDto {
    /// The joint's full path token, as authored.
    pub path: String,
    /// Index of the parent joint, derived from the path hierarchy; absent for a root.
    pub parent: Option<u32>,
    /// Rest transform, row-major.
    pub rest: Vec<f64>,
    /// World-space bind transform, row-major.
    pub bind: Vec<f64>,
}
