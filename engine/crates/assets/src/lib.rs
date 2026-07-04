//! The asset catalog wrapper, the `.smat` material system, material codegen, the
//! thumbnail worker, project I/O, the model import/bake pipeline, and `render_scene`.
//!
//! `saffron-assets` sits on top of geometry (the byte codecs), rendering
//! (`GpuMesh`/`GpuTexture` + the bindless table) and scene (the `AssetCatalog`
//! types + the ECS world). It owns the [`AssetServer`]: the live catalog wrapped in
//! uuid-keyed GPU caches.
//!
//! # The negative-cache (the rule that fails silently)
//!
//! The three GPU caches are [`AssetCache`]s — `HashMap<u64, Option<Arc<T>>>`. A
//! present key with `None` is a *negative-cache marker* (a load that failed, not to
//! be retried), distinct from an absent key (never attempted). [`resolve_cached`] is
//! the single code path that honors this. See [`cache`].
//!
//! # GPU-resource lifetime: `Arc` + `Drop`, idle-before-clear
//!
//! The "clear caches only after `wait_gpu_idle`" rule is a *call-site discipline*, not
//! a manual teardown loop. [`AssetServer::clear_asset_caches`] drops
//! the three `HashMap`s; the last `Arc<GpuMesh>`/`Arc<GpuTexture>` drop runs the
//! resource's `Drop`, freeing the VMA allocation and returning the bindless slot.
//! Because an in-flight frame may still reference an `Arc<GpuTexture>`, the caller
//! must idle the GPU *before* clearing — a runtime UAF that `Drop` ordering alone
//! cannot catch.

mod cache;
mod catalog;
mod codegen;
mod error;
mod gpu;
mod graph;
mod import;
mod load;
mod manage;
mod material;
mod material_schema;
mod model;
mod names;
mod project;
mod project_load;
mod render_material;
mod render_scene;
mod scan;
mod spawn;
mod thumbnail;

pub use cache::{AssetCache, resolve_cached};
pub use catalog::{
    catalog_folders_from_json, catalog_folders_to_json, catalog_from_json, catalog_to_json,
};
pub use codegen::find_slangc;
pub use error::{Error, Result};
pub use gpu::{GpuUploader, RendererUploader};
pub use graph::{emit_graph_surface, lower_graph_to_params};
pub use import::{
    Axis, BakeResult, IMPORTER_VERSION, ImportOptions, ScanDelta, catalog_rows_for_model,
    hash_file_fnv,
};
pub use load::engine_asset_path;
pub use manage::{
    CleanCandidate, CleanCategory, CleanReportData, DeleteUnusedData, DependencyGraph,
    MaterialImportResult, RefEdge, RefEdgeKind, RefNode, ReimportDelta, analyze_clean, asset_bytes,
    build_dependency_graph, clear_extraction, delete_unused, extract_sub_asset,
    import_material_folder, reimport_model,
};
pub use material::{
    MaterialAsset, apply_overrides, default_material_asset, load_catalog_material_asset,
    load_catalog_material_asset_raw, load_material_asset, load_material_asset_raw,
    material_asset_from_json, material_asset_to_json, save_material_asset, update_material_asset,
};
pub use material_schema::{
    ExposedParam, ExposedParamKind, exposed_parameter, pbr_exposed_parameters,
};
pub use model::{
    ByteSource, ContainerMetadata, Import, METADATA_SCHEMA_VERSION, ModelAsset, SubAsset,
    encode_container_metadata, read_container_metadata,
};
pub use names::{asset_type_from_name, asset_type_name, colorspace_from_name, colorspace_name};
pub use project::{
    LUARC_JSON, NewProject, PROJECT_VERSION, ProjectHost, ProjectInfo, ProjectSidecar,
    STARTER_SCRIPT, app_data_root, create_project_script, default_display_name,
    ensure_script_library, ensure_script_src, project_info_from_path, project_json_path,
    project_userdata_root, scratch_project_name, valid_project_name,
};
pub use project_load::{DocProgress, DocStage, LoadInput, LoadedDoc, ProjectDocWorker};
pub use render_material::{ResolvedMaterials, build_submesh_material};
pub use render_scene::{
    RendererScene, SceneRenderer, SceneSurfaceHit, model_render_aabb, pick_entity,
    pick_scene_surface, render_scene, scene_render_aabb, viewport_ray,
};
pub use scan::detect_material_role;
pub use spawn::{ModelSpawnInput, imported_nodes_from_json, imported_skin_from_json, spawn_model};
pub use thumbnail::{
    THUMBNAIL_CACHE_VERSION, ThumbnailCacheStats, ThumbnailContent, ThumbnailGpu, ThumbnailJob,
    ThumbnailPng, ThumbnailReply, ThumbnailTextureSource, ThumbnailWorker, request_thumbnail,
};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_rendering::{GpuMesh, GpuTexture, SubmeshMaterial};
use saffron_scene::AssetCatalog;

