//! The shared catalog write path every authored vegetation asset kind goes through.

use std::io::Write;
use std::path::Path;

use atomic_write_file::AtomicWriteFile;
use saffron_core::Uuid;
use saffron_scene::{AssetEntry, AssetType};

use crate::cook_reader::CookAssetAccess;
use crate::import::hash_bytes_fnv;
use crate::{AssetServer, Error, Result};

pub(super) fn read_typed_asset_from(
    assets: &dyn CookAssetAccess,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
) -> Result<Vec<u8>> {
    let entry = typed_entry_from(assets, id, asset_type, wanted)?;
    assets.read_file(&assets.root().join(&entry.path))
}

pub(super) fn typed_entry_from<'a>(
    assets: &'a dyn CookAssetAccess,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
) -> Result<&'a AssetEntry> {
    let entry = assets
        .catalog()
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?;
    if entry.asset_type != asset_type {
        return Err(Error::WrongAssetType {
            id: id.value(),
            wanted,
        });
    }
    Ok(entry)
}

pub(super) fn save_typed_asset(
    assets: &mut AssetServer,
    id: Uuid,
    name: &str,
    folder: &str,
    asset_type: AssetType,
    relative_path: String,
    bytes: &[u8],
) -> Result<Uuid> {
    assets.ensure_asset_directories();
    atomic_write(&assets.root.join(&relative_path), bytes)?;
    let unique_name = assets.catalog().unique_name(name);
    assets.register_imported_asset(AssetEntry {
        id,
        name: unique_name,
        asset_type,
        path: relative_path.clone(),
        folder: folder.to_owned(),
        content_hash: hash_bytes_fnv(bytes),
        ..AssetEntry::default()
    });
    if let Err(error) = assets.write_asset_sidecar(id) {
        let _ = assets.delete_asset_entry(id);
        let _ = std::fs::remove_file(assets.root.join(&relative_path));
        return Err(error);
    }
    Ok(id)
}

pub(super) fn import_typed_asset(
    assets: &mut AssetServer,
    id: Uuid,
    name: &str,
    folder: &str,
    asset_type: AssetType,
    relative_path: String,
    bytes: &[u8],
) -> Result<Uuid> {
    if id.value() < 1024 {
        return Err(Error::Io(
            "authored vegetation asset identity is in the reserved range".to_owned(),
        ));
    }
    if assets.catalog().find(id).is_some() || assets.root.join(&relative_path).exists() {
        return Err(Error::Io(format!(
            "vegetation asset identity {} already exists in the project",
            id.value()
        )));
    }
    save_typed_asset(assets, id, name, folder, asset_type, relative_path, bytes)
}

pub(super) fn update_typed_asset(
    assets: &mut AssetServer,
    id: Uuid,
    asset_type: AssetType,
    wanted: &'static str,
    bytes: &[u8],
) -> Result<()> {
    let path = typed_entry_from(assets, id, asset_type, wanted)?
        .path
        .clone();
    atomic_write(&assets.root.join(path), bytes)?;
    let content_hash = hash_bytes_fnv(bytes);
    let updated = assets.update_asset_content_hash(id, content_hash);
    debug_assert!(updated, "validated vegetation asset remains catalogued");
    Ok(())
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = AtomicWriteFile::options()
        .open(path)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| Error::Io(error.to_string()))?;
    file.commit().map_err(|error| Error::Io(error.to_string()))
}
