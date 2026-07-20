//! Astronomical coordinates for the scene time-of-day driver.

use std::f64::consts::{PI, TAU};

use glam::{Mat3, Quat, Vec3};
use saffron_scene::{Scene, TodCurve};
use solar_positioning::{spa, time::DeltaT, time::JulianDate};

use crate::Result;

const DEG_TO_RAD: f64 = PI / 180.0;
const EARTH_EQUATORIAL_RADIUS_KM: f64 = 6378.14;

/// A UTC calendar instant represented by a date and normalized time within that date.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CelestialTime {
    /// Gregorian calendar year.
    pub year: i32,
    /// Gregorian calendar month (`1..=12`).
    pub month: u32,
    /// Gregorian calendar day.
    pub day: u32,
    /// UTC time within the date (`0` is midnight, `0.5` is noon).
    pub time_of_day: f64,
}

impl CelestialTime {
    fn components(self) -> (u32, u32, f64) {
        let seconds = self.time_of_day.rem_euclid(1.0) * 86_400.0;
        let hour = (seconds / 3600.0).floor() as u32;
        let minute = ((seconds - f64::from(hour) * 3600.0) / 60.0).floor() as u32;
        let second = seconds - f64::from(hour * 3600 + minute * 60);
        (hour, minute, second.min(59.999_999_999))
    }
}

/// A topocentric direction in the local sky.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CelestialPosition {
    /// Clockwise angle from geographic north, in radians.
    pub azimuth: f64,
    /// Angle above the astronomical horizon, in radians.
    pub elevation: f64,
    /// Distance from the observer in kilometres when the ephemeris provides it.
    pub distance_km: f64,
}

/// Computes the UT Julian date for a calendar instant.
pub fn julian_date(time: CelestialTime) -> Result<f64> {
    let (hour, minute, second) = time.components();
    let jd = JulianDate::from_utc_simple(time.year, time.month, time.day, hour, minute, second)?;
    Ok(jd.julian_date())
}

/// Computes the topocentric solar position with the NREL Solar Position Algorithm.
pub fn solar_position(
    time: CelestialTime,
    latitude_degrees: f64,
    longitude_degrees: f64,
    elevation_m: f64,
) -> Result<CelestialPosition> {
    let (hour, minute, second) = time.components();
    let delta_t = DeltaT::estimate_from_date(time.year, time.month)?;
    let jd = JulianDate::from_utc(
        time.year, time.month, time.day, hour, minute, second, delta_t,
    )?;
    let position = spa::solar_position_from_julian(
        jd,
        latitude_degrees,
        longitude_degrees,
        elevation_m,
        None,
    )?;
    Ok(CelestialPosition {
        azimuth: position.azimuth().to_radians(),
        elevation: position.elevation_angle().to_radians(),
        distance_km: 0.0,
    })
}

/// Computes local mean sidereal time in radians for an east-positive longitude.
pub fn local_sidereal_time(julian_date: f64, longitude_degrees: f64) -> f64 {
    let t = (julian_date - 2_451_545.0) / 36_525.0;
    let gmst =
        280.460_618_37 + 360.985_647_366_29 * (julian_date - 2_451_545.0) + 0.000_387_933 * t * t
            - t * t * t / 38_710_000.0;
    (gmst + longitude_degrees).to_radians().rem_euclid(TAU)
}

/// Converts a north-clockwise azimuth and elevation into the engine's Y-up frame.
///
/// Geographic north is `+Z` and east is `+X`.
pub fn dir_from_az_el(azimuth: f64, elevation: f64) -> Vec3 {
    let cos_elevation = elevation.cos() as f32;
    Vec3::new(
        azimuth.sin() as f32 * cos_elevation,
        elevation.sin() as f32,
        azimuth.cos() as f32 * cos_elevation,
    )
}

