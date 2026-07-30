//! The asset catalog wrapper, the `.smat` material system, material codegen, the
//! thumbnail worker, project I/O, the model import/bake pipeline, and `render_scene`.
//!
//! [`AssetServer`] owns the live catalog wrapped in uuid-keyed GPU caches. Two invariants the
//! type system cannot enforce:
//!
//! - The GPU caches are [`AssetCache`]s — `HashMap<u64, Option<Arc<T>>>` — where a present key
//!   holding `None` means "this load failed, do not retry" and an absent key means "never
//!   attempted". [`resolve_cached`] is the single path that honours the distinction.
//! - The caller must `wait_gpu_idle` before [`AssetServer::clear_asset_caches`]: clearing drops
//!   the last `Arc<GpuMesh>`/`Arc<GpuTexture>`, whose `Drop` frees the VMA allocation and returns
//!   the bindless slot, and an in-flight frame may still reference it.

mod atlas;
mod cache;
mod catalog;
mod codegen;
mod cook_reader;
mod coverage;
mod cube;
mod environment_profile;
mod error;
mod gpu;
mod gpu_scene_mirror;
mod graph;
mod import;
mod journal;
mod load;
mod manage;
mod material;
mod material_schema;
mod mesh_surface;
mod model;
mod names;
mod page_stream;
mod plant_cook;
mod plant_render;
mod project;
mod project_load;
mod render_material;
mod render_scene;
mod scan;
mod spawn;
mod thumbnail;
mod time_of_day;
mod vegetation;
mod vegetation_cooker;
mod vegetation_export;
mod vegetation_state;
mod vegetation_store;

pub use atlas::{
    AtlasLayout, AtlasPlacement, DEFAULT_ATLAS_GUTTER, FamilyAtlas, FamilySlotImage,
    generate_family_atlas, pack_atlas,
};
pub use cache::{AssetCache, resolve_cached};
pub use catalog::{
    catalog_folders_from_json, catalog_folders_to_json, catalog_from_json, catalog_to_json,
};
pub use codegen::find_slangc;
pub use cook_reader::{AuthoredInputGuard, CookProjectView};
pub use coverage::{CoverageMip, coverage_preserving_mips};
pub use cube::{BakedLut, CubeError, CubeLut, parse_cube};
pub use environment_profile::{
    BuiltinEnvironmentProfile, builtin_environment_profile, builtin_environment_profiles,
    load_environment_profile, save_environment_profile, update_environment_profile,
};
pub use error::{Error, Result};
pub use gpu::{GpuUploader, RendererUploader};
pub use gpu_scene_mirror::{
    GpuSceneMirror, GpuSceneMirrorStats, GpuSceneMirrorTarget, VegetationBudgets,
    VegetationCellRenderRow, VegetationFamilyRenderRow, VegetationRenderBreakdown,
};
pub use graph::{emit_graph_surface, lower_graph_to_params};
pub use import::{
    Axis, BakeResult, IMPORTER_VERSION, ImportOptions, ScanDelta, catalog_rows_for_container,
    hash_file_fnv,
};
pub use journal::{
    AssetCatalogSnapshot, AssetInvalidations, AssetJournalCursor, AssetJournalRead, AssetMutation,
    AssetMutationKind, AssetMutationTarget, AssetRevision,
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
    material_asset_from_json, material_asset_to_json, material_asset_to_text, save_material_asset,
    update_material_asset,
};
pub use material_schema::{
    ExposedParam, ExposedParamKind, exposed_parameter, pbr_exposed_parameters,
};
pub use mesh_surface::{CanonicalCpuCoverage, StaticMeshSurfaceInput, StaticMeshSurfaceProvider};
pub use model::{
    ByteSource, ContainerMetadata, Import, METADATA_SCHEMA_VERSION, ModelAsset, SubAsset,
    encode_container_metadata, read_container_metadata,
};
pub use names::{
    asset_type_from_name, asset_type_name, colorspace_from_name, colorspace_name,
    texture_role_from_name, texture_role_name,
};
pub use page_stream::{PageLoadRequest, PageLoadResult, PagePayloadSource, PageStreamWorker};
pub use plant_cook::{
    PlantModules, PlantRecookOptions, PlantRecookOutcome, PlantValidationOutcome,
    PreparedPlantFamily, PublishedPlantRecook, prepare_plant_family_sources, recook_plant_family,
    validate_plant_family_sources,
};
pub use plant_render::{
    PlantAtlasImage, PlantFamilyRender, PlantPhenotypeRender, plant_family_atlas_image,
    plant_family_hierarchy,
};
pub use project::{
    LUARC_JSON, NewProject, PROJECT_VERSION, ProjectHost, ProjectInfo, ProjectSidecar,
    STARTER_SCRIPT, app_data_root, create_project_script, default_display_name,
    ensure_script_library, ensure_script_src, project_info_from_path, project_json_path,
    project_userdata_root, scratch_project_name, valid_project_name,
};
pub use project_load::{DocProgress, DocStage, LoadInput, LoadedDoc, ProjectDocWorker};
pub use render_material::{ResolvedMaterials, build_submesh_material};
pub use render_scene::{
    RendererScene, SceneRenderer, SceneSurfaceHit, SceneSurfaceProvider, model_render_aabb,
    pick_entity, pick_scene_surface, query_scene_surface_ray, render_scene,
    sample_scene_surface_field, scene_render_aabb, scene_surface_field_snapshots,
    scene_surface_providers, viewport_pick_ray, viewport_ray,
};
pub use scan::{
    colorspace_for_role_explicit, detect_height_mode, detect_material_role, infer_texture_role,
    texture_role_from_hint,
};
pub use spawn::{ModelSpawnInput, imported_nodes_from_json, imported_skin_from_json, spawn_model};
pub use thumbnail::{
    PreviewRenderJob, PreviewRenderKind, THUMBNAIL_CACHE_VERSION, ThumbnailCacheStats,
    ThumbnailContent, ThumbnailJob, ThumbnailPng, ThumbnailReply, ThumbnailTextureSource,
    request_thumbnail, write_thumbnail_cache,
};
pub use time_of_day::{
    CelestialPosition, CelestialTime, advance_time_of_day, dir_from_az_el, eval_monotone_curve,
    julian_date, local_sidereal_time, lunar_position, solar_position, world_from_equatorial,
};
pub use vegetation::{
    CatalogBiomeGraphResolver, ResolvedBiomeGraph, VegetationImport, VegetationMapTransaction,
    assemble_biome_graph_evaluation_job, commit_vegetation_map_transaction,
    compile_catalog_biome_graph, compile_catalog_biome_instance_graph, import_vegetation_asset,
    load_biome_asset, load_plant_family_asset, load_vegetation_map_chunks,
    load_vegetation_map_root, load_vegetation_map_snapshot, load_vegetation_map_tile_snapshot,
    remove_vegetation_map_package, save_biome_asset, save_plant_family_asset,
    save_vegetation_map_asset, update_biome_asset, update_plant_family_asset,
    update_vegetation_map_asset, vegetation_graph_dependency_hashes,
};
pub use vegetation_cooker::{
    PlantSourceAcceptance, StagedVegetationCook, VegetationCookEvent, VegetationCookOutput,
    VegetationCookRequest, VegetationCookStatistics, commit_staged_vegetation_cook,
    portable_vegetation_platform_profile, stage_vegetation_cook, vegetation_cook_versions,
};
pub use vegetation_export::{
    VegetationArtifactFault, VegetationExportClosure, VegetationExportFacet, VegetationExportFile,
    VegetationExportMap, VegetationFaultKind, VegetationVerifyReport, vegetation_export_closure,
    verify_vegetation_artifacts,
};
pub use vegetation_state::VegetationStateStore;
pub use vegetation_store::{
    VegetationArtifactKind, VegetationArtifactPublication, VegetationArtifactStore,
    VegetationAuthoredLock, VegetationGenerationLock,
};

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_rendering::{GpuMesh, GpuTexture};
use saffron_scene::AssetCatalog;

