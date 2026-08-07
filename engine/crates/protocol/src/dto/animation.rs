use super::coerce;
use crate::{AssetSelector, EntitySelector, Uuid};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One animation channel (track) of a clip, carrying enough to draw a real per-channel
/// keyframe strip. The editor draws one strip per channel keyed on `times`, independent of
/// `width`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationChannelDto {
    /// `"node-translation" | "node-rotation" | "node-scale" | "morph-weights" | "bone"` — a
    /// plain wire string; serde/ts-rs handle it natively, no enum DTO row.
    pub kind: String,
    /// The display label: the resolved entity name for a node/bone binding (the raw glTF node
    /// name when the binding is unresolved — which doubles as the broken-binding signal), and
    /// the raw glTF target name for a morph-weights channel.
    pub label: String,
    /// The raw glTF binding key (node name, or morph target name) — durable, what the runtime
    /// binds on. Distinct from `label` so the editor can show the friendly name yet key on it.
    pub target_name: String,
    /// The keyframe sample times in seconds, ascending — the strip's tick positions.
    pub times: Vec<f32>,
    /// Value components per keyframe, so `values.len() == times.len() * width`: `3` for
    /// translation/scale, `4` for a rotation quaternion, `morph_count` for a morph channel.
    pub width: i32,
    /// The per-keyframe values, row-major `times.len() * width`. Translation/scale rows are
    /// `xyz`, rotation rows are quaternion `xyzw`, morph rows are the N weights.
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationClipDto {
    pub id: Uuid,
    pub name: String,
    pub duration: f32,
    /// One entry per track in the clip — the editor renders a keyframe strip per channel.
    pub channels: Vec<AnimationChannelDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BoneDto {
    pub index: i32,
    pub name: String,
    pub parent: i32,
    pub joint: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListClipsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset: Option<AssetSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListClipsResult {
    pub clips: Vec<AnimationClipDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PlayAnimationParams {
    pub entity: EntitySelector,
    pub clip: AssetSelector,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#loop: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub paused: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SeekAnimationParams {
    pub entity: EntitySelector,
    pub time: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seek_blend: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAnimationLoopParams {
    pub entity: EntitySelector,
    pub wrap: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAnimationPlayingParams {
    pub entity: EntitySelector,
    #[serde(deserialize_with = "coerce::boolean")]
    pub playing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationStateParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AnimationStateResult {
    pub clip: Uuid,
    pub clip_name: String,
    pub duration: f32,
    pub time: f32,
    pub playing: bool,
    pub wrap: String,
    pub speed: f32,
    pub animation_version: i32,
    /// The target's live morph weights (canonical 0..1) — always present, empty when the
    /// target has no morph mesh. The runtime override if a preview is live, else the durable
    /// component's rest weights.
    pub morph_weights: Vec<f32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkeletonOverlayParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub show: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub axes: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joint_size: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SkeletonOverlayResult {
    pub show: bool,
    pub axes: bool,
    pub joint_size: f32,
    pub highlight_joint: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DebugOverlaysParams {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub bounds: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub scene_aabb: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub light_volumes: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub grid: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub colliders: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub vegetation_cells: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub vegetation_bounds: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub vegetation_rejections: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub vegetation_heatmap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vegetation_navigation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wind_vectors: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DebugOverlaysResult {
    pub bounds: bool,
    pub scene_aabb: bool,
    pub light_volumes: bool,
    pub grid: bool,
    pub colliders: bool,
    pub vegetation_cells: bool,
    pub vegetation_bounds: bool,
    pub vegetation_rejections: bool,
    pub vegetation_heatmap: bool,
    pub vegetation_navigation: bool,
    pub wind_vectors: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetSkeletonHighlightParams {
    pub joint: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickSkeletonJointParams {
    pub u: f32,
    pub v: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius_px: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PickSkeletonJointResult {
    pub found: bool,
    pub node_index: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetFootIkParams {
    pub entity: EntitySelector,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ground_height: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetFootIkParams {
    pub entity: EntitySelector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FootIkResult {
    pub enabled: bool,
    pub ground_height: f32,
    pub chains: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetMorphWeightsParams {
    pub entity: EntitySelector,
    /// The morph-target weights (canonical 0..1); the length must equal the target's morph
    /// count.
    pub weights: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct GetMorphWeightsParams {
    pub entity: EntitySelector,
}

/// The live morph weights + the durable target names, shared by `set-`/`get-morph-weights`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct MorphWeightsResult {
    pub weights: Vec<f32>,
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ListClipBindingsParams {
    pub entity: EntitySelector,
    pub clip: AssetSelector,
}

/// A clip's channels resolved against a live entity forest — an unresolved channel surfaces
/// as a broken binding via its `label`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ClipBindingsResult {
    pub channels: Vec<AnimationChannelDto>,
}
