//! Scene-wide environment/sky state and the asset catalog.
//!
//! [`SceneEnvironment`] is global frame state (no transform, not picked, not in the
//! hierarchy) so it lives on the [`crate::Scene`] rather than as an entity component.
//! [`AssetCatalog`] maps imported assets by id; the asset layer constructs it and hands
//! the scene a shared read-only handle. The catalog is never serialized with the scene
//! (`Scene.catalog`).

use std::collections::HashMap;

use glam::Vec3;

use saffron_core::Uuid;

/// How the visible sky background is produced.
///
/// `Color` = a flat fill; `Texture` = an equirectangular panorama asset; `Procedural`
/// = the renderer's baked procedural-sky environment cube (the same cube that feeds IBL,
/// so background and lighting match).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkyMode {
    /// A flat color fill.
    Color,
    /// An equirectangular panorama asset.
    Texture,
    /// The renderer's baked procedural-sky cube (the default).
    #[default]
    Procedural,
}

/// Physically based atmosphere parameters (Hillaire 2020).
///
/// When enabled, the atmosphere LUT chain replaces the gradient sky as the env-cube
/// source, so the visible sky and the IBL convolutions both become the atmosphere.
/// Coefficients are in `1/Mm` at sea level; lengths in km.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmosphereSettings {
    /// Whether the atmosphere model drives the env cube.
    pub enabled: bool,
    /// Planet radius (km).
    pub planet_radius: f32,
    /// Atmosphere thickness (km).
    pub atmosphere_height: f32,
    /// Rayleigh scattering coefficients (`1/Mm`).
    pub rayleigh_scattering: Vec3,
    /// Rayleigh density scale height (km).
    pub rayleigh_scale_height: f32,
    /// Mie scattering coefficient (`1/Mm`).
    pub mie_scattering: f32,
    /// Mie density scale height (km).
    pub mie_scale_height: f32,
    /// Mie phase anisotropy.
    pub mie_anisotropy: f32,
    /// Ozone absorption coefficients (`1/Mm`).
    pub ozone_absorption: Vec3,
    /// Sun disk angular radius (radians).
    pub sun_disk_angular_radius: f32,
    /// Sun disk intensity.
    pub sun_disk_intensity: f32,
    /// Moon disk angular radius (radians).
    pub moon_disk_angular_radius: f32,
    /// Moon disk radiance trim.
    pub moon_disk_intensity: f32,
    /// Earthshine contribution on the moon's dark side.
    pub moon_earthshine: f32,
    /// Whether celestial discs evaluate atmosphere transmittance per pixel.
    pub per_pixel_transmittance: bool,
    /// Frames over which the live specular sky capture reconverges.
    pub sky_capture_cadence: f32,
}

impl Default for AtmosphereSettings {
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

/// The fog backend: the always-on analytic closed form, or the frustum-aligned froxel volumetric
/// pipeline. In `Volumetric` the analytic height density is injected as the froxel base medium — it
/// is never applied twice (the two backends share one transmittance ledger, no double-count).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FogMode {
    /// The Phase-1 closed-form exponential height/distance integral (the default).
    #[default]
    Analytic,
    /// The Wronski/Hillaire froxel inject → integrate → composite path (shadowed god-rays).
    Volumetric,
}

/// The froxel-grid quality tier for volumetric fog: how many froxels the volume carries. Z is the
/// expensive axis (per-slice light eval), so `low`/`medium` share the Z count and only `high` doubles
/// it; XY steps up for tile density.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FogQuality {
    /// `128×72×64` — the coarse tier.
    Low,
    /// `160×90×64` — the default.
    #[default]
    Medium,
    /// `160×90×128` — the full-resolution grid.
    High,
}

