//! Filesystem-as-source-of-truth catalog reconciliation + standalone texture register.
//!
//! [`AssetServer::scan_assets`] walks the asset root (skipping the `.cache/` dir), rebuilds the
//! catalog from disk, and diffs it against the live one. The walk is **sorted**, so the catalog cache
//! is reproducible. A co-located `<path>.smeta` is overlaid on every row as the durable, id-keyed home
//! for its name / folder / colorspace — authoritative over the `project.json` seed, which is only as
//! fresh as the last save, so a never-saved import or rename survives a cold scan.
//!
//! [`AssetServer::load_catalog`] is the cache-fast path over `assets/.cache/catalog.json`, keyed by an
//! on-disk signature. The cache is **never** load-bearing: a cold scan always yields the identical
//! catalog.

mod reconcile;
mod roles;
mod sidecar;
mod texture;

#[cfg(test)]
mod test_support;

pub use reconcile::{reconcile_catalog_from_disk, resolve_catalog_from_disk};
pub use roles::{
    colorspace_for_role_explicit, detect_height_mode, detect_material_role, infer_texture_role,
    texture_role_from_hint,
};

use saffron_json::dump_json;
use saffron_scene::AssetCatalog;
use walkdir::WalkDir;

use crate::AssetServer;
use crate::catalog::{catalog_folders_to_json, catalog_to_json};
use crate::error::Result;
use crate::import::ScanDelta;

impl AssetServer {
    /// Walks `assets/`, rebuilds the catalog from disk, and diffs it against the live one.
    ///
    /// The filesystem is the source of truth: a never-saved import is rediscovered, a
    /// deleted file's row is dropped. Containers contribute rows via
    /// [`catalog_rows_for_container`](crate::catalog_rows_for_container);
    /// engine-written standalone files identify by their uuid filename stem; foreign files
    /// identify via a `.smeta` sidecar (minted + written on first sight). Display names +
    /// folders are preserved from the prior catalog by id.
    ///
    /// # Errors
    ///
    /// Currently infallible in practice (filesystem errors on individual entries are
    /// skipped with a warn), but returns [`Result`] so the cache-write caller composes
    /// with `?`.
    pub fn scan_assets(&mut self) -> Result<ScanDelta> {
        let (rebuilt, delta) =
            reconcile_catalog_from_disk(&self.root, &self.catalog, &mut |_, _, _| {});
        self.replace_scanned_catalog(rebuilt);
        Ok(delta)
    }
}

/// The `assets/.cache/catalog.json` path under `root`.
fn catalog_cache_path_for(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".cache").join("catalog.json")
}

/// Persists `catalog` + the current asset signature to `root`'s catalog cache. Free of any
/// `AssetServer` borrow so the off-thread loader can refresh the cache after a cold scan.
fn write_catalog_cache_to(root: &std::path::Path, catalog: &AssetCatalog) {
    let cache_path = catalog_cache_path_for(root);
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let doc = serde_json::json!({
        "version": 1,
        "signature": asset_signature(root).to_string(),
        "assets": catalog_to_json(catalog),
        "assetFolders": catalog_folders_to_json(catalog),
    });
    let _ = std::fs::write(&cache_path, dump_json(&doc, 0));
}

impl AssetServer {
    /// Builds the catalog with the cache as a latency shortcut: a valid, signature-matching
    /// `assets/.cache/catalog.json` is reused verbatim; on any mismatch / missing / corrupt
    /// cache it falls back to a full [`Self::scan_assets`] and refreshes the cache.
    ///
    /// The cache is never load-bearing — a cold scan always yields the identical catalog.
    ///
    /// # Errors
    ///
    /// Propagates a [`Self::scan_assets`] error on the fallback path.
    pub fn load_catalog(&mut self) -> Result<ScanDelta> {
        let (catalog, delta) =
            resolve_catalog_from_disk(&self.root, &self.catalog, &mut |_, _, _| {});
        self.replace_scanned_catalog(catalog);
        Ok(delta)
    }

    /// Persists the catalog + the current asset signature to `assets/.cache/catalog.json`.
    /// Regenerable and gitignored: deleting it is always safe (the next load is a cold scan).
    pub fn write_catalog_cache(&self) {
        write_catalog_cache_to(&self.root, &self.catalog);
    }
}

