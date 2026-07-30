//! The native `.smat` material asset: a reference-only property bag over the
//! übershader, its byte-compatible JSON serde, the physically defined surface-response
//! union, and the parent + sparse-override instance model.
//!
//! [`MaterialAsset`] bakes nothing — texture references are catalog [`Uuid`]s and the
//! colorspace / normal convention are recorded on the referenced texture's catalog row.
//! It resolves to a renderer `SubmeshMaterial` at draw time; here it is pure CPU + JSON.
//!
//! # The frozen `.smat` wire shape
//!
//! [`material_asset_to_json`] / [`material_asset_from_json`] are a frozen contract with
//! the editor and the baked-container chunk: the nested `factors` / `textures` objects,
//! the named-array vectors (`baseColor` 4-elem, `emissive` 3-elem, `uvTiling` / `uvOffset`
//! 2-elem), the texture key spellings (`albedo`, `ormOrMr`, `normal`, `emissive`,
//! `height`), `normalConvention`, the `surfaceModel` tagged union, and the uuid fields
//! emitted as **decimal strings** (read back from a string *or* a number). `graph` and
//! `overrides` ride as opaque [`Value`] trees — the editor's node-graph schema is their
//! single source of truth.
//!
//! # Instances
//!
//! A material with `parent != 0` resolves to the parent's resolved params with this
//! material's `overrides` applied on top ([`apply_overrides`]); a `parent` of `0` is a
//! master material. [`load_material_asset`] recurses through parents to a fixed depth cap
//! of 8 — the cycle / over-deep guard — keeping `parent` + `overrides` on the resolved
//! result so the editor still sees an instance. [`DEFAULT_MATERIAL_ID`] short-circuits to
//! [`default_material_asset`].

mod codec;
mod io;
mod overrides;

#[cfg(test)]
mod test_support;

pub use codec::{material_asset_from_json, material_asset_to_json, material_asset_to_text};
pub use io::{
    load_catalog_material_asset, load_catalog_material_asset_raw, load_material_asset,
    load_material_asset_raw, save_material_asset, update_material_asset,
};
pub use overrides::apply_overrides;

use saffron_core::{HeightMode, Uuid};
use saffron_geometry::glam::{Vec2, Vec3, Vec4};
use saffron_json::Value;
use saffron_vegetation::MaterialSurface;

/// The maximum parent-resolution depth — the instance cycle / over-deep guard.
const MAX_INSTANCE_DEPTH: u32 = 8;

