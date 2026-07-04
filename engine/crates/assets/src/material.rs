//! The native `.smat` material asset: a reference-only property bag over the
//! übershader, its byte-compatible JSON serde, and the parent + sparse-override
//! instance model.
//!
//! [`MaterialAsset`] bakes nothing — texture references are catalog [`Uuid`]s and the
//! colorspace / normal convention are recorded on the referenced texture's catalog row.
//! It resolves to a renderer `SubmeshMaterial` at draw time (phase 7); here it is pure
//! CPU + JSON.
//!
//! # The frozen `.smat` wire shape
//!
//! [`material_asset_to_json`] / [`material_asset_from_json`] are a frozen contract with
//! the editor and the baked-container chunk: the nested `factors` / `textures` objects,
//! the named-array vectors (`baseColor` 4-elem, `emissive` 3-elem, `uvTiling` / `uvOffset`
//! 2-elem), the texture key spellings (`albedo`, `ormOrMr`, `normal`, `emissive`,
//! `height`), `normalConvention`, and the uuid fields emitted as **decimal strings**
//! (read back from a string *or* a number). `graph` and `overrides` ride as opaque
//! [`Value`] trees — the editor's node-graph schema is their single source of truth.
//!
//! # Instances
//!
//! A material with `parent != 0` resolves to the parent's resolved params with this
//! material's `overrides` applied on top ([`apply_overrides`]); a `parent` of `0` is a
//! master material. [`load_material_asset`] recurses through parents to a fixed depth cap
//! of 8 — the cycle / over-deep guard — keeping `parent` + `overrides` on the resolved
//! result so the editor still sees an instance. [`DEFAULT_MATERIAL_ID`] short-circuits to
//! [`default_material_asset`].

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind,
    glam::{Vec2, Vec3, Vec4},
};
use saffron_json::{Value, json_bool_or, json_f32_or, json_string_or, parse_json};
use saffron_scene::{AssetEntry, AssetType};

use crate::AssetServer;
use crate::DEFAULT_MATERIAL_ID;
use crate::error::{Error, Result};

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
    /// The parallax/displacement height-map scale.
    pub height_scale: f32,
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
            uv_tiling: Vec2::ONE,
            uv_offset: Vec2::ZERO,
            albedo_texture: Uuid(0),
            orm_texture: Uuid(0),
            normal_texture: Uuid(0),
            emissive_texture: Uuid(0),
            height_texture: Uuid(0),
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

/// A uuid emitted as a decimal JSON *string*.
fn uuid_string(id: Uuid) -> Value {
    Value::String(id.value().to_string())
}

/// Reads a [`Uuid`] from a JSON value, accepting a decimal string *or* an unsigned number
/// (the frozen lenient read). Anything else is `0`.
fn uuid_from_value(value: &Value) -> Uuid {
    match value {
        Value::String(s) => Uuid(s.parse::<u64>().unwrap_or(0)),
        Value::Number(n) => Uuid(n.as_u64().unwrap_or(0)),
        _ => Uuid(0),
    }
}

/// Reads a fixed-length `f32` array field, returning `None` unless it is an array of
/// exactly `N` numbers.
fn read_fixed_array<const N: usize>(object: &Value, key: &str) -> Option<[f32; N]> {
    let array = object.as_object()?.get(key)?.as_array()?;
    if array.len() != N {
        return None;
    }
    let mut out = [0.0f32; N];
    for (slot, element) in out.iter_mut().zip(array) {
        *slot = element.as_f64()? as f32;
    }
    Some(out)
}

