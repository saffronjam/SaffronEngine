//! The [`SceneEnvironment`] block serde: atmosphere, fog, clouds, wind, and time of day.

use serde_json::Value;

use saffron_core::Uuid;
use saffron_json::{json_bool_or, json_f32_or, json_string_or, json_u64_or, uuid_to_json};

use super::{f32_value, field, json_i32_or, object, vec3_from_json, vec3_to_json};
use crate::environment::{
    AtmosphereSettings, CloudSettings, FogMode, FogQuality, FogSettings, SceneEnvironment, SkyMode,
    TimeOfDaySettings, TodCurve, TodTintCurve, WindSettings,
};

/// The lowercase wire name for a [`SkyMode`].
fn sky_mode_name(mode: SkyMode) -> &'static str {
    match mode {
        SkyMode::Color => "color",
        SkyMode::Texture => "texture",
        SkyMode::Procedural => "procedural",
    }
}

/// The wire name for a [`FogMode`].
fn fog_mode_name(mode: FogMode) -> &'static str {
    match mode {
        FogMode::Analytic => "analytic",
        FogMode::Volumetric => "volumetric",
    }
}

/// The wire name for a [`FogQuality`].
fn fog_quality_name(quality: FogQuality) -> &'static str {
    match quality {
        FogQuality::Low => "low",
        FogQuality::Medium => "medium",
        FogQuality::High => "high",
    }
}

/// Reads a [`FogQuality`] from its wire name, defaulting to `Medium` on an unknown spelling.
fn fog_quality_from_name(name: &str) -> FogQuality {
    match name {
        "low" => FogQuality::Low,
        "high" => FogQuality::High,
        _ => FogQuality::Medium,
    }
}

/// Reads a [`FogMode`] from its wire name, warning and defaulting to `Analytic` on an unknown spelling.
fn fog_mode_from_name(name: &str) -> FogMode {
    match name {
        "analytic" => FogMode::Analytic,
        "volumetric" => FogMode::Volumetric,
        other => {
            tracing::warn!("unknown fog mode '{other}', defaulting to analytic");
            FogMode::Analytic
        }
    }
}

/// Reads a [`SkyMode`] from its wire name, warning and defaulting to `Procedural` on an
/// unknown spelling.
fn sky_mode_from_name(name: &str) -> SkyMode {
    match name {
        "color" => SkyMode::Color,
        "texture" => SkyMode::Texture,
        "procedural" => SkyMode::Procedural,
        other => {
            tracing::warn!("unknown sky mode '{other}', defaulting to procedural");
            SkyMode::Procedural
        }
    }
}

/// The `AtmosphereSettings` block, nested inside the environment.
fn atmosphere_to_json(a: &AtmosphereSettings) -> Value {
    object([
        ("enabled", Value::Bool(a.enabled)),
        ("planetRadius", f32_value(a.planet_radius)),
        ("atmosphereHeight", f32_value(a.atmosphere_height)),
        ("rayleighScattering", vec3_to_json(a.rayleigh_scattering)),
        ("rayleighScaleHeight", f32_value(a.rayleigh_scale_height)),
        ("mieScattering", f32_value(a.mie_scattering)),
        ("mieScaleHeight", f32_value(a.mie_scale_height)),
        ("mieAnisotropy", f32_value(a.mie_anisotropy)),
        ("ozoneAbsorption", vec3_to_json(a.ozone_absorption)),
        ("sunDiskAngularRadius", f32_value(a.sun_disk_angular_radius)),
        ("sunDiskIntensity", f32_value(a.sun_disk_intensity)),
        (
            "moonDiskAngularRadius",
            f32_value(a.moon_disk_angular_radius),
        ),
        ("moonDiskIntensity", f32_value(a.moon_disk_intensity)),
        ("moonEarthshine", f32_value(a.moon_earthshine)),
        (
            "perPixelTransmittance",
            Value::Bool(a.per_pixel_transmittance),
        ),
        ("skyCaptureCadence", f32_value(a.sky_capture_cadence)),
    ])
}

