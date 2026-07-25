//! The shared wind service: one deterministic, analytically sampled wind field every
//! consumer (clouds, fog, vegetation deformation, physics queries) reads identically.
//!
//! The field is a pure function of its [`WindProfile`] parameters, a world position,
//! and a monotonic simulation time — no retained state, so any thread, pass, or
//! shader mirror evaluates the same velocity for the same inputs. Turbulence is a
//! fixed-phase multiscale sum of analytic gradients; gust fronts are a traveling
//! envelope along the mean direction; height response is a power-law shear profile.

use glam::{DVec3, Vec3};

/// The global wind parameter set one field samples from. Mirrors the authored
/// `SceneEnvironment` wind settings; consumers build it once per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindProfile {
    /// Horizontal direction in degrees, clockwise from world +Z.
    pub orientation: f32,
    /// Mean advection speed in metres per second at the reference height.
    pub speed: f32,
    /// Turbulent fraction of the mean speed (0 = laminar).
    pub gust: f32,
    /// Turbulence octave count (0 = mean advection only).
    pub turbulence_octaves: u32,
    /// Per-octave amplitude falloff in (0, 1].
    pub turbulence_roughness: f32,
    /// Gust-front passage frequency in hertz.
    pub gust_frequency: f32,
    /// Height in metres at which `speed` is authored.
    pub reference_height: f32,
    /// Power-law shear exponent for the height response (0 = uniform).
    pub height_exponent: f32,
    /// Deterministic seed for the turbulence phases.
    pub seed: u32,
}

impl Default for WindProfile {
    fn default() -> Self {
        Self {
            orientation: 0.0,
            speed: 10.0,
            gust: 0.25,
            turbulence_octaves: 3,
            turbulence_roughness: 0.55,
            gust_frequency: 0.15,
            reference_height: 10.0,
            height_exponent: 0.2,
            seed: 0,
        }
    }
}

/// One sampled wind value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindSample {
    /// World-space velocity in metres per second.
    pub velocity: Vec3,
    /// The gust-front envelope 0..1 at this position and time (1 inside a front).
    pub gust_front: f32,
}

/// The influence shape of one placeable local wind source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindSourceKind {
    /// Blows along the source's forward axis.
    #[default]
    Directional,
    /// Blows radially outward from the source position.
    Point,
    /// Circulates tangentially around the source's vertical axis.
    Vortex,
    /// Opposes the global mean flow (a sheltered slow zone).
    Wake,
    /// Scales the global contribution inside the bounds (`strength` is the factor;
    /// 0 is a calm on the inside).
    Volume,
}

/// One resolved local wind source, composited over the global field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalWindSource {
    pub kind: WindSourceKind,
    /// World position in metres.
    pub position: DVec3,
    /// Unit forward axis (directional sources).
    pub direction: Vec3,
    /// Peak speed in metres per second (`Volume`: the global scale factor).
    pub strength: f32,
    /// Influence radius in metres.
    pub radius: f32,
    /// Edge-falloff fraction of the radius in 0..1 (0 = hard edge).
    pub falloff: f32,
}

/// The 0..1 influence weight of a source at a distance (1 inside the un-falloff'd
/// core, fading linearly to the radius edge).
fn source_weight(source: &LocalWindSource, distance: f32) -> f32 {
    if source.radius <= 0.0 || distance >= source.radius {
        return 0.0;
    }
    let inner = source.radius * (1.0 - source.falloff.clamp(0.0, 1.0));
    if distance <= inner || source.radius <= inner {
        1.0
    } else {
        1.0 - (distance - inner) / (source.radius - inner)
    }
}

/// Samples the composed field: the global profile plus every local source. `Volume`
/// sources scale the global term; the rest add their own velocities. Equal inputs
/// sample equal velocities — determinism carries through composition.
#[must_use]
pub fn sample_composed(
    profile: &WindProfile,
    sources: &[LocalWindSource],
    position: DVec3,
    time: f64,
) -> WindSample {
    let global = sample(profile, position, time);
    let mut global_scale = 1.0_f32;
    let mut added = Vec3::ZERO;
    let mean_direction = direction(profile.orientation);
    for source in sources {
        let offset = position - source.position;
        let weight = source_weight(source, offset.length() as f32);
        if weight <= 0.0 {
            continue;
        }
        match source.kind {
            WindSourceKind::Directional => {
                added += source.direction.normalize_or_zero() * source.strength * weight;
            }
            WindSourceKind::Point => {
                let radial = Vec3::new(offset.x as f32, offset.y as f32, offset.z as f32)
                    .normalize_or_zero();
                added += radial * source.strength * weight;
            }
            WindSourceKind::Vortex => {
                let planar = Vec3::new(offset.x as f32, 0.0, offset.z as f32);
                let tangent = Vec3::new(-planar.z, 0.0, planar.x).normalize_or_zero();
                added += tangent * source.strength * weight;
            }
            WindSourceKind::Wake => {
                added -= mean_direction * source.strength * weight;
            }
            WindSourceKind::Volume => {
                global_scale *= 1.0 + (source.strength - 1.0) * weight;
            }
        }
    }
    WindSample {
        velocity: global.velocity * global_scale.max(0.0) + added,
        gust_front: global.gust_front,
    }
}