/// Scene-wide analytic height & distance fog (exponential-density closed form).
///
/// Composites into scene-linear HDR before bloom. The broad layer plus an optional
/// ground layer sum into one optical depth; `directional_*` adds a sun-through-haze lobe.
/// The world up axis is `+Y`, so height is measured along `worldPos.y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogSettings {
    /// Whether the fog composite runs.
    pub enabled: bool,
    /// The fog backend (analytic closed form vs. froxel volumetric).
    pub mode: FogMode,
    /// The froxel-grid quality tier for the volumetric path (grid dimensions).
    pub quality: FogQuality,
    /// Temporal history blend: the fresh-sample weight per frame for the froxel reprojection
    /// (`0.05` default — 95% carried from last frame's linear scatter). `0` freezes the volume.
    pub history_blend: f32,
    /// Clamp the reprojected history to a band of the fresh sample (firefly / ghost suppression for
    /// fast-moving lights). Off by default.
    pub neighborhood_clamp: bool,
    /// Cap each light's per-froxel in-scatter before accumulation (`0` = off) — kills the
    /// single-froxel spike a bright light grazing a shadow edge injects.
    pub light_clamp: f32,
    /// Constant scattering-medium extinction floor for the volumetric path (`sigma_t` base).
    pub base_density: f32,
    /// Single-scattering albedo for the volumetric path (`sigma_s = albedo * sigma_t`).
    pub scatter_albedo: f32,
    /// Henyey-Greenstein phase anisotropy `g` for the volumetric path (forward-scattering `g > 0`).
    pub phase_g: f32,
    /// Broad-layer sigma at `height`.
    pub density: f32,
    /// In-scatter tint (multiplied by the sky-view LUT when the atmosphere is live).
    pub albedo: Vec3,
    /// World-up reference height of the broad layer.
    pub height: f32,
    /// Exponential density falloff with world-up distance.
    pub height_falloff: f32,
    /// Fog begins this far from the eye.
    pub start_distance: f32,
    /// Clamps `1 - transmittance`.
    pub max_opacity: f32,
    /// Constant in-medium emission.
    pub emissive: Vec3,
    /// Sun-through-haze lobe color.
    pub directional_color: Vec3,
    /// Lobe sharpness (~4..64).
    pub directional_exponent: f32,
    /// Ground-haze layer sigma; `0` disables it.
    pub layer2_density: f32,
    /// Ground-haze exponential density falloff.
    pub layer2_falloff: f32,
    /// Ground-haze world-up reference height.
    pub layer2_height: f32,
    /// Tint distant geometry with Hillaire-2020 aerial perspective (needs an active atmosphere; the
    /// atmosphere's `enabled` gates whether the LUTs exist, so this is a no-op without one).
    pub aerial_perspective: bool,
    /// Aerial-perspective strength multiplier (scales the atmosphere in-scatter written to the AP
    /// volume; a coherence knob, not an exposure control).
    pub aerial_intensity: f32,
}

impl Default for FogSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: FogMode::Analytic,
            quality: FogQuality::Medium,
            history_blend: 0.05,
            neighborhood_clamp: false,
            light_clamp: 0.0,
            base_density: 0.02,
            scatter_albedo: 0.9,
            phase_g: 0.6,
            density: 0.02,
            albedo: Vec3::new(0.5, 0.6, 0.7),
            height: 0.0,
            height_falloff: 0.2,
            start_distance: 0.0,
            max_opacity: 1.0,
            emissive: Vec3::ZERO,
            directional_color: Vec3::new(1.0, 0.9, 0.7),
            directional_exponent: 8.0,
            layer2_density: 0.0,
            layer2_falloff: 0.5,
            layer2_height: 0.0,
            aerial_perspective: false,
            aerial_intensity: 1.0,
        }
    }
}