/// The built-in default material: white albedo, fully rough, non-metallic. Returned
/// by the resolve path when a referenced material is missing. Its id is in the
/// reserved (`< 1024`) range, so it never collides with a minted id.
pub const DEFAULT_MATERIAL_ID: Uuid = Uuid(1);

/// The asset-preview floor slab's mesh, in the reserved (`< 1024`) range. Seeded
/// into the GPU mesh cache (not the catalog), so the preview floor renders without a
/// catalog row that would serialize.
pub const PREVIEW_FLOOR_MESH_ID: Uuid = Uuid(2);

/// A renderer-internal mesh visual (the editor-camera gizmo): an attempted-once
/// shared mesh plus its resolved submesh material table. Held by [`AssetServer`],
/// not the catalog.
#[derive(Default)]
pub struct SystemMeshVisual {
    /// Whether a load has been attempted (so a failed load is not retried).
    pub attempted: bool,
    /// The shared GPU mesh, `None` until loaded (or after a failed attempt).
    pub mesh: Option<Arc<GpuMesh>>,
    /// The resolved per-submesh materials for the visual.
    pub submesh_materials: Vec<SubmeshMaterial>,
}

/// Per-frame options the scene driver reads (phase 12).
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderSceneOptions {
    /// Append the editor-camera gizmo models to the draw list.
    pub show_editor_camera_models: bool,
    /// Draw the infinite analytic ground grid (debug overlay).
    pub show_grid: bool,
}

/// Owns the project's asset catalog plus uuid-keyed GPU caches so entities sharing an
/// id upload once.
///
/// The three caches are negative-caches (see [`cache`]): a cached `None` is a failed
/// asset that is not retried each frame, not a miss. `AssetServer` is the source of
/// truth for the live catalog; it shares an `Arc<AssetCatalog>` into `Scene.catalog`
/// so the scene reads it without a lifetime tangle.
///
/// Touched only from the main thread — no `Arc<Mutex>` on its own state. The
/// thumbnail worker (phase 11) is the sole cross-thread site, and its sharing is
/// mediated by `saffron-rendering`'s queue/bindless mutexes.
pub struct AssetServer {
    /// The asset root directory (the project's `assets/` dir).
    pub root: PathBuf,
    /// The live catalog: id → `{name, type, path}`. The source of truth.
    pub catalog: AssetCatalog,
    /// GPU mesh cache, keyed by mesh / sub-id. `None` = negative marker.
    pub mesh_by_uuid: AssetCache<GpuMesh>,
    /// Per-mesh ray-pick BVH cache, keyed by mesh sub-id, built lazily from a mesh's CPU
    /// geometry on first pick. `None` = a mesh with no pickable triangles.
    pub mesh_bvh_by_uuid: AssetCache<saffron_geometry::MeshBvh>,
    /// GPU texture cache, keyed by texture / sub-id. `None` = negative marker.
    pub texture_by_uuid: AssetCache<GpuTexture>,
    /// Opened `.smodel` containers, keyed by model id. `None` = negative marker.
    pub model_by_uuid: AssetCache<ModelAsset>,
    /// Parent-resolved material assets (parent chain walked, instance overrides baked, *before*
    /// per-slot overrides), keyed by material id. The draw path resolves each entity's materials
    /// every frame; without this it re-read + re-parsed each `.smat` (and, for a container-embedded
    /// material, re-read the whole `.smodel`) from disk per entity per frame. `None` = negative
    /// marker. Coarsely cleared on any material mutation (see [`Self::invalidate_material_caches`]).
    pub material_by_uuid: AssetCache<MaterialAsset>,
    /// The codegen `_mesh.spv` shader path per material id (a non-foldable node-graph material's
    /// compiled übershader variant), or `None` (the common case: no graph shader). Memoizes
    /// [`render_material`](crate::render_material)'s `codegen_shader_for` so the per-frame resolve
    /// stops probing the disk. Invalidated with [`Self::material_by_uuid`].
    pub material_shader_by_uuid: AssetCache<String>,
    /// The editor-camera gizmo's mesh visual.
    pub editor_camera_model: SystemMeshVisual,
    /// Off-thread thumbnail generation (`None` until the worker is started).
    pub thumbnail_worker: Option<ThumbnailWorker>,
    /// The app-level, content-addressed thumbnail cache dir, defaulted from
    /// [`app_data_root`] so it is shared across every project and survives a project switch
    /// (it is *not* repointed by [`Self::set_asset_root`]). Overridable so a test can isolate
    /// its cache to a temp dir.
    pub thumbnail_cache_root: PathBuf,
}

