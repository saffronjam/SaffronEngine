//! The off-thread thumbnail worker — the crate's one cross-thread shared-mutable site.
//!
//! Thumbnails are generated off the frame loop so a cold cache-miss never blocks
//! rendering. [`ThumbnailWorker`] owns a [`std::thread::JoinHandle`] plus the shared job
//! / result state behind a [`Mutex`] + [`Condvar`] — the legitimate `Arc<Mutex<…>>` of
//! the assets Ref-policy ledger (bucket 2), and *exactly* the marked GPU-queue-sharing
//! thread. [`WorkerState`] holds the job [`VecDeque`], the `in_flight` / `failed` dedup
//! sets (keyed by cache path), the two handback [`Vec`]s, and the `stop` flag.
//!
//! # The seam to the GPU (the worker decodes, then calls a [`ThumbnailGpu`])
//!
//! The worker **decodes the image bytes on its own thread**, then calls the GPU
//! primitives through the [`ThumbnailGpu`] trait — the upload trio plus the three
//! render-to-PNG entry points. A live implementation routes these to
//! `saffron-rendering` (which takes the queue + bindless mutexes internally) bound to
//! the worker's dedicated command pool via [`ThumbnailGpu::bind_worker_thread`]; the
//! tests drive a counting stub. The finished `Arc<GpuTexture>` / `Arc<GpuMesh>` handles
//! cross the thread boundary in the handback — the one place this crate relies on GPU
//! `Arc`s crossing threads (they are `Send + Sync`).
//!
//! # Teardown ordering (idle-before-clear)
//!
//! [`AssetServer::stop_thumbnail_worker`] sets `stop`, notifies, and joins **before**
//! `wait_gpu_idle` / renderer teardown, so the worker's last submit's fences have
//! completed and its un-handed-back textures drop while the renderer is still alive.
//! [`AssetServer::clear_thumbnail_queue`] (a project switch, GPU idle at the call site)
//! abandons queued jobs + dedup state + un-drained handbacks; an already-running job
//! finishes harmlessly and its single handback is dropped on the next switch.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use saffron_core::Uuid;
use saffron_geometry::glam::{Mat3, Mat4};
use saffron_geometry::{
    ChunkKind, Mesh, Submesh, Vertex, decode_image, decode_image_from_memory,
    decode_image_from_memory_hdr, decode_image_hdr, load_mesh, load_mesh_from_bytes,
};
use saffron_rendering::{GpuMesh, GpuTexture, PngTransfer, SubmeshMaterial};
use saffron_scene::{AssetType, Colorspace};

use crate::gpu::GpuUploader;
use crate::material::MaterialAsset;
use crate::render_material::build_submesh_material;
use crate::{AssetServer, Error, Result};

/// The thumbnail cache version, folded into every stamp so a behaviour change retires
/// the whole on-disk cache. At `v2`, model thumbnails render textured.
pub const THUMBNAIL_CACHE_VERSION: u32 = 3;

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

/// A thumbnail request's reply: the PNG (a cache hit or freshly generated), or a
/// `pending` flag telling the editor to retry while the worker generates it.
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

/// The GPU seam the thumbnail worker drives: the upload trio (from [`GpuUploader`]) plus
/// the three render-to-PNG entry points and the per-thread command-pool bind.
///
/// A live implementation routes these to `saffron-rendering`'s thumbnail primitives
/// (`render_material_preview` / `encode_asset_thumbnail_png` / `encode_model_thumbnail_png`
/// / `encode_texture_thumbnail_png` / `bind_thumbnail_worker_thread`), which take the
/// queue + bindless mutexes internally; the tests implement a counting stub. The worker
/// holds it as a `&dyn ThumbnailGpu`, so the worker logic is exercised without a Vulkan
/// device while the production path performs the real render.
pub trait ThumbnailGpu: GpuUploader {
    /// Binds the calling thread to the renderer's dedicated thumbnail command pool (every
    /// subsequent one-off upload/render/readback on this thread allocates from it). Called
    /// once at the top of the worker loop; idempotent per thread.
    fn bind_worker_thread(&self);

    /// Renders `texture` downscaled to fit `size`×`size`, encoding the read-back to PNG.
    /// `transfer` selects the HDR mapping (`Tonemap` for an HDR asset, `Clamp` otherwise).
    ///
    /// # Errors
    ///
    /// Propagates the renderer's render/read-back/encode failure.
    fn encode_texture_thumbnail_png(
        &self,
        texture: &Arc<GpuTexture>,
        size: u32,
        transfer: PngTransfer,
    ) -> saffron_rendering::Result<ThumbnailPng>;

    /// Renders `mesh` framed by its AABB under fixed lighting, encoding the read-back to
    /// PNG (the flat-mesh asset tile).
    ///
    /// # Errors
    ///
    /// Propagates the renderer's render/read-back/encode failure.
    fn encode_asset_thumbnail_png(
        &self,
        mesh: &Arc<GpuMesh>,
        size: u32,
    ) -> saffron_rendering::Result<ThumbnailPng>;

    /// Renders `mesh` shaded with its per-submesh materials (the textured model tile),
    /// encoding the read-back to PNG.
    ///
    /// # Errors
    ///
    /// Propagates the renderer's render/read-back/encode failure.
    fn encode_model_thumbnail_png(
        &self,
        mesh: &Arc<GpuMesh>,
        submesh_materials: &[SubmeshMaterial],
        size: u32,
    ) -> saffron_rendering::Result<ThumbnailPng>;

    /// Renders a unit sphere with `material` under studio lighting into a `size`×`size`
    /// texture (the material-preview pane + cached material thumbnails). `shader_spv` of
    /// `None` uses the cached default studio preview pipeline; a non-foldable graph material
    /// passes its compiled `_preview.spv` path for a per-call codegen pipeline.
    ///
    /// # Errors
    ///
    /// Propagates the renderer's preview-render failure.
    fn render_material_preview(
        &self,
        material: &SubmeshMaterial,
        size: u32,
        shader_spv: Option<&Path>,
    ) -> saffron_rendering::Result<Arc<GpuTexture>>;
}

/// One texture the worker must decode + upload, resolved from the catalog at enqueue.
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
    /// Embedded chunk image bytes (decoded from memory when non-empty).
    pub bytes: Vec<u8>,
}