/// Serializes a [`MaterialAsset`] to the frozen `.smat` JSON document.
///
/// Uuid fields are emitted as decimal strings, never numbers; `version` is pinned to `1`;
/// `graph` / `overrides` ride through verbatim (an empty object when unset). Object key
/// order is incidental: the `.smat` write path serializes via `dump_json_sorted`
/// (alphabetically sorted keys), so the byte shape is stable regardless of insertion order.
#[must_use]
pub fn material_asset_to_json(material: &MaterialAsset) -> Value {
    let graph = if material.graph.is_null() {
        empty_object()
    } else {
        material.graph.clone()
    };
    let overrides = if material.overrides.is_null() {
        empty_object()
    } else {
        material.overrides.clone()
    };
    serde_json::json!({
        "version": 1,
        "shader": material.shader,
        "blend": material.blend,
        "unlit": material.unlit,
        "doubleSided": material.double_sided,
        "normalConvention": material.normal_convention,
        "factors": {
            "baseColor": [
                material.base_color.x,
                material.base_color.y,
                material.base_color.z,
                material.base_color.w,
            ],
            "metallic": material.metallic,
            "roughness": material.roughness,
            "emissive": [material.emissive.x, material.emissive.y, material.emissive.z],
            "emissiveStrength": material.emissive_strength,
            "normalStrength": material.normal_strength,
            "alphaCutoff": material.alpha_cutoff,
            "heightScale": material.height_scale,
            "uvTiling": [material.uv_tiling.x, material.uv_tiling.y],
            "uvOffset": [material.uv_offset.x, material.uv_offset.y],
        },
        "textures": {
            "albedo": uuid_string(material.albedo_texture),
            "ormOrMr": uuid_string(material.orm_texture),
            "normal": uuid_string(material.normal_texture),
            "emissive": uuid_string(material.emissive_texture),
            "height": uuid_string(material.height_texture),
        },
        "graph": graph,
        "parent": uuid_string(material.parent),
        "overrides": overrides,
    })
}