use crate::journal::invalidations_for;

const DEFAULT_ASSET_JOURNAL_CAPACITY: usize = 4096;

/// The built-in default material: white albedo, fully rough, non-metallic. Returned
/// by the resolve path when a referenced material is missing. Its id is in the
/// reserved (`< 1024`) range, so it never collides with a minted id.
pub const DEFAULT_MATERIAL_ID: Uuid = Uuid(1);

/// The asset-preview floor slab's mesh, in the reserved (`< 1024`) range. Seeded
/// into the GPU mesh cache (not the catalog), so the preview floor renders without a
/// catalog row that would serialize.
pub const PREVIEW_FLOOR_MESH_ID: Uuid = Uuid(2);

/// The built-in cube primitive's mesh id, in the reserved (`< 1024`) range.
pub const BUILTIN_CUBE_MESH_ID: Uuid = Uuid(3);
/// The built-in plane primitive's mesh id, in the reserved (`< 1024`) range.
pub const BUILTIN_PLANE_MESH_ID: Uuid = Uuid(4);
/// The built-in sphere primitive's mesh id, in the reserved (`< 1024`) range.
pub const BUILTIN_SPHERE_MESH_ID: Uuid = Uuid(5);

/// The reserved (`< 1024`) id of the ephemeral single-slot material the texture preview seeds
/// into `material_by_uuid` to shade the preview sphere. Never a catalog row: it exists only
/// while a standalone texture is open in the interactive preview and is rebuilt on each enter.
pub const PREVIEW_MATERIAL_ID: Uuid = Uuid(6);

/// The reserved (`< 1024`) id of the ephemeral single-slot material a **background thumbnail**
/// render seeds for a texture-role subject — distinct from [`PREVIEW_MATERIAL_ID`] so a thumbnail
/// rendered on the offscreen Thumbnail view can never overwrite the slot an interactive texture
/// preview is using. Never a catalog row.
pub const PREVIEW_THUMBNAIL_MATERIAL_ID: Uuid = Uuid(8);

/// The editor-camera gizmo model's mesh, in the reserved (`< 1024`) range. Seeded into the
/// GPU mesh cache from the engine's `models/editor-camera.glb` on demand (never a catalog
/// row), so a camera's runtime gizmo ghost references it like any mesh asset.
pub const EDITOR_CAMERA_MESH_ID: Uuid = Uuid(9);

/// The editor-camera gizmo model's material, in the reserved (`< 1024`) range. Seeded
/// beside the mesh; never a catalog row.
pub const EDITOR_CAMERA_MATERIAL_ID: Uuid = Uuid(10);

/// A native built-in primitive mesh — geometry the engine generates itself, referenced by
/// a reserved id and seeded into the GPU cache on demand. Never a catalog asset: a
/// primitive entity carries the reserved id, which serializes as a stable decimal string
/// and resolves cache-first (see [`AssetServer::load_mesh_asset`]), so it needs no project
/// and pollutes no catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinMesh {
    /// A unit cube (edge 1, `±0.5`).
    Cube,
    /// A unit plane on XZ (`1×1`, facing +Y).
    Plane,
    /// A unit UV sphere (radius 1).
    Sphere,
}

