//! Image-based lighting, the visible sky, and reflection probes.
//!
//! Produces an environment cube, nine raw-radiance spherical-harmonic coefficients, a
//! roughness-mipped prefiltered specular cube, and a split-sum BRDF LUT, sampled as the mesh
//! ambient (set 3). Environment sources sit behind [`EnvSource`]; probe captures prefilter local
//! environments into the same set (bindings 3-5). Dynamic refreshes run on a fence-owned
//! submission and blend into a retained front set, so the render loop never stalls on a bake.

mod bake;
mod cubes;
mod live_capture;
mod probes;
mod sky;
mod state;

use std::sync::Arc;

use ash::vk;
use bytemuck::Zeroable;
use saffron_geometry::glam::{UVec4, Vec3, Vec4};
use vk_mem::Alloc;

use crate::descriptors::{Descriptors, MAX_REFLECTION_PROBES};
use crate::frame::MAX_FRAMES_IN_FLIGHT;
use crate::render_graph::{RenderGraph, RgPass, RgResource, RgUsage};
use crate::resources::{Buffer, DeviceResources, GpuTexture};
use crate::{Device, Error, Result, checked};

use bake::*;
use cubes::*;
use live_capture::*;
pub use probes::{ReflectionProbe, ReflectionProbes};
use sky::*;
pub use sky::{Sky, SkyDraw, record_sky};
pub use state::Ibl;
use state::*;

/// The IBL cube / LUT format — `R16G16B16A16_SFLOAT`, sampled and storage-written by the
/// convolution compute passes.
pub const IBL_COLOR_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// Source environment cube resolution per face. (Mirrored as `EnvSize` in `ibl_prefilter.slang` —
/// update both together.)
pub const IBL_ENV_SIZE: u32 = 256;

/// Number of second-order spherical-harmonic coefficients used for sky radiance.
pub const SKY_SH_COEFFICIENTS: u64 = 9;

/// Prefiltered specular cube base resolution per face: 256² mip-0 gives near-mirror metals 4× the
/// texels of a 128² base.
pub const IBL_PREFILTER_SIZE: u32 = 256;

/// Prefiltered specular mip count — `mesh.slang`'s `IblPrefilterMaxMip` must be this − 1.
pub const IBL_PREFILTER_MIPS: u32 = 5;

/// Split-sum BRDF LUT resolution.
pub const IBL_LUT_SIZE: u32 = 256;

/// Atmosphere transmittance LUT width (view-zenith).
pub const ATMOS_TRANSMITTANCE_W: u32 = 256;

/// Atmosphere transmittance LUT height (altitude).
pub const ATMOS_TRANSMITTANCE_H: u32 = 64;

/// Atmosphere isotropic multiple-scattering LUT size.
pub const ATMOS_MULTI_SCATTER_SIZE: u32 = 32;

/// Atmosphere sky-view LUT width (azimuth).
pub const ATMOS_SKY_VIEW_W: u32 = 192;

/// Atmosphere sky-view LUT height (elevation).
pub const ATMOS_SKY_VIEW_H: u32 = 108;

const SUN_REFRESH_ANGLE_RADIANS: f32 = 0.25_f32.to_radians();

/// Solar illuminance above the atmosphere, scaled from 120,000 lux into linear-light units.
pub const SOLAR_ILLUMINANCE_TOA: f32 = 1.2;

/// Full-moon illuminance, scaled from 0.26 lux by the same linear-light factor.
pub const LUNAR_ILLUMINANCE_FULL: f32 = 0.000_002_6;

/// Which shader fills the IBL environment cube before the convolution chain.
///
/// The bake dispatches on it; a missing [`EnvSource::Equirect`] panorama degrades to
/// [`EnvSource::Procedural`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvSource {
    /// `ibl_skygen.slang` from [`SkygenParams`] (the default).
    #[default]
    Procedural,
    /// `ibl_equirect.slang` projecting a user panorama.
    Equirect,
    /// The `atmos_*` LUT chain into `atmos_skygen` (Hillaire 2020).
    Atmosphere,
}

