//! Typed scene-component and environment wire DTOs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::{FogMode, FogQuality, Uuid, Vec3};

/// A serialized entity name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Name {
    pub name: String,
}

/// A serialized local transform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Transform {
    pub translation: Vec3,
    pub scale: Vec3,
    pub rotation: Vec3,
}

/// A serialized mesh reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Mesh {
    pub mesh: Uuid,
}

/// The scene's single serialized vegetation-world binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct VegetationField {
    pub map: Uuid,
    pub enabled: bool,
}

/// A serialized perspective camera.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Camera {
    pub fov: f32,
    pub near: f32,
    pub far: f32,
    pub primary: bool,
    pub show_model: bool,
    pub show_frustum: bool,
    pub frustum_max_distance: f32,
}

/// One serialized material binding and its sparse overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MaterialSlot {
    pub material: Uuid,
    #[schemars(with = "std::collections::BTreeMap<String, Value>")]
    #[ts(type = "Record<string, unknown>")]
    pub overrides: Value,
}

/// An entity's serialized submesh material bindings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct MaterialSet {
    pub slots: Vec<MaterialSlot>,
}

/// A serialized model-instance marker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ModelInstance {
    pub model_id: Uuid,
}

/// One serialized script slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ScriptSlot {
    pub script_path: String,
    #[schemars(with = "std::collections::BTreeMap<String, Value>")]
    #[ts(type = "Record<string, unknown>")]
    pub overrides: Value,
}

/// An entity's ordered serialized scripts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Script {
    pub scripts: Vec<ScriptSlot>,
}

/// How an animation clip wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AnimationWrapDto {
    Once,
    Loop,
    PingPong,
}

/// How an animation clip transition blends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AnimationTransitionDto {
    Inertialize,
    CrossFade,
}

/// A serialized animation player.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AnimationPlayer {
    pub clip: Uuid,
    pub autoplay: bool,
    pub speed: f32,
    pub wrap: AnimationWrapDto,
    pub transition_mode: AnimationTransitionDto,
    pub loop_blend: f32,
}

/// A directional light's celestial role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum AtmosphereRoleDto {
    Sun,
    Moon,
}

/// A serialized directional light.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct DirectionalLight {
    pub atmosphere_role: AtmosphereRoleDto,
    pub direction: Vec3,
    pub color: Vec3,
    pub intensity: f32,
    pub ambient: f32,
    pub volumetric_scattering: f32,
    pub cast_volumetric_shadow: bool,
}

/// A serialized point light.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PointLight {
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub volumetric_scattering: f32,
    pub cast_volumetric_shadow: bool,
}

/// A serialized spot light.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SpotLight {
    pub direction: Vec3,
    pub color: Vec3,
    pub intensity: f32,
    pub range: f32,
    pub inner_angle: f32,
    pub outer_angle: f32,
    pub volumetric_scattering: f32,
    pub cast_volumetric_shadow: bool,
}

/// A serialized local reflection probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct ReflectionProbe {
    pub influence_radius: f32,
    pub intensity: f32,
    pub box_projection: bool,
    pub box_extent: Vec3,
}

/// A local fog volume's bounds primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum FogShapeDto {
    Box,
    Sphere,
}

/// A serialized local fog volume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FogVolume {
    pub shape: FogShapeDto,
    pub extents: Vec3,
    pub radius: f32,
    pub edge_falloff: f32,
    pub density: f32,
    pub albedo: Vec3,
    pub emissive: Vec3,
    pub phase_g: f32,
    pub height_falloff: f32,
    pub noise_scale: f32,
    pub noise_intensity: f32,
    pub noise_detail: f32,
    pub wind: Vec3,
    pub speed: f32,
}

/// A serialized hierarchy parent reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Relationship {
    pub parent: Uuid,
}

/// A serialized skinned-mesh binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SkinnedMesh {
    pub mesh: Uuid,
    pub root_bone: Uuid,
    pub bones: Vec<Uuid>,
    #[ts(type = "number[][]")]
    pub inverse_bind: Vec<[f32; 16]>,
}

/// Durable morph-target weights and names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Morph {
    pub weights: Vec<f32>,
    pub names: Vec<String>,
}

/// A serialized bone marker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Bone {}

/// One serialized foot-IK chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FootChainDto {
    pub upper: i32,
    pub mid: i32,
    pub end: i32,
    pub pole_vector: Vec3,
}

/// Serialized foot-IK settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FootIk {
    pub enabled: bool,
    pub ground_height: f32,
    pub chains: Vec<FootChainDto>,
}

/// A ragdoll bone's joint kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum JointDto {
    Fixed,
    Hinge,
    SwingTwist,
    Free,
}

