//! The disk-side import pipeline: `bake_model` / `import_model` and the shared
//! `catalog_rows_for_container`.
//!
//! Bake is pure disk + catalog — no GPU, no spawn. It turns an [`ImportedModel`] (from
//! geometry's `translate_model`) into one self-contained `assets/models/<uuid>.smodel`:
//! the mesh chunk, each material as a `.smat`-JSON chunk, each texture as a raw chunk
//! (colorspace in the chunk flags), each clip as a `.sanim` chunk, and a META chunk
//! ([`ContainerMetadata`]) carrying the node/skin hierarchy plus the deterministic
//! reimport recipe (source path, **content hash — not mtime**, [`IMPORTER_VERSION`],
//! and the recorded [`ImportOptions`]).
//!
//! Sub-ids are stable via geometry's `sub_id_for`, keyed by source name, so a re-bake of
//! the same source resolves every sub-asset to its prior identity. `model_id` is reused
//! on reimport (`0` mints a fresh one). [`catalog_rows_for_container`] is shared by bake and
//! the scan so a freshly-baked container and a rediscovered one yield identical rows.

mod bake;
mod meta;

#[cfg(test)]
mod test_support;

use saffron_core::{HeightMode, Uuid};
use saffron_geometry::MaterialMapRole;
use saffron_json::{Value, dump_json_sorted, json_bool_or, json_f32_or, json_string_or};
use saffron_scene::{AssetEntry, AssetType, Colorspace};

use crate::model::ContainerMetadata;
use crate::names::colorspace_from_name;

/// The bump-on-incompatible-translator version stamped into a container's import recipe;
/// a reimport whose stored value differs is re-baked rather than skipped.
pub const IMPORTER_VERSION: u32 = 1;

/// Every decision an import makes, in one serializable place.
///
/// Stored verbatim in a container's META chunk so a reimport replays the same options
/// rather than today's defaults. For v1 `scale`/`axis`/`gen_tangents` are recorded intent;
/// `embed_textures` is always true. [`ImportOptions::colorspace_for`] is the single source
/// of truth for per-role texture colorspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImportOptions {
    /// Uniform import scale (recorded intent for v1).
    pub scale: f32,
    /// The source up-axis (recorded intent for v1).
    pub axis: Axis,
    /// Whether to generate tangents (recorded intent for v1).
    pub gen_tangents: bool,
    /// Whether textures are embedded in the container (always true for v1).
    pub embed_textures: bool,
}

/// The source model's up-axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    /// Y-up (the glTF default).
    #[default]
    YUp,
    /// Z-up.
    ZUp,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            axis: Axis::YUp,
            gen_tangents: true,
            embed_textures: true,
        }
    }
}

impl ImportOptions {
    /// The colorspace a material map of `role` is imported with: albedo/emissive are
    /// sRGB color; normal / metallic-roughness / occlusion / height are linear data maps.
    #[must_use]
    pub fn colorspace_for(self, role: MaterialMapRole) -> Colorspace {
        match role {
            MaterialMapRole::Albedo | MaterialMapRole::Emissive => Colorspace::Srgb,
            _ => Colorspace::Linear,
        }
    }

    /// The options as the META `import.options` JSON (stored verbatim).
    #[must_use]
    pub fn to_json(self) -> Value {
        let axis = match self.axis {
            Axis::YUp => "y-up",
            Axis::ZUp => "z-up",
        };
        serde_json::json!({
            "scale": self.scale,
            "axis": axis,
            "genTangents": self.gen_tangents,
            "embedTextures": self.embed_textures,
        })
    }

    /// Parses options back from the stored `import.options` JSON (the reimport replay).
    /// Lenient: missing keys take the defaults.
    #[must_use]
    pub fn from_json(doc: &Value) -> Self {
        let axis = if json_string_or(doc, "axis", "y-up".to_owned()) == "z-up" {
            Axis::ZUp
        } else {
            Axis::YUp
        };
        Self {
            scale: json_f32_or(doc, "scale", 1.0),
            axis,
            gen_tangents: json_bool_or(doc, "genTangents", true),
            embed_textures: json_bool_or(doc, "embedTextures", true),
        }
    }
}

/// What [`AssetServer::bake_model`](crate::AssetServer::bake_model) produces: the new container's
/// id, its project-relative
/// path, and the catalog rows it contributes (one [`AssetType::Model`] parent + one row
/// per embedded sub-asset). No GPU, no spawn.
#[derive(Clone, Debug)]
pub struct BakeResult {
    /// The baked container's model id.
    pub model_id: Uuid,
    /// Project-relative path to the `.smodel`.
    pub path: String,
    /// The catalog rows the container contributes.
    pub rows: Vec<AssetEntry>,
}