impl BuiltinMesh {
    /// The reserved mesh id a [`saffron_scene::Mesh`] component carries to reference this
    /// primitive.
    pub fn reserved_id(self) -> Uuid {
        match self {
            BuiltinMesh::Cube => BUILTIN_CUBE_MESH_ID,
            BuiltinMesh::Plane => BUILTIN_PLANE_MESH_ID,
            BuiltinMesh::Sphere => BUILTIN_SPHERE_MESH_ID,
        }
    }

    /// The primitive for a reserved id, or `None` for any non-built-in id. The single
    /// `< 1024` → primitive decoder used across the engine.
    pub fn from_reserved_id(id: Uuid) -> Option<Self> {
        match id {
            BUILTIN_CUBE_MESH_ID => Some(BuiltinMesh::Cube),
            BUILTIN_PLANE_MESH_ID => Some(BuiltinMesh::Plane),
            BUILTIN_SPHERE_MESH_ID => Some(BuiltinMesh::Sphere),
            _ => None,
        }
    }

    /// The human label shown in the Inspector's built-in picker group.
    pub fn display_name(self) -> &'static str {
        match self {
            BuiltinMesh::Cube => "Cube",
            BuiltinMesh::Plane => "Plane",
            BuiltinMesh::Sphere => "Sphere",
        }
    }

    /// The generated CPU geometry (position / normal / uv0, one submesh).
    pub fn geometry(self) -> saffron_geometry::Mesh {
        match self {
            BuiltinMesh::Cube => saffron_geometry::cube(),
            BuiltinMesh::Plane => saffron_geometry::plane(),
            BuiltinMesh::Sphere => saffron_geometry::uv_sphere(),
        }
    }
}

/// Per-frame options the scene driver reads.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderSceneOptions {
    /// Keep a runtime gizmo-model ghost under each `show_model` camera.
    pub show_editor_camera_models: bool,
    /// Draw the infinite analytic ground grid (debug overlay).
    pub show_grid: bool,
}

fn project_vegetation_cache_root(asset_root: &Path) -> PathBuf {
    asset_root
        .parent()
        .unwrap_or(asset_root)
        .join("cache")
        .join("vegetation")
}

fn project_vegetation_state_root(asset_root: &Path) -> PathBuf {
    asset_root
        .parent()
        .unwrap_or(asset_root)
        .join("state")
        .join("vegetation")
}