/// Renderer-side mirror of the scene's atmosphere settings (the renderer does not import
/// the scene). A plain aggregate compared memberwise (`!=`) to gate the re-bake.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AtmosphereParams {
    /// Gates the atmosphere LUT chain (false = the source falls back to procedural).
    pub enabled: bool,
    /// Planet radius (km).
    pub planet_radius: f32,
    /// Atmosphere thickness above the surface (km).
    pub atmosphere_height: f32,
    /// Rayleigh scattering coefficients (per channel).
    pub rayleigh_scattering: Vec3,
    /// Rayleigh density scale height (km).
    pub rayleigh_scale_height: f32,
    /// Mie scattering coefficient.
    pub mie_scattering: f32,
    /// Mie density scale height (km).
    pub mie_scale_height: f32,
    /// Mie phase anisotropy `g` (forward-scattering bias).
    pub mie_anisotropy: f32,
    /// Ozone absorption coefficients (per channel).
    pub ozone_absorption: Vec3,
    /// Sun disk angular radius (radians).
    pub sun_disk_angular_radius: f32,
    /// Sun disk intensity multiplier.
    pub sun_disk_intensity: f32,
    /// Moon disk angular radius (radians).
    pub moon_disk_angular_radius: f32,
    /// Moon disk radiance trim.
    pub moon_disk_intensity: f32,
    /// Earthshine contribution on the moon's dark side.
    pub moon_earthshine: f32,
    /// Whether celestial discs evaluate atmosphere transmittance per pixel.
    pub per_pixel_transmittance: bool,
    /// Number of frames over which one complete specular sky capture is distributed.
    pub sky_capture_cadence: f32,
}

impl Default for AtmosphereParams {
    fn default() -> Self {
        Self {
            enabled: false,
            planet_radius: 6360.0,
            atmosphere_height: 100.0,
            rayleigh_scattering: Vec3::new(5.802, 13.558, 33.1),
            rayleigh_scale_height: 8.0,
            mie_scattering: 3.996,
            mie_scale_height: 1.2,
            mie_anisotropy: 0.8,
            ozone_absorption: Vec3::new(0.650, 1.881, 0.085),
            sun_disk_angular_radius: 0.004_65,
            sun_disk_intensity: 1.0,
            moon_disk_angular_radius: 0.004_96,
            moon_disk_intensity: 1.0,
            moon_earthshine: 0.02,
            per_pixel_transmittance: false,
            sky_capture_cadence: 9.0,
        }
    }
}

/// Inputs that drive the procedural-sky bake (`ibl_skygen`). The sun follows the scene's
/// directional light, so a re-bake re-tints the visible sky AND the IBL together. Derives
/// [`PartialEq`] so the "did the inputs change" check is a `!=`, not a hand-written
/// memberwise compare.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkygenParams {
    /// Direction TO the sun (= −lightDir); the shader normalizes.
    pub sun_dir: Vec3,
    /// Sun intensity multiplier.
    pub sun_intensity: f32,
    /// Sun color (RGB).
    pub sun_color: Vec3,
    /// Direction TO the moon (= −lightDir); the shader normalizes.
    pub moon_dir: Vec3,
    /// Moon light intensity trim.
    pub moon_intensity: f32,
    /// Geometric illuminated fraction of the lunar disc.
    pub moon_illuminated_fraction: f32,
    /// Physically based source params; [`AtmosphereParams::enabled`] gates the LUT chain.
    pub atmosphere: AtmosphereParams,
}

impl Default for SkygenParams {
    fn default() -> Self {
        Self {
            sun_dir: Vec3::new(0.5, 1.0, 0.3),
            sun_intensity: 1.0,
            sun_color: Vec3::ONE,
            moon_dir: Vec3::new(-0.5, -1.0, -0.3),
            moon_intensity: 0.0,
            moon_illuminated_fraction: 0.0,
            atmosphere: AtmosphereParams::default(),
        }
    }
}

/// The visible-sky settings the host pushes each frame.
/// Carried as POD so the renderer never imports the scene; `mode` matches `SkyMode`'s
/// values (0 = Color, 1 = Texture, 2 = Procedural).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkyRenderSettings {
    /// 0 = Color (flat fill), 1 = Texture (bindless panorama), 2 = Procedural (env cube),
    /// 3 = the fixed studio gradient (see [`SKY_MODE_THUMBNAIL_GRADIENT`]).
    pub mode: u32,
    /// Color-mode flat fill (also the sky pass's clear color).
    pub clear_color: Vec3,
    /// Overall sky intensity (applied by the visible-sky pass, not baked).
    pub intensity: f32,
    /// Per-frame artist tint applied to the visible sky.
    pub tint: Vec3,
    /// Yaw rotation of the lookup around world up (radians).
    pub rotation: f32,
    /// Whether the visible-sky pass runs at all.
    pub visible: bool,
    /// Bindless panorama slot (Texture mode).
    pub texture_index: u32,
    /// Equatorial night-content orientation and radiance controls.
    pub night: NightSkyParams,
}