/// One role-tagged map fed to [`AssetServer::bake_material_container`].
#[derive(Clone, Debug)]
pub struct MaterialMap {
    /// The canonical role (`albedo`/`normal`/`orm`/`roughness`/`metallic`/`ao`/`height`/
    /// `emissive`) that selects the material slot + colorspace.
    pub role: String,
    /// For a `height` role, the technique the source implies (from the filename —
    /// [`detect_height_mode`](crate::detect_height_mode)); ignored for every other role.
    pub height_mode: HeightMode,
    /// The raw encoded image bytes (png/jpg/…); the loader sniffs the format on decode.
    pub bytes: Vec<u8>,
}

/// The result of baking a material container: the parent material id, its `.smatx` path,
/// the catalog rows it contributes (the self-container material + its hidden texture
/// sub-rows), and the space-joined role summary.
pub struct MaterialBakeResult {
    /// The baked material's id (also its own container id).
    pub material_id: Uuid,
    /// Project-relative path to the `.smatx`.
    pub path: String,
    /// The catalog rows the container contributes.
    pub rows: Vec<AssetEntry>,
    /// The space-joined roles that were slotted (e.g. `"albedo normal roughness height "`).
    pub roles: String,
}

/// What a scan changed relative to the live catalog: rows added (newly discovered on
/// disk) and ids removed (their backing file is gone).
///
/// The filesystem is the source of truth, so an unsaved import can never become a dead
/// orphan — its `.smodel` is rediscovered on the next scan.
#[derive(Clone, Debug, Default)]
pub struct ScanDelta {
    /// Catalog rows newly discovered on disk.
    pub added: Vec<AssetEntry>,
    /// Ids whose backing file vanished.
    pub removed: Vec<Uuid>,
}

/// The catalog rows a container contributes: one `parent_type` parent + one row per
/// embedded sub-asset (container linkage + chunk index + colorspace).
///
/// Shared by [`AssetServer::bake_model`](crate::AssetServer::bake_model) /
/// [`AssetServer::bake_material_container`](crate::AssetServer::bake_material_container) and the
/// scan so a freshly-baked container and a rediscovered one yield **identical** rows. A
/// `Model` parent is a standalone container (`container == 0`); a texture-embedding
/// `Material` parent points its own `container` at itself so its `.smat` chunk resolves
/// through the shared container path while the frontend still shows it (its `container`
/// equals its `id`, and only sub-assets — `container != id` — are hidden). A rigged
/// container (its META carries a skin) flags every row so the editor routes a rigged mesh
/// to the rig editor without a per-click probe. An extracted (remapped) sub-asset is a
/// standalone file: its row points at the external path with `container == 0` / `chunk ==
/// -1`, so the scan agrees with the resolver and the ids never alias.
#[must_use]
pub fn catalog_rows_for_container(
    meta: &ContainerMetadata,
    relative_path: &str,
    parent_type: AssetType,
) -> Vec<AssetEntry> {
    let rigged = !meta.skin.is_null();
    let parent_container = if parent_type == AssetType::Material {
        meta.model_id
    } else {
        Uuid(0)
    };
    let mut rows = Vec::with_capacity(meta.sub_assets.len() + 1);
    rows.push(AssetEntry {
        id: meta.model_id,
        name: meta.name.clone(),
        asset_type: parent_type,
        path: relative_path.to_owned(),
        rigged,
        container: parent_container,
        content_hash: model_content_hash(meta),
        ..AssetEntry::default()
    });
    for sub in &meta.sub_assets {
        let mut row = AssetEntry {
            id: sub.sub_id,
            name: sub.name.clone(),
            asset_type: sub.asset_type,
            rigged,
            colorspace: colorspace_from_name(&sub.colorspace),
            duration: sub.duration,
            tracks: sub.tracks,
            content_hash: sub.content_hash,
            ..AssetEntry::default()
        };
        let key = sub.sub_id.value().to_string();
        let remapped = meta
            .remap
            .as_object()
            .and_then(|m| m.get(&key))
            .and_then(|entry| entry.get("external"))
            .and_then(Value::as_str);
        if let Some(external) = remapped {
            row.path = external.to_owned();
            row.container = Uuid(0);
            row.chunk = -1;
        } else {
            row.path = relative_path.to_owned();
            row.container = meta.model_id;
            row.chunk = sub.chunk as i32;
        }
        rows.push(row);
    }
    rows
}

