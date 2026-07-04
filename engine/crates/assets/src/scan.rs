//! Filesystem-as-source-of-truth catalog reconciliation + standalone texture register.
//!
//! [`AssetServer::scan_assets`] walks the asset root (skipping the `.cache/` dir),
//! rebuilds the catalog from disk, and diffs it against the live one: a container
//! contributes a [`AssetType::Model`] parent + sub-asset rows via the prefix read;
//! engine-written standalone files (a uuid filename stem) keep their prior row (only the
//! path is refreshed); a foreign file (a raw `.png` dropped in) identifies via a sibling
//! `.smeta` minted on first sight. A co-located `<path>.smeta` is then overlaid on every
//! row as the durable, id-keyed home for its name / folder / colorspace — authoritative
//! over the (only-as-fresh-as-the-last-save) `project.json` seed, so a never-saved import
//! or rename survives a cold scan. The walk is **sorted** so the catalog cache is
//! reproducible.
//!
//! [`AssetServer::load_catalog`] is the cache-fast path: if `assets/.cache/catalog.json`'s
//! stored signature still matches the on-disk signature, it reuses the cached rows
//! (skipping every `.smodel` prefix read); on any mismatch / missing / corrupt cache it
//! falls back to a full [`AssetServer::scan_assets`]. The cache is **never** load-bearing —
//! a cold scan always yields the identical catalog.
//!
//! [`AssetServer::register_texture_bytes`] / [`AssetServer::register_hdr_texture_bytes`]
//! write the encoded bytes under `textures/<uuid>.<ext>`, add a [`AssetType::Texture`]
//! catalog row, and seed the GPU texture cache so the just-uploaded texture is served
//! without a re-read.

use saffron_core::Uuid;
use saffron_geometry::{decode_image_from_memory, decode_image_from_memory_hdr};
use saffron_json::{Value, dump_json, json_string_or, json_u64_or, parse_json, uuid_to_json};
use saffron_scene::{AssetCatalog, AssetEntry, AssetType, Colorspace};
use walkdir::WalkDir;

use crate::AssetServer;
use crate::catalog::{
    catalog_folders_from_json, catalog_folders_to_json, catalog_from_json, catalog_to_json,
};
use crate::error::{Error, Result};
use crate::gpu::GpuUploader;
use crate::import::{ScanDelta, catalog_rows_for_model};
use crate::model::read_container_metadata;
use crate::names::{asset_type_from_name, asset_type_name, colorspace_from_name, colorspace_name};

/// The `.smeta` sidecar co-located with an asset file — the durable, id-keyed home for its
/// name / folder / colorspace.
///
/// A foreign/headerless file (a raw `.png` dropped into `assets/`) also uses it for a stable
/// id (its bytes carry none). Engine-written files (`.smodel`, `textures/<uuid>.*`, extracted
/// `.smat`/`.smesh`/`.sanim`) take their *identity* from the uuid stem / container, but their
/// *metadata* lives here too — written eagerly on import and on every rename/move so it
/// survives a cold scan without a project save.
#[derive(Clone, Debug)]
struct SmetaData {
    id: Uuid,
    asset_type: AssetType,
    colorspace: Colorspace,
    folder: String,
    name: String,
}