/// What kind of thumbnail a [`ThumbnailJob`] renders, carrying the type-specific inputs
/// the worker needs, as a data-carrying enum.
#[derive(Clone, Debug)]
pub enum ThumbnailContent {
    /// A texture preview: decode + upload the source, then read it back.
    Texture(ThumbnailTextureSource),
    /// A standalone or embedded mesh: load (file path) or decode (`bytes`) then render.
    Mesh {
        /// The standalone `.smesh` path (empty for an embedded mesh).
        path: String,
        /// The embedded `.smesh` chunk image (empty for a standalone mesh).
        bytes: Vec<u8>,
    },
    /// A material preview: upload the referenced textures, build the submesh material,
    /// then render the studio sphere.
    Material {
        /// The parent-resolved material (boxed — it dwarfs the other variants).
        material: Box<MaterialAsset>,
        /// The material's referenced textures (decoded + uploaded on the worker thread).
        textures: Vec<ThumbnailTextureSource>,
    },
    /// A model preview: every mesh-bearing node of the model's forest, each at its node world
    /// transform, shaded with the container's per-submesh materials. The worker bakes the
    /// transforms and merges the chunks into one mesh before rendering, so a multi-node model
    /// previews as the assembled whole, not a single node's fragment.
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

/// One mesh chunk of a model thumbnail: a node's `.smesh` image plus the node's world
/// transform, which the worker bakes into the vertices when assembling the forest.
#[derive(Clone, Debug)]
pub struct ModelMeshChunk {
    /// The node's `.smesh` chunk image.
    pub bytes: Vec<u8>,
    /// The node's world transform (composed up its parent chain).
    pub transform: Mat4,
}

/// One unit of work for the thumbnail worker: a resolved {asset, size} request.
///
/// The catalog/material/container resolution happens on the main thread at enqueue (the
/// worker has no [`AssetServer`]); the job carries the resolved bytes/materials so the
/// worker only decodes + uploads + renders.
#[derive(Clone, Debug)]
pub struct ThumbnailJob {
    /// The asset id.
    pub id: Uuid,
    /// The requested square pixel size.
    pub size: u32,
    /// The on-disk cache path (`<projectRoot>/cache/thumbnails/<…>.png`).
    pub cache_path: String,
    /// The type-specific content + inputs.
    pub content: ThumbnailContent,
}

/// The handback bucket the worker fills and the main thread drains: the freshly uploaded
/// GPU resources, keyed by their asset id, to insert into the caches.
type TextureHandback = Vec<(Uuid, Arc<GpuTexture>)>;
type MeshHandback = Vec<(Uuid, Arc<GpuMesh>)>;

/// The mutex-guarded shared state between the worker thread and the main thread.
///
/// Guarded by one [`Mutex`], woken by a [`Condvar`].
#[derive(Default)]
pub struct WorkerState {
    /// Pending jobs, FIFO.
    queue: VecDeque<ThumbnailJob>,
    /// Cache paths queued or running — dedup retries.
    in_flight: HashSet<String>,
    /// Cache paths that failed — settle to the type icon, never retried.
    failed: HashSet<String>,
    /// Finished texture uploads to hand back to the main-thread cache.
    texture_handback: TextureHandback,
    /// Finished mesh uploads to hand back to the main-thread cache.
    mesh_handback: MeshHandback,
    /// Teardown / project-switch signal: the loop returns on its next wake.
    stop: bool,
}

/// The off-thread thumbnail worker: a [`JoinHandle`] plus the shared [`WorkerState`]
/// behind a [`Mutex`] + [`Condvar`].
///
/// [`Drop`] does **not** join — joining must happen *before* `wait_gpu_idle` / renderer
/// teardown, so [`AssetServer::stop_thumbnail_worker`] joins explicitly.
pub struct ThumbnailWorker {
    /// The worker thread's join handle (`None` after an explicit stop+join).
    handle: Option<JoinHandle<()>>,
    /// The shared state + its wake condvar.
    shared: Arc<(Mutex<WorkerState>, Condvar)>,
}

impl ThumbnailWorker {
    /// Spawns the worker over `gpu` (a `'static` GPU seam the worker owns for its life).
    ///
    /// The worker binds its thread to the dedicated command pool, then loops:
    /// wait → pop → decode + upload + render → handback / mark-failed.
    fn spawn(gpu: Box<dyn ThumbnailGpu + Send>) -> Self {
        let shared = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let worker_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("thumbnail-worker".to_owned())
            .spawn(move || worker_loop(&worker_shared, gpu.as_ref()))
            .expect("spawn thumbnail worker");
        Self {
            handle: Some(handle),
            shared,
        }
    }
}

/// The worker thread body: bind the pool, then wait → pop → generate → handback forever
/// until `stop`.
fn worker_loop(shared: &Arc<(Mutex<WorkerState>, Condvar)>, gpu: &dyn ThumbnailGpu) {
    gpu.bind_worker_thread();
    let (lock, cv) = &**shared;
    loop {
        let job = {
            let mut state = lock.lock().expect("worker state mutex");
            state = cv
                .wait_while(state, |s| !s.stop && s.queue.is_empty())
                .expect("worker condvar wait");
            if state.stop {
                return; // teardown / project switch: abandon any queued jobs.
            }
            state.queue.pop_front().expect("queue non-empty after wait")
        };

        let mut texture_out: TextureHandback = Vec::new();
        let mut mesh_out: MeshHandback = Vec::new();
        let png = generate_thumbnail(gpu, &job, &mut texture_out, &mut mesh_out);

        let mut state = lock.lock().expect("worker state mutex");
        state.in_flight.remove(&job.cache_path);
        match png {
            Ok(png) => {
                if let Err(err) = write_thumbnail_cache(Path::new(&job.cache_path), &png.bytes) {
                    tracing::warn!("{err}");
                }
                state.texture_handback.append(&mut texture_out);
                state.mesh_handback.append(&mut mesh_out);
            }
            Err(err) => {
                tracing::warn!("thumbnail {}: {err}", job.id.value());
                // A missing thumbnail settles to the type icon — never retried.
                state.failed.insert(job.cache_path.clone());
            }
        }
    }
}

/// Decodes + uploads one texture (worker or sync path), recording the `(id, Arc)` for the
/// cache handback. Returns the live `Arc<GpuTexture>` or `None` on a decode/upload failure
/// (logged warn).
fn upload_thumbnail_texture(
    gpu: &dyn ThumbnailGpu,
    src: &ThumbnailTextureSource,
    handback: &mut TextureHandback,
) -> Option<Arc<GpuTexture>> {
    if src.hdr {
        let decoded = if src.bytes.is_empty() {
            decode_image_hdr(&src.path)
        } else {
            decode_image_from_memory_hdr(&src.bytes)
        };
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(err) => {
                tracing::warn!("{err}");
                return None;
            }
        };
        let tex = match gpu.upload_texture_float(&decoded.rgba, decoded.width, decoded.height) {
            Ok(tex) => tex,
            Err(err) => {
                tracing::warn!("{err}");
                return None;
            }
        };
        handback.push((src.id, Arc::clone(&tex)));
        return Some(tex);
    }
    let decoded = if src.bytes.is_empty() {
        decode_image(&src.path)
    } else {
        decode_image_from_memory(&src.bytes)
    };
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(err) => {
            tracing::warn!("{err}");
            return None;
        }
    };
    let tex = match gpu.upload_texture(&decoded.rgba, decoded.width, decoded.height, src.srgb) {
        Ok(tex) => tex,
        Err(err) => {
            tracing::warn!("{err}");
            return None;
        }
    };
    handback.push((src.id, Arc::clone(&tex)));
    Some(tex)
}