/// The FNV-1a offset basis (64-bit).
const FNV_OFFSET: u64 = 1469598103934665603;
/// The FNV-1a prime (64-bit).
const FNV_PRIME: u64 = 1099511628211;

/// One FNV-1a step: mix `v` into the running `hash`.
#[must_use]
fn fnv_mix(hash: u64, v: u64) -> u64 {
    (hash ^ v).wrapping_mul(FNV_PRIME)
}

/// The FNV-1a fold over a byte slice — the content hash of a baked chunk (mesh/texture/
/// material bytes), used as the content-addressed thumbnail cache key.
#[must_use]
pub fn hash_bytes_fnv(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &byte in bytes {
        hash = fnv_mix(hash, u64::from(byte));
    }
    hash
}

/// The FNV-1a fold over a source file's bytes, as a decimal string. The reimport recipe
/// stores this — a **content** hash, not the mtime — so a touched-but-unchanged source is
/// a content-addressed skip. An unreadable path hashes to the empty string.
#[must_use]
pub fn hash_file_fnv(path: &str) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    hash_bytes_fnv(&bytes).to_string()
}

/// The model preview's content hash: a fold of its embedded mesh/material/texture sub-hashes
/// plus the canonical (sorted) node block, so the whole-forest thumbnail key changes only
/// when the baked geometry, materials, or layout change. Deterministic across bake and scan
/// (sub-hashes live in META; nodes fold via [`dump_json_sorted`]). Returns `0` when no sub-asset
/// carries a per-chunk hash, which leaves the model row uncacheable until one is derived.
#[must_use]
fn model_content_hash(meta: &ContainerMetadata) -> u64 {
    let mut hash = FNV_OFFSET;
    let mut folded = 0usize;
    for sub in &meta.sub_assets {
        if sub.content_hash != 0 {
            hash = fnv_mix(hash, sub.sub_id.value());
            hash = fnv_mix(hash, sub.content_hash);
            folded += 1;
        }
    }
    if folded == 0 {
        return 0;
    }
    for byte in dump_json_sorted(&meta.nodes, -1).into_bytes() {
        hash = fnv_mix(hash, u64::from(byte));
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::test_support::scratch;

    #[test]
    fn import_options_json_round_trips() {
        let options = ImportOptions {
            scale: 2.5,
            axis: Axis::ZUp,
            gen_tangents: false,
            embed_textures: true,
        };
        let restored = ImportOptions::from_json(&options.to_json());
        assert_eq!(restored, options);
        let default_json = ImportOptions::default().to_json();
        assert_eq!(
            default_json
                .get("axis")
                .and_then(saffron_json::Value::as_str),
            Some("y-up")
        );
        assert_eq!(
            ImportOptions::from_json(&default_json),
            ImportOptions::default()
        );
    }

    #[test]
    fn colorspace_for_role_keys_albedo_and_emissive_srgb() {
        let options = ImportOptions::default();
        use saffron_geometry::MaterialMapRole as Role;
        assert_eq!(options.colorspace_for(Role::Albedo), Colorspace::Srgb);
        assert_eq!(options.colorspace_for(Role::Emissive), Colorspace::Srgb);
        assert_eq!(options.colorspace_for(Role::Normal), Colorspace::Linear);
        assert_eq!(
            options.colorspace_for(Role::MetallicRoughness),
            Colorspace::Linear
        );
        assert_eq!(options.colorspace_for(Role::Occlusion), Colorspace::Linear);
        assert_eq!(options.colorspace_for(Role::Height), Colorspace::Linear);
    }

    #[test]
    fn hash_file_fnv_is_content_addressed() {
        let dir = scratch("hash");
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        std::fs::write(&a, b"hello world").unwrap();
        std::fs::write(&b, b"hello world").unwrap();
        let ha = hash_file_fnv(a.to_str().unwrap());
        let hb = hash_file_fnv(b.to_str().unwrap());
        assert_eq!(ha, hb, "identical content hashes identically");
        assert!(!ha.is_empty());
        std::fs::write(&b, b"goodbye world").unwrap();
        assert_ne!(ha, hash_file_fnv(b.to_str().unwrap()));
        assert_eq!(hash_file_fnv("/nonexistent/path/xyz"), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_options_default_matches_the_documented_defaults() {
        let options = ImportOptions::default();
        assert_eq!(options.scale, 1.0);
        assert_eq!(options.axis, Axis::YUp);
        assert!(options.gen_tangents);
        assert!(options.embed_textures);
        assert_eq!(IMPORTER_VERSION, 1);
    }
}
