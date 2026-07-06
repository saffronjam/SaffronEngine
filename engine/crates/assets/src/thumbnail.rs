//! Thumbnail resolution: classify an asset into a preview subject, resolve its content-addressed
//! cache key, and either return a cache hit or enqueue a main-graph render.
//!
//! Every tile — material, texture map, mesh, model, HDRI — renders through the **main forward+
//! graph** on the offscreen thumbnail view (the interactive previewer's path). [`request_thumbnail`]
//! runs on the main thread: it resolves `{preview subject, content hash, cache path}` — a stored
//! catalog hash for a mesh/texture/model (self-healing a legacy `0` from the source bytes), a live
//! resolved hash for a material — then returns a cache hit or enqueues a [`PreviewRenderJob`] onto
//! [`AssetServer::preview_render_queue`] and replies `pending`. The host drains that queue in
//! `on_update` (build the preview scene → render → write the disk cache); the editor repolls and
//! hits the written cache. No off-thread rendering: decode + upload happen lazily on the main thread
//! during the render, through the scene's own asset loaders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use saffron_core::Uuid;
use saffron_geometry::ChunkKind;
use saffron_geometry::glam::Mat4;
use saffron_scene::{AssetType, Colorspace, TextureRole};

use crate::material::MaterialAsset;
use crate::{AssetServer, Error, Result};

/// The thumbnail cache version. It prefixes every on-disk cache filename
/// (`v<VERSION>-<contentHash>-<size>.png` under the app-level cache dir), so a render-behaviour
/// change retires the whole cache — every kind, not just materials — by bumping this one number:
/// the new prefix simply never matches the old files (which age out via the size-cap eviction).
/// Bump it whenever the rendered look of a tile changes.
pub const THUMBNAIL_CACHE_VERSION: u32 = 11;

/// The FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 1469598103934665603;
/// The FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 1099511628211;

/// PNG bytes plus the actual encoded pixel dimensions, so a control reply reports the
/// truthful width/height rather than echoing the requested size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailPng {
    /// The encoded PNG bytes.
    pub bytes: Vec<u8>,
    /// The encoded image width.
    pub width: u32,
    /// The encoded image height.
    pub height: u32,
}

/// A thumbnail request's reply: the PNG (a cache hit), or a `pending` flag telling the editor to
/// retry while the enqueued main-graph render produces it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailReply {
    /// The PNG bytes (empty while `pending`).
    pub png: Vec<u8>,
    /// The encoded width (`0` while `pending`).
    pub width: u32,
    /// The encoded height (`0` while `pending`).
    pub height: u32,
    /// The job was enqueued and is not ready — the caller should retry.
    pub pending: bool,
}

/// What the on-disk thumbnail cache holds: entry count + total bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThumbnailCacheStats {
    /// Number of cached thumbnail files.
    pub entries: u32,
    /// Total bytes across the cache files.
    pub bytes: u64,
}

/// One texture source resolved from the catalog at enqueue; its bytes/path fold into the content
/// hash, and (for a standalone texture) its role selects the preview subject.
#[derive(Clone, Debug, Default)]
pub struct ThumbnailTextureSource {
    /// The texture's catalog id.
    pub id: Uuid,
    /// The absolute source file under the asset root (empty when `bytes` is set).
    pub path: String,
    /// An `.hdr` float source: decode float, upload float, tonemap the preview.
    pub hdr: bool,
    /// LDR colorspace: albedo/emissive sRGB, data maps linear.
    pub srgb: bool,
    /// The texture's semantic role: a standalone texture tile renders on the studio sphere in its
    /// role (albedo lit, normal bumped, …); `Hdri` renders as a chrome ball reflecting the
    /// environment; `Unknown`/`Gloss` fall back to the flat swatch. Ignored when this source is one
    /// texture of a material/model (the material decides the slot).
    pub role: TextureRole,
    /// Embedded chunk image bytes (decoded from memory when non-empty).
    pub bytes: Vec<u8>,
}

/// The asset kind a [`ThumbnailJob`] resolves to, carrying the source inputs the content hash folds
/// (a legacy `0`-hash row self-heals from these bytes) plus the texture role the classifier reads.
/// The main-graph render re-resolves everything from the catalog by id, so no render payload is
/// carried here.
#[derive(Clone, Debug)]
pub enum ThumbnailContent {
    /// A texture, hashed from its source bytes; the role selects its preview subject.
    Texture(ThumbnailTextureSource),
    /// A standalone or embedded mesh, hashed from its `.smesh` bytes.
    Mesh {
        /// The standalone `.smesh` path (empty for an embedded mesh).
        path: String,
        /// The embedded `.smesh` chunk image (empty for a standalone mesh).
        bytes: Vec<u8>,
    },
    /// A material, keyed on its live resolved params (via [`thumbnail_material_hash`]) rather than
    /// source bytes — editing a parent reflows every instance.
    Material,
    /// A model, hashed from its merged mesh-chunk bytes + node transforms + per-slot material state.
    Model {
        /// One `.smesh` chunk per mesh-bearing forest node, with its node world transform
        /// (resolved on the main thread at enqueue).
        meshes: Vec<ModelMeshChunk>,
        /// One material per slot, in submesh-slot order (container-wide).
        materials: Vec<MaterialAsset>,
        /// The referenced textures across the model's materials.
        textures: Vec<ThumbnailTextureSource>,
    },
}