/// Scene-wide volumetric cloud shape, lighting, and reconstruction controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudSettings {
    /// Whether the cloud density field is evaluated.
    pub enabled: bool,
    /// Global coverage applied to the weather map.
    pub coverage: f32,
    /// Height-profile blend from stratus through cumulus to cumulonimbus.
    pub cloud_type: f32,
    /// Precipitation carried by the weather map.
    pub precipitation: f32,
    /// Cumulonimbus spread near the layer top.
    pub anvil_bias: f32,
    /// Cloud-layer bottom altitude in world metres.
    pub layer_altitude: f32,
    /// Cloud-layer thickness in metres.
    pub layer_height: f32,
    /// World-to-base-noise frequency.
    pub base_scale: f32,
    /// World-to-detail-noise frequency.
    pub detail_scale: f32,
    /// Detail erosion strength.
    pub detail_strength: f32,
    /// Curl-warp displacement in world metres.
    pub curl_strength: f32,
    /// World-XZ-to-weather-map frequency.
    pub weather_scale: f32,
    /// Weather-map sampling offset.
    pub weather_offset: Vec3,
    /// Painted weather-map texture, or zero for procedural weather.
    pub weather_texture: Uuid,
    /// Maximum adaptive view-ray samples.
    pub primary_steps: u32,
    /// Cone-march samples toward the sun.
    pub light_steps: u32,
    /// Water-droplet diameter in micrometres for the analytic Mie phase fit.
    pub droplet_diameter: f32,
    /// Fresh-sample weight used by temporal reconstruction.
    pub temporal_factor: f32,
    /// Whether the density-integrated cloud shadow is rendered and consumed.
    pub cast_cloud_shadows: bool,
    /// Cloud-shadow strength for cloud self-shadowing and volumetric fog.
    pub cloud_shadow_strength: f32,
    /// Cloud-shadow strength on opaque surfaces.
    pub cloud_shadow_on_surface_strength: f32,
}

impl Default for CloudSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            coverage: 0.5,
            cloud_type: 0.4,
            precipitation: 0.0,
            anvil_bias: 0.0,
            layer_altitude: 1500.0,
            layer_height: 2500.0,
            base_scale: 8e-5,
            detail_scale: 1e-3,
            detail_strength: 0.35,
            curl_strength: 120.0,
            weather_scale: 2e-5,
            weather_offset: Vec3::ZERO,
            weather_texture: Uuid(0),
            primary_steps: 64,
            light_steps: 6,
            droplet_diameter: 20.0,
            temporal_factor: 0.1,
            cast_cloud_shadows: true,
            cloud_shadow_strength: 1.0,
            cloud_shadow_on_surface_strength: 1.0,
        }
    }
}

/// Scene-wide horizontal wind shared by atmospheric and visual systems.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindSettings {
    /// Horizontal direction in degrees, clockwise from world +Z.
    pub orientation: f32,
    /// Mean advection speed in metres per second at the reference height.
    pub speed: f32,
    /// Divergence-free turbulent warp amplitude (fraction of the mean speed).
    pub gust: f32,
    /// Turbulence octave count (0 = mean advection only).
    pub turbulence_octaves: u32,
    /// Per-octave turbulence amplitude falloff in (0, 1].
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

impl WindSettings {
    /// The sampling profile these settings author (field-for-field).
    #[must_use]
    pub fn profile(&self) -> saffron_wind::WindProfile {
        saffron_wind::WindProfile {
            orientation: self.orientation,
            speed: self.speed,
            gust: self.gust,
            turbulence_octaves: self.turbulence_octaves,
            turbulence_roughness: self.turbulence_roughness,
            gust_frequency: self.gust_frequency,
            reference_height: self.reference_height,
            height_exponent: self.height_exponent,
            seed: self.seed,
        }
    }
}

impl Default for WindSettings {
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

/// Monotone-cubic control points for a time-of-day appearance channel.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TodCurve(pub Vec<(f32, f32)>);

impl TodCurve {
    /// Whether this curve owns its target value.
    pub fn is_active(&self) -> bool {
        !self.0.is_empty()
    }
}

/// Master and per-channel curves for the time-of-day RGB tint.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TodTintCurve {
    /// Overall tint multiplier.
    pub master: TodCurve,
    /// Red-channel multiplier.
    pub red: TodCurve,
    /// Green-channel multiplier.
    pub green: TodCurve,
    /// Blue-channel multiplier.
    pub blue: TodCurve,
}