/// Rebuilds a [`MaterialAsset`] from a `.smat` JSON document.
///
/// Lenient: every field defaults to the [`MaterialAsset::default`] value when absent or
/// mistyped; uuid fields accept a decimal string *or* a number. `features` is not read
/// (it is a resolved bitset, never serialized).
#[must_use]
pub fn material_asset_from_json(doc: &Value) -> MaterialAsset {
    let mut material = MaterialAsset {
        shader: json_string_or(doc, "shader", "mesh".to_owned()),
        blend: json_string_or(doc, "blend", "opaque".to_owned()),
        unlit: json_bool_or(doc, "unlit", false),
        double_sided: json_bool_or(doc, "doubleSided", false),
        normal_convention: json_string_or(doc, "normalConvention", "gl".to_owned()),
        ..MaterialAsset::default()
    };

    if let Some(factors) = doc.get("factors").filter(|v| v.is_object()) {
        if let Some([r, g, b, a]) = read_fixed_array::<4>(factors, "baseColor") {
            material.base_color = Vec4::new(r, g, b, a);
        }
        material.metallic = json_f32_or(factors, "metallic", 0.0);
        material.roughness = json_f32_or(factors, "roughness", 1.0);
        if let Some([r, g, b]) = read_fixed_array::<3>(factors, "emissive") {
            material.emissive = Vec3::new(r, g, b);
        }
        material.emissive_strength = json_f32_or(factors, "emissiveStrength", 1.0);
        material.normal_strength = json_f32_or(factors, "normalStrength", 1.0);
        material.alpha_cutoff = json_f32_or(factors, "alphaCutoff", 0.5);
        material.height_scale = json_f32_or(factors, "heightScale", 0.05);
        if let Some([x, y]) = read_fixed_array::<2>(factors, "uvTiling") {
            material.uv_tiling = Vec2::new(x, y);
        }
        if let Some([x, y]) = read_fixed_array::<2>(factors, "uvOffset") {
            material.uv_offset = Vec2::new(x, y);
        }
    }

    if let Some(textures) = doc.get("textures").filter(|v| v.is_object()) {
        if let Some(v) = textures.get("albedo") {
            material.albedo_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("ormOrMr") {
            material.orm_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("normal") {
            material.normal_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("emissive") {
            material.emissive_texture = uuid_from_value(v);
        }
        if let Some(v) = textures.get("height") {
            material.height_texture = uuid_from_value(v);
        }
    }

    if let Some(graph) = doc.get("graph").filter(|v| is_non_empty_object(v)) {
        material.graph = graph.clone();
    }
    if let Some(parent) = doc.get("parent") {
        material.parent = uuid_from_value(parent);
    }
    if let Some(overrides) = doc.get("overrides").filter(|v| is_non_empty_object(v)) {
        material.overrides = overrides.clone();
    }

    material
}

/// Whether `value` is a JSON object with at least one member.
fn is_non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|map| !map.is_empty())
}

/// Applies a sparse override map `{ field: value }` onto a base material — the instance
/// path.
///
/// Writes only the named, well-typed fields and leaves the rest untouched. A non-object
/// `overrides` is a no-op. The field set is the material's exposed parameters (see
/// [`crate::material_schema`]): the scalar factors, the two colour vectors, the two UV
/// vectors, the three flags/enum, and the five texture ids.
pub fn apply_overrides(material: &mut MaterialAsset, overrides: &Value) {
    let Some(map) = overrides.as_object() else {
        return;
    };
    for (field, value) in map {
        match field.as_str() {
            "baseColor" => {
                if let Some([r, g, b, a]) = read_array4(value) {
                    material.base_color = Vec4::new(r, g, b, a);
                }
            }
            "emissive" => {
                if let Some([r, g, b]) = read_array3(value) {
                    material.emissive = Vec3::new(r, g, b);
                }
            }
            "uvTiling" => {
                if let Some([x, y]) = read_array2(value) {
                    material.uv_tiling = Vec2::new(x, y);
                }
            }
            "uvOffset" => {
                if let Some([x, y]) = read_array2(value) {
                    material.uv_offset = Vec2::new(x, y);
                }
            }
            "metallic" => {
                if let Some(v) = value.as_f64() {
                    material.metallic = v as f32;
                }
            }
            "roughness" => {
                if let Some(v) = value.as_f64() {
                    material.roughness = v as f32;
                }
            }
            "emissiveStrength" => {
                if let Some(v) = value.as_f64() {
                    material.emissive_strength = v as f32;
                }
            }
            "normalStrength" => {
                if let Some(v) = value.as_f64() {
                    material.normal_strength = v as f32;
                }
            }
            "alphaCutoff" => {
                if let Some(v) = value.as_f64() {
                    material.alpha_cutoff = v as f32;
                }
            }
            "heightScale" => {
                if let Some(v) = value.as_f64() {
                    material.height_scale = v as f32;
                }
            }
            "unlit" => {
                if let Some(v) = value.as_bool() {
                    material.unlit = v;
                }
            }
            "doubleSided" => {
                if let Some(v) = value.as_bool() {
                    material.double_sided = v;
                }
            }
            "blend" => {
                if let Some(v) = value.as_str() {
                    material.blend = v.to_owned();
                }
            }
            "albedoTexture" => material.albedo_texture = uuid_from_value(value),
            "ormTexture" => material.orm_texture = uuid_from_value(value),
            "normalTexture" => material.normal_texture = uuid_from_value(value),
            "emissiveTexture" => material.emissive_texture = uuid_from_value(value),
            "heightTexture" => material.height_texture = uuid_from_value(value),
            _ => {}
        }
    }
}

/// Reads a 4-element `f32` array from a JSON value (with at least 4 numbers), for the
/// override path's `baseColor`.
fn read_array4(value: &Value) -> Option<[f32; 4]> {
    let array = value.as_array()?;
    if array.len() < 4 {
        return None;
    }
    Some([
        array[0].as_f64()? as f32,
        array[1].as_f64()? as f32,
        array[2].as_f64()? as f32,
        array[3].as_f64()? as f32,
    ])
}

/// Reads a 3-element `f32` array from a JSON value (with at least 3 numbers), for the
/// override path's `emissive`.
fn read_array3(value: &Value) -> Option<[f32; 3]> {
    let array = value.as_array()?;
    if array.len() < 3 {
        return None;
    }
    Some([
        array[0].as_f64()? as f32,
        array[1].as_f64()? as f32,
        array[2].as_f64()? as f32,
    ])
}

/// Reads a 2-element `f32` array from a JSON value (with at least 2 numbers), for the
/// override path's `uvTiling` / `uvOffset`.
fn read_array2(value: &Value) -> Option<[f32; 2]> {
    let array = value.as_array()?;
    if array.len() < 2 {
        return None;
    }
    Some([array[0].as_f64()? as f32, array[1].as_f64()? as f32])
}

/// Reads the stored `.smat` as-is — no parent resolution, no graph fold. The editor's
/// edit path: it mutates this and writes it back via
/// [`update_material_asset`].
///
/// [`DEFAULT_MATERIAL_ID`] short-circuits to [`default_material_asset`].
///
/// # Errors
///
/// [`Error::NotInCatalog`] / [`Error::WrongAssetType`] when the id is absent or not a
/// material; [`Error::Io`] on a read failure; [`Error::Json`] on an unparseable document.
pub fn load_material_asset_raw(assets: &AssetServer, id: Uuid) -> Result<MaterialAsset> {
    if id.value() == DEFAULT_MATERIAL_ID.value() {
        return Ok(default_material_asset());
    }
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Material {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "material",
        });
    }
    let path = assets.root.join(&entry.path);
    let text = std::fs::read_to_string(&path).map_err(|e| Error::Io(e.to_string()))?;
    let doc = parse_json(&text)?;
    Ok(material_asset_from_json(&doc))
}

