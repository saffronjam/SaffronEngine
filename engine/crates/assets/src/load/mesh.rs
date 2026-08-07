use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind, Mesh, MeshBvh, VertexSkin, load_mesh_from_bytes, load_mesh_hierarchy_from_bytes,
    load_mesh_morph_from_bytes, load_mesh_skin_from_bytes,
};
use saffron_rendering::{GpuMesh, SdfBake, SdfSource};
use saffron_scene::AssetType;

use crate::AssetServer;
use crate::error::{Error, Result};
use crate::gpu::GpuUploader;
use crate::model::ByteSource;

/// Complete CPU payload resolved from one catalog mesh row.
pub(crate) struct CpuMeshSource {
    pub(crate) mesh: Mesh,
    pub(crate) skin: Vec<VertexSkin>,
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
        let read_started = Instant::now();
        let bytes = match source.read() {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!("mesh {}: {err}", sub_id.value());
                return None;
            }
        };
        let read_ms = read_started.elapsed().as_millis();
        let decode_started = Instant::now();
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
        let decode_ms = decode_started.elapsed().as_millis();
        let upload_started = Instant::now();
        match gpu.upload_mesh(
            &mesh,
            &hierarchy,
            &skin,
            morph.as_ref(),
            sdf_bake.as_ref().map_or(SdfSource::None, SdfSource::Bake),
        ) {
            Ok(mesh_ref) => {
                tracing::debug!(
                    "mesh {} resident — read {read_ms} ms ({} KiB), decode {decode_ms} ms, upload {} ms",
                    sub_id.value(),
                    bytes.len() / 1024,
                    upload_started.elapsed().as_millis()
                );
                Some(mesh_ref)
            }
            Err(err) => {
                tracing::warn!("mesh {}: {err}", sub_id.value());
                None
            }
        }
    }

    /// The per-mesh SDF bake request for a project mesh: the default `resolution_scale` and
    /// the project's `assets/cache` sidecar directory (a content hash of the geometry keys
    /// the cache, so a re-load skips the GPU bake). The gizmo/preview meshes pass `None`.
    pub(super) fn sdf_bake(&self) -> SdfBake {
        SdfBake {
            resolution_scale: 1.0,
            cache_dir: Some(self.root.join("cache")),
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

    /// Resolves a mesh id to a GPU mesh, loading + uploading the baked `.smesh` on a
    /// cache miss. An embedded sub-asset routes through its container; a standalone file
    /// reads its path (with the `meshes/` → `models/` path fixup). Returns `None`
    /// (negative-cached) for an unregistered, wrong-type, or unreadable asset.
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
        self.load_mesh_from_source(gpu, id, &source, Some(self.sdf_bake()))
    }

    /// Decodes a mesh id's baked `.smesh` to a CPU [`Mesh`] (for physics cooking): a catalog
    /// lookup + bytes read + decode, with no GPU upload and no cache entry.
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use saffron_rendering::validation_issue_count;

    use super::*;

    use super::super::test_support::{gpu_or_skip, scratch, write_standalone_mesh};

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
    fn upload_failure_negative_caches_and_does_not_retry() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("uploadfail");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
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
        assert!(assets.load_mesh_asset(&gpu, id).is_none());
        assert_eq!(gpu.mesh_uploads.load(Ordering::SeqCst), 1);

        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