/// Owns the project's asset catalog plus uuid-keyed GPU caches so entities sharing an
/// id upload once. It is the source of truth for the live catalog.
///
/// Touched only from the main thread — no `Arc<Mutex>` on its own state. The thumbnail worker is
/// the sole cross-thread site, and its sharing is mediated by `saffron-rendering`'s
/// queue/bindless mutexes.
pub struct AssetServer {
    /// The asset root directory (the project's `assets/` dir).
    pub root: PathBuf,
    /// The live catalog: id → `{name, type, path}`. The source of truth.
    catalog: AssetCatalog,
    /// GPU mesh cache, keyed by mesh / sub-id. `None` = negative marker.
    mesh_by_uuid: AssetCache<GpuMesh>,
    /// Per-mesh page-payload source (artifact slice or retained cooked hierarchy),
    /// recorded at mesh load for the page-stream worker.
    page_source_by_uuid: std::collections::HashMap<u64, crate::page_stream::PagePayloadSource>,
    /// Each loaded plant family's authored wind and bend response, keyed by family id,
    /// recorded beside its mesh so the mirror's generic prototype path can find it.
    plant_mechanics_by_uuid: std::collections::HashMap<u64, saffron_vegetation::MechanicalResponse>,
    /// Loaded plant-family renders keyed by exact `.splantc` identity. `None` = negative
    /// marker (a failed load, not retried until a cache clear).
    plant_render_by_hash: std::collections::HashMap<
        saffron_vegetation::ContentHash,
        Option<crate::plant_render::PlantFamilyRender>,
    >,
    /// Per-mesh ray-pick BVH cache, keyed by mesh sub-id, built lazily from a mesh's CPU
    /// geometry on first pick. `None` = a mesh with no pickable triangles.
    mesh_bvh_by_uuid: AssetCache<saffron_geometry::MeshBvh>,
    /// GPU texture cache, keyed by texture / sub-id. `None` = negative marker.
    texture_by_uuid: AssetCache<GpuTexture>,
    /// Decoded RGBA8 source pixels used by canonical CPU coverage queries.
    texture_pixels_by_uuid:
        std::collections::HashMap<u64, Option<Arc<saffron_geometry::DecodedImage>>>,
    /// GPU cache of textures resolved as **displacement height maps**, keyed by texture / sub-id. A
    /// separate map so a height map carries its per-height min/max pyramid (built at upload for the
    /// tessellation factor kernel), independent of the same image used as a plain albedo/data texture.
    /// `None` = negative marker.
    height_texture_by_uuid: AssetCache<GpuTexture>,
    /// Coverage-preserving texture variants keyed by `(texture id, reference cutoff bits)`.
    /// Each variant owns an exact CPU-derived mip chain used by every foliage coverage pass.
    coverage_texture_by_uuid: std::collections::HashMap<(u64, u16), Option<Arc<GpuTexture>>>,
    /// GPU creative-LUT cache, keyed by LUT asset id. `None` = negative marker. Holds the `GpuLut`
    /// (a 3D image, no bindless slot) a `.cube` import or baked `.slut` resolves to.
    lut_by_uuid: AssetCache<saffron_rendering::GpuLut>,
    /// Opened `.smodel` containers, keyed by model id. `None` = negative marker.
    model_by_uuid: AssetCache<ModelAsset>,
    /// Parent-resolved material assets (parent chain walked, instance overrides baked, *before*
    /// per-slot overrides), keyed by material id. The draw path resolves each entity's materials
    /// every frame, so this keeps that off the disk. `None` = negative marker. Coarsely cleared on
    /// any material mutation.
    material_by_uuid: AssetCache<MaterialAsset>,
    /// The codegen `_mesh.spv` shader path per material id (a non-foldable node-graph material's
    /// compiled übershader variant), or `None` (the common case: no graph shader). Memoizes
    /// [`render_material`](crate::render_material)'s `codegen_shader_for` so the per-frame resolve
    /// does not probe the disk. Invalidated with [`Self::material_by_uuid`].
    material_shader_by_uuid: AssetCache<String>,
    /// Monotonic invalidation epoch for render-derived asset content.
    ///
    /// Static shadow caches fold this into their content key so a material, texture, or mesh
    /// replacement invalidates cached silhouettes even when catalog ids and transforms are stable.
    render_content_revision: u64,
    asset_revision: AssetRevision,
    asset_journal_base: AssetRevision,
    asset_journal_capacity: usize,
    asset_journal: VecDeque<AssetMutation>,
    /// The app-level, content-addressed thumbnail cache dir, defaulted from
    /// [`app_data_root`] so it is shared across every project and survives a project switch
    /// (it is *not* repointed by [`Self::set_asset_root`]). Overridable so a test can isolate
    /// its cache to a temp dir.
    pub thumbnail_cache_root: PathBuf,
    /// Project-local derived vegetation CAS. It is a sibling of `assets/`, never catalogued or
    /// serialized as authored project state, and disposable: everything under it is reproducible
    /// from authored sources.
    pub vegetation_cache_root: PathBuf,
    /// Project-local durable vegetation state. Also a sibling of `assets/` and not catalogued, but
    /// **not** disposable: a published baseline is a snapshot of runtime mutations that no authored
    /// source reproduces, so it must never share a root with the cache.
    pub vegetation_state_root: PathBuf,
    /// Every thumbnail renders through the **main forward+ graph**, which lives only on the render
    /// thread: [`request_thumbnail`] classifies and enqueues each here, and the host drains them in
    /// `on_update` (build the preview scene → render on the offscreen thumbnail view → write the disk
    /// cache). FIFO.
    pub preview_render_queue: std::collections::VecDeque<crate::thumbnail::PreviewRenderJob>,
    /// The cache paths of preview-render jobs queued or rendering — dedups re-requests while the
    /// editor repolls.
    pub preview_render_in_flight: std::collections::HashSet<String>,
}