/// Rotates J2000 equatorial directions into the local east-up-north world frame.
pub fn world_from_equatorial(
    julian_date: f64,
    latitude_degrees: f64,
    longitude_degrees: f64,
) -> Quat {
    let sidereal = local_sidereal_time(julian_date, longitude_degrees) as f32;
    let latitude = latitude_degrees.to_radians() as f32;
    let (sin_sidereal, cos_sidereal) = sidereal.sin_cos();
    let (sin_latitude, cos_latitude) = latitude.sin_cos();
    Quat::from_mat3(&Mat3::from_cols(
        Vec3::new(
            -sin_sidereal,
            cos_sidereal * cos_latitude,
            -cos_sidereal * sin_latitude,
        ),
        Vec3::new(
            cos_sidereal,
            sin_sidereal * cos_latitude,
            -sin_sidereal * sin_latitude,
        ),
        Vec3::new(0.0, sin_latitude, cos_latitude),
    ))
    .normalize()
}

fn monotone_slopes(points: &[(f32, f32)]) -> Vec<f32> {
    if points.len() < 2 {
        return vec![0.0; points.len()];
    }
    let secants: Vec<f32> = points
        .windows(2)
        .map(|pair| {
            let dx = pair[1].0 - pair[0].0;
            (pair[1].1 - pair[0].1) / if dx == 0.0 { 1.0e-6 } else { dx }
        })
        .collect();
    let mut slopes = vec![0.0; points.len()];
    slopes[0] = secants[0];
    slopes[points.len() - 1] = secants[secants.len() - 1];
    for index in 1..points.len() - 1 {
        slopes[index] = if secants[index - 1] * secants[index] <= 0.0 {
            0.0
        } else {
            (secants[index - 1] + secants[index]) * 0.5
        };
    }
    for index in 0..secants.len() {
        if secants[index] == 0.0 {
            slopes[index] = 0.0;
            slopes[index + 1] = 0.0;
            continue;
        }
        let a = slopes[index] / secants[index];
        let b = slopes[index + 1] / secants[index];
        let magnitude = a.hypot(b);
        if magnitude > 3.0 {
            let scale = 3.0 / magnitude;
            slopes[index] = scale * a * secants[index];
            slopes[index + 1] = scale * b * secants[index];
        }
    }
    slopes
}

/// Evaluates the editor's Fritsch-Carlson monotone cubic, clamped to `[0, 1]`.
pub fn eval_monotone_curve(curve: &TodCurve, x: f32) -> f32 {
    let mut points = curve.0.clone();
    points.sort_by(|left, right| left.0.total_cmp(&right.0));
    let Some(&first) = points.first() else {
        return x.clamp(0.0, 1.0);
    };
    let last = points[points.len() - 1];
    if x <= first.0 {
        return first.1.clamp(0.0, 1.0);
    }
    if x >= last.0 {
        return last.1.clamp(0.0, 1.0);
    }
    let slopes = monotone_slopes(&points);
    let mut index = 0;
    while index < points.len() - 1 && x > points[index + 1].0 {
        index += 1;
    }
    let width = points[index + 1].0 - points[index].0;
    let width = if width == 0.0 { 1.0e-6 } else { width };
    let t = (x - points[index].0) / width;
    let t2 = t * t;
    let t3 = t2 * t;
    let value = (2.0 * t3 - 3.0 * t2 + 1.0) * points[index].1
        + (t3 - 2.0 * t2 + t) * width * slopes[index]
        + (-2.0 * t3 + 3.0 * t2) * points[index + 1].1
        + (t3 - t2) * width * slopes[index + 1];
    value.clamp(0.0, 1.0)
}

fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let mut year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    year -= if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year
}

fn civil_from_days(days: i64) -> (i32, i32, i32) {
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    (year as i32, month as i32, day as i32)
}

