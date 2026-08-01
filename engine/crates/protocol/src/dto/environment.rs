use super::coerce;
use crate::{Uuid, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

/// The fog backend: the analytic closed form, or the froxel volumetric pipeline. In `Volumetric` the
/// analytic height density becomes the froxel base medium — it is never applied twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FogMode {
    Analytic,
    Volumetric,
}

/// The froxel-grid quality tier for volumetric fog: how many froxels the volume carries. Z is the
/// expensive axis, so `low`/`medium` share the Z count and only `high` doubles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum FogQuality {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetEnvironmentParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_mode: Option<crate::SkyModeDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_rotation: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub visible: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub use_sky_for_ambient: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient_color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient_intensity: Option<f32>,
}

/// A built-in complete environment profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum BuiltinEnvironmentProfileDto {
    Neutral,
    ClearDay,
    GoldenHour,
    Overcast,
    Night,
}

/// A typed reference to either a built-in or project environment profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[ts(export)]
pub enum EnvironmentProfileRefDto {
    Builtin {
        profile: BuiltinEnvironmentProfileDto,
    },
    Asset {
        id: Uuid,
    },
}

/// One environment profile shown in the profile browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnvironmentProfileSummaryDto {
    pub reference: EnvironmentProfileRefDto,
    pub name: String,
}

/// Every built-in and project environment profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EnvironmentProfileListDto {
    pub profiles: Vec<EnvironmentProfileSummaryDto>,
}

/// Params for saving the active environment as a new project profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SaveEnvironmentProfileParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// Params for replacing a project profile with the active environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct UpdateEnvironmentProfileParams {
    pub profile: Uuid,
}

/// Params for applying a complete environment profile to the active scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyEnvironmentProfileParams {
    pub profile: EnvironmentProfileRefDto,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetAtmosphereParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planet_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atmosphere_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rayleigh_scattering: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rayleigh_scale_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mie_scattering: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mie_scale_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mie_anisotropy: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ozone_absorption: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sun_disk_angular_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sun_disk_intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moon_disk_angular_radius: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moon_disk_intensity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moon_earthshine: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub per_pixel_transmittance: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky_capture_cadence: Option<f32>,
}

/// A partial merge onto `environment.fog` — the analytic height & distance fog. Each `Some` field
/// overwrites its key; `json` is an escape hatch merged first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetFogParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<FogMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<FogQuality>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_blend: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub neighborhood_clamp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_clamp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scatter_albedo: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_g: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albedo: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height_falloff: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_distance: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_opacity: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emissive: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directional_color: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directional_exponent: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer2_density: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer2_falloff: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer2_height: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub aerial_perspective: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aerial_intensity: Option<f32>,
}

/// A partial merge onto `environment.cloud`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetCloudsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_type: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precipitation: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anvil_bias: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_altitude: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layer_height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curl_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather_scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather_offset: Option<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weather_texture: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub light_steps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub droplet_diameter: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal_factor: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub cast_cloud_shadows: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_shadow_strength: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_shadow_on_surface_strength: Option<f32>,
}

/// A partial merge onto `environment.wind`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SampleWindParams {
    /// World position in metres.
    pub position_m: [f64; 3],
    /// Simulation seconds; the engine's monotonic clock when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_s: Option<f64>,
}

/// Params of `emit-interaction-impulse`: one world-space impulse into the
/// interaction field (a horizontal push disc with a smooth falloff).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EmitInteractionImpulseParams {
    /// World-space XZ centre in metres.
    pub position_m: [f64; 2],
    /// Falloff radius in metres.
    #[schemars(range(min = 0.01, max = 64.0))]
    pub radius_m: f64,
    /// Velocity change at the centre in metres per second.
    #[schemars(range(min = 0.0, max = 50.0))]
    pub strength: f64,
    /// Horizontal push direction; radial from the centre when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f64; 2]>,
    /// Ground-depression velocity change at the centre.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 10.0))]
    pub depress: Option<f64>,
}

/// Reply of `emit-interaction-impulse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct EmitInteractionImpulseResult {
    /// Whether the impulse was staged for the next frame.
    pub accepted: bool,
}

/// One composed wind sample: the global profile plus every enabled `WindSource`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct SampleWindResult {
    pub velocity_mps: [f32; 3],
    pub gust_front: f32,
    pub time_s: f64,
    /// The mean advection term, including its gust-front boost.
    pub mean_mps: [f32; 3],
    /// The turbulence term the octaves below sum to.
    pub turbulence_mps: [f32; 3],
    /// The turbulence spectrum: one entry per evaluated octave, coarsest first.
    pub octaves: Vec<WindOctaveDto>,
    /// What each enabled local source contributed here, in scene order.
    pub sources: Vec<WindSourceInfluenceDto>,
}

/// One turbulence octave's own share of a wind sample.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WindOctaveDto {
    /// Octave index, zero being the coarsest.
    pub octave: u32,
    /// The octave's spatial wavelength in metres; each is half the one before.
    pub wavelength_m: f32,
    /// The velocity this octave alone contributes.
    pub velocity_mps: [f32; 3],
}

/// Params of `wind-interaction-field`: one whole cascade of the world interaction field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WindInteractionFieldParams {
    /// Which cascade to read; zero is the fine one.
    #[serde(default)]
    #[schemars(range(min = 0, max = 1))]
    pub cascade: u32,
    /// Side length of the reduced grid; 32 when absent, 256 at most.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 256))]
    pub resolution: Option<u32>,
}

/// Reply of `wind-interaction-field`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export)]
pub struct WindInteractionFieldResult {
    /// The cascade this reduces.
    pub cascade: u32,
    /// Metres per texel of that cascade.
    pub texel_meters: f32,
    /// The cascade's centre in absolute world texel coordinates.
    pub center_texel: [i32; 2],
    /// The field's monotonic generation.
    pub generation: u32,
    /// Side length of the reduced grid.
    pub resolution: u32,
    /// Row-major block means of `[displacementX, displacementZ, depress]` in metres.
    pub cells: Vec<[f32; 3]>,
    /// Texels holding current, nonzero state — what the cascade is carrying right now.
    pub live_texels: u32,
    /// The largest horizontal displacement in the cascade, in metres.
    pub peak_displacement_m: f32,
    /// The largest horizontal recovery velocity in the cascade, in metres per second.
    pub peak_velocity_mps: f32,
}

/// One local wind source's contribution at a sampled position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WindSourceInfluenceDto {
    /// The source entity.
    pub entity: String,
    /// `directional`, `point`, `vortex`, `wake`, or `volume`.
    pub kind: String,
    /// Distance from the sample to the source in metres.
    pub distance_m: f32,
    /// The source's 0..1 edge weight at that distance; zero is out of range.
    pub weight: f32,
    /// The velocity this source adds. Zero for a volume source, which scales instead.
    pub added_mps: [f32; 3],
    /// The factor a volume source scales the global term by; one for every other kind.
    pub global_scale: f32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetWindParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gust: Option<f32>,
}

/// Master and per-channel time-of-day tint curves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TodTintCurveDto {
    pub master: Vec<[f32; 2]>,
    pub red: Vec<[f32; 2]>,
    pub green: Vec<[f32; 2]>,
    pub blue: Vec<[f32; 2]>,
}

/// A partial merge onto `environment.timeOfDay`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SetTimeOfDayParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub enabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "coerce::opt_boolean"
    )]
    pub manual_override: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_of_day: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day_length_seconds: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure_curve: Option<Vec<[f32; 2]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint_curve: Option<TodTintCurveDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage_curve: Option<Vec<[f32; 2]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_type_curve: Option<Vec<[f32; 2]>>,
}
