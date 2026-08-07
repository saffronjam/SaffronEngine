use saffron_assets::{
    builtin_environment_profile, builtin_environment_profiles, load_environment_profile,
    save_environment_profile, update_environment_profile,
};
use saffron_protocol::{
    ApplyEnvironmentProfileParams, AtmosphereSettingsDto, BuiltinEnvironmentProfileDto,
    CloudSettingsDto, EmptyParams, EnvironmentDto, EnvironmentProfileListDto,
    EnvironmentProfileRefDto, EnvironmentProfileSummaryDto, FogMode as FogModeDto,
    FogQuality as FogQualityDto, FogSettingsDto, SaveEnvironmentProfileParams, SetAtmosphereParams,
    SetCloudsParams, SetEnvironmentParams, SetFogParams, SetTimeOfDayParams, SetWindParams,
    SkyModeDto, TimeOfDaySettingsDto, TodCurvePointDto, TodTintSettingsDto,
    UpdateEnvironmentProfileParams, Uuid as WireUuid, Vec3, WindSettingsDto,
};
use saffron_scene::{
    AssetType, CloudSettings, FogMode as SceneFogMode, FogQuality as SceneFogQuality,
    SceneEnvironment, SkyMode, TimeOfDaySettings, TodCurve, WindSettings, environment_from_json,
    environment_to_json,
};
use serde_json::{Value, json};

use super::*;
use crate::error::Error;
use crate::registry::{CommandRegistry, EngineContext};

/// A wire `Vec3` as its `{x,y,z}` JSON object.
pub(crate) fn vec3_json(v: &Vec3) -> Value {
    json!({ "x": v.x, "y": v.y, "z": v.z })
}

pub(crate) fn curve_json(points: &[[f32; 2]]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|point| json!({ "x": point[0], "y": point[1] }))
            .collect(),
    )
}

pub(crate) fn is_leap_year(year: i32) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

pub(crate) fn days_in_month(year: i32, month: i32) -> Option<i32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

pub(crate) fn validate_curve(name: &str, curve: &TodCurve) -> Result<(), Error> {
    let mut previous_x = None;
    for &(x, y) in &curve.0 {
        if !x.is_finite() || !y.is_finite() {
            return Err(Error::command(format!(
                "{name} points must contain finite numbers"
            )));
        }
        if !(0.0..=1.0).contains(&x) {
            return Err(Error::command(format!(
                "{name} point x values must be in [0, 1]"
            )));
        }
        if previous_x.is_some_and(|previous| x <= previous) {
            return Err(Error::command(format!(
                "{name} point x values must be strictly increasing"
            )));
        }
        previous_x = Some(x);
    }
    Ok(())
}

