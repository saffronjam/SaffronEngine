//! Thumbnail resolution: classify an asset into a preview subject, resolve its content-addressed
//! cache key, and either return a cache hit or enqueue a main-graph render.
//!
//! Material, texture-map, mesh, model, and HDRI tiles render through the **main forward+
//! graph** on the offscreen thumbnail view (the interactive previewer's path). Authored vegetation
//! kinds rasterize their canonical vector type icons synchronously. [`request_thumbnail`] runs on
//! the main thread: it resolves `{preview subject, content hash, cache path}` — a stored catalog
//! hash for a mesh/texture/model, a live resolved hash for a material — then returns a cache hit or
//! enqueues a [`PreviewRenderJob`] onto [`AssetServer::preview_render_queue`] and replies
//! `pending`. The host drains that queue in `on_update` (build the preview scene → render → write
//! the disk cache); the editor repolls and hits the written cache. No off-thread rendering: decode +
//! upload happen lazily on the main thread during the render, through the scene's own asset loaders.

mod cache;
mod hash;
mod job;

#[cfg(test)]
mod test_support;

pub use cache::{THUMBNAIL_CACHE_VERSION, ThumbnailCacheStats, write_thumbnail_cache};

use std::path::Path;

use saffron_core::Uuid;
use saffron_geometry::glam::Mat4;
use saffron_scene::{AssetType, TextureRole};

use crate::material::MaterialAsset;
use crate::{AssetServer, Error, Result};

use self::cache::read_thumbnail_cache;
use self::job::build_thumbnail_job;

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
/// plus the texture role the classifier reads. The main-graph render re-resolves everything from the
/// catalog by id, so no render payload is carried here.
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
    /// A material, keyed on its live resolved params (via `thumbnail_material_hash`) rather than
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
    /// The content hash was derived from the gathered bytes because the catalog row carried none;
    /// the caller backfills + persists it.
    pub self_healed: bool,
    /// The type-specific content + inputs.
    pub content: ThumbnailContent,
}

/// A preview render subject — every asset kind maps to one. Rendered through the main forward+ graph
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
    /// A plant family, shown as its compiled renderable form on the studio floor.
    Plant(Uuid),
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

impl AssetServer {
    /// Whether any main-graph preview render is queued. Drives the host's render-activity reason so
    /// the loop keeps full cadence until the queue drains.
    #[must_use]
    pub fn preview_render_pending(&self) -> bool {
        !self.preview_render_queue.is_empty()
    }

    /// Pops the next queued preview render job. Exactly one tile is in flight at a time: each
    /// converges over many frames on the shared frame ring, so a second tile would interleave two
    /// converging subjects through one set of temporal accumulators.
    pub fn take_preview_render_job(&mut self) -> Option<PreviewRenderJob> {
        self.preview_render_queue.pop_front()
    }

    /// Clears a job's in-flight marker after the host renders (or fails to render) it, so a later
    /// re-request re-enqueues if the cache miss recurs (e.g. after an eviction).
    pub fn finish_preview_render(&mut self, cache_path: &str) {
        self.preview_render_in_flight.remove(cache_path);
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
/// thumbnail or a failed generation.
pub fn request_thumbnail(assets: &mut AssetServer, id: Uuid, size: u32) -> Result<ThumbnailReply> {
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?
        .clone();

    if let Some(svg) = vegetation_icon_svg(entry.asset_type) {
        return vegetation_icon_thumbnail(assets, svg, size);
    }

    // Texture / mesh / model carry a stored content hash: resolve the cache path + preview subject
    // without loading the container or building a render payload (it also skips the model-forest
    // slice). A cache hit returns; a miss enqueues the main-graph render. A material keys on its
    // live resolved state, so it falls through to the job build below.
    if matches!(
        entry.asset_type,
        AssetType::Texture | AssetType::Mesh | AssetType::Model | AssetType::Plant
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

    // Persist a derived hash so later boots take the cheap path above instead of re-loading the
    // container every time.
    if job.self_healed && job.content_hash != 0 {
        let updated = assets.backfill_asset_content_hash(id, job.content_hash);
        debug_assert!(updated, "thumbnail subject remains catalogued");
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

fn vegetation_icon_svg(asset_type: AssetType) -> Option<&'static str> {
    match asset_type {
        AssetType::Biome => Some(include_str!("../../../../assets/icons/tree-pine.svg")),
        AssetType::VegetationMap => Some(include_str!("../../../../assets/icons/map.svg")),
        _ => None,
    }
}

fn vegetation_icon_thumbnail(
    assets: &AssetServer,
    svg: &'static str,
    size: u32,
) -> Result<ThumbnailReply> {
    let content_hash = crate::import::hash_bytes_fnv(svg.as_bytes());
    let cache_path = assets.thumbnail_content_cache_path(content_hash, size);
    if let Some(hit) = read_thumbnail_cache(&cache_path) {
        return Ok(ready_reply(hit));
    }
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
        .map_err(|error| Error::Thumbnail(error.to_string()))?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| Error::Thumbnail("thumbnail size is invalid".to_owned()))?;
    let scale = size as f32 / 24.0;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let png = pixmap
        .encode_png()
        .map_err(|error| Error::Thumbnail(error.to_string()))?;
    write_thumbnail_cache(&cache_path, &png)?;
    Ok(ThumbnailReply {
        png,
        width: size,
        height: size,
        pending: false,
    })
}

/// The preview subject a texture / mesh / model catalog entry renders as — classified from its type
/// and role alone, no payload build.
fn entry_preview_kind(entry: &saffron_scene::AssetEntry, id: Uuid) -> PreviewRenderKind {
    match entry.asset_type {
        AssetType::Mesh => PreviewRenderKind::Mesh(id),
        AssetType::Model => PreviewRenderKind::Model(id),
        AssetType::Plant => PreviewRenderKind::Plant(id),
        AssetType::Texture if entry.role == TextureRole::Hdri => PreviewRenderKind::Hdri(id),
        AssetType::Texture => PreviewRenderKind::TextureRole {
            tid: id,
            role: entry.role,
        },
        _ => unreachable!("only texture/mesh/model/plant reach the stored-hash cheap path"),
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

    use super::test_support::{isolated_server, temp_root};

    /// Every content kind classifies to a main-graph preview subject: material/texture on the
    /// sphere, HDRI to the chrome ball, mesh/model to itself.
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
        assert_eq!(
            preview_render_kind(&texture(TextureRole::Hdri), Uuid(3)),
            PreviewRenderKind::Hdri(Uuid(3))
        );
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

    /// The preview render queue reports pending, drains up to the budget, and dedups on the cache
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

        let job = assets.take_preview_render_job().expect("one job queued");
        assert_eq!(job.kind, PreviewRenderKind::Material(Uuid(1)));
        assert!(!assets.preview_render_pending(), "the queue drained");

        assets.finish_preview_render("v7-1-64.png");
        assert!(
            !assets.preview_render_in_flight.contains("v7-1-64.png"),
            "the in-flight marker cleared so a later miss re-enqueues"
        );
    }
}