/// A cheap fingerprint of `assets/`: an FNV-1a fold over sorted `(relpath, mtime, size)`
/// for every file (including `.smeta` sidecars; excluding `.cache/`). Stat-only — no file
/// contents read. Any add / remove / touch / sidecar edit changes it, so it is a sound
/// trigger for invalidating the cache.
fn asset_signature(root: &std::path::Path) -> u64 {
    if !root.exists() {
        return 0;
    }
    let mut entries: Vec<String> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".cache")
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let rel = e
                .path()
                .strip_prefix(root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let meta = e.metadata().ok()?;
            let size = meta.len();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0u64, |d| d.as_nanos() as u64);
            Some(format!("{rel}|{mtime}|{size}"))
        })
        .collect();
    entries.sort();
    let mut hash = 1469598103934665603u64;
    for entry in &entries {
        for ch in entry.bytes() {
            hash ^= u64::from(ch);
            hash = hash.wrapping_mul(1099511628211u64);
        }
        hash = hash.wrapping_mul(1099511628211u64); // separator between entries
    }
    hash
}

#[cfg(test)]
mod tests {
    use saffron_core::Uuid;
    use saffron_scene::{AssetEntry, AssetType, Colorspace};

    use super::*;

    use super::test_support::{bake_fixture, png_2x2, scratch};