/// Scene-owned calendar, ephemeris locale, playback, and appearance curves.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeOfDaySettings {
    /// Whether the time-of-day driver is active.
    pub enabled: bool,
    /// Whether authored light directions override the ephemeris.
    pub manual_override: bool,
    /// Normalized UTC time within the date (`0` midnight, `0.5` noon).
    pub time_of_day: f32,
    /// Gregorian year.
    pub year: i32,
    /// Gregorian month.
    pub month: i32,
    /// Gregorian day.
    pub day: i32,
    /// Observer latitude in degrees.
    pub latitude: f32,
    /// Observer east-positive longitude in degrees.
    pub longitude: f32,
    /// Real seconds per simulated day; non-positive values pause playback.
    pub day_length_seconds: f32,
    /// Tonemap exposure in EV, indexed by normalized sun elevation.
    pub exposure_curve: TodCurve,
    /// RGB sky and ambient tint, indexed by normalized sun elevation.
    pub tint_curve: TodTintCurve,
    /// Cloud coverage indexed by normalized sun elevation.
    pub coverage_curve: TodCurve,
    /// Cloud type indexed by normalized sun elevation.
    pub cloud_type_curve: TodCurve,
}

impl Default for TimeOfDaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            manual_override: false,
            time_of_day: 0.5,
            year: 2025,
            month: 6,
            day: 21,
            latitude: 0.0,
            longitude: 0.0,
            day_length_seconds: 600.0,
            exposure_curve: TodCurve::default(),
            tint_curve: TodTintCurve::default(),
            coverage_curve: TodCurve::default(),
            cloud_type_curve: TodCurve::default(),
        }
    }
}

/// Scene-wide environment / sky state.
///
/// The renderer resolves it into sky render settings each frame.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneEnvironment {
    /// How the sky background is produced.
    pub sky_mode: SkyMode,
    /// Color-mode fill and clear fallback.
    pub clear_color: Vec3,
    /// Texture-mode panorama asset (`0` = none).
    pub sky_texture: Uuid,
    /// Sky intensity multiplier.
    pub sky_intensity: f32,
    /// Yaw radians applied to the sky lookup.
    pub sky_rotation: f32,
    /// Reserved exposure (tonemap exposure is set via the renderer).
    pub exposure: f32,
    /// Whether the sky background draws.
    pub visible: bool,
    /// Drive fallback ambient from `ambient_color`.
    pub use_sky_for_ambient: bool,
    /// Non-IBL fallback ambient tint.
    pub ambient_color: Vec3,
    /// Fallback ambient intensity.
    pub ambient_intensity: f32,
    /// Physically based env-cube source (off = gradient).
    pub atmosphere: AtmosphereSettings,
    /// Analytic height & distance fog composited into scene-linear HDR before bloom.
    pub fog: FogSettings,
    /// Volumetric cloud shape and weather controls.
    pub cloud: CloudSettings,
    /// Shared global wind field.
    pub wind: WindSettings,
    /// Calendar-driven celestial directions and appearance curves.
    pub time_of_day: TimeOfDaySettings,
}

impl Default for SceneEnvironment {
    fn default() -> Self {
        Self {
            sky_mode: SkyMode::Procedural,
            clear_color: Vec3::new(0.05, 0.06, 0.08),
            sky_texture: Uuid(0),
            sky_intensity: 1.0,
            sky_rotation: 0.0,
            exposure: 1.0,
            visible: true,
            use_sky_for_ambient: true,
            ambient_color: Vec3::ONE,
            ambient_intensity: 0.15,
            atmosphere: AtmosphereSettings::default(),
            fog: FogSettings::default(),
            cloud: CloudSettings::default(),
            wind: WindSettings::default(),
            time_of_day: TimeOfDaySettings::default(),
        }
    }
}

/// A project asset's kind.
///
/// A model imported and baked to a mesh, a texture, an animation clip, a native authored asset,
/// or a `.smodel` container.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AssetType {
    /// A baked mesh (the default).
    #[default]
    Mesh,
    /// A texture.
    Texture,
    /// An asset of no specific engine kind.
    Other,
    /// An animation clip.
    Animation,
    /// A `.smat` material.
    Material,
    /// A `.smodel` container (parent of its embedded mesh/material/texture sub-assets).
    Model,
    /// A creative 3D look-up table (`.cube` import or a baked `.slut`).
    Lut,
    /// A complete reusable scene environment (`.senv`).
    Environment,
    /// A complete plant-family source and normalized intrinsic payload (`.splant`).
    Plant,
    /// A root biome or reusable typed biome graph module (`.sbiome`).
    Biome,
    /// A sparse authored vegetation-map package (`.svegmap`).
    VegetationMap,
}