/// Advances the enabled scene clock without creating an authored edit or version bump.
pub fn advance_time_of_day(scene: &mut Scene, delta_seconds: f32) {
    let settings = &mut scene.environment.time_of_day;
    if !settings.enabled
        || settings.day_length_seconds <= 0.0
        || !settings.day_length_seconds.is_finite()
        || !delta_seconds.is_finite()
        || delta_seconds <= 0.0
    {
        return;
    }
    let total = f64::from(settings.time_of_day)
        + f64::from(delta_seconds) / f64::from(settings.day_length_seconds);
    let elapsed_days = total.floor() as i64;
    settings.time_of_day = total.rem_euclid(1.0) as f32;
    if elapsed_days != 0 {
        let days = days_from_civil(settings.year, settings.month, settings.day) + elapsed_days;
        (settings.year, settings.month, settings.day) = civil_from_days(days);
    }
}

#[derive(Clone, Copy)]
struct LunarArguments {
    mean_longitude: f64,
    elongation: f64,
    solar_anomaly: f64,
    lunar_anomaly: f64,
    argument_latitude: f64,
}

#[derive(Clone, Copy)]
struct LongitudeDistanceTerm {
    d: i8,
    m: i8,
    mp: i8,
    f: i8,
    longitude: i32,
    distance: i32,
}

#[derive(Clone, Copy)]
struct LatitudeTerm {
    d: i8,
    m: i8,
    mp: i8,
    f: i8,
    latitude: i32,
}

const LR: [LongitudeDistanceTerm; 60] = [
    lr(0, 0, 1, 0, 6_288_774, -20_905_355),
    lr(2, 0, -1, 0, 1_274_027, -3_699_111),
    lr(2, 0, 0, 0, 658_314, -2_955_968),
    lr(0, 0, 2, 0, 213_618, -569_925),
    lr(0, 1, 0, 0, -185_116, 48_888),
    lr(0, 0, 0, 2, -114_332, -3_149),
    lr(2, 0, -2, 0, 58_793, 246_158),
    lr(2, -1, -1, 0, 57_066, -152_138),
    lr(2, 0, 1, 0, 53_322, -170_733),
    lr(2, -1, 0, 0, 45_758, -204_586),
    lr(0, 1, -1, 0, -40_923, -129_620),
    lr(1, 0, 0, 0, -34_720, 108_743),
    lr(0, 1, 1, 0, -30_383, 104_755),
    lr(2, 0, 0, -2, 15_327, 10_321),
    lr(0, 0, 1, 2, -12_528, 0),
    lr(0, 0, 1, -2, 10_980, 79_661),
    lr(4, 0, -1, 0, 10_675, -34_782),
    lr(0, 0, 3, 0, 10_034, -23_210),
    lr(4, 0, -2, 0, 8_548, -21_636),
    lr(2, 1, -1, 0, -7_888, 24_208),
    lr(2, 1, 0, 0, -6_766, 30_824),
    lr(1, 0, -1, 0, -5_163, -8_379),
    lr(1, 1, 0, 0, 4_987, -16_675),
    lr(2, -1, 1, 0, 4_036, -12_831),
    lr(2, 0, 2, 0, 3_994, -10_445),
    lr(4, 0, 0, 0, 3_861, -11_650),
    lr(2, 0, -3, 0, 3_665, 14_403),
    lr(0, 1, -2, 0, -2_689, -7_003),
    lr(2, 0, -1, 2, -2_602, 0),
    lr(2, -1, -2, 0, 2_390, 10_056),
    lr(1, 0, 1, 0, -2_348, 6_322),
    lr(2, -2, 0, 0, 2_236, -9_884),
    lr(0, 1, 2, 0, -2_120, 5_751),
    lr(0, 2, 0, 0, -2_069, 0),
    lr(2, -2, -1, 0, 2_048, -4_950),
    lr(2, 0, 1, -2, -1_773, 4_130),
    lr(2, 0, 0, 2, -1_595, 0),
    lr(4, -1, -1, 0, 1_215, -3_958),
    lr(0, 0, 2, 2, -1_110, 0),
    lr(3, 0, -1, 0, -892, 3_258),
    lr(2, 1, 1, 0, -810, 2_616),
    lr(4, -1, -2, 0, 759, -1_897),
    lr(0, 2, -1, 0, -713, -2_117),
    lr(2, 2, -1, 0, -700, 2_354),
    lr(2, 1, -2, 0, 691, 0),
    lr(2, -1, 0, -2, 596, 0),
    lr(4, 0, 1, 0, 549, -1_423),
    lr(0, 0, 4, 0, 537, -1_117),
    lr(4, -1, 0, 0, 520, -1_571),
    lr(1, 0, -2, 0, -487, -1_739),
    lr(2, 1, 0, -2, -399, 0),
    lr(0, 0, 2, -2, -381, -4_421),
    lr(1, 1, 1, 0, 351, 0),
    lr(3, 0, -2, 0, -340, 0),
    lr(4, 0, -3, 0, 330, 0),
    lr(2, -1, 2, 0, 327, 0),
    lr(0, 2, 1, 0, -323, 1_165),
    lr(1, 1, -1, 0, 299, 0),
    lr(2, 0, 3, 0, 294, 0),
    lr(2, 0, -1, -2, 0, 8_752),
];