/// One serialized ragdoll bone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BonePhysicsDto {
    pub shape_half_extents: Vec3,
    pub mass: f32,
    pub joint: JointDto,
    pub swing_twist_limits: Vec3,
    pub drive_stiffness: f32,
    pub drive_damping: f32,
    pub drive_max_force: f32,
}

/// Serialized ragdoll physics settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BonePhysics {
    pub bones: Vec<BonePhysicsDto>,
}

/// A named boolean vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct BVec3 {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

/// A rigid body's motion type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum MotionDto {
    Static,
    Kinematic,
    Dynamic,
}

/// A serialized rigid body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Rigidbody {
    pub motion: MotionDto,
    pub mass: f32,
    pub linear_damping: f32,
    pub angular_damping: f32,
    pub gravity_factor: f32,
    pub lock_position: BVec3,
    pub lock_rotation: BVec3,
    pub collision_layer: i32,
}

/// A collider's bounds primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ColliderShapeDto {
    Box,
    Sphere,
    Capsule,
    ConvexHull,
    Mesh,
}

/// Serialized contact-material coefficients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct PhysicsMaterial {
    pub friction: f32,
    pub restitution: f32,
}

/// A serialized collider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct Collider {
    pub shape: ColliderShapeDto,
    pub half_extents: Vec3,
    pub source_mesh: Uuid,
    pub offset: Vec3,
    pub material: PhysicsMaterial,
    pub is_sensor: bool,
}

/// Serialized kinematic-bone settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct KinematicBones {
    pub enabled: bool,
    pub driven: Vec<i32>,
}

/// Serialized virtual-character settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CharacterController {
    pub max_speed: f32,
    pub max_slope_angle: f32,
    pub max_step_height: f32,
    pub gravity_factor: f32,
}

/// Every serialized built-in component keyed by its registry name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default, rename_all = "PascalCase", deny_unknown_fields)]
#[ts(export)]
pub struct Components {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<Name>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<Mesh>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vegetation_field: Option<VegetationField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material_set: Option<MaterialSet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_instance: Option<ModelInstance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script: Option<Script>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_player: Option<AnimationPlayer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directional_light: Option<DirectionalLight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub point_light: Option<PointLight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spot_light: Option<SpotLight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflection_probe: Option<ReflectionProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fog_volume: Option<FogVolume>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relationship: Option<Relationship>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skinned_mesh: Option<SkinnedMesh>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub morph: Option<Morph>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone: Option<Bone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foot_ik: Option<FootIk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bone_physics: Option<BonePhysics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rigidbody: Option<Rigidbody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collider: Option<Collider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinematic_bones: Option<KinematicBones>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character_controller: Option<CharacterController>,
}

/// Any complete serialized built-in component body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(untagged)]
#[ts(export)]
pub enum ComponentBody {
    Name(Name),
    Transform(Transform),
    Mesh(Mesh),
    VegetationField(VegetationField),
    Camera(Camera),
    MaterialSet(MaterialSet),
    ModelInstance(ModelInstance),
    Script(Script),
    AnimationPlayer(AnimationPlayer),
    DirectionalLight(DirectionalLight),
    PointLight(PointLight),
    SpotLight(SpotLight),
    ReflectionProbe(ReflectionProbe),
    FogVolume(FogVolume),
    Relationship(Relationship),
    SkinnedMesh(SkinnedMesh),
    Morph(Morph),
    Bone(Bone),
    FootIk(FootIk),
    BonePhysics(BonePhysics),
    Rigidbody(Rigidbody),
    Collider(Collider),
    KinematicBones(KinematicBones),
    CharacterController(CharacterController),
}

/// How the visible sky background is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SkyModeDto {
    Color,
    Texture,
    Procedural,
}

/// Physically based atmosphere settings on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct AtmosphereSettingsDto {
    pub enabled: bool,
    pub planet_radius: f32,
    pub atmosphere_height: f32,
    pub rayleigh_scattering: Vec3,
    pub rayleigh_scale_height: f32,
    pub mie_scattering: f32,
    pub mie_scale_height: f32,
    pub mie_anisotropy: f32,
    pub ozone_absorption: Vec3,
    pub sun_disk_angular_radius: f32,
    pub sun_disk_intensity: f32,
    pub moon_disk_angular_radius: f32,
    pub moon_disk_intensity: f32,
    pub moon_earthshine: f32,
    pub per_pixel_transmittance: bool,
    #[schemars(range(min = 1.0, max = 60.0))]
    pub sky_capture_cadence: f32,
}