/// How a texture's bytes are interpreted on upload.
///
/// Recovered from a container chunk flag (embedded) or a `.smeta` sidecar (standalone).
/// `Auto` defers the choice to a heuristic at scan time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Colorspace {
    /// Defer to a scan-time heuristic (the default).
    #[default]
    Auto,
    /// sRGB-encoded.
    Srgb,
    /// Linear-encoded.
    Linear,
    /// HDR float.
    Hdr,
}

/// A texture's semantic role — what surface channel it feeds.
///
/// Inferred from the filename at scan/import, or supplied authoritatively by an import
/// connector (Poly Haven, ambientCG). Drives two things: the preview routing (which slot of the
/// preview ball material a lone texture is shown through) and the colorspace policy for an
/// imported/foreign file (color maps → sRGB, data maps → linear, HDR → float).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextureRole {
    /// Role not recognized (the default) — treated as a plain color map for display.
    #[default]
    Unknown,
    /// Base color / albedo (sRGB).
    Albedo,
    /// Tangent-space normal map (linear).
    Normal,
    /// Roughness (linear).
    Roughness,
    /// Metallic (linear).
    Metallic,
    /// Ambient occlusion (linear).
    Ao,
    /// Height / displacement (linear).
    Height,
    /// Emissive (sRGB).
    Emissive,
    /// Opacity / alpha mask (linear).
    Opacity,
    /// Packed occlusion-roughness-metallic / ARM (linear).
    Orm,
    /// Glossiness (linear).
    Gloss,
    /// HDR environment / equirectangular map (float).
    Hdri,
}

/// Where an imported asset came from and under what license.
///
/// Captured at import for assets pulled from an online store connector so the
/// attribution travels with the asset (CC-BY / Sketchfab require it). Absent for
/// hand-imported local assets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attribution {
    /// Canonical license id (`cc0`, `cc-by`, `cc-by-sa`, …).
    pub license_id: String,
    /// Whether the license requires visible attribution (CC-BY / Sketchfab).
    pub requires_attribution: bool,
    /// Canonical license url.
    pub license_url: String,
    /// The asset author / creator.
    pub author: String,
    /// The asset's page on the source service.
    pub source_url: String,
    /// The connector id the asset came from (`polyhaven`, `sketchfab`, …).
    pub store_id: String,
}

/// A catalog entry for one project asset.
///
/// Carries a human name (UTF-8, renameable) and the relative path to the baked
/// `.smesh` / copied texture under the asset root. A sub-asset's `id` is its stable
/// sub-id (unique across the catalog).
#[derive(Clone, Debug, PartialEq)]
pub struct AssetEntry {
    /// The asset id (a sub-asset's stable sub-id).
    pub id: Uuid,
    /// The human-readable name.
    pub name: String,
    /// The asset kind.
    pub asset_type: AssetType,
    /// Sub-asset: the owning `.smodel`'s path; standalone: its own file.
    pub path: String,
    /// The catalog folder the entry lives in.
    pub folder: String,
    /// Texture: decode as linear float (`.hdr`); else sRGB RGBA8.
    pub hdr: bool,
    /// Texture: upload as a linear RGBA8 format (metallic-roughness), not sRGB.
    pub linear: bool,
    /// Animation: clip length in seconds (`0` for non-animation entries).
    pub duration: f32,
    /// Animation: animated joint-channel count (`0` for non-animation entries).
    pub tracks: i32,
    /// Belongs to a rigged `.smodel` (the container has a skin); scan-derived.
    pub rigged: bool,
    /// `0` = standalone file; else the owning model's id.
    pub container: Uuid,
    /// TOC chunk index inside the container (`-1` = standalone / n/a).
    pub chunk: i32,
    /// Texture: how its bytes are interpreted on upload.
    pub colorspace: Colorspace,
    /// Texture: its semantic role (albedo/normal/roughness/…), for preview routing.
    pub role: TextureRole,
    /// Source/license, set when the asset was imported from an online store.
    pub attribution: Option<Attribution>,
    /// FNV-1a hash of the asset's baked content, the content-addressed thumbnail cache
    /// key; `0` when unknown (materials key on resolved state instead, and legacy rows
    /// backfill lazily).
    pub content_hash: u64,
}