/// Reads a material catalog row, whether it is a standalone `.smat` or an embedded
/// `.smodel` `SMAT` chunk.
///
/// Standalone rows follow [`load_material_asset_raw`]. Embedded rows resolve the owning
/// container, slice the material chunk, and parse the chunk's `.smat` JSON.
///
/// # Errors
///
/// [`Error::NotInCatalog`] / [`Error::WrongAssetType`] when the id is absent or not a
/// material; [`Error::ContainerMissingSubAsset`] when an embedded row has no material
/// chunk; [`Error::Io`] / [`Error::Json`] when the bytes cannot be read or parsed.
pub fn load_catalog_material_asset_raw(
    assets: &mut AssetServer,
    id: Uuid,
) -> Result<MaterialAsset> {
    if id.value() == DEFAULT_MATERIAL_ID.value() {
        return Ok(default_material_asset());
    }
    let entry = assets
        .catalog
        .find(id)
        .cloned()
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Material {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "material",
        });
    }
    if entry.container.value() == 0 {
        return load_material_asset_raw(assets, id);
    }

    let model = assets.load_model_asset(entry.container).ok_or_else(|| {
        Error::Io(format!(
            "material {}: container {} is not loadable",
            id.value(),
            entry.container.value()
        ))
    })?;
    let source = assets.chunk_source_for(&model, ChunkKind::Material, id);
    if source.is_empty() {
        return Err(Error::ContainerMissingSubAsset {
            container: entry.container.value(),
            sub: id.value(),
        });
    }
    let bytes = source.read()?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| Error::Io(format!("material {} chunk is not UTF-8: {e}", id.value())))?;
    let doc = parse_json(text)?;
    Ok(material_asset_from_json(&doc))
}

/// Reads a `.smat` resolved for rendering.
///
/// An instance (`parent != 0`) resolves to its parent's resolved params with this
/// material's `overrides` applied on top, keeping `parent` + `overrides` so the editor
/// still sees an instance. Resolution recurses to a depth cap of [`MAX_INSTANCE_DEPTH`];
/// a missing or cyclic parent (or hitting the cap) falls back to this material's own
/// stored params.
///
/// # Errors
///
/// Propagates [`load_material_asset_raw`]'s errors for this material's own row.
pub fn load_material_asset(assets: &AssetServer, id: Uuid) -> Result<MaterialAsset> {
    load_material_asset_at(assets, id, 0)
}

/// Reads a material catalog row resolved for rendering, supporting standalone and
/// embedded material rows.
///
/// Parent resolution mirrors [`load_material_asset`], but parent ids may also point at
/// embedded material rows.
pub fn load_catalog_material_asset(assets: &mut AssetServer, id: Uuid) -> Result<MaterialAsset> {
    load_catalog_material_asset_at(assets, id, 0)
}