/// Reads an [`AtmosphereSettings`] block, defaulting per field and leaving the vector
/// fields at their struct defaults when absent.
fn atmosphere_from_json(j: &Value) -> AtmosphereSettings {
    let mut a = AtmosphereSettings::default();
    if !j.is_object() {
        return a;
    }
    a.enabled = json_bool_or(j, "enabled", false);
    a.planet_radius = json_f32_or(j, "planetRadius", 6360.0);
    a.atmosphere_height = json_f32_or(j, "atmosphereHeight", 100.0);
    if let Some(v) = field(j, "rayleighScattering") {
        a.rayleigh_scattering = vec3_from_json(v);
    }
    a.rayleigh_scale_height = json_f32_or(j, "rayleighScaleHeight", 8.0);
    a.mie_scattering = json_f32_or(j, "mieScattering", 3.996);
    a.mie_scale_height = json_f32_or(j, "mieScaleHeight", 1.2);
    a.mie_anisotropy = json_f32_or(j, "mieAnisotropy", 0.8);
    if let Some(v) = field(j, "ozoneAbsorption") {
        a.ozone_absorption = vec3_from_json(v);
    }
    a.sun_disk_angular_radius = json_f32_or(j, "sunDiskAngularRadius", 0.00465);
    a.sun_disk_intensity = json_f32_or(j, "sunDiskIntensity", 1.0);
    a.moon_disk_angular_radius = json_f32_or(j, "moonDiskAngularRadius", 0.00496);
    a.moon_disk_intensity = json_f32_or(j, "moonDiskIntensity", 1.0);
    a.moon_earthshine = json_f32_or(j, "moonEarthshine", 0.02);
    a.per_pixel_transmittance = json_bool_or(j, "perPixelTransmittance", false);
    a.sky_capture_cadence = json_f32_or(j, "skyCaptureCadence", 9.0).clamp(1.0, 60.0);
    a
}

/// The `FogSettings` block, nested inside the environment.
fn fog_to_json(f: &FogSettings) -> Value {
    object([
        ("enabled", Value::Bool(f.enabled)),
        ("mode", Value::String(fog_mode_name(f.mode).to_string())),
        (
            "quality",
            Value::String(fog_quality_name(f.quality).to_string()),
        ),
        ("historyBlend", f32_value(f.history_blend)),
        ("neighborhoodClamp", Value::Bool(f.neighborhood_clamp)),
        ("lightClamp", f32_value(f.light_clamp)),
        ("baseDensity", f32_value(f.base_density)),
        ("scatterAlbedo", f32_value(f.scatter_albedo)),
        ("phaseG", f32_value(f.phase_g)),
        ("density", f32_value(f.density)),
        ("albedo", vec3_to_json(f.albedo)),
        ("height", f32_value(f.height)),
        ("heightFalloff", f32_value(f.height_falloff)),
        ("startDistance", f32_value(f.start_distance)),
        ("maxOpacity", f32_value(f.max_opacity)),
        ("emissive", vec3_to_json(f.emissive)),
        ("directionalColor", vec3_to_json(f.directional_color)),
        ("directionalExponent", f32_value(f.directional_exponent)),
        ("layer2Density", f32_value(f.layer2_density)),
        ("layer2Falloff", f32_value(f.layer2_falloff)),
        ("layer2Height", f32_value(f.layer2_height)),
        ("aerialPerspective", Value::Bool(f.aerial_perspective)),
        ("aerialIntensity", f32_value(f.aerial_intensity)),
    ])
}

/// Reads a [`FogSettings`] block, defaulting per field and leaving the vector fields at
/// their struct defaults when absent.
fn fog_from_json(j: &Value) -> FogSettings {
    let mut f = FogSettings::default();
    if !j.is_object() {
        return f;
    }
    f.enabled = json_bool_or(j, "enabled", false);
    if let Some(Value::String(s)) = field(j, "mode") {
        f.mode = fog_mode_from_name(s);
    }
    if let Some(Value::String(s)) = field(j, "quality") {
        f.quality = fog_quality_from_name(s);
    }
    f.history_blend = json_f32_or(j, "historyBlend", 0.05);
    f.neighborhood_clamp = json_bool_or(j, "neighborhoodClamp", false);
    f.light_clamp = json_f32_or(j, "lightClamp", 0.0);
    f.base_density = json_f32_or(j, "baseDensity", 0.02);
    f.scatter_albedo = json_f32_or(j, "scatterAlbedo", 0.9);
    f.phase_g = json_f32_or(j, "phaseG", 0.6);
    f.density = json_f32_or(j, "density", 0.02);
    if let Some(v) = field(j, "albedo") {
        f.albedo = vec3_from_json(v);
    }
    f.height = json_f32_or(j, "height", 0.0);
    f.height_falloff = json_f32_or(j, "heightFalloff", 0.2);
    f.start_distance = json_f32_or(j, "startDistance", 0.0);
    f.max_opacity = json_f32_or(j, "maxOpacity", 1.0);
    if let Some(v) = field(j, "emissive") {
        f.emissive = vec3_from_json(v);
    }
    if let Some(v) = field(j, "directionalColor") {
        f.directional_color = vec3_from_json(v);
    }
    f.directional_exponent = json_f32_or(j, "directionalExponent", 8.0);
    f.layer2_density = json_f32_or(j, "layer2Density", 0.0);
    f.layer2_falloff = json_f32_or(j, "layer2Falloff", 0.5);
    f.layer2_height = json_f32_or(j, "layer2Height", 0.0);
    f.aerial_perspective = json_bool_or(j, "aerialPerspective", false);
    f.aerial_intensity = json_f32_or(j, "aerialIntensity", 1.0);
    f
}