impl Default for AssetEntry {
    fn default() -> Self {
        Self {
            id: Uuid(0),
            name: String::new(),
            asset_type: AssetType::Mesh,
            path: String::new(),
            folder: String::new(),
            hdr: false,
            linear: false,
            duration: 0.0,
            tracks: 0,
            rigged: false,
            container: Uuid(0),
            chunk: -1,
            colorspace: Colorspace::Auto,
            role: TextureRole::Unknown,
            attribution: None,
            content_hash: 0,
        }
    }
}

/// The catalog of imported assets a scene draws from.
///
/// The asset layer constructs the real catalog and hands the scene a shared, read-only
/// handle (`Option<Arc<AssetCatalog>>`); it is never serialized with the scene. `by_id`
/// is an index map from id to position in `entries`, rebuilt by [`AssetCatalog::put`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetCatalog {
    /// The catalog entries.
    pub entries: Vec<AssetEntry>,
    /// The catalog folders.
    pub folders: Vec<String>,
    /// Index map from asset id to position in `entries`.
    pub by_id: HashMap<u64, usize>,
}

impl AssetCatalog {
    /// The entry carrying `id`, or `None`.
    #[must_use]
    pub fn find(&self, id: Uuid) -> Option<&AssetEntry> {
        self.by_id.get(&id.value()).map(|&i| &self.entries[i])
    }

    /// Inserts or replaces the entry for its id.
    ///
    /// An entry whose id already exists overwrites it in place; a new id appends and
    /// records its position in `by_id`.
    pub fn put(&mut self, entry: AssetEntry) {
        if let Some(&i) = self.by_id.get(&entry.id.value()) {
            self.entries[i] = entry;
            return;
        }
        self.by_id.insert(entry.id.value(), self.entries.len());
        self.entries.push(entry);
    }

    /// Removes and returns the entry carrying `id`, preserving catalog order.
    pub fn remove(&mut self, id: Uuid) -> Option<AssetEntry> {
        let index = self.by_id.remove(&id.value())?;
        let removed = self.entries.remove(index);
        for (offset, entry) in self.entries[index..].iter().enumerate() {
            self.by_id.insert(entry.id.value(), index + offset);
        }
        Some(removed)
    }

    /// Records source/license attribution on the entry for `id`, returning whether it
    /// existed.
    pub fn set_attribution(&mut self, id: Uuid, attribution: Attribution) -> bool {
        match self.by_id.get(&id.value()) {
            Some(&i) => {
                self.entries[i].attribution = Some(attribution);
                true
            }
            None => false,
        }
    }

    /// Records the content hash on the entry for `id`, returning whether it existed.
    /// Backfilled when a thumbnail lookup self-heals a legacy row that predates the
    /// content-addressed cache.
    pub fn set_content_hash(&mut self, id: Uuid, content_hash: u64) -> bool {
        match self.by_id.get(&id.value()) {
            Some(&i) => {
                self.entries[i].content_hash = content_hash;
                true
            }
            None => false,
        }
    }

    /// Renames the entry for `id`, returning whether it existed.
    pub fn rename(&mut self, id: Uuid, name: impl Into<String>) -> bool {
        match self.by_id.get(&id.value()) {
            Some(&i) => {
                self.entries[i].name = name.into();
                true
            }
            None => false,
        }
    }