/// One mesh chunk of a model thumbnail: a node's `.smesh` image plus the node's world transform,
/// both folded into the model's content hash.
#[derive(Clone, Debug)]
pub struct ModelMeshChunk {
    /// The node's `.smesh` chunk image.
    pub bytes: Vec<u8>,
    /// The node's world transform (composed up its parent chain).
    pub transform: Mat4,
}

/// A resolved `{asset, size}` thumbnail request: the classified content + its content-addressed
/// cache stamp. The content carries the source bytes the hash folds; the main-graph render
/// re-resolves the asset from the catalog by id.
#[derive(Clone, Debug)]
pub struct ThumbnailJob {
    /// The asset id.
    pub id: Uuid,
    /// The requested square pixel size.
    pub size: u32,
    /// The content-addressed cache path (`v<VERSION>-<contentHash>-<size>.png` under the app-level
    /// cache dir); empty when the content hash is unknown (unreadable bytes → uncacheable).
    pub cache_path: String,
    /// The content hash keying `cache_path` (`0` when uncacheable).
    pub content_hash: u64,
    /// The content hash was derived from the gathered bytes because the catalog row carried
    /// none (a legacy row); the caller backfills + persists it.
    pub self_healed: bool,
    /// The type-specific content + inputs.
    pub content: ThumbnailContent,
}

/// A preview-render subject — every asset kind maps to one. Rendered through the main forward+ graph
/// on the offscreen thumbnail view. The host maps this to the control crate's `PreviewSubject` when
/// draining [`AssetServer::preview_render_queue`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewRenderKind {
    /// A material asset, shaded on the dense displacement sphere by its catalog id.
    Material(Uuid),
    /// A texture map, shown in its role through an ephemeral single-slot material.
    TextureRole {
        /// The texture's catalog id.
        tid: Uuid,
        /// The texture's semantic role (albedo lit, normal bumped, height displaced, …).
        role: TextureRole,
    },
    /// A standalone / embedded mesh, shown with the default material.
    Mesh(Uuid),
    /// A model container, shown as its instantiated forest.
    Model(Uuid),
    /// An HDRI, shown as a chrome ball reflecting the equirect (which also backs the tile).
    Hdri(Uuid),
}

/// One queued main-graph preview render: the subject, the square size, and the content-addressed
/// cache path the rendered PNG is written to (and deduped on).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewRenderJob {
    /// What to render.
    pub kind: PreviewRenderKind,
    /// The requested square pixel size.
    pub size: u32,
    /// The content-addressed cache path the rendered PNG is written to.
    pub cache_path: String,
}

/// Classifies a built job's content as the main-graph preview subject that renders it. Every asset
/// kind maps to one — an HDRI to a chrome ball, any other texture role to the sphere in that role,
/// a material to the sphere by id, a mesh/model to itself.
fn preview_render_kind(content: &ThumbnailContent, id: Uuid) -> PreviewRenderKind {
    match content {
        ThumbnailContent::Material => PreviewRenderKind::Material(id),
        ThumbnailContent::Texture(src) if src.role == TextureRole::Hdri => {
            PreviewRenderKind::Hdri(id)
        }
        ThumbnailContent::Texture(src) => PreviewRenderKind::TextureRole {
            tid: id,
            role: src.role,
        },
        ThumbnailContent::Mesh { .. } => PreviewRenderKind::Mesh(id),
        ThumbnailContent::Model { .. } => PreviewRenderKind::Model(id),
    }
}

/// An FNV-1a 64-bit accumulator: the content-hash fold over `u64` words + `f32` bits.
struct FnvHash(u64);

impl FnvHash {
    fn new() -> Self {
        Self(FNV_OFFSET)
    }

    fn mix(&mut self, v: u64) {
        self.0 ^= v;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }

    fn mix_f(&mut self, f: f32) {
        self.mix(u64::from(f.to_bits()));
    }
}

/// A material thumbnail keys on its *resolved* state (a content hash of the resolved
/// params + texture uuids), not a stored catalog hash — editing a parent material reflows
/// every instance without touching the child `.smat`. Folded with the cache version.
fn thumbnail_material_hash(m: &MaterialAsset) -> u64 {
    let mut h = FnvHash::new();
    h.mix(u64::from(THUMBNAIL_CACHE_VERSION));
    h.mix_f(m.base_color.x);
    h.mix_f(m.base_color.y);
    h.mix_f(m.base_color.z);
    h.mix_f(m.base_color.w);
    h.mix_f(m.metallic);
    h.mix_f(m.roughness);
    h.mix_f(m.emissive.x);
    h.mix_f(m.emissive.y);
    h.mix_f(m.emissive.z);
    h.mix_f(m.emissive_strength);
    h.mix_f(m.normal_strength);
    h.mix_f(m.alpha_cutoff);
    h.mix_f(m.height_scale);
    h.mix_f(m.uv_tiling.x);
    h.mix_f(m.uv_tiling.y);
    h.mix_f(m.uv_offset.x);
    h.mix_f(m.uv_offset.y);
    h.mix(m.albedo_texture.value());
    h.mix(m.orm_texture.value());
    h.mix(m.normal_texture.value());
    h.mix(m.emissive_texture.value());
    h.mix(m.height_texture.value());
    h.mix(u64::from(m.unlit));
    h.mix(u64::from(m.double_sided));
    for c in m.shader.bytes() {
        h.mix(u64::from(c));
    }
    for c in m.blend.bytes() {
        h.mix(u64::from(c));
    }
    h.0
}

