//! Catalog ↔ JSON serde (entries and folder lists).
//!
//! The catalog is the live id → `{name, type, path, …}` table the renderer and pick
//! read from. It serializes into the regenerable catalog cache and the `project.json`
//! `assets` block, so the byte shape is a frozen contract:
//! every row carries `id`/`name`/`type`/`path`/`folder`/`hdr`/`linear`, and the
//! optional `duration`/`tracks`/`container`/`chunk`/`colorspace`/`rigged` fields are
//! omitted when default so a standalone row stays minimal. The reader is lenient
//! (unknown keys ignored, missing keys default).

use saffron_core::Uuid;
use saffron_json::{Value, json_bool_or, json_f32_or, json_string_or, json_u64_or, uuid_to_json};
use saffron_scene::{AssetCatalog, AssetEntry, AssetType, Attribution, Colorspace};

use crate::names::{asset_type_from_name, asset_type_name, colorspace_from_name, colorspace_name};

/// Serializes a catalog's entries to the `assets` JSON array.
///
/// Each row always carries `id`/`name`/`type`/`path`/`folder`/`hdr`/`linear`. The
/// container linkage (`container`/`chunk`), the texture `colorspace`, the animation
/// `duration`/`tracks`, and the `rigged` flag are emitted only when non-default, so a
/// standalone row carries only the fields it uses.
#[must_use]
pub fn catalog_to_json(catalog: &AssetCatalog) -> Value {
    let mut assets = Vec::with_capacity(catalog.entries.len());
    for entry in &catalog.entries {
        let mut record = serde_json::Map::new();
        record.insert("id".to_owned(), uuid_to_json(entry.id.value()));
        record.insert("name".to_owned(), Value::String(entry.name.clone()));
        record.insert(
            "type".to_owned(),
            Value::String(asset_type_name(entry.asset_type).to_owned()),
        );
        record.insert("path".to_owned(), Value::String(entry.path.clone()));
        record.insert("folder".to_owned(), Value::String(entry.folder.clone()));
        record.insert("hdr".to_owned(), Value::Bool(entry.hdr));
        record.insert("linear".to_owned(), Value::Bool(entry.linear));
        if entry.asset_type == AssetType::Animation {
            record.insert("duration".to_owned(), Value::from(entry.duration));
            record.insert("tracks".to_owned(), Value::from(entry.tracks));
        }
        if entry.container.value() != 0 {
            record.insert(
                "container".to_owned(),
                uuid_to_json(entry.container.value()),
            );
        }
        if entry.chunk >= 0 {
            record.insert("chunk".to_owned(), Value::from(entry.chunk));
        }
        if entry.colorspace != Colorspace::Auto {
            record.insert(
                "colorspace".to_owned(),
                Value::String(colorspace_name(entry.colorspace).to_owned()),
            );
        }
        if entry.rigged {
            record.insert("rigged".to_owned(), Value::Bool(true));
        }
        if let Some(attribution) = &entry.attribution {
            record.insert("attribution".to_owned(), attribution_to_json(attribution));
        }
        if entry.content_hash != 0 {
            record.insert("contentHash".to_owned(), Value::from(entry.content_hash));
        }
        assets.push(Value::Object(record));
    }
    Value::Array(assets)
}

/// Rebuilds a catalog's entries from an `assets` JSON array.
///
/// Clears the catalog's entries + index first, then re-inserts every well-formed row
/// (an `id` of `0` or a non-object element is skipped). Lenient: missing keys take
/// their defaults, unknown keys are ignored.
pub fn catalog_from_json(catalog: &mut AssetCatalog, assets: &Value) {
    catalog.entries.clear();
    catalog.by_id.clear();
    let Some(records) = assets.as_array() else {
        return;
    };
    for record in records {
        if !record.is_object() {
            continue;
        }
        let chunk = record
            .get("chunk")
            .and_then(Value::as_i64)
            .map_or(-1, |c| c as i32);
        let parsed = AssetEntry {
            id: Uuid(json_u64_or(record, "id", 0)),
            name: json_string_or(record, "name", String::new()),
            asset_type: asset_type_from_name(&json_string_or(record, "type", "mesh".to_owned())),
            path: json_string_or(record, "path", String::new()),
            folder: json_string_or(record, "folder", String::new()),
            hdr: json_bool_or(record, "hdr", false),
            linear: json_bool_or(record, "linear", false),
            duration: json_f32_or(record, "duration", 0.0),
            tracks: json_u64_or(record, "tracks", 0) as i32,
            rigged: json_bool_or(record, "rigged", false),
            container: Uuid(json_u64_or(record, "container", 0)),
            chunk,
            colorspace: colorspace_from_name(&json_string_or(
                record,
                "colorspace",
                "auto".to_owned(),
            )),
            attribution: record.get("attribution").and_then(attribution_from_json),
            content_hash: json_u64_or(record, "contentHash", 0),
        };
        if parsed.id.value() != 0 {
            catalog.put(parsed);
        }
    }
}

/// Serializes store attribution to a JSON object.
fn attribution_to_json(attribution: &Attribution) -> Value {
    let mut record = serde_json::Map::new();
    record.insert(
        "licenseId".to_owned(),
        Value::String(attribution.license_id.clone()),
    );
    record.insert(
        "requiresAttribution".to_owned(),
        Value::Bool(attribution.requires_attribution),
    );
    record.insert(
        "licenseUrl".to_owned(),
        Value::String(attribution.license_url.clone()),
    );
    record.insert(
        "author".to_owned(),
        Value::String(attribution.author.clone()),
    );
    record.insert(
        "sourceUrl".to_owned(),
        Value::String(attribution.source_url.clone()),
    );
    record.insert(
        "storeId".to_owned(),
        Value::String(attribution.store_id.clone()),
    );
    Value::Object(record)
}

