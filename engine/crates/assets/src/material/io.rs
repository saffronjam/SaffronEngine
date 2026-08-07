use saffron_core::Uuid;
use saffron_geometry::ChunkKind;
use saffron_json::parse_json;
use saffron_scene::{AssetEntry, AssetType};

use crate::error::{Error, Result};
use crate::{AssetServer, DEFAULT_MATERIAL_ID};

use super::codec::{material_asset_from_json, material_asset_to_text};
use super::overrides::apply_overrides;
use super::{MAX_INSTANCE_DEPTH, MaterialAsset, default_material_asset};

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
    material_asset_from_json(&doc)
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
    material_asset_from_json(&doc)
}

/// Reads a `.smat` resolved for rendering.
///
/// An instance (`parent != 0`) resolves to its parent's resolved params with this
/// material's `overrides` applied on top, keeping `parent` + `overrides` so the editor
/// still sees an instance. Resolution recurses to a fixed depth cap;
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
    if material.parent.value() != 0
        && depth < MAX_INSTANCE_DEPTH
        && let Ok(parent_resolved) = load_material_asset_at(assets, material.parent, depth + 1)
    {
        let mut base = parent_resolved;
        apply_overrides(&mut base, &material.overrides);
        base.parent = material.parent;
        base.overrides = material.overrides;
        return Ok(base);
    }
    // A missing or cyclic parent falls back to this material's own stored params.
    Ok(material)
}

fn load_catalog_material_asset_at(
    assets: &mut AssetServer,
    id: Uuid,
    depth: u32,
) -> Result<MaterialAsset> {
    let material = load_catalog_material_asset_raw(assets, id)?;
    if material.parent.value() != 0
        && depth < MAX_INSTANCE_DEPTH
        && let Ok(parent_resolved) =
            load_catalog_material_asset_at(assets, material.parent, depth + 1)
    {
        let mut base = parent_resolved;
        apply_overrides(&mut base, &material.overrides);
        base.parent = material.parent;
        base.overrides = material.overrides;
        return Ok(base);
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
    material.surface.validate()?;
    let id = Uuid::new();
    assets.ensure_asset_directories();
    let relative_path = format!("materials/{}.smat", id.value());
    let text = material_asset_to_text(material, 2);
    std::fs::write(assets.root.join(&relative_path), text).map_err(|e| Error::Io(e.to_string()))?;
    let unique = assets.catalog.unique_name(name);
    assets.register_imported_asset(AssetEntry {
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
    material.surface.validate()?;
    let (container, rel_path) = {
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
        (entry.container, entry.path.clone())
    };
    let text = material_asset_to_text(material, 2);
    if container.value() == 0 {
        // Standalone `.smat`: the whole file is the material JSON.
        let path = assets.root.join(&rel_path);
        std::fs::write(path, text).map_err(|e| Error::Io(e.to_string()))?;
        // The edited material (and every instance that resolves through it) is now stale.
        let _ = assets.asset_edited(id);
    } else {
        // Container-embedded (a `.smatx` self-container, or a model container): rewrite only the
        // material chunk, preserving the META + every texture chunk. Invalidates caches internally.
        crate::manage::rewrite_material_chunk(assets, id, text.into_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use saffron_geometry::glam::Vec4;

    use super::*;

    use super::super::test_support::{populated_material, scratch_server};

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
        let resolved = load_material_asset(&assets, DEFAULT_MATERIAL_ID).unwrap();
        assert_eq!(resolved, default_material_asset());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_raw_errors_on_missing_and_wrong_type() {
        let (mut assets, tmp) = scratch_server("errors");
        assert!(matches!(
            load_material_asset_raw(&assets, Uuid(5000)),
            Err(Error::NotInCatalog(5000))
        ));
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
            // Discarded in favour of the parent's, with the overrides applied on top.
            base_color: Vec4::new(9.0, 9.0, 9.0, 9.0),
            metallic: 0.5,
            overrides: serde_json::json!({ "metallic": 0.75, "roughness": 0.2 }),
            ..MaterialAsset::default()
        };
        let child_id = save_material_asset(&mut assets, &child, "Child", "").unwrap();

        let resolved = load_material_asset(&assets, child_id).unwrap();
        assert_eq!(resolved.base_color, Vec4::new(0.2, 0.2, 0.2, 1.0));
        assert_eq!(resolved.albedo_texture, Uuid(1111));
        assert_eq!(resolved.metallic, 0.75);
        assert_eq!(resolved.roughness, 0.2);
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
        assert_eq!(resolved.metallic, 0.42);
        assert_eq!(resolved.roughness, 1.0);
        assert_eq!(resolved.parent, Uuid(424_242));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cyclic_parent_chain_terminates_without_infinite_recursion() {
        let (mut assets, tmp) = scratch_server("cycle");
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

        let resolved = load_material_asset(&assets, a_id).unwrap();
        // The exact resolved value is the depth cap's fallback; the contract is *termination*.
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

        assert_eq!(assets.catalog.find(id).unwrap().path, path_before);
        let reloaded = load_material_asset_raw(&assets, id).unwrap();
        assert_eq!(reloaded.roughness, 0.05);

        assert!(matches!(
            update_material_asset(&mut assets, Uuid(7777), &edited),
            Err(Error::NotInCatalog(7777))
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