/// The content hash of a resolved thumbnail job's inputs, mirroring the value scan/bake
/// store on the [`AssetEntry`] — used to self-heal a legacy row (`content_hash == 0`) whose
/// container predates the content-addressed cache. A single mesh/texture folds its chunk (or
/// file) bytes exactly as bake/scan does; a model folds its merged mesh bytes + node
/// transforms + material state. `0` when the bytes are unreadable (the entry stays
/// uncacheable) or for a material (which keys on resolved state, never self-heals).
fn content_hash_from_content(content: &ThumbnailContent) -> u64 {
    match content {
        ThumbnailContent::Texture(src) => {
            if src.bytes.is_empty() {
                std::fs::read(&src.path)
                    .map(|b| crate::import::hash_bytes_fnv(&b))
                    .unwrap_or(0)
            } else {
                crate::import::hash_bytes_fnv(&src.bytes)
            }
        }
        ThumbnailContent::Mesh { path, bytes } => {
            if bytes.is_empty() {
                std::fs::read(path)
                    .map(|b| crate::import::hash_bytes_fnv(&b))
                    .unwrap_or(0)
            } else {
                crate::import::hash_bytes_fnv(bytes)
            }
        }
        ThumbnailContent::Model {
            meshes,
            materials,
            textures,
        } => {
            let mut h = FnvHash::new();
            h.mix(u64::from(THUMBNAIL_CACHE_VERSION));
            for chunk in meshes {
                h.mix(crate::import::hash_bytes_fnv(&chunk.bytes));
                for f in chunk.transform.to_cols_array() {
                    h.mix_f(f);
                }
            }
            for mat in materials {
                h.mix(thumbnail_material_hash(mat));
            }
            for t in textures {
                if !t.bytes.is_empty() {
                    h.mix(crate::import::hash_bytes_fnv(&t.bytes));
                }
            }
            h.0
        }
        ThumbnailContent::Material => 0,
    }
}

/// A cached thumbnail's bytes + the dimensions read from its PNG header (so a hit reports
/// truthful width/height without a decode). `None` if absent or not a readable PNG.
fn read_thumbnail_cache(path: &Path) -> Option<ThumbnailPng> {
    let bytes = std::fs::read(path).ok()?;
    // 8-byte signature + IHDR length/type + the width/height fields.
    if bytes.len() < 24 {
        return None;
    }
    const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes[..8] != PNG_SIG {
        return None;
    }
    let be32 = |at: usize| -> u32 {
        u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    };
    let width = be32(16); // IHDR width
    let height = be32(20); // IHDR height
    Some(ThumbnailPng {
        bytes,
        width,
        height,
    })
}

/// The app-level cache is bounded to this many bytes; a write that pushes it over the cap
/// evicts the oldest files (by mtime) down to [`THUMBNAIL_CACHE_EVICT_BYTES`].
const THUMBNAIL_CACHE_MAX_BYTES: u64 = 1 << 30; // 1 GiB
/// The low-water mark eviction drains down to, so a burst of writes does not re-trigger a
/// full eviction on every file.
const THUMBNAIL_CACHE_EVICT_BYTES: u64 = THUMBNAIL_CACHE_MAX_BYTES / 5 * 4; // 80%

/// Writes a generated PNG into the cache dir, creating the parent dir, then bounds the shared cache
/// with a size-cap eviction.
///
/// # Errors
///
/// [`Error::Io`] if the parent dir cannot be created or the file cannot be written.
pub fn write_thumbnail_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    std::fs::write(path, bytes).map_err(|e| {
        Error::Io(format!(
            "write failed for thumbnail cache '{}': {e}",
            path.display()
        ))
    })?;
    if let Some(parent) = path.parent() {
        evict_thumbnail_cache_over_cap(parent);
    }
    Ok(())
}

/// Bounds the shared content-addressed cache to the production cap.
fn evict_thumbnail_cache_over_cap(dir: &Path) {
    evict_thumbnail_cache(dir, THUMBNAIL_CACHE_MAX_BYTES, THUMBNAIL_CACHE_EVICT_BYTES);
}

