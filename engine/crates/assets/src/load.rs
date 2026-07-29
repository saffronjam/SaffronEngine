//! The cache resolve/load paths: the negative-cache loaders over geometry's byte
//! codecs + image decode and rendering's GPU upload.
//!
//! Every loader follows the get-or-negative-cache shape (see [`crate::cache`]): a cache
//! hit returns the stored `Option<Arc<T>>` (live or negative); a miss attempts a load,
//! caches the outcome (or `None` on failure plus a one-time warn), and returns it. Every
//! distinct failure negative-caches — bytes unreadable, decode failed, upload failed,
//! dangling catalog id, no such chunk — so a broken asset is not retried (or re-warned)
//! each frame. In the draw path a dangling texture id falls back to rendering's
//! default-white slot; the loader returns `None` and never retries.
//!
//! The colorspace → upload-format map is exact: [`Colorspace::Hdr`] → the float uploader;
//! [`Colorspace::Linear`] → unorm; [`Colorspace::Srgb`]/[`Colorspace::Auto`] → sRGB
//! (`srgb = space != Linear`). A standalone texture's explicit `.smeta` colorspace wins,
//! else the row's `hdr`/`linear` provenance.
//!
//! [`AssetServer::load_anim_clip`] and [`AssetServer::load_mesh_cpu_asset`] are
//! `Result`-returning one-shots (not cache-backed) used by the animation runtime and
//! physics cooking: they resolve the same embedded/standalone fork but read CPU data.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use saffron_core::Uuid;
use saffron_geometry::{
    AnimClip, ChunkKind, DecodedImage, Mesh, MeshBvh, PortableHierarchyInput,
    PortableVirtualHierarchy, VertexSkin, cook_portable_virtual_hierarchy,
    decode_image_from_memory, decode_image_from_memory_hdr, load_animation,
    load_animation_from_bytes, load_mesh_from_bytes, load_mesh_hierarchy_from_bytes,
    load_mesh_morph_from_bytes, load_mesh_skin_from_bytes, translate_model,
};
use saffron_rendering::{GpuMesh, GpuTexture, SdfBake, SdfSource};
use saffron_scene::{AssetType, Colorspace};

use crate::error::{Error, Result};
use crate::gpu::GpuUploader;
use crate::model::ByteSource;
use crate::{AssetServer, PREVIEW_FLOOR_MESH_ID};

/// Complete CPU payload resolved from one catalog mesh row.
pub(crate) struct CpuMeshSource {
    pub(crate) mesh: Mesh,
    pub(crate) skin: Vec<VertexSkin>,
}

fn hierarchy_for_generated_mesh(
    mesh: &Mesh,
    skin: &[VertexSkin],
) -> saffron_geometry::Result<PortableVirtualHierarchy> {
    let input = PortableHierarchyInput::from_mesh(mesh, skin)?;
    cook_portable_virtual_hierarchy(&input)
}

/// The `Colorspace` a `.smodel` texture chunk's `flags` word encodes.
///
/// The container writes the [`Colorspace`] discriminant straight into the chunk flags
/// (`Auto = 0`, `Srgb = 1`, `Linear = 2`, `Hdr = 3`), so the reader maps it back. An
/// unknown value falls back to [`Colorspace::Srgb`].
fn colorspace_from_flags(flags: u32) -> Colorspace {
    match flags {
        0 => Colorspace::Auto,
        2 => Colorspace::Linear,
        3 => Colorspace::Hdr,
        _ => Colorspace::Srgb,
    }
}

/// Resolves an engine-shipped asset (e.g. `models/cube.gltf`) to an absolute path.
///
/// The `SAFFRON_ASSET_DIR` override wins; else the directory beside the running binary,
/// walking up to find one that holds the relative path (a test binary runs from
/// `target/<profile>/deps/`, one level below the `models/` the xtask copies into
/// `target/<profile>/`). Mirrors rendering's shader-dir resolution. An absolute
/// `relative` is returned as-is.
pub fn engine_asset_path(relative: &str) -> PathBuf {
    if relative.starts_with('/') {
        return PathBuf::from(relative);
    }
    if let Some(dir) = std::env::var_os("SAFFRON_ASSET_DIR") {
        return PathBuf::from(dir).join(relative);
    }
    if let Ok(exe) = std::env::current_exe() {
        #[cfg(target_os = "macos")]
        if let Some(executable_dir) = exe.parent() {
            let bundled = executable_dir.join("..").join("Resources").join(relative);
            if bundled.exists() {
                return bundled;
            }
        }
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(candidate) = dir {
            if candidate.join(relative).exists() {
                return candidate.join(relative);
            }
            dir = candidate.parent().map(Path::to_path_buf);
        }
    }
    PathBuf::from(relative)
}

impl AssetServer {
    /// Loads + uploads a mesh from any byte source (a standalone file or a `.smodel`
    /// chunk slice), caching the GPU `Arc` under `sub_id`.
    ///
    /// A cache hit returns the stored entry (live or negative). On a miss each failure
    /// mode — bytes unreadable, mesh decode failed, upload failed — negative-caches with
    /// a one-time warn so the broken sub-asset is not retried each frame.
    pub fn load_mesh_from_source(
        &mut self,
        gpu: &dyn GpuUploader,
        sub_id: Uuid,
        source: &ByteSource,
        sdf_bake: Option<SdfBake>,
    ) -> Option<Arc<GpuMesh>> {
        if let Some(cached) = self.mesh_by_uuid.get(&sub_id.value()) {
            return cached.clone();
        }
        let result = self.upload_mesh_from_source(gpu, sub_id, source, sdf_bake);
        if result.is_some() {
            self.page_source_by_uuid.insert(
                sub_id.value(),
                crate::page_stream::PagePayloadSource::Artifact(source.clone()),
            );
        }
        self.mesh_by_uuid.insert(sub_id.value(), result.clone());
        result
    }

    /// The recorded page-payload source for a loaded mesh, if any.
    pub fn page_payload_source(
        &self,
        sub_id: Uuid,
    ) -> Option<crate::page_stream::PagePayloadSource> {
        self.page_source_by_uuid.get(&sub_id.value()).cloned()
    }