/// Rebuilds store attribution from a JSON object, or `None` for a non-object.
fn attribution_from_json(record: &Value) -> Option<Attribution> {
    if !record.is_object() {
        return None;
    }
    Some(Attribution {
        license_id: json_string_or(record, "licenseId", String::new()),
        requires_attribution: json_bool_or(record, "requiresAttribution", false),
        license_url: json_string_or(record, "licenseUrl", String::new()),
        author: json_string_or(record, "author", String::new()),
        source_url: json_string_or(record, "sourceUrl", String::new()),
        store_id: json_string_or(record, "storeId", String::new()),
    })
}

/// Serializes a catalog's folder list to a JSON string array.
#[must_use]
pub fn catalog_folders_to_json(catalog: &AssetCatalog) -> Value {
    Value::Array(
        catalog
            .folders
            .iter()
            .map(|folder| Value::String(folder.clone()))
            .collect(),
    )
}

/// Rebuilds a catalog's folder list from a JSON string array. Non-string elements are
/// skipped.
pub fn catalog_folders_from_json(catalog: &mut AssetCatalog, folders: &Value) {
    catalog.folders.clear();
    let Some(records) = folders.as_array() else {
        return;
    };
    for folder in records {
        if let Some(name) = folder.as_str() {
            catalog.folders.push(name.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model parent + embedded sub-assets + a standalone asset round-trips through JSON
    /// with container linkage, chunk index, and colorspace intact.
    #[test]
    fn catalog_model_and_sub_asset_linkage_round_trips() {
        let mut catalog = AssetCatalog::default();
        catalog.put(AssetEntry {
            id: Uuid(100),
            name: "town".to_owned(),
            asset_type: AssetType::Model,
            path: "models/100.smodel".to_owned(),
            ..AssetEntry::default()
        });
        catalog.put(AssetEntry {
            id: Uuid(101),
            name: "town_mesh".to_owned(),
            asset_type: AssetType::Mesh,
            path: "models/100.smodel".to_owned(),
            container: Uuid(100),
            chunk: 1,
            ..AssetEntry::default()
        });
        catalog.put(AssetEntry {
            id: Uuid(102),
            name: "town_albedo".to_owned(),
            asset_type: AssetType::Texture,
            path: "models/100.smodel".to_owned(),
            container: Uuid(100),
            chunk: 2,
            colorspace: Colorspace::Srgb,
            ..AssetEntry::default()
        });
        catalog.put(AssetEntry {
            id: Uuid(200),
            name: "loose".to_owned(),
            asset_type: AssetType::Material,
            path: "materials/200.smat".to_owned(),
            ..AssetEntry::default()
        });

        let mut restored = AssetCatalog::default();
        catalog_from_json(&mut restored, &catalog_to_json(&catalog));

        assert_eq!(restored.entries.len(), 4);
        let model = restored.find(Uuid(100)).unwrap();
        assert_eq!(model.asset_type, AssetType::Model);
        assert_eq!(model.container, Uuid(0));
        assert_eq!(model.chunk, -1);
        let mesh = restored.find(Uuid(101)).unwrap();
        assert_eq!(mesh.container, Uuid(100));
        assert_eq!(mesh.chunk, 1);
        let tex = restored.find(Uuid(102)).unwrap();
        assert_eq!(tex.container, Uuid(100));
        assert_eq!(tex.chunk, 2);
        assert_eq!(tex.colorspace, Colorspace::Srgb);
        let loose = restored.find(Uuid(200)).unwrap();
        assert_eq!(loose.container, Uuid(0));
        assert_eq!(loose.chunk, -1);
        assert_eq!(loose.colorspace, Colorspace::Auto);
    }

    #[test]
    fn standalone_rows_omit_default_fields() {
        let mut catalog = AssetCatalog::default();
        catalog.put(AssetEntry {
            id: Uuid(7),
            name: "tex".to_owned(),
            asset_type: AssetType::Texture,
            path: "textures/7.png".to_owned(),
            ..AssetEntry::default()
        });
        let json = catalog_to_json(&catalog);
        let row = &json.as_array().unwrap()[0];
        // A standalone, Auto-colorspace, non-rigged texture row omits the optional keys.
        assert!(row.get("container").is_none());
        assert!(row.get("chunk").is_none());
        assert!(row.get("colorspace").is_none());
        assert!(row.get("rigged").is_none());
        assert!(row.get("duration").is_none());
    }

    #[test]
    fn animation_rows_carry_duration_and_tracks() {
        let mut catalog = AssetCatalog::default();
        catalog.put(AssetEntry {
            id: Uuid(9),
            name: "walk".to_owned(),
            asset_type: AssetType::Animation,
            path: "animations/9.sanim".to_owned(),
            duration: 1.25,
            tracks: 4,
            ..AssetEntry::default()
        });
        let mut restored = AssetCatalog::default();
        catalog_from_json(&mut restored, &catalog_to_json(&catalog));
        let row = restored.find(Uuid(9)).unwrap();
        assert!((row.duration - 1.25).abs() < 1e-6);
        assert_eq!(row.tracks, 4);
    }

    #[test]
    fn folders_round_trip() {
        let catalog = AssetCatalog {
            folders: vec!["props".to_owned(), "props/rocks".to_owned()],
            ..AssetCatalog::default()
        };
        let mut restored = AssetCatalog::default();
        catalog_folders_from_json(&mut restored, &catalog_folders_to_json(&catalog));
        assert_eq!(restored.folders, catalog.folders);
    }
}