const B: [LatitudeTerm; 60] = [
    b(0, 0, 0, 1, 5_128_122),
    b(0, 0, 1, 1, 280_602),
    b(0, 0, 1, -1, 277_693),
    b(2, 0, 0, -1, 173_237),
    b(2, 0, -1, 1, 55_413),
    b(2, 0, -1, -1, 46_271),
    b(2, 0, 0, 1, 32_573),
    b(0, 0, 2, 1, 17_198),
    b(2, 0, 1, -1, 9_266),
    b(0, 0, 2, -1, 8_822),
    b(2, -1, 0, -1, 8_216),
    b(2, 0, -2, -1, 4_324),
    b(2, 0, 1, 1, 4_200),
    b(2, 1, 0, -1, -3_359),
    b(2, -1, -1, 1, 2_463),
    b(2, -1, 0, 1, 2_211),
    b(2, -1, -1, -1, 2_065),
    b(0, 1, -1, -1, -1_870),
    b(4, 0, -1, -1, 1_828),
    b(0, 1, 0, 1, -1_794),
    b(0, 0, 0, 3, -1_749),
    b(0, 1, -1, 1, -1_565),
    b(1, 0, 0, 1, -1_491),
    b(0, 1, 1, 1, -1_475),
    b(0, 1, 1, -1, -1_410),
    b(0, 1, 0, -1, -1_344),
    b(1, 0, 0, -1, -1_335),
    b(0, 0, 3, 1, 1_107),
    b(4, 0, 0, -1, 1_021),
    b(4, 0, -1, 1, 833),
    b(0, 0, 1, -3, 777),
    b(4, 0, -2, 1, 671),
    b(2, 0, 0, -3, 607),
    b(2, 0, 2, -1, 596),
    b(2, -1, 1, -1, 491),
    b(2, 0, -2, 1, -451),
    b(0, 0, 3, -1, 439),
    b(2, 0, 2, 1, 422),
    b(2, 0, -3, -1, 421),
    b(2, 1, -1, 1, -366),
    b(2, 1, 0, 1, -351),
    b(4, 0, 0, 1, 331),
    b(2, -1, 1, 1, 315),
    b(2, -2, 0, -1, 302),
    b(0, 0, 1, 3, -283),
    b(2, 1, 1, -1, -229),
    b(1, 1, 0, -1, 223),
    b(1, 1, 0, 1, 223),
    b(0, 1, -2, -1, -220),
    b(2, 1, -1, -1, -220),
    b(1, 0, 1, 1, -185),
    b(2, -1, -2, -1, 181),
    b(0, 1, 2, 1, -177),
    b(4, 0, -2, -1, 176),
    b(4, -1, -1, -1, 166),
    b(1, 0, 1, -1, -164),
    b(4, 0, 1, -1, 132),
    b(1, 0, -1, -1, -119),
    b(4, -1, 0, -1, 115),
    b(2, -2, 0, 1, 107),
];

