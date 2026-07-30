use std::sync::Arc;

use saffron_geometry::translate_model;
use saffron_rendering::{GpuMesh, SdfSource};

use crate::gpu::GpuUploader;
use crate::{AssetServer, PREVIEW_FLOOR_MESH_ID};

use super::{engine_asset_path, hierarchy_for_generated_mesh};

impl AssetServer {
    /// Seeds the asset-preview floor mesh (a unit cube) into the GPU mesh cache under the
    /// reserved [`PREVIEW_FLOOR_MESH_ID`], once.
    ///
    /// No catalog row — a preview floor entity carries the reserved id, and
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
                    crate::page_stream::PagePayloadSource::Cooked(Arc::new(hierarchy)),
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
    pub(super) fn seed_builtin_mesh(
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
                    crate::page_stream::PagePayloadSource::Cooked(Arc::new(hierarchy)),
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
    pub(super) fn seed_editor_camera_mesh(
        &mut self,
        gpu: &dyn GpuUploader,
    ) -> Option<Arc<GpuMesh>> {
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
            crate::page_stream::PagePayloadSource::Cooked(Arc::new(hierarchy)),
        );
        self.mesh_by_uuid.insert(key, Some(mesh_ref.clone()));
        Some(mesh_ref)
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

#[cfg(test)]
mod tests {
    use saffron_rendering::validation_issue_count;

    use super::*;

    use crate::RendererUploader;

    use super::super::test_support::{gpu_or_skip, scratch};

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
        assert!(matches!(
            assets.mesh_by_uuid.get(&PREVIEW_FLOOR_MESH_ID.value()),
            Some(Some(_))
        ));
        assert!(assets.catalog.find(PREVIEW_FLOOR_MESH_ID).is_none());
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

        let second = assets
            .load_mesh_asset(&gpu, crate::EDITOR_CAMERA_MESH_ID)
            .expect("cached resolve");
        assert!(Arc::ptr_eq(&first, &second), "the seeded mesh is cached");

        drop(first);
        drop(second);
        fx.teardown(assets);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
