//! The 54 scene-edit control commands registered here: entity lifecycle (create/add/destroy/copy/
//! rename/parent), the registry-driven component commands (add/remove/set/set-field/
//! order), selection (select/deselect/get-selection), picking + inspect + focus +
//! world-transform, the editor camera + gizmo + fly/script input, the play-state machine
//! (play/pause/step/stop/get-play-state), environment + atmosphere, and the scripting
//! surface registered here (status/drain-errors/drain-logs/set-override).
//!
//! This is the most `sceneEdit`-coupled domain: the handlers drive the
//! [`SceneEditContext`] (selection, gizmo state, play machine, the active-scene
//! resolution) and read the scene world through its component registry. `set-component` /
//! `add-entity` / `pick` also touch `assets` and the renderer.
//!
//! The `get/set-debug-overlays` commands live in the animation domain
//! (`commands_animation.rs`), `set-probes` / `recapture-probes` / `list-probes` in the
//! render domain (`commands_render.rs`), and `quit` / `create-script` /
//! `get-script-schema` in the asset domain / host.

use saffron_assets::{
    BuiltinMesh, builtin_environment_profile, builtin_environment_profiles,
    load_environment_profile, model_render_aabb, sample_scene_surface_field,
    save_environment_profile, scene_surface_providers, update_environment_profile,
};
use saffron_geometry::glam::{Mat4, Vec2, Vec3 as GlamVec3};
use saffron_protocol::{
    AddComponentResult, AddEntityParams, AddEntityPreset, ApplyEnvironmentProfileParams,
    AtmosphereSettingsDto, BuiltinEnvironmentProfileDto, CloudSettingsDto, ComponentList,
    ComponentParams, CreateEntityParams, DeselectResult, DestroyEntityResult,
    DrainScriptErrorsParams, DrainScriptErrorsResult, DrainScriptLogsParams, DrainScriptLogsResult,
    EditorCamera, EmptyParams, EntityList, EntityListEntry, EntityParams, EntityRef,
    EnvironmentDto, EnvironmentProfileListDto, EnvironmentProfileRefDto,
    EnvironmentProfileSummaryDto, FlyInputParams, FlyInputResult, FogMode as FogModeDto,
    FogQuality as FogQualityDto, FogSettingsDto, GizmoOpDto, GizmoPointerParams, GizmoPointerPhase,
    GizmoPointerResult, GizmoSpaceDto, GizmoState, InspectResult, PickKind, PickParams, PickResult,
    PlayStateResult, RemoveComponentResult, RenameEntityParams, ResidencyCountsDto,
    ResidencyFacetDto, SaveEnvironmentProfileParams, ScriptErrorDto, ScriptInputParams,
    ScriptInputResult, ScriptLogDto, ScriptStatusResult, SelectionResult, SetAtmosphereParams,
    SetCameraParams, SetCloudsParams, SetComponentFieldParams, SetComponentFieldResult,
    SetComponentOrderParams, SetComponentOrderResult, SetComponentParams, SetComponentResult,
    SetEnvironmentParams, SetFogParams, SetGizmoParams, SetLightParams, SetParentParams,
    SetScriptOverrideParams, SetScriptOverrideResult, SetTimeOfDayParams, SetTransformParams,
    SetWindParams, SkyModeDto, SpatialBoundsDto, SpatialCellParams, SpatialCellResult,
    SpatialFieldChannelDto, SpatialFieldDerivativeDto, SpatialLocalPositionDto,
    SpatialResidencyCellDto, SpatialResidencyResult, SpatialSampleParams, SpatialSampleResult,
    SpatialSourceDto, SpatialSourceLevelDto, SpatialTicksDto, SpatialWorldPositionDto, StepParams,
    SurfaceCapabilitiesDto, SurfaceProviderDto, SurfaceProvidersResult, TimeOfDaySettingsDto,
    TodCurvePointDto, TodTintSettingsDto, UpdateEnvironmentProfileParams, Uuid as WireUuid, Vec3,
    WindSettingsDto, WorldCellKeyDto,
};
use saffron_scene::{
    AssetType, Bone, Camera, CameraView, CloudSettings, ComponentTraits, DirectionalLight, Entity,
    FogMode as SceneFogMode, FogQuality as SceneFogQuality, IdComponent, MaterialSet, MaterialSlot,
    Mesh, Name, PointLight, PreviewGhost, Relationship, SceneEnvironment, Script, SkyMode,
    SpotLight, TimeOfDaySettings, TodCurve, Transform, WindSettings, environment_from_json,
    environment_to_json,
};
use saffron_sceneedit::{
    GizmoOp, GizmoSpace, NativeGizmoHandle, OrbitState, PlayState, SceneEditCamera,
    SceneEditContext, viewport_project,
};
use saffron_spatial::{
    FieldChannel, FieldDerivative, ResidencyFacet, SurfaceCapabilities, WorldCellKey, WorldPosition,
};
use serde_json::{Map, Value, json};

use crate::error::Error;
use crate::registry::{CommandRegistry, EngineContext};
use crate::selector::{entity_ref_dto, entity_uuid, fit_collider, resolve_entity};

/// Converts a wire `Vec3` to glam.
fn to_glam3(v: Vec3) -> GlamVec3 {
    GlamVec3::new(v.x, v.y, v.z)
}

/// Converts a glam vector to the wire `Vec3`.
fn from_glam3(v: GlamVec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

fn spatial_ticks_dto(ticks: [i128; 3]) -> SpatialTicksDto {
    SpatialTicksDto {
        x: ticks[0].to_string(),
        y: ticks[1].to_string(),
        z: ticks[2].to_string(),
    }
}

fn world_cell_dto(cell: WorldCellKey) -> WorldCellKeyDto {
    let coordinates = cell.coordinates();
    WorldCellKeyDto {
        x: coordinates[0].to_string(),
        y: coordinates[1].to_string(),
        z: coordinates[2].to_string(),
        level: cell.level(),
        canonical_hex: cell
            .canonical_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    }
}

fn world_position_dto(position: WorldPosition) -> SpatialWorldPositionDto {
    let local = position.local().ticks();
    SpatialWorldPositionDto {
        cell: world_cell_dto(position.cell()),
        local: SpatialLocalPositionDto {
            x: local[0],
            y: local[1],
            z: local[2],
        },
        global_ticks: spatial_ticks_dto(position.global_ticks()),
    }
}

fn surface_capabilities_dto(capabilities: SurfaceCapabilities) -> SurfaceCapabilitiesDto {
    SurfaceCapabilitiesDto {
        ray: capabilities.ray,
        project: capabilities.project,
        nearest: capabilities.nearest,
        uv: capabilities.uv,
        authoritative_attachments: capabilities.authoritative_attachments,
        authoritative_fields: capabilities.authoritative_fields,
    }
}

fn field_channel(
    channel: SpatialFieldChannelDto,
    user_channel: Option<&str>,
) -> crate::Result<FieldChannel> {
    let ordinary = match channel {
        SpatialFieldChannelDto::Altitude => Some(FieldChannel::Altitude),
        SpatialFieldChannelDto::Slope => Some(FieldChannel::Slope),
        SpatialFieldChannelDto::Curvature => Some(FieldChannel::Curvature),
        SpatialFieldChannelDto::Concavity => Some(FieldChannel::Concavity),
        SpatialFieldChannelDto::Drainage => Some(FieldChannel::Drainage),
        SpatialFieldChannelDto::Moisture => Some(FieldChannel::Moisture),
        SpatialFieldChannelDto::Temperature => Some(FieldChannel::Temperature),
        SpatialFieldChannelDto::Precipitation => Some(FieldChannel::Precipitation),
        SpatialFieldChannelDto::Sunlight => Some(FieldChannel::Sunlight),
        SpatialFieldChannelDto::Exposure => Some(FieldChannel::Exposure),
        SpatialFieldChannelDto::WaterDistance => Some(FieldChannel::WaterDistance),
        SpatialFieldChannelDto::WaterDepth => Some(FieldChannel::WaterDepth),
        SpatialFieldChannelDto::SignedBlocker => Some(FieldChannel::SignedBlocker),
        SpatialFieldChannelDto::SplineDistance => Some(FieldChannel::SplineDistance),
        SpatialFieldChannelDto::User => None,
    };
    match (ordinary, user_channel) {
        (Some(channel), None) => Ok(channel),
        (Some(_), Some(_)) => Err(Error::command(
            "userChannel is valid only when channel is 'user'",
        )),
        (None, Some(value)) => value
            .parse::<u64>()
            .map(FieldChannel::User)
            .map_err(|_| Error::command("userChannel must be a decimal u64")),
        (None, None) => Err(Error::command(
            "userChannel is required when channel is 'user'",
        )),
    }
}

fn field_derivative(derivative: SpatialFieldDerivativeDto) -> FieldDerivative {
    match derivative {
        SpatialFieldDerivativeDto::Value => FieldDerivative::Value,
        SpatialFieldDerivativeDto::Gradient => FieldDerivative::Gradient,
        SpatialFieldDerivativeDto::Hessian => FieldDerivative::Hessian,
    }
}

fn residency_facet_dto(facet: ResidencyFacet) -> ResidencyFacetDto {
    match facet {
        ResidencyFacet::Render => ResidencyFacetDto::Render,
        ResidencyFacet::Physics => ResidencyFacetDto::Physics,
        ResidencyFacet::Simulation => ResidencyFacetDto::Simulation,
        ResidencyFacet::Editing => ResidencyFacetDto::Editing,
        ResidencyFacet::Navigation => ResidencyFacetDto::Navigation,
        ResidencyFacet::Network => ResidencyFacetDto::Network,
    }
}

/// A wire `Vec3` as its `{x,y,z}` JSON object.
fn vec3_json(v: &Vec3) -> Value {
    json!({ "x": v.x, "y": v.y, "z": v.z })
}

fn curve_json(points: &[[f32; 2]]) -> Value {
    Value::Array(
        points
            .iter()
            .map(|point| json!({ "x": point[0], "y": point[1] }))
            .collect(),
    )
}

fn is_leap_year(year: i32) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

fn days_in_month(year: i32, month: i32) -> Option<i32> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => return None,
    })
}