const fn lr(d: i8, m: i8, mp: i8, f: i8, longitude: i32, distance: i32) -> LongitudeDistanceTerm {
    LongitudeDistanceTerm {
        d,
        m,
        mp,
        f,
        longitude,
        distance,
    }
}

const fn b(d: i8, m: i8, mp: i8, f: i8, latitude: i32) -> LatitudeTerm {
    LatitudeTerm {
        d,
        m,
        mp,
        f,
        latitude,
    }
}

fn lunar_arguments(jd: f64) -> LunarArguments {
    let t = (jd - 2_451_545.0) / 36_525.0;
    let t2 = t * t;
    let t3 = t2 * t;
    let t4 = t3 * t;
    LunarArguments {
        mean_longitude: (218.316_447_7 + 481_267.881_234_21 * t - 0.001_578_6 * t2
            + t3 / 538_841.0
            - t4 / 65_194_000.0)
            .to_radians(),
        elongation: (297.850_192_1 + 445_267.111_403_4 * t - 0.001_881_9 * t2 + t3 / 545_868.0
            - t4 / 113_065_000.0)
            .to_radians(),
        solar_anomaly: (357.529_109_2 + 35_999.050_290_9 * t - 0.000_153_6 * t2
            + t3 / 24_490_000.0)
            .to_radians(),
        lunar_anomaly: (134.963_396_4 + 477_198.867_505_5 * t + 0.008_741_4 * t2 + t3 / 69_699.0
            - t4 / 14_712_000.0)
            .to_radians(),
        argument_latitude: (93.272_095 + 483_202.017_523_3 * t
            - 0.003_653_9 * t2
            - t3 / 3_526_000.0
            + t4 / 863_310_000.0)
            .to_radians(),
    }
}

fn term_argument(d: i8, m: i8, mp: i8, f: i8, a: LunarArguments) -> f64 {
    f64::from(d) * a.elongation
        + f64::from(m) * a.solar_anomaly
        + f64::from(mp) * a.lunar_anomaly
        + f64::from(f) * a.argument_latitude
}

fn eccentricity_factor(m: i8, e: f64) -> f64 {
    match m.unsigned_abs() {
        0 => 1.0,
        1 => e,
        _ => e * e,
    }
}

fn moon_ecliptic(jd: f64) -> (f64, f64, f64) {
    let t = (jd - 2_451_545.0) / 36_525.0;
    let a = lunar_arguments(jd);
    let e = 1.0 - 0.002_516 * t - 0.000_007_4 * t * t;
    let mut longitude_sum = 0.0;
    let mut distance_sum = 0.0;
    for term in LR {
        let arg = term_argument(term.d, term.m, term.mp, term.f, a);
        let ef = eccentricity_factor(term.m, e);
        longitude_sum += f64::from(term.longitude) * ef * arg.sin();
        distance_sum += f64::from(term.distance) * ef * arg.cos();
    }
    let mut latitude_sum = 0.0;
    for term in B {
        let arg = term_argument(term.d, term.m, term.mp, term.f, a);
        latitude_sum += f64::from(term.latitude) * eccentricity_factor(term.m, e) * arg.sin();
    }

    let a1 = (119.75 + 131.849 * t).to_radians();
    let a2 = (53.09 + 479_264.29 * t).to_radians();
    let a3 = (313.45 + 481_266.484 * t).to_radians();
    longitude_sum += 3_958.0 * a1.sin()
        + 1_962.0 * (a.mean_longitude - a.argument_latitude).sin()
        + 318.0 * a2.sin();
    latitude_sum += -2_235.0 * a.mean_longitude.sin()
        + 382.0 * a3.sin()
        + 175.0 * (a1 - a.argument_latitude).sin()
        + 175.0 * (a1 + a.argument_latitude).sin()
        + 127.0 * (a.mean_longitude - a.lunar_anomaly).sin()
        - 115.0 * (a.mean_longitude + a.lunar_anomaly).sin();

    let longitude = (a.mean_longitude + longitude_sum * 1.0e-6 * DEG_TO_RAD).rem_euclid(TAU);
    let latitude = latitude_sum * 1.0e-6 * DEG_TO_RAD;
    let distance = 385_000.56 + distance_sum / 1_000.0;
    (longitude, latitude, distance)
}

