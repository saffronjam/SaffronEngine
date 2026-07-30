use std::collections::HashSet;

use saffron_core::Uuid;
use saffron_geometry::translate_model;
use saffron_json::Value;
use saffron_scene::AssetType;

use crate::import::{IMPORTER_VERSION, ImportOptions, catalog_rows_for_container, hash_file_fnv};
use crate::model::read_container_metadata;
use crate::{AssetServer, Error, Result};

use super::container::rewrite_container_meta;

/// What a reimport changed, diffed by stable sub-id.
#[derive(Clone, Debug, Default)]
pub struct ReimportDelta {
    /// Sub-ids the source still produces (re-baked bytes).
    pub updated: Vec<Uuid>,
    /// Sub-ids the source newly produces.
    pub added: Vec<Uuid>,
    /// Sub-ids absent from the source, kept and reported for cleanup to decide.
    pub removed_from_source: Vec<Uuid>,
    /// The source bytes + importer version are unchanged — nothing was re-baked.
    pub skipped: bool,
}

/// Re-bakes a container from its stored source + options when the source bytes changed
/// (else a content-addressed skip). Sub-ids are stable (by source name), so the diff
/// matches; an extracted (remapped) sub-asset's external override is preserved — the
/// freshly baked chunk is the dormant fallback. Live instances resolve by (model_id,
/// sub_id), so they pick up the new bytes with no re-instantiation. The caller idles the
/// GPU; this drops sub-id caches.
///
/// # Errors
///
/// [`Error::Io`] if the model is not loadable or the source is unreadable; propagates a
/// translate/bake/container failure.
pub fn reimport_model(assets: &mut AssetServer, model_id: Uuid) -> Result<ReimportDelta> {
    let mut delta = ReimportDelta::default();
    let model = assets
        .load_model_asset(model_id)
        .ok_or_else(|| Error::Io(format!("model {} is not loadable", model_id.value())))?;
    let old_meta = model.meta.clone();
    let source = old_meta.import.source_path.clone();
    let current_hash = hash_file_fnv(&source);
    if current_hash.is_empty() {
        return Err(Error::Io(format!("source '{source}' is unreadable")));
    }
    if current_hash == old_meta.import.source_hash
        && old_meta.import.importer_version == IMPORTER_VERSION
    {
        delta.skipped = true;
        return Ok(delta);
    }

    let old_subs: HashSet<u64> = old_meta
        .sub_assets
        .iter()
        .map(|s| s.sub_id.value())
        .collect();

    let graph = translate_model(&source)?;
    let options = ImportOptions::from_json(&old_meta.import.options);
    let bake = assets.bake_model(&graph, options, &source, model_id)?;

    let container_full = format!("{}/{}", assets.root.display(), bake.path);
    let mut new_meta = read_container_metadata(&container_full)?;
    let new_subs: HashSet<u64> = new_meta
        .sub_assets
        .iter()
        .map(|s| s.sub_id.value())
        .collect();

    // Keep the remap for sub-assets that still exist, so an extracted edit survives the reimport.
    let mut kept_remap = serde_json::Map::new();
    if let Some(object) = old_meta.remap.as_object() {
        for (key, value) in object {
            let sid: u64 = key.parse().unwrap_or(0);
            if new_subs.contains(&sid) {
                kept_remap.insert(key.clone(), value.clone());
            }
        }
    }
    if !kept_remap.is_empty() {
        new_meta.remap = Value::Object(kept_remap);
        if let Ok(reader) = saffron_geometry::read_container(&container_full)
            && let Err(err) = rewrite_container_meta(&container_full, &reader, &new_meta)
        {
            tracing::warn!(
                "reimport: could not preserve remap for model {}: {err}",
                model_id.value()
            );
        }
    }

    for sid in &new_subs {
        if old_subs.contains(sid) {
            delta.updated.push(Uuid(*sid));
        } else {
            delta.added.push(Uuid(*sid));
        }
    }
    for sid in &old_subs {
        if !new_subs.contains(sid) {
            delta.removed_from_source.push(Uuid(*sid));
        }
    }

    if let Ok(final_meta) = read_container_metadata(&container_full) {
        for row in catalog_rows_for_container(&final_meta, &bake.path, AssetType::Model) {
            assets.register_reimported_asset(row);
        }
    }
    for sid in old_subs.difference(&new_subs) {
        let _ = assets.asset_edited(Uuid(*sid));
    }
    Ok(delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::{bake_and_register, one_material_graph, scratch};

    #[test]
    fn reimport_skips_when_the_source_is_unchanged() {
        let dir = scratch("reimport-skip");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // A real source file so the stored hash matches the recomputed one.
        let source = dir.join("paint.obj");
        std::fs::write(&source, b"o cube\nv 0 0 0\n").unwrap();
        let source_path = source.to_string_lossy().into_owned();
        let model_id = bake_and_register(&mut assets, &one_material_graph(), &source_path);

        let delta = reimport_model(&mut assets, model_id).expect("reimport");
        assert!(
            delta.skipped,
            "unchanged source is a content-addressed skip"
        );
        assert!(delta.updated.is_empty());
        assert!(delta.added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reimport_errors_when_the_source_is_unreadable() {
        let dir = scratch("reimport-missing");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let model_id = bake_and_register(&mut assets, &one_material_graph(), "/no/such/source.obj");

        let err = reimport_model(&mut assets, model_id).expect_err("unreadable source errors");
        assert!(matches!(err, Error::Io(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
