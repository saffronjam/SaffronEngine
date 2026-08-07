//! The cold catalog walk: rebuild every row from disk, then overlay each row's durable sidecar.

use saffron_core::Uuid;
use saffron_json::{Value, json_string_or, parse_json};
use saffron_scene::{AssetCatalog, AssetEntry, AssetType, Colorspace, TextureRole};
use walkdir::WalkDir;

use crate::catalog::{catalog_folders_from_json, catalog_from_json};
use crate::import::{ScanDelta, catalog_rows_for_container};
use crate::model::read_container_metadata;
use crate::names::{colorspace_name, texture_role_name};

use super::roles::{colorspace_for_role_explicit, infer_texture_role};
use super::sidecar::{SmetaData, read_smeta, write_smeta};
use super::{asset_signature, catalog_cache_path_for, write_catalog_cache_to};

/// The content hash for a standalone thumbnail-bearing file (mesh/texture), the
/// content-addressed thumbnail cache key. `0` for kinds keyed differently (materials key on
/// resolved state, animations have no thumbnail) or an unreadable file. Read once here on a
/// cold scan — which runs exactly when a file changed — so an in-place edit reflows the key.
fn standalone_content_hash(asset_type: AssetType, path: &str) -> u64 {
    if asset_type == AssetType::VegetationMap {
        return crate::vegetation::vegetation_map_content_hash_path(std::path::Path::new(path))
            .unwrap_or(0);
    }
    if !matches!(
        asset_type,
        AssetType::Mesh
            | AssetType::Texture
            | AssetType::Plant
            | AssetType::Biome
            | AssetType::VegetationMap
    ) {
        return 0;
    }
    match std::fs::read(path) {
        Ok(bytes) => crate::import::hash_bytes_fnv(&bytes),
        Err(_) => 0,
    }
}

/// Whether `name` parses as a pure decimal uuid stem (an engine-written standalone file).
fn parse_uuid_stem(stem: &str) -> Option<u64> {
    if stem.is_empty() {
        return None;
    }
    stem.parse::<u64>().ok()
}

/// The lowercased extension (without the dot) of a path's filename.
fn lower_ext(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Lists every regular file under `root`, skipping the `.cache/` subtree, sorted by
/// relative path so the scan order (and thus the catalog cache) is deterministic.
fn sorted_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| e.file_name() != ".cache")
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .collect();
    files.sort();
    files
}