/// Generates the PNG for a resolved job — no cache write, no catalog/map access.
///
/// Uploaded GPU resources are appended to `texture_out` / `mesh_out` for the caller to
/// cache. Runs on the worker thread (worker command pool, queue/bindless mutexes) or, with
/// no worker, inline on the main thread.
///
/// # Errors
///
/// [`Error::Thumbnail`] when the asset fails to load/decode/render, propagating the
/// renderer's failure message.
fn generate_thumbnail(
    gpu: &dyn ThumbnailGpu,
    job: &ThumbnailJob,
    texture_out: &mut TextureHandback,
    mesh_out: &mut MeshHandback,
) -> Result<ThumbnailPng> {
    match &job.content {
        ThumbnailContent::Texture(src) => {
            let tex = upload_thumbnail_texture(gpu, src, texture_out)
                .ok_or_else(|| Error::Thumbnail("texture failed to load".to_owned()))?;
            let transfer = if src.hdr {
                PngTransfer::Tonemap
            } else {
                PngTransfer::Clamp
            };
            Ok(gpu
                .encode_texture_thumbnail_png(&tex, job.size, transfer)
                .map_err(|e| Error::Thumbnail(e.to_string()))?)
        }
        ThumbnailContent::Mesh { path, bytes } => {
            let mesh = if bytes.is_empty() {
                load_mesh(path)?
            } else {
                load_mesh_from_bytes(bytes)?
            };
            let mesh_ref = gpu
                .upload_mesh(&mesh, &[], None, None)
                .map_err(|e| Error::Thumbnail(e.to_string()))?;
            mesh_out.push((job.id, Arc::clone(&mesh_ref)));
            Ok(gpu
                .encode_asset_thumbnail_png(&mesh_ref, job.size)
                .map_err(|e| Error::Thumbnail(e.to_string()))?)
        }
        ThumbnailContent::Material { material, textures } => {
            let mut local: std::collections::HashMap<u64, Arc<GpuTexture>> =
                std::collections::HashMap::new();
            for src in textures {
                if let Some(tex) = upload_thumbnail_texture(gpu, src, texture_out) {
                    local.insert(src.id.value(), tex);
                }
            }
            let sm = build_submesh_material(material, &mut |tid| local.get(&tid.value()).cloned());
            // The disk-cached material tile renders through the default studio preview; the
            // codegen `_preview.spv` path is reserved for the live `preview-render` command,
            // which
            // has the `AssetServer` to compile it.
            let tex = gpu
                .render_material_preview(&sm, job.size, None)
                .map_err(|e| Error::Thumbnail(e.to_string()))?;
            Ok(gpu
                .encode_texture_thumbnail_png(&tex, job.size, PngTransfer::Clamp)
                .map_err(|e| Error::Thumbnail(e.to_string()))?)
        }
        ThumbnailContent::Model {
            meshes,
            materials,
            textures,
        } => {
            let mut local: std::collections::HashMap<u64, Arc<GpuTexture>> =
                std::collections::HashMap::new();
            for src in textures {
                if let Some(tex) = upload_thumbnail_texture(gpu, src, texture_out) {
                    local.insert(src.id.value(), tex);
                }
            }
            let submesh_materials: Vec<SubmeshMaterial> = materials
                .iter()
                .map(|mat| build_submesh_material(mat, &mut |tid| local.get(&tid.value()).cloned()))
                .collect();
            // Decode each node chunk, bake its world transform, and merge into one mesh so the
            // single-mesh render path frames the assembled forest by its full bounds.
            let mut chunks = Vec::with_capacity(meshes.len());
            for chunk in meshes {
                chunks.push((load_mesh_from_bytes(&chunk.bytes)?, chunk.transform));
            }
            let mesh = merge_model_meshes(&chunks);
            let mesh_ref = gpu
                .upload_mesh(&mesh, &[], None, None)
                .map_err(|e| Error::Thumbnail(e.to_string()))?;
            Ok(gpu
                .encode_model_thumbnail_png(&mesh_ref, &submesh_materials, job.size)
                .map_err(|e| Error::Thumbnail(e.to_string()))?)
        }
    }
}

/// An FNV-1a 64-bit accumulator: the thumbnail-stamp fold over `u64` words + `f32` bits.
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

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// FNV-1a folds `version | file_size | mtime_ticks` into a compact hex token — the
/// `<stamp>` filename field.
fn fold_thumbnail_stamp(version: u64, file_size: u64, mtime_ticks: u64) -> String {
    let mut h = FnvHash::new();
    h.mix(version);
    h.mix(file_size);
    h.mix(mtime_ticks);
    h.hex()
}

/// A material thumbnail keys on its *resolved* state (a content hash of the resolved
/// params + texture uuids), not a file stamp — editing a parent material reflows every
/// instance without touching the child `.smat`. Folded with the cache version.
fn thumbnail_material_stamp(m: &MaterialAsset) -> String {
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
    h.hex()
}