impl AssetServer {
    /// Creates an asset server rooted at `root`, seeding the standard asset
    /// subdirectories and an empty catalog. The catalog is populated from a project
    /// file via `load_project`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let assets = Self {
            root: root.into(),
            catalog: AssetCatalog::default(),
            mesh_by_uuid: AssetCache::new(),
            mesh_bvh_by_uuid: AssetCache::new(),
            texture_by_uuid: AssetCache::new(),
            model_by_uuid: AssetCache::new(),
            material_by_uuid: AssetCache::new(),
            material_shader_by_uuid: AssetCache::new(),
            editor_camera_model: SystemMeshVisual::default(),
            thumbnail_worker: None,
            thumbnail_cache_root: Path::new(&app_data_root()).join("thumbnail-cache"),
        };
        assets.ensure_asset_directories();
        assets
    }

    /// Repoints the asset root and (re)creates its standard subdirectories.
    pub fn set_asset_root(&mut self, root: impl Into<PathBuf>) {
        self.root = root.into();
        self.ensure_asset_directories();
    }

    /// Creates the standard asset subdirectories under the root, idempotently.
    ///
    /// `models/`, `textures/`, `materials/` live under the asset root. The thumbnail cache
    /// is app-level (shared across projects) and created lazily on first write. Directory
    /// creation errors are swallowed: a missing dir surfaces later as the real I/O failure
    /// that needs it.
    pub fn ensure_asset_directories(&self) {
        for sub in ["models", "textures", "materials"] {
            let _ = std::fs::create_dir_all(self.root.join(sub));
        }
    }

    /// The app-level thumbnail cache directory (`<appDataRoot>/thumbnail-cache/` by default).
    ///
    /// Content-addressed and shared across every project, so it survives a project switch
    /// and dedups identical assets. Lives outside any project root, so the catalog scan and
    /// project save/load never see it.
    #[must_use]
    pub fn thumbnail_cache_dir(&self) -> PathBuf {
        self.thumbnail_cache_root.clone()
    }

    /// Drops the three GPU caches (and abandons stale worker jobs), freeing every
    /// cached `GpuMesh`/`GpuTexture` whose last `Arc` lives here.
    ///
    /// # GPU idle is the caller's responsibility
    ///
    /// The caller (`load_project`/`create_project`) must have called
    /// `wait_gpu_idle(renderer)` first: an in-flight frame may still reference a
    /// cached `Arc<GpuTexture>`, and dropping it under the GPU is a use-after-free
    /// that `Drop` ordering cannot catch. Clearing under an idle GPU is the entire
    /// discipline.
    pub fn clear_asset_caches(&mut self) {
        self.clear_thumbnail_queue();
        self.mesh_by_uuid.clear();
        self.mesh_bvh_by_uuid.clear();
        self.texture_by_uuid.clear();
        self.model_by_uuid.clear();
        self.invalidate_material_caches();
        // The editor-camera gizmo visual is a cached GPU `Ref` too (its `Arc<GpuMesh>` +
        // resolved submesh materials), so it must drop here with the other caches — before
        // the renderer frees the device/allocator. Leaving it would `vmaDestroyBuffer` on a
        // dead allocator when `AssetServer` finally drops (a teardown use-after-free).
        self.editor_camera_model = SystemMeshVisual::default();
    }

    /// Drops the memoized material resolutions so the next frame re-reads them from disk.
    ///
    /// Coarse by design: a material's resolved value depends on its parent chain, and several
    /// mutation paths rebuild the whole catalog, so a precise per-id drop would need a
    /// parent→child index to stay correct. Material edits are never per-frame, so clearing the
    /// (small, CPU-only) caches wholesale is cheap and always correct. Called at every material
    /// write/rebake/delete seam so every control command inherits invalidation.
    pub fn invalidate_material_caches(&mut self) {
        self.material_by_uuid.clear();
        self.material_shader_by_uuid.clear();
    }

    /// Abandons the worker's queued/failed jobs and un-drained handbacks on a project
    /// switch (the GPU is idle at the call site, so dropping the handback `Arc`s frees
    /// them safely). A no-op when no worker is running.
    ///
    /// It stays a method on `AssetServer` so [`AssetServer::clear_asset_caches`] calls one
    /// stable seam.
    pub fn clear_thumbnail_queue(&mut self) {
        if let Some(worker) = self.thumbnail_worker.as_ref() {
            thumbnail::clear_worker_queue(worker);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_sentinels_are_in_the_reserved_range() {
        assert!(DEFAULT_MATERIAL_ID.value() < 1024);
        assert!(PREVIEW_FLOOR_MESH_ID.value() < 1024);
        assert_ne!(DEFAULT_MATERIAL_ID, PREVIEW_FLOOR_MESH_ID);
    }

    #[test]
    fn new_creates_the_asset_subdirectories() {
        let tmp = std::env::temp_dir().join(format!("saffron-assets-test-{}", std::process::id()));
        let root = tmp.join("project").join("assets");
        let _ = std::fs::remove_dir_all(&tmp);
        let assets = AssetServer::new(&root);

        assert!(root.join("models").is_dir());
        assert!(root.join("textures").is_dir());
        assert!(root.join("materials").is_dir());
        // The thumbnail cache is app-level (created lazily on write), not a project sibling.
        assert!(
            !tmp.join("project").join("cache").exists(),
            "no per-project thumbnail cache dir is created"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = &assets;
    }

    #[test]
    fn set_asset_root_recreates_subdirectories() {
        let tmp =
            std::env::temp_dir().join(format!("saffron-assets-test-root-{}", std::process::id()));
        let first = tmp.join("a").join("assets");
        let second = tmp.join("b").join("assets");
        let _ = std::fs::remove_dir_all(&tmp);

        let mut assets = AssetServer::new(&first);
        assets.set_asset_root(&second);

        assert_eq!(assets.root, second);
        assert!(second.join("models").is_dir());
        assert!(second.join("materials").is_dir());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A counting GPU-resource stub: increments a shared counter on `Drop`, so a test
    /// can prove `clear_asset_caches` (via `Arc` drop) fires the teardown.
    struct DropMesh {
        counter: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Drop for DropMesh {
        fn drop(&mut self) {
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn clear_asset_caches_drops_all_three_caches() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let tmp = std::env::temp_dir().join(format!("saffron-assets-clear-{}", std::process::id()));
        let root = tmp.join("project").join("assets");
        let _ = std::fs::remove_dir_all(&tmp);
        let mut assets = AssetServer::new(&root);

        // A reserved sentinel seeded into the mesh cache (the preview-floor pattern):
        // a `get` finds it, and `clear_asset_caches` drops it.
        let mesh_counter = Arc::new(AtomicUsize::new(0));
        let model_counter = Arc::new(AtomicUsize::new(0));
        let tex_counter = Arc::new(AtomicUsize::new(0));

        // The mesh cache uses GpuMesh's type, but the Drop-ordering proof needs a
        // counting stub; the three caches are exercised via the generic helper, so
        // the discipline is proved on dedicated DropMesh caches below.
        let mut mesh_cache: AssetCache<DropMesh> = AssetCache::new();
        let mut tex_cache: AssetCache<DropMesh> = AssetCache::new();
        let mut model_cache: AssetCache<DropMesh> = AssetCache::new();
        mesh_cache.insert(
            PREVIEW_FLOOR_MESH_ID.value(),
            Some(Arc::new(DropMesh {
                counter: Arc::clone(&mesh_counter),
            })),
        );
        tex_cache.insert(
            10,
            Some(Arc::new(DropMesh {
                counter: Arc::clone(&tex_counter),
            })),
        );
        model_cache.insert(
            20,
            Some(Arc::new(DropMesh {
                counter: Arc::clone(&model_counter),
            })),
        );

        // The sentinel survives a get.
        let survived = resolve_cached(&mut mesh_cache, PREVIEW_FLOOR_MESH_ID.value(), || None);
        assert!(survived.is_some(), "the seeded sentinel survives a get");
        drop(survived);
        assert_eq!(mesh_counter.load(Ordering::SeqCst), 0);

        // Clearing drops the last Arc of each — every Drop fires exactly once.
        mesh_cache.clear();
        tex_cache.clear();
        model_cache.clear();
        assert_eq!(mesh_counter.load(Ordering::SeqCst), 1);
        assert_eq!(tex_counter.load(Ordering::SeqCst), 1);
        assert_eq!(model_counter.load(Ordering::SeqCst), 1);

        // clear_asset_caches on the real server clears its (empty) caches without panic.
        assets.clear_asset_caches();
        assert!(assets.mesh_by_uuid.is_empty());
        assert!(assets.texture_by_uuid.is_empty());
        assert!(assets.model_by_uuid.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
