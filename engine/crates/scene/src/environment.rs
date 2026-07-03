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
            sun_disk_intensity: 20.0,
        }
    }
}

/// Scene-wide environment / sky state.
///
/// The renderer resolves it into sky render settings each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
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
        }
    }
}

/// A project asset's kind.
///
/// A model imported and baked to a mesh, a texture, an animation clip, a `.smat`
/// material, or a `.smodel` container (the parent of its embedded sub-assets).
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
        assert_eq!(a.sun_disk_intensity, 20.0);
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