/// The native material asset (`.smat`): a reference-only property bag over the
/// übershader.
///
/// Texture references are catalog [`Uuid`]s (`0` = none); the colorspace and normal
/// convention are recorded on the referenced texture's catalog row, not baked here. The
/// flat factor / texture fields are the resolved params; an optional node `graph` is the
/// editable source of truth when present, and `parent` + `overrides` make this an
/// instance (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialAsset {
    /// Exactly one physically defined surface response family.
    pub surface: MaterialSurface,
    /// The übershader family selector.
    pub shader: String,
    /// The PSO blend axis: `opaque` | `masked` | `translucent`.
    pub blend: String,
    /// Skip lighting — emit base color directly.
    pub unlit: bool,
    /// Render both faces (no backface cull).
    pub double_sided: bool,
    /// The base color factor (linear RGBA), multiplied with the albedo texture.
    pub base_color: Vec4,
    /// The metallic factor, multiplied with the ORM/MR texture's blue channel.
    pub metallic: f32,
    /// The roughness factor, multiplied with the ORM/MR texture's green channel.
    pub roughness: f32,
    /// The emissive color factor (linear RGB).
    pub emissive: Vec3,
    /// A scalar multiplier on the emissive color.
    pub emissive_strength: f32,
    /// The normal-map intensity (`1` = full strength).
    pub normal_strength: f32,
    /// The masked-blend alpha cutoff threshold.
    pub alpha_cutoff: f32,
    /// The height-map scale — parallax march depth ([`HeightMode::Parallax`]) or OBJECT-space
    /// displacement amplitude ([`HeightMode::Displacement`]; scales with the object like its geometry).
    pub height_scale: f32,
    /// How the height map is realized: [`HeightMode::Bump`] (shading bump), [`HeightMode::Parallax`]
    /// (parallax-occlusion mapping), or [`HeightMode::Displacement`] (real geometry — needs a
    /// densely-tessellated mesh to read well).
    pub height_mode: HeightMode,
    /// The UV tiling (scale) factor.
    pub uv_tiling: Vec2,
    /// The UV offset (translation).
    pub uv_offset: Vec2,
    /// The albedo (base-color) texture id (`0` = none).
    pub albedo_texture: Uuid,
    /// The packed AO/roughness/metallic (or standalone metallic-roughness) texture id.
    pub orm_texture: Uuid,
    /// The tangent-space normal-map texture id.
    pub normal_texture: Uuid,
    /// The emissive texture id.
    pub emissive_texture: Uuid,
    /// The height/displacement texture id.
    pub height_texture: Uuid,
    /// The vector-displacement (tangent-space XYZ) texture id (`0` = none). Under
    /// [`HeightMode::Displacement`] it switches the tessellation dice from scalar-along-normal to
    /// tangent-space vector offset, so overhangs become real geometry.
    pub vector_displacement_texture: Uuid,
    /// The authored normal convention: `gl` | `dx` (baked to `gl` at import; kept for
    /// provenance).
    pub normal_convention: String,
    /// The resolved feature bitset.
    pub features: u32,
    /// The optional node graph — the editable source of truth for a graph-authored
    /// material. Empty (`{}` or null) = no graph. Opaque editor-shaped JSON.
    pub graph: Value,
    /// The parent material id for an instance (`0` = a master material).
    pub parent: Uuid,
    /// The sparse `{ fieldName: value }` override map this instance applies on top of its
    /// parent. Opaque editor-shaped JSON.
    pub overrides: Value,
}

impl Default for MaterialAsset {
    /// The built-in default: white albedo, fully rough, non-metallic, opaque, lit. Equals
    /// [`default_material_asset`].
    fn default() -> Self {
        Self {
            surface: MaterialSurface::Standard,
            shader: "mesh".to_owned(),
            blend: "opaque".to_owned(),
            unlit: false,
            double_sided: false,
            base_color: Vec4::ONE,
            metallic: 0.0,
            roughness: 1.0,
            emissive: Vec3::ZERO,
            emissive_strength: 1.0,
            normal_strength: 1.0,
            alpha_cutoff: 0.5,
            height_scale: 0.05,
            height_mode: HeightMode::Bump,
            uv_tiling: Vec2::ONE,
            uv_offset: Vec2::ZERO,
            albedo_texture: Uuid(0),
            orm_texture: Uuid(0),
            normal_texture: Uuid(0),
            emissive_texture: Uuid(0),
            height_texture: Uuid(0),
            vector_displacement_texture: Uuid(0),
            normal_convention: "gl".to_owned(),
            features: 0,
            graph: empty_object(),
            parent: Uuid(0),
            overrides: empty_object(),
        }
    }
}

/// The built-in default material: white albedo, fully rough, non-metallic. Returned by
/// the resolve path when a referenced material is missing.
#[must_use]
pub fn default_material_asset() -> MaterialAsset {
    MaterialAsset::default()
}

/// An empty JSON object — the resting value for `graph` / `overrides`.
fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_default_material_asset() {
        let default = default_material_asset();
        assert_eq!(default, MaterialAsset::default());
        assert_eq!(default.base_color, Vec4::ONE);
        assert_eq!(default.roughness, 1.0);
        assert_eq!(default.metallic, 0.0);
        assert_eq!(default.shader, "mesh");
        assert_eq!(default.blend, "opaque");
        assert_eq!(default.normal_convention, "gl");
    }
}