fn nutation_and_obliquity(jd: f64) -> (f64, f64) {
    let t = (jd - 2_451_545.0) / 36_525.0;
    let omega = (125.044_52 - 1_934.136_261 * t).to_radians();
    let solar_longitude = (280.4665 + 36_000.769_8 * t).to_radians();
    let lunar_longitude = (218.3165 + 481_267.881_3 * t).to_radians();
    let delta_psi_arcsec = -17.2 * omega.sin()
        - 1.32 * (2.0 * solar_longitude).sin()
        - 0.23 * (2.0 * lunar_longitude).sin()
        + 0.21 * (2.0 * omega).sin();
    let delta_epsilon_arcsec = 9.2 * omega.cos()
        + 0.57 * (2.0 * solar_longitude).cos()
        + 0.1 * (2.0 * lunar_longitude).cos()
        - 0.09 * (2.0 * omega).cos();
    let u = t / 100.0;
    let mean_obliquity_arcsec = 84_381.448 - 4_680.93 * u - 1.55 * u.powi(2) + 1_999.25 * u.powi(3)
        - 51.38 * u.powi(4)
        - 249.67 * u.powi(5)
        - 39.05 * u.powi(6)
        + 7.12 * u.powi(7)
        + 27.87 * u.powi(8)
        + 5.79 * u.powi(9)
        + 2.45 * u.powi(10);
    (
        delta_psi_arcsec / 3600.0 * DEG_TO_RAD,
        (mean_obliquity_arcsec + delta_epsilon_arcsec) / 3600.0 * DEG_TO_RAD,
    )
}

