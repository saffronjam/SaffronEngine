use super::*;

const STAR_RADIANCE_SCALE: f32 = 4.0e-5;
const MILKY_WAY_RADIANCE_SCALE: f32 = 0.025;

#[derive(Clone, Copy, Debug)]
pub(super) struct CelestialDirectionOverrides {
    pub(super) sun: Option<Vec3>,
    pub(super) moon: Option<Vec3>,
}

impl CelestialDirectionOverrides {
    pub(super) const NONE: Self = Self {
        sun: None,
        moon: None,
    };
}

#[derive(Clone, Copy, Debug)]
pub(super) struct TimeOfDayFrame {
    pub(super) directions: CelestialDirectionOverrides,
    pub(super) exposure: Option<f32>,
    pub(super) tint: Vec3,
    pub(super) night_factor: f32,
    pub(super) star_intensity: f32,
    pub(super) milky_way_intensity: f32,
    pub(super) world_from_equatorial: Vec4,
    pub(super) moon_illuminated_fraction: f32,
    pub(super) cloud_coverage: Option<f32>,
    pub(super) cloud_type: Option<f32>,
}

impl Default for TimeOfDayFrame {
    fn default() -> Self {
        Self {
            directions: CelestialDirectionOverrides::NONE,
            exposure: None,
            tint: Vec3::ONE,
            night_factor: 0.0,
            star_intensity: 0.0,
            milky_way_intensity: 0.0,
            world_from_equatorial: Vec4::new(0.0, 0.0, 0.0, 1.0),
            moon_illuminated_fraction: 0.0,
            cloud_coverage: None,
            cloud_type: None,
        }
    }
}

fn authored_sun_elevation(scene: &mut Scene) -> Option<f32> {
    let mut sun = None;
    scene.for_each::<&DirectionalLight, _>(|entity, light| {
        if sun.is_none() && light.atmosphere_role == AtmosphereRole::Sun {
            sun = Some((entity, *light));
        }
    });
    let (entity, light) = sun?;
    let travel_direction = (scene.world_rotation(entity) * light.direction).try_normalize()?;
    Some((-travel_direction).y.clamp(-1.0, 1.0).asin())
}

fn curve_factor(curve: &saffron_scene::TodCurve, x: f32) -> f32 {
    if curve.is_active() {
        eval_monotone_curve(curve, x)
    } else {
        1.0
    }
}

pub(super) fn drive_time_of_day(scene: &mut Scene) -> TimeOfDayFrame {
    let settings = scene.environment.time_of_day.clone();
    if !settings.enabled {
        return TimeOfDayFrame::default();
    }
    let Ok(month) = u32::try_from(settings.month) else {
        tracing::error!("time-of-day: month is outside the supported calendar range");
        return TimeOfDayFrame::default();
    };
    let Ok(day) = u32::try_from(settings.day) else {
        tracing::error!("time-of-day: day is outside the supported calendar range");
        return TimeOfDayFrame::default();
    };
    let time = CelestialTime {
        year: settings.year,
        month,
        day,
        time_of_day: f64::from(settings.time_of_day),
    };
    let julian = match julian_date(time) {
        Ok(value) => value,
        Err(err) => {
            tracing::error!("time-of-day: {err}");
            return TimeOfDayFrame::default();
        }
    };
    let sun = match solar_position(
        time,
        f64::from(settings.latitude),
        f64::from(settings.longitude),
        0.0,
    ) {
        Ok(value) => value,
        Err(err) => {
            tracing::error!("time-of-day: {err}");
            return TimeOfDayFrame::default();
        }
    };
    let moon = lunar_position(
        julian,
        f64::from(settings.latitude),
        f64::from(settings.longitude),
        0.0,
    );
    let sun_direction = dir_from_az_el(sun.azimuth, sun.elevation);
    let moon_direction = dir_from_az_el(moon.azimuth, moon.elevation);
    let elevation = if settings.manual_override {
        authored_sun_elevation(scene).unwrap_or(sun.elevation as f32)
    } else {
        sun.elevation as f32
    };
    let sun_elevation_norm = (elevation.to_degrees() / 90.0 * 0.5 + 0.5).clamp(0.0, 1.0);
    let night_linear = (-elevation.to_degrees() / 18.0).clamp(0.0, 1.0);
    let night_factor = night_linear * night_linear * (3.0 - 2.0 * night_linear);
    let master = curve_factor(&settings.tint_curve.master, sun_elevation_norm);
    let tint = Vec3::new(
        curve_factor(&settings.tint_curve.red, sun_elevation_norm),
        curve_factor(&settings.tint_curve.green, sun_elevation_norm),
        curve_factor(&settings.tint_curve.blue, sun_elevation_norm),
    ) * master;
    let rotation = world_from_equatorial(
        julian,
        f64::from(settings.latitude),
        f64::from(settings.longitude),
    );
    TimeOfDayFrame {
        directions: if settings.manual_override {
            CelestialDirectionOverrides::NONE
        } else {
            CelestialDirectionOverrides {
                sun: Some(-sun_direction),
                moon: Some(-moon_direction),
            }
        },
        exposure: settings
            .exposure_curve
            .is_active()
            .then(|| eval_monotone_curve(&settings.exposure_curve, sun_elevation_norm)),
        tint,
        night_factor,
        star_intensity: STAR_RADIANCE_SCALE,
        milky_way_intensity: MILKY_WAY_RADIANCE_SCALE,
        world_from_equatorial: Vec4::from_array(rotation.to_array()),
        moon_illuminated_fraction: ((1.0 - sun_direction.dot(moon_direction)) * 0.5)
            .clamp(0.0, 1.0),
        cloud_coverage: settings
            .coverage_curve
            .is_active()
            .then(|| eval_monotone_curve(&settings.coverage_curve, sun_elevation_norm)),
        cloud_type: settings
            .cloud_type_curve
            .is_active()
            .then(|| eval_monotone_curve(&settings.cloud_type_curve, sun_elevation_norm)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn night_sky_radiance_is_not_hand_faded_by_sun_elevation() {
        let mut scene = Scene::new();
        scene.environment.time_of_day.enabled = true;
        scene.environment.time_of_day.time_of_day = 0.0;
        let midnight = drive_time_of_day(&mut scene);
        scene.environment.time_of_day.time_of_day = 0.5;
        let noon = drive_time_of_day(&mut scene);

        assert_eq!(midnight.star_intensity, STAR_RADIANCE_SCALE);
        assert_eq!(noon.star_intensity, STAR_RADIANCE_SCALE);
        assert_eq!(midnight.milky_way_intensity, MILKY_WAY_RADIANCE_SCALE);
        assert_eq!(noon.milky_way_intensity, MILKY_WAY_RADIANCE_SCALE);
    }

    #[test]
    fn time_of_day_weather_curves_own_cloud_shape_when_active() {
        let mut scene = Scene::new();
        scene.environment.time_of_day.enabled = true;
        scene.environment.time_of_day.coverage_curve =
            saffron_scene::TodCurve(vec![(0.0, 0.8), (1.0, 0.8)]);
        scene.environment.time_of_day.cloud_type_curve =
            saffron_scene::TodCurve(vec![(0.0, 0.7), (1.0, 0.7)]);

        let frame = drive_time_of_day(&mut scene);

        assert_eq!(frame.cloud_coverage, Some(0.8));
        assert_eq!(frame.cloud_type, Some(0.7));
    }
}