    /// Reads + decodes + uploads the mesh, GPU-baking (or cache-loading) its signed distance
    /// field when `sdf_bake` is set, or returns `None` (with a warn) on any failure. The
    /// caller caches the outcome.
    fn upload_mesh_from_source(
        &self,
        gpu: &dyn GpuUploader,
        sub_id: Uuid,
        source: &ByteSource,
        sdf_bake: Option<SdfBake>,
    ) -> Option<Arc<GpuMesh>> {
        let bytes = match source.read() {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("mesh {}: {err}", sub_id.value());
                return None;
            }
        };
        let mesh = match load_mesh_from_bytes(&bytes) {
            Ok(mesh) => mesh,
            Err(err) => {
                tracing::warn!("mesh {}: {err}", sub_id.value());
                return None;
            }
        };
        let hierarchy = match load_mesh_hierarchy_from_bytes(&bytes) {
            Ok(hierarchy) => hierarchy,
            Err(err) => {
                tracing::warn!("mesh {}: {err}", sub_id.value());
                return None;
            }
        };
        // A skinned `.smesh` carries a parallel skin stream; an unskinned one returns an
        // empty stream (the uploader treats an empty skin as a static mesh). A morph
        // `.smesh` carries its sparse deltas; the deform pass reads them on the GPU.
        let skin = load_mesh_skin_from_bytes(&bytes).unwrap_or_default();
        let morph = load_mesh_morph_from_bytes(&bytes).ok().flatten();
        match gpu.upload_mesh(
            &mesh,
            &hierarchy,
            &skin,
            morph.as_ref(),
            sdf_bake.as_ref().map_or(SdfSource::None, SdfSource::Bake),
        ) {
            Ok(mesh_ref) => Some(mesh_ref),
            Err(err) => {
                tracing::warn!("mesh {}: {err}", sub_id.value());
                None
            }
        }
    }

    /// The per-mesh SDF bake request for a project mesh: the default `resolution_scale` and
    /// the project's `assets/cache` sidecar directory (a content hash of the geometry keys
    /// the cache, so a re-load skips the GPU bake). The gizmo/preview meshes pass `None`.
    fn sdf_bake(&self) -> SdfBake {
        SdfBake {
            resolution_scale: 1.0,
            cache_dir: Some(self.root.join("cache")),
        }
    }

    /// Loads + uploads a texture from any byte source, picking the upload format from
    /// the colorspace ([`Colorspace::Hdr`] → float; [`Colorspace::Linear`] → unorm;
    /// [`Colorspace::Srgb`]/[`Colorspace::Auto`] → sRGB). Caches the GPU `Arc` under
    /// `sub_id`.
    pub fn load_texture_from_source(
        &mut self,
        gpu: &dyn GpuUploader,
        sub_id: Uuid,
        source: &ByteSource,
        space: Colorspace,
    ) -> Option<Arc<GpuTexture>> {
        self.load_texture_from_source_role(gpu, sub_id, source, space, false)
    }

    /// The role-aware core behind [`Self::load_texture_from_source`]: `as_height` routes the upload
    /// through [`GpuUploader::upload_height_texture`] (building the min/max pyramid) and caches into the
    /// separate `height_texture_by_uuid` map, so a displacement height map is a distinct GPU resource
    /// from the same image used as a plain texture.
    fn load_texture_from_source_role(
        &mut self,
        gpu: &dyn GpuUploader,
        sub_id: Uuid,
        source: &ByteSource,
        space: Colorspace,
        as_height: bool,
    ) -> Option<Arc<GpuTexture>> {
        if let Some(cached) = self.texture_cache(as_height).get(&sub_id.value()) {
            return cached.clone();
        }
        let result = upload_texture_from_source(gpu, sub_id, source, space, as_height);
        self.texture_cache_mut(as_height)
            .insert(sub_id.value(), result.clone());
        result
    }

    /// The texture GPU cache for the role: the plain `texture_by_uuid`, or the pyramid-carrying
    /// `height_texture_by_uuid` when resolving a displacement height map.
    fn texture_cache(&self, as_height: bool) -> &crate::cache::AssetCache<GpuTexture> {
        if as_height {
            &self.height_texture_by_uuid
        } else {
            &self.texture_by_uuid
        }
    }

    /// The mutable texture GPU cache for the role (see [`Self::texture_cache`]).
    fn texture_cache_mut(&mut self, as_height: bool) -> &mut crate::cache::AssetCache<GpuTexture> {
        if as_height {
            &mut self.height_texture_by_uuid
        } else {
            &mut self.texture_by_uuid
        }
    }

    /// Resolves an embedded mesh sub-asset to a live GPU mesh, honoring the remap table;
    /// keyed by sub-id.
    pub fn resolve_mesh(
        &mut self,
        gpu: &dyn GpuUploader,
        model_id: Uuid,
        sub_id: Uuid,
    ) -> Option<Arc<GpuMesh>> {
        if let Some(cached) = self.mesh_by_uuid.get(&sub_id.value()) {
            return cached.clone();
        }
        let Some(model) = self.load_model_asset(model_id) else {
            self.mesh_by_uuid.insert(sub_id.value(), None);
            return None;
        };
        let source = self.chunk_source_for(&model, ChunkKind::Mesh, sub_id);
        if source.is_empty() {
            tracing::warn!(
                "model {}: no mesh sub-asset {}",
                model_id.value(),
                sub_id.value()
            );
            self.mesh_by_uuid.insert(sub_id.value(), None);
            return None;
        }
        // The per-mesh signed distance field is GPU jump-flood baked (or sidecar-cache
        // loaded) at upload time from the mesh geometry, tied to the mesh's lifetime.
        self.load_mesh_from_source(gpu, sub_id, &source, Some(self.sdf_bake()))
    }

    /// Resolves an embedded texture sub-asset to a live GPU texture (colorspace from the
    /// chunk flags).
    pub fn resolve_texture(
        &mut self,
        gpu: &dyn GpuUploader,
        model_id: Uuid,
        sub_id: Uuid,
    ) -> Option<Arc<GpuTexture>> {
        self.resolve_texture_role(gpu, model_id, sub_id, false)
    }

    /// The role-aware core behind [`Self::resolve_texture`]: `as_height` resolves the embedded texture
    /// as a displacement height map (building its min/max pyramid, caching separately).
    fn resolve_texture_role(
        &mut self,
        gpu: &dyn GpuUploader,
        model_id: Uuid,
        sub_id: Uuid,
        as_height: bool,
    ) -> Option<Arc<GpuTexture>> {
        if let Some(cached) = self.texture_cache(as_height).get(&sub_id.value()) {
            return cached.clone();
        }
        let Some(model) = self.load_model_asset(model_id) else {
            self.texture_cache_mut(as_height)
                .insert(sub_id.value(), None);
            return None;
        };
        let space = model
            .reader
            .find(ChunkKind::Texture, sub_id.value())
            .map_or(Colorspace::Srgb, |entry| colorspace_from_flags(entry.flags));
        let source = self.chunk_source_for(&model, ChunkKind::Texture, sub_id);
        if source.is_empty() {
            tracing::warn!(
                "model {}: no texture sub-asset {}",
                model_id.value(),
                sub_id.value()
            );
            self.texture_cache_mut(as_height)
                .insert(sub_id.value(), None);
            return None;
        }
        self.load_texture_from_source_role(gpu, sub_id, &source, space, as_height)
    }

    /// Resolves a mesh id to a GPU mesh, loading + uploading the baked `.smesh` on a
    /// cache miss. An embedded sub-asset routes through its container; a standalone file
    /// reads its path (with the `meshes/` → `models/` path fixup). Returns `None`
    /// (negative-cached) for an unregistered, wrong-type, or unreadable asset.
    /// The cached ray-pick [`MeshBvh`] for a mesh sub-id, built from the resolved mesh's CPU
    /// geometry on the first request and reused thereafter. `None` when the mesh has no pickable
    /// triangles. Lets the placement/selection pick descend a hierarchy instead of scanning every
    /// triangle of every scene mesh on each cursor move.
    pub fn mesh_pick_bvh(&mut self, sub_id: Uuid, mesh: &GpuMesh) -> Option<Arc<MeshBvh>> {
        crate::cache::resolve_cached(&mut self.mesh_bvh_by_uuid, sub_id.value(), || {
            let positions: Vec<_> = mesh
                .cpu_vertices
                .iter()
                .map(|vertex| vertex.position)
                .collect();
            MeshBvh::build(&positions, &mesh.cpu_indices).map(Arc::new)
        })
    }

    pub fn load_mesh_asset(&mut self, gpu: &dyn GpuUploader, id: Uuid) -> Option<Arc<GpuMesh>> {
        if let Some(cached) = self.mesh_by_uuid.get(&id.value()) {
            return cached.clone();
        }
        // A reserved built-in id has no catalog row: generate + upload its geometry on
        // demand and cache it. This re-seeds automatically after a project-load cache
        // clear, so a primitive survives project switches with no eager bookkeeping.
        if let Some(builtin) = crate::BuiltinMesh::from_reserved_id(id) {
            return self.seed_builtin_mesh(gpu, builtin);
        }
        if id == crate::EDITOR_CAMERA_MESH_ID {
            return self.seed_editor_camera_mesh(gpu);
        }
        // Extract the owned row fields, dropping the catalog borrow before the `&mut self`
        // resolve/upload calls below.
        let (container, rel_path) = match self.catalog.find(id) {
            Some(entry) if entry.asset_type == AssetType::Mesh => {
                (entry.container, entry.path.clone())
            }
            _ => return None,
        };
        if container.value() != 0 {
            return self.resolve_mesh(gpu, container, id);
        }
        let path = self.standalone_mesh_path(&rel_path);
        let source = ByteSource {
            path,
            ..ByteSource::default()
        };
        // The signed distance field is GPU-baked (or sidecar-cache loaded) from the mesh
        // geometry at upload time — standalone meshes get a field too.
        self.load_mesh_from_source(gpu, id, &source, Some(self.sdf_bake()))
    }

    /// Resolves a texture id to a GPU texture, decoding + uploading the copied file on a
    /// cache miss. An embedded sub-asset routes through its container's chunk; a
    /// standalone file picks its colorspace from an explicit `.smeta` (the row's
    /// `colorspace`) else the `hdr`/`linear` provenance. A dangling id warns once and
    /// negative-caches; the draw path substitutes the default-white slot.
    pub fn load_texture_asset(
        &mut self,
        gpu: &dyn GpuUploader,
        id: Uuid,
    ) -> Option<Arc<GpuTexture>> {
        self.load_texture_asset_role(gpu, id, false)
    }

    /// Resolves and caches the exact decoded RGBA8 source pixels for CPU coverage queries.
    pub(crate) fn load_texture_pixels(&mut self, id: Uuid) -> Option<Arc<DecodedImage>> {
        if let Some(cached) = self.texture_pixels_by_uuid.get(&id.value()) {
            return cached.clone();
        }
        let (container, path) = match self.catalog.find(id) {
            Some(entry) if entry.asset_type == AssetType::Texture => {
                (entry.container, entry.path.clone())
            }
            _ => {
                self.texture_pixels_by_uuid.insert(id.value(), None);
                return None;
            }
        };
        let source = if container.value() == 0 {
            ByteSource {
                path: format!("{}/{}", self.root.display(), path),
                ..ByteSource::default()
            }
        } else {
            let Some(model) = self.load_model_asset(container) else {
                self.texture_pixels_by_uuid.insert(id.value(), None);
                return None;
            };
            self.chunk_source_for(&model, ChunkKind::Texture, id)
        };
        let decoded = source
            .read()
            .ok()
            .and_then(|bytes| decode_image_from_memory(&bytes).ok())
            .map(Arc::new);
        self.texture_pixels_by_uuid
            .insert(id.value(), decoded.clone());
        decoded
    }

    /// Resolves a texture id as a **displacement height map**: identical resolution to
    /// [`Self::load_texture_asset`], but the upload builds the per-height min/max pyramid (for the
    /// tessellation factor kernel) and the result is cached separately. The draw path calls this for a
    /// [`saffron_core::HeightMode::Displacement`] material's height slot; a `None` falls back to
    /// default-white (no displacement).
    pub fn load_height_texture_asset(
        &mut self,
        gpu: &dyn GpuUploader,
        id: Uuid,
    ) -> Option<Arc<GpuTexture>> {
        self.load_texture_asset_role(gpu, id, true)
    }

    /// Resolves a texture into its canonical cutoff-preserving coverage mip chain.
    ///
    /// Coverage variants are distinct from color and height textures because their
    /// mips are exact alpha-area integrals and the reference cutoff is part of the
    /// material's canonical identity.
    pub fn load_coverage_texture_asset(
        &mut self,
        gpu: &dyn GpuUploader,
        id: Uuid,
        cutoff_bits: u16,
    ) -> Option<Arc<GpuTexture>> {
        let key = (id.value(), cutoff_bits);
        if let Some(cached) = self.coverage_texture_by_uuid.get(&key) {
            return cached.clone();
        }
        let (container, path) = match self.catalog.find(id) {
            Some(entry) if entry.asset_type == AssetType::Texture => {
                (entry.container, entry.path.clone())
            }
            _ => {
                tracing::warn!(
                    "coverage texture {} not in catalog; using default",
                    id.value()
                );
                self.coverage_texture_by_uuid.insert(key, None);
                return None;
            }
        };
        let source = if container.value() == 0 {
            ByteSource {
                path: format!("{}/{}", self.root.display(), path),
                ..ByteSource::default()
            }
        } else {
            let Some(model) = self.load_model_asset(container) else {
                self.coverage_texture_by_uuid.insert(key, None);
                return None;
            };
            self.chunk_source_for(&model, ChunkKind::Texture, id)
        };
        let loaded = upload_coverage_texture_from_source(gpu, id, &source, cutoff_bits);
        self.coverage_texture_by_uuid.insert(key, loaded.clone());
        loaded
    }

    /// The role-aware core behind [`Self::load_texture_asset`] / [`Self::load_height_texture_asset`]:
    /// one resolution (catalog → embedded/standalone fork, colorspace), with `as_height` selecting the
    /// pyramid-building upload + the separate height cache.
    fn load_texture_asset_role(
        &mut self,
        gpu: &dyn GpuUploader,
        id: Uuid,
        as_height: bool,
    ) -> Option<Arc<GpuTexture>> {
        if let Some(cached) = self.texture_cache(as_height).get(&id.value()) {
            return cached.clone();
        }
        let entry = match self.catalog.find(id) {
            Some(entry) if entry.asset_type == AssetType::Texture => entry,
            _ => {
                // A dangling reference: a material/scene names a texture not in the
                // catalog. Warn once and negative-cache; the draw path falls back to the
                // default-white slot (it does not retry).
                tracing::warn!("texture {} not in catalog; using default", id.value());
                self.texture_cache_mut(as_height).insert(id.value(), None);
                return None;
            }
        };
        let container = entry.container;
        if container.value() != 0 {
            return self.resolve_texture_role(gpu, container, id, as_height);
        }
        // A standalone image file: an explicit `.smeta` colorspace wins; else the row's
        // hdr/linear provenance (engine-written textures set those at registration).
        let space = if entry.colorspace != Colorspace::Auto {
            entry.colorspace
        } else if entry.hdr {
            Colorspace::Hdr
        } else if entry.linear {
            Colorspace::Linear
        } else {
            Colorspace::Srgb
        };
        let source = ByteSource {
            path: format!("{}/{}", self.root.display(), entry.path),
            ..ByteSource::default()
        };
        self.load_texture_from_source_role(gpu, id, &source, space, as_height)
    }

    /// Warms one catalog asset into its GPU cache (mesh or texture), for the project loader's
    /// residency prefetch. A no-op for other kinds. Idempotent (cache-hit fast path), so the
    /// loader can call it once per residency step without re-uploading.
    pub fn warm_asset(&mut self, gpu: &dyn GpuUploader, id: Uuid) {
        match self.catalog.find(id).map(|entry| entry.asset_type) {
            Some(AssetType::Mesh) => {
                self.load_mesh_asset(gpu, id);
            }
            Some(AssetType::Texture) => {
                self.load_texture_asset(gpu, id);
            }
            _ => {}
        }
    }

    /// Loads an animation clip by id into a CPU [`AnimClip`]. An embedded clip reads its
    /// `SANM` chunk through the owning container; a standalone clip reads its file. A
    /// `Result`-returning one-shot (not cache-backed) the animation runtime calls on a
    /// cache miss.
    ///
    /// # Errors
    ///
    /// [`Error::NotInCatalog`] for a missing id, [`Error::WrongAssetType`] for a
    /// non-animation entry, [`Error::Io`] if the container is unloadable or the sub-asset
    /// absent, or [`Error::Geometry`] for malformed clip bytes.
    pub fn load_anim_clip(&mut self, id: Uuid) -> Result<AnimClip> {
        let entry = self
            .catalog
            .find(id)
            .ok_or(Error::NotInCatalog(id.value()))?;
        if entry.asset_type != AssetType::Animation {
            return Err(Error::WrongAssetType {
                id: id.value(),
                wanted: "animation",
            });
        }
        let container = entry.container;
        let rel_path = entry.path.clone();
        if container.value() != 0 {
            let model = self.load_model_asset(container).ok_or_else(|| {
                Error::Io(format!(
                    "clip {}: container {} is not loadable",
                    id.value(),
                    container.value()
                ))
            })?;
            let source = self.chunk_source_for(&model, ChunkKind::Animation, id);
            if source.is_empty() {
                return Err(Error::ContainerMissingSubAsset {
                    container: container.value(),
                    sub: id.value(),
                });
            }
            let bytes = source.read()?;
            return Ok(load_animation_from_bytes(&bytes)?);
        }
        let full_path = format!("{}/{rel_path}", self.root.display());
        Ok(load_animation(&full_path)?)
    }

    /// Decodes a mesh id's baked `.smesh` to a CPU [`Mesh`] (for physics cooking).
    ///
    /// A catalog lookup + bytes read + decode; no GPU upload, no cache entry — cooking is
    /// a one-shot at Edit→Playing, not the draw path. Mirrors [`Self::load_anim_clip`]'s
    /// resolve fork.
    ///
    /// # Errors
    ///
    /// [`Error::NotInCatalog`] for a missing id, [`Error::WrongAssetType`] for a
    /// non-mesh entry, [`Error::Io`] if the container is unloadable or the sub-asset
    /// absent, or [`Error::Geometry`] for malformed mesh bytes.
    pub fn load_mesh_cpu_asset(&mut self, id: Uuid) -> Result<Mesh> {
        Ok(self.load_mesh_cpu_source(id)?.mesh)
    }

    /// Decodes a mesh and its optional parallel skin stream through the catalog's single
    /// embedded-or-standalone source resolver.
    pub(crate) fn load_mesh_cpu_source(&mut self, id: Uuid) -> Result<CpuMeshSource> {
        let entry = self
            .catalog
            .find(id)
            .ok_or(Error::NotInCatalog(id.value()))?;
        if entry.asset_type != AssetType::Mesh {
            return Err(Error::WrongAssetType {
                id: id.value(),
                wanted: "mesh",
            });
        }
        let container = entry.container;
        let rel_path = entry.path.clone();
        if container.value() != 0 {
            let model = self.load_model_asset(container).ok_or_else(|| {
                Error::Io(format!(
                    "mesh {}: container {} is not loadable",
                    id.value(),
                    container.value()
                ))
            })?;
            let source = self.chunk_source_for(&model, ChunkKind::Mesh, id);
            if source.is_empty() {
                return Err(Error::ContainerMissingSubAsset {
                    container: container.value(),
                    sub: id.value(),
                });
            }
            let bytes = source.read()?;
            return Ok(CpuMeshSource {
                mesh: load_mesh_from_bytes(&bytes)?,
                skin: load_mesh_skin_from_bytes(&bytes)?,
            });
        }
        let path = self.standalone_mesh_path(&rel_path);
        let bytes = ByteSource {
            path,
            ..ByteSource::default()
        }
        .read()?;
        Ok(CpuMeshSource {
            mesh: load_mesh_from_bytes(&bytes)?,
            skin: load_mesh_skin_from_bytes(&bytes)?,
        })
    }

    /// Seeds the asset-preview floor mesh (a unit cube) into the GPU mesh cache under the
    /// reserved [`PREVIEW_FLOOR_MESH_ID`], once.
    ///
    /// No catalog row — a preview floor entity carries a mesh id for the reserved id, and
    /// [`Self::load_mesh_asset`] resolves it cache-first, so it never serializes into the
    /// project. A `None` left on failure is a negative-cache marker;
    /// [`AssetServer::clear_asset_caches`] drops it on project load. Returns whether a
    /// live mesh is in the cache.
    pub fn ensure_preview_floor_mesh(&mut self, gpu: &dyn GpuUploader) -> bool {
        if let Some(cached) = self.mesh_by_uuid.get(&PREVIEW_FLOOR_MESH_ID.value()) {
            return cached.is_some();
        }
        let model = match translate_model(engine_asset_path("models/cube.gltf")) {
            Ok(model) => model,
            Err(err) => {
                tracing::warn!("preview floor mesh: {err}");
                self.mesh_by_uuid
                    .insert(PREVIEW_FLOOR_MESH_ID.value(), None);
                return false;
            }
        };
        let Some(mesh) = model.primary_mesh() else {
            tracing::warn!("preview floor mesh: no geometry");
            self.mesh_by_uuid
                .insert(PREVIEW_FLOOR_MESH_ID.value(), None);
            return false;
        };
        let hierarchy = match hierarchy_for_generated_mesh(mesh, &[]) {
            Ok(hierarchy) => hierarchy,
            Err(err) => {
                tracing::warn!("preview floor mesh: {err}");
                self.mesh_by_uuid
                    .insert(PREVIEW_FLOOR_MESH_ID.value(), None);
                return false;
            }
        };
        match gpu.upload_mesh(mesh, &hierarchy, &[], None, SdfSource::None) {
            Ok(mesh_ref) => {
                self.page_source_by_uuid.insert(
                    PREVIEW_FLOOR_MESH_ID.value(),
                    crate::page_stream::PagePayloadSource::Cooked(std::sync::Arc::new(hierarchy)),
                );
                self.mesh_by_uuid
                    .insert(PREVIEW_FLOOR_MESH_ID.value(), Some(mesh_ref));
                true
            }
            Err(err) => {
                tracing::warn!("preview floor mesh: {err}");
                self.mesh_by_uuid
                    .insert(PREVIEW_FLOOR_MESH_ID.value(), None);
                false
            }
        }
    }

    /// Generates + uploads a [`crate::BuiltinMesh`] into the GPU cache under its reserved
    /// id, returning the resolved mesh. No catalog row, no SDF bake (like the preview
    /// floor) — a primitive works with no project loaded. A `None` cached on failure is a
    /// negative-cache marker cleared by [`AssetServer::clear_asset_caches`].
    fn seed_builtin_mesh(
        &mut self,
        gpu: &dyn GpuUploader,
        builtin: crate::BuiltinMesh,
    ) -> Option<Arc<GpuMesh>> {
        let key = builtin.reserved_id().value();
        let mesh = builtin.geometry();
        let hierarchy = match hierarchy_for_generated_mesh(&mesh, &[]) {
            Ok(hierarchy) => hierarchy,
            Err(err) => {
                tracing::warn!("built-in {builtin:?} mesh: {err}");
                self.mesh_by_uuid.insert(key, None);
                return None;
            }
        };
        match gpu.upload_mesh(&mesh, &hierarchy, &[], None, SdfSource::None) {
            Ok(mesh_ref) => {
                self.page_source_by_uuid.insert(
                    key,
                    crate::page_stream::PagePayloadSource::Cooked(std::sync::Arc::new(hierarchy)),
                );
                self.mesh_by_uuid.insert(key, Some(mesh_ref.clone()));
                Some(mesh_ref)
            }
            Err(err) => {
                tracing::warn!("built-in {builtin:?} mesh: {err}");
                self.mesh_by_uuid.insert(key, None);
                None
            }
        }
    }

    /// Seeds the editor-camera gizmo mesh (the reserved [`crate::EDITOR_CAMERA_MESH_ID`])
    /// into the GPU mesh cache from the engine's `models/editor-camera.glb`. Its dark
    /// material is answered analytically from [`crate::EDITOR_CAMERA_MATERIAL_ID`] by the
    /// material loader, so nothing is cached for it here. A failed translate/upload caches
    /// `None`, so the load is attempted exactly once (until a project-switch cache clear
    /// re-seeds on demand).
    fn seed_editor_camera_mesh(&mut self, gpu: &dyn GpuUploader) -> Option<Arc<GpuMesh>> {
        let key = crate::EDITOR_CAMERA_MESH_ID.value();
        let fail = |assets: &mut Self, err: String| {
            tracing::warn!("editor camera model: {err}");
            assets.mesh_by_uuid.insert(key, None);
            None
        };
        let model = match translate_model(engine_asset_path("models/editor-camera.glb")) {
            Ok(model) => model,
            Err(err) => return fail(self, err.to_string()),
        };
        let skin = model
            .skin
            .as_ref()
            .map_or(&[][..], |skin| skin.stream.as_slice());
        let Some(mesh) = model.primary_mesh() else {
            return fail(self, "no geometry".to_owned());
        };
        let hierarchy = match hierarchy_for_generated_mesh(mesh, skin) {
            Ok(hierarchy) => hierarchy,
            Err(err) => return fail(self, err.to_string()),
        };
        let mesh_ref = match gpu.upload_mesh(mesh, &hierarchy, skin, None, SdfSource::None) {
            Ok(mesh_ref) => mesh_ref,
            Err(err) => return fail(self, err.to_string()),
        };
        self.page_source_by_uuid.insert(
            key,
            crate::page_stream::PagePayloadSource::Cooked(std::sync::Arc::new(hierarchy)),
        );
        self.mesh_by_uuid.insert(key, Some(mesh_ref.clone()));
        Some(mesh_ref)
    }

    /// The standalone-mesh path with the `meshes/` → `models/` fixup: a row whose file is
    /// absent under `assets/<path>` but begins `meshes/` is retried under `assets/models/`
    /// (where the importer writes baked `.smesh` siblings).
    fn standalone_mesh_path(&self, rel: &str) -> String {
        let full_path = format!("{}/{}", self.root.display(), rel);
        if !Path::new(&full_path).exists()
            && let Some(suffix) = rel.strip_prefix("meshes/")
        {
            return format!("{}/models/{suffix}", self.root.display());
        }
        full_path
    }
}