    /// A name not already used by another entry.
    ///
    /// Appends `" (2)"`, `" (3)"`, … on collision, scanning suffixes upward until one
    /// is free.
    #[must_use]
    pub fn unique_name(&self, base: &str) -> String {
        if !self.entries.iter().any(|e| e.name == base) {
            return base.to_string();
        }
        let mut suffix = 2u32;
        loop {
            let candidate = format!("{base} ({suffix})");
            if !self.entries.iter().any(|e| e.name == candidate) {
                return candidate;
            }
            suffix += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_mode_default_is_procedural() {
        assert_eq!(SkyMode::default(), SkyMode::Procedural);
    }

    #[test]
    fn atmosphere_defaults() {
        let a = AtmosphereSettings::default();
        assert!(!a.enabled);
        assert_eq!(a.planet_radius, 6360.0);
        assert_eq!(a.atmosphere_height, 100.0);
        assert_eq!(a.rayleigh_scattering, Vec3::new(5.802, 13.558, 33.1));
        assert_eq!(a.rayleigh_scale_height, 8.0);
        assert_eq!(a.mie_scattering, 3.996);
        assert_eq!(a.mie_scale_height, 1.2);
        assert_eq!(a.mie_anisotropy, 0.8);
        assert_eq!(a.ozone_absorption, Vec3::new(0.650, 1.881, 0.085));
        assert_eq!(a.sun_disk_angular_radius, 0.004_65);
        assert_eq!(a.sun_disk_intensity, 1.0);
        assert_eq!(a.moon_disk_angular_radius, 0.004_96);
        assert_eq!(a.moon_disk_intensity, 1.0);
        assert_eq!(a.moon_earthshine, 0.02);
        assert!(!a.per_pixel_transmittance);
        assert_eq!(a.sky_capture_cadence, 9.0);
    }

    #[test]
    fn catalog_remove_preserves_order_and_rebuilds_indices() {
        let mut catalog = AssetCatalog::default();
        for id in [1, 2, 3] {
            catalog.put(AssetEntry {
                id: Uuid(id),
                name: id.to_string(),
                ..AssetEntry::default()
            });
        }

        assert_eq!(catalog.remove(Uuid(2)).unwrap().id, Uuid(2));
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![Uuid(1), Uuid(3)]
        );
        assert_eq!(catalog.find(Uuid(3)).unwrap().name, "3");
        assert!(catalog.remove(Uuid(2)).is_none());
    }

    #[test]
    fn fog_defaults() {
        let f = FogSettings::default();
        assert!(!f.enabled);
        assert_eq!(f.quality, FogQuality::Medium);
        assert_eq!(f.history_blend, 0.05);
        assert!(!f.neighborhood_clamp);
        assert_eq!(f.light_clamp, 0.0);
        assert_eq!(f.density, 0.02);
        assert_eq!(f.albedo, Vec3::new(0.5, 0.6, 0.7));
        assert_eq!(f.height, 0.0);
        assert_eq!(f.height_falloff, 0.2);
        assert_eq!(f.start_distance, 0.0);
        assert_eq!(f.max_opacity, 1.0);
        assert_eq!(f.emissive, Vec3::ZERO);
        assert_eq!(f.directional_color, Vec3::new(1.0, 0.9, 0.7));
        assert_eq!(f.directional_exponent, 8.0);
        assert_eq!(f.layer2_density, 0.0);
        assert_eq!(f.layer2_falloff, 0.5);
        assert_eq!(f.layer2_height, 0.0);
        assert!(!f.aerial_perspective);
        assert_eq!(f.aerial_intensity, 1.0);
    }

    #[test]
    fn cloud_defaults() {
        let cloud = CloudSettings::default();
        assert!(!cloud.enabled);
        assert_eq!(cloud.coverage, 0.5);
        assert_eq!(cloud.cloud_type, 0.4);
        assert_eq!(cloud.layer_altitude, 1500.0);
        assert_eq!(cloud.layer_height, 2500.0);
        assert_eq!(cloud.base_scale, 8e-5);
        assert_eq!(cloud.detail_scale, 1e-3);
        assert_eq!(cloud.detail_strength, 0.35);
        assert_eq!(cloud.curl_strength, 120.0);
        assert_eq!(cloud.weather_scale, 2e-5);
        assert_eq!(cloud.weather_offset, Vec3::ZERO);
        assert_eq!(cloud.weather_texture, Uuid(0));
        assert_eq!(cloud.primary_steps, 64);
        assert_eq!(cloud.light_steps, 6);
        assert_eq!(cloud.droplet_diameter, 20.0);
        assert_eq!(cloud.temporal_factor, 0.1);
    }

    #[test]
    fn scene_environment_defaults() {
        let e = SceneEnvironment::default();
        assert_eq!(e.sky_mode, SkyMode::Procedural);
        assert_eq!(e.clear_color, Vec3::new(0.05, 0.06, 0.08));
        assert_eq!(e.sky_texture, Uuid(0));
        assert_eq!(e.sky_intensity, 1.0);
        assert_eq!(e.sky_rotation, 0.0);
        assert_eq!(e.exposure, 1.0);
        assert!(e.visible);
        assert!(e.use_sky_for_ambient);
        assert_eq!(e.ambient_color, Vec3::ONE);
        assert_eq!(e.ambient_intensity, 0.15);
        assert!(!e.atmosphere.enabled);
        assert!(!e.fog.enabled);
        assert!(!e.cloud.enabled);
    }

    #[test]
    fn asset_enum_defaults() {
        assert_eq!(AssetType::default(), AssetType::Mesh);
        assert_eq!(Colorspace::default(), Colorspace::Auto);
    }

    #[test]
    fn asset_entry_defaults() {
        let e = AssetEntry::default();
        assert_eq!(e.id, Uuid(0));
        assert!(e.name.is_empty());
        assert_eq!(e.asset_type, AssetType::Mesh);
        assert!(!e.hdr);
        assert!(!e.linear);
        assert_eq!(e.duration, 0.0);
        assert_eq!(e.tracks, 0);
        assert!(!e.rigged);
        assert_eq!(e.container, Uuid(0));
        assert_eq!(e.chunk, -1);
        assert_eq!(e.colorspace, Colorspace::Auto);
        assert_eq!(e.content_hash, 0);
    }

    fn named_entry(id: u64, name: &str) -> AssetEntry {
        AssetEntry {
            id: Uuid(id),
            name: name.to_string(),
            ..AssetEntry::default()
        }
    }

    #[test]
    fn put_find_and_replace_round_trip() {
        let mut catalog = AssetCatalog::default();
        assert!(catalog.find(Uuid(1024)).is_none());

        catalog.put(named_entry(1024, "cube"));
        catalog.put(named_entry(2048, "sphere"));
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.find(Uuid(1024)).unwrap().name, "cube");
        assert_eq!(catalog.find(Uuid(2048)).unwrap().name, "sphere");

        // A put with an existing id replaces in place, not append.
        catalog.put(named_entry(1024, "cube-v2"));
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.find(Uuid(1024)).unwrap().name, "cube-v2");