fn cloud_to_json(cloud: &CloudSettings) -> Value {
    object([
        ("enabled", Value::Bool(cloud.enabled)),
        ("coverage", f32_value(cloud.coverage)),
        ("cloudType", f32_value(cloud.cloud_type)),
        ("precipitation", f32_value(cloud.precipitation)),
        ("anvilBias", f32_value(cloud.anvil_bias)),
        ("layerAltitude", f32_value(cloud.layer_altitude)),
        ("layerHeight", f32_value(cloud.layer_height)),
        ("baseScale", f32_value(cloud.base_scale)),
        ("detailScale", f32_value(cloud.detail_scale)),
        ("detailStrength", f32_value(cloud.detail_strength)),
        ("curlStrength", f32_value(cloud.curl_strength)),
        ("weatherScale", f32_value(cloud.weather_scale)),
        ("weatherOffset", vec3_to_json(cloud.weather_offset)),
        (
            "weatherTexture",
            uuid_to_json(cloud.weather_texture.value()),
        ),
        ("primarySteps", Value::from(cloud.primary_steps)),
        ("lightSteps", Value::from(cloud.light_steps)),
        ("dropletDiameter", f32_value(cloud.droplet_diameter)),
        ("temporalFactor", f32_value(cloud.temporal_factor)),
        ("castCloudShadows", Value::Bool(cloud.cast_cloud_shadows)),
        (
            "cloudShadowStrength",
            f32_value(cloud.cloud_shadow_strength),
        ),
        (
            "cloudShadowOnSurfaceStrength",
            f32_value(cloud.cloud_shadow_on_surface_strength),
        ),
    ])
}

fn cloud_from_json(value: &Value) -> CloudSettings {
    let mut cloud = CloudSettings::default();
    if !value.is_object() {
        return cloud;
    }
    cloud.enabled = json_bool_or(value, "enabled", cloud.enabled);
    cloud.coverage = json_f32_or(value, "coverage", cloud.coverage);
    cloud.cloud_type = json_f32_or(value, "cloudType", cloud.cloud_type);
    cloud.precipitation = json_f32_or(value, "precipitation", cloud.precipitation);
    cloud.anvil_bias = json_f32_or(value, "anvilBias", cloud.anvil_bias);
    cloud.layer_altitude = json_f32_or(value, "layerAltitude", cloud.layer_altitude);
    cloud.layer_height = json_f32_or(value, "layerHeight", cloud.layer_height);
    cloud.base_scale = json_f32_or(value, "baseScale", cloud.base_scale);
    cloud.detail_scale = json_f32_or(value, "detailScale", cloud.detail_scale);
    cloud.detail_strength = json_f32_or(value, "detailStrength", cloud.detail_strength);
    cloud.curl_strength = json_f32_or(value, "curlStrength", cloud.curl_strength);
    cloud.weather_scale = json_f32_or(value, "weatherScale", cloud.weather_scale);
    if let Some(offset) = field(value, "weatherOffset") {
        cloud.weather_offset = vec3_from_json(offset);
    }
    cloud.weather_texture = Uuid(json_u64_or(
        value,
        "weatherTexture",
        cloud.weather_texture.value(),
    ));
    cloud.primary_steps = json_u64_or(value, "primarySteps", u64::from(cloud.primary_steps)) as u32;
    cloud.light_steps = json_u64_or(value, "lightSteps", u64::from(cloud.light_steps)) as u32;
    cloud.droplet_diameter = json_f32_or(value, "dropletDiameter", cloud.droplet_diameter);
    cloud.temporal_factor = json_f32_or(value, "temporalFactor", cloud.temporal_factor);
    cloud.cast_cloud_shadows = json_bool_or(value, "castCloudShadows", cloud.cast_cloud_shadows);
    cloud.cloud_shadow_strength =
        json_f32_or(value, "cloudShadowStrength", cloud.cloud_shadow_strength);
    cloud.cloud_shadow_on_surface_strength = json_f32_or(
        value,
        "cloudShadowOnSurfaceStrength",
        cloud.cloud_shadow_on_surface_strength,
    );
    cloud
}