/// When `dir`'s total size exceeds `max`, deletes the oldest files (by mtime) until it is
/// back under `target`. A no-op while under `max` (the common case).
fn evict_thumbnail_cache(dir: &Path, max: u64, target: u64) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
    let mut total = 0u64;
    for entry in read.flatten() {
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            total += meta.len();
            files.push((entry.path(), mtime, meta.len()));
        }
    }
    if total <= max {
        return;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
    for (file, _, size) in files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(&file).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

impl AssetServer {
    /// Whether any main-graph preview render is queued. Drives the host's render-activity reason so
    /// the loop keeps full cadence until the queue drains.
    #[must_use]
    pub fn preview_render_pending(&self) -> bool {
        !self.preview_render_queue.is_empty()
    }

    /// Pops up to `max` queued preview-render jobs for the host to render this tick (a small budget
    /// offsets the K-frame converge cost of each tile).
    pub fn take_preview_render_jobs(&mut self, max: usize) -> Vec<PreviewRenderJob> {
        let n = max.min(self.preview_render_queue.len());
        self.preview_render_queue.drain(..n).collect()
    }

    /// Clears a job's in-flight marker after the host renders (or fails to render) it, so a later
    /// re-request re-enqueues if the cache miss recurs (e.g. after an eviction).
    pub fn finish_preview_render(&mut self, cache_path: &str) {
        self.preview_render_in_flight.remove(cache_path);
    }

    /// The cache path for a content hash + size (`v<VERSION>-<contentHash>-<size>.png` under the
    /// app-level thumbnail cache dir). The [`THUMBNAIL_CACHE_VERSION`] prefix makes a version bump
    /// retire every kind's tiles (mesh/texture/model key on the stored `content_hash`, which carries
    /// no version of its own), so the one constant is authoritative for the whole cache.
    fn thumbnail_content_cache_path(&self, content_hash: u64, size: u32) -> PathBuf {
        self.thumbnail_cache_dir().join(format!(
            "v{THUMBNAIL_CACHE_VERSION}-{content_hash}-{size}.png"
        ))
    }

    /// What the on-disk thumbnail cache holds (count + bytes).
    #[must_use]
    pub fn thumbnail_cache_stats(&self) -> ThumbnailCacheStats {
        let mut stats = ThumbnailCacheStats::default();
        let dir = self.thumbnail_cache_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return stats;
        };
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                stats.entries += 1;
                stats.bytes += meta.len();
            }
        }
        stats
    }

    /// Empties the app-level cache dir, returning what was removed.
    pub fn clear_thumbnail_cache_dir(&self) -> ThumbnailCacheStats {
        let removed = self.thumbnail_cache_stats();
        let dir = self.thumbnail_cache_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        removed
    }
}

/// Builds the resolved [`ThumbnailJob`] for `{id, size}` from the catalog/material/container state,
/// plus its cache stamp. Returns the job alone — the caller decides cache-hit vs. enqueue. The
/// catalog/material resolution half of the thumbnail request (a material's live hash, or a
/// legacy-`0` mesh/model/texture row self-healed from its source bytes).
///
/// # Errors
///
/// [`Error::NotInCatalog`] for a missing id, [`Error::Thumbnail`] for an asset with no
/// thumbnail or an unloadable container/mesh chunk.
fn build_thumbnail_job(assets: &mut AssetServer, id: Uuid, size: u32) -> Result<ThumbnailJob> {
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?
        .clone();

    // Materials key on their resolved state (a live hash); every other kind keys on the
    // stored catalog `content_hash`, so the arm returns `Some(hash)` only for a material.
    let (content, material_hash): (ThumbnailContent, Option<u64>) = match entry.asset_type {
        AssetType::Material => {
            // The material only needs its live resolved hash; the main-graph render re-resolves the
            // material + its textures from the catalog by id, so no payload is gathered here.
            let material = crate::material::load_catalog_material_asset(assets, id)?;
            (
                ThumbnailContent::Material,
                Some(thumbnail_material_hash(&material)),
            )
        }
        // An embedded texture sub-asset lives inside its `.smodel`; read the chunk bytes
        // from the container instead of decoding the container path as an image.
        AssetType::Texture if entry.container.value() != 0 => {
            let container = assets.load_model_asset(entry.container).ok_or_else(|| {
                Error::Thumbnail(format!("model {} is not loadable", entry.container.value()))
            })?;
            let tsrc = assets.chunk_source_for(&container, ChunkKind::Texture, id);
            if tsrc.is_empty() {
                return Err(Error::Thumbnail(format!(
                    "no texture sub-asset {}",
                    id.value()
                )));
            }
            let bytes = tsrc.read().map_err(|e| Error::Thumbnail(e.to_string()))?;
            let space = container
                .reader
                .find(ChunkKind::Texture, id.value())
                .map_or(Colorspace::Srgb, |toc| colorspace_from_flags(toc.flags));
            let src = ThumbnailTextureSource {
                id,
                path: String::new(),
                hdr: space == Colorspace::Hdr,
                srgb: space != Colorspace::Linear && space != Colorspace::Hdr,
                role: entry.role,
                bytes,
            };
            (ThumbnailContent::Texture(src), None)
        }
        AssetType::Texture => {
            let space = if entry.colorspace != Colorspace::Auto {
                entry.colorspace
            } else if entry.hdr {
                Colorspace::Hdr
            } else if entry.linear {
                Colorspace::Linear
            } else {
                Colorspace::Srgb
            };
            let src = ThumbnailTextureSource {
                id,
                path: format!("{}/{}", assets.root.display(), entry.path),
                hdr: space == Colorspace::Hdr,
                srgb: space != Colorspace::Linear && space != Colorspace::Hdr,
                role: entry.role,
                bytes: Vec::new(),
            };
            (ThumbnailContent::Texture(src), None)
        }
        AssetType::Mesh if entry.container.value() == 0 => {
            let path = format!("{}/{}", assets.root.display(), entry.path);
            (
                ThumbnailContent::Mesh {
                    path,
                    bytes: Vec::new(),
                },
                None,
            )
        }
        AssetType::Mesh | AssetType::Model => (build_embedded_job(assets, id, &entry)?, None),
        _ => {
            return Err(Error::Thumbnail(format!(
                "asset {} has no thumbnail",
                id.value()
            )));
        }
    };

    // Resolve the content-addressed key: a material's live hash, else the stored catalog
    // hash, self-healing a legacy `0` from the gathered bytes (flagged so the caller persists
    // it). A `0` hash is uncacheable (unreadable bytes) — generated but not cached.
    let (content_hash, self_healed) = match material_hash {
        Some(hash) => (hash, false),
        None if entry.content_hash != 0 => (entry.content_hash, false),
        None => (content_hash_from_content(&content), true),
    };
    let cache_path = if content_hash == 0 {
        String::new()
    } else {
        assets
            .thumbnail_content_cache_path(content_hash, size)
            .display()
            .to_string()
    };

    Ok(ThumbnailJob {
        id,
        size,
        cache_path,
        content_hash,
        self_healed,
        content,
    })
}