/// The cold catalog scan core: walks `root`, rebuilds the catalog from disk, and diffs it
/// against `previous`. Free of any `AssetServer` borrow (every input is owned/borrowed) so the
/// off-thread project loader can run it. `progress(done, total, current)` fires per walked file,
/// driving the loader's determinate `Catalog` stage.
pub fn reconcile_catalog_from_disk(
    root: &std::path::Path,
    previous: &AssetCatalog,
    progress: &mut dyn FnMut(u32, u32, &str),
) -> (AssetCatalog, ScanDelta) {
    let mut delta = ScanDelta::default();
    if root.as_os_str().is_empty() || !root.exists() {
        return (previous.clone(), delta);
    }
    let root = root.to_path_buf();
    let previous = previous.clone();
    let files = sorted_files(&root);
    let total = files.len() as u32;
    let mut rebuilt = AssetCatalog {
        folders: previous.folders.clone(),
        ..AssetCatalog::default()
    };

    for (index, path) in files.into_iter().enumerate() {
        let rel = match path.strip_prefix(&root) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        progress(index as u32, total, &rel);
        let ext = lower_ext(&path);
        let path_str = path.to_string_lossy().to_string();

        // A container file: a `.smodel` model or a `.smatx` texture-embedding material. The
        // parent row's type follows the extension; both read the same META + TOC.
        if ext == "smodel" || ext == "smatx" {
            let parent_type = if ext == "smatx" {
                AssetType::Material
            } else {
                AssetType::Model
            };
            match read_container_metadata(&path) {
                Ok(meta) => {
                    for mut row in catalog_rows_for_container(&meta, &rel, parent_type) {
                        preserve_name_folder(&previous, &mut row);
                        rebuilt.put(row);
                    }
                }
                Err(err) => {
                    tracing::warn!("scan: skipping '{rel}': {err}");
                }
            }
            continue;
        }

        let (asset_type, hdr) = match ext.as_str() {
            "smesh" => (AssetType::Mesh, false),
            "smat" => (AssetType::Material, false),
            "sanim" => (AssetType::Animation, false),
            "senv" => (AssetType::Environment, false),
            "splant" => (AssetType::Plant, false),
            "sbiome" => (AssetType::Biome, false),
            "svegmap" => (AssetType::VegetationMap, false),
            "png" | "jpg" | "jpeg" | "tga" | "bmp" => (AssetType::Texture, false),
            "hdr" => (AssetType::Texture, true),
            _ => continue,
        };

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if let Some(id) = parse_uuid_stem(stem) {
            // Engine-written standalone file (uuid name). A known one keeps its row's
            // name/folder/duration/colorspace (not recoverable from the filename) — its
            // path and content hash refresh. A genuinely new one infers type/hdr from the
            // extension.
            if let Some(prev) = previous.find(Uuid(id)) {
                let mut row = prev.clone();
                row.path = rel;
                row.container = Uuid(0);
                row.chunk = -1;
                row.content_hash = standalone_content_hash(row.asset_type, &path_str);
                rebuilt.put(row);
            } else {
                rebuilt.put(AssetEntry {
                    id: Uuid(id),
                    name: stem.to_owned(),
                    asset_type,
                    path: rel,
                    hdr,
                    content_hash: standalone_content_hash(asset_type, &path_str),
                    ..AssetEntry::default()
                });
            }
            continue;
        }

        // A foreign / headerless file: identity + colorspace come from a sibling
        // `.smeta`, minted + written on first sight (a wrong-colorspace guess is warned).
        let smeta_path = format!("{path_str}.smeta");
        let mut sidecar = None;
        if std::path::Path::new(&smeta_path).exists() {
            match read_smeta(&smeta_path) {
                Ok(loaded) => sidecar = Some(loaded),
                Err(err) => {
                    tracing::warn!("scan: ignoring bad .smeta '{rel}.smeta': {err}");
                }
            }
        }
        let sidecar = match sidecar {
            Some(sidecar) => sidecar,
            None => {
                // Infer the role from the filename so a data map (normal/roughness/AO) drops in
                // linear; a truly unrecognized image keeps the safe sRGB default, with a warn.
                let role = if asset_type == AssetType::Texture {
                    infer_texture_role(stem, hdr)
                } else {
                    TextureRole::Unknown
                };
                let colorspace = match role {
                    TextureRole::Unknown => {
                        if hdr {
                            Colorspace::Hdr
                        } else {
                            Colorspace::Srgb
                        }
                    }
                    known => colorspace_for_role_explicit(known),
                };
                let minted = SmetaData {
                    id: Uuid::new(),
                    asset_type,
                    colorspace,
                    role,
                    folder: String::new(),
                    name: stem.to_owned(),
                };
                if let Err(err) = write_smeta(&smeta_path, &minted) {
                    tracing::warn!("scan: could not write '{rel}.smeta': {err}");
                }
                tracing::warn!(
                    "scan: minted .smeta for foreign file '{rel}' (colorspace {}, role {} — verify it for data maps like normals)",
                    colorspace_name(colorspace),
                    texture_role_name(role)
                );
                minted
            }
        };

        let mut row = AssetEntry {
            id: sidecar.id,
            name: if sidecar.name.is_empty() {
                stem.to_owned()
            } else {
                sidecar.name.clone()
            },
            asset_type: sidecar.asset_type,
            path: rel,
            folder: sidecar.folder.clone(),
            colorspace: sidecar.colorspace,
            role: sidecar.role,
            hdr: sidecar.colorspace == Colorspace::Hdr,
            linear: sidecar.colorspace == Colorspace::Linear,
            content_hash: standalone_content_hash(sidecar.asset_type, &path_str),
            ..AssetEntry::default()
        };
        preserve_name_folder(&previous, &mut row);
        rebuilt.put(row);
    }

    apply_sidecar_overrides(&root, &mut rebuilt);

    for (&id, &index) in &rebuilt.by_id {
        if !previous.by_id.contains_key(&id) {
            delta.added.push(rebuilt.entries[index].clone());
        }
    }
    for &id in previous.by_id.keys() {
        if !rebuilt.by_id.contains_key(&id) {
            delta.removed.push(Uuid(id));
        }
    }
    (rebuilt, delta)
}