impl AssetServer {
    /// Creates an asset server rooted at `root`, seeding the standard asset
    /// subdirectories and an empty catalog. The catalog is populated from a project
    /// file via `load_project`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let vegetation_cache_root = project_vegetation_cache_root(&root);
        let vegetation_state_root = project_vegetation_state_root(&root);
        let assets = Self {
            root,
            catalog: AssetCatalog::default(),
            mesh_by_uuid: AssetCache::new(),
            plant_mechanics_by_uuid: std::collections::HashMap::new(),
            page_source_by_uuid: std::collections::HashMap::new(),
            plant_render_by_hash: std::collections::HashMap::new(),
            mesh_bvh_by_uuid: AssetCache::new(),
            texture_by_uuid: AssetCache::new(),
            texture_pixels_by_uuid: std::collections::HashMap::new(),
            height_texture_by_uuid: AssetCache::new(),
            coverage_texture_by_uuid: std::collections::HashMap::new(),
            lut_by_uuid: AssetCache::new(),
            model_by_uuid: AssetCache::new(),
            material_by_uuid: AssetCache::new(),
            material_shader_by_uuid: AssetCache::new(),
            render_content_revision: 1,
            asset_revision: AssetRevision::ZERO,
            asset_journal_base: AssetRevision::ZERO,
            asset_journal_capacity: DEFAULT_ASSET_JOURNAL_CAPACITY,
            asset_journal: VecDeque::new(),
            thumbnail_cache_root: Path::new(&app_data_root()).join("thumbnail-cache"),
            vegetation_cache_root,
            vegetation_state_root,
            preview_render_queue: std::collections::VecDeque::new(),
            preview_render_in_flight: std::collections::HashSet::new(),
        };
        assets.ensure_asset_directories();
        assets
    }

    /// Immutable live asset catalog.
    #[must_use]
    pub fn catalog(&self) -> &AssetCatalog {
        &self.catalog
    }

    /// Current mutation cursor for an atomically read catalog snapshot.
    #[must_use]
    pub fn asset_journal_cursor(&self) -> AssetJournalCursor {
        AssetJournalCursor::at(self.asset_revision)
    }

    /// Captures the complete catalog and the exact cursor representing it.
    #[must_use]
    pub fn asset_catalog_snapshot(&self) -> AssetCatalogSnapshot {
        AssetCatalogSnapshot {
            catalog: self.catalog.clone(),
            cursor: self.asset_journal_cursor(),
        }
    }

    /// Reads every retained mutation after `cursor`, or requests a complete snapshot rebuild.
    #[must_use]
    pub fn read_asset_journal(&self, cursor: AssetJournalCursor) -> AssetJournalRead {
        let revision = cursor.revision();
        let next = self.asset_journal_cursor();
        if revision < self.asset_journal_base || revision > self.asset_revision {
            return AssetJournalRead::SnapshotRequired { next };
        }
        AssetJournalRead::Delta {
            mutations: self
                .asset_journal
                .iter()
                .copied()
                .filter(|mutation| mutation.revision > revision)
                .collect(),
            next,
        }
    }

    /// Registers one newly imported catalog entry.
    pub fn register_imported_asset(&mut self, entry: saffron_scene::AssetEntry) {
        assert!(
            self.catalog.find(entry.id).is_none(),
            "imported asset id {} already exists",
            entry.id.value()
        );
        let id = entry.id;
        let asset_type = entry.asset_type;
        self.catalog.put(entry);
        // A failed load of this id may have negative-cached before the import existed;
        // the import makes that cached absence stale.
        self.invalidate_loaded_asset(id, asset_type);
        self.record_asset_mutation(
            AssetMutationTarget::Asset { id, asset_type },
            AssetMutationKind::Imported,
            invalidations_for(asset_type),
        );
    }

    /// Inserts or replaces a stable catalog entry produced by reimport.
    pub fn register_reimported_asset(&mut self, entry: saffron_scene::AssetEntry) {
        let id = entry.id;
        let asset_type = entry.asset_type;
        self.catalog.put(entry);
        self.invalidate_loaded_asset(id, asset_type);
        self.record_asset_mutation(
            AssetMutationTarget::Asset { id, asset_type },
            AssetMutationKind::Reimported,
            invalidations_for(asset_type),
        );
    }

    /// Replaces an existing catalog entry after an authored edit.
    pub fn replace_edited_asset_entry(&mut self, entry: saffron_scene::AssetEntry) -> bool {
        if self.catalog.find(entry.id).is_none() {
            return false;
        }
        let id = entry.id;
        let asset_type = entry.asset_type;
        self.catalog.put(entry);
        self.invalidate_loaded_asset(id, asset_type);
        self.record_asset_mutation(
            AssetMutationTarget::Asset { id, asset_type },
            AssetMutationKind::Edited,
            invalidations_for(asset_type),
        );
        true
    }

    /// Marks authored bytes for an existing catalog asset as edited.
    pub fn asset_edited(&mut self, id: Uuid) -> bool {
        let Some(asset_type) = self.catalog.find(id).map(|entry| entry.asset_type) else {
            return false;
        };
        self.invalidate_loaded_asset(id, asset_type);
        self.record_asset_mutation(
            AssetMutationTarget::Asset { id, asset_type },
            AssetMutationKind::Edited,
            invalidations_for(asset_type),
        );
        true
    }

    fn edit_asset_metadata(
        &mut self,
        id: Uuid,
        edit: impl FnOnce(&mut saffron_scene::AssetEntry),
    ) -> bool {
        let Some(index) = self.catalog.by_id.get(&id.value()).copied() else {
            return false;
        };
        let asset_type = self.catalog.entries[index].asset_type;
        edit(&mut self.catalog.entries[index]);
        self.record_asset_mutation(
            AssetMutationTarget::Asset { id, asset_type },
            AssetMutationKind::Edited,
            AssetInvalidations::NONE,
        );
        true
    }

    /// Renames one catalog asset without invalidating its render content.
    pub fn rename_asset(&mut self, id: Uuid, name: String) -> bool {
        self.edit_asset_metadata(id, |entry| entry.name = name)
    }

    /// Moves one catalog asset to a folder without invalidating its render content.
    pub fn move_asset_to_folder(&mut self, id: Uuid, folder: String) -> bool {
        self.edit_asset_metadata(id, |entry| entry.folder = folder)
    }

    /// Publishes a content hash written as part of an authored content edit.
    pub fn update_asset_content_hash(&mut self, id: Uuid, content_hash: u64) -> bool {
        let Some(index) = self.catalog.by_id.get(&id.value()).copied() else {
            return false;
        };
        self.catalog.entries[index].content_hash = content_hash;
        self.asset_edited(id)
    }

    /// Backfills derived catalog metadata without invalidating unchanged render content.
    pub fn backfill_asset_content_hash(&mut self, id: Uuid, content_hash: u64) -> bool {
        self.edit_asset_metadata(id, |entry| entry.content_hash = content_hash)
    }

    /// Records source attribution without invalidating render-derived content.
    pub fn set_asset_attribution(
        &mut self,
        id: Uuid,
        attribution: saffron_scene::Attribution,
    ) -> bool {
        self.edit_asset_metadata(id, |entry| entry.attribution = Some(attribution))
    }

    /// Removes one catalog asset, invalidates its loaded representations, and publishes deletion.
    pub fn delete_asset_entry(&mut self, id: Uuid) -> Option<saffron_scene::AssetEntry> {
        let removed = self.catalog.remove(id)?;
        self.invalidate_loaded_asset(id, removed.asset_type);
        self.record_asset_mutation(
            AssetMutationTarget::Asset {
                id,
                asset_type: removed.asset_type,
            },
            AssetMutationKind::Deleted,
            invalidations_for(removed.asset_type),
        );
        Some(removed)
    }

    /// Drops the loaded representations for one asset while retaining its catalog identity.
    pub fn unload_asset(&mut self, id: Uuid) -> bool {
        let Some(asset_type) = self.catalog.find(id).map(|entry| entry.asset_type) else {
            return false;
        };
        self.invalidate_loaded_asset(id, asset_type);
        self.record_asset_mutation(
            AssetMutationTarget::Asset { id, asset_type },
            AssetMutationKind::Unloaded,
            invalidations_for(asset_type),
        );
        true
    }

    /// Replaces the complete live catalog and publishes one rebuild boundary.
    ///
    /// The caller must idle the GPU before replacement because loaded GPU representations are
    /// discarded with the old catalog.
    pub fn replace_catalog(&mut self, catalog: AssetCatalog) {
        self.clear_loaded_asset_state();
        self.catalog = catalog;
        self.record_asset_mutation(
            AssetMutationTarget::All,
            AssetMutationKind::CatalogReplaced,
            AssetInvalidations::ALL,
        );
    }

    pub(crate) fn replace_scanned_catalog(&mut self, catalog: AssetCatalog) {
        let previous = std::mem::replace(&mut self.catalog, catalog);
        let unloaded = !self.asset_caches_are_empty();
        self.clear_loaded_asset_state();
        if unloaded {
            self.record_asset_mutation(
                AssetMutationTarget::All,
                AssetMutationKind::Unloaded,
                AssetInvalidations::ALL,
            );
        }

        let removed = previous
            .entries
            .iter()
            .filter(|entry| self.catalog.find(entry.id).is_none())
            .map(|entry| (entry.id, entry.asset_type))
            .collect::<Vec<_>>();
        for (id, asset_type) in removed {
            self.record_asset_mutation(
                AssetMutationTarget::Asset { id, asset_type },
                AssetMutationKind::Deleted,
                invalidations_for(asset_type),
            );
        }
        let changed = self
            .catalog
            .entries
            .iter()
            .filter_map(|entry| match previous.find(entry.id) {
                None => Some((entry.id, entry.asset_type, AssetMutationKind::Imported)),
                Some(previous) if previous != entry => {
                    Some((entry.id, entry.asset_type, AssetMutationKind::Reimported))
                }
                Some(_) => None,
            })
            .collect::<Vec<_>>();
        for (id, asset_type, kind) in changed {
            self.record_asset_mutation(
                AssetMutationTarget::Asset { id, asset_type },
                kind,
                invalidations_for(asset_type),
            );
        }
    }

    /// Catalog folder names in display order.
    #[must_use]
    pub fn catalog_folders(&self) -> &[String] {
        &self.catalog.folders
    }

    /// Adds a catalog folder.
    pub fn add_catalog_folder(&mut self, folder: String) {
        self.catalog.folders.push(folder);
    }

    /// Replaces the catalog folder list after a validated hierarchy edit.
    pub fn replace_catalog_folders(&mut self, folders: Vec<String>) {
        self.catalog.folders = folders;
    }

    /// Renames a catalog folder and every direct asset assignment.
    pub fn rename_catalog_folder(&mut self, from: &str, to: &str) {
        for folder in &mut self.catalog.folders {
            if folder == from {
                *folder = to.to_owned();
            }
        }
        let edited = self
            .catalog
            .entries
            .iter()
            .filter(|entry| entry.folder == from)
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        for id in edited {
            let _ = self.move_asset_to_folder(id, to.to_owned());
        }
    }

    /// Removes a catalog folder and reparents its contents to the folder's parent.
    pub fn remove_catalog_folder(&mut self, folder: &str, parent: &str) {
        self.catalog.folders.retain(|candidate| candidate != folder);
        let edited = self
            .catalog
            .entries
            .iter()
            .filter(|entry| entry.folder == folder)
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        for id in edited {
            let _ = self.move_asset_to_folder(id, parent.to_owned());
        }
    }

    /// Whether the GPU and decoded asset caches contain no entries.
    #[must_use]
    pub fn asset_caches_are_empty(&self) -> bool {
        self.mesh_by_uuid.is_empty()
            && self.mesh_bvh_by_uuid.is_empty()
            && self.texture_by_uuid.is_empty()
            && self.height_texture_by_uuid.is_empty()
            && self.coverage_texture_by_uuid.is_empty()
            && self.lut_by_uuid.is_empty()
            && self.model_by_uuid.is_empty()
            && self.material_by_uuid.is_empty()
            && self.material_shader_by_uuid.is_empty()
            && self.preview_render_queue.is_empty()
            && self.preview_render_in_flight.is_empty()
    }

    /// Seeds a reserved editor-preview material that is not a catalog asset.
    pub fn seed_preview_material(&mut self, id: Uuid, material: MaterialAsset) {
        assert!(id.value() < 1024, "preview material id must be reserved");
        self.material_by_uuid
            .insert(id.value(), Some(Arc::new(material)));
    }

    /// Repoints the asset root and (re)creates its standard subdirectories.
    pub fn set_asset_root(&mut self, root: impl Into<PathBuf>) {
        self.root = root.into();
        self.vegetation_cache_root = project_vegetation_cache_root(&self.root);
        self.vegetation_state_root = project_vegetation_state_root(&self.root);
        self.ensure_asset_directories();
    }

    /// Returns the derived vegetation artifact store for the active project.
    #[must_use]
    pub fn vegetation_artifact_store(&self) -> VegetationArtifactStore {
        VegetationArtifactStore::new(&self.vegetation_cache_root)
    }

    /// Returns the durable vegetation state store for the active project.
    #[must_use]
    pub fn vegetation_state_store(&self) -> VegetationStateStore {
        VegetationStateStore::new(&self.vegetation_state_root)
    }

    /// Creates the standard asset subdirectories under the root, idempotently. Directory-creation
    /// errors are swallowed: a missing dir surfaces later as the real I/O failure that needs it.
    pub fn ensure_asset_directories(&self) {
        for sub in [
            "models",
            "textures",
            "materials",
            "luts",
            "environments",
            "vegetation/plants",
            "vegetation/biomes",
            "vegetation/maps",
        ] {
            let _ = std::fs::create_dir_all(self.root.join(sub));
        }
    }

    /// The app-level thumbnail cache directory (`<appDataRoot>/thumbnail-cache/` by default):
    /// content-addressed and shared across projects, and outside any project root so the catalog
    /// scan and project save/load never see it.
    #[must_use]
    pub fn thumbnail_cache_dir(&self) -> PathBuf {
        self.thumbnail_cache_root.clone()
    }

    /// Drops the GPU caches (and abandons stale worker jobs), freeing every cached
    /// `GpuMesh`/`GpuTexture` whose last `Arc` lives here.
    ///
    /// # GPU idle is the caller's responsibility
    ///
    /// The caller must have called `wait_gpu_idle(renderer)` first: an in-flight frame may still
    /// reference a cached `Arc<GpuTexture>`, and dropping it under the GPU is a use-after-free that
    /// `Drop` ordering cannot catch.
    pub fn clear_asset_caches(&mut self) {
        self.clear_loaded_asset_state();
        self.record_asset_mutation(
            AssetMutationTarget::All,
            AssetMutationKind::Unloaded,
            AssetInvalidations::ALL,
        );
    }

    fn clear_loaded_asset_state(&mut self) {
        self.clear_thumbnail_queue();
        self.mesh_by_uuid.clear();
        self.plant_mechanics_by_uuid.clear();
        self.plant_render_by_hash.clear();
        self.mesh_bvh_by_uuid.clear();
        self.texture_by_uuid.clear();
        self.texture_pixels_by_uuid.clear();
        self.height_texture_by_uuid.clear();
        self.coverage_texture_by_uuid.clear();
        self.lut_by_uuid.clear();
        self.model_by_uuid.clear();
        self.clear_material_caches();
    }

    fn clear_material_caches(&mut self) {
        self.material_by_uuid.clear();
        self.material_shader_by_uuid.clear();
    }

    /// Asset-content epoch used by camera-independent render caches.
    #[must_use]
    pub fn render_content_revision(&self) -> u64 {
        self.render_content_revision
    }

    /// Drops every GPU texture representation derived from one catalog texture.
    pub(crate) fn invalidate_texture_caches(&mut self, id: Uuid) {
        self.texture_by_uuid.remove(&id.value());
        self.texture_pixels_by_uuid.remove(&id.value());
        self.height_texture_by_uuid.remove(&id.value());
        self.coverage_texture_by_uuid
            .retain(|(texture, _), _| *texture != id.value());
    }

    /// Abandons queued main-graph preview renders + their dedup set on a project switch, so tiles
    /// for the closed project never render into the new one.
    pub fn clear_thumbnail_queue(&mut self) {
        self.preview_render_queue.clear();
        self.preview_render_in_flight.clear();
    }

    fn invalidate_loaded_asset(&mut self, id: Uuid, asset_type: saffron_scene::AssetType) {
        use saffron_scene::AssetType;

        match asset_type {
            AssetType::Mesh => {
                self.mesh_by_uuid.remove(&id.value());
                self.mesh_bvh_by_uuid.remove(&id.value());
            }
            AssetType::Texture => {
                self.invalidate_texture_caches(id);
                self.clear_material_caches();
            }
            AssetType::Material => self.clear_material_caches(),
            AssetType::Model => {
                self.model_by_uuid.remove(&id.value());
                self.clear_material_caches();
            }
            AssetType::Lut => {
                self.lut_by_uuid.remove(&id.value());
            }
            AssetType::Other
            | AssetType::Animation
            | AssetType::Environment
            | AssetType::Plant
            | AssetType::Biome
            | AssetType::VegetationMap => {}
        }
    }

    fn record_asset_mutation(
        &mut self,
        target: AssetMutationTarget,
        kind: AssetMutationKind,
        invalidations: AssetInvalidations,
    ) {
        self.asset_revision = self.asset_revision.next();
        if invalidations != AssetInvalidations::NONE {
            self.render_content_revision = self.render_content_revision.wrapping_add(1).max(1);
        }
        if self.asset_journal_capacity == 0 {
            self.asset_journal_base = self.asset_revision;
            return;
        }
        self.asset_journal.push_back(AssetMutation {
            revision: self.asset_revision,
            target,
            kind,
            invalidations,
        });
        while self.asset_journal.len() > self.asset_journal_capacity {
            if let Some(dropped) = self.asset_journal.pop_front() {
                self.asset_journal_base = dropped.revision;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal_entry(id: u64, asset_type: saffron_scene::AssetType) -> saffron_scene::AssetEntry {
        saffron_scene::AssetEntry {
            id: Uuid(id),
            name: format!("asset-{id}"),
            asset_type,
            path: format!("assets/{id}"),
            ..saffron_scene::AssetEntry::default()
        }
    }

    #[test]
    fn asset_journal_orders_import_edit_reimport_unload_and_delete() {
        use saffron_scene::AssetType;

        let root =
            std::env::temp_dir().join(format!("saffron-asset-journal-{}", Uuid::new().value()));
        let mut assets = AssetServer::new(&root);
        let start = assets.asset_journal_cursor();
        let id = Uuid(4096);

        assets.register_imported_asset(journal_entry(id.value(), AssetType::Mesh));
        assert!(assets.rename_asset(id, "renamed".to_owned()));
        assets.register_reimported_asset(saffron_scene::AssetEntry {
            name: "reimported".to_owned(),
            ..journal_entry(id.value(), AssetType::Mesh)
        });
        assert!(assets.unload_asset(id));
        assert!(assets.delete_asset_entry(id).is_some());

        let AssetJournalRead::Delta { mutations, next } = assets.read_asset_journal(start) else {
            panic!("fresh cursor retains the complete mutation delta");
        };
        assert_eq!(next.revision().get(), 5);
        assert_eq!(
            mutations
                .iter()
                .map(|mutation| mutation.kind)
                .collect::<Vec<_>>(),
            vec![
                AssetMutationKind::Imported,
                AssetMutationKind::Edited,
                AssetMutationKind::Reimported,
                AssetMutationKind::Unloaded,
                AssetMutationKind::Deleted,
            ]
        );
        assert_eq!(
            mutations
                .iter()
                .map(|mutation| mutation.revision.get())
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert_eq!(
            mutations[0].target,
            AssetMutationTarget::Asset {
                id,
                asset_type: AssetType::Mesh
            }
        );
        assert!(
            mutations[0]
                .invalidations
                .contains(AssetInvalidations::PROTOTYPE)
        );
        assert!(
            mutations[0]
                .invalidations
                .contains(AssetInvalidations::PAGE)
        );
        assert_eq!(mutations[1].invalidations, AssetInvalidations::NONE);
        assert!(assets.catalog().find(id).is_none());

        let snapshot = assets.asset_catalog_snapshot();
        assert_eq!(snapshot.cursor, next);
        assert!(snapshot.catalog.entries.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn asset_journal_overflow_requires_snapshot_rebuild() {
        use saffron_scene::AssetType;

        let root =
            std::env::temp_dir().join(format!("saffron-asset-overflow-{}", Uuid::new().value()));
        let mut assets = AssetServer::new(&root);
        assets.asset_journal_capacity = 2;
        assets.register_imported_asset(journal_entry(4096, AssetType::Material));
        let retained = assets.asset_journal_cursor();
        assets.register_imported_asset(journal_entry(4097, AssetType::Texture));
        assets.register_imported_asset(journal_entry(4098, AssetType::Plant));

        assert_eq!(
            assets.read_asset_journal(AssetJournalCursor::START),
            AssetJournalRead::SnapshotRequired {
                next: assets.asset_journal_cursor()
            }
        );
        let AssetJournalRead::Delta { mutations, next } = assets.read_asset_journal(retained)
        else {
            panic!("cursor at the retained boundary remains readable");
        };
        assert_eq!(mutations.len(), 2);
        assert_eq!(next.revision().get(), 3);
        assert!(
            mutations[0]
                .invalidations
                .contains(AssetInvalidations::TEXTURE)
        );
        assert!(
            mutations[1]
                .invalidations
                .contains(AssetInvalidations::PAGE)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reimport_evicts_negative_caches_before_publication() {
        use saffron_scene::AssetType;

        let root =
            std::env::temp_dir().join(format!("saffron-asset-cache-{}", Uuid::new().value()));
        let mut assets = AssetServer::new(&root);
        let id = Uuid(4096);
        assets.register_imported_asset(journal_entry(id.value(), AssetType::Mesh));
        assets.mesh_by_uuid.insert(id.value(), None);
        assets.mesh_bvh_by_uuid.insert(id.value(), None);

        assets.register_reimported_asset(journal_entry(id.value(), AssetType::Mesh));

        assert!(!assets.mesh_by_uuid.contains_key(&id.value()));
        assert!(!assets.mesh_bvh_by_uuid.contains_key(&id.value()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reserved_sentinels_are_in_the_reserved_range() {
        assert!(DEFAULT_MATERIAL_ID.value() < 1024);
        assert!(PREVIEW_FLOOR_MESH_ID.value() < 1024);
        assert!(PREVIEW_MATERIAL_ID.value() < 1024);
        assert!(PREVIEW_THUMBNAIL_MATERIAL_ID.value() < 1024);
        assert_ne!(DEFAULT_MATERIAL_ID, PREVIEW_FLOOR_MESH_ID);
        assert_ne!(PREVIEW_MATERIAL_ID, DEFAULT_MATERIAL_ID);
        assert_ne!(PREVIEW_THUMBNAIL_MATERIAL_ID, PREVIEW_MATERIAL_ID);
        for builtin in [BuiltinMesh::Cube, BuiltinMesh::Plane, BuiltinMesh::Sphere] {
            let id = builtin.reserved_id();
            assert!(id.value() < 1024);
            assert_ne!(id, DEFAULT_MATERIAL_ID);
            assert_ne!(id, PREVIEW_FLOOR_MESH_ID);
            assert_ne!(id, PREVIEW_MATERIAL_ID);
            assert_eq!(BuiltinMesh::from_reserved_id(id), Some(builtin));
        }
        assert_eq!(BuiltinMesh::from_reserved_id(DEFAULT_MATERIAL_ID), None);
        assert_eq!(BuiltinMesh::from_reserved_id(Uuid(4096)), None);
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