/// Resolves an embedded mesh or a model's preview job: slice the primary mesh chunk, and for a
/// model resolve each material slot + its referenced textures — the inputs the content hash folds.
fn build_embedded_job(
    assets: &mut AssetServer,
    id: Uuid,
    entry: &saffron_scene::AssetEntry,
) -> Result<ThumbnailContent> {
    let is_model = entry.asset_type == AssetType::Model;

    // An embedded mesh sub-asset previews that one chunk; a model previews its whole forest.
    if !is_model {
        let container = assets.load_model_asset(entry.container).ok_or_else(|| {
            Error::Thumbnail(format!("model {} is not loadable", entry.container.value()))
        })?;
        let source = assets.chunk_source_for(&container, ChunkKind::Mesh, id);
        if source.is_empty() {
            return Err(Error::Thumbnail(format!(
                "no mesh chunk for sub-asset {}",
                id.value()
            )));
        }
        return Ok(ThumbnailContent::Mesh {
            path: String::new(),
            bytes: source.read()?,
        });
    }

    let container = assets
        .load_model_asset(id)
        .ok_or_else(|| Error::Thumbnail(format!("model {} is not loadable", id.value())))?;
    // Every mesh sub-asset (one per mesh-bearing node), each at its node world transform, so the
    // thumbnail assembles the forest rather than rendering a single node. The transform comes from
    // the node whose `mesh` references the sub-asset; a model with no node table renders its
    // chunks at the identity (correct for the single-node case).
    let nodes = crate::spawn::imported_nodes_from_json(&container.meta.nodes);
    let node_mesh_ids = crate::spawn::node_mesh_ids_from_json(&container.meta.nodes);
    let world = node_world_transforms(&nodes);
    let mut transform_by_mesh: std::collections::HashMap<u64, Mat4> =
        std::collections::HashMap::new();
    for (i, mesh_id) in node_mesh_ids.iter().enumerate() {
        if mesh_id.value() != 0 {
            transform_by_mesh
                .entry(mesh_id.value())
                .or_insert_with(|| world.get(i).copied().unwrap_or(Mat4::IDENTITY));
        }
    }
    let mesh_subs: Vec<Uuid> = container
        .meta
        .sub_assets
        .iter()
        .filter(|s| s.asset_type == AssetType::Mesh)
        .map(|s| s.sub_id)
        .collect();
    let mut meshes = Vec::new();
    for sub_id in mesh_subs {
        let source = assets.chunk_source_for(&container, ChunkKind::Mesh, sub_id);
        if source.is_empty() {
            continue; // a degenerate / missing node chunk drops out, not the whole model
        }
        meshes.push(ModelMeshChunk {
            bytes: source.read()?,
            transform: transform_by_mesh
                .get(&sub_id.value())
                .copied()
                .unwrap_or(Mat4::IDENTITY),
        });
    }
    if meshes.is_empty() {
        return Err(Error::Thumbnail(format!(
            "model {} has no mesh to preview",
            id.value()
        )));
    }

    // Textured model preview: resolve each material slot (sub-asset order matches the submesh
    // material slot) and gather each referenced texture's bytes for the content hash.
    let sub_assets = container.meta.sub_assets.clone();
    let mut materials = Vec::new();
    let mut textures = Vec::new();
    let mut added = HashSet::new();
    for sub in &sub_assets {
        if sub.asset_type != AssetType::Material {
            continue;
        }
        let material = match crate::material::load_catalog_material_asset(assets, sub.sub_id) {
            Ok(material) => material,
            Err(err) => {
                tracing::warn!(
                    "model {}: material {} unresolved: {err}",
                    id.value(),
                    sub.sub_id.value()
                );
                crate::material::default_material_asset()
            }
        };
        for tid in material_texture_ids(&material) {
            add_model_texture(assets, &container, tid, &mut added, &mut textures);
        }
        materials.push(material);
    }

    Ok(ThumbnailContent::Model {
        meshes,
        materials,
        textures,
    })
}

/// World transforms for an imported node forest: each node's local `T·R·S` composed up its
/// parent chain. Parallel to `nodes`.
fn node_world_transforms(nodes: &[saffron_geometry::ImportedNode]) -> Vec<Mat4> {
    let locals: Vec<Mat4> = nodes
        .iter()
        .map(|n| Mat4::from_scale_rotation_translation(n.scale, n.rotation, n.translation))
        .collect();
    (0..nodes.len())
        .map(|i| {
            let mut m = locals[i];
            let mut parent = nodes[i].parent;
            while parent >= 0 && (parent as usize) < nodes.len() {
                m = locals[parent as usize] * m;
                parent = nodes[parent as usize].parent;
            }
            m
        })
        .collect()
}

/// The five texture slot ids of a material, in slot order.
fn material_texture_ids(m: &MaterialAsset) -> [Uuid; 5] {
    [
        m.albedo_texture,
        m.orm_texture,
        m.normal_texture,
        m.emissive_texture,
        m.height_texture,
    ]
}