/// The dark, slightly-emissive resolved material the editor-camera gizmo renders with
/// (the reserved [`crate::EDITOR_CAMERA_MATERIAL_ID`], answered analytically by the
/// material loader like the default material).
pub(crate) fn editor_camera_material_asset() -> crate::MaterialAsset {
    use saffron_geometry::glam::{Vec3, Vec4};
    crate::MaterialAsset {
        base_color: Vec4::new(0.02, 0.018, 0.016, 1.0),
        roughness: 0.78,
        emissive: Vec3::splat(0.012),
        emissive_strength: 1.0,
        ..crate::material::default_material_asset()
    }
}

/// Reads + decodes + uploads the texture (the colorspace selects the uploader), or
/// returns `None` (with a warn) on any failure. The caller caches the outcome.
///
/// `as_height` uploads the decoded RGBA8 through [`GpuUploader::upload_height_texture`], which builds
/// the per-height min/max pyramid alongside the texture. A displacement height map is linear data, so
/// the `Hdr` float path is skipped for it (a height map is never an HDR panorama).
fn upload_texture_from_source(
    gpu: &dyn GpuUploader,
    sub_id: Uuid,
    source: &ByteSource,
    space: Colorspace,
    as_height: bool,
) -> Option<Arc<GpuTexture>> {
    let bytes = match source.read() {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!("texture {}: {err}", sub_id.value());
            return None;
        }
    };
    if space == Colorspace::Hdr && !as_height {
        return match decode_image_from_memory_hdr(&bytes) {
            Ok(decoded) => {
                match gpu.upload_texture_float(&decoded.rgba, decoded.width, decoded.height) {
                    Ok(texture) => Some(texture),
                    Err(err) => {
                        tracing::warn!("texture {}: {err}", sub_id.value());
                        None
                    }
                }
            }
            Err(err) => {
                tracing::warn!("texture {}: {err}", sub_id.value());
                None
            }
        };
    }
    match decode_image_from_memory(&bytes) {
        Ok(decoded) => {
            let result = if as_height {
                gpu.upload_height_texture(&decoded.rgba, decoded.width, decoded.height)
            } else {
                let srgb = space != Colorspace::Linear;
                gpu.upload_texture(&decoded.rgba, decoded.width, decoded.height, srgb)
            };
            match result {
                Ok(texture) => Some(texture),
                Err(err) => {
                    tracing::warn!("texture {}: {err}", sub_id.value());
                    None
                }
            }
        }
        Err(err) => {
            tracing::warn!("texture {}: {err}", sub_id.value());
            None
        }
    }
}