/// Computes the topocentric lunar position with the complete Meeus periodic series.
pub fn lunar_position(
    julian_date: f64,
    latitude_degrees: f64,
    longitude_degrees: f64,
    elevation_m: f64,
) -> CelestialPosition {
    let (longitude, latitude, distance_km) = moon_ecliptic(julian_date);
    let (delta_psi, obliquity) = nutation_and_obliquity(julian_date);
    let apparent_longitude = longitude + delta_psi;
    let right_ascension = (apparent_longitude.sin() * obliquity.cos()
        - latitude.tan() * obliquity.sin())
    .atan2(apparent_longitude.cos())
    .rem_euclid(TAU);
    let declination = (latitude.sin() * obliquity.cos()
        + latitude.cos() * obliquity.sin() * apparent_longitude.sin())
    .asin();

    let observer_latitude = latitude_degrees.to_radians();
    let hour_angle =
        (local_sidereal_time(julian_date, longitude_degrees) - right_ascension).rem_euclid(TAU);
    let u = (0.996_647_19 * observer_latitude.tan()).atan();
    let elevation_ratio = elevation_m / (EARTH_EQUATORIAL_RADIUS_KM * 1_000.0);
    let rho_sin_phi = 0.996_647_19 * u.sin() + elevation_ratio * observer_latitude.sin();
    let rho_cos_phi = u.cos() + elevation_ratio * observer_latitude.cos();
    let sin_parallax = (EARTH_EQUATORIAL_RADIUS_KM / distance_km).clamp(-1.0, 1.0);
    let delta_alpha = (-rho_cos_phi * sin_parallax * hour_angle.sin())
        .atan2(declination.cos() - rho_cos_phi * sin_parallax * hour_angle.cos());
    let topocentric_declination = ((declination.sin() - rho_sin_phi * sin_parallax)
        * delta_alpha.cos())
    .atan2(declination.cos() - rho_cos_phi * sin_parallax * hour_angle.cos());
    let topocentric_hour_angle = hour_angle - delta_alpha;

    let east = -topocentric_declination.cos() * topocentric_hour_angle.sin();
    let north = topocentric_declination.sin() * observer_latitude.cos()
        - topocentric_declination.cos() * topocentric_hour_angle.cos() * observer_latitude.sin();
    let up = topocentric_declination.sin() * observer_latitude.sin()
        + topocentric_declination.cos() * topocentric_hour_angle.cos() * observer_latitude.cos();
    CelestialPosition {
        azimuth: east.atan2(north).rem_euclid(TAU),
        elevation: up.clamp(-1.0, 1.0).asin(),
        distance_km,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn julian_epoch_is_exact() {
        let jd = julian_date(CelestialTime {
            year: 2000,
            month: 1,
            day: 1,
            time_of_day: 0.5,
        })
        .expect("valid J2000 date");
        assert!((jd - 2_451_545.0).abs() < 1.0e-9);
    }

    #[test]
    fn solar_position_matches_nrel_reference_case() {
        let position = solar_position(
            CelestialTime {
                year: 2003,
                month: 10,
                day: 17,
                time_of_day: (19.0 + 30.0 / 60.0 + 30.0 / 3600.0) / 24.0,
            },
            39.742_476,
            -105.1786,
            1_830.14,
        )
        .expect("NREL reference date is valid");
        assert!((position.azimuth.to_degrees() - 194.340_24).abs() < 0.001);
        assert!((position.elevation.to_degrees() - 39.872_046).abs() < 0.001);
    }

    #[test]
    fn full_lunar_series_matches_meeus_example() {
        let jd = julian_date(CelestialTime {
            year: 1992,
            month: 4,
            day: 12,
            time_of_day: 0.0,
        })
        .expect("Meeus reference date is valid");
        let (longitude, latitude, distance) = moon_ecliptic(jd);
        assert!((longitude.to_degrees() - 133.162_655).abs() < 0.001);
        assert!((latitude.to_degrees() + 3.229_126).abs() < 0.001);
        assert!((distance - 368_409.7).abs() < 2.0);
    }

    #[test]
    fn horizontal_direction_uses_y_up_north_z() {
        assert!(dir_from_az_el(0.0, 0.0).abs_diff_eq(Vec3::Z, 1.0e-6));
        assert!(dir_from_az_el(PI * 0.5, 0.0).abs_diff_eq(Vec3::X, 1.0e-6));
        assert!(dir_from_az_el(0.0, PI * 0.5).abs_diff_eq(Vec3::Y, 1.0e-6));
    }

    #[test]
    fn monotone_curve_matches_linear_identity_and_clamps_endpoints() {
        let curve = TodCurve(vec![(0.0, 0.0), (0.5, 0.5), (1.0, 1.0)]);
        for sample in [0.0, 0.125, 0.5, 0.875, 1.0] {
            assert!((eval_monotone_curve(&curve, sample) - sample).abs() < 1.0e-6);
        }
        assert_eq!(eval_monotone_curve(&curve, -1.0), 0.0);
        assert_eq!(eval_monotone_curve(&curve, 2.0), 1.0);
    }

    #[test]
    fn advancing_the_clock_rolls_the_gregorian_calendar() {
        let mut scene = Scene::default();
        let settings = &mut scene.environment.time_of_day;
        settings.enabled = true;
        settings.year = 2024;
        settings.month = 2;
        settings.day = 28;
        settings.time_of_day = 0.75;
        settings.day_length_seconds = 4.0;

        advance_time_of_day(&mut scene, 1.0);

        let settings = &scene.environment.time_of_day;
        assert_eq!((settings.year, settings.month, settings.day), (2024, 2, 29));
        assert_eq!(settings.time_of_day, 0.0);
    }

    #[test]
    fn a_paused_clock_does_not_advance() {
        let mut scene = Scene::default();
        scene.environment.time_of_day.enabled = true;
        scene.environment.time_of_day.day_length_seconds = 0.0;
        let before = scene.environment.time_of_day.clone();

        advance_time_of_day(&mut scene, 60.0);

        assert_eq!(scene.environment.time_of_day, before);
    }
}
