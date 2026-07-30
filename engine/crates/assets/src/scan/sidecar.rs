//! The `.smeta` sidecar: an asset's durable, id-keyed name / folder / texture metadata.

use saffron_core::Uuid;
use saffron_json::{Value, dump_json, json_string_or, json_u64_or, parse_json, uuid_to_json};
use saffron_scene::{AssetType, Colorspace, TextureRole};

use crate::AssetServer;
use crate::error::{Error, Result};
use crate::names::{
    asset_type_from_name, asset_type_name, colorspace_from_name, colorspace_name,
    texture_role_from_name, texture_role_name,
};

/// The `.smeta` sidecar co-located with an asset file — the durable, id-keyed home for its
/// name / folder / texture metadata.
///
/// A foreign/headerless file (a raw `.png` dropped into `assets/`) also uses it for a stable
/// id (its bytes carry none). Engine-written files (`.smodel`, `textures/<uuid>.*`, extracted
/// `.smat`/`.smesh`/`.sanim`) take their *identity* from the uuid stem / container, but their
/// *metadata* lives here too — written eagerly on import and on every rename/move so it
/// survives a cold scan without a project save.
#[derive(Clone, Debug)]
pub(super) struct SmetaData {
    pub(super) id: Uuid,
    pub(super) asset_type: AssetType,
    pub(super) colorspace: Colorspace,
    pub(super) role: TextureRole,
    pub(super) folder: String,
    pub(super) name: String,
}

impl Default for SmetaData {
    fn default() -> Self {
        Self {
            id: Uuid(0),
            asset_type: AssetType::Texture,
            colorspace: Colorspace::Auto,
            role: TextureRole::Unknown,
            folder: String::new(),
            name: String::new(),
        }
    }
}

/// Reads a `.smeta` sidecar.
///
/// # Errors
///
/// [`Error::Io`] if the file is unreadable or not a JSON object, or has no id.
pub(super) fn read_smeta(path: &str) -> Result<SmetaData> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Io(format!("cannot open '{path}': {e}")))?;
    let doc = parse_json(&text)?;
    if !doc.is_object() {
        return Err(Error::Io(format!("'{path}' is not a valid .smeta")));
    }
    let meta = SmetaData {
        id: Uuid(json_u64_or(&doc, "id", 0)),
        asset_type: asset_type_from_name(&json_string_or(&doc, "type", "texture".to_owned())),
        colorspace: colorspace_from_name(&json_string_or(&doc, "colorspace", "auto".to_owned())),
        role: texture_role_from_name(&json_string_or(&doc, "role", "unknown".to_owned())),
        folder: json_string_or(&doc, "folder", String::new()),
        name: json_string_or(&doc, "name", String::new()),
    };
    if meta.id.value() == 0 {
        return Err(Error::Io(format!("'{path}' has no id")));
    }
    Ok(meta)
}

/// Writes a `.smeta` sidecar (pretty, 2-space indent).
///
/// # Errors
///
/// [`Error::Io`] if the file cannot be written.
pub(super) fn write_smeta(path: &str, meta: &SmetaData) -> Result<()> {
    let mut doc = serde_json::Map::new();
    doc.insert("version".to_owned(), Value::from(1));
    doc.insert("id".to_owned(), uuid_to_json(meta.id.value()));
    doc.insert(
        "type".to_owned(),
        Value::String(asset_type_name(meta.asset_type).to_owned()),
    );
    doc.insert(
        "colorspace".to_owned(),
        Value::String(colorspace_name(meta.colorspace).to_owned()),
    );
    if meta.role != TextureRole::Unknown {
        doc.insert(
            "role".to_owned(),
            Value::String(texture_role_name(meta.role).to_owned()),
        );
    }
    if !meta.folder.is_empty() {
        doc.insert("folder".to_owned(), Value::String(meta.folder.clone()));
    }
    if !meta.name.is_empty() {
        doc.insert("name".to_owned(), Value::String(meta.name.clone()));
    }
    std::fs::write(path, dump_json(&Value::Object(doc), 2))
        .map_err(|e| Error::Io(format!("cannot write '{path}': {e}")))
}

impl AssetServer {
    /// Writes the durable `<path>.smeta` sidecar for the catalog row `id` — its name, folder,
    /// and (for a texture) colorspace and role. Call after any create / rename / move so the metadata
    /// survives a cold scan without a project save; the cold scan's sidecar overlay reads it back.
    ///
    /// A no-op for an unknown id, a row with no own file, or an **embedded** sub-asset
    /// (`container != 0`): those share their `.smodel`'s path, so writing there would clobber the
    /// model's own sidecar — their durable rename is out of scope (identity/name stay in the
    /// container META). The model row and *extracted* sub-assets have `container == 0` and their
    /// own file, so they get one.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the sidecar cannot be written.
    pub fn write_asset_sidecar(&self, id: Uuid) -> Result<()> {
        let Some(row) = self.catalog.find(id) else {
            return Ok(());
        };
        if row.path.is_empty() || row.container != Uuid(0) {
            return Ok(());
        }
        // Resolve a texture's upload space the way the loader does, so the sidecar records a
        // concrete colorspace (never `Auto`) and a never-saved linear data map can't rescan as
        // sRGB. Non-textures carry the row's colorspace verbatim (meaningless but harmless).
        let colorspace = if row.asset_type == AssetType::Texture {
            if row.colorspace != Colorspace::Auto {
                row.colorspace
            } else if row.hdr {
                Colorspace::Hdr
            } else if row.linear {
                Colorspace::Linear
            } else {
                Colorspace::Srgb
            }
        } else {
            row.colorspace
        };
        let meta = SmetaData {
            id,
            asset_type: row.asset_type,
            colorspace,
            role: row.role,
            folder: row.folder.clone(),
            name: row.name.clone(),
        };
        let smeta_path = format!("{}/{}.smeta", self.root.display(), row.path);
        write_smeta(&smeta_path, &meta)
    }

    /// Removes the `<path>.smeta` sidecar co-located with the row for `id`, if any. Best-effort;
    /// call before dropping a row on delete so no orphan sidecar lingers. Skips embedded
    /// sub-assets (`container != 0`) so it never deletes the shared model sidecar.
    pub fn remove_asset_sidecar(&self, id: Uuid) {
        if let Some(row) = self.catalog.find(id)
            && !row.path.is_empty()
            && row.container == Uuid(0)
        {
            let smeta_path = format!("{}/{}.smeta", self.root.display(), row.path);
            let _ = std::fs::remove_file(smeta_path);
        }
    }
}