fn wind_to_json(wind: &WindSettings) -> Value {
    object([
        ("orientation", f32_value(wind.orientation)),
        ("speed", f32_value(wind.speed)),
        ("gust", f32_value(wind.gust)),
        ("turbulenceOctaves", Value::from(wind.turbulence_octaves)),
        ("turbulenceRoughness", f32_value(wind.turbulence_roughness)),
        ("gustFrequency", f32_value(wind.gust_frequency)),
        ("referenceHeight", f32_value(wind.reference_height)),
        ("heightExponent", f32_value(wind.height_exponent)),
        ("seed", Value::from(wind.seed)),
    ])
}

fn wind_from_json(value: &Value) -> WindSettings {
    let mut wind = WindSettings::default();
    if !value.is_object() {
        return wind;
    }
    wind.orientation = json_f32_or(value, "orientation", wind.orientation);
    wind.speed = json_f32_or(value, "speed", wind.speed);
    wind.gust = json_f32_or(value, "gust", wind.gust);
    wind.turbulence_octaves = u32::try_from(json_u64_or(
        value,
        "turbulenceOctaves",
        wind.turbulence_octaves.into(),
    ))
    .unwrap_or(wind.turbulence_octaves);
    wind.turbulence_roughness =
        json_f32_or(value, "turbulenceRoughness", wind.turbulence_roughness);
    wind.gust_frequency = json_f32_or(value, "gustFrequency", wind.gust_frequency);
    wind.reference_height = json_f32_or(value, "referenceHeight", wind.reference_height);
    wind.height_exponent = json_f32_or(value, "heightExponent", wind.height_exponent);
    wind.seed = u32::try_from(json_u64_or(value, "seed", wind.seed.into())).unwrap_or(wind.seed);
    wind
}

fn tod_curve_to_json(curve: &TodCurve) -> Value {
    Value::Array(
        curve
            .0
            .iter()
            .map(|&(x, y)| object([("x", f32_value(x)), ("y", f32_value(y))]))
            .collect(),
    )
}

fn tod_curve_from_json(value: &Value) -> TodCurve {
    let Some(points) = value.as_array() else {
        return TodCurve::default();
    };
    TodCurve(
        points
            .iter()
            .filter(|point| point.is_object())
            .map(|point| (json_f32_or(point, "x", 0.0), json_f32_or(point, "y", 0.0)))
            .collect(),
    )
}

fn tint_curve_to_json(curve: &TodTintCurve) -> Value {
    object([
        ("master", tod_curve_to_json(&curve.master)),
        ("red", tod_curve_to_json(&curve.red)),
        ("green", tod_curve_to_json(&curve.green)),
        ("blue", tod_curve_to_json(&curve.blue)),
    ])
}

fn tint_curve_from_json(value: &Value) -> TodTintCurve {
    let mut curve = TodTintCurve::default();
    if let Some(value) = field(value, "master") {
        curve.master = tod_curve_from_json(value);
    }
    if let Some(value) = field(value, "red") {
        curve.red = tod_curve_from_json(value);
    }
    if let Some(value) = field(value, "green") {
        curve.green = tod_curve_from_json(value);
    }
    if let Some(value) = field(value, "blue") {
        curve.blue = tod_curve_from_json(value);
    }
    curve
}

fn time_of_day_to_json(settings: &TimeOfDaySettings) -> Value {
    object([
        ("enabled", Value::Bool(settings.enabled)),
        ("manualOverride", Value::Bool(settings.manual_override)),
        ("timeOfDay", f32_value(settings.time_of_day)),
        ("year", Value::from(settings.year)),
        ("month", Value::from(settings.month)),
        ("day", Value::from(settings.day)),
        ("latitude", f32_value(settings.latitude)),
        ("longitude", f32_value(settings.longitude)),
        ("dayLengthSeconds", f32_value(settings.day_length_seconds)),
        ("exposureCurve", tod_curve_to_json(&settings.exposure_curve)),
        ("tintCurve", tint_curve_to_json(&settings.tint_curve)),
        ("coverageCurve", tod_curve_to_json(&settings.coverage_curve)),
        (
            "cloudTypeCurve",
            tod_curve_to_json(&settings.cloud_type_curve),
        ),
    ])
}