    #[test]
    fn fresh_scan_adds_a_containers_rows() {
        let dir = scratch("freshscan");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let (model_id, _path) = bake_fixture(&assets, "/tmp/flat.glb");

        // The catalog starts empty; the scan rediscovers the baked container.
        assert!(assets.catalog.entries.is_empty());
        let delta = assets.scan_assets().expect("scan");
        // 1 mesh + 1 material = 2 sub-assets; 3 rows (the Model parent + 2).
        assert_eq!(assets.catalog.entries.len(), 3);
        assert_eq!(delta.added.len(), 3);
        assert!(delta.removed.is_empty());
        let model = assets.catalog.find(model_id).expect("model row");
        assert_eq!(model.asset_type, AssetType::Model);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_after_deleting_the_file_removes_the_rows() {
        let dir = scratch("delete");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let (model_id, path) = bake_fixture(&assets, "/tmp/flat.glb");

        assets.scan_assets().expect("first scan");
        assert!(assets.catalog.find(model_id).is_some());

        // Delete the file and rescan: every row it contributed is dropped.
        std::fs::remove_file(format!("{}/{path}", root.display())).unwrap();
        let delta = assets.scan_assets().expect("rescan");
        assert!(
            assets.catalog.entries.is_empty(),
            "deleting the file drops its rows"
        );
        assert_eq!(delta.removed.len(), 3);
        assert!(delta.added.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_order_is_deterministic_across_runs() {
        let dir = scratch("order");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        // Several containers so the walk order matters.
        bake_fixture(&assets, "/tmp/a.glb");
        bake_fixture(&assets, "/tmp/b.glb");
        bake_fixture(&assets, "/tmp/c.glb");

        assets.scan_assets().expect("scan");
        let first: Vec<Uuid> = assets.catalog.entries.iter().map(|e| e.id).collect();

        // A fresh server over the same dir scans in the same order.
        let mut again = AssetServer::new(&root);
        again.scan_assets().expect("rescan");
        let second: Vec<Uuid> = again.catalog.entries.iter().map(|e| e.id).collect();
        assert_eq!(first, second, "the sorted walk yields a reproducible order");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_catalog_uses_the_cache_when_the_signature_matches() {
        let dir = scratch("cachehit");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        bake_fixture(&assets, "/tmp/flat.glb");

        // A cold load scans + writes the cache.
        let cold = assets.load_catalog().expect("cold load");
        assert_eq!(cold.added.len(), 3, "the cold path scans");
        assert!(
            root.join(".cache").join("catalog.json").exists(),
            "the cold load writes the cache"
        );
        let cold_ids: Vec<Uuid> = assets.catalog.entries.iter().map(|e| e.id).collect();

        // A second load over an unchanged dir is a cache hit: no scan delta, identical rows.
        let mut warm = AssetServer::new(&root);
        let warm_delta = warm.load_catalog().expect("warm load");
        assert!(warm_delta.added.is_empty(), "a cache hit reports no delta");
        let warm_ids: Vec<Uuid> = warm.catalog.entries.iter().map(|e| e.id).collect();
        assert_eq!(cold_ids, warm_ids, "the cache yields the cold-scan catalog");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_catalog_falls_back_to_a_scan_when_the_dir_changes() {
        let dir = scratch("cachemiss");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        bake_fixture(&assets, "/tmp/flat.glb");
        assets.load_catalog().expect("cold load writes the cache");

        // Add a second container, then load: the signature changed, so it cold-scans (and the
        // new container's rows appear).
        bake_fixture(&assets, "/tmp/second.glb");
        let mut reloaded = AssetServer::new(&root);
        reloaded.load_catalog().expect("reload");
        assert_eq!(
            reloaded.catalog.entries.len(),
            6,
            "both containers are catalogued"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_catalog_falls_back_to_a_clean_scan_on_a_corrupt_cache() {
        let dir = scratch("corruptcache");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        bake_fixture(&assets, "/tmp/flat.glb");

        // A cold load scans and writes the cache; that catalog is the source of truth.
        assets.load_catalog().expect("cold load");
        let baseline: Vec<Uuid> = assets.catalog.entries.iter().map(|e| e.id).collect();
        assert!(!baseline.is_empty());

        // Corrupt the cache file. A fresh server's load must fail to parse it and fall back to a
        // full scan, rebuilding the identical catalog — the cache is never load-bearing.
        std::fs::write(
            root.join(".cache").join("catalog.json"),
            b"{ not valid json ]",
        )
        .unwrap();
        let mut reloaded = AssetServer::new(&root);
        reloaded.load_catalog().expect("reload over corrupt cache");
        let recovered: Vec<Uuid> = reloaded.catalog.entries.iter().map(|e| e.id).collect();
        assert_eq!(
            recovered, baseline,
            "a corrupt cache yields the cold-scan catalog"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_preserves_display_names_across_runs() {
        let dir = scratch("preserve");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let (model_id, _path) = bake_fixture(&assets, "/tmp/flat.glb");
        assets.scan_assets().expect("first scan");

        // Rename the model row; a rescan keeps the display name (the filesystem refreshes only
        // the path, not the human name).
        assert!(assets.catalog.rename(model_id, "Renamed Model"));
        assets.scan_assets().expect("rescan");
        assert_eq!(assets.catalog.find(model_id).unwrap().name, "Renamed Model");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn foreign_file_gets_a_minted_smeta_and_a_texture_row() {
        let dir = scratch("foreign");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        // A foreign PNG with a non-uuid name dropped into textures/.
        let png = png_2x2();
        let rel = "textures/brick_albedo.png";
        std::fs::write(format!("{}/{rel}", root.display()), &png).unwrap();

        let delta = assets.scan_assets().expect("scan");
        assert_eq!(delta.added.len(), 1, "the foreign file is catalogued");
        // A `.smeta` was minted beside it.
        assert!(std::path::Path::new(&format!("{}/{rel}.smeta", root.display())).exists());
        let row = &delta.added[0];
        assert_eq!(row.asset_type, AssetType::Texture);
        assert_eq!(row.name, "brick_albedo");

        // A rescan reuses the minted `.smeta` id (no second mint, the id is stable).
        let id = row.id;
        let mut again = AssetServer::new(&root);
        again.scan_assets().expect("rescan");
        assert!(
            again.catalog.find(id).is_some(),
            "the minted id is stable across scans"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn native_uuid_texture_recovers_name_and_colorspace_from_its_smeta() {
        let dir = scratch("nativesmeta");
        let root = dir.join("assets");
        std::fs::create_dir_all(root.join("textures")).unwrap();

        // A uuid-named native texture on disk (as an import writes it), with no prior catalog row.
        let id = Uuid::new();
        let rel = format!("textures/{}.png", id.value());
        std::fs::write(format!("{}/{rel}", root.display()), png_2x2()).unwrap();

        // Seed a row + write its durable sidecar (a linear data map), as an import does, then drop
        // that server so the next scan starts cold (empty `previous`, no cache).
        {
            let mut seed = AssetServer::new(&root);
            seed.catalog.put(AssetEntry {
                id,
                name: "Brick Normal".to_owned(),
                asset_type: AssetType::Texture,
                path: rel.clone(),
                linear: true,
                ..AssetEntry::default()
            });
            seed.write_asset_sidecar(id).expect("write sidecar");
        }
        assert!(std::path::Path::new(&format!("{}/{rel}.smeta", root.display())).exists());

        // The cold scan recovers name + colorspace from the sidecar, not the uuid stem / defaults.
        let mut cold = AssetServer::new(&root);
        cold.scan_assets().expect("cold scan");
        let row = cold.catalog.find(id).expect("row");
        assert_eq!(
            row.name, "Brick Normal",
            "the .smeta name beats the uuid stem"
        );
        assert_eq!(
            row.colorspace,
            Colorspace::Linear,
            "linear colorspace survives (no silent revert to sRGB)"
        );
        assert!(row.linear, "the linear flag survives");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_id_smeta_does_not_bleed_onto_a_path_sharing_row() {
        let dir = scratch("smetaguard");
        let root = dir.join("assets");
        std::fs::create_dir_all(root.join("textures")).unwrap();

        // A native texture whose sibling `.smeta` names a *different* id (as a model sidecar sitting
        // beside an embedded sub-asset would): the id guard must reject it, keeping the uuid-stem name.
        let id = Uuid::new();
        let rel = format!("textures/{}.png", id.value());
        std::fs::write(format!("{}/{rel}", root.display()), png_2x2()).unwrap();
        {
            let other = Uuid::new();
            let mut seed = AssetServer::new(&root);
            seed.catalog.put(AssetEntry {
                id: other,
                name: "Someone Else".to_owned(),
                asset_type: AssetType::Texture,
                path: rel.clone(),
                ..AssetEntry::default()
            });
            seed.write_asset_sidecar(other).expect("write sidecar");
        }

        let mut cold = AssetServer::new(&root);
        cold.scan_assets().expect("cold scan");
        let row = cold.catalog.find(id).expect("row");
        assert_eq!(
            row.name,
            id.value().to_string(),
            "an id-mismatched sidecar is ignored; the row keeps its stem name"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cold_scan_recovers_renamed_model_and_extracted_subasset_names_from_sidecars() {
        // A never-saved rename survives a cold scan because the name lives in a co-located `.smeta`
        // sidecar, not only in project.json — across a `.smodel` model row and an extracted sub-asset.
        let dir = scratch("durablenames");
        let root = dir.join("assets");
        let mut assets = AssetServer::new(&root);
        let (model_id, _path) = bake_fixture(&assets, "/tmp/flat.glb");
        assets.scan_assets().expect("scan");

        let material_sub = assets
            .catalog
            .entries
            .iter()
            .find(|e| e.asset_type == AssetType::Material && e.container == model_id)
            .map(|e| e.id)
            .expect("embedded material sub-asset");

        // Rename the model row and write its durable sidecar (as a rename command does).
        assert!(assets.catalog.rename(model_id, "Hero"));
        assets.write_asset_sidecar(model_id).expect("model sidecar");

        // Extract the embedded material to a standalone `.smat`, rename it, and write its sidecar.
        let extracted = crate::manage::extract_sub_asset(&mut assets, model_id, material_sub, "")
            .expect("extract");
        assert!(assets.catalog.rename(extracted, "Brass"));
        assets
            .write_asset_sidecar(extracted)
            .expect("sub-asset sidecar");

        // A cold scan (fresh server, no cache, empty previous) recovers both names from the
        // sidecars alone.
        let mut cold = AssetServer::new(&root);
        cold.scan_assets().expect("cold scan");
        assert_eq!(cold.catalog.find(model_id).expect("model row").name, "Hero");
        assert_eq!(
            cold.catalog.find(extracted).expect("extracted row").name,
            "Brass"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