/// The base spatial wavelength of the coarsest turbulence octave, in metres.
const BASE_WAVELENGTH_M: f32 = 61.0;
/// The advection rate of turbulence features relative to the mean speed.
const TURBULENCE_ADVECTION: f32 = 0.7;
/// The spatial wavelength of a gust front along the mean direction, in metres.
const GUST_FRONT_WAVELENGTH_M: f32 = 190.0;

/// The unit mean-advection direction for an orientation in degrees.
#[must_use]
pub fn direction(orientation_degrees: f32) -> Vec3 {
    let radians = orientation_degrees.to_radians();
    Vec3::new(radians.sin(), 0.0, radians.cos())
}

/// One fixed per-octave phase derived from the seed (deterministic across runs).
fn octave_phase(seed: u32, octave: u32, lane: u32) -> f32 {
    let mut hash = seed
        .wrapping_mul(0x9e37_79b9)
        .wrapping_add(octave.wrapping_mul(0x85eb_ca6b))
        .wrapping_add(lane.wrapping_mul(0xc2b2_ae35));
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7feb_352d);
    hash ^= hash >> 15;
    (hash as f32 / u32::MAX as f32) * core::f32::consts::TAU
}

/// Samples the field at a world position (metres) and monotonic simulation time
/// (seconds). Pure and deterministic: equal inputs sample equal velocities.
#[must_use]
/// The height response of the mean speed: a power-law shear profile of the authored
/// reference speed, clamped at ground level so subterranean samples stay finite.
pub fn shear_factor(profile: &WindProfile, height_m: f32) -> f32 {
    if profile.height_exponent <= 0.0 {
        return 1.0;
    }
    let reference = profile.reference_height.max(0.1);
    (height_m.max(0.0) / reference)
        .max(0.05)
        .powf(profile.height_exponent)
}