/// The leading uuid of a cache filename (`<uuid>-<size>-<stamp>.png`); `0` if it doesn't
/// parse.
fn thumbnail_cache_file_uuid(filename: &str) -> u64 {
    match filename.split_once('-') {
        Some((head, _)) => head.parse().unwrap_or(0),
        None => 0,
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

/// Writes a generated PNG into the cache dir, creating the parent dir.
///
/// # Errors
///
/// [`Error::Io`] if the parent dir cannot be created or the file cannot be written.
fn write_thumbnail_cache(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
    }
    std::fs::write(path, bytes).map_err(|e| {
        Error::Io(format!(
            "write failed for thumbnail cache '{}': {e}",
            path.display()
        ))
    })
}

/// Inserts handed-back GPU resources into the caches, skipping uuids already cached.
/// Main thread only.
fn insert_thumbnail_handback(
    assets: &mut AssetServer,
    textures: TextureHandback,
    meshes: MeshHandback,
) {
    for (id, tex) in textures {
        assets
            .texture_by_uuid
            .entry(id.value())
            .or_insert(Some(tex));
    }
    for (id, mesh) in meshes {
        assets.mesh_by_uuid.entry(id.value()).or_insert(Some(mesh));
    }
}

impl AssetServer {
    /// The cache path for `{id, size, stamp}` (`<uuid>-<size>-<stamp>.png` under the
    /// thumbnail cache dir).
    fn thumbnail_cache_path(&self, id: Uuid, size: u32, stamp: &str) -> PathBuf {
        self.thumbnail_cache_dir()
            .join(format!("{}-{size}-{stamp}.png", id.value()))
    }

    /// FNV-1a stamp of the asset's source file (size + mtime, folded with the cache
    /// version). Empty when the file is missing/unstattable — the entry is then never
    /// cached.
    fn thumbnail_source_stamp(&self, rel_path: &str) -> String {
        if rel_path.is_empty() {
            return String::new();
        }
        let src = self.root.join(rel_path);
        let Ok(meta) = std::fs::metadata(&src) else {
            return String::new();
        };
        let Ok(modified) = meta.modified() else {
            return String::new();
        };
        let mtime = modified
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        fold_thumbnail_stamp(u64::from(THUMBNAIL_CACHE_VERSION), meta.len(), mtime)
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

    /// Empties the project's cache dir, returning what was removed.
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

    /// Removes every cached thumbnail for one asset uuid (all sizes/stamps) — on delete +
    /// reimport.
    pub fn remove_thumbnail_cache_for_asset(&self, id: Uuid) {
        let dir = self.thumbnail_cache_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if thumbnail_cache_file_uuid(&name.to_string_lossy()) == id.value() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Deletes cache files whose uuid is no longer in the catalog (reimport mints new
    /// uuids, orphaning the old PNGs). Run on project load.
    pub fn sweep_thumbnail_cache_orphans(&self) {
        let dir = self.thumbnail_cache_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let uuid = thumbnail_cache_file_uuid(&name.to_string_lossy());
            if uuid == 0 || !self.catalog.by_id.contains_key(&uuid) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Starts the off-thread thumbnail worker over `gpu` (a `'static` GPU seam), if not
    /// already running.
    ///
    /// The caller must have prewarmed the renderer's lazy preview pipelines on the main
    /// thread first, so the worker never races their
    /// first-use initialization. Idempotent: a second call while a worker runs is a no-op.
    pub fn start_thumbnail_worker(&mut self, gpu: Box<dyn ThumbnailGpu + Send>) {
        if self.thumbnail_worker.is_some() {
            return;
        }
        self.thumbnail_worker = Some(ThumbnailWorker::spawn(gpu));
    }

    /// Sets `stop`, notifies, and joins the worker thread, then drops it.
    ///
    /// Called **before** `wait_gpu_idle` / renderer teardown: the worker's last submit's
    /// fences have completed and its un-handed-back textures are referenced by no frame, so
    /// dropping them here frees their GPU resources safely.
    pub fn stop_thumbnail_worker(&mut self) {
        let Some(mut worker) = self.thumbnail_worker.take() else {
            return;
        };
        let (lock, cv) = &*worker.shared;
        {
            let mut state = lock.lock().expect("worker state mutex");
            state.stop = true;
        }
        cv.notify_all();
        if let Some(handle) = worker.handle.take() {
            let _ = handle.join();
        }
    }

    /// Drains the worker's finished uploads into the GPU caches. Call once per frame on the
    /// main thread.
    pub fn drain_thumbnail_completions(&mut self) {
        let Some(worker) = self.thumbnail_worker.as_ref() else {
            return;
        };
        let (lock, _cv) = &*worker.shared;
        let (textures, meshes) = {
            let mut state = lock.lock().expect("worker state mutex");
            (
                std::mem::take(&mut state.texture_handback),
                std::mem::take(&mut state.mesh_handback),
            )
        };
        insert_thumbnail_handback(self, textures, meshes);
    }
}

/// Abandons the worker's queue + dedup/failed state + un-drained handbacks (a project
/// switch, GPU idle at the call site). Standalone so [`AssetServer::clear_thumbnail_queue`]
/// (defined in `lib.rs`, called by `clear_asset_caches`) can drive it without a borrow
/// tangle.
pub(crate) fn clear_worker_queue(worker: &ThumbnailWorker) {
    let (lock, _cv) = &*worker.shared;
    let mut state = lock.lock().expect("worker state mutex");
    state.queue.clear();
    state.in_flight.clear();
    state.failed.clear();
    state.texture_handback.clear();
    state.mesh_handback.clear();
}

/// Builds the resolved [`ThumbnailJob`] for `{id, size}` from the catalog/material/
/// container state, plus its cache stamp. Runs on the main thread (the worker has no
/// [`AssetServer`]). Returns the job alone — the caller decides cache-hit vs. enqueue vs.
/// sync. The catalog/material resolution half of the thumbnail request.
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

    let (content, stamp): (ThumbnailContent, String) = match entry.asset_type {
        AssetType::Material => {
            let material = crate::material::load_catalog_material_asset(assets, id)?;
            let stamp = thumbnail_material_stamp(&material);
            let textures = if entry.container.value() == 0 {
                resolve_material_textures(assets, &material)
            } else {
                let container = assets.load_model_asset(entry.container).ok_or_else(|| {
                    Error::Thumbnail(format!("model {} is not loadable", entry.container.value()))
                })?;
                let mut textures = Vec::new();
                let mut added = HashSet::new();
                for tid in material_texture_ids(&material) {
                    add_model_texture(assets, &container, tid, &mut added, &mut textures);
                }
                textures
            };
            (
                ThumbnailContent::Material {
                    material: Box::new(material),
                    textures,
                },
                stamp,
            )
        }
        // An embedded texture sub-asset lives inside its `.smodel`; read the chunk bytes
        // from the container instead of decoding the container path as an image.
        AssetType::Texture if entry.container.value() != 0 => {
            let stamp = assets.thumbnail_source_stamp(&entry.path);
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
                bytes,
            };
            (ThumbnailContent::Texture(src), stamp)
        }
        AssetType::Texture => {
            let stamp = assets.thumbnail_source_stamp(&entry.path);
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
                bytes: Vec::new(),
            };
            (ThumbnailContent::Texture(src), stamp)
        }
        AssetType::Mesh if entry.container.value() == 0 => {
            let stamp = assets.thumbnail_source_stamp(&entry.path);
            let path = format!("{}/{}", assets.root.display(), entry.path);
            (
                ThumbnailContent::Mesh {
                    path,
                    bytes: Vec::new(),
                },
                stamp,
            )
        }
        AssetType::Mesh | AssetType::Model => build_embedded_job(assets, id, &entry)?,
        _ => {
            return Err(Error::Thumbnail(format!(
                "asset {} has no thumbnail",
                id.value()
            )));
        }
    };

    let cache_path = if stamp.is_empty() {
        String::new()
    } else {
        assets
            .thumbnail_cache_path(id, size, &stamp)
            .display()
            .to_string()
    };

    Ok(ThumbnailJob {
        id,
        size,
        cache_path,
        content,
    })
}

/// Resolves an embedded mesh or a model's preview job: slice the primary mesh chunk on the
/// main thread (the worker parses the bytes we hand it), and for a model resolve each
/// material slot + its referenced textures.
fn build_embedded_job(
    assets: &mut AssetServer,
    id: Uuid,
    entry: &saffron_scene::AssetEntry,
) -> Result<(ThumbnailContent, String)> {
    let is_model = entry.asset_type == AssetType::Model;
    let stamp = assets.thumbnail_source_stamp(&entry.path);

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
        return Ok((
            ThumbnailContent::Mesh {
                path: String::new(),
                bytes: source.read()?,
            },
            stamp,
        ));
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

    // Textured model preview: resolve each material slot (sub-asset order matches the
    // submesh material slot) and hand the worker each referenced texture's bytes.
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

    Ok((
        ThumbnailContent::Model {
            meshes,
            materials,
            textures,
        },
        stamp,
    ))
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

/// Bakes each chunk's world transform into its vertices (normals through the inverse-transpose)
/// and concatenates them into one mesh, rebasing every submesh's indices onto the shared vertex
/// stream. Material slots are preserved (the container's material table is shared across nodes).
fn merge_model_meshes(chunks: &[(Mesh, Mat4)]) -> Mesh {
    let mut out = Mesh::default();
    for (mesh, world) in chunks {
        let normal_mat = Mat3::from_mat4(*world).inverse().transpose();
        let base_vertex = out.vertices.len() as i64;
        for v in &mesh.vertices {
            out.vertices.push(Vertex {
                position: world.transform_point3(v.position),
                normal: (normal_mat * v.normal).normalize_or_zero(),
                uv0: v.uv0,
            });
        }
        for sm in &mesh.submeshes {
            let first_index = out.indices.len() as u32;
            let start = sm.first_index as usize;
            let end = start + sm.index_count as usize;
            for &idx in &mesh.indices[start..end] {
                let rebased = i64::from(idx) + i64::from(sm.vertex_offset) + base_vertex;
                out.indices.push(rebased as u32);
            }
            out.submeshes.push(Submesh {
                first_index,
                index_count: sm.index_count,
                vertex_offset: 0,
                material_slot: sm.material_slot,
            });
        }
    }
    out
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

/// Resolves a standalone material's referenced textures to [`ThumbnailTextureSource`]s
/// (file paths + colorspace from the catalog rows).
fn resolve_material_textures(
    assets: &AssetServer,
    material: &MaterialAsset,
) -> Vec<ThumbnailTextureSource> {
    let mut out = Vec::new();
    for tid in material_texture_ids(material) {
        if tid.value() == 0 {
            continue;
        }
        if let Some(te) = assets.catalog.find(tid)
            && te.asset_type == AssetType::Texture
        {
            out.push(ThumbnailTextureSource {
                id: tid,
                path: format!("{}/{}", assets.root.display(), te.path),
                hdr: te.hdr,
                srgb: !te.linear,
                bytes: Vec::new(),
            });
        }
    }
    out
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

/// Resolves `{asset, size}` to a thumbnail over `gpu` — a cache hit returns the PNG, a
/// miss generates it (sync when there is no worker, else enqueued with a `pending` reply).
///
/// Materials key on resolved state, mesh/texture on the source-file stat.
///
/// # Errors
///
/// [`Error::NotInCatalog`] for a missing id, [`Error::Thumbnail`] for an asset with no
/// thumbnail / a failed generation / a previously-failed cache key.
pub fn request_thumbnail(
    assets: &mut AssetServer,
    gpu: &dyn ThumbnailGpu,
    id: Uuid,
    size: u32,
) -> Result<ThumbnailReply> {
    let job = build_thumbnail_job(assets, id, size)?;

    if !job.cache_path.is_empty()
        && let Some(hit) = read_thumbnail_cache(Path::new(&job.cache_path))
    {
        return Ok(ThumbnailReply {
            png: hit.bytes,
            width: hit.width,
            height: hit.height,
            pending: false,
        });
    }

    // No worker, or no cache key to dedup/persist against: generate inline on the calling
    // thread and return the result directly.
    if assets.thumbnail_worker.is_none() || job.cache_path.is_empty() {
        let mut texture_out = Vec::new();
        let mut mesh_out = Vec::new();
        let png = generate_thumbnail(gpu, &job, &mut texture_out, &mut mesh_out)?;
        insert_thumbnail_handback(assets, texture_out, mesh_out);
        if !job.cache_path.is_empty() {
            if let Err(err) = write_thumbnail_cache(Path::new(&job.cache_path), &png.bytes) {
                tracing::warn!("{err}");
            }
        }
        return Ok(ThumbnailReply {
            png: png.bytes,
            width: png.width,
            height: png.height,
            pending: false,
        });
    }

    // Worker path: dedup on the cache path, enqueue once, reply pending.
    let worker = assets.thumbnail_worker.as_ref().expect("worker present");
    let (lock, cv) = &*worker.shared;
    let mut state = lock.lock().expect("worker state mutex");
    if state.failed.contains(&job.cache_path) {
        return Err(Error::Thumbnail("thumbnail generation failed".to_owned()));
    }
    if !state.in_flight.contains(&job.cache_path) {
        state.in_flight.insert(job.cache_path.clone());
        state.queue.push_back(job);
        cv.notify_one();
    }
    Ok(ThumbnailReply {
        png: Vec::new(),
        width: 0,
        height: 0,
        pending: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{
        ContainerChunk, Mesh, Submesh, Vertex, save_mesh_to_buffer, write_container,
    };
    use saffron_rendering::{
        BindlessFreeList, Descriptors, Device, GpuQueue, SurfaceSource, Uploader,
    };

    /// A counting GPU seam over a *real* headless device: the upload trio performs the
    /// genuine upload (so the handback `Arc<GpuTexture>`/`Arc<GpuMesh>` are real GPU
    /// resources that drop correctly), while the render-to-PNG primitives count their
    /// calls and return a fixed PNG (no scene render needed to prove the worker mechanism).
    /// Owns its `Uploader` + `Descriptors`, so it is `'static + Send` and the worker thread
    /// can own it for its whole life (the production seam shape).
    struct CountingThumbGpu {
        uploader: Uploader,
        descriptors: Descriptors,
        binds: Arc<AtomicUsize>,
        texture_uploads: Arc<AtomicUsize>,
        mesh_uploads: Arc<AtomicUsize>,
        renders: Arc<AtomicUsize>,
        fail_render: bool,
    }

    impl GpuUploader for CountingThumbGpu {
        fn upload_mesh(
            &self,
            mesh: &Mesh,
            skin: &[saffron_geometry::VertexSkin],
            morph: Option<&saffron_geometry::MorphData>,
            sdf_bake: Option<&saffron_rendering::SdfBake>,
        ) -> saffron_rendering::Result<Arc<GpuMesh>> {
            self.mesh_uploads.fetch_add(1, Ordering::SeqCst);
            self.uploader
                .upload_mesh(&self.descriptors, mesh, skin, morph, sdf_bake)
        }

        fn upload_texture(
            &self,
            rgba: &[u8],
            width: u32,
            height: u32,
            srgb: bool,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            self.texture_uploads.fetch_add(1, Ordering::SeqCst);
            self.uploader
                .upload_texture(&self.descriptors, rgba, width, height, srgb)
        }

        fn upload_texture_float(
            &self,
            rgba: &[f32],
            width: u32,
            height: u32,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            self.texture_uploads.fetch_add(1, Ordering::SeqCst);
            self.uploader
                .upload_texture_float(&self.descriptors, rgba, width, height)
        }

        fn skinning_enabled(&self) -> bool {
            false
        }
    }

    impl ThumbnailGpu for CountingThumbGpu {
        fn bind_worker_thread(&self) {
            self.binds.fetch_add(1, Ordering::SeqCst);
        }

        fn encode_texture_thumbnail_png(
            &self,
            _texture: &Arc<GpuTexture>,
            size: u32,
            _transfer: PngTransfer,
        ) -> saffron_rendering::Result<ThumbnailPng> {
            self.renders.fetch_add(1, Ordering::SeqCst);
            if self.fail_render {
                return Err(saffron_rendering::Error::EmptyMesh);
            }
            Ok(test_png(size))
        }

        fn encode_asset_thumbnail_png(
            &self,
            _mesh: &Arc<GpuMesh>,
            size: u32,
        ) -> saffron_rendering::Result<ThumbnailPng> {
            self.renders.fetch_add(1, Ordering::SeqCst);
            if self.fail_render {
                return Err(saffron_rendering::Error::EmptyMesh);
            }
            Ok(test_png(size))
        }

        fn encode_model_thumbnail_png(
            &self,
            _mesh: &Arc<GpuMesh>,
            _submesh_materials: &[SubmeshMaterial],
            size: u32,
        ) -> saffron_rendering::Result<ThumbnailPng> {
            self.renders.fetch_add(1, Ordering::SeqCst);
            Ok(test_png(size))
        }

        fn render_material_preview(
            &self,
            _material: &SubmeshMaterial,
            _size: u32,
            _shader_spv: Option<&Path>,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            self.renders.fetch_add(1, Ordering::SeqCst);
            // A 1×1 white texture stands in for the rendered sphere.
            self.uploader
                .upload_texture(&self.descriptors, &[255, 255, 255, 255], 1, 1, true)
        }
    }

    /// Serializes the GPU-backed worker tests: each spawns a thread that submits on its
    /// own queue, and the software Vulkan stack (lavapipe) cannot have two devices + two
    /// worker threads live and tearing down at once. Only one fixture exists at a time.
    static GPU_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A live headless device + the owning GPU seam, or `None` (no Vulkan ICD) so the
    /// GPU-backed tests skip rather than fail off-hardware. Holds the `Device` so it
    /// outlives the worker, plus the process-wide [`GPU_LOCK`] guard so no two fixtures
    /// race the software Vulkan stack.
    struct GpuFixture {
        _guard: std::sync::MutexGuard<'static, ()>,
        device: Device,
        free_list: BindlessFreeList,
        queue: GpuQueue,
        binds: Arc<AtomicUsize>,
        texture_uploads: Arc<AtomicUsize>,
        mesh_uploads: Arc<AtomicUsize>,
        renders: Arc<AtomicUsize>,
    }

    fn gpu_or_skip() -> Option<GpuFixture> {
        let guard = GPU_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(std::sync::Mutex::new(Vec::new()));
        let queue = GpuQueue::new(device.graphics_queue);
        Some(GpuFixture {
            _guard: guard,
            device,
            free_list,
            queue,
            binds: Arc::new(AtomicUsize::new(0)),
            texture_uploads: Arc::new(AtomicUsize::new(0)),
            mesh_uploads: Arc::new(AtomicUsize::new(0)),
            renders: Arc::new(AtomicUsize::new(0)),
        })
    }

    impl GpuFixture {
        /// Builds an owning GPU seam sharing this fixture's counters + free-list.
        fn seam(&self, fail_render: bool) -> CountingThumbGpu {
            let descriptors = Descriptors::new(&self.device, &self.free_list).expect("descriptors");
            let uploader = Uploader::new(&self.device, &self.queue).expect("uploader");
            CountingThumbGpu {
                uploader,
                descriptors,
                binds: Arc::clone(&self.binds),
                texture_uploads: Arc::clone(&self.texture_uploads),
                mesh_uploads: Arc::clone(&self.mesh_uploads),
                renders: Arc::clone(&self.renders),
                fail_render,
            }
        }

        /// Idle the GPU, drop the caches, then the device — the README §3 discipline.
        fn teardown(self, mut assets: AssetServer) {
            self.device.wait_idle().expect("idle before teardown");
            assets.clear_asset_caches();
            drop(assets);
            drop(self.device);
        }
    }

    /// A minimal valid PNG of `size`×`size`, so a cache write + header read-back round-trip.
    fn test_png(size: u32) -> ThumbnailPng {
        let s = size.max(1);
        let buffer = image::RgbaImage::from_pixel(s, s, image::Rgba([200, 100, 50, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        buffer
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        ThumbnailPng {
            bytes: out.into_inner(),
            width: s,
            height: s,
        }
    }

    /// A baked `.smesh` byte image (a single-triangle mesh) the worker decodes + uploads.
    fn smesh_bytes() -> Vec<u8> {
        let mesh = Mesh {
            vertices: vec![
                Vertex {
                    position: Vec3::ZERO,
                    normal: Vec3::Z,
                    uv0: Vec2::ZERO,
                },
                Vertex {
                    position: Vec3::X,
                    normal: Vec3::Z,
                    uv0: Vec2::new(1.0, 0.0),
                },
                Vertex {
                    position: Vec3::Y,
                    normal: Vec3::Z,
                    uv0: Vec2::new(0.0, 1.0),
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

    /// Blocks until `predicate(state)` holds or a deadline passes, polling the worker state
    /// — a deterministic settle without a sleep race.
    fn wait_until<F: Fn(&WorkerState) -> bool>(worker: &ThumbnailWorker, predicate: F) -> bool {
        let (lock, _cv) = &*worker.shared;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if predicate(&lock.lock().expect("mutex")) {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::yield_now();
        }
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

    fn put_mesh_row(assets: &mut AssetServer, id: Uuid) {
        std::fs::create_dir_all(assets.root.join("models")).expect("models dir");
        std::fs::write(assets.root.join("models/m.smesh"), smesh_bytes()).expect("smesh");
        assets.catalog.put(saffron_scene::AssetEntry {
            id,
            name: "m".to_owned(),
            asset_type: AssetType::Mesh,
            path: "models/m.smesh".to_owned(),
            ..Default::default()
        });
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
        for row in crate::import::catalog_rows_for_model(&meta, &rel) {
            assets.catalog.put(row);
        }
    }

    #[test]
    fn model_thumbnail_job_reads_embedded_material_chunks() {
        let root = temp_root("embedded-model-material");
        let mut assets = AssetServer::new(&root);
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
        let mut assets = AssetServer::new(&root);
        let material = MaterialAsset {
            base_color: saffron_geometry::glam::Vec4::new(0.8, 0.2, 0.1, 1.0),
            unlit: true,
            ..MaterialAsset::default()
        };
        put_embedded_material_model(&mut assets, Uuid(13_000), Uuid(13_002), &material);

        let job = build_thumbnail_job(&mut assets, Uuid(13_002), 64).expect("job");
        let ThumbnailContent::Material {
            material: loaded, ..
        } = job.content
        else {
            panic!("material thumbnail content");
        };
        assert_eq!(loaded.base_color, material.base_color);
        assert_eq!(loaded.unlit, material.unlit);
    }

    #[test]
    fn worker_decodes_uploads_and_drains_into_the_mesh_cache() {
        let Some(gpu) = gpu_or_skip() else { return };
        let root = temp_root("drain");
        let mut assets = AssetServer::new(&root);
        put_mesh_row(&mut assets, Uuid(5000));

        assets.start_thumbnail_worker(Box::new(gpu.seam(false)));
        let reply = request_thumbnail(&mut assets, &gpu.seam(false), Uuid(5000), 64).expect("req");
        assert!(reply.pending, "the worker path replies pending");

        let worker = assets.thumbnail_worker.as_ref().expect("worker");
        assert!(
            wait_until(worker, |s| !s.mesh_handback.is_empty()),
            "the worker uploads + hands back the mesh"
        );
        assert_eq!(gpu.binds.load(Ordering::SeqCst), 1, "bound its pool once");
        assert!(gpu.mesh_uploads.load(Ordering::SeqCst) >= 1);
        assert!(gpu.renders.load(Ordering::SeqCst) >= 1);

        assets.drain_thumbnail_completions();
        assert!(
            assets.mesh_by_uuid.get(&5000).is_some_and(Option::is_some),
            "the drained Arc lands in the mesh cache"
        );

        assets.stop_thumbnail_worker();
        gpu.teardown(assets);
    }

    #[test]
    fn enqueuing_the_same_cache_path_twice_yields_one_cache_file() {
        let Some(gpu) = gpu_or_skip() else { return };
        let root = temp_root("dedup");
        let mut assets = AssetServer::new(&root);
        put_mesh_row(&mut assets, Uuid(6000));

        assets.start_thumbnail_worker(Box::new(gpu.seam(false)));
        let r1 = request_thumbnail(&mut assets, &gpu.seam(false), Uuid(6000), 64).expect("r1");
        assert!(r1.pending);
        let r2 = request_thumbnail(&mut assets, &gpu.seam(false), Uuid(6000), 64).expect("r2");
        assert!(
            r2.pending || !r2.png.is_empty(),
            "deduped pending or already cached"
        );

        let worker = assets.thumbnail_worker.as_ref().expect("worker");
        assert!(wait_until(worker, |s| s.in_flight.is_empty()));
        let count = std::fs::read_dir(assets.thumbnail_cache_dir())
            .map(|d| d.flatten().count())
            .unwrap_or(0);
        assert_eq!(count, 1, "exactly one cache file for one dedup'd job");

        assets.stop_thumbnail_worker();
        gpu.teardown(assets);
    }

    #[test]
    fn a_failing_job_marks_failed_and_is_not_retried() {
        let Some(gpu) = gpu_or_skip() else { return };
        let root = temp_root("fail");
        let mut assets = AssetServer::new(&root);
        put_mesh_row(&mut assets, Uuid(7000));

        assets.start_thumbnail_worker(Box::new(gpu.seam(true)));
        let r1 = request_thumbnail(&mut assets, &gpu.seam(true), Uuid(7000), 64).expect("r1");
        assert!(r1.pending);

        let worker = assets.thumbnail_worker.as_ref().expect("worker");
        assert!(
            wait_until(worker, |s| !s.failed.is_empty()),
            "the failing job marks the cache path failed"
        );
        let renders_after_fail = gpu.renders.load(Ordering::SeqCst);

        let r2 = request_thumbnail(&mut assets, &gpu.seam(true), Uuid(7000), 64);
        assert!(r2.is_err(), "a failed cache key is not retried");
        assert_eq!(
            gpu.renders.load(Ordering::SeqCst),
            renders_after_fail,
            "no re-render after the failure"
        );

        assets.stop_thumbnail_worker();
        gpu.teardown(assets);
    }

    #[test]
    fn stop_joins_before_a_recorded_wait_gpu_idle() {
        let Some(gpu) = gpu_or_skip() else { return };
        let root = temp_root("stop");
        let mut assets = AssetServer::new(&root);
        assets.start_thumbnail_worker(Box::new(gpu.seam(false)));
        assert!(assets.thumbnail_worker.is_some());

        // The host teardown order: stop (join) BEFORE wait_gpu_idle. Record both.
        let (tx, rx) = mpsc::channel::<&'static str>();
        assets.stop_thumbnail_worker();
        tx.send("stop").expect("send");
        tx.send("wait_gpu_idle").expect("send");
        assert_eq!(rx.recv().unwrap(), "stop");
        assert_eq!(rx.recv().unwrap(), "wait_gpu_idle");
        assert!(assets.thumbnail_worker.is_none(), "worker joined + dropped");

        // A redundant stop is a no-op (no panic / deadlock).
        assets.stop_thumbnail_worker();
        gpu.teardown(assets);
    }

    #[test]
    fn clear_thumbnail_queue_empties_queue_dedup_and_handbacks() {
        let Some(gpu) = gpu_or_skip() else { return };
        let root = temp_root("clear");
        let mut assets = AssetServer::new(&root);
        put_mesh_row(&mut assets, Uuid(9100));
        assets.start_thumbnail_worker(Box::new(gpu.seam(false)));

        // Produce a real handback Arc on the main thread (a single live seam, kept alive
        // for the test), and a queued job — without ever waking the worker, so the worker
        // thread never races a GPU submit. Then signal stop and let the worker exit before
        // we seed/clear, so `clear_thumbnail_queue` operates against a quiescent worker.
        let inline = gpu.seam(false);
        let mut tex_out = Vec::new();
        let mut mesh_out = Vec::new();
        let job = build_thumbnail_job(&mut assets, Uuid(9100), 32).expect("job");
        let _ = generate_thumbnail(&inline, &job, &mut tex_out, &mut mesh_out).expect("gen");

        // Park the worker thread (stop without taking it from the server), then seed every
        // bucket and clear — no live consumer, no GPU race.
        let worker = assets.thumbnail_worker.as_ref().expect("worker");
        {
            let (lock, cv) = &*worker.shared;
            lock.lock().expect("mutex").stop = true;
            cv.notify_all();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !worker.handle.as_ref().is_some_and(JoinHandle::is_finished) {
            assert!(std::time::Instant::now() < deadline, "worker exits on stop");
            std::thread::yield_now();
        }
        {
            let (lock, _cv) = &*worker.shared;
            let mut state = lock.lock().expect("mutex");
            state.in_flight.insert("a".to_owned());
            state.failed.insert("b".to_owned());
            state.queue.push_back(job);
            state.mesh_handback.append(&mut mesh_out);
        }

        assets.clear_thumbnail_queue();

        {
            let worker = assets.thumbnail_worker.as_ref().expect("worker");
            let (lock, _cv) = &*worker.shared;
            let state = lock.lock().expect("mutex");
            assert!(state.queue.is_empty());
            assert!(state.in_flight.is_empty());
            assert!(state.failed.is_empty());
            assert!(state.texture_handback.is_empty());
            assert!(
                state.mesh_handback.is_empty(),
                "the un-drained handback is dropped"
            );
        }
        drop(inline);

        assets.stop_thumbnail_worker();
        gpu.teardown(assets);
    }

    #[test]
    fn sync_fallback_generates_inline_when_no_worker() {
        let Some(gpu) = gpu_or_skip() else { return };
        let root = temp_root("sync");
        let mut assets = AssetServer::new(&root);
        put_mesh_row(&mut assets, Uuid(8000));

        // No worker started: the request generates inline and returns the PNG directly.
        let reply = request_thumbnail(&mut assets, &gpu.seam(false), Uuid(8000), 32).expect("req");
        assert!(
            !reply.pending,
            "the sync fallback returns the result directly"
        );
        assert!(!reply.png.is_empty());
        assert_eq!(reply.width, 32);
        assert!(assets.mesh_by_uuid.get(&8000).is_some_and(Option::is_some));

        gpu.teardown(assets);
    }

    #[test]
    fn handback_arcs_are_send_and_shared_state_is_send_sync() {
        // Compile-time assertions: the handback Arc types cross the thread boundary, and
        // the shared worker state moves into the spawned thread (Send) + is reachable from
        // both threads (Sync). These hold without a GPU.
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<Arc<GpuTexture>>();
        assert_send::<Arc<GpuMesh>>();
        assert_send::<Arc<(Mutex<WorkerState>, Condvar)>>();
        assert_sync::<Arc<(Mutex<WorkerState>, Condvar)>>();
    }

    #[test]
    fn material_stamp_changes_with_resolved_params() {
        // No GPU needed: a pure CPU hash over the resolved material.
        let mut a = crate::material::default_material_asset();
        let s1 = thumbnail_material_stamp(&a);
        a.metallic = 0.5;
        let s2 = thumbnail_material_stamp(&a);
        assert_ne!(
            s1, s2,
            "a param change retires the cached material thumbnail"
        );
        a.albedo_texture = Uuid(1234);
        let s3 = thumbnail_material_stamp(&a);
        assert_ne!(s2, s3, "a texture id change moves the stamp");
    }

    #[test]
    fn cache_stats_count_files_and_sweep_drops_orphans() {
        let root = temp_root("stats");
        let mut assets = AssetServer::new(&root);
        let dir = assets.thumbnail_cache_dir();
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("100-64-abc.png"), [0u8; 30]).expect("png1");
        std::fs::write(dir.join("200-64-def.png"), [0u8; 30]).expect("png2");

        let stats = assets.thumbnail_cache_stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.bytes, 60);

        assets.catalog.put(saffron_scene::AssetEntry {
            id: Uuid(200),
            name: "k".to_owned(),
            asset_type: AssetType::Mesh,
            ..Default::default()
        });
        assets.sweep_thumbnail_cache_orphans();
        assert!(!dir.join("100-64-abc.png").exists(), "the orphan is swept");
        assert!(
            dir.join("200-64-def.png").exists(),
            "the catalog'd file is kept"
        );
    }
}