impl Default for SmetaData {
    fn default() -> Self {
        Self {
            id: Uuid(0),
            asset_type: AssetType::Texture,
            colorspace: Colorspace::Auto,
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
fn read_smeta(path: &str) -> Result<SmetaData> {
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
fn write_smeta(path: &str, meta: &SmetaData) -> Result<()> {
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
    if !meta.folder.is_empty() {
        doc.insert("folder".to_owned(), Value::String(meta.folder.clone()));
    }
    if !meta.name.is_empty() {
        doc.insert("name".to_owned(), Value::String(meta.name.clone()));
    }
    std::fs::write(path, dump_json(&Value::Object(doc), 2))
        .map_err(|e| Error::Io(format!("cannot write '{path}': {e}")))
}

/// The content hash for a standalone thumbnail-bearing file (mesh/texture), the
/// content-addressed thumbnail cache key. `0` for kinds keyed differently (materials key on
/// resolved state, animations have no thumbnail) or an unreadable file. Read once here on a
/// cold scan — which runs exactly when a file changed — so an in-place edit reflows the key.
fn standalone_content_hash(asset_type: AssetType, path: &str) -> u64 {
    if !matches!(asset_type, AssetType::Mesh | AssetType::Texture) {
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

impl AssetServer {
    /// Walks `assets/`, rebuilds the catalog from disk, and diffs it against the live one.
    ///
    /// The filesystem is the source of truth: a never-saved import is rediscovered, a
    /// deleted file's row is dropped. Containers contribute rows via [`catalog_rows_for_model`];
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
        self.catalog = rebuilt;
        // The catalog was rebuilt from disk (ids/paths may have changed), so any memoized material
        // resolution keyed by id could be stale.
        self.invalidate_material_caches();
        Ok(delta)
    }
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

        if ext == "smodel" {
            match read_container_metadata(&path) {
                Ok(meta) => {
                    for mut row in catalog_rows_for_model(&meta, &rel) {
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
                let colorspace = if hdr {
                    Colorspace::Hdr
                } else {
                    Colorspace::Srgb
                };
                let minted = SmetaData {
                    id: Uuid::new(),
                    asset_type,
                    colorspace,
                    folder: String::new(),
                    name: stem.to_owned(),
                };
                if let Err(err) = write_smeta(&smeta_path, &minted) {
                    tracing::warn!("scan: could not write '{rel}.smeta': {err}");
                }
                tracing::warn!(
                    "scan: minted .smeta for foreign file '{rel}' (colorspace {} — verify it for data maps like normals)",
                    colorspace_name(colorspace)
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
        self.catalog = catalog;
        Ok(delta)
    }

    /// Persists the catalog + the current asset signature to `assets/.cache/catalog.json`.
    /// Regenerable and gitignored: deleting it is always safe (the next load is a cold scan).
    pub fn write_catalog_cache(&self) {
        write_catalog_cache_to(&self.root, &self.catalog);
    }

    /// Writes the durable `<path>.smeta` sidecar for the catalog row `id` — its name, folder,
    /// and (for a texture) colorspace. Call after any create / rename / move so the metadata
    /// survives a cold scan without a project save; [`apply_sidecar_overrides`] reads it back.
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

    /// Decodes + uploads `encoded` (RGBA8) as a `srgb`/unorm texture, writes the bytes to
    /// `textures/<uuid>.<ext>`, adds a [`AssetType::Texture`] catalog row, and seeds the
    /// GPU texture cache. Returns the minted id.
    ///
    /// # Errors
    ///
    /// [`Error::Geometry`] if the bytes do not decode, [`Error::Render`] if the upload
    /// fails, [`Error::Io`] if the file cannot be written.
    pub fn register_texture_bytes(
        &mut self,
        gpu: &dyn GpuUploader,
        encoded: &[u8],
        ext: &str,
        name: &str,
        srgb: bool,
    ) -> Result<Uuid> {
        let decoded = decode_image_from_memory(encoded)?;
        let texture = gpu.upload_texture(&decoded.rgba, decoded.width, decoded.height, srgb)?;
        let id = Uuid::new();
        let extension = if ext.is_empty() { "png" } else { ext };
        self.ensure_asset_directories();
        let relative_path = format!("textures/{}.{extension}", id.value());
        std::fs::write(format!("{}/{relative_path}", self.root.display()), encoded)
            .map_err(|e| Error::Io(format!("cannot write texture '{relative_path}': {e}")))?;
        self.put_texture_row(
            id,
            name,
            relative_path,
            false,
            !srgb,
            crate::import::hash_bytes_fnv(encoded),
        );
        self.texture_by_uuid.insert(id.value(), Some(texture));
        Ok(id)
    }

    /// Decodes + uploads `encoded` HDR bytes as a linear float texture, writes them to
    /// `textures/<uuid>.hdr`, adds a Texture row with `hdr = true`, and seeds the cache.
    /// Returns the minted id.
    ///
    /// # Errors
    ///
    /// [`Error::Geometry`] if the bytes do not decode, [`Error::Render`] if the upload
    /// fails, [`Error::Io`] if the file cannot be written.
    pub fn register_hdr_texture_bytes(
        &mut self,
        gpu: &dyn GpuUploader,
        encoded: &[u8],
        name: &str,
    ) -> Result<Uuid> {
        let decoded = decode_image_from_memory_hdr(encoded)?;
        let texture = gpu.upload_texture_float(&decoded.rgba, decoded.width, decoded.height)?;
        let id = Uuid::new();
        self.ensure_asset_directories();
        let relative_path = format!("textures/{}.hdr", id.value());
        std::fs::write(format!("{}/{relative_path}", self.root.display()), encoded)
            .map_err(|e| Error::Io(format!("cannot write texture '{relative_path}': {e}")))?;
        self.put_texture_row(
            id,
            name,
            relative_path,
            true,
            false,
            crate::import::hash_bytes_fnv(encoded),
        );
        self.texture_by_uuid.insert(id.value(), Some(texture));
        Ok(id)
    }

    /// Imports an external image file into the asset dir + catalog (name = filename stem),
    /// dispatching `.hdr` to the float register path.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the file cannot be read, plus any
    /// [`Self::register_texture_bytes`] / [`Self::register_hdr_texture_bytes`] error.
    pub fn import_texture(
        &mut self,
        gpu: &dyn GpuUploader,
        path: &str,
        colorspace: Option<Colorspace>,
    ) -> Result<Uuid> {
        let encoded =
            std::fs::read(path).map_err(|e| Error::Io(format!("cannot open '{path}': {e}")))?;
        let file = std::path::Path::new(path);
        let stem = file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        match colorspace {
            // Data maps (normal/roughness/metallic/AO/…) must upload linear, not sRGB.
            Some(Colorspace::Linear) => {
                self.register_texture_bytes(gpu, &encoded, ext, stem, false)
            }
            Some(Colorspace::Srgb) => self.register_texture_bytes(gpu, &encoded, ext, stem, true),
            Some(Colorspace::Hdr) => self.register_hdr_texture_bytes(gpu, &encoded, stem),
            // Auto / unspecified: dispatch `.hdr` to the float path, else sRGB (prior behaviour).
            _ if ext.eq_ignore_ascii_case("hdr") => {
                self.register_hdr_texture_bytes(gpu, &encoded, stem)
            }
            _ => self.register_texture_bytes(gpu, &encoded, ext, stem, true),
        }
    }

    /// Inserts a standalone Texture row with a name uniqued against the live catalog.
    fn put_texture_row(
        &mut self,
        id: Uuid,
        name: &str,
        path: String,
        hdr: bool,
        linear: bool,
        content_hash: u64,
    ) {
        let unique = self.catalog.unique_name(name);
        self.catalog.put(AssetEntry {
            id,
            name: unique,
            asset_type: AssetType::Texture,
            path,
            hdr,
            linear,
            content_hash,
            ..AssetEntry::default()
        });
        if let Err(err) = self.write_asset_sidecar(id) {
            tracing::warn!(
                "import: could not write texture .smeta for {}: {err}",
                id.value()
            );
        }
    }
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

/// Overlays each row's durable metadata from a co-located `<path>.smeta` sidecar — the
/// authoritative home for an asset's name / folder / colorspace. Runs after the walk (and
/// after [`preserve_name_folder`]) so the eagerly-written sidecar wins over the stale
/// `project.json` seed; that is what makes a never-saved rename or import survive a cold scan.
///
/// **Id-guarded.** A `.smodel` sidecar carries only the model row's metadata, and an embedded
/// sub-asset row can share the model's `.smodel` path — so a sidecar is applied only to the row
/// whose id it names, never to a path-sharing sibling. This also makes the extracted-asset
/// case order-independent: the leaf row and the container's remap row share an id, so the
/// overlay lands on whichever won the `put`. The invariant it rests on: every name/folder/
/// colorspace mutation writes the sidecar, so it never lags `project.json`.
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
            }
            // A sidecar whose id names a different asset (a path-sharing sibling) is not ours.
            Ok(_) => {}
            Err(err) => tracing::warn!("scan: ignoring bad .smeta '{}.smeta': {err}", row.path),
        }
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

/// Detects a texture's material-map role from its filename (lowercased substring match).
/// Returns an empty string when no role token is recognized.
#[must_use]
pub fn detect_material_role(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    let has = |token: &str| lower.contains(token);
    if has("arm") || has("orm") || has("_mra") {
        "orm"
    } else if has("albedo")
        || has("basecolor")
        || has("base_color")
        || has("diffuse")
        || has("_diff")
        || has("_col")
        || has("color")
    {
        "albedo"
    } else if has("normal") || has("_nor") || has("nrm") {
        "normal"
    } else if has("rough") {
        "roughness"
    } else if has("metal") {
        "metallic"
    } else if has("emissive") || has("emission") || has("_emit") {
        "emissive"
    } else if has("height") || has("displace") || has("_disp") || has("bump") {
        "height"
    } else if has("occlusion") || has("_ao") || has("ambientocclusion") {
        "ao"
    } else if has("gloss") {
        "gloss"
    } else if has("opacity") || has("alpha") || has("_mask") {
        "opacity"
    } else {
        ""
    }
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