pub fn sample(profile: &WindProfile, position: DVec3, time: f64) -> WindSample {
    if profile.speed <= 0.0 {
        return WindSample {
            velocity: Vec3::ZERO,
            gust_front: 0.0,
        };
    }
    let mean_direction = direction(profile.orientation);
    let mean_speed = profile.speed * shear_factor(profile, position.y as f32);

    // The gust-front envelope travels along the mean direction at the mean speed.
    let along = position.x as f32 * mean_direction.x + position.z as f32 * mean_direction.z;
    let front_phase = (along - mean_speed * time as f32) / GUST_FRONT_WAVELENGTH_M
        * core::f32::consts::TAU
        + time as f32 * profile.gust_frequency * core::f32::consts::TAU;
    let gust_front = (front_phase.sin() * 0.5 + 0.5).powi(2);

    // Multiscale turbulence: fixed-phase sinusoid gradients advected with the mean
    // flow. Amplitudes decay by the roughness ratio per octave; the horizontal
    // components dominate with a small vertical lane.
    let mut turbulence = Vec3::ZERO;
    let mut amplitude = 1.0_f32;
    let mut total = 0.0_f32;
    let advected = Vec3::new(
        position.x as f32 - mean_direction.x * mean_speed * TURBULENCE_ADVECTION * time as f32,
        position.y as f32,
        position.z as f32 - mean_direction.z * mean_speed * TURBULENCE_ADVECTION * time as f32,
    );
    for octave in 0..profile.turbulence_octaves {
        let frequency = core::f32::consts::TAU / (BASE_WAVELENGTH_M / (1 << octave) as f32);
        let px = octave_phase(profile.seed, octave, 0);
        let py = octave_phase(profile.seed, octave, 1);
        let pz = octave_phase(profile.seed, octave, 2);
        turbulence += amplitude
            * Vec3::new(
                (advected.z * frequency + px).sin() * (advected.y * frequency * 0.31 + pz).cos(),
                0.35 * (advected.x * frequency * 0.83 + py).sin()
                    * (advected.z * frequency * 0.57 + px).cos(),
                (advected.x * frequency + pz).sin() * (advected.y * frequency * 0.27 + py).cos(),
            );
        total += amplitude;
        amplitude *= profile.turbulence_roughness.clamp(0.0, 1.0);
    }
    if total > 0.0 {
        turbulence /= total;
    }

    let gust_speed = mean_speed * profile.gust;
    let velocity = mean_direction * mean_speed * (1.0 + profile.gust * 0.5 * gust_front)
        + turbulence * gust_speed;
    WindSample {
        velocity,
        gust_front,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> WindProfile {
        WindProfile::default()
    }

    #[test]
    fn equal_inputs_sample_equal_velocities() {
        let a = sample(&profile(), DVec3::new(12.5, 3.0, -40.0), 7.25);
        let b = sample(&profile(), DVec3::new(12.5, 3.0, -40.0), 7.25);
        assert_eq!(a, b);
    }

    #[test]
    fn zero_speed_is_calm() {
        let calm = WindProfile {
            speed: 0.0,
            ..profile()
        };
        let sampled = sample(&calm, DVec3::new(5.0, 2.0, 5.0), 3.0);
        assert_eq!(sampled.velocity, Vec3::ZERO);
    }

    #[test]
    fn height_shear_strengthens_with_altitude() {
        let low = sample(&profile(), DVec3::new(0.0, 1.0, 0.0), 0.0);
        let high = sample(&profile(), DVec3::new(0.0, 80.0, 0.0), 0.0);
        assert!(high.velocity.length() > low.velocity.length());
    }

    #[test]
    fn seeds_decorrelate_turbulence() {
        let reseeded = WindProfile {
            seed: 7,
            ..profile()
        };
        let a = sample(&profile(), DVec3::new(30.0, 5.0, 11.0), 2.0);
        let b = sample(&reseeded, DVec3::new(30.0, 5.0, 11.0), 2.0);
        assert_ne!(a.velocity, b.velocity);
    }

    #[test]
    fn volume_source_calms_its_interior() {
        let shelter = LocalWindSource {
            kind: WindSourceKind::Volume,
            position: DVec3::ZERO,
            direction: Vec3::Z,
            strength: 0.0,
            radius: 20.0,
            falloff: 0.0,
        };
        let inside = sample_composed(&profile(), &[shelter], DVec3::new(1.0, 2.0, 1.0), 4.0);
        assert!(inside.velocity.length() < 1e-4);
        let outside = sample_composed(&profile(), &[shelter], DVec3::new(50.0, 2.0, 50.0), 4.0);
        assert!(outside.velocity.length() > 0.0);
    }

    #[test]
    fn point_source_blows_radially_outward() {
        let calm = WindProfile {
            speed: 0.0,
            ..profile()
        };
        let source = LocalWindSource {
            kind: WindSourceKind::Point,
            position: DVec3::ZERO,
            direction: Vec3::Z,
            strength: 5.0,
            radius: 30.0,
            falloff: 0.0,
        };
        let sampled = sample_composed(&calm, &[source], DVec3::new(10.0, 0.0, 0.0), 0.0);
        assert!(sampled.velocity.x > 4.9);
        assert!(sampled.velocity.z.abs() < 1e-4);
    }

    #[test]
    fn source_weight_fades_to_the_radius_edge() {
        let source = LocalWindSource {
            kind: WindSourceKind::Directional,
            position: DVec3::ZERO,
            direction: Vec3::Z,
            strength: 1.0,
            radius: 10.0,
            falloff: 0.5,
        };
        assert_eq!(source_weight(&source, 2.0), 1.0);
        let mid = source_weight(&source, 7.5);
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(source_weight(&source, 10.0), 0.0);
    }

    #[test]
    fn laminar_profile_points_along_the_mean_direction() {
        let laminar = WindProfile {
            gust: 0.0,
            turbulence_octaves: 0,
            height_exponent: 0.0,
            ..profile()
        };
        let sampled = sample(&laminar, DVec3::new(9.0, 4.0, -2.0), 11.0);
        let expected = direction(laminar.orientation) * laminar.speed;
        assert!((sampled.velocity - expected).length() < 1e-4);
    }
}