/// Resolves one of a model's textures into `textures` (dedup via `added`): an embedded
/// chunk ships its bytes + colorspace-from-flags; a standalone texture ships its file path.
fn add_model_texture(
    assets: &AssetServer,
    container: &crate::model::ModelAsset,
    tid: Uuid,
    added: &mut HashSet<u64>,
    textures: &mut Vec<ThumbnailTextureSource>,
) {
    if tid.value() == 0 || added.contains(&tid.value()) {
        return;
    }
    let Some(te) = assets.catalog.find(tid) else {
        return;
    };
    if te.asset_type != AssetType::Texture {
        return;
    }
    let mut src = ThumbnailTextureSource {
        id: tid,
        ..ThumbnailTextureSource::default()
    };
    if te.container.value() != 0 {
        let tsrc = assets.chunk_source_for(container, ChunkKind::Texture, tid);
        if tsrc.is_empty() {
            return;
        }
        let Ok(bytes) = tsrc.read() else {
            return;
        };
        let space = container
            .reader
            .find(ChunkKind::Texture, tid.value())
            .map(|toc| colorspace_from_flags(toc.flags))
            .unwrap_or(Colorspace::Srgb);
        src.bytes = bytes;
        src.hdr = space == Colorspace::Hdr;
        src.srgb = space != Colorspace::Linear && space != Colorspace::Hdr;
    } else {
        src.path = format!("{}/{}", assets.root.display(), te.path);
        src.hdr = te.hdr;
        src.srgb = !te.linear;
    }
    added.insert(tid.value());
    textures.push(src);
}

/// Maps a container texture chunk's flag word to its [`Colorspace`].
fn colorspace_from_flags(flags: u32) -> Colorspace {
    match flags {
        1 => Colorspace::Srgb,
        2 => Colorspace::Linear,
        3 => Colorspace::Hdr,
        _ => Colorspace::Auto,
    }
}

/// A ready [`ThumbnailReply`] from a decoded/generated PNG.
fn ready_reply(png: ThumbnailPng) -> ThumbnailReply {
    ThumbnailReply {
        png: png.bytes,
        width: png.width,
        height: png.height,
        pending: false,
    }
}

/// Resolves `{asset, size}` to a thumbnail — a cache hit returns the PNG; a miss enqueues a
/// main-graph render and replies `pending` (the host drains the queue, the editor repolls).
///
/// The cache is content-addressed: a mesh/texture/model keys on the catalog `content_hash`
/// (checked before any container load), a material on its live resolved state.
///
/// # Errors
///
/// [`Error::NotInCatalog`] for a missing id, [`Error::Thumbnail`] for an asset with no
/// thumbnail / a failed generation / a previously-failed cache key.
pub fn request_thumbnail(assets: &mut AssetServer, id: Uuid, size: u32) -> Result<ThumbnailReply> {
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?
        .clone();

    // Texture / mesh / model carry a stored content hash: resolve the cache path + preview subject
    // WITHOUT loading the container or building a render payload (the boot-hitch fix; it also skips
    // the model-forest slice). A cache hit returns; a miss enqueues the main-graph render. A material
    // keys on its live resolved state, so it falls through to the job build below.
    if matches!(
        entry.asset_type,
        AssetType::Texture | AssetType::Mesh | AssetType::Model
    ) && entry.content_hash != 0
    {
        let cache_path = assets.thumbnail_content_cache_path(entry.content_hash, size);
        if let Some(hit) = read_thumbnail_cache(&cache_path) {
            return Ok(ready_reply(hit));
        }
        let kind = entry_preview_kind(&entry, id);
        return Ok(enqueue_preview_render(
            assets,
            kind,
            size,
            cache_path.display().to_string(),
        ));
    }

    let job = build_thumbnail_job(assets, id, size)?;

    // Self-heal a legacy row: persist the derived hash so later boots take the cheap path
    // above instead of re-loading the container every time.
    if job.self_healed && job.content_hash != 0 {
        assets.catalog.set_content_hash(id, job.content_hash);
        assets.write_catalog_cache();
    }

    // The shared, content-addressed cache may already hold this exact content (rendered for
    // another asset or in another project).
    if !job.cache_path.is_empty()
        && let Some(hit) = read_thumbnail_cache(Path::new(&job.cache_path))
    {
        return Ok(ready_reply(hit));
    }

    if job.cache_path.is_empty() {
        // No content hash to cache / dedup against (unreadable bytes) — settle to the type icon.
        return Err(Error::Thumbnail(format!(
            "asset {} has no cacheable thumbnail content",
            id.value()
        )));
    }
    let kind = preview_render_kind(&job.content, job.id);
    Ok(enqueue_preview_render(assets, kind, size, job.cache_path))
}

/// The preview subject a texture / mesh / model catalog entry renders as — classified from its type
/// and role alone, no payload build.
fn entry_preview_kind(entry: &saffron_scene::AssetEntry, id: Uuid) -> PreviewRenderKind {
    match entry.asset_type {
        AssetType::Mesh => PreviewRenderKind::Mesh(id),
        AssetType::Model => PreviewRenderKind::Model(id),
        AssetType::Texture if entry.role == TextureRole::Hdri => PreviewRenderKind::Hdri(id),
        AssetType::Texture => PreviewRenderKind::TextureRole {
            tid: id,
            role: entry.role,
        },
        _ => unreachable!("only texture/mesh/model reach the stored-hash cheap path"),
    }
}