/// The depth-tracked recursion behind [`load_material_asset`].
fn load_material_asset_at(assets: &AssetServer, id: Uuid, depth: u32) -> Result<MaterialAsset> {
    let material = load_material_asset_raw(assets, id)?;
    if material.parent.value() != 0 && depth < MAX_INSTANCE_DEPTH {
        if let Ok(parent_resolved) = load_material_asset_at(assets, material.parent, depth + 1) {
            let mut base = parent_resolved;
            apply_overrides(&mut base, &material.overrides);
            base.parent = material.parent;
            base.overrides = material.overrides;
            return Ok(base);
        }
        // A missing or cyclic parent falls back to this material's own stored params.
    }
    Ok(material)
}

fn load_catalog_material_asset_at(
    assets: &mut AssetServer,
    id: Uuid,
    depth: u32,
) -> Result<MaterialAsset> {
    let material = load_catalog_material_asset_raw(assets, id)?;
    if material.parent.value() != 0 && depth < MAX_INSTANCE_DEPTH {
        if let Ok(parent_resolved) =
            load_catalog_material_asset_at(assets, material.parent, depth + 1)
        {
            let mut base = parent_resolved;
            apply_overrides(&mut base, &material.overrides);
            base.parent = material.parent;
            base.overrides = material.overrides;
            return Ok(base);
        }
    }
    Ok(material)
}

/// Writes a new `.smat` and registers a catalog row for it.
///
/// Mints a fresh id, writes `materials/<id>.smat`, and puts an [`AssetType::Material`]
/// catalog entry under `folder` with a unique name. Returns the minted id.
///
/// # Errors
///
/// [`Error::Io`] if the file cannot be written.
pub fn save_material_asset(
    assets: &mut AssetServer,
    material: &MaterialAsset,
    name: &str,
    folder: &str,
) -> Result<Uuid> {
    let id = Uuid::new();
    assets.ensure_asset_directories();
    let relative_path = format!("materials/{}.smat", id.value());
    let text = saffron_json::dump_json_sorted(&material_asset_to_json(material), 2);
    std::fs::write(assets.root.join(&relative_path), text).map_err(|e| Error::Io(e.to_string()))?;
    let unique = assets.catalog.unique_name(name);
    assets.catalog.put(AssetEntry {
        id,
        name: unique,
        asset_type: AssetType::Material,
        path: relative_path,
        folder: folder.to_owned(),
        ..AssetEntry::default()
    });
    if let Err(err) = assets.write_asset_sidecar(id) {
        tracing::warn!("material: could not write .smeta for {}: {err}", id.value());
    }
    // A fresh id has no cached form, but an instance-create rewrites the parent link others resolve
    // through — clear so the resolve sees the new topology on the next frame.
    assets.invalidate_material_caches();
    Ok(id)
}