fn upload_coverage_texture_from_source(
    gpu: &dyn GpuUploader,
    id: Uuid,
    source: &ByteSource,
    cutoff_bits: u16,
) -> Option<Arc<GpuTexture>> {
    let bytes = source
        .read()
        .map_err(|error| {
            tracing::warn!("coverage texture {}: {error}", id.value());
        })
        .ok()?;
    let decoded = decode_image_from_memory(&bytes)
        .map_err(|error| {
            tracing::warn!("coverage texture {}: {error}", id.value());
        })
        .ok()?;
    let mips =
        crate::coverage_preserving_mips(&decoded.rgba, decoded.width, decoded.height, cutoff_bits);
    let levels = mips
        .iter()
        .map(|mip| saffron_rendering::TextureMipLevel {
            rgba: &mip.rgba,
            width: mip.width,
            height: mip.height,
        })
        .collect::<Vec<_>>();
    gpu.upload_texture_mips(&levels, false)
        .map_err(|error| {
            tracing::warn!("coverage texture {}: {error}", id.value());
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ash::vk;
    use saffron_geometry::glam::{Vec2, Vec3};
    use saffron_geometry::{
        ChunkKind, ContainerChunk, Mesh, Submesh, Vertex, save_mesh_to_buffer, write_container,
    };
    use saffron_rendering::{
        BindlessFreeList, Descriptors, Device, SurfaceSource, Uploader, validation_issue_count,
    };
    use saffron_scene::{AssetEntry, AssetType, Colorspace};

    use crate::{ContainerMetadata, RendererUploader};

    /// A unique scratch dir under the system temp, removed and recreated per test.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("saffron-assets-load-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A single-triangle `.smesh` byte image.
    fn triangle_mesh() -> Mesh {
        Mesh {
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
        }
    }

    /// A 2×2 RGBA8 PNG (the encoded bytes the texture loaders decode).
    fn png_2x2() -> Vec<u8> {
        let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 120, 60, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        buffer
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    /// A `GpuUploader` that wraps the live renderer and counts mesh/texture uploads, so a
    /// test can prove a cache hit skips the whole load (no re-decode, no re-upload).
    struct CountingUploader<'a> {
        inner: RendererUploader<'a>,
        mesh_uploads: AtomicUsize,
        texture_uploads: AtomicUsize,
        fail_mesh_uploads: bool,
    }

    impl<'a> CountingUploader<'a> {
        fn new(uploader: &'a Uploader, descriptors: &'a Descriptors) -> Self {
            Self {
                inner: RendererUploader::new(uploader, descriptors, true),
                mesh_uploads: AtomicUsize::new(0),
                texture_uploads: AtomicUsize::new(0),
                fail_mesh_uploads: false,
            }
        }

        /// Every mesh upload counts an attempt and then fails, for the negative-cache path.
        fn fail_mesh_uploads(mut self) -> Self {
            self.fail_mesh_uploads = true;
            self
        }
    }

    impl GpuUploader for CountingUploader<'_> {
        fn upload_mesh(
            &self,
            mesh: &Mesh,
            hierarchy: &saffron_geometry::PortableVirtualHierarchy,
            skin: &[saffron_geometry::VertexSkin],
            morph: Option<&saffron_geometry::MorphData>,
            sdf: saffron_rendering::SdfSource<'_>,
        ) -> saffron_rendering::Result<Arc<GpuMesh>> {
            self.mesh_uploads.fetch_add(1, Ordering::SeqCst);
            if self.fail_mesh_uploads {
                return Err(saffron_rendering::Error::InvalidUploadData(
                    "injected upload failure".to_owned(),
                ));
            }
            self.inner.upload_mesh(mesh, hierarchy, skin, morph, sdf)
        }

        fn upload_texture(
            &self,
            rgba: &[u8],
            width: u32,
            height: u32,
            srgb: bool,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            self.texture_uploads.fetch_add(1, Ordering::SeqCst);
            self.inner.upload_texture(rgba, width, height, srgb)
        }

        fn upload_texture_float(
            &self,
            rgba: &[f32],
            width: u32,
            height: u32,
        ) -> saffron_rendering::Result<Arc<GpuTexture>> {
            self.texture_uploads.fetch_add(1, Ordering::SeqCst);
            self.inner.upload_texture_float(rgba, width, height)
        }

        fn skinning_enabled(&self) -> bool {
            self.inner.skinning_enabled()
        }
    }

    /// A live headless device + uploader + descriptors, or `None` (no Vulkan ICD) so the
    /// GPU-backed tests skip rather than fail off-hardware.
    struct GpuFixture {
        uploader: Uploader,
        descriptors: Descriptors,
        device: Device,
    }

    fn gpu_or_skip() -> Option<GpuFixture> {
        let device = match Device::new(&SurfaceSource::Offscreen) {
            Ok(device) => device,
            Err(err) => {
                eprintln!("skipping (no Vulkan device): {err}");
                return None;
            }
        };
        let free_list: BindlessFreeList = Arc::new(std::sync::Mutex::new(Vec::new()));
        let descriptors = Descriptors::new(&device, &free_list).expect("Descriptors::new");
        let queue = device.graphics_queue.clone();
        let uploader = Uploader::new(&device, &queue).expect("Uploader::new");
        Some(GpuFixture {
            uploader,
            descriptors,
            device,
        })
    }

    impl GpuFixture {
        fn counting(&self) -> CountingUploader<'_> {
            CountingUploader::new(&self.uploader, &self.descriptors)
        }

        /// Idle the GPU before teardown so cached `Arc<GpuMesh>`/`Arc<GpuTexture>` drop
        /// while the device is alive and quiescent (the README §3 discipline), then tear
        /// down the borrowing GPU sub-state in the correct order (uploader's command pool
        /// + descriptors' set layouts borrow the device, so they drop before it).
        fn teardown(self, mut assets: AssetServer) {
            let GpuFixture {
                device,
                descriptors,
                uploader,
            } = self;
            // GPU quiescent first, then drop the cached GPU `Arc`s (their `Drop` frees the
            // VMA allocations + returns the bindless slots), then the borrowing sub-state,
            // then the device last.
            device.wait_idle().expect("idle before teardown");
            assets.clear_asset_caches();
            drop(assets);
            drop(uploader);
            drop(descriptors);
            drop(device);
        }
    }

    /// Writes a standalone `.smesh` under `<root>/meshes/<name>.smesh` and registers a
    /// `Mesh` catalog row for `id`.
    fn write_standalone_mesh(assets: &mut AssetServer, id: Uuid, name: &str) {
        let rel = format!("meshes/{name}.smesh");
        let full = format!("{}/{rel}", assets.root.display());
        std::fs::create_dir_all(format!("{}/meshes", assets.root.display())).unwrap();
        std::fs::write(
            &full,
            save_mesh_to_buffer(&triangle_mesh(), &[], None).unwrap(),
        )
        .unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Mesh,
            path: rel,
            chunk: -1,
            ..AssetEntry::default()
        });
    }

    /// Writes a standalone texture (the PNG bytes) under `<root>/textures/<name>.png` and
    /// registers a `Texture` catalog row for `id` with the given colorspace provenance.
    fn write_standalone_texture(
        assets: &mut AssetServer,
        id: Uuid,
        name: &str,
        colorspace: Colorspace,
        hdr: bool,
        linear: bool,
    ) {
        let rel = format!("textures/{name}.png");
        let full = format!("{}/{rel}", assets.root.display());
        std::fs::write(&full, png_2x2()).unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: name.to_owned(),
            asset_type: AssetType::Texture,
            path: rel,
            chunk: -1,
            colorspace,
            hdr,
            linear,
            ..AssetEntry::default()
        });
    }

    #[test]
    fn colorspace_from_flags_maps_the_chunk_flag_word() {
        assert_eq!(colorspace_from_flags(0), Colorspace::Auto);
        assert_eq!(colorspace_from_flags(1), Colorspace::Srgb);
        assert_eq!(colorspace_from_flags(2), Colorspace::Linear);
        assert_eq!(colorspace_from_flags(3), Colorspace::Hdr);
        // An unknown flag falls back to sRGB.
        assert_eq!(colorspace_from_flags(99), Colorspace::Srgb);
    }

    #[test]
    fn load_mesh_asset_caches_a_live_arc_and_reuses_it() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let dir = scratch("meshreuse");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        let id = Uuid(5000);
        write_standalone_mesh(&mut assets, id, "tri");

        let gpu = fx.counting();
        let first = assets.load_mesh_asset(&gpu, id).expect("uploads the mesh");
        assert_eq!(first.index_count, 3);
        assert_eq!(gpu.mesh_uploads.load(Ordering::SeqCst), 1);

        // Delete the source: a re-load would now fail. The cached Arc must survive, and
        // the second call reuses it (no re-decode, no re-upload).
        std::fs::remove_file(format!("{}/meshes/tri.smesh", assets.root.display())).unwrap();
        let second = assets.load_mesh_asset(&gpu, id).expect("served from cache");
        assert!(Arc::ptr_eq(&first, &second), "second call reuses the Arc");
        assert_eq!(
            gpu.mesh_uploads.load(Ordering::SeqCst),
            1,
            "a cache hit must not re-upload"
        );

        drop(first);
        drop(second);
        fx.teardown(assets);
        assert_eq!(before, validation_issue_count(), "uploads validation-clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn decode_failure_negative_caches_and_does_not_retry() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("decodefail");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // A texture row whose file holds bytes that are not a decodable image.
        let id = Uuid(5100);
        let rel = "textures/garbage.png";
        std::fs::write(format!("{}/{rel}", root.display()), b"not an image").unwrap();
        assets.catalog.put(AssetEntry {
            id,
            name: "garbage".to_owned(),
            asset_type: AssetType::Texture,
            path: rel.to_owned(),
            chunk: -1,
            colorspace: Colorspace::Srgb,
            ..AssetEntry::default()
        });

        let gpu = fx.counting();
        assert!(assets.load_texture_asset(&gpu, id).is_none());
        assert!(matches!(
            assets.texture_by_uuid.get(&id.value()),
            Some(None)
        ));
        // The decode ran once; the second call is a negative-cache hit (no decode, no
        // upload attempt).
        assert!(assets.load_texture_asset(&gpu, id).is_none());
        assert_eq!(gpu.texture_uploads.load(Ordering::SeqCst), 0);

        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upload_failure_negative_caches_and_does_not_retry() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("uploadfail");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // A valid `.smesh` whose upload the uploader fails by injection — the
        // upload-failure path.
        let id = Uuid(5200);
        write_standalone_mesh(&mut assets, id, "uploadfail");

        let gpu = fx.counting().fail_mesh_uploads();
        assert!(assets.load_mesh_asset(&gpu, id).is_none());
        assert!(matches!(assets.mesh_by_uuid.get(&id.value()), Some(None)));
        assert_eq!(
            gpu.mesh_uploads.load(Ordering::SeqCst),
            1,
            "upload attempted once"
        );
        // The second call is a negative-cache hit — no second upload attempt.
        assert!(assets.load_mesh_asset(&gpu, id).is_none());
        assert_eq!(gpu.mesh_uploads.load(Ordering::SeqCst), 1);

        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dangling_texture_id_negative_caches_once() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("dangling");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // No catalog row for this id at all.
        let id = Uuid(5300);

        let gpu = fx.counting();
        assert!(assets.load_texture_asset(&gpu, id).is_none());
        assert!(matches!(
            assets.texture_by_uuid.get(&id.value()),
            Some(None)
        ));
        // The second call is a negative-cache hit — the loader does not re-attempt.
        assert!(assets.load_texture_asset(&gpu, id).is_none());
        assert_eq!(gpu.texture_uploads.load(Ordering::SeqCst), 0);

        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn colorspace_selects_the_upload_format() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let before = validation_issue_count();
        let dir = scratch("colorspace");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        write_standalone_texture(
            &mut assets,
            Uuid(6001),
            "srgb",
            Colorspace::Srgb,
            false,
            false,
        );
        write_standalone_texture(
            &mut assets,
            Uuid(6002),
            "auto",
            Colorspace::Auto,
            false,
            false,
        );
        write_standalone_texture(
            &mut assets,
            Uuid(6003),
            "linear",
            Colorspace::Linear,
            false,
            false,
        );
        write_standalone_texture(
            &mut assets,
            Uuid(6004),
            "hdr",
            Colorspace::Hdr,
            false,
            false,
        );

        let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);

        let srgb = assets.load_texture_asset(&gpu, Uuid(6001)).expect("srgb");
        assert_eq!(srgb.format, vk::Format::R8G8B8A8_SRGB);
        let auto = assets.load_texture_asset(&gpu, Uuid(6002)).expect("auto");
        assert_eq!(auto.format, vk::Format::R8G8B8A8_SRGB, "Auto uploads sRGB");
        let linear = assets.load_texture_asset(&gpu, Uuid(6003)).expect("linear");
        assert_eq!(linear.format, vk::Format::R8G8B8A8_UNORM, "Linear → unorm");
        let hdr = assets.load_texture_asset(&gpu, Uuid(6004)).expect("hdr");
        assert_eq!(
            hdr.format,
            vk::Format::R16G16B16A16_SFLOAT,
            "Hdr → the float uploader"
        );

        drop(srgb);
        drop(auto);
        drop(linear);
        drop(hdr);
        fx.teardown(assets);
        assert_eq!(before, validation_issue_count(), "uploads validation-clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smeta_colorspace_overrides_the_hdr_linear_provenance() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("override");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
        // An explicit `.smeta` colorspace (Linear) on a row that *also* carries the `hdr`
        // provenance flag: the explicit colorspace wins, so the upload is unorm, not float.
        write_standalone_texture(
            &mut assets,
            Uuid(6100),
            "ovr",
            Colorspace::Linear,
            true,
            false,
        );

        let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
        let tex = assets
            .load_texture_asset(&gpu, Uuid(6100))
            .expect("override");
        assert_eq!(
            tex.format,
            vk::Format::R8G8B8A8_UNORM,
            "the explicit .smeta colorspace beats the hdr provenance"
        );

        drop(tex);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_texture_uses_the_chunk_flag_colorspace() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("embeddedtex");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        // A container with a META chunk + a texture chunk flagged Linear (flag word 2).
        let mut meta = ContainerMetadata {
            model_id: Uuid(7000),
            name: "m".to_owned(),
            source_format: "gltf".to_owned(),
            ..ContainerMetadata::default()
        };
        meta.sub_assets.push(crate::SubAsset {
            sub_id: Uuid(7001),
            asset_type: AssetType::Texture,
            name: "albedo".to_owned(),
            chunk: 1,
            colorspace: "linear".to_owned(),
            ..crate::SubAsset::default()
        });
        let meta_bytes = encode_meta(&meta);
        let png = png_2x2();
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Texture,
                sub_id: 7001,
                flags: 2, // Colorspace::Linear
                bytes: &png,
            },
        ];
        let rel = "models/m.smodel";
        let full = format!("{}/{rel}", root.display());
        write_container(&full, &chunks).unwrap();
        assets.catalog.put(AssetEntry {
            id: Uuid(7000),
            name: "m".to_owned(),
            asset_type: AssetType::Model,
            path: rel.to_owned(),
            chunk: -1,
            ..AssetEntry::default()
        });

        let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
        let tex = assets
            .resolve_texture(&gpu, Uuid(7000), Uuid(7001))
            .expect("resolves the embedded texture");
        assert_eq!(
            tex.format,
            vk::Format::R8G8B8A8_UNORM,
            "the Linear chunk flag selects unorm"
        );

        drop(tex);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Re-encode metadata via the public model codec (kept local so the test reads
    /// without a `use` for an internal-only path).
    fn encode_meta(meta: &ContainerMetadata) -> Vec<u8> {
        crate::encode_container_metadata(meta)
    }

    #[test]
    fn ensure_preview_floor_mesh_seeds_the_reserved_id_without_a_catalog_row() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        // The engine `models/cube.gltf` resolves via the exe-walk in `engine_asset_path`
        // (the test binary runs from `target/<profile>/deps/`, below the copied `models/`).
        if !engine_asset_path("models/cube.gltf").exists() {
            eprintln!("skipping: engine models/cube.gltf not staged beside the test binary");
            fx.teardown(AssetServer::new(scratch("previewfloor-skip")));
            return;
        }
        let before = validation_issue_count();
        let dir = scratch("previewfloor");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
        assert!(assets.ensure_preview_floor_mesh(&gpu), "the cube uploads");
        // Seeded into the mesh cache under the reserved id — and NOT into the catalog.
        assert!(matches!(
            assets.mesh_by_uuid.get(&PREVIEW_FLOOR_MESH_ID.value()),
            Some(Some(_))
        ));
        assert!(assets.catalog.find(PREVIEW_FLOOR_MESH_ID).is_none());
        // A second call is a cache hit (still true), no re-upload.
        assert!(assets.ensure_preview_floor_mesh(&gpu));

        fx.teardown(assets);
        assert_eq!(before, validation_issue_count(), "uploads validation-clean");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_camera_mesh_seeds_once_and_caches() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        if !engine_asset_path("models/editor-camera.glb").exists() {
            eprintln!("skipping: engine editor-camera.glb not staged beside the test binary");
            fx.teardown(AssetServer::new(scratch("editorcam-skip")));
            return;
        }
        let dir = scratch("editorcam");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        let gpu = RendererUploader::new(&fx.uploader, &fx.descriptors, true);
        let first = assets
            .load_mesh_asset(&gpu, crate::EDITOR_CAMERA_MESH_ID)
            .expect("the editor-camera model seeds on first resolve");
        // The gizmo's dark material is answered analytically by the material loader, so it
        // resolves without a catalog row and without a cache entry.
        let material = assets
            .resolve_slot_material(crate::EDITOR_CAMERA_MATERIAL_ID, &serde_json::Value::Null);
        assert_eq!(
            material.base_color,
            editor_camera_material_asset().base_color,
            "the reserved id resolves to the dark gizmo material"
        );

        // The cache contract: a second resolve returns the same live `Arc` without
        // re-translating.
        let second = assets
            .load_mesh_asset(&gpu, crate::EDITOR_CAMERA_MESH_ID)
            .expect("cached resolve");
        assert!(Arc::ptr_eq(&first, &second), "the seeded mesh is cached");

        drop(first);
        drop(second);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_anim_clip_resolves_standalone_and_errors_on_missing_or_wrong_type() {
        let dir = scratch("animclip");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        // A standalone `.sanim` clip.
        let clip = saffron_geometry::AnimClip {
            name: "walk".to_owned(),
            duration: 1.5,
            tracks: Vec::new(),
        };
        let rel = "animations/walk.sanim";
        std::fs::create_dir_all(format!("{}/animations", root.display())).unwrap();
        std::fs::write(
            format!("{}/{rel}", root.display()),
            saffron_geometry::save_animation_to_buffer(&clip),
        )
        .unwrap();
        let id = Uuid(8000);
        assets.catalog.put(AssetEntry {
            id,
            name: "walk".to_owned(),
            asset_type: AssetType::Animation,
            path: rel.to_owned(),
            chunk: -1,
            ..AssetEntry::default()
        });

        let loaded = assets
            .load_anim_clip(id)
            .expect("loads the standalone clip");
        assert_eq!(loaded.name, "walk");
        assert!((loaded.duration - 1.5).abs() < 1e-6);

        // A missing id is a typed Err, not a panic.
        assert!(matches!(
            assets.load_anim_clip(Uuid(9999)),
            Err(Error::NotInCatalog(9999))
        ));

        // A wrong-type id (a mesh) is a typed Err.
        write_standalone_mesh(&mut assets, Uuid(8100), "tri");
        assert!(matches!(
            assets.load_anim_clip(Uuid(8100)),
            Err(Error::WrongAssetType { id: 8100, .. })
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_anim_clip_resolves_an_embedded_chunk() {
        let dir = scratch("animembedded");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        let clip = saffron_geometry::AnimClip {
            name: "run".to_owned(),
            duration: 2.0,
            tracks: Vec::new(),
        };
        let clip_bytes = saffron_geometry::save_animation_to_buffer(&clip);
        let mut meta = ContainerMetadata {
            model_id: Uuid(8200),
            name: "rig".to_owned(),
            ..ContainerMetadata::default()
        };
        meta.sub_assets.push(crate::SubAsset {
            sub_id: Uuid(8201),
            asset_type: AssetType::Animation,
            name: "run".to_owned(),
            chunk: 1,
            duration: 2.0,
            ..crate::SubAsset::default()
        });
        let meta_bytes = encode_meta(&meta);
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Animation,
                sub_id: 8201,
                flags: 0,
                bytes: &clip_bytes,
            },
        ];
        let rel = "models/rig.smodel";
        write_container(format!("{}/{rel}", root.display()), &chunks).unwrap();
        assets.catalog.put(AssetEntry {
            id: Uuid(8200),
            name: "rig".to_owned(),
            asset_type: AssetType::Model,
            path: rel.to_owned(),
            chunk: -1,
            ..AssetEntry::default()
        });
        assets.catalog.put(AssetEntry {
            id: Uuid(8201),
            name: "run".to_owned(),
            asset_type: AssetType::Animation,
            path: rel.to_owned(),
            container: Uuid(8200),
            chunk: 1,
            ..AssetEntry::default()
        });

        let loaded = assets
            .load_anim_clip(Uuid(8201))
            .expect("loads the embedded clip");
        assert_eq!(loaded.name, "run");

        // A clip whose container row is present but the chunk is absent → typed Err.
        assets.catalog.put(AssetEntry {
            id: Uuid(8202),
            name: "ghost".to_owned(),
            asset_type: AssetType::Animation,
            path: rel.to_owned(),
            container: Uuid(8200),
            chunk: 9,
            ..AssetEntry::default()
        });
        assert!(matches!(
            assets.load_anim_clip(Uuid(8202)),
            Err(Error::ContainerMissingSubAsset {
                container: 8200,
                sub: 8202
            })
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_mesh_cpu_asset_resolves_embedded_and_standalone() {
        let dir = scratch("cpumesh");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

        // Standalone.
        let id = Uuid(8300);
        write_standalone_mesh(&mut assets, id, "tri");
        let cpu = assets
            .load_mesh_cpu_asset(id)
            .expect("decodes the standalone mesh");
        assert_eq!(cpu.vertices.len(), 3);
        assert_eq!(cpu.indices, vec![0, 1, 2]);

        // Embedded.
        let mesh_bytes = save_mesh_to_buffer(&triangle_mesh(), &[], None).unwrap();
        let mut meta = ContainerMetadata {
            model_id: Uuid(8400),
            name: "c".to_owned(),
            ..ContainerMetadata::default()
        };
        meta.sub_assets.push(crate::SubAsset {
            sub_id: Uuid(8401),
            asset_type: AssetType::Mesh,
            name: "c_mesh".to_owned(),
            chunk: 1,
            ..crate::SubAsset::default()
        });
        let meta_bytes = encode_meta(&meta);
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: 8401,
                flags: 0,
                bytes: &mesh_bytes,
            },
        ];
        let rel = "models/c.smodel";
        write_container(format!("{}/{rel}", root.display()), &chunks).unwrap();
        assets.catalog.put(AssetEntry {
            id: Uuid(8400),
            name: "c".to_owned(),
            asset_type: AssetType::Model,
            path: rel.to_owned(),
            chunk: -1,
            ..AssetEntry::default()
        });
        assets.catalog.put(AssetEntry {
            id: Uuid(8401),
            name: "c_mesh".to_owned(),
            asset_type: AssetType::Mesh,
            path: rel.to_owned(),
            container: Uuid(8400),
            chunk: 1,
            ..AssetEntry::default()
        });
        let embedded = assets
            .load_mesh_cpu_asset(Uuid(8401))
            .expect("decodes the embedded mesh chunk");
        assert_eq!(embedded.vertices.len(), 3);

        // Missing / wrong-type are typed Errs.
        assert!(matches!(
            assets.load_mesh_cpu_asset(Uuid(1)),
            Err(Error::NotInCatalog(1))
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