/// Per-frame night-sky controls derived by the time-of-day driver.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NightSkyParams {
    /// Quaternion rotating J2000 equatorial directions into the scene's Y-up world frame.
    pub world_from_equatorial: Vec4,
    /// BSC5 point-source radiance scale.
    pub star_intensity: f32,
    /// Milky Way cube radiance scale.
    pub milky_way_intensity: f32,
    /// Atmosphere height used to sample the transmittance LUT.
    pub atmosphere_height: f32,
    /// Whether atmosphere extinction applies to night content.
    pub atmosphere_live: bool,
}

impl Default for NightSkyParams {
    fn default() -> Self {
        Self {
            world_from_equatorial: Vec4::new(0.0, 0.0, 0.0, 1.0),
            star_intensity: 0.0,
            milky_way_intensity: 0.0,
            atmosphere_height: 100.0,
            atmosphere_live: false,
        }
    }
}

/// The visible-sky mode that draws the fixed neutral studio gradient (`sky.slang` mode 3): a
/// backdrop independent of the environment, forced on the offscreen thumbnail view so every subject
/// — including a chrome ball reflecting a dark HDRI — reads as a clean silhouette. Never a scene
/// `sky_mode`; the renderer overrides the submitted mode for the thumbnail draw only.
pub const SKY_MODE_THUMBNAIL_GRADIENT: u32 = 3;

impl Default for SkyRenderSettings {
    fn default() -> Self {
        Self {
            mode: 2,
            clear_color: Vec3::new(0.05, 0.06, 0.08),
            intensity: 1.0,
            tint: Vec3::ONE,
            rotation: 0.0,
            visible: true,
            texture_index: 0,
            night: NightSkyParams::default(),
        }
    }
}

/// A per-frame snapshot of one reflection-probe component, passed from the host without
/// the renderer depending on the scene. `dirty` arms a (re)capture.
#[derive(Debug, Clone, Copy)]
pub struct ReflectionProbeUpload {
    /// Owning entity id (the capture re-uses the slot when re-armed).
    pub entity: u64,
    /// World-space origin (the entity's translation).
    pub origin: Vec3,
    /// Influence radius (world units).
    pub influence_radius: f32,
    /// Specular intensity multiplier.
    pub intensity: f32,
    /// Whether to box-project the cube (parallax-corrected reflections).
    pub box_projection: bool,
    /// Box half-extents (box-projection mode).
    pub box_extent: Vec3,
    /// Explicitly arms a (re)capture this frame.
    pub dirty: bool,
}

impl Default for ReflectionProbeUpload {
    fn default() -> Self {
        Self {
            entity: 0,
            origin: Vec3::ZERO,
            influence_radius: 10.0,
            intensity: 1.0,
            box_projection: false,
            box_extent: Vec3::splat(10.0),
            dirty: false,
        }
    }
}

/// The per-probe metadata record the mesh fragment reads (IBL set binding 5 SSBO). std430:
/// three 16-byte vec4/uvec4 blocks.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ProbeMetaGpu {
    /// `xyz` world origin, `w` influence radius.
    pub origin_radius: Vec4,
    /// `xyz` box half-extents, `w` intensity.
    pub extent_intensity: Vec4,
    /// `x` valid (1/0), `y` box-projection (1/0), `zw` reserved.
    pub flags: UVec4,
}

const _: () = assert!(size_of::<ProbeMetaGpu>() == 48);

/// The push the procedural-skygen compute reads (`ibl_skygen.slang`): two vec4s.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkygenPush {
    /// `xyz` direction to the sun, `w` sun intensity.
    sun_dir: Vec4,
    /// `rgb` sun color, `a` unused.
    sun_color: Vec4,
}

/// The push the equirect-projection compute reads (`ibl_equirect.slang`): one vec4
/// (`x` rotation, `y` intensity). The IBL bakes the raw panorama; the visible-sky pass
/// applies the scene's rotation/intensity itself.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EquirectPush {
    params: Vec4,
}