fn validate_curve(name: &str, curve: &TodCurve) -> Result<(), Error> {
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

fn validate_curve_json(name: &str, value: &Value) -> Result<(), Error> {
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

fn validate_time_of_day_json(value: &Value) -> Result<(), Error> {
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

fn validate_time_of_day(settings: &TimeOfDaySettings) -> Result<(), Error> {
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

fn validate_clouds(settings: &CloudSettings) -> Result<(), Error> {
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

fn validate_wind(settings: &WindSettings) -> Result<(), Error> {
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

/// The editor fly-camera as its wire DTO.
fn camera_dto(camera: &SceneEditCamera) -> EditorCamera {
    EditorCamera {
        position: from_glam3(camera.position),
        yaw: camera.yaw,
        pitch: camera.pitch,
        fov: camera.fov,
        near: camera.near_plane,
        far: camera.far_plane,
        move_speed: camera.move_speed,
        look_speed: camera.look_speed,
    }
}

/// A complete scene environment as its wire DTO.
fn scene_environment_dto(environment: &SceneEnvironment) -> EnvironmentDto {
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
fn environment_dto(ctx: &mut EngineContext<'_>) -> EnvironmentDto {
    scene_environment_dto(&ctx.scene_edit.active_scene().environment)
}

fn builtin_environment_profile_key(profile: BuiltinEnvironmentProfileDto) -> &'static str {
    match profile {
        BuiltinEnvironmentProfileDto::Neutral => "neutral",
        BuiltinEnvironmentProfileDto::ClearDay => "clear-day",
        BuiltinEnvironmentProfileDto::GoldenHour => "golden-hour",
        BuiltinEnvironmentProfileDto::Overcast => "overcast",
        BuiltinEnvironmentProfileDto::Night => "night",
    }
}

fn builtin_environment_profile_dto(key: &str) -> Option<BuiltinEnvironmentProfileDto> {
    match key {
        "neutral" => Some(BuiltinEnvironmentProfileDto::Neutral),
        "clear-day" => Some(BuiltinEnvironmentProfileDto::ClearDay),
        "golden-hour" => Some(BuiltinEnvironmentProfileDto::GoldenHour),
        "overcast" => Some(BuiltinEnvironmentProfileDto::Overcast),
        "night" => Some(BuiltinEnvironmentProfileDto::Night),
        _ => None,
    }
}

fn tod_curve_dto(curve: &TodCurve) -> Vec<TodCurvePointDto> {
    curve
        .0
        .iter()
        .map(|&(x, y)| TodCurvePointDto { x, y })
        .collect()
}

/// Maps the backend-neutral [`GizmoOp`] to its wire spelling.
fn gizmo_op_dto(op: GizmoOp) -> GizmoOpDto {
    match op {
        GizmoOp::Rotate => GizmoOpDto::Rotate,
        GizmoOp::Scale => GizmoOpDto::Scale,
        GizmoOp::Translate => GizmoOpDto::Translate,
    }
}

/// Maps a wire op spelling to the backend-neutral [`GizmoOp`].
fn gizmo_op_from_dto(op: GizmoOpDto) -> GizmoOp {
    match op {
        GizmoOpDto::Rotate => GizmoOp::Rotate,
        GizmoOpDto::Scale => GizmoOp::Scale,
        GizmoOpDto::Translate => GizmoOp::Translate,
    }
}

/// Maps the backend-neutral [`GizmoSpace`] to its wire spelling.
fn gizmo_space_dto(space: GizmoSpace) -> GizmoSpaceDto {
    match space {
        GizmoSpace::Local => GizmoSpaceDto::Local,
        GizmoSpace::World => GizmoSpaceDto::World,
    }
}

/// Maps a wire space spelling to the backend-neutral [`GizmoSpace`].
fn gizmo_space_from_dto(space: GizmoSpaceDto) -> GizmoSpace {
    match space {
        GizmoSpaceDto::Local => GizmoSpace::Local,
        GizmoSpaceDto::World => GizmoSpace::World,
    }
}

/// The overlay handle's wire name.
fn native_gizmo_handle_name(handle: NativeGizmoHandle) -> &'static str {
    match handle {
        NativeGizmoHandle::X => "x",
        NativeGizmoHandle::Y => "y",
        NativeGizmoHandle::Z => "z",
        NativeGizmoHandle::Xy => "xy",
        NativeGizmoHandle::Yz => "yz",
        NativeGizmoHandle::Xz => "xz",
        NativeGizmoHandle::Screen => "screen",
        NativeGizmoHandle::Uniform => "uniform",
        NativeGizmoHandle::None => "none",
    }
}

/// The gizmo op/space/preserve-children as its wire DTO.
fn gizmo_state_dto(editor: &SceneEditContext) -> GizmoState {
    GizmoState {
        op: gizmo_op_dto(editor.gizmo_op),
        space: gizmo_space_dto(editor.gizmo_space),
        preserve_children: editor.preserve_children,
    }
}

/// The uniform play-state reply.
fn play_state_result_dto(editor: &SceneEditContext) -> PlayStateResult {
    PlayStateResult {
        state: editor.play_state.name().to_owned(),
        play_version: editor.play_version as i32,
        scene_version: editor.scene_version as i32,
        has_primary_camera: editor.had_primary_camera,
        animation_version: editor.animation_version as i32,
        preview_asset: WireUuid(editor.preview_asset.value()),
    }
}

/// Lowercases a script-input key/button.
fn normalize_script_key(key: &str) -> String {
    key.to_ascii_lowercase()
}

/// Whether a parent selector means "the scene root" — absent, `0`, `"0"`, or empty: a
/// detach never resolves entity 0.
fn is_root_selector(selector: &saffron_protocol::EntitySelector) -> bool {
    selector.id() == Some(0) || selector.name().is_some_and(str::is_empty)
}

/// Server-side billboard hit-test: the nearest meshless light/camera entity whose
/// screen-space glyph contains `mouse` (viewport pixels).
fn pick_billboard(
    ctx: &mut SceneEditContext,
    cam: &CameraView,
    width: u32,
    height: u32,
    mouse: Vec2,
) -> Entity {
    if width == 0 || height == 0 {
        return Entity::NULL;
    }
    // A touch larger than the drawn glyph for easier clicking.
    const HALF: f32 = 13.0;
    let scene = ctx.active_scene();

    // Collect candidate entities first (a `for_each` borrows the scene mutably), then
    // hit-test each — the light/camera billboard set: a meshless entity that is a point
    // light, or a spot light that is not also a point light, or a camera that is neither.
    let mut candidates: Vec<Entity> = Vec::new();
    scene.for_each::<&PointLight, _>(|e, _| candidates.push(e));
    scene.for_each::<&SpotLight, _>(|e, _| candidates.push(e));
    scene.for_each::<&Camera, _>(|e, _| candidates.push(e));

    let mut hit = Entity::NULL;
    let mut best = HALF;
    for e in candidates {
        if !scene.has_component::<Transform>(e) || scene.has_component::<Mesh>(e) {
            continue;
        }
        // De-dupe: a point light is tested via the PointLight pass; a spot light only
        // when not also a point light; a camera only when neither.
        let is_point = scene.has_component::<PointLight>(e);
        let is_spot = scene.has_component::<SpotLight>(e);
        let pos = scene.world_translation(e);
        let p = viewport_project(cam, width, height, pos);
        if !p.visible {
            continue;
        }
        let _ = (is_point, is_spot); // the candidate set already encodes the precedence
        let d = (mouse - p.pixel).abs();
        if d.x <= HALF && d.y <= HALF {
            let dist = (mouse - p.pixel).length();
            if dist <= best {
                best = dist;
                hit = e;
            }
        }
    }
    hit
}

/// Registers the scene-domain commands in the frozen registration order, minus the
/// commands grouped into other files: the debug-overlay pair (animation), the probe
/// trio (render), and `quit` (asset).
pub fn register_scene_commands(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, EntityList>(
        "list-entities",
        "list all entities",
        |ctx, _params| {
            let scene = ctx.scene_edit.active_scene();
            // Gather the id+name during the scan (which borrows the scene), then read the
            // parent/bone flags after, so the per-entity reads don't overlap the for_each.
            let mut rows: Vec<(Entity, u64, String)> = Vec::new();
            scene.for_each::<(&IdComponent, &Name), _>(|entity, (id, name)| {
                rows.push((entity, id.id.value(), name.name.clone()));
            });
            // Asset-placement preview ghosts render in the viewport but are not authored
            // entities, so they never appear in the outliner.
            rows.retain(|&(entity, _, _)| !scene.has_component::<PreviewGhost>(entity));
            let mut entities = Vec::with_capacity(rows.len());
            for (entity, id, name) in rows {
                let mut entry = EntityListEntry {
                    id: WireUuid(id),
                    name,
                    parent_id: None,
                    bone: None,
                };
                // Omit parentId for roots (and bone for non-joints) so the optional fields
                // stay genuinely optional.
                if let Ok(parent) =
                    scene.with_component::<Relationship, _>(entity, |r| r.parent.value())
                    && parent != 0
                {
                    entry.parent_id = Some(WireUuid(parent));
                }
                if scene.has_component::<Bone>(entity) {
                    entry.bone = Some(true);
                }
                entities.push(entry);
            }
            Ok(EntityList { entities })
        },
    );

    reg.register::<EmptyParams, ComponentList>(
        "list-components",
        "list registered component types",
        |ctx, _params| {
            let components = ctx
                .scene_edit
                .registry
                .rows()
                .iter()
                .map(|t| t.name.to_owned())
                .collect();
            Ok(ComponentList { components })
        },
    );

    reg.register::<CreateEntityParams, EntityRef>(
        "create-entity",
        "create-entity {name}",
        |ctx, params| {
            let entity = ctx.scene_edit.active_scene().create_entity(params.name);
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<EntityParams, DestroyEntityResult>(
        "destroy-entity",
        "destroy-entity {entity}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            let id = entity_uuid(scene, entity);
            // destroyEntity takes the whole subtree, so clear the selection when it sits
            // anywhere under the doomed root (walk the selection's ancestry).
            let selected = ctx.scene_edit.selected;
            let mut cursor =
                if selected != Entity::NULL && ctx.scene_edit.active_scene().valid(selected) {
                    Some(selected)
                } else {
                    None
                };
            while let Some(node) = cursor {
                if node == entity {
                    ctx.scene_edit.set_selection(Entity::NULL);
                    break;
                }
                cursor = ctx
                    .scene_edit
                    .active_scene()
                    .with_component::<Relationship, _>(node, |r| r.parent_handle)
                    .ok()
                    .flatten();
            }
            ctx.scene_edit.active_scene().destroy_entity(entity);
            ctx.scene_edit.scene_version += 1;
            Ok(DestroyEntityResult {
                destroyed: WireUuid(id),
            })
        },
    );

    reg.register::<SetParentParams, EntityRef>(
        "set-parent",
        "set-parent {entity, parent?} — reparent (absent/0 parent detaches to root)",
        |ctx, params| {
            let child = resolve_entity(ctx, &params.entity)?;
            let mut new_parent = None;
            if let Some(parent) = &params.parent
                && !is_root_selector(parent)
            {
                new_parent = Some(resolve_entity(ctx, parent)?);
            }
            // set_parent carries the self/cycle guards and the world-preserving rebase
            // (keep_world); the selection stays intact (only sceneVersion bumps).
            ctx.scene_edit
                .active_scene()
                .set_parent(child, new_parent, true)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, child))
        },
    );

    reg.register::<ComponentParams, AddComponentResult>(
        "add-component",
        "add-component {entity, component}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            if (row.has)(ctx.scene_edit.active_scene(), entity) {
                return Err(Error::command(format!(
                    "entity already has '{}'",
                    params.component
                )));
            }
            (row.add_default)(ctx.scene_edit.active_scene(), entity)
                .map_err(|error| Error::command(error.to_string()))?;
            // Auto-fit a Collider's shape to the entity mesh AABB on add (the locked
            // decision). The registry add hook can't see the asset/renderer handles, so it
            // runs here.
            if row.name == "Collider" {
                let _ = fit_collider(ctx, entity);
            } else if row.name == "KinematicBones" {
                // Auto-fit per-bone capsules through the shared physics helper.
                let _ = saffron_physics::fit_bone_capsules(ctx.scene_edit.active_scene(), entity);
            }
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            registry.append_component_order(scene, entity, row.name);
            ctx.scene_edit.scene_version += 1;
            Ok(AddComponentResult {
                added: row.name.to_owned(),
            })
        },
    );

    reg.register::<ComponentParams, RemoveComponentResult>(
        "remove-component",
        "remove-component {entity, component}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            if !row.removable {
                return Err(Error::command(format!(
                    "component '{}' is not removable",
                    row.name
                )));
            }
            (row.remove)(ctx.scene_edit.active_scene(), entity);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            registry.remove_component_order(scene, entity, row.name);
            ctx.scene_edit.scene_version += 1;
            Ok(RemoveComponentResult {
                removed: row.name.to_owned(),
            })
        },
    );

    reg.register::<SetComponentOrderParams, SetComponentOrderResult>(
        "set-component-order",
        "set-component-order {entity, components}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            registry
                .set_component_order(scene, entity, params.components)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let components = registry.component_order(scene, entity);
            Ok(SetComponentOrderResult { components })
        },
    );

    // Applies a component's serialized form. Routing through the registry's deserialize
    // keeps the wire shape identical to scene files.
    reg.register::<SetComponentParams, SetComponentResult>(
        "set-component",
        "set-component {entity, component, json}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            let had_component = (row.has)(ctx.scene_edit.active_scene(), entity);
            (row.deserialize)(ctx.scene_edit.active_scene(), entity, &params.json)
                .map_err(|e| Error::command(e.to_string()))?;
            if !had_component {
                let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
                registry.append_component_order(scene, entity, row.name);
            }
            // A raw Relationship write changes the durable parent uuid; relink so the caches
            // follow (a cyclic parent is cut back to root with a warning).
            if row.name == "Relationship" {
                ctx.scene_edit.active_scene().relink_hierarchy();
            }
            ctx.scene_edit.scene_version += 1;
            Ok(SetComponentResult {
                set: row.name.to_owned(),
            })
        },
    );

    // Routes through the Transform row's deserialize so the wire shape matches scene files
    // exactly: {translation:{x,y,z}, rotation:{x,y,z} Euler radians, scale:{x,y,z}}.
    reg.register::<SetTransformParams, EntityRef>(
        "set-transform",
        "set-transform {entity, translation?, rotation?, scale?, smooth?:0|1}",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name("Transform")
                .ok_or_else(|| Error::command("Transform component is not registered"))?;
            if !(row.has)(ctx.scene_edit.active_scene(), entity) {
                return Err(Error::command("entity has no Transform"));
            }
            // With preserve-children, freeze each direct child's world pose so the write
            // below can rebase their locals (the children visually stay put).
            let mut child_worlds: Vec<(Entity, Mat4)> = Vec::new();
            if ctx.scene_edit.preserve_children
                && ctx
                    .scene_edit
                    .active_scene()
                    .has_component::<Relationship>(entity)
            {
                let children = ctx
                    .scene_edit
                    .active_scene()
                    .with_component::<Relationship, _>(entity, |r| r.children.clone())
                    .unwrap_or_default();
                for child in children {
                    if ctx
                        .scene_edit
                        .active_scene()
                        .has_component::<Transform>(child)
                    {
                        let world = ctx.scene_edit.active_scene().compose_world_matrix(child);
                        child_worlds.push((child, world));
                    }
                }
            }
            // Smooth edits become per-frame animation targets (step_edit_smoothing) instead
            // of writes — except under preserve-children, where every write must rebase the
            // subtree, so the edit applies exact.
            if params.smooth.unwrap_or(false) && child_worlds.is_empty() {
                let target = ctx.scene_edit.transform_smooth_entry_for(entity);
                if let Some(t) = &params.translation {
                    target.translation = Some(to_glam3(*t));
                }
                if let Some(r) = &params.rotation {
                    target.rotation = Some(to_glam3(*r));
                }
                if let Some(s) = &params.scale {
                    target.scale = Some(to_glam3(*s));
                }
                ctx.scene_edit.scene_version += 1;
                let scene = ctx.scene_edit.active_scene();
                return Ok(entity_ref_dto(scene, entity));
            }
            ctx.scene_edit.cancel_transform_smoothing(entity);
            // Merge provided fields over the current transform so unspecified fields (e.g.
            // scale) are preserved rather than reset to defaults.
            let mut body = (row.serialize)(ctx.scene_edit.active_scene(), entity);
            if let Some(t) = &params.translation {
                body["translation"] = vec3_json(t);
            }
            if let Some(r) = &params.rotation {
                body["rotation"] = vec3_json(r);
            }
            if let Some(s) = &params.scale {
                body["scale"] = vec3_json(s);
            }
            (row.deserialize)(ctx.scene_edit.active_scene(), entity, &body)
                .map_err(|e| Error::command(e.to_string()))?;
            if !child_worlds.is_empty() {
                let inv_world = ctx
                    .scene_edit
                    .active_scene()
                    .compose_world_matrix(entity)
                    .inverse();
                for (child, world) in child_worlds {
                    ctx.scene_edit
                        .active_scene()
                        .set_local_from_matrix(child, inv_world * world);
                }
            }
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    // Sets the directional light (the given entity, else the first one), merging provided
    // fields (direction/color as {x,y,z}) over its current value.
    reg.register::<SetLightParams, EntityRef>(
        "set-light",
        "set-light {entity?, direction?, color?, intensity?, ambient?}",
        |ctx, params| {
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name("DirectionalLight")
                .ok_or_else(|| Error::command("DirectionalLight component is not registered"))?;
            let target = if let Some(selector) = &params.entity {
                resolve_entity(ctx, selector)?
            } else {
                let mut found = Entity::NULL;
                ctx.scene_edit
                    .active_scene()
                    .for_each::<&DirectionalLight, _>(|entity, _| {
                        if found == Entity::NULL {
                            found = entity;
                        }
                    });
                found
            };
            if target == Entity::NULL || !(row.has)(ctx.scene_edit.active_scene(), target) {
                return Err(Error::command("no DirectionalLight to set"));
            }
            let mut body = (row.serialize)(ctx.scene_edit.active_scene(), target);
            if let Some(d) = &params.direction {
                body["direction"] = vec3_json(d);
            }
            if let Some(c) = &params.color {
                body["color"] = vec3_json(c);
            }
            if let Some(i) = params.intensity {
                body["intensity"] = json!(i);
            }
            if let Some(a) = params.ambient {
                body["ambient"] = json!(a);
            }
            (row.deserialize)(ctx.scene_edit.active_scene(), target, &body)
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, target))
        },
    );

    reg.register::<EntityParams, EntityRef>("select", "select {entity}", |ctx, params| {
        let entity = resolve_entity(ctx, &params.entity)?;
        ctx.scene_edit.set_selection(entity);
        let scene = ctx.scene_edit.active_scene();
        Ok(entity_ref_dto(scene, entity))
    });

    reg.register::<PickParams, PickResult>(
        "pick",
        "pick {u=0.5, v=0.5} — pick at viewport UV (0,0 = top-left); tests billboards then mesh AABBs",
        |ctx, params| {
            let u = params.u.unwrap_or(0.5);
            let v = params.v.unwrap_or(0.5);
            // The eye the frame was rendered with, so a click during play ray-casts from the
            // game camera, not the parked fly-cam.
            let cam = ctx.scene_edit.render_camera_view();
            let width = ctx.renderer.viewport_width();
            let height = ctx.renderer.viewport_height();
            let mouse = Vec2::new(u * width as f32, v * height as f32);

            // Billboards first (light/camera glyphs aren't in the mesh AABB set), then the
            // mesh ray-pick. The glyph hit rect mirrors the overlay's ~12px half-size.
            let billboard = pick_billboard(ctx.scene_edit, &cam, width, height, mouse);
            if billboard != Entity::NULL {
                ctx.scene_edit.set_selection(billboard);
                let scene = ctx.scene_edit.active_scene();
                let r = entity_ref_dto(scene, billboard);
                return Ok(PickResult {
                    hit: true,
                    id: Some(r.id),
                    name: Some(r.name),
                    kind: Some(PickKind::Billboard),
                    plant: None,
                    position: None,
                    normal: None,
                });
            }

            // pick_scene_surface flips proj[1][1] to match the renderer's clip space, so it
            // expects y-down NDC: v=0 (viewport top) maps to ndc.y=-1.
            let ndc = Vec2::new(u * 2.0 - 1.0, v * 2.0 - 1.0);
            let assets = &mut *ctx.assets;
            let viewport = (width, height);
            let mut hit_result = Ok(None);
            // The borrow split: the surface pick needs the upload seam + the active scene +
            // the asset server at once. The scene is borrowed from scene_edit; take it inside
            // the upload closure so the renderer borrow does not overlap it.
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                hit_result = saffron_assets::pick_scene_surface(
                    gpu,
                    viewport,
                    ctx.scene_edit.active_scene(),
                    assets,
                    &cam,
                    ndc,
                );
            });
            let surface_hit = hit_result.map_err(|error| Error::command(error.to_string()))?;
            // The same viewport ray tests the resident macro vegetation; the nearest of the
            // two vocabularies wins. Plants resolve through the CPU cell snapshot to their
            // stable identity — never a GPU slot.
            let pick_ray = saffron_assets::viewport_pick_ray(viewport, &cam, ndc);
            let plant_hit = ctx
                .vegetation
                .as_ref()
                .zip(pick_ray)
                .and_then(|(world, ray)| {
                    world
                        .query_ray(ray, &saffron_vegetation::VegetationQueryFilter::default())
                        .ok()?
                        .into_iter()
                        .next()
                })
                .filter(|plant| {
                    surface_hit
                        .as_ref()
                        .is_none_or(|hit| plant.distance_m < hit.surface.distance_m)
                });
            if let Some(nearest) = plant_hit {
                ctx.scene_edit.set_selection(Entity::NULL);
                return Ok(PickResult {
                    hit: true,
                    id: None,
                    name: None,
                    kind: Some(PickKind::Vegetation),
                    plant: Some(saffron_protocol::PlantId(nearest.plant.plant.to_string())),
                    position: None,
                    normal: None,
                });
            }
            // A micro-field ground hit is nonpersistent paint feedback: it never beats
            // an entity surface or a macro plant, and it carries no identity.
            let micro_hit = ctx
                .vegetation
                .as_ref()
                .zip(pick_ray)
                .and_then(|(world, ray)| world.query_micro_ray(ray))
                .filter(|micro| {
                    surface_hit
                        .as_ref()
                        .is_none_or(|hit| micro.distance_m < hit.surface.distance_m)
                });
            if let Some(micro) = micro_hit {
                ctx.scene_edit.set_selection(Entity::NULL);
                return Ok(PickResult {
                    hit: true,
                    id: None,
                    name: None,
                    kind: Some(PickKind::MicroVegetation),
                    plant: None,
                    position: Some(micro.position.to_array()),
                    normal: None,
                });
            }
            let Some(surface) = surface_hit else {
                ctx.scene_edit.set_selection(Entity::NULL);
                return Ok(PickResult {
                    hit: false,
                    id: None,
                    name: None,
                    kind: None,
                    plant: None,
                    position: None,
                    normal: None,
                });
            };
            let hit = surface.entity;
            // A model instance is a single subtree; a click anywhere in it selects the whole
            // model (its container root), not the bare mesh/bone node the ray hit.
            let selected = ctx.scene_edit.active_scene().model_root_of(hit);
            ctx.scene_edit.set_selection(selected);
            let scene = ctx.scene_edit.active_scene();
            let r = entity_ref_dto(scene, selected);
            Ok(PickResult {
                hit: true,
                id: Some(r.id),
                name: Some(r.name),
                kind: Some(PickKind::Mesh),
                plant: None,
                position: Some(surface.surface.position.world_meters().to_array()),
                normal: Some(surface.surface.frame.normal.to_array()),
            })
        },
    );

    reg.register::<saffron_protocol::QuerySurfaceRayParams, saffron_protocol::SurfaceRayResult>(
        "query-surface-ray",
        "query-surface-ray {originM, direction, maxDistanceM?} — nearest scene-surface hit",
        |ctx, params| {
            let origin = saffron_spatial::WorldPosition::from_global_ticks(
                params
                    .origin_m
                    .map(|meters| (meters * 4096.0).round() as i128),
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let direction = saffron_geometry::glam::DVec3::new(
                f64::from(params.direction[0]),
                f64::from(params.direction[1]),
                f64::from(params.direction[2]),
            );
            let ray = saffron_spatial::SurfaceRay::new(
                origin,
                direction.normalize_or_zero(),
                params.max_distance_m.unwrap_or(10_000.0),
            )
            .map_err(|error| Error::command(error.to_string()))?;
            let assets = &mut *ctx.assets;
            let mut hit_result = Ok(None);
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                hit_result = saffron_assets::query_scene_surface_ray(
                    gpu,
                    ctx.scene_edit.active_scene(),
                    assets,
                    &ray,
                );
            });
            let hit = hit_result.map_err(|error| Error::command(error.to_string()))?;
            Ok(match hit {
                Some(surface) => saffron_protocol::SurfaceRayResult {
                    hit: true,
                    position: Some(surface.surface.position.world_meters().to_array()),
                    normal: Some(surface.surface.frame.normal.to_array()),
                },
                None => saffron_protocol::SurfaceRayResult {
                    hit: false,
                    position: None,
                    normal: None,
                },
            })
        },
    );

    reg.register::<SpatialCellParams, SpatialCellResult>(
        "spatial-cell",
        "spatial-cell {world? | ticks?, level?} — canonical position and owner cell",
        |_ctx, params| {
            if params.world.is_some() && params.ticks.is_some() {
                return Err(Error::command("provide world or ticks, not both"));
            }
            let position = if let Some(ticks) = params.ticks {
                let parse = |value: &str| {
                    value
                        .parse::<i128>()
                        .map_err(|_| Error::command("ticks must be signed decimal integers"))
                };
                WorldPosition::from_global_ticks([
                    parse(&ticks.x)?,
                    parse(&ticks.y)?,
                    parse(&ticks.z)?,
                ])
                .map_err(|error| Error::command(error.to_string()))?
            } else {
                let world = params.world.unwrap_or(Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                });
                WorldPosition::from_world_meters(saffron_geometry::glam::DVec3::new(
                    f64::from(world.x),
                    f64::from(world.y),
                    f64::from(world.z),
                ))
                .map_err(|error| Error::command(error.to_string()))?
            };
            let selected_cell = position
                .cell()
                .ancestor(params.level.unwrap_or(0))
                .map_err(|error| Error::command(error.to_string()))?;
            Ok(SpatialCellResult {
                position: world_position_dto(position),
                selected_cell: world_cell_dto(selected_cell),
            })
        },
    );

    reg.register::<EmptyParams, SurfaceProvidersResult>(
        "spatial-providers",
        "spatial-providers — list live surface providers and capabilities",
        |ctx, _params| {
            let assets = &mut *ctx.assets;
            let mut result = Ok(Vec::new());
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = scene_surface_providers(gpu, ctx.scene_edit.active_scene(), assets);
            });
            let providers = result
                .map_err(|error| Error::command(error.to_string()))?
                .into_iter()
                .map(|provider| {
                    let descriptor = provider.descriptor;
                    let (entity, name) = {
                        let scene = ctx.scene_edit.active_scene();
                        let reference = entity_ref_dto(scene, provider.entity);
                        (reference.id, reference.name)
                    };
                    SurfaceProviderDto {
                        id: WireUuid(descriptor.id.0),
                        entity,
                        name,
                        revision: descriptor.revision.0.to_string(),
                        bounds: SpatialBoundsDto {
                            min_ticks: spatial_ticks_dto(descriptor.bounds.min_ticks()),
                            max_ticks_exclusive: spatial_ticks_dto(
                                descriptor.bounds.max_ticks_exclusive(),
                            ),
                        },
                        primitive_count: descriptor.primitive_count.to_string(),
                        max_tags_per_hit: descriptor.max_tags_per_hit,
                        capabilities: surface_capabilities_dto(descriptor.capabilities),
                    }
                })
                .collect();
            Ok(SurfaceProvidersResult { providers })
        },
    );

    reg.register::<SpatialSampleParams, SpatialSampleResult>(
        "spatial-sample",
        "spatial-sample {provider, channel, position, derivative?}",
        |ctx, params| {
            let derivative_dto = params.derivative.unwrap_or_default();
            let channel = field_channel(params.channel, params.user_channel.as_deref())?;
            let derivative = field_derivative(derivative_dto);
            let position = WorldPosition::from_world_meters(saffron_geometry::glam::DVec3::new(
                f64::from(params.position.x),
                f64::from(params.position.y),
                f64::from(params.position.z),
            ))
            .map_err(|error| Error::command(error.to_string()))?;
            let provider_id = saffron_spatial::SurfaceProviderId(params.provider.0);
            let assets = &mut *ctx.assets;
            let mut result = Ok(None);
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                result = sample_scene_surface_field(
                    gpu,
                    ctx.scene_edit.active_scene(),
                    assets,
                    provider_id,
                    channel,
                    derivative,
                    position,
                );
            });
            let sample = result
                .map_err(|error| Error::command(error.to_string()))?
                .ok_or_else(|| Error::command("surface provider not found"))?;
            Ok(SpatialSampleResult {
                provider: params.provider,
                channel: params.channel,
                user_channel: params.user_channel,
                derivative: derivative_dto,
                value_bits: sample.value.bits(),
                value: sample.value.to_f64(),
                revision: sample.revision.0.to_string(),
            })
        },
    );

    reg.register::<EmptyParams, SpatialResidencyResult>(
        "spatial-residency",
        "spatial-residency — list spatial sources and per-facet cell references",
        |ctx, _params| {
            let sources = ctx
                .spatial
                .sources()
                .into_iter()
                .map(|source| SpatialSourceDto {
                    id: source.id.0.to_string(),
                    revision: source.revision.to_string(),
                    position: world_position_dto(source.position),
                    velocity_mps: Vec3 {
                        x: source.velocity_mps.x as f32,
                        y: source.velocity_mps.y as f32,
                        z: source.velocity_mps.z as f32,
                    },
                    prediction_seconds: source.prediction_seconds,
                    levels: source
                        .levels
                        .into_iter()
                        .map(|level| SpatialSourceLevelDto {
                            level: level.level,
                            load_radius_cells: level.load_radius_cells,
                            cleanup_radius_cells: level.cleanup_radius_cells,
                        })
                        .collect(),
                    facets: source.facets.iter().map(residency_facet_dto).collect(),
                    priority: source.priority,
                })
                .collect();
            let cells = ctx
                .spatial
                .snapshots()
                .map_err(|error| Error::command(error.to_string()))?
                .into_iter()
                .map(|snapshot| SpatialResidencyCellDto {
                    cell: world_cell_dto(snapshot.cell),
                    reference_counts: ResidencyCountsDto {
                        render: snapshot.reference_counts[ResidencyFacet::Render as usize],
                        physics: snapshot.reference_counts[ResidencyFacet::Physics as usize],
                        simulation: snapshot.reference_counts[ResidencyFacet::Simulation as usize],
                        editing: snapshot.reference_counts[ResidencyFacet::Editing as usize],
                        navigation: snapshot.reference_counts[ResidencyFacet::Navigation as usize],
                        network: snapshot.reference_counts[ResidencyFacet::Network as usize],
                    },
                    priority: snapshot.priority,
                })
                .collect();
            Ok(SpatialResidencyResult { sources, cells })
        },
    );

    reg.register::<EntityParams, InspectResult>(
        "inspect",
        "inspect {entity} — dump all its components as json",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let mut components = Map::new();
            // The registry rows are `Copy`; snapshot them so the per-row active-scene borrow
            // does not overlap the registry borrow.
            let rows: Vec<ComponentTraits> = ctx.scene_edit.registry.rows().to_vec();
            for row in rows {
                let scene = ctx.scene_edit.active_scene();
                if (row.has)(scene, entity) {
                    components.insert(row.name.to_owned(), (row.serialize)(scene, entity));
                }
            }
            let scene = ctx.scene_edit.active_scene();
            let r = entity_ref_dto(scene, entity);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let component_order = registry.component_order(scene, entity);
            Ok(InspectResult {
                id: r.id,
                name: r.name,
                components: Value::Object(components),
                component_order,
            })
        },
    );

    reg.register::<EntityParams, EntityRef>(
        "focus",
        "focus {entity} — aim the editor camera at it",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if !ctx
                .scene_edit
                .active_scene()
                .has_component::<Transform>(entity)
            {
                return Err(Error::command("entity has no Transform"));
            }
            let fovy = ctx.scene_edit.camera.fov.to_radians();
            let forward = ctx.scene_edit.camera.forward();
            // Frame the whole model: union the forest's mesh AABB and pull the camera back to
            // fit it, rather than aiming at the container pivot at a fixed distance (which
            // mis-frames large or off-pivot models).
            let scene = ctx.scene_edit.active_scene();
            let assets = &mut *ctx.assets;
            let mut bounds = None;
            ctx.renderer.with_gpu_uploader(&mut |gpu| {
                bounds = model_render_aabb(gpu, scene, assets, entity);
            });
            let (target, distance) = match bounds {
                Some((lo, hi)) => {
                    let center = (lo + hi) * 0.5;
                    let radius = (hi - lo).length() * 0.5;
                    (center, (radius / (fovy * 0.5).tan() * 1.3).max(0.5))
                }
                None => (ctx.scene_edit.active_scene().world_translation(entity), 5.0),
            };
            ctx.scene_edit.camera.position = target - forward * distance;
            // Framing an entity jumps the eye — snap the target so it does not ease back.
            ctx.scene_edit.camera.sync_target();
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<EntityParams, saffron_protocol::WorldTransformResult>(
        "get-world-transform",
        "get-world-transform {entity} — the entity's composed world translation + scale",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let world = ctx.scene_edit.active_scene().world_matrix(entity);
            let t = world.w_axis.truncate();
            let s = GlamVec3::new(
                world.x_axis.truncate().length(),
                world.y_axis.truncate().length(),
                world.z_axis.truncate().length(),
            );
            Ok(saffron_protocol::WorldTransformResult {
                translation: from_glam3(t),
                scale: from_glam3(s),
            })
        },
    );

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
            .map_err(|error| Error::command(error.to_string()))?;
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
                .map_err(|error| Error::command(error.to_string()))?;
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
                    load_environment_profile(ctx.assets, id.into())
                        .map_err(|error| Error::command(error.to_string()))?
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
            let sources = scene.local_wind_sources();
            let sampled = saffron_wind::sample_composed(
                &profile,
                &sources,
                saffron_geometry::glam::DVec3::from_array(params.position_m),
                time,
            );
            Ok(saffron_protocol::SampleWindResult {
                velocity_mps: sampled.velocity.to_array(),
                gust_front: sampled.gust_front,
                time_s: time,
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

    reg.register::<EmptyParams, SelectionResult>(
        "get-selection",
        "get-selection — the current editor selection + scene/selection version stamps",
        |ctx, _params| {
            let sel = ctx.scene_edit.selected;
            let entity = if sel != Entity::NULL && ctx.scene_edit.active_scene().valid(sel) {
                let scene = ctx.scene_edit.active_scene();
                Some(entity_ref_dto(scene, sel))
            } else {
                None
            };
            Ok(SelectionResult {
                selection_version: ctx.scene_edit.selection_version as i32,
                scene_version: ctx.scene_edit.scene_version as i32,
                entity,
                play_state: ctx.scene_edit.play_state.name().to_owned(),
                play_version: ctx.scene_edit.play_version as i32,
                animation_version: ctx.scene_edit.animation_version as i32,
            })
        },
    );

    reg.register::<EmptyParams, DeselectResult>(
        "deselect",
        "deselect — clear the editor selection",
        |ctx, _params| {
            ctx.scene_edit.set_selection(Entity::NULL);
            Ok(DeselectResult {
                selection_version: ctx.scene_edit.selection_version as i32,
            })
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "play",
        "play — enter play mode (Edit) or resume (Paused)",
        |ctx, _params| {
            if ctx.scene_edit.previewing() {
                return Err(Error::command("exit the asset preview first"));
            }
            if ctx.scene_edit.play_state == PlayState::Paused {
                ctx.scene_edit
                    .resume_play()
                    .map_err(|e| Error::command(e.to_string()))?;
            } else {
                ctx.scene_edit
                    .enter_play()
                    .map_err(|e| Error::command(e.to_string()))?;
            }
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "pause",
        "pause — freeze the running scene (Playing only)",
        |ctx, _params| {
            ctx.scene_edit
                .pause_play()
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<StepParams, PlayStateResult>(
        "step",
        "step {frames=1} — advance fixed ticks (Paused only)",
        |ctx, params| {
            ctx.scene_edit
                .step_play(params.frames.unwrap_or(1))
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "stop",
        "stop — discard the play scene and restore the authored one",
        |ctx, _params| {
            ctx.scene_edit
                .stop_play()
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(play_state_result_dto(ctx.scene_edit))
        },
    );

    reg.register::<EmptyParams, PlayStateResult>(
        "get-play-state",
        "get-play-state — the current play state + version",
        |ctx, _params| Ok(play_state_result_dto(ctx.scene_edit)),
    );

    reg.register::<EmptyParams, ScriptStatusResult>(
        "get-script-status",
        "get-script-status — play state, live script instances, error high-water",
        |ctx, _params| {
            Ok(ScriptStatusResult {
                state: ctx.scene_edit.play_state.name().to_owned(),
                instances: ctx.scene_edit.script_instance_count,
                error_high_water: ctx.scene_edit.script_error_seq,
            })
        },
    );

    reg.register::<SetScriptOverrideParams, SetScriptOverrideResult>(
        "set-script-override",
        "set-script-override {entity, slot, name, value} — write one per-instance script \
         field override (a null value clears it)",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            let scene = ctx.scene_edit.active_scene();
            if !scene.has_component::<Script>(entity) {
                return Err(Error::command("entity has no Script component"));
            }
            let slot_count = scene
                .with_component::<Script, _>(entity, |c| c.scripts.len())
                .unwrap_or(0);
            if params.slot < 0 || params.slot as usize >= slot_count {
                return Err(Error::command(format!(
                    "slot {} out of range ({} slot(s))",
                    params.slot, slot_count
                )));
            }
            let (script_path, overrides) = scene
                .with_component_mut::<Script, _>(entity, |component| {
                    let slot = &mut component.scripts[params.slot as usize];
                    if !slot.overrides.is_object() {
                        slot.overrides = Value::Object(Map::new());
                    }
                    if params.value.is_null() {
                        if let Some(map) = slot.overrides.as_object_mut() {
                            map.remove(&params.name);
                        }
                    } else {
                        slot.overrides[&params.name] = params.value.clone();
                    }
                    (slot.script_path.clone(), slot.overrides.clone())
                })
                .map_err(|e| Error::command(e.to_string()))?;
            ctx.scene_edit.scene_version += 1;
            Ok(SetScriptOverrideResult {
                script_path,
                overrides,
            })
        },
    );

    reg.register::<DrainScriptErrorsParams, DrainScriptErrorsResult>(
        "drain-script-errors",
        "drain-script-errors {since} — script errors with seq > since (non-blocking)",
        |ctx, params| {
            let since = params.since.unwrap_or(0);
            let high_water_seq = ctx.scene_edit.script_error_seq;
            let oldest_seq = ctx.scene_edit.script_errors.first().map_or(0, |e| e.seq);
            // The ring drops its oldest entries; a cursor older than what survives means the
            // caller missed events.
            let overflowed = oldest_seq > 0 && since + 1 < oldest_seq;
            let events = ctx
                .scene_edit
                .script_errors
                .iter()
                .filter(|e| e.seq > since)
                .map(|e| ScriptErrorDto {
                    seq: e.seq,
                    entity: WireUuid(e.entity_uuid),
                    script: e.script.clone(),
                    message: e.message.clone(),
                    tick: e.tick,
                })
                .collect();
            Ok(DrainScriptErrorsResult {
                events,
                high_water_seq,
                oldest_seq,
                overflowed,
            })
        },
    );

    reg.register::<DrainScriptLogsParams, DrainScriptLogsResult>(
        "drain-script-logs",
        "drain-script-logs {since} — sa.log lines with seq > since (non-blocking)",
        |ctx, params| {
            let since = params.since.unwrap_or(0);
            let high_water_seq = ctx.scene_edit.script_log_seq;
            let oldest_seq = ctx.scene_edit.script_logs.first().map_or(0, |e| e.seq);
            let overflowed = oldest_seq > 0 && since + 1 < oldest_seq;
            let events = ctx
                .scene_edit
                .script_logs
                .iter()
                .filter(|e| e.seq > since)
                .map(|e| ScriptLogDto {
                    seq: e.seq,
                    entity: WireUuid(e.entity_uuid),
                    message: e.message.clone(),
                    epoch_ms: e.epoch_ms,
                    tick: e.tick,
                })
                .collect();
            Ok(DrainScriptLogsResult {
                events,
                high_water_seq,
                oldest_seq,
                overflowed,
            })
        },
    );

    reg.register::<AddEntityParams, EntityRef>(
        "add-entity",
        "add-entity {preset=empty|cube|plane|sphere|point-light|spot-light|directional-light|camera|reflection-probe|fog-volume}",
        |ctx, params| {
            let preset = params.preset.unwrap_or(AddEntityPreset::Empty);
            let entity = match preset {
                AddEntityPreset::Empty => ctx.scene_edit.active_scene().create_entity("Entity"),
                AddEntityPreset::Cube | AddEntityPreset::Plane | AddEntityPreset::Sphere => {
                    // Built-in primitives are native geometry: a reserved-id mesh (seeded on
                    // demand, never a catalog asset) plus a default material slot. No project
                    // required.
                    let (builtin, name) = match preset {
                        AddEntityPreset::Plane => (BuiltinMesh::Plane, "Plane"),
                        AddEntityPreset::Sphere => (BuiltinMesh::Sphere, "Sphere"),
                        _ => (BuiltinMesh::Cube, "Cube"),
                    };
                    let scene = ctx.scene_edit.active_scene();
                    let e = scene.create_entity(name);
                    let _ = scene.add_component(
                        e,
                        Mesh {
                            mesh: builtin.reserved_id(),
                        },
                    );
                    let _ = scene.add_component(
                        e,
                        MaterialSet {
                            slots: vec![MaterialSlot::default()],
                        },
                    );
                    e
                }
                AddEntityPreset::PointLight => {
                    let e = ctx.scene_edit.active_scene().create_entity("Point Light");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, PointLight::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 2.0, 0.0);
                    });
                    e
                }
                AddEntityPreset::SpotLight => {
                    let e = ctx.scene_edit.active_scene().create_entity("Spot Light");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, SpotLight::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 4.0, 0.0);
                    });
                    e
                }
                AddEntityPreset::DirectionalLight => {
                    let e = ctx
                        .scene_edit
                        .active_scene()
                        .create_entity("Directional Light");
                    let _ = ctx
                        .scene_edit
                        .active_scene()
                        .add_component(e, DirectionalLight::default());
                    e
                }
                AddEntityPreset::Camera => {
                    let e = ctx.scene_edit.active_scene().create_entity("Camera");
                    let _ = ctx
                        .scene_edit
                        .active_scene()
                        .add_component(e, Camera::default());
                    e
                }
                AddEntityPreset::ReflectionProbe => {
                    let e = ctx
                        .scene_edit
                        .active_scene()
                        .create_entity("Reflection Probe");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, saffron_scene::ReflectionProbe::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 2.0, 0.0);
                    });
                    e
                }
                AddEntityPreset::FogVolume => {
                    let e = ctx.scene_edit.active_scene().create_entity("Fog Volume");
                    let scene = ctx.scene_edit.active_scene();
                    let _ = scene.add_component(e, saffron_scene::FogVolume::default());
                    let _ = scene.with_component_mut::<Transform, _>(e, |t| {
                        t.translation = GlamVec3::new(0.0, 2.0, 0.0);
                    });
                    e
                }
            };
            ctx.scene_edit.scene_version += 1;
            ctx.scene_edit.set_selection(entity);
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<EntityParams, EntityRef>(
        "copy-entity",
        "copy-entity {entity} — deep-duplicate it (selects the copy)",
        |ctx, params| {
            let src = resolve_entity(ctx, &params.entity)?;
            let src_name = ctx
                .scene_edit
                .active_scene()
                .with_component::<Name, _>(src, |n| n.name.clone())
                .unwrap_or_default();
            let copy_name = format!("{src_name} (copy)");
            let fresh = ctx
                .scene_edit
                .active_scene()
                .create_entity(copy_name.clone());
            // deserialize add-defaults each missing component and applies fromJson, so we do
            // not call addDefault (which would double-emplace Name/Transform that
            // create_entity already added). Copying the Name component overwrites the
            // "(copy)" suffix, so restore it afterwards.
            let rows: Vec<ComponentTraits> = ctx.scene_edit.registry.rows().to_vec();
            for row in rows {
                let scene = ctx.scene_edit.active_scene();
                if (row.has)(scene, src) {
                    let body = (row.serialize)(scene, src);
                    let _ = (row.deserialize)(scene, fresh, &body);
                }
            }
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<Name, _>(fresh, |n| n.name = copy_name);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let src_order = registry.component_order(scene, src);
            let (registry, scene) = ctx.scene_edit.registry_and_active_scene();
            let _ = registry.set_component_order(scene, fresh, src_order);
            // The round-trip duplicated the source's parent uuid (the copy joins the source's
            // parent as a sibling); relink so the copy lands in its parent's children cache.
            ctx.scene_edit.active_scene().relink_hierarchy();
            ctx.scene_edit.scene_version += 1;
            ctx.scene_edit.set_selection(fresh);
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, fresh))
        },
    );

    reg.register::<RenameEntityParams, EntityRef>(
        "rename-entity",
        "rename-entity {entity, name} — set its Name component",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if params.name.is_empty() {
                return Err(Error::command("usage: rename-entity {entity, name}"));
            }
            let _ = ctx
                .scene_edit
                .active_scene()
                .with_component_mut::<Name, _>(entity, |n| n.name = params.name.clone());
            ctx.scene_edit.scene_version += 1;
            let scene = ctx.scene_edit.active_scene();
            Ok(entity_ref_dto(scene, entity))
        },
    );

    reg.register::<SetComponentFieldParams, SetComponentFieldResult>(
        "set-component-field",
        "set-component-field {entity, component, field, value} — merge one field (value may \
         be a uuid string, number, bool, or json object)",
        |ctx, params| {
            let entity = resolve_entity(ctx, &params.entity)?;
            if params.component.is_empty() || params.field.is_empty() {
                return Err(Error::command(
                    "usage: set-component-field {entity, component, field, value}",
                ));
            }
            let row = *ctx
                .scene_edit
                .registry
                .find_by_name(&params.component)
                .ok_or_else(|| {
                    Error::command(format!("unknown component '{}'", params.component))
                })?;
            if !(row.has)(ctx.scene_edit.active_scene(), entity) {
                (row.add_default)(ctx.scene_edit.active_scene(), entity)
                    .map_err(|error| Error::command(error.to_string()))?;
            }
            let mut body = (row.serialize)(ctx.scene_edit.active_scene(), entity);
            // The CLI passes every value as a string; a fully-numeric one becomes a u64 so
            // numeric/id fields land as numbers, while non-numeric strings pass through.
            let mut value = params.value.clone();
            if let Some(s) = value.as_str()
                && let Ok(n) = s.parse::<u64>()
            {
                value = json!(n);
            }
            if let Some(index) = params.index {
                // Address one element of an array field: an object value merges its keys into
                // body[field][index] (a partial edit), any other value replaces the element.
                let array = body.get_mut(&params.field).and_then(Value::as_array_mut);
                let out_of_range = array
                    .as_ref()
                    .is_none_or(|a| index < 0 || index as usize >= a.len());
                if out_of_range {
                    return Err(Error::command(format!(
                        "'{}.{}' has no array index {}",
                        params.component, params.field, index
                    )));
                }
                let element = &mut body[&params.field][index as usize];
                if let Value::Object(map) = value {
                    for (key, sub) in map {
                        element[&key] = sub;
                    }
                } else {
                    *element = value;
                }
            } else {
                body[&params.field] = value;
            }
            (row.deserialize)(ctx.scene_edit.active_scene(), entity, &body)
                .map_err(|e| Error::command(e.to_string()))?;
            // A raw Relationship write changes the durable parent uuid; relink so the caches
            // follow (a cyclic parent is cut back to root with a warning).
            if row.name == "Relationship" {
                ctx.scene_edit.active_scene().relink_hierarchy();
            }
            ctx.scene_edit.scene_version += 1;
            Ok(SetComponentFieldResult {
                set: row.name.to_owned(),
                field: params.field,
            })
        },
    );

    reg.register::<EmptyParams, EditorCamera>(
        "get-camera",
        "get-camera — the editor fly-camera state",
        |ctx, _params| Ok(camera_dto(&ctx.scene_edit.camera)),
    );

    reg.register::<SetCameraParams, EditorCamera>(
        "set-camera",
        "set-camera {position?, yaw?, pitch?, fov?, near?, far?, moveSpeed?, lookSpeed?, \
         pivot?, distance?} — pivot+distance eases the preview orbit along the arc; else a \
         free-eye set that snaps to position",
        |ctx, params| {
            let c = &mut ctx.scene_edit.camera;
            if let Some(f) = params.fov {
                c.fov = f;
            }
            if let Some(n) = params.near {
                c.near_plane = n;
            }
            if let Some(f) = params.far {
                c.far_plane = f;
            }
            if let Some(m) = params.move_speed {
                c.move_speed = m;
            }
            if let Some(l) = params.look_speed {
                c.look_speed = l;
            }
            match (params.pivot, params.distance) {
                (Some(pivot), Some(distance)) => {
                    // Orbit drive (the preview drag): ease pivot / distance / angles toward the
                    // sample; the per-frame update sweeps the eye along the arc.
                    let pivot = to_glam3(pivot);
                    if let Some(y) = params.yaw {
                        c.target_yaw = y;
                    }
                    if let Some(p) = params.pitch {
                        c.target_pitch = p.clamp(-89.0, 89.0);
                    }
                    if let Some(orbit) = c.orbit.as_mut() {
                        orbit.target_pivot = pivot;
                        orbit.target_distance = distance;
                    } else {
                        // Not framed into orbit yet: snap into it at the sample.
                        if let Some(y) = params.yaw {
                            c.yaw = y;
                        }
                        if let Some(p) = params.pitch {
                            c.pitch = p.clamp(-89.0, 89.0);
                        }
                        c.orbit = Some(OrbitState {
                            pivot,
                            distance,
                            target_pivot: pivot,
                            target_distance: distance,
                        });
                        c.position = pivot - c.forward() * distance;
                    }
                }
                _ => {
                    // Free-eye set (scripting, absolute framing restore): leave orbit mode and
                    // snap to the pose so a scripted set lands at once.
                    c.orbit = None;
                    if let Some(p) = params.position {
                        c.position = to_glam3(p);
                    }
                    if let Some(y) = params.yaw {
                        c.yaw = y;
                    }
                    if let Some(p) = params.pitch {
                        c.pitch = p;
                    }
                    c.sync_target();
                }
            }
            Ok(camera_dto(c))
        },
    );

    reg.register::<EmptyParams, GizmoState>(
        "get-gizmo",
        "get-gizmo — the gizmo op + space",
        |ctx, _params| Ok(gizmo_state_dto(ctx.scene_edit)),
    );

    reg.register::<SetGizmoParams, GizmoState>(
        "set-gizmo",
        "set-gizmo {op?:translate|rotate|scale, space?:world|local, preserveChildren?:0|1}",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("gizmo is hidden during play"));
            }
            if let Some(op) = params.op {
                ctx.scene_edit.gizmo_op = gizmo_op_from_dto(op);
            }
            if let Some(space) = params.space {
                ctx.scene_edit.gizmo_space = gizmo_space_from_dto(space);
            }
            if let Some(preserve) = params.preserve_children {
                ctx.scene_edit.preserve_children = preserve;
            }
            Ok(gizmo_state_dto(ctx.scene_edit))
        },
    );

    reg.register::<GizmoPointerParams, GizmoPointerResult>(
        "gizmo-pointer",
        "gizmo-pointer {phase:hover|begin|drag|end, x, y} — drive the overlay gizmo \
         (x,y are NDC [-1,1])",
        |ctx, params| {
            if ctx.scene_edit.play_state != PlayState::Edit {
                return Err(Error::command("gizmo is hidden during play"));
            }
            // Keep mode/space in sync with the backend-neutral gizmo state (the single source).
            ctx.scene_edit.sync_native_gizmo();
            let cam = ctx.scene_edit.camera.view();
            let width = ctx.renderer.viewport_width();
            let height = ctx.renderer.viewport_height();
            // NDC [-1,1] (top-left = -1,-1) → viewport pixels, matching the SDL pointer path.
            let x = params.x.unwrap_or(0.0);
            let y = params.y.unwrap_or(0.0);
            let mouse = Vec2::new(
                (x * 0.5 + 0.5) * width as f32,
                (y * 0.5 + 0.5) * height as f32,
            );

            let phase = params.phase.unwrap_or(GizmoPointerPhase::Hover);
            match phase {
                GizmoPointerPhase::Hover => {
                    ctx.scene_edit.native_gizmo.hovered =
                        ctx.scene_edit.hit_native_gizmo(&cam, width, height, mouse);
                }
                GizmoPointerPhase::Begin => {
                    let hovered = ctx.scene_edit.hit_native_gizmo(&cam, width, height, mouse);
                    ctx.scene_edit.native_gizmo.hovered = hovered;
                    let selected = ctx.scene_edit.selected;
                    if hovered != NativeGizmoHandle::None
                        && selected != Entity::NULL
                        && ctx
                            .scene_edit
                            .active_scene()
                            .has_component::<Transform>(selected)
                    {
                        ctx.scene_edit.native_gizmo.active = hovered;
                        ctx.scene_edit.native_gizmo.dragging = true;
                        ctx.scene_edit.native_gizmo.start_mouse = mouse;
                        ctx.scene_edit.native_gizmo.drag_target = mouse;
                        ctx.scene_edit.native_gizmo.drag_smoothed = mouse;
                        ctx.scene_edit.native_gizmo.drag_pending = false;
                        ctx.scene_edit.native_gizmo.target = selected;
                        ctx.scene_edit.snapshot_native_gizmo_start(selected);
                    }
                }
                GizmoPointerPhase::Drag => {
                    // Record the sample only; step_native_gizmo_drag smooths toward it every
                    // rendered frame, so ~60Hz pointer samples don't staircase on screen.
                    ctx.scene_edit.native_gizmo.drag_target = mouse;
                    ctx.scene_edit.native_gizmo.drag_pending = true;
                }
                GizmoPointerPhase::End => {
                    // Land exactly on the release position regardless of smoothing lag.
                    if ctx.scene_edit.native_gizmo.dragging {
                        ctx.scene_edit
                            .apply_native_gizmo_drag(&cam, width, height, mouse);
                    }
                    ctx.scene_edit.native_gizmo.dragging = false;
                    ctx.scene_edit.native_gizmo.drag_pending = false;
                    ctx.scene_edit.native_gizmo.active = NativeGizmoHandle::None;
                    ctx.scene_edit.native_gizmo.target = Entity::NULL;
                }
            }

            let handle = if ctx.scene_edit.native_gizmo.dragging {
                ctx.scene_edit.native_gizmo.active
            } else {
                ctx.scene_edit.native_gizmo.hovered
            };
            Ok(GizmoPointerResult {
                hovered: native_gizmo_handle_name(handle).to_owned(),
                dragging: ctx.scene_edit.native_gizmo.dragging,
            })
        },
    );

    reg.register::<FlyInputParams, FlyInputResult>(
        "fly-input",
        "fly-input {active, lookDx, lookDy, forward, back, left, right, up, down} — stream \
         editor fly-cam input (look deltas in pixels accumulate until the next frame)",
        |ctx, params| {
            let fly = &mut ctx.scene_edit.fly_input;
            fly.active = params.active.unwrap_or(false);
            fly.look_delta +=
                Vec2::new(params.look_dx.unwrap_or(0.0), params.look_dy.unwrap_or(0.0));
            fly.forward = params.forward.unwrap_or(false);
            fly.back = params.back.unwrap_or(false);
            fly.left = params.left.unwrap_or(false);
            fly.right = params.right.unwrap_or(false);
            fly.up = params.up.unwrap_or(false);
            fly.down = params.down.unwrap_or(false);
            if !fly.active {
                fly.look_delta = Vec2::ZERO;
            }
            Ok(FlyInputResult { active: fly.active })
        },
    );

    reg.register::<ScriptInputParams, ScriptInputResult>(
        "script-input",
        "script-input {keys, mouseButtons?, mouseX?, mouseY?, scroll?} — forward gameplay \
         input to Lua",
        |ctx, params| {
            let input = &mut ctx.scene_edit.script_input;
            input.held.clear();
            for key in &params.keys {
                let normalized = normalize_script_key(key);
                if !normalized.is_empty() {
                    input.held.insert(normalized);
                }
            }
            if let Some(buttons) = &params.mouse_buttons {
                input.mouse_buttons.clear();
                for button in buttons {
                    let normalized = normalize_script_key(button);
                    if !normalized.is_empty() {
                        input.mouse_buttons.insert(normalized);
                    }
                }
            }
            if let Some(x) = params.mouse_x {
                input.mouse_x = x;
            }
            if let Some(y) = params.mouse_y {
                input.mouse_y = y;
            }
            if let Some(scroll) = params.scroll {
                input.scroll = scroll;
            }
            let mut keys: Vec<String> = input.held.iter().cloned().collect();
            keys.sort();
            Ok(ScriptInputResult { keys })
        },
    );
}