fn time_of_day_from_json(value: &Value) -> TimeOfDaySettings {
    let mut settings = TimeOfDaySettings::default();
    if !value.is_object() {
        return settings;
    }
    settings.enabled = json_bool_or(value, "enabled", settings.enabled);
    settings.manual_override = json_bool_or(value, "manualOverride", settings.manual_override);
    settings.time_of_day = json_f32_or(value, "timeOfDay", settings.time_of_day);
    settings.year = json_i32_or(value, "year", settings.year);
    settings.month = json_i32_or(value, "month", settings.month);
    settings.day = json_i32_or(value, "day", settings.day);
    settings.latitude = json_f32_or(value, "latitude", settings.latitude);
    settings.longitude = json_f32_or(value, "longitude", settings.longitude);
    settings.day_length_seconds =
        json_f32_or(value, "dayLengthSeconds", settings.day_length_seconds);
    if let Some(curve) = field(value, "exposureCurve") {
        settings.exposure_curve = tod_curve_from_json(curve);
    }
    if let Some(curve) = field(value, "tintCurve") {
        settings.tint_curve = tint_curve_from_json(curve);
    }
    if let Some(curve) = field(value, "coverageCurve") {
        settings.coverage_curve = tod_curve_from_json(curve);
    }
    if let Some(curve) = field(value, "cloudTypeCurve") {
        settings.cloud_type_curve = tod_curve_from_json(curve);
    }
    settings
}

/// Serializes the [`SceneEnvironment`] block.
///
/// The scene-document phase writes this under the document's `environment` key. It is a free
/// function rather than a [`SceneSerialize`](crate::SceneSerialize) impl because the environment
/// lives on the [`Scene`](crate::Scene), not as an entity component.
#[must_use]
pub fn environment_to_json(env: &SceneEnvironment) -> Value {
    object([
        (
            "skyMode",
            Value::String(sky_mode_name(env.sky_mode).to_string()),
        ),
        ("clearColor", vec3_to_json(env.clear_color)),
        ("skyTexture", uuid_to_json(env.sky_texture.value())),
        ("skyIntensity", f32_value(env.sky_intensity)),
        ("skyRotation", f32_value(env.sky_rotation)),
        ("exposure", f32_value(env.exposure)),
        ("visible", Value::Bool(env.visible)),
        ("useSkyForAmbient", Value::Bool(env.use_sky_for_ambient)),
        ("ambientColor", vec3_to_json(env.ambient_color)),
        ("ambientIntensity", f32_value(env.ambient_intensity)),
        ("atmosphere", atmosphere_to_json(&env.atmosphere)),
        ("fog", fog_to_json(&env.fog)),
        ("cloud", cloud_to_json(&env.cloud)),
        ("wind", wind_to_json(&env.wind)),
        ("timeOfDay", time_of_day_to_json(&env.time_of_day)),
    ])
}

/// Reads a [`SceneEnvironment`] block, defaulting per field and leaving the vector fields
/// at their struct defaults when absent.
#[must_use]
pub fn environment_from_json(j: &Value) -> SceneEnvironment {
    let mut env = SceneEnvironment::default();
    if !j.is_object() {
        return env;
    }
    env.sky_mode = sky_mode_from_name(&json_string_or(j, "skyMode", "procedural".to_string()));
    if let Some(v) = field(j, "clearColor") {
        env.clear_color = vec3_from_json(v);
    }
    env.sky_texture = Uuid(json_u64_or(j, "skyTexture", 0));
    env.sky_intensity = json_f32_or(j, "skyIntensity", 1.0);
    env.sky_rotation = json_f32_or(j, "skyRotation", 0.0);
    env.exposure = json_f32_or(j, "exposure", 1.0);
    env.visible = json_bool_or(j, "visible", true);
    env.use_sky_for_ambient = json_bool_or(j, "useSkyForAmbient", true);
    if let Some(v) = field(j, "ambientColor") {
        env.ambient_color = vec3_from_json(v);
    }
    env.ambient_intensity = json_f32_or(j, "ambientIntensity", 0.15);
    if let Some(v) = field(j, "atmosphere") {
        env.atmosphere = atmosphere_from_json(v);
    }
    if let Some(v) = field(j, "fog") {
        env.fog = fog_from_json(v);
    }
    if let Some(v) = field(j, "cloud") {
        env.cloud = cloud_from_json(v);
    }
    if let Some(v) = field(j, "wind") {
        env.wind = wind_from_json(v);
    }
    if let Some(v) = field(j, "timeOfDay") {
        env.time_of_day = time_of_day_from_json(v);
    }
    env
}