/// The push the atmosphere LUT + skygen passes share (`atmos_*.slang`): seven vec4s packing
/// [`AtmosphereParams`] + the celestial lights.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AtmosPush {
    /// `xyz` dir to sun, `w` sun intensity.
    sun_dir: Vec4,
    /// `xyz` rayleigh scattering, `w` rayleigh scale height.
    rayleigh: Vec4,
    /// `xyz` ozone absorption, `w` mie scattering.
    ozone: Vec4,
    /// `x` planet radius, `y` atmosphere height, `z` mie scale height, `w` mie anisotropy.
    params0: Vec4,
    /// `x` sun-disk angular radius, `y` sun-disk intensity, `z` camera altitude,
    /// `w` illuminated lunar fraction.
    params1: Vec4,
    /// `xyz` dir to moon, `w` moon intensity trim.
    moon: Vec4,
    /// `x` angular radius, `y` disk trim, `z` earthshine, `w` per-pixel transmittance.
    moon_disk: Vec4,
}

impl AtmosPush {
    fn new(sky: &SkygenParams) -> Self {
        let a = &sky.atmosphere;
        Self {
            sun_dir: sky.sun_dir.normalize_or_zero().extend(sky.sun_intensity),
            rayleigh: a.rayleigh_scattering.extend(a.rayleigh_scale_height),
            ozone: a.ozone_absorption.extend(a.mie_scattering),
            params0: Vec4::new(
                a.planet_radius,
                a.atmosphere_height,
                a.mie_scale_height,
                a.mie_anisotropy,
            ),
            params1: Vec4::new(
                a.sun_disk_angular_radius,
                a.sun_disk_intensity,
                0.0,
                sky.moon_illuminated_fraction,
            ),
            moon: sky.moon_dir.normalize_or_zero().extend(sky.moon_intensity),
            moon_disk: Vec4::new(
                a.moon_disk_angular_radius,
                a.moon_disk_intensity,
                a.moon_earthshine,
                if a.per_pixel_transmittance { 1.0 } else { 0.0 },
            ),
        }
    }
}

/// Evaluates the atmosphere transmittance toward a celestial light for a ground observer.
/// The midpoint integral mirrors `atmos_transmittance.slang` without a GPU readback.
pub fn sun_transmittance(atmos: &AtmosphereParams, dir_to_light: Vec3) -> Vec3 {
    let direction = dir_to_light.normalize_or_zero();
    if direction == Vec3::ZERO {
        return Vec3::ZERO;
    }

    let radius = atmos.planet_radius + 0.5;
    let mu = direction.y.clamp(-1.0, 1.0);
    let ground_discriminant =
        radius * radius * (mu * mu - 1.0) + atmos.planet_radius * atmos.planet_radius;
    if mu < 0.0 && ground_discriminant >= 0.0 {
        return Vec3::ZERO;
    }

    let top = atmos.planet_radius + atmos.atmosphere_height;
    let top_discriminant = radius * radius * (mu * mu - 1.0) + top * top;
    let distance = (-radius * mu + top_discriminant.max(0.0).sqrt()).max(0.0);
    const STEPS: usize = 40;
    let dt = distance / STEPS as f32;
    let mut optical_depth = Vec3::ZERO;
    for step in 0..STEPS {
        let t = (step as f32 + 0.5) * dt;
        let sample_radius = (radius * radius + t * t + 2.0 * radius * t * mu)
            .max(0.0)
            .sqrt();
        let height = sample_radius - atmos.planet_radius;
        let rayleigh_density = (-height.max(0.0) / atmos.rayleigh_scale_height).exp();
        let mie_density = (-height.max(0.0) / atmos.mie_scale_height).exp();
        let ozone_density = (1.0 - (height - 25.0).abs() / 15.0).max(0.0);
        let extinction = atmos.rayleigh_scattering * rayleigh_density
            + Vec3::splat(atmos.mie_scattering * 1.1 * mie_density)
            + atmos.ozone_absorption * ozone_density;
        optical_depth += extinction * dt;
    }
    (-optical_depth * 1.0e-3).exp()
}

/// The push the visible-sky fragment reads (`sky.slang`'s `SkyPush`): the inverse
/// view-projection + params + clear color.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkyPush {
    inv_view_proj: saffron_geometry::glam::Mat4,
    /// `x` intensity, `y` rotation, `z` mode, `w` texture index.
    params: Vec4,
    /// `rgb` Color-mode fill.
    clear_color: Vec4,
    /// Quaternion rotating J2000 equatorial directions into world space.
    world_from_equatorial: Vec4,
    /// `x` Milky Way intensity, `y` atmosphere height, `z` atmosphere live, `w` reserved.
    night: Vec4,
}

const _: () = assert!(size_of::<SkyPush>() == 128);

#[cfg(test)]
mod tests;