        // An unknown id resolves to None.
        assert!(catalog.find(Uuid(9999)).is_none());
    }

    #[test]
    fn rename_reports_existence() {
        let mut catalog = AssetCatalog::default();
        catalog.put(named_entry(1024, "old"));
        assert!(catalog.rename(Uuid(1024), "new"));
        assert_eq!(catalog.find(Uuid(1024)).unwrap().name, "new");
        // Renaming an absent id is a no-op returning false.
        assert!(!catalog.rename(Uuid(7777), "ghost"));
    }

    #[test]
    fn unique_name_appends_collision_suffix() {
        let mut catalog = AssetCatalog::default();
        // No collision: the base name is returned verbatim.
        assert_eq!(catalog.unique_name("mesh"), "mesh");

        catalog.put(named_entry(1024, "mesh"));
        // First collision picks " (2)".
        assert_eq!(catalog.unique_name("mesh"), "mesh (2)");

        catalog.put(named_entry(2048, "mesh (2)"));
        // With (2) taken, scan upward to " (3)".
        assert_eq!(catalog.unique_name("mesh"), "mesh (3)");

        catalog.put(named_entry(4096, "mesh (3)"));
        assert_eq!(catalog.unique_name("mesh"), "mesh (4)");

        // A distinct base is unaffected by the collisions above.
        assert_eq!(catalog.unique_name("texture"), "texture");
    }
}