#[cfg(test)]
mod tests {
    use saffron_geometry::glam::DVec3;
    use saffron_spatial::{
        ResidencyFacet, ResidencyMask, SourceLevel, SpatialSource, SpatialSourceId, WorldPosition,
    };
    use serde_json::json;

    use crate::registry::{CommandRegistry, EngineContext, register_builtin_commands};
    use crate::test_support::{StubRenderer, with_stub};

    fn registry() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    #[test]
    fn spatial_cell_reports_exact_negative_face_ownership() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let reply = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "spatial-cell",
                    "params": { "ticks": { "x": "-1", "y": "-262144", "z": "262144" }, "level": 1 }
                }),
            );
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["position"]["cell"]["x"], json!("-1"));
            assert_eq!(reply["result"]["position"]["cell"]["y"], json!("-1"));
            assert_eq!(reply["result"]["position"]["cell"]["z"], json!("1"));
            assert_eq!(reply["result"]["position"]["local"]["x"], json!(262_143));
            assert_eq!(reply["result"]["selectedCell"]["x"], json!("-1"));
            assert_eq!(reply["result"]["selectedCell"]["y"], json!("-1"));
            assert_eq!(reply["result"]["selectedCell"]["z"], json!("0"));
            assert_eq!(
                reply["result"]["selectedCell"]["canonicalHex"]
                    .as_str()
                    .unwrap()
                    .len(),
                50
            );
        });
    }

    #[test]
    fn spatial_provider_and_user_channel_diagnostics_are_read_only() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let providers = reg.dispatch(ctx, &json!({ "cmd": "spatial-providers" }));
            assert_eq!(providers["result"]["providers"], json!([]));

            let missing_user_id = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "spatial-sample",
                    "params": {
                        "provider": "1",
                        "channel": "user",
                        "position": { "x": 0, "y": 0, "z": 0 }
                    }
                }),
            );
            assert_eq!(missing_user_id["ok"], json!(false));
            assert_eq!(
                missing_user_id["error"]["message"],
                json!("userChannel is required when channel is 'user'")
            );
        });
    }

    #[test]
    fn spatial_residency_reports_sources_and_facet_counts() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            ctx.spatial
                .update_source(SpatialSource {
                    id: SpatialSourceId(19),
                    revision: 7,
                    position: WorldPosition::origin(),
                    velocity_mps: DVec3::new(2.0, 0.0, 0.0),
                    prediction_seconds: 0.5,
                    levels: vec![SourceLevel {
                        level: 0,
                        load_radius_cells: 0,
                        cleanup_radius_cells: 1,
                    }],
                    facets: ResidencyMask::one(ResidencyFacet::Render)
                        .with(ResidencyFacet::Editing),
                    priority: 42,
                })
                .unwrap();
            let reply = reg.dispatch(ctx, &json!({ "cmd": "spatial-residency" }));
            assert_eq!(reply["ok"], json!(true));
            assert_eq!(reply["result"]["sources"][0]["id"], json!("19"));
            assert_eq!(
                reply["result"]["sources"][0]["facets"],
                json!(["render", "editing"])
            );
            assert_eq!(
                reply["result"]["cells"][0]["referenceCounts"]["render"],
                json!(1)
            );
            assert_eq!(
                reply["result"]["cells"][0]["referenceCounts"]["editing"],
                json!(1)
            );
            assert_eq!(reply["result"]["cells"][0]["priority"], json!(42));
        });
    }

    /// `create-entity` then `destroy-entity` round-trips, and the returned `EntityRef.id` is
    /// a decimal string (the frozen wire encoding).
    #[test]
    fn create_then_destroy_entity_round_trip() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Crate" } }),
            );
            assert_eq!(created["ok"], json!(true));
            let id = created["result"]["id"].as_str().expect("id is a string");
            assert_eq!(created["result"]["name"], json!("Crate"));
            assert!(id.parse::<u64>().is_ok(), "id is a decimal string");

            let destroyed = reg.dispatch(
                ctx,
                &json!({ "cmd": "destroy-entity", "params": { "entity": id } }),
            );
            assert_eq!(destroyed["ok"], json!(true));
            assert_eq!(destroyed["result"]["destroyed"], json!(id));
        });
    }

    /// `resolve_entity` finds by UUID (a numeric string), by name, and errors with the dumped
    /// selector when absent — surfaced through `select`.
    #[test]
    fn resolve_entity_by_uuid_name_and_missing() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Target" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            // By numeric-string UUID.
            let by_uuid =
                reg.dispatch(ctx, &json!({ "cmd": "select", "params": { "entity": id } }));
            assert_eq!(by_uuid["ok"], json!(true));
            assert_eq!(by_uuid["result"]["name"], json!("Target"));

            // By name.
            let by_name = reg.dispatch(
                ctx,
                &json!({ "cmd": "select", "params": { "entity": "Target" } }),
            );
            assert_eq!(by_name["ok"], json!(true));
            assert_eq!(by_name["result"]["id"], json!(id));

            // Absent: the error dumps the selector byte-for-byte.
            let missing = reg.dispatch(
                ctx,
                &json!({ "cmd": "select", "params": { "entity": "ghost" } }),
            );
            assert_eq!(missing["ok"], json!(false));
            assert_eq!(
                missing["error"]["message"],
                json!("entity not found: \"ghost\"")
            );
        });
    }

    /// `add-component` / `set-component-field` dispatch through the registry by name; an
    /// unknown component name is a typed error.
    #[test]
    fn component_commands_dispatch_through_registry() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "E" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            let added = reg.dispatch(
                ctx,
                &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Camera" } }),
            );
            assert_eq!(added["ok"], json!(true));
            assert_eq!(added["result"]["added"], json!("Camera"));

            // Re-add the same component is rejected.
            let again = reg.dispatch(
                ctx,
                &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Camera" } }),
            );
            assert_eq!(again["ok"], json!(false));
            assert_eq!(
                again["error"]["message"],
                json!("entity already has 'Camera'")
            );

            // An unknown component name is a typed error.
            let unknown = reg.dispatch(
                ctx,
                &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Nope" } }),
            );
            assert_eq!(unknown["ok"], json!(false));
            assert_eq!(
                unknown["error"]["message"],
                json!("unknown component 'Nope'")
            );

            // set-component-field merges one field on the Name component (the string value
            // passes through, not parsed as a number).
            let set = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-component-field",
                    "params": { "entity": id, "component": "Name", "field": "name", "value": "Renamed" }
                }),
            );
            assert_eq!(set["ok"], json!(true));
            assert_eq!(set["result"]["set"], json!("Name"));
            assert_eq!(set["result"]["field"], json!("name"));

            let inspect = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": id } }),
            );
            assert_eq!(
                inspect["result"]["components"]["Name"]["name"],
                json!("Renamed")
            );
        });
    }

    #[test]
    fn vegetation_field_create_inspect_remove_and_singleton_contract() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let first = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Vegetation" } }),
            );
            let second = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Other" } }),
            );
            let first_id = first["result"]["id"].as_str().unwrap().to_owned();
            let second_id = second["result"]["id"].as_str().unwrap().to_owned();

            let added = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "add-component",
                    "params": { "entity": first_id, "component": "VegetationField" }
                }),
            );
            assert_eq!(added["ok"], json!(true), "add: {added:?}");
            let inspect = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": first_id } }),
            );
            assert_eq!(
                inspect["result"]["components"]["VegetationField"],
                json!({ "map": "0", "enabled": true })
            );

            let duplicate = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "add-component",
                    "params": { "entity": second_id, "component": "VegetationField" }
                }),
            );
            assert_eq!(duplicate["ok"], json!(false));
            assert_eq!(
                duplicate["error"]["message"],
                json!("scene already has a VegetationField component")
            );

            let removed = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "remove-component",
                    "params": { "entity": first_id, "component": "VegetationField" }
                }),
            );
            assert_eq!(removed["ok"], json!(true), "remove: {removed:?}");
            let replacement = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "add-component",
                    "params": { "entity": second_id, "component": "VegetationField" }
                }),
            );
            assert_eq!(
                replacement["ok"],
                json!(true),
                "replacement: {replacement:?}"
            );
        });
    }

    /// `set-component-field` with an array `index` merges an object value into just that
    /// slot of a `MaterialSet` (leaving its siblings untouched) and rejects an out-of-range
    /// index — the per-slot override edit the editor's material inspector drives.
    #[test]
    fn set_component_field_slot_index_merges_and_rejects_out_of_range() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Mesh" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            reg.dispatch(
                ctx,
                &json!({ "cmd": "add-component", "params": { "entity": id, "component": "MaterialSet" } }),
            );
            // Seed two slots, each referencing a distinct material with empty overrides.
            let seeded = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-component-field",
                    "params": {
                        "entity": id, "component": "MaterialSet", "field": "slots",
                        "value": [
                            { "material": "11", "overrides": {} },
                            { "material": "22", "overrides": {} }
                        ]
                    }
                }),
            );
            assert_eq!(seeded["ok"], json!(true));

            // Merge an override into slot 1 only.
            let edited = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-component-field",
                    "params": {
                        "entity": id, "component": "MaterialSet", "field": "slots", "index": 1,
                        "value": { "overrides": { "roughness": 0.25 } }
                    }
                }),
            );
            assert_eq!(edited["ok"], json!(true));

            let inspect = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": id } }),
            );
            let slots = &inspect["result"]["components"]["MaterialSet"]["slots"];
            assert_eq!(slots[1]["overrides"]["roughness"], json!(0.25));
            // Slot 0 is untouched — its overrides stay empty.
            assert!(slots[0]["overrides"]["roughness"].is_null());

            // An out-of-range index is a typed error, not a silent no-op.
            let bad = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-component-field",
                    "params": {
                        "entity": id, "component": "MaterialSet", "field": "slots", "index": 9,
                        "value": { "overrides": { "metallic": 0.5 } }
                    }
                }),
            );
            assert_eq!(bad["ok"], json!(false));
        });
    }

    /// `set-transform` merges the provided fields onto the entity's `Transform` and the write
    /// is observable through `inspect`.
    #[test]
    fn set_transform_is_observable_through_inspect() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Movable" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            let set = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-transform",
                    "params": { "entity": id, "translation": { "x": 1.5, "y": -2.0, "z": 3.25 } }
                }),
            );
            assert_eq!(set["ok"], json!(true));

            let inspect = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": id } }),
            );
            let translation = &inspect["result"]["components"]["Transform"]["translation"];
            assert_eq!(translation["x"], json!(1.5));
            assert_eq!(translation["y"], json!(-2.0));
            assert_eq!(translation["z"], json!(3.25));
        });
    }

    /// `add-component` appends at the bottom of the order, `set-component-order` reorders the
    /// present set, and a list that drops or duplicates a present component is rejected.
    #[test]
    fn component_order_appends_reorders_and_validates() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Ordered" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            // A new component appends at the bottom.
            let added = reg.dispatch(
                ctx,
                &json!({ "cmd": "add-component", "params": { "entity": id, "component": "Camera" } }),
            );
            assert_eq!(added["ok"], json!(true));
            let inspect = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": id } }),
            );
            assert_eq!(
                inspect["result"]["componentOrder"],
                json!(["Name", "Transform", "Camera"])
            );

            // An explicit reorder of the present set applies verbatim.
            let reordered = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-component-order",
                    "params": { "entity": id, "components": ["Camera", "Name", "Transform"] }
                }),
            );
            assert_eq!(reordered["ok"], json!(true));
            assert_eq!(
                reordered["result"]["components"],
                json!(["Camera", "Name", "Transform"])
            );

            // A list that duplicates a component (and drops another) is rejected, not applied.
            let bad = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-component-order",
                    "params": { "entity": id, "components": ["Camera", "Name", "Camera"] }
                }),
            );
            assert_eq!(bad["ok"], json!(false));
        });
    }

    /// `inspect` returns `{id, name, components, componentOrder}` with the order in registry
    /// order and the component blob as an opaque object.
    #[test]
    fn inspect_dumps_components_and_order() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Inspectable" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            let inspect = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": id } }),
            );
            assert_eq!(inspect["ok"], json!(true));
            assert_eq!(inspect["result"]["name"], json!("Inspectable"));
            assert!(inspect["result"]["components"].is_object());
            let order = inspect["result"]["componentOrder"]
                .as_array()
                .expect("componentOrder is an array");
            // A fresh entity carries Name + Transform.
            assert_eq!(order[0], json!("Name"));
            assert_eq!(order[1], json!("Transform"));
        });
    }

    /// `play` / `pause` / `step` / `stop` produce the expected `PlayStateResult` transitions.
    #[test]
    fn play_machine_transitions() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let play = reg.dispatch(ctx, &json!({ "cmd": "play" }));
            assert_eq!(play["ok"], json!(true));
            assert_eq!(play["result"]["state"], json!("playing"));

            let pause = reg.dispatch(ctx, &json!({ "cmd": "pause" }));
            assert_eq!(pause["ok"], json!(true));
            assert_eq!(pause["result"]["state"], json!("paused"));

            let step = reg.dispatch(ctx, &json!({ "cmd": "step", "params": { "frames": 1 } }));
            assert_eq!(step["ok"], json!(true));
            assert_eq!(step["result"]["state"], json!("paused"));

            let stop = reg.dispatch(ctx, &json!({ "cmd": "stop" }));
            assert_eq!(stop["ok"], json!(true));
            assert_eq!(stop["result"]["state"], json!("edit"));

            // pause from Edit is rejected (wrong state).
            let bad = reg.dispatch(ctx, &json!({ "cmd": "pause" }));
            assert_eq!(bad["ok"], json!(false));
        });
    }

    /// `set-gizmo` applies op/space/preserve-children and `get-gizmo` reads them back.
    #[test]
    fn gizmo_state_round_trips() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let set = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-gizmo",
                    "params": { "op": "rotate", "space": "local", "preserveChildren": true }
                }),
            );
            assert_eq!(set["ok"], json!(true));
            assert_eq!(set["result"]["op"], json!("rotate"));
            assert_eq!(set["result"]["space"], json!("local"));
            assert_eq!(set["result"]["preserveChildren"], json!(true));

            let get = reg.dispatch(ctx, &json!({ "cmd": "get-gizmo" }));
            assert_eq!(get["result"]["op"], json!("rotate"));
            assert_eq!(get["result"]["space"], json!("local"));
        });
    }

    /// Every scene mutation strictly bumps `sceneVersion` (read through get-selection) — the
    /// stamp the editor re-polls on. Covers add / copy / rename / destroy plus set-transform,
    /// set-component, and set-environment, with the entity id surfacing as a decimal string.
    #[test]
    fn scene_mutations_bump_scene_version() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let scene_version = |ctx: &mut EngineContext| -> i64 {
                reg.dispatch(ctx, &json!({ "cmd": "get-selection" }))["result"]["sceneVersion"]
                    .as_i64()
                    .expect("sceneVersion is an integer")
            };

            // A built-in primitive needs no project (reserved-id geometry).
            let cube = reg.dispatch(
                ctx,
                &json!({ "cmd": "add-entity", "params": { "preset": "cube" } }),
            );
            assert_eq!(cube["ok"], json!(true));
            let id = cube["result"]["id"]
                .as_str()
                .expect("id is a string")
                .to_owned();
            assert!(
                id.parse::<u64>().is_ok(),
                "id round-trips as a decimal string"
            );
            let mut version = scene_version(ctx);

            for command in [
                json!({ "cmd": "copy-entity", "params": { "entity": id } }),
                json!({ "cmd": "rename-entity", "params": { "entity": id, "name": "Renamed" } }),
                json!({ "cmd": "set-transform",
                        "params": { "entity": id, "translation": { "x": 1, "y": 2, "z": 3 } } }),
                json!({ "cmd": "set-component",
                        "params": { "entity": id, "component": "Name", "json": { "name": "Again" } } }),
                json!({ "cmd": "set-environment", "params": { "skyIntensity": 2.0 } }),
                json!({ "cmd": "destroy-entity", "params": { "entity": id } }),
            ] {
                let reply = reg.dispatch(ctx, &command);
                assert_eq!(reply["ok"], json!(true), "{command}");
                let next = scene_version(ctx);
                assert!(next > version, "{command} bumps sceneVersion");
                version = next;
            }
        });
    }

    /// `set-atmosphere` reflects every field it is given, a partial call merges over the current
    /// atmosphere block (it does not reset), and the free-form `{json}` path merges arbitrary keys.
    #[test]
    fn set_atmosphere_echoes_fields_and_merges_over_state() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let env = reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-atmosphere",
                    "params": {
                        "enabled": true,
                        "planetRadius": 6360000.0,
                        "rayleighScattering": { "x": 5.8, "y": 13.5, "z": 33.1 },
                        "sunDiskIntensity": 20.0
                    }
                }),
            );
            assert_eq!(env["ok"], json!(true));
            let atmos = &env["result"]["atmosphere"];
            assert_eq!(atmos["enabled"], json!(true));
            assert!((atmos["planetRadius"].as_f64().unwrap() - 6_360_000.0).abs() < 1.0);
            assert!((atmos["rayleighScattering"]["y"].as_f64().unwrap() - 13.5).abs() < 1e-3);
            assert!((atmos["sunDiskIntensity"].as_f64().unwrap() - 20.0).abs() < 1e-3);

            // A partial call merges over the read-back — the earlier fields survive.
            let merged = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-atmosphere", "params": { "mieAnisotropy": 0.8 } }),
            );
            let atmos = &merged["result"]["atmosphere"];
            assert!((atmos["mieAnisotropy"].as_f64().unwrap() - 0.8).abs() < 1e-3);
            assert_eq!(
                atmos["enabled"],
                json!(true),
                "prior fields survive the merge"
            );
            assert!((atmos["sunDiskIntensity"].as_f64().unwrap() - 20.0).abs() < 1e-3);

            // The free-form {json} path merges arbitrary keys over the same block.
            let free = reg.dispatch(
                ctx,
                &json!({ "cmd": "set-atmosphere", "params": { "json": { "mieScattering": 4.2 } } }),
            );
            let atmos = &free["result"]["atmosphere"];
            assert!((atmos["mieScattering"].as_f64().unwrap() - 4.2).abs() < 1e-3);
            assert_eq!(atmos["enabled"], json!(true));
        });
    }

    /// A smooth `set-transform` registers a per-frame animation target instead of writing; a
    /// following non-smooth write cancels that target and applies the exact value.
    #[test]
    fn set_transform_smooth_defers_then_exact_write_cancels() {
        let reg = registry();
        let mut renderer = StubRenderer::default();
        with_stub(&mut renderer, |ctx| {
            let created = reg.dispatch(
                ctx,
                &json!({ "cmd": "create-entity", "params": { "name": "Mover" } }),
            );
            let id = created["result"]["id"].as_str().unwrap().to_owned();

            reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-transform",
                    "params": { "entity": id, "translation": { "x": 10, "y": 0, "z": 0 }, "smooth": true }
                }),
            );
            assert_eq!(
                ctx.scene_edit.transform_smoothing.len(),
                1,
                "a smooth edit defers to a target instead of writing"
            );

            reg.dispatch(
                ctx,
                &json!({
                    "cmd": "set-transform",
                    "params": { "entity": id, "translation": { "x": -1, "y": 2.5, "z": 0.75 } }
                }),
            );
            assert!(
                ctx.scene_edit.transform_smoothing.is_empty(),
                "an exact write cancels the pending target"
            );

            let info = reg.dispatch(
                ctx,
                &json!({ "cmd": "inspect", "params": { "entity": id } }),
            );
            let t = &info["result"]["components"]["Transform"]["translation"];
            assert_eq!(t["x"], json!(-1.0));
            assert_eq!(t["y"], json!(2.5));
            assert_eq!(t["z"], json!(0.75));
        });
    }
}
