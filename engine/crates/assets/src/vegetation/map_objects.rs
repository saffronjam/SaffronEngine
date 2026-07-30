//! The `.svegmap` package's immutable object files: content-addressed names, the strict identity
//! check every read performs, and the write lock that orders one transaction against another.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use saffron_core::Uuid;
use saffron_vegetation::{
    VegetationMapAsset, VegetationMapChunk, VegetationMapChunkReference,
    read_vegetation_map_asset as decode_map, read_vegetation_map_chunk, vegetation_content_hash,
    write_vegetation_map_chunk,
};

use super::catalog_io::atomic_write;
use super::map_package::remove_vegetation_map_package;
use crate::cook_reader::CookAssetAccess;
use crate::import::hash_bytes_fnv;
use crate::{AssetServer, Error, Result};

pub(super) fn read_source_map_chunks(
    source: &Path,
    map: &VegetationMapAsset,
) -> Result<Vec<VegetationMapChunk>> {
    let package = map_package_path(source);
    if map.inventory.is_empty() && !package.exists() {
        return Ok(Vec::new());
    }
    if !package.is_dir() {
        return Err(Error::Io(
            "vegetation-map object package is missing or is not a directory".to_owned(),
        ));
    }
    let mut chunks = Vec::with_capacity(map.inventory.len());
    for reference in &map.inventory {
        chunks.push(read_map_object(&package, map.id, reference)?);
    }
    Ok(chunks)
}

pub(super) fn publish_imported_map_objects(
    package: &Path,
    map: &VegetationMapAsset,
    chunks: &[VegetationMapChunk],
) -> Result<()> {
    if chunks.len() != map.inventory.len() {
        return Err(Error::Io(
            "vegetation-map import object count does not match its root inventory".to_owned(),
        ));
    }
    std::fs::create_dir_all(package.join("objects"))
        .map_err(|error| Error::Io(error.to_string()))?;
    for (reference, chunk) in map.inventory.iter().zip(chunks) {
        let bytes = write_vegetation_map_chunk(chunk)?;
        if map_object_reference(chunk, &bytes)? != *reference {
            let _ = std::fs::remove_dir_all(package);
            return Err(Error::Io(
                "vegetation-map import object does not match its root inventory".to_owned(),
            ));
        }
        if let Err(error) = atomic_write(&map_object_path(package, &reference.content_hash), &bytes)
        {
            let _ = std::fs::remove_dir_all(package);
            return Err(error);
        }
    }
    Ok(())
}

pub(super) fn map_object_reference(
    chunk: &VegetationMapChunk,
    bytes: &[u8],
) -> Result<VegetationMapChunkReference> {
    Ok(VegetationMapChunkReference {
        key: chunk.key,
        content_hash: vegetation_content_hash(bytes),
        byte_length: u64::try_from(bytes.len())
            .map_err(|_| Error::Vegetation(saffron_vegetation::Error::NumericOverflow))?,
        revision: chunk.revision,
    })
}

pub(super) fn read_map_object(
    package: &Path,
    map: Uuid,
    reference: &VegetationMapChunkReference,
) -> Result<VegetationMapChunk> {
    let path = map_object_path(package, &reference.content_hash);
    let bytes = std::fs::read(&path).map_err(|error| Error::Io(error.to_string()))?;
    validate_map_object_bytes(map, reference, &bytes)
}

fn validate_map_object_bytes(
    map: Uuid,
    reference: &VegetationMapChunkReference,
    bytes: &[u8],
) -> Result<VegetationMapChunk> {
    if u64::try_from(bytes.len()).ok() != Some(reference.byte_length) {
        return Err(Error::Vegetation(
            saffron_vegetation::Error::ArtifactFormat {
                format: ".svegmap object",
                field: "root.inventory.byteLength".to_owned(),
            },
        ));
    }
    if vegetation_content_hash(bytes) != reference.content_hash {
        return Err(Error::Vegetation(
            saffron_vegetation::Error::ArtifactHashMismatch {
                format: ".svegmap object",
                subject: "root.inventory.contentHash".to_owned(),
            },
        ));
    }
    let chunk = read_vegetation_map_chunk(bytes)?;
    if chunk.map != map || chunk.key != reference.key || chunk.revision != reference.revision {
        return Err(Error::Vegetation(
            saffron_vegetation::Error::ArtifactFormat {
                format: ".svegmap object",
                field: "root.inventory.identity".to_owned(),
            },
        ));
    }
    Ok(chunk)
}

pub(super) fn read_map_object_from(
    assets: &dyn CookAssetAccess,
    package: &Path,
    map: Uuid,
    reference: &VegetationMapChunkReference,
) -> Result<VegetationMapChunk> {
    let path = map_object_path(package, &reference.content_hash);
    let bytes = assets.read_file(&path)?;
    validate_map_object_bytes(map, reference, &bytes)
}

pub(super) fn lock_map_package(package: &Path) -> Result<std::fs::File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(package.join("write.lock"))
        .map_err(|error| Error::Io(error.to_string()))?;
    file.lock().map_err(|error| Error::Io(error.to_string()))?;
    Ok(file)
}

pub(super) fn rollback_map_objects(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let _ = std::fs::remove_file(path);
    }
}

pub(super) fn map_package_directory(map_path: &str) -> String {
    format!("{map_path}.data")
}

pub(super) fn map_package_path(map_path: &Path) -> PathBuf {
    let mut value = map_path.as_os_str().to_os_string();
    value.push(".data");
    PathBuf::from(value)
}

pub(super) fn map_object_path(package: &Path, hash: &[u8; 32]) -> PathBuf {
    let mut name = String::with_capacity(64 + ".svegmapc".len());
    for byte in hash {
        use std::fmt::Write as _;
        write!(&mut name, "{byte:02x}").unwrap();
    }
    name.push_str(".svegmapc");
    package.join("objects").join(name)
}

pub(super) fn rollback_new_asset(assets: &mut AssetServer, id: Uuid) {
    let Some(entry) = assets.catalog().find(id).cloned() else {
        return;
    };
    assets.remove_asset_sidecar(id);
    let _ = remove_vegetation_map_package(assets, &entry);
    if !entry.path.is_empty() {
        let _ = std::fs::remove_file(assets.root.join(&entry.path));
    }
    let _ = assets.delete_asset_entry(id);
}

pub(crate) fn vegetation_map_content_hash_path(path: &Path) -> Result<u64> {
    let root_bytes = std::fs::read(path).map_err(|error| Error::Io(error.to_string()))?;
    let root = decode_map(&root_bytes)?;
    let package = map_package_path(path);
    for reference in &root.inventory {
        read_map_object(&package, root.id, reference)?;
    }
    Ok(hash_bytes_fnv(&root_bytes))
}