pub(crate) fn validate_curve_json(name: &str, value: &Value) -> Result<(), Error> {
    let points = value
        .as_array()
        .ok_or_else(|| Error::command(format!("{name} must be an array")))?;
    for point in points {
        let point = point
            .as_object()
            .ok_or_else(|| Error::command(format!("{name} points must be objects")))?;
        let x = point.get("x").and_then(Value::as_f64);
        let y = point.get("y").and_then(Value::as_f64);
        if point.len() != 2 || x.is_none() || y.is_none() {
            return Err(Error::command(format!(
                "{name} points must contain only numeric x and y fields"
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_time_of_day_json(value: &Value) -> Result<(), Error> {
    if let Some(curve) = value.get("exposureCurve") {
        validate_curve_json("exposureCurve", curve)?;
    }
    if let Some(curve) = value.get("coverageCurve") {
        validate_curve_json("coverageCurve", curve)?;
    }
    if let Some(curve) = value.get("cloudTypeCurve") {
        validate_curve_json("cloudTypeCurve", curve)?;
    }
    if let Some(tint) = value.get("tintCurve") {
        let tint = tint
            .as_object()
            .ok_or_else(|| Error::command("tintCurve must be an object"))?;
        for channel in ["master", "red", "green", "blue"] {
            if let Some(curve) = tint.get(channel) {
                validate_curve_json(&format!("tintCurve.{channel}"), curve)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_time_of_day(settings: &TimeOfDaySettings) -> Result<(), Error> {
    if !settings.time_of_day.is_finite() || !(0.0..=1.0).contains(&settings.time_of_day) {
        return Err(Error::command("timeOfDay must be in [0, 1]"));
    }
    if days_in_month(settings.year, settings.month)
        .is_none_or(|days| !(1..=days).contains(&settings.day))
    {
        return Err(Error::command(
            "year, month, and day must form a valid Gregorian date",
        ));
    }
    if !settings.latitude.is_finite() || !(-90.0..=90.0).contains(&settings.latitude) {
        return Err(Error::command("latitude must be in [-90, 90]"));
    }
    if !settings.longitude.is_finite() || !(-180.0..=180.0).contains(&settings.longitude) {
        return Err(Error::command("longitude must be in [-180, 180]"));
    }
    if !settings.day_length_seconds.is_finite() || settings.day_length_seconds < 0.0 {
        return Err(Error::command("dayLengthSeconds must be finite and >= 0"));
    }
    validate_curve("exposureCurve", &settings.exposure_curve)?;
    validate_curve("tintCurve.master", &settings.tint_curve.master)?;
    validate_curve("tintCurve.red", &settings.tint_curve.red)?;
    validate_curve("tintCurve.green", &settings.tint_curve.green)?;
    validate_curve("tintCurve.blue", &settings.tint_curve.blue)?;
    validate_curve("coverageCurve", &settings.coverage_curve)?;
    validate_curve("cloudTypeCurve", &settings.cloud_type_curve)
}

pub(crate) fn validate_clouds(settings: &CloudSettings) -> Result<(), Error> {
    for (name, value) in [
        ("coverage", settings.coverage),
        ("cloudType", settings.cloud_type),
        ("precipitation", settings.precipitation),
        ("anvilBias", settings.anvil_bias),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Error::command(format!("{name} must be in [0, 1]")));
        }
    }
    if !settings.layer_altitude.is_finite() {
        return Err(Error::command("layerAltitude must be finite"));
    }
    for (name, value) in [
        ("layerHeight", settings.layer_height),
        ("baseScale", settings.base_scale),
        ("detailScale", settings.detail_scale),
        ("weatherScale", settings.weather_scale),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(Error::command(format!("{name} must be finite and > 0")));
        }
    }
    for (name, value) in [
        ("detailStrength", settings.detail_strength),
        ("curlStrength", settings.curl_strength),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(Error::command(format!("{name} must be finite and >= 0")));
        }
    }
    if !settings.weather_offset.is_finite() {
        return Err(Error::command("weatherOffset must be finite"));
    }
    if settings.primary_steps == 0 {
        return Err(Error::command("primarySteps must be >= 1"));
    }
    if settings.light_steps == 0 {
        return Err(Error::command("lightSteps must be >= 1"));
    }
    if !settings.droplet_diameter.is_finite() || !(5.0..=50.0).contains(&settings.droplet_diameter)
    {
        return Err(Error::command("dropletDiameter must be in [5, 50]"));
    }
    if !settings.temporal_factor.is_finite() || !(0.0..=1.0).contains(&settings.temporal_factor) {
        return Err(Error::command("temporalFactor must be in [0, 1]"));
    }
    for (name, value) in [
        ("cloudShadowStrength", settings.cloud_shadow_strength),
        (
            "cloudShadowOnSurfaceStrength",
            settings.cloud_shadow_on_surface_strength,
        ),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(Error::command(format!("{name} must be in [0, 1]")));
        }
    }
    Ok(())
}

pub(crate) fn validate_wind(settings: &WindSettings) -> Result<(), Error> {
    if !settings.orientation.is_finite() {
        return Err(Error::command("orientation must be finite"));
    }
    for (name, value) in [
        ("speed", settings.speed),
        ("gust", settings.gust),
        ("gustFrequency", settings.gust_frequency),
        ("referenceHeight", settings.reference_height),
        ("heightExponent", settings.height_exponent),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(Error::command(format!("{name} must be finite and >= 0")));
        }
    }
    if settings.turbulence_octaves > 8 {
        return Err(Error::command("turbulenceOctaves must be 8 or fewer"));
    }
    if !settings.turbulence_roughness.is_finite()
        || !(0.0..=1.0).contains(&settings.turbulence_roughness)
    {
        return Err(Error::command("turbulenceRoughness must be within 0..=1"));
    }
    Ok(())
}

/// A complete scene environment as its wire DTO.
pub(crate) fn scene_environment_dto(environment: &SceneEnvironment) -> EnvironmentDto {
    let atmosphere = &environment.atmosphere;
    let fog = &environment.fog;
    let cloud = &environment.cloud;
    let wind = &environment.wind;
    let time = &environment.time_of_day;
    EnvironmentDto {
        sky_mode: match environment.sky_mode {
            SkyMode::Color => SkyModeDto::Color,
            SkyMode::Texture => SkyModeDto::Texture,
            SkyMode::Procedural => SkyModeDto::Procedural,
        },
        clear_color: from_glam3(environment.clear_color),
        sky_texture: environment.sky_texture.into(),
        sky_intensity: environment.sky_intensity,
        sky_rotation: environment.sky_rotation,
        exposure: environment.exposure,
        visible: environment.visible,
        use_sky_for_ambient: environment.use_sky_for_ambient,
        ambient_color: from_glam3(environment.ambient_color),
        ambient_intensity: environment.ambient_intensity,
        atmosphere: AtmosphereSettingsDto {
            enabled: atmosphere.enabled,
            planet_radius: atmosphere.planet_radius,
            atmosphere_height: atmosphere.atmosphere_height,
            rayleigh_scattering: from_glam3(atmosphere.rayleigh_scattering),
            rayleigh_scale_height: atmosphere.rayleigh_scale_height,
            mie_scattering: atmosphere.mie_scattering,
            mie_scale_height: atmosphere.mie_scale_height,
            mie_anisotropy: atmosphere.mie_anisotropy,
            ozone_absorption: from_glam3(atmosphere.ozone_absorption),
            sun_disk_angular_radius: atmosphere.sun_disk_angular_radius,
            sun_disk_intensity: atmosphere.sun_disk_intensity,
            moon_disk_angular_radius: atmosphere.moon_disk_angular_radius,
            moon_disk_intensity: atmosphere.moon_disk_intensity,
            moon_earthshine: atmosphere.moon_earthshine,
            per_pixel_transmittance: atmosphere.per_pixel_transmittance,
            sky_capture_cadence: atmosphere.sky_capture_cadence,
        },
        fog: FogSettingsDto {
            enabled: fog.enabled,
            mode: match fog.mode {
                SceneFogMode::Analytic => FogModeDto::Analytic,
                SceneFogMode::Volumetric => FogModeDto::Volumetric,
            },
            quality: match fog.quality {
                SceneFogQuality::Low => FogQualityDto::Low,
                SceneFogQuality::Medium => FogQualityDto::Medium,
                SceneFogQuality::High => FogQualityDto::High,
            },
            history_blend: fog.history_blend,
            neighborhood_clamp: fog.neighborhood_clamp,
            light_clamp: fog.light_clamp,
            base_density: fog.base_density,
            scatter_albedo: fog.scatter_albedo,
            phase_g: fog.phase_g,
            density: fog.density,
            albedo: from_glam3(fog.albedo),
            height: fog.height,
            height_falloff: fog.height_falloff,
            start_distance: fog.start_distance,
            max_opacity: fog.max_opacity,
            emissive: from_glam3(fog.emissive),
            directional_color: from_glam3(fog.directional_color),
            directional_exponent: fog.directional_exponent,
            layer2_density: fog.layer2_density,
            layer2_falloff: fog.layer2_falloff,
            layer2_height: fog.layer2_height,
            aerial_perspective: fog.aerial_perspective,
            aerial_intensity: fog.aerial_intensity,
        },
        cloud: CloudSettingsDto {
            enabled: cloud.enabled,
            coverage: cloud.coverage,
            cloud_type: cloud.cloud_type,
            precipitation: cloud.precipitation,
            anvil_bias: cloud.anvil_bias,
            layer_altitude: cloud.layer_altitude,
            layer_height: cloud.layer_height,
            base_scale: cloud.base_scale,
            detail_scale: cloud.detail_scale,
            detail_strength: cloud.detail_strength,
            curl_strength: cloud.curl_strength,
            weather_scale: cloud.weather_scale,
            weather_offset: from_glam3(cloud.weather_offset),
            weather_texture: cloud.weather_texture.into(),
            primary_steps: cloud.primary_steps,
            light_steps: cloud.light_steps,
            droplet_diameter: cloud.droplet_diameter,
            temporal_factor: cloud.temporal_factor,
            cast_cloud_shadows: cloud.cast_cloud_shadows,
            cloud_shadow_strength: cloud.cloud_shadow_strength,
            cloud_shadow_on_surface_strength: cloud.cloud_shadow_on_surface_strength,
        },
        wind: WindSettingsDto {
            orientation: wind.orientation,
            speed: wind.speed,
            gust: wind.gust,
            turbulence_octaves: wind.turbulence_octaves,
            turbulence_roughness: wind.turbulence_roughness,
            gust_frequency: wind.gust_frequency,
            reference_height: wind.reference_height,
            height_exponent: wind.height_exponent,
            seed: wind.seed,
        },
        time_of_day: TimeOfDaySettingsDto {
            enabled: time.enabled,
            manual_override: time.manual_override,
            time_of_day: time.time_of_day,
            year: time.year,
            month: time.month,
            day: time.day,
            latitude: time.latitude,
            longitude: time.longitude,
            day_length_seconds: time.day_length_seconds,
            exposure_curve: tod_curve_dto(&time.exposure_curve),
            tint_curve: TodTintSettingsDto {
                master: tod_curve_dto(&time.tint_curve.master),
                red: tod_curve_dto(&time.tint_curve.red),
                green: tod_curve_dto(&time.tint_curve.green),
                blue: tod_curve_dto(&time.tint_curve.blue),
            },
            coverage_curve: tod_curve_dto(&time.coverage_curve),
            cloud_type_curve: tod_curve_dto(&time.cloud_type_curve),
        },
    }
}

/// The active scene's environment as its wire DTO.
pub(crate) fn environment_dto(ctx: &mut EngineContext<'_>) -> EnvironmentDto {
    scene_environment_dto(&ctx.scene_edit.active_scene().environment)
}

pub(crate) fn builtin_environment_profile_key(
    profile: BuiltinEnvironmentProfileDto,
) -> &'static str {
    match profile {
        BuiltinEnvironmentProfileDto::Neutral => "neutral",
        BuiltinEnvironmentProfileDto::ClearDay => "clear-day",
        BuiltinEnvironmentProfileDto::GoldenHour => "golden-hour",
        BuiltinEnvironmentProfileDto::Overcast => "overcast",
        BuiltinEnvironmentProfileDto::Night => "night",
    }
}

pub(crate) fn builtin_environment_profile_dto(key: &str) -> Option<BuiltinEnvironmentProfileDto> {
    match key {
        "neutral" => Some(BuiltinEnvironmentProfileDto::Neutral),
        "clear-day" => Some(BuiltinEnvironmentProfileDto::ClearDay),
        "golden-hour" => Some(BuiltinEnvironmentProfileDto::GoldenHour),
        "overcast" => Some(BuiltinEnvironmentProfileDto::Overcast),
        "night" => Some(BuiltinEnvironmentProfileDto::Night),
        _ => None,
    }
}

pub(crate) fn tod_curve_dto(curve: &TodCurve) -> Vec<TodCurvePointDto> {
    curve
        .0
        .iter()
        .map(|&(x, y)| TodCurvePointDto { x, y })
        .collect()
}

/// Registers the environment, atmosphere, fog, cloud, wind, and time-of-day commands.
pub(crate) fn register_environment(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, EnvironmentDto>(
        "get-environment",
        "get-environment — dump the scene sky/environment settings",
        |ctx, _params| Ok(environment_dto(ctx)),
    );

    reg.register::<EmptyParams, EnvironmentDto>(
        "get-environment-defaults",
        "get-environment-defaults — dump the canonical environment defaults",
        |_ctx, _params| Ok(scene_environment_dto(&SceneEnvironment::default())),
    );

    reg.register::<EmptyParams, EnvironmentProfileListDto>(
        "list-environment-profiles",
        "list-environment-profiles — list built-in and project environment profiles",
        |ctx, _params| {
            let mut profiles = builtin_environment_profiles()
                .into_iter()
                .filter_map(|profile| {
                    Some(EnvironmentProfileSummaryDto {
                        reference: EnvironmentProfileRefDto::Builtin {
                            profile: builtin_environment_profile_dto(profile.key)?,
                        },
                        name: profile.name.to_owned(),
                    })
                })
                .collect::<Vec<_>>();
            let mut project_profiles = ctx
                .assets
                .catalog()
                .entries
                .iter()
                .filter(|entry| entry.asset_type == AssetType::Environment)
                .map(|entry| EnvironmentProfileSummaryDto {
                    reference: EnvironmentProfileRefDto::Asset {
                        id: WireUuid(entry.id.value()),
                    },
                    name: entry.name.clone(),
                })
                .collect::<Vec<_>>();
            project_profiles.sort_by(|a, b| a.name.cmp(&b.name));
            profiles.extend(project_profiles);
            Ok(EnvironmentProfileListDto { profiles })
        },
    );

    reg.register::<SaveEnvironmentProfileParams, EnvironmentProfileSummaryDto>(
        "save-environment-profile",
        "save-environment-profile {name, folder?} — save the active environment as a project profile",
        |ctx, params| {
            let name = params.name.trim();
            if name.is_empty() {
                return Err(Error::command("environment profile name cannot be empty"));
            }
            let environment = ctx.scene_edit.active_scene().environment.clone();
            let id = save_environment_profile(
                ctx.assets,
                &environment,
                name,
                params.folder.as_deref().unwrap_or_default(),
            )
            .map_err(Error::command)?;
            let entry = ctx
                .assets
                .catalog()
                .find(id)
                .ok_or_else(|| Error::command("saved environment profile is not in the catalog"))?;
            Ok(EnvironmentProfileSummaryDto {
                reference: EnvironmentProfileRefDto::Asset {
                    id: WireUuid(id.value()),
                },
                name: entry.name.clone(),
            })
        },
    );

    reg.register::<UpdateEnvironmentProfileParams, EnvironmentProfileSummaryDto>(
        "update-environment-profile",
        "update-environment-profile {profile} — replace a project profile with the active environment",
        |ctx, params| {
            let id = params.profile.into();
            let environment = ctx.scene_edit.active_scene().environment.clone();
            update_environment_profile(ctx.assets, id, &environment)
                .map_err(Error::command)?;
            let entry = ctx
                .assets
                .catalog()
                .find(id)
                .ok_or_else(|| Error::command("updated environment profile is not in the catalog"))?;
            Ok(EnvironmentProfileSummaryDto {
                reference: EnvironmentProfileRefDto::Asset { id: params.profile },
                name: entry.name.clone(),
            })
        },
    );

    reg.register::<ApplyEnvironmentProfileParams, EnvironmentDto>(
        "apply-environment-profile",
        "apply-environment-profile {profile} — apply a complete environment profile",
        |ctx, params| {
            let environment = match params.profile {
                EnvironmentProfileRefDto::Builtin { profile } => {
                    let key = builtin_environment_profile_key(profile);
                    builtin_environment_profile(key)
                        .ok_or_else(|| {
                            Error::command(format!("unknown environment profile '{key}'"))
                        })?
                        .environment
                }
                EnvironmentProfileRefDto::Asset { id } => {
                    load_environment_profile(ctx.assets, id.into()).map_err(Error::command)?
                }
            };
            ctx.scene_edit.active_scene().environment = environment;
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );

    // Merges the provided fields over the current environment (same wire shape as the scene
    // file's "environment" block) so unspecified fields are preserved.
    reg.register::<SetEnvironmentParams, EnvironmentDto>(
        "set-environment",
        "set-environment {--json {...} | skyMode?:color|texture|procedural, clearColor?:{x,y,z}, \
         skyTexture?:uuid, skyIntensity?, skyRotation?, exposure?, visible?:bool, \
         useSkyForAmbient?:bool, ambientColor?:{x,y,z}, ambientIntensity?}",
        |ctx, params| {
            let mut body = environment_to_json(&ctx.scene_edit.active_scene().environment);
            if let Some(Value::Object(map)) = &params.json {
                for (key, value) in map {
                    body[key] = value.clone();
                }
            }
            if let Some(v) = &params.sky_mode {
                body["skyMode"] = json!(v);
            }
            if let Some(v) = &params.clear_color {
                body["clearColor"] = vec3_json(v);
            }
            if let Some(v) = params.sky_texture {
                body["skyTexture"] = json!(v.value());
            }
            if let Some(v) = params.sky_intensity {
                body["skyIntensity"] = json!(v);
            }
            if let Some(v) = params.sky_rotation {
                body["skyRotation"] = json!(v);
            }
            if let Some(v) = params.exposure {
                body["exposure"] = json!(v);
            }
            if let Some(v) = params.visible {
                body["visible"] = json!(v);
            }
            if let Some(v) = params.use_sky_for_ambient {
                body["useSkyForAmbient"] = json!(v);
            }
            if let Some(v) = &params.ambient_color {
                body["ambientColor"] = vec3_json(v);
            }
            if let Some(v) = params.ambient_intensity {
                body["ambientIntensity"] = json!(v);
            }
            ctx.scene_edit.active_scene().environment = environment_from_json(&body);
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );

    // Merges atmosphere fields over the current environment's "atmosphere" block (same wire
    // shape as the scene file), so unspecified fields are preserved.
    reg.register::<SetAtmosphereParams, EnvironmentDto>(
        "set-atmosphere",
        "set-atmosphere {--json {...} | enabled?:bool, planetRadius?, atmosphereHeight?, \
         rayleighScattering?:{x,y,z}, rayleighScaleHeight?, mieScattering?, mieScaleHeight?, \
         mieAnisotropy?, ozoneAbsorption?:{x,y,z}, sunDiskAngularRadius?, sunDiskIntensity?, \
         moonDiskAngularRadius?, moonDiskIntensity?, moonEarthshine?, \
         perPixelTransmittance?:bool, skyCaptureCadence?}",
        |ctx, params| {
            let mut body = environment_to_json(&ctx.scene_edit.active_scene().environment);
            let mut atmos = body.get("atmosphere").cloned().unwrap_or_else(|| json!({}));
            if let Some(Value::Object(map)) = &params.json {
                for (key, value) in map {
                    atmos[key] = value.clone();
                }
            }
            if let Some(v) = params.enabled {
                atmos["enabled"] = json!(v);
            }
            if let Some(v) = params.planet_radius {
                atmos["planetRadius"] = json!(v);
            }
            if let Some(v) = params.atmosphere_height {
                atmos["atmosphereHeight"] = json!(v);
            }
            if let Some(v) = &params.rayleigh_scattering {
                atmos["rayleighScattering"] = vec3_json(v);
            }
            if let Some(v) = params.rayleigh_scale_height {
                atmos["rayleighScaleHeight"] = json!(v);
            }
            if let Some(v) = params.mie_scattering {
                atmos["mieScattering"] = json!(v);
            }
            if let Some(v) = params.mie_scale_height {
                atmos["mieScaleHeight"] = json!(v);
            }
            if let Some(v) = params.mie_anisotropy {
                atmos["mieAnisotropy"] = json!(v);
            }
            if let Some(v) = &params.ozone_absorption {
                atmos["ozoneAbsorption"] = vec3_json(v);
            }
            if let Some(v) = params.sun_disk_angular_radius {
                atmos["sunDiskAngularRadius"] = json!(v);
            }
            if let Some(v) = params.sun_disk_intensity {
                atmos["sunDiskIntensity"] = json!(v);
            }
            if let Some(v) = params.moon_disk_angular_radius {
                atmos["moonDiskAngularRadius"] = json!(v);
            }
            if let Some(v) = params.moon_disk_intensity {
                atmos["moonDiskIntensity"] = json!(v);
            }
            if let Some(v) = params.moon_earthshine {
                atmos["moonEarthshine"] = json!(v);
            }
            if let Some(v) = params.per_pixel_transmittance {
                atmos["perPixelTransmittance"] = json!(v);
            }
            if let Some(v) = params.sky_capture_cadence {
                atmos["skyCaptureCadence"] = json!(v.clamp(1.0, 60.0));
            }
            body["atmosphere"] = atmos;
            ctx.scene_edit.active_scene().environment = environment_from_json(&body);
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );

    reg.register::<SetFogParams, EnvironmentDto>(
        "set-fog",
        "set-fog {--json {...} | enabled?:bool, mode?:analytic|volumetric, \
         quality?:low|medium|high, historyBlend?, neighborhoodClamp?:bool, lightClamp?, \
         baseDensity?, scatterAlbedo?, phaseG?, density?, albedo?:{x,y,z}, height?, \
         heightFalloff?, startDistance?, maxOpacity?, emissive?:{x,y,z}, \
         directionalColor?:{x,y,z}, directionalExponent?, layer2Density?, layer2Falloff?, \
         layer2Height?, aerialPerspective?:bool, aerialIntensity?}",
        |ctx, params| {
            let mut body = environment_to_json(&ctx.scene_edit.active_scene().environment);
            let mut fog = body.get("fog").cloned().unwrap_or_else(|| json!({}));
            if let Some(Value::Object(map)) = &params.json {
                for (key, value) in map {
                    fog[key] = value.clone();
                }
            }
            if let Some(v) = params.enabled {
                fog["enabled"] = json!(v);
            }
            if let Some(v) = params.mode {
                fog["mode"] = json!(v);
            }
            if let Some(v) = params.quality {
                fog["quality"] = json!(v);
            }
            if let Some(v) = params.history_blend {
                if !(0.0..=1.0).contains(&v) {
                    return Err(Error::command("historyBlend must be in [0, 1]"));
                }
                fog["historyBlend"] = json!(v);
            }
            if let Some(v) = params.neighborhood_clamp {
                fog["neighborhoodClamp"] = json!(v);
            }
            if let Some(v) = params.light_clamp {
                if v < 0.0 {
                    return Err(Error::command("lightClamp must be >= 0"));
                }
                fog["lightClamp"] = json!(v);
            }
            if let Some(v) = params.base_density {
                fog["baseDensity"] = json!(v);
            }
            if let Some(v) = params.scatter_albedo {
                fog["scatterAlbedo"] = json!(v);
            }
            if let Some(v) = params.phase_g {
                fog["phaseG"] = json!(v);
            }
            if let Some(v) = params.density {
                fog["density"] = json!(v);
            }
            if let Some(v) = &params.albedo {
                fog["albedo"] = vec3_json(v);
            }
            if let Some(v) = params.height {
                fog["height"] = json!(v);
            }
            if let Some(v) = params.height_falloff {
                fog["heightFalloff"] = json!(v);
            }
            if let Some(v) = params.start_distance {
                fog["startDistance"] = json!(v);
            }
            if let Some(v) = params.max_opacity {
                fog["maxOpacity"] = json!(v);
            }
            if let Some(v) = &params.emissive {
                fog["emissive"] = vec3_json(v);
            }
            if let Some(v) = &params.directional_color {
                fog["directionalColor"] = vec3_json(v);
            }
            if let Some(v) = params.directional_exponent {
                fog["directionalExponent"] = json!(v);
            }
            if let Some(v) = params.layer2_density {
                fog["layer2Density"] = json!(v);
            }
            if let Some(v) = params.layer2_falloff {
                fog["layer2Falloff"] = json!(v);
            }
            if let Some(v) = params.layer2_height {
                fog["layer2Height"] = json!(v);
            }
            if let Some(v) = params.aerial_perspective {
                fog["aerialPerspective"] = json!(v);
            }
            if let Some(v) = params.aerial_intensity {
                if v < 0.0 {
                    return Err(Error::command("aerialIntensity must be >= 0"));
                }
                fog["aerialIntensity"] = json!(v);
            }
            body["fog"] = fog;
            ctx.scene_edit.active_scene().environment = environment_from_json(&body);
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );

    reg.register::<SetCloudsParams, EnvironmentDto>(
        "set-clouds",
        "set-clouds {--json {...} | enabled?:bool, coverage?, cloudType?, precipitation?, \
         anvilBias?, layerAltitude?, layerHeight?, baseScale?, detailScale?, detailStrength?, \
         curlStrength?, weatherScale?, weatherOffset?:{x,y,z}, weatherTexture?, primarySteps?, \
         lightSteps?, dropletDiameter?, temporalFactor?, castCloudShadows?, \
         cloudShadowStrength?, cloudShadowOnSurfaceStrength?}",
        |ctx, params| {
            let mut body = environment_to_json(&ctx.scene_edit.active_scene().environment);
            let mut cloud = body.get("cloud").cloned().unwrap_or_else(|| json!({}));
            if let Some(Value::Object(map)) = &params.json {
                for (key, value) in map {
                    cloud[key] = value.clone();
                }
            }
            if let Some(value) = params.enabled {
                cloud["enabled"] = json!(value);
            }
            if let Some(value) = params.coverage {
                cloud["coverage"] = json!(value);
            }
            if let Some(value) = params.cloud_type {
                cloud["cloudType"] = json!(value);
            }
            if let Some(value) = params.precipitation {
                cloud["precipitation"] = json!(value);
            }
            if let Some(value) = params.anvil_bias {
                cloud["anvilBias"] = json!(value);
            }
            if let Some(value) = params.layer_altitude {
                cloud["layerAltitude"] = json!(value);
            }
            if let Some(value) = params.layer_height {
                cloud["layerHeight"] = json!(value);
            }
            if let Some(value) = params.base_scale {
                cloud["baseScale"] = json!(value);
            }
            if let Some(value) = params.detail_scale {
                cloud["detailScale"] = json!(value);
            }
            if let Some(value) = params.detail_strength {
                cloud["detailStrength"] = json!(value);
            }
            if let Some(value) = params.curl_strength {
                cloud["curlStrength"] = json!(value);
            }
            if let Some(value) = params.weather_scale {
                cloud["weatherScale"] = json!(value);
            }
            if let Some(value) = &params.weather_offset {
                cloud["weatherOffset"] = vec3_json(value);
            }
            if let Some(value) = params.weather_texture {
                cloud["weatherTexture"] = json!(value);
            }
            if let Some(value) = params.primary_steps {
                cloud["primarySteps"] = json!(value);
            }
            if let Some(value) = params.light_steps {
                cloud["lightSteps"] = json!(value);
            }
            if let Some(value) = params.droplet_diameter {
                cloud["dropletDiameter"] = json!(value);
            }
            if let Some(value) = params.temporal_factor {
                cloud["temporalFactor"] = json!(value);
            }
            if let Some(value) = params.cast_cloud_shadows {
                cloud["castCloudShadows"] = json!(value);
            }
            if let Some(value) = params.cloud_shadow_strength {
                cloud["cloudShadowStrength"] = json!(value);
            }
            if let Some(value) = params.cloud_shadow_on_surface_strength {
                cloud["cloudShadowOnSurfaceStrength"] = json!(value);
            }

            body["cloud"] = cloud;
            let environment = environment_from_json(&body);
            validate_clouds(&environment.cloud)?;
            ctx.scene_edit.active_scene().environment = environment;
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );

    reg.register::<SetWindParams, EnvironmentDto>(
        "set-wind",
        "set-wind {--json {turbulenceOctaves?, turbulenceRoughness?, gustFrequency?, referenceHeight?, heightExponent?, seed?, ...} | orientation?, speed?, gust?}",
        |ctx, params| {
            let mut body = environment_to_json(&ctx.scene_edit.active_scene().environment);
            let mut wind = body.get("wind").cloned().unwrap_or_else(|| json!({}));
            if let Some(Value::Object(map)) = &params.json {
                for (key, value) in map {
                    wind[key] = value.clone();
                }
            }
            if let Some(value) = params.orientation {
                wind["orientation"] = json!(value);
            }
            if let Some(value) = params.speed {
                wind["speed"] = json!(value);
            }
            if let Some(value) = params.gust {
                wind["gust"] = json!(value);
            }
            body["wind"] = wind;
            let environment = environment_from_json(&body);
            validate_wind(&environment.wind)?;
            ctx.scene_edit.active_scene().environment = environment;
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );

    reg.register::<saffron_protocol::SampleWindParams, saffron_protocol::SampleWindResult>(
        "sample-wind",
        "sample-wind {positionM, timeS?} — the composed wind velocity at a world position",
        |ctx, params| {
            for value in params.position_m {
                if !value.is_finite() {
                    return Err(Error::command("positionM must be finite"));
                }
            }
            let time = match params.time_s {
                Some(time) if !time.is_finite() || time < 0.0 => {
                    return Err(Error::command("timeS must be finite and >= 0"));
                }
                Some(time) => time,
                None => ctx.scene_edit.simulation_time_s,
            };
            let scene = ctx.scene_edit.active_scene();
            let profile = scene.environment.wind.profile();
            let placed = scene.local_wind_sources();
            let sources: Vec<saffron_wind::LocalWindSource> =
                placed.iter().map(|entry| entry.source).collect();
            let position = saffron_geometry::glam::DVec3::from_array(params.position_m);
            let sampled = saffron_wind::sample_composed(&profile, &sources, position, time);
            // The spectrum is the global field taken apart, so it describes the mean and the
            // turbulence the local sources then compose over rather than the composed total.
            let spectrum = saffron_wind::sample_decomposed(&profile, position, time);
            Ok(saffron_protocol::SampleWindResult {
                velocity_mps: sampled.velocity.to_array(),
                gust_front: sampled.gust_front,
                time_s: time,
                mean_mps: spectrum.mean.to_array(),
                turbulence_mps: spectrum.turbulence().to_array(),
                octaves: (0..spectrum.octave_count as usize)
                    .map(|octave| saffron_protocol::WindOctaveDto {
                        octave: octave as u32,
                        wavelength_m: spectrum.octave_wavelengths_m[octave],
                        velocity_mps: spectrum.octave_velocity(octave).to_array(),
                    })
                    .collect(),
                sources: placed
                    .iter()
                    .map(|entry| {
                        let influence =
                            saffron_wind::source_influence(&profile, &entry.source, position);
                        saffron_protocol::WindSourceInfluenceDto {
                            entity: entry.entity.to_string(),
                            kind: entry.source.kind.name().to_owned(),
                            distance_m: influence.distance,
                            weight: influence.weight,
                            added_mps: influence.added.to_array(),
                            global_scale: influence.global_scale,
                        }
                    })
                    .collect(),
            })
        },
    );

    reg.register::<SetTimeOfDayParams, EnvironmentDto>(
        "set-time-of-day",
        "set-time-of-day {--json {...} | enabled?:bool, manualOverride?:bool, timeOfDay?, \
         year?, month?, day?, latitude?, longitude?, dayLengthSeconds?, \
         exposureCurve?:[[x,y]], tintCurve?:{master,red,green,blue}, \
         coverageCurve?:[[x,y]], cloudTypeCurve?:[[x,y]]}",
        |ctx, params| {
            let mut body = environment_to_json(&ctx.scene_edit.active_scene().environment);
            let mut time = body.get("timeOfDay").cloned().unwrap_or_else(|| json!({}));
            if let Some(Value::Object(map)) = &params.json {
                for (key, value) in map {
                    if key == "tintCurve" {
                        let mut tint = time.get("tintCurve").cloned().unwrap_or_else(|| json!({}));
                        if let Value::Object(channels) = value {
                            for (channel, curve) in channels {
                                tint[channel] = curve.clone();
                            }
                            time[key] = tint;
                            continue;
                        }
                    }
                    time[key] = value.clone();
                }
            }
            if let Some(v) = params.enabled {
                time["enabled"] = json!(v);
            }
            if let Some(v) = params.manual_override {
                time["manualOverride"] = json!(v);
            }
            if let Some(v) = params.time_of_day {
                time["timeOfDay"] = json!(v);
            }
            if let Some(v) = params.year {
                time["year"] = json!(v);
            }
            if let Some(v) = params.month {
                time["month"] = json!(v);
            }
            if let Some(v) = params.day {
                time["day"] = json!(v);
            }
            if let Some(v) = params.latitude {
                time["latitude"] = json!(v);
            }
            if let Some(v) = params.longitude {
                time["longitude"] = json!(v);
            }
            if let Some(v) = params.day_length_seconds {
                time["dayLengthSeconds"] = json!(v);
            }
            if let Some(v) = &params.exposure_curve {
                time["exposureCurve"] = curve_json(v);
            }
            if let Some(v) = &params.tint_curve {
                time["tintCurve"] = json!({
                    "master": curve_json(&v.master),
                    "red": curve_json(&v.red),
                    "green": curve_json(&v.green),
                    "blue": curve_json(&v.blue),
                });
            }
            if let Some(v) = &params.coverage_curve {
                time["coverageCurve"] = curve_json(v);
            }
            if let Some(v) = &params.cloud_type_curve {
                time["cloudTypeCurve"] = curve_json(v);
            }

            validate_time_of_day_json(&time)?;
            body["timeOfDay"] = time;
            let environment = environment_from_json(&body);
            validate_time_of_day(&environment.time_of_day)?;
            ctx.scene_edit.active_scene().environment = environment;
            ctx.scene_edit.scene_version += 1;
            Ok(environment_dto(ctx))
        },
    );
}