/// The catalog-cache-fast reconcile: reuse a signature-matching `assets/.cache/catalog.json`
/// verbatim (applied onto `seed`), else a full [`reconcile_catalog_from_disk`] + cache refresh.
/// Free of any `AssetServer` borrow so the off-thread loader can run it; `progress` drives the
/// `Catalog` stage on the cold-scan path.
pub fn resolve_catalog_from_disk(
    root: &std::path::Path,
    seed: &AssetCatalog,
    progress: &mut dyn FnMut(u32, u32, &str),
) -> (AssetCatalog, ScanDelta) {
    let cache_path = catalog_cache_path_for(root);
    if cache_path.exists()
        && let Ok(text) = std::fs::read_to_string(&cache_path)
        && let Ok(doc) = parse_json(&text)
        && doc.is_object()
    {
        let cached = json_string_or(&doc, "signature", String::new());
        let live = asset_signature(root).to_string();
        if !cached.is_empty() && cached == live {
            let mut catalog = seed.clone();
            catalog_from_json(
                &mut catalog,
                doc.get("assets").unwrap_or(&Value::Array(Vec::new())),
            );
            catalog_folders_from_json(
                &mut catalog,
                doc.get("assetFolders").unwrap_or(&Value::Array(Vec::new())),
            );
            return (catalog, ScanDelta::default());
        }
    }
    let (rebuilt, delta) = reconcile_catalog_from_disk(root, seed, progress);
    write_catalog_cache_to(root, &rebuilt);
    (rebuilt, delta)
}

/// Restores a row's display name (and non-empty folder) from a prior catalog entry by id.
fn preserve_name_folder(previous: &AssetCatalog, row: &mut AssetEntry) {
    if let Some(prev) = previous.find(row.id) {
        row.name = prev.name.clone();
        if !prev.folder.is_empty() {
            row.folder = prev.folder.clone();
        }
    }
}

/// Overlays each row's durable metadata from a co-located `<path>.smeta` sidecar — the authoritative
/// home for an asset's name / folder / texture metadata. It runs after the walk so the sidecar wins
/// over the stale `project.json` seed, which is what makes a never-saved rename or import survive a
/// cold scan. It is id-guarded: an embedded sub-asset row can share its container's `.smodel` path,
/// so a sidecar applies only to the row whose id it names, never to a path-sharing sibling.
fn apply_sidecar_overrides(root: &std::path::Path, rebuilt: &mut AssetCatalog) {
    for row in &mut rebuilt.entries {
        if row.path.is_empty() {
            continue;
        }
        let smeta_path = format!("{}/{}.smeta", root.display(), row.path);
        if !std::path::Path::new(&smeta_path).exists() {
            continue;
        }
        match read_smeta(&smeta_path) {
            Ok(sidecar) if sidecar.id == row.id => {
                if !sidecar.name.is_empty() {
                    row.name = sidecar.name;
                }
                row.folder = sidecar.folder;
                if row.asset_type == AssetType::Texture && sidecar.colorspace != Colorspace::Auto {
                    row.colorspace = sidecar.colorspace;
                    row.hdr = sidecar.colorspace == Colorspace::Hdr;
                    row.linear = sidecar.colorspace == Colorspace::Linear;
                }
                if row.asset_type == AssetType::Texture {
                    row.role = sidecar.role;
                }
            }
            // A sidecar whose id names a different asset (a path-sharing sibling) is not ours.
            Ok(_) => {}
            Err(err) => tracing::warn!("scan: ignoring bad .smeta '{}.smeta': {err}", row.path),
        }
    }
}
