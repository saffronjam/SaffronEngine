use saffron_core::Uuid;
use saffron_geometry::ChunkKind;
use saffron_json::Value;
use saffron_scene::{AssetEntry, AssetType};

use crate::names::colorspace_from_name;
use crate::{AssetServer, Error, Result};

use super::container::{
    container_path, default_extract_dest, image_ext_from_bytes, rewrite_container_meta,
};

/// Slices a sub-asset's chunk out of its container to a standalone file (keeping the same
/// sub-id), registers a standalone catalog row for it, and writes a remap entry so
/// resolution prefers the external file. The container's bytes are otherwise untouched;
/// [`clear_extraction`] reverts. Returns the standalone asset's id (`== sub_id`). `dest`
/// is project-relative; empty picks the per-type default.
///
/// # Errors
///
/// [`Error::Io`] if the model is not loadable, lacks the sub-asset/chunk, or the external
/// file cannot be written; propagates a container chunk-read / rewrite failure.
pub fn extract_sub_asset(
    assets: &mut AssetServer,
    model_id: Uuid,
    sub_id: Uuid,
    dest: &str,
) -> Result<Uuid> {
    let model = assets
        .load_model_asset(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let sub = model
        .meta
        .sub_assets
        .iter()
        .find(|s| s.sub_id.value() == sub_id.value())
        .cloned()
        .ok_or(Error::ContainerMissingSubAsset {
            container: model_id.value(),
            sub: sub_id.value(),
        })?;
    let entry = model
        .reader
        .toc()
        .iter()
        .find(|e| e.sub_id == sub_id.value() && e.fourcc != ChunkKind::Meta as u32)
        .copied()
        .ok_or_else(|| {
            Error::Io(format!(
                "model {} has no chunk for sub-asset {}",
                model_id.value(),
                sub_id.value()
            ))
        })?;
    let bytes = model.reader.read_chunk(&entry)?;

    let relative_dest = if dest.is_empty() {
        let image_ext = if sub.asset_type == AssetType::Texture {
            image_ext_from_bytes(&bytes)
        } else {
            ""
        };
        default_extract_dest(sub.asset_type, sub_id, image_ext)
    } else {
        dest.to_owned()
    };
    let full_dest = format!("{}/{relative_dest}", assets.root.display());
    if let Some(parent) = std::path::Path::new(&full_dest).parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    std::fs::write(&full_dest, &bytes)
        .map_err(|e| Error::Io(format!("cannot write '{relative_dest}': {e}")))?;

    let mut updated = model.meta.clone();
    if !updated.remap.is_object() {
        updated.remap = Value::Object(serde_json::Map::new());
    }
    if let Some(object) = updated.remap.as_object_mut() {
        object.insert(
            sub_id.value().to_string(),
            serde_json::json!({ "external": relative_dest }),
        );
    }
    let container = container_path(assets, model_id)
        .ok_or_else(|| Error::Io(format!("model {} not in catalog", model_id.value())))?;
    let container_full = format!("{}/{container}", assets.root.display());
    rewrite_container_meta(&container_full, &model.reader, &updated)?;

    let replaced = assets.replace_edited_asset_entry(AssetEntry {
        id: sub_id,
        name: sub.name.clone(),
        asset_type: sub.asset_type,
        path: relative_dest,
        colorspace: colorspace_from_name(&sub.colorspace),
        duration: sub.duration,
        tracks: sub.tracks,
        ..AssetEntry::default()
    });
    debug_assert!(replaced, "extracted sub-asset remains catalogued");
    // Pin the name/colorspace to the now-standalone leaf so a cold scan can't fall back to the
    // uuid stem (and doesn't depend on the walk order vs the container's remap row).
    if let Err(err) = assets.write_asset_sidecar(sub_id) {
        tracing::warn!(
            "extract: could not write .smeta for {}: {err}",
            sub_id.value()
        );
    }

    // The reader's TOC offsets shifted, so drop it: the next resolve reads the external file.
    assets.model_by_uuid.remove(&model_id.value());
    Ok(sub_id)
}

/// Drops a sub-asset's extraction: removes the remap entry, deletes the external file (so
/// its uuid name can never alias the embedded chunk on a later scan), reverts the catalog
/// row to the embedded chunk, and refreshes caches.
///
/// # Errors
///
/// [`Error::Io`] if the model is not loadable / not in the catalog; propagates a container
/// rewrite failure.
pub fn clear_extraction(assets: &mut AssetServer, model_id: Uuid, sub_id: Uuid) -> Result<()> {
    let model = assets
        .load_model_asset(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let key = sub_id.value().to_string();
    let external = model
        .meta
        .remap
        .as_object()
        .and_then(|m| m.get(&key))
        .and_then(|entry| entry.get("external"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    let mut updated = model.meta.clone();
    if let Some(object) = updated.remap.as_object_mut() {
        object.remove(&key);
    }
    let container = container_path(assets, model_id)
        .ok_or_else(|| Error::Io(format!("model {} not in catalog", model_id.value())))?;
    let container_full = format!("{}/{container}", assets.root.display());
    rewrite_container_meta(&container_full, &model.reader, &updated)?;
    if let Some(external) = external {
        let _ = std::fs::remove_file(format!("{}/{external}", assets.root.display()));
        let _ = std::fs::remove_file(format!("{}/{external}.smeta", assets.root.display()));
    }

    if let Some(sub) = model
        .meta
        .sub_assets
        .iter()
        .find(|s| s.sub_id.value() == sub_id.value())
    {
        let replaced = assets.replace_edited_asset_entry(AssetEntry {
            id: sub_id,
            name: sub.name.clone(),
            asset_type: sub.asset_type,
            path: container.clone(),
            container: model_id,
            chunk: sub.chunk as i32,
            colorspace: colorspace_from_name(&sub.colorspace),
            duration: sub.duration,
            tracks: sub.tracks,
            ..AssetEntry::default()
        });
        debug_assert!(replaced, "embedded sub-asset remains catalogued");
    }
    assets.model_by_uuid.remove(&model_id.value());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::read_container_metadata;

    use super::super::test_support::{bake_and_register, first_sub, one_material_graph, scratch};

    #[test]
    fn extract_then_clear_round_trips_a_material_sub_asset() {
        let dir = scratch("extract");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let model_id = bake_and_register(&mut assets, &one_material_graph(), "/tmp/paint.obj");
        let material_sub = first_sub(&assets, model_id, AssetType::Material);

        assert_ne!(
            assets.catalog.find(material_sub).unwrap().container.value(),
            0
        );

        let extracted =
            extract_sub_asset(&mut assets, model_id, material_sub, "").expect("extract");
        assert_eq!(extracted, material_sub, "extraction keeps the sub-id");

        let row = assets.catalog.find(material_sub).unwrap();
        assert_eq!(row.container.value(), 0);
        assert_eq!(row.path, format!("materials/{}.smat", material_sub.value()));
        assert!(root.join(&row.path).exists(), "external file written");

        let container_path = assets.catalog.find(model_id).unwrap().path.clone();
        let meta = read_container_metadata(format!("{}/{container_path}", root.display())).unwrap();
        assert!(
            meta.remap
                .as_object()
                .is_some_and(|m| m.contains_key(&material_sub.value().to_string())),
            "remap records the extraction"
        );

        clear_extraction(&mut assets, model_id, material_sub).expect("clear");
        assert!(
            !root
                .join(format!("materials/{}.smat", material_sub.value()))
                .exists()
        );
        let reverted = assets.catalog.find(material_sub).unwrap();
        assert_eq!(reverted.container.value(), model_id.value());
        assert_eq!(reverted.path, container_path);
        let meta = read_container_metadata(format!("{}/{container_path}", root.display())).unwrap();
        assert!(
            meta.remap
                .as_object()
                .is_none_or(|m| !m.contains_key(&material_sub.value().to_string())),
            "remap entry cleared"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