/// Fog settings on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct FogSettingsDto {
    pub enabled: bool,
    pub mode: FogMode,
    pub quality: FogQuality,
    pub history_blend: f32,
    pub neighborhood_clamp: bool,
    pub light_clamp: f32,
    pub base_density: f32,
    pub scatter_albedo: f32,
    pub phase_g: f32,
    pub density: f32,
    pub albedo: Vec3,
    pub height: f32,
    pub height_falloff: f32,
    pub start_distance: f32,
    pub max_opacity: f32,
    pub emissive: Vec3,
    pub directional_color: Vec3,
    pub directional_exponent: f32,
    pub layer2_density: f32,
    pub layer2_falloff: f32,
    pub layer2_height: f32,
    pub aerial_perspective: bool,
    pub aerial_intensity: f32,
}

/// Volumetric cloud shape, lighting, and reconstruction settings on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct CloudSettingsDto {
    pub enabled: bool,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub coverage: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub cloud_type: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub precipitation: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub anvil_bias: f32,
    pub layer_altitude: f32,
    #[schemars(range(min = 0.0))]
    pub layer_height: f32,
    #[schemars(range(min = 0.0))]
    pub base_scale: f32,
    #[schemars(range(min = 0.0))]
    pub detail_scale: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub detail_strength: f32,
    #[schemars(range(min = 0.0))]
    pub curl_strength: f32,
    #[schemars(range(min = 0.0))]
    pub weather_scale: f32,
    pub weather_offset: Vec3,
    pub weather_texture: Uuid,
    #[schemars(range(min = 1))]
    pub primary_steps: u32,
    #[schemars(range(min = 1))]
    pub light_steps: u32,
    #[schemars(range(min = 5.0, max = 50.0))]
    pub droplet_diameter: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub temporal_factor: f32,
    pub cast_cloud_shadows: bool,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub cloud_shadow_strength: f32,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub cloud_shadow_on_surface_strength: f32,
}

/// Shared global wind settings on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WindSettingsDto {
    pub orientation: f32,
    #[schemars(range(min = 0.0))]
    pub speed: f32,
    #[schemars(range(min = 0.0))]
    pub gust: f32,
}

/// One time-of-day curve control point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TodCurvePointDto {
    #[schemars(range(min = 0.0, max = 1.0))]
    pub x: f32,
    pub y: f32,
}

/// Master and per-channel time-of-day tint curves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TodTintSettingsDto {
    pub master: Vec<TodCurvePointDto>,
    pub red: Vec<TodCurvePointDto>,
    pub green: Vec<TodCurvePointDto>,
    pub blue: Vec<TodCurvePointDto>,
}

/// Calendar, location, playback, and appearance settings on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct TimeOfDaySettingsDto {
    pub enabled: bool,
    pub manual_override: bool,
    #[schemars(range(min = 0.0, max = 1.0))]
    pub time_of_day: f32,
    pub year: i32,
    #[schemars(range(min = 1, max = 12))]
    pub month: i32,
    #[schemars(range(min = 1, max = 31))]
    pub day: i32,
    #[schemars(range(min = -90.0, max = 90.0))]
    pub latitude: f32,
    #[schemars(range(min = -180.0, max = 180.0))]
    pub longitude: f32,
    #[schemars(range(min = 0.0))]
    pub day_length_seconds: f32,
    pub exposure_curve: Vec<TodCurvePointDto>,
    pub tint_curve: TodTintSettingsDto,
    pub coverage_curve: Vec<TodCurvePointDto>,
    pub cloud_type_curve: Vec<TodCurvePointDto>,
}

/// The complete scene environment block on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EnvironmentDto {
    pub sky_mode: SkyModeDto,
    pub clear_color: Vec3,
    pub sky_texture: Uuid,
    pub sky_intensity: f32,
    pub sky_rotation: f32,
    pub exposure: f32,
    pub visible: bool,
    pub use_sky_for_ambient: bool,
    pub ambient_color: Vec3,
    pub ambient_intensity: f32,
    pub atmosphere: AtmosphereSettingsDto,
    pub fog: FogSettingsDto,
    pub cloud: CloudSettingsDto,
    pub wind: WindSettingsDto,
    pub time_of_day: TimeOfDaySettingsDto,
}

/// The registered component names in registry order.
pub const COMPONENT_NAMES: &[&str] = &[
    "Name",
    "Transform",
    "Mesh",
    "VegetationField",
    "Camera",
    "MaterialSet",
    "ModelInstance",
    "Script",
    "AnimationPlayer",
    "DirectionalLight",
    "PointLight",
    "SpotLight",
    "ReflectionProbe",
    "FogVolume",
    "Relationship",
    "SkinnedMesh",
    "Morph",
    "Bone",
    "FootIk",
    "BonePhysics",
    "Rigidbody",
    "Collider",
    "KinematicBones",
    "CharacterController",
];
