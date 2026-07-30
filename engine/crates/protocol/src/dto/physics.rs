use super::coerce;
use crate::{EntitySelector, Uuid, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PhysicsStateResult {
    pub active: bool,
    pub body_count: i32,
    pub dynamic_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FitColliderParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FitColliderResult {
    pub entity: Uuid,
    pub shape: String,
    pub half_extents: Vec3,
    pub offset: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ContactEventDto {
    pub seq: i64,
    pub kind: String,
    /// One body's owner; absent when the body has no owner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_a: Option<WorldHitTargetDto>,
    /// The other body's owner; absent when none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_b: Option<WorldHitTargetDto>,
    pub sensor: bool,
    pub point: Vec3,
    pub normal: Vec3,
    pub tick: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainContactsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DrainContactsResult {
    pub events: Vec<ContactEventDto>,
    pub high_water_seq: i64,
    pub oldest_seq: i64,
    pub overflowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PhysicsBodyDto {
    /// The body's owner; absent when the body carried no owner identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<WorldHitTargetDto>,
    pub motion: String,
    pub active: bool,
    pub position: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PhysicsBodiesResult {
    pub bodies: Vec<PhysicsBodyDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyImpulseParams {
    pub entity: EntitySelector,
    pub impulse: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyImpulseResult {
    pub velocity: Vec3,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetKinematicBonesParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct KinematicBonesResult {
    pub entity: Uuid,
    pub enabled: bool,
    pub bone_count: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MoveCharacterParams {
    pub entity: EntitySelector,
    pub velocity: Vec3,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub jump: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MoveCharacterResult {
    pub position: Vec3,
    pub on_ground: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RaycastParams {
    pub origin: Vec3,
    pub dir: Vec3,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_dist: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ShapecastParams {
    pub origin: Vec3,
    pub dir: Vec3,
    pub radius: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_dist: Option<f32>,
}

/// The tagged owner a physics interaction resolves to on the wire: a scene entity by
/// stable uuid, or an authoritative macro plant by canonical identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[ts(export)]
pub enum WorldHitTargetDto {
    /// A hecs scene entity.
    #[serde(rename = "scene-entity")]
    SceneEntity {
        /// The entity's stable uuid.
        id: Uuid,
    },
    /// An authoritative macro plant.
    #[serde(rename = "vegetation")]
    Vegetation {
        /// The plant's canonical 32-hex identity.
        plant: crate::PlantId,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RaycastResult {
    pub hit: bool,
    /// The struck body's owner; absent on a miss or an unowned body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<WorldHitTargetDto>,
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnableRagdollParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RagdollResult {
    pub present: bool,
    pub active: bool,
    pub body_weight: f32,
    pub bones: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetRagdollParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_weight: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetRagdollParams {
    pub entity: EntitySelector,
}