/// Enqueue a main-graph preview render, deduped on the cache path, and reply `pending`; the editor
/// repolls and hits the written cache.
fn enqueue_preview_render(
    assets: &mut AssetServer,
    kind: PreviewRenderKind,
    size: u32,
    cache_path: String,
) -> ThumbnailReply {
    if !assets.preview_render_in_flight.contains(&cache_path) {
        assets.preview_render_in_flight.insert(cache_path.clone());
        assets.preview_render_queue.push_back(PreviewRenderJob {
            kind,
            size,
            cache_path,
        });
    }
    ThumbnailReply {
        png: Vec::new(),
        width: 0,
        height: 0,
        pending: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{
        ContainerChunk, Mesh, Submesh, Vertex, save_mesh_to_buffer, write_container,
    };

    /// A baked `.smesh` byte image (a single-triangle mesh) used to seed catalog rows.
    fn smesh_bytes() -> Vec<u8> {
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::ZERO,
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::X,
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                    ..Vertex::default()
                },
                Vertex {
                    position: Vec3::Y,
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 1.0),
                    ..Vertex::default()
                },
            ],
            indices: vec![0, 1, 2],
            submeshes: vec![Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            }],
        };
        save_mesh_to_buffer(&mesh, &[], None).unwrap()
    }

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "saffron-thumb-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("project").join("assets")
    }

    /// An asset server whose content-addressed thumbnail cache is isolated to a unique temp
    /// dir. The production cache is app-level (shared), so tests must point it at their own
    /// dir to get a cold miss and exercise the resolve/enqueue path deterministically.
    fn isolated_server(root: &Path) -> AssetServer {
        let mut assets = AssetServer::new(root);
        assets.thumbnail_cache_root = root.parent().unwrap_or(root).join("thumbnail-cache");
        let _ = std::fs::remove_dir_all(&assets.thumbnail_cache_root);
        assets
    }

    fn put_embedded_material_model(
        assets: &mut AssetServer,
        model_id: Uuid,
        material_id: Uuid,
        material: &MaterialAsset,
    ) {
        let mesh_id = Uuid(model_id.value() + 1);
        let rel = format!("models/{}.smodel", model_id.value());
        std::fs::create_dir_all(assets.root.join("models")).expect("models dir");

        let mut meta = crate::model::ContainerMetadata {
            model_id,
            name: "embedded".to_owned(),
            ..crate::model::ContainerMetadata::default()
        };
        meta.sub_assets.push(crate::model::SubAsset {
            sub_id: mesh_id,
            asset_type: AssetType::Mesh,
            name: "mesh".to_owned(),
            chunk: 1,
            ..crate::model::SubAsset::default()
        });
        meta.sub_assets.push(crate::model::SubAsset {
            sub_id: material_id,
            asset_type: AssetType::Material,
            name: "mat".to_owned(),
            chunk: 2,
            ..crate::model::SubAsset::default()
        });

        let meta_bytes = crate::model::encode_container_metadata(&meta);
        let mesh_bytes = smesh_bytes();
        let material_doc = crate::material::material_asset_to_json(material);
        let material_bytes = saffron_json::dump_json(&material_doc, -1).into_bytes();
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: mesh_id.value(),
                flags: 0,
                bytes: &mesh_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Material,
                sub_id: material_id.value(),
                flags: 0,
                bytes: &material_bytes,
            },
        ];
        write_container(assets.root.join(&rel), &chunks).expect("smodel");
        for row in crate::import::catalog_rows_for_container(&meta, &rel, AssetType::Model) {
            assets.catalog.put(row);
        }
    }

    #[test]
    fn model_thumbnail_job_reads_embedded_material_chunks() {
        let root = temp_root("embedded-model-material");
        let mut assets = isolated_server(&root);
        let material = MaterialAsset {
            base_color: saffron_geometry::glam::Vec4::new(0.25, 0.5, 0.75, 1.0),
            metallic: 0.4,
            roughness: 0.2,
            ..MaterialAsset::default()
        };
        put_embedded_material_model(&mut assets, Uuid(12_000), Uuid(12_002), &material);

        let job = build_thumbnail_job(&mut assets, Uuid(12_000), 64).expect("job");
        let ThumbnailContent::Model { materials, .. } = job.content else {
            panic!("model thumbnail content");
        };
        assert_eq!(materials.len(), 1);
        assert_eq!(materials[0].base_color, material.base_color);
        assert_eq!(materials[0].metallic, material.metallic);
        assert_eq!(materials[0].roughness, material.roughness);
    }

    #[test]
    fn material_thumbnail_job_reads_embedded_material_chunks() {
        let root = temp_root("embedded-material");
        let mut assets = isolated_server(&root);
        let material = MaterialAsset {
            base_color: saffron_geometry::glam::Vec4::new(0.8, 0.2, 0.1, 1.0),
            unlit: true,
            ..MaterialAsset::default()
        };
        put_embedded_material_model(&mut assets, Uuid(13_000), Uuid(13_002), &material);

        // The embedded material resolves from its container chunk.
        let loaded = crate::material::load_catalog_material_asset(&mut assets, Uuid(13_002))
            .expect("embedded material resolves");
        assert_eq!(loaded.base_color, material.base_color);
        assert_eq!(loaded.unlit, material.unlit);

        // The job classifies it as a material keyed on that live resolved state.
        let job = build_thumbnail_job(&mut assets, Uuid(13_002), 64).expect("job");
        assert!(matches!(job.content, ThumbnailContent::Material));
        assert_eq!(job.content_hash, thumbnail_material_hash(&loaded));
    }

    /// Every content kind classifies to a main-graph preview subject: material/texture on the sphere,
    /// HDRI to the chrome ball, mesh/model to itself.
    #[test]
    fn preview_render_kind_routes_every_content_to_the_main_graph() {
        assert_eq!(
            preview_render_kind(&ThumbnailContent::Material, Uuid(1)),
            PreviewRenderKind::Material(Uuid(1))
        );

        let texture = |role| {
            ThumbnailContent::Texture(ThumbnailTextureSource {
                role,
                ..ThumbnailTextureSource::default()
            })
        };
        // A non-HDRI role (incl. gloss / unknown) shows in its role on the sphere.
        for role in [
            TextureRole::Normal,
            TextureRole::Gloss,
            TextureRole::Unknown,
        ] {
            assert_eq!(
                preview_render_kind(&texture(role), Uuid(2)),
                PreviewRenderKind::TextureRole { tid: Uuid(2), role }
            );
        }
        // An HDRI is the chrome ball.
        assert_eq!(
            preview_render_kind(&texture(TextureRole::Hdri), Uuid(3)),
            PreviewRenderKind::Hdri(Uuid(3))
        );
        // A mesh and a model render as themselves.
        assert_eq!(
            preview_render_kind(
                &ThumbnailContent::Mesh {
                    path: String::new(),
                    bytes: Vec::new(),
                },
                Uuid(4)
            ),
            PreviewRenderKind::Mesh(Uuid(4))
        );
        assert_eq!(
            preview_render_kind(
                &ThumbnailContent::Model {
                    meshes: Vec::new(),
                    materials: Vec::new(),
                    textures: Vec::new(),
                },
                Uuid(5)
            ),
            PreviewRenderKind::Model(Uuid(5))
        );
    }

    /// The preview-render queue reports pending, drains up to the budget, and dedups on the cache
    /// path — the host-drain contract.
    #[test]
    fn preview_render_queue_drains_and_clears_in_flight() {
        let root = temp_root("preview-queue");
        let mut assets = isolated_server(&root);
        assert!(!assets.preview_render_pending());

        assets
            .preview_render_in_flight
            .insert("v7-1-64.png".to_owned());
        assets.preview_render_queue.push_back(PreviewRenderJob {
            kind: PreviewRenderKind::Material(Uuid(1)),
            size: 64,
            cache_path: "v7-1-64.png".to_owned(),
        });
        assert!(assets.preview_render_pending());

        let jobs = assets.take_preview_render_jobs(8);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].kind, PreviewRenderKind::Material(Uuid(1)));
        assert!(!assets.preview_render_pending(), "the queue drained");

        assets.finish_preview_render("v7-1-64.png");
        assert!(
            !assets.preview_render_in_flight.contains("v7-1-64.png"),
            "the in-flight marker cleared so a later miss re-enqueues"
        );
    }

    #[test]
    fn material_hash_changes_with_resolved_params() {
        // No GPU needed: a pure CPU hash over the resolved material.
        let mut a = crate::material::default_material_asset();
        let s1 = thumbnail_material_hash(&a);
        a.metallic = 0.5;
        let s2 = thumbnail_material_hash(&a);
        assert_ne!(
            s1, s2,
            "a param change retires the cached material thumbnail"
        );
        a.albedo_texture = Uuid(1234);
        let s3 = thumbnail_material_hash(&a);
        assert_ne!(s2, s3, "a texture id change moves the hash");
    }

    #[test]
    fn content_cache_path_is_version_hash_and_size() {
        // The cache is content-addressed with a version prefix: `v<VERSION>-<contentHash>-<size>.png`,
        // no project/uuid in it, so identical content shares one file across projects and a version
        // bump retires every kind's tiles.
        let assets = AssetServer::new(temp_root("path"));
        let path = assets.thumbnail_content_cache_path(0xABCD, 128);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(format!("v{THUMBNAIL_CACHE_VERSION}-43981-128.png")).as_deref(),
            "the filename is the version prefix, decimal content hash, and size"
        );
        assert_eq!(path.parent(), Some(assets.thumbnail_cache_dir().as_path()));
    }

    #[test]
    fn eviction_drops_oldest_files_down_to_target() {
        let dir = temp_root("evict")
            .parent()
            .expect("parent")
            .join("thumb-cache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        // Five 100-byte files, written oldest-first with strictly increasing mtimes so the
        // eviction order is deterministic.
        for i in 0..5u32 {
            let path = dir.join(format!("{i}.png"));
            std::fs::write(&path, [0u8; 100]).expect("write");
            filetime_set(&path, 1_000 + u64::from(i));
        }

        // Cap 500, target 250: over-cap by one file's worth; evict oldest until <= 250.
        evict_thumbnail_cache(&dir, 450, 250);

        let survivors: Vec<u32> = (0..5)
            .filter(|i| dir.join(format!("{i}.png")).exists())
            .collect();
        assert_eq!(
            survivors,
            vec![3, 4],
            "the two newest files survive; the three oldest are evicted to reach the target"
        );
    }

    /// Sets a file's mtime to `secs` past the epoch (test-only, so eviction order is
    /// deterministic rather than depending on write timing).
    fn filetime_set(path: &Path, secs: u64) {
        let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        std::fs::File::open(path)
            .and_then(|f| f.set_modified(mtime))
            .expect("set mtime");
    }
}