/// Overwrites an existing `.smat` in place — same id + path.
///
/// The edit path, distinct from [`save_material_asset`] which mints a new asset.
///
/// # Errors
///
/// [`Error::NotInCatalog`] / [`Error::WrongAssetType`] when the id is absent or not a
/// material; [`Error::Io`] if the file cannot be written.
pub fn update_material_asset(
    assets: &mut AssetServer,
    id: Uuid,
    material: &MaterialAsset,
) -> Result<()> {
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != AssetType::Material {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted: "material",
        });
    }
    let path = assets.root.join(&entry.path);
    let text = saffron_json::dump_json_sorted(&material_asset_to_json(material), 2);
    std::fs::write(path, text).map_err(|e| Error::Io(e.to_string()))?;
    // The edited `.smat` (and every instance that resolves through it) is now stale in the cache.
    assets.invalidate_material_caches();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_material() -> MaterialAsset {
        MaterialAsset {
            shader: "mesh".to_owned(),
            blend: "masked".to_owned(),
            unlit: true,
            double_sided: true,
            base_color: Vec4::new(0.1, 0.2, 0.3, 0.4),
            metallic: 0.6,
            roughness: 0.25,
            emissive: Vec3::new(1.5, 2.5, 3.5),
            emissive_strength: 4.0,
            normal_strength: 0.75,
            alpha_cutoff: 0.33,
            height_scale: 0.125,
            uv_tiling: Vec2::new(2.0, 3.0),
            uv_offset: Vec2::new(0.25, 0.5),
            albedo_texture: Uuid(1001),
            orm_texture: Uuid(1002),
            normal_texture: Uuid(1003),
            emissive_texture: Uuid(1004),
            height_texture: Uuid(1005),
            normal_convention: "dx".to_owned(),
            features: 0,
            graph: empty_object(),
            parent: Uuid(0),
            overrides: empty_object(),
        }
    }

    #[test]
    fn default_matches_default_material_asset() {
        let default = default_material_asset();
        assert_eq!(default, MaterialAsset::default());
        // White albedo, fully rough, non-metallic.
        assert_eq!(default.base_color, Vec4::ONE);
        assert_eq!(default.roughness, 1.0);
        assert_eq!(default.metallic, 0.0);
        assert_eq!(default.shader, "mesh");
        assert_eq!(default.blend, "opaque");
        assert_eq!(default.normal_convention, "gl");
    }

    #[test]
    fn round_trip_reproduces_every_field() {
        let original = populated_material();
        let restored = material_asset_from_json(&material_asset_to_json(&original));
        assert_eq!(restored, original);
    }

    #[test]
    fn round_trip_preserves_opaque_graph_and_overrides() {
        let mut original = populated_material();
        original.parent = Uuid(42);
        original.graph = serde_json::json!({
            "nodes": [{ "id": "n1", "type": "constant", "props": { "value": [1, 0, 0, 1] } }],
            "edges": [],
        });
        original.overrides = serde_json::json!({ "metallic": 0.9, "albedoTexture": "7" });
        let restored = material_asset_from_json(&material_asset_to_json(&original));
        assert_eq!(restored.graph, original.graph);
        assert_eq!(restored.overrides, original.overrides);
        assert_eq!(restored.parent, Uuid(42));
    }

    #[test]
    fn uuid_fields_serialize_as_strings_never_numbers() {
        let material = populated_material();
        let doc = material_asset_to_json(&material);
        let textures = doc.get("textures").unwrap();
        for key in ["albedo", "ormOrMr", "normal", "emissive", "height"] {
            assert!(
                textures.get(key).unwrap().is_string(),
                "texture {key} must serialize as a string"
            );
        }
        assert!(doc.get("parent").unwrap().is_string());
        // The serialized bytes must carry quotes around every id.
        let serialized = saffron_json::dump_json(&doc, -1);
        assert!(serialized.contains(r#""albedo":"1001""#));
        assert!(serialized.contains(r#""parent":"0""#));
    }

    #[test]
    fn byte_equal_to_captured_smat_fixture() {
        // A `.smat` document: alphabetically-sorted keys (via `dump_json_sorted`), uuid
        // fields as decimal strings, `version: 1`. The default material with two texture
        // ids assigned.
        let material = MaterialAsset {
            albedo_texture: Uuid(5),
            normal_texture: Uuid(6),
            ..MaterialAsset::default()
        };
        let serialized = saffron_json::dump_json_sorted(&material_asset_to_json(&material), -1);
        // `heightScale` (f32 `0.05`) carries its f64-promoted long decimal (an
        // exactly-representable value like `0.5` stays short).
        let expected = concat!(
            r#"{"blend":"opaque","doubleSided":false,"#,
            r#""factors":{"alphaCutoff":0.5,"baseColor":[1.0,1.0,1.0,1.0],"#,
            r#""emissive":[0.0,0.0,0.0],"emissiveStrength":1.0,"#,
            r#""heightScale":0.05000000074505806,"#,
            r#""metallic":0.0,"normalStrength":1.0,"roughness":1.0,"#,
            r#""uvOffset":[0.0,0.0],"uvTiling":[1.0,1.0]},"#,
            r#""graph":{},"normalConvention":"gl","overrides":{},"parent":"0","#,
            r#""shader":"mesh","#,
            r#""textures":{"albedo":"5","emissive":"0","height":"0","normal":"6","ormOrMr":"0"},"#,
            r#""unlit":false,"version":1}"#,
        );
        assert_eq!(serialized, expected);
    }

    #[test]
    fn from_json_accepts_uuid_string_or_number() {
        let doc = serde_json::json!({
            "textures": { "albedo": "1234", "ormOrMr": 5678 },
            "parent": 99,
        });
        let material = material_asset_from_json(&doc);
        assert_eq!(material.albedo_texture, Uuid(1234));
        assert_eq!(material.orm_texture, Uuid(5678));
        assert_eq!(material.parent, Uuid(99));
    }

    #[test]
    fn from_json_defaults_missing_fields() {
        let material = material_asset_from_json(&serde_json::json!({}));
        assert_eq!(material, MaterialAsset::default());
    }

    #[test]
    fn apply_overrides_writes_only_named_fields() {
        let mut material = MaterialAsset::default();
        let overrides = serde_json::json!({
            "metallic": 0.8,
            "baseColor": [0.1, 0.2, 0.3, 0.4],
            "albedoTexture": "77",
            // An unknown field is ignored.
            "bogus": 5,
            // A mistyped field is left at its default.
            "roughness": "not a number",
        });
        apply_overrides(&mut material, &overrides);
        assert_eq!(material.metallic, 0.8);
        assert_eq!(material.base_color, Vec4::new(0.1, 0.2, 0.3, 0.4));
        assert_eq!(material.albedo_texture, Uuid(77));
        // Everything else stays at the default.
        assert_eq!(material.roughness, 1.0);
        assert_eq!(material.emissive, Vec3::ZERO);
        assert_eq!(material.normal_texture, Uuid(0));
    }

    #[test]
    fn apply_overrides_ignores_non_object() {
        let mut material = MaterialAsset::default();
        let before = material.clone();
        apply_overrides(&mut material, &Value::Null);
        apply_overrides(&mut material, &serde_json::json!([1, 2, 3]));
        assert_eq!(material, before);
    }

    /// A scratch [`AssetServer`] rooted under a per-test temp dir.
    fn scratch_server(tag: &str) -> (AssetServer, std::path::PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "saffron-material-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("project").join("assets");
        (AssetServer::new(&root), tmp)
    }

    #[test]
    fn save_then_load_raw_round_trips_through_disk() {
        let (mut assets, tmp) = scratch_server("save-load");
        let original = populated_material();
        let id = save_material_asset(&mut assets, &original, "Brass", "metals").unwrap();

        let entry = assets.catalog.find(id).unwrap();
        assert_eq!(entry.asset_type, AssetType::Material);
        assert_eq!(entry.folder, "metals");
        assert_eq!(entry.path, format!("materials/{}.smat", id.value()));

        let loaded = load_material_asset_raw(&assets, id).unwrap();
        assert_eq!(loaded, original);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn default_id_short_circuits_without_a_catalog_row() {
        let (assets, tmp) = scratch_server("default-id");
        let loaded = load_material_asset_raw(&assets, DEFAULT_MATERIAL_ID).unwrap();
        assert_eq!(loaded, default_material_asset());
        // And through the resolving path.
        let resolved = load_material_asset(&assets, DEFAULT_MATERIAL_ID).unwrap();
        assert_eq!(resolved, default_material_asset());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_raw_errors_on_missing_and_wrong_type() {
        let (mut assets, tmp) = scratch_server("errors");
        // Absent id.
        assert!(matches!(
            load_material_asset_raw(&assets, Uuid(5000)),
            Err(Error::NotInCatalog(5000))
        ));
        // Present but wrong type.
        assets.catalog.put(AssetEntry {
            id: Uuid(5001),
            name: "mesh".to_owned(),
            asset_type: AssetType::Mesh,
            path: "models/5001.smodel".to_owned(),
            ..AssetEntry::default()
        });
        assert!(matches!(
            load_material_asset_raw(&assets, Uuid(5001)),
            Err(Error::WrongAssetType { id: 5001, .. })
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn instance_resolves_parent_params_with_overrides_on_top() {
        let (mut assets, tmp) = scratch_server("instance");

        let parent = MaterialAsset {
            base_color: Vec4::new(0.2, 0.2, 0.2, 1.0),
            metallic: 0.1,
            roughness: 0.9,
            albedo_texture: Uuid(1111),
            ..MaterialAsset::default()
        };
        let parent_id = save_material_asset(&mut assets, &parent, "Parent", "").unwrap();

        let child = MaterialAsset {
            parent: parent_id,
            // The stored child params differ from the parent and the overrides; they must
            // be discarded in favor of the parent's, with overrides applied on top.
            base_color: Vec4::new(9.0, 9.0, 9.0, 9.0),
            metallic: 0.5,
            overrides: serde_json::json!({ "metallic": 0.75, "roughness": 0.2 }),
            ..MaterialAsset::default()
        };
        let child_id = save_material_asset(&mut assets, &child, "Child", "").unwrap();

        let resolved = load_material_asset(&assets, child_id).unwrap();
        // The parent's base color and albedo win (the child's stored ones are dropped).
        assert_eq!(resolved.base_color, Vec4::new(0.2, 0.2, 0.2, 1.0));
        assert_eq!(resolved.albedo_texture, Uuid(1111));
        // The overrides sit on top of the parent's params.
        assert_eq!(resolved.metallic, 0.75);
        assert_eq!(resolved.roughness, 0.2);
        // The lineage is kept so the editor still sees an instance.
        assert_eq!(resolved.parent, parent_id);
        assert_eq!(resolved.overrides, child.overrides);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_parent_falls_back_to_own_params() {
        let (mut assets, tmp) = scratch_server("missing-parent");
        let child = MaterialAsset {
            parent: Uuid(424_242), // no such material
            metallic: 0.42,
            overrides: serde_json::json!({ "roughness": 0.1 }),
            ..MaterialAsset::default()
        };
        let child_id = save_material_asset(&mut assets, &child, "Orphan", "").unwrap();
        let resolved = load_material_asset(&assets, child_id).unwrap();
        // The overrides are not applied; the child's own stored params stand.
        assert_eq!(resolved.metallic, 0.42);
        assert_eq!(resolved.roughness, 1.0);
        assert_eq!(resolved.parent, Uuid(424_242));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cyclic_parent_chain_terminates_without_infinite_recursion() {
        let (mut assets, tmp) = scratch_server("cycle");
        // Mint two ids, then make each the other's parent (a 2-cycle).
        let a = MaterialAsset::default();
        let b = MaterialAsset::default();
        let a_id = save_material_asset(&mut assets, &a, "A", "").unwrap();
        let b_id = save_material_asset(&mut assets, &b, "B", "").unwrap();

        let a_cyclic = MaterialAsset {
            parent: b_id,
            metallic: 0.11,
            ..MaterialAsset::default()
        };
        let b_cyclic = MaterialAsset {
            parent: a_id,
            metallic: 0.22,
            ..MaterialAsset::default()
        };
        update_material_asset(&mut assets, a_id, &a_cyclic).unwrap();
        update_material_asset(&mut assets, b_id, &b_cyclic).unwrap();

        // The depth cap stops the recursion; the call returns rather than overflowing.
        let resolved = load_material_asset(&assets, a_id).unwrap();
        // The exact resolved value is the cap's fallback; the contract is *termination*.
        assert_eq!(resolved.parent, b_id);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn update_overwrites_in_place_same_id_and_path() {
        let (mut assets, tmp) = scratch_server("update");
        let id = save_material_asset(&mut assets, &MaterialAsset::default(), "Mat", "").unwrap();
        let path_before = assets.catalog.find(id).unwrap().path.clone();

        let edited = MaterialAsset {
            roughness: 0.05,
            ..MaterialAsset::default()
        };
        update_material_asset(&mut assets, id, &edited).unwrap();

        // Same id, same path, content updated.
        assert_eq!(assets.catalog.find(id).unwrap().path, path_before);
        let reloaded = load_material_asset_raw(&assets, id).unwrap();
        assert_eq!(reloaded.roughness, 0.05);

        // Update of an absent / wrong-type id errors.
        assert!(matches!(
            update_material_asset(&mut assets, Uuid(7777), &edited),
            Err(Error::NotInCatalog(7777))
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
