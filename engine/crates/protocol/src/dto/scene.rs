use super::coerce;
use crate::{EntityRef, EntitySelector, Uuid, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// The `add-entity` preset selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum AddEntityPreset {
    Empty,
    Cube,
    Plane,
    Sphere,
    PointLight,
    SpotLight,
    DirectionalLight,
    Camera,
    ReflectionProbe,
    FogVolume,
}

/// What a viewport pick resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PickKind {
    Billboard,
    Mesh,
    Vegetation,
    MicroVegetation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CreateEntityParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetParentParams {
    pub entity: EntitySelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<EntitySelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DestroyEntityResult {
    pub destroyed: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityListEntry {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EntityList {
    pub entities: Vec<EntityListEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ComponentList {
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ComponentParams {
    pub entity: EntitySelector,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddComponentResult {
    pub added: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RemoveComponentResult {
    pub removed: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentParams {
    pub entity: EntitySelector,
    pub component: String,
    #[schemars(with = "crate::ComponentBody")]
    #[ts(type = "ComponentBody")]
    pub json: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentResult {
    pub set: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentOrderParams {
    pub entity: EntitySelector,
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentOrderResult {
    pub components: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTransformParams {
    pub entity: EntitySelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<Vec3>,
    /// Animate the fields toward the given values over ~25ms instead of snapping
    /// (ignored when preserve-children must rebase the subtree on each write).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub smooth: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetLightParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntitySelector>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickResult {
    pub hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<PickKind>,
    /// The stable plant identity when a macro plant is the nearest hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plant: Option<crate::PlantId>,
    /// The world-space hit position in metres (a mesh surface or micro-vegetation
    /// ground hit; absent for billboards and misses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 3]>,
    /// The geometric surface normal at a mesh hit (absent for billboards, plants,
    /// micro hits, and misses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<[f32; 3]>,
}

/// Parameters for one explicit surface-ray query (metres; the direction normalizes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct QuerySurfaceRayParams {
    pub origin_m: [f64; 3],
    pub direction: [f32; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_distance_m: Option<f64>,
}

/// Result of one explicit surface-ray query: the nearest scene-surface hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SurfaceRayResult {
    pub hit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<[f32; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InspectResult {
    pub id: Uuid,
    pub name: String,
    #[schemars(with = "crate::Components")]
    #[ts(type = "Components")]
    pub components: Value,
    pub component_order: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SelectionResult {
    pub selection_version: i32,
    pub scene_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<EntityRef>,
    pub play_state: String,
    pub play_version: i32,
    pub animation_version: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlayStateResult {
    pub state: String,
    pub play_version: i32,
    pub scene_version: i32,
    pub has_primary_camera: bool,
    pub animation_version: i32,
    pub preview_asset: Uuid,
}

/// An entity's composed world-space transform (the cached WorldTransformComponent), so a
/// caller can read a bone's world position — e.g. to verify foot IK plants on the ground.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WorldTransformResult {
    pub translation: Vec3,
    pub scale: Vec3,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StepParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frames: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DeselectResult {
    pub selection_version: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AddEntityParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<AddEntityPreset>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenameEntityParams {
    pub entity: EntitySelector,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentFieldParams {
    pub entity: EntitySelector,
    pub component: String,
    pub field: String,
    pub value: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetComponentFieldResult {
    pub set: String,
    pub field: String,
}
