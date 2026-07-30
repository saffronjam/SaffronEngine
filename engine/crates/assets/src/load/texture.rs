use std::sync::Arc;

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind, DecodedImage, decode_image_from_memory, decode_image_from_memory_hdr,
};
use saffron_rendering::GpuTexture;
use saffron_scene::{AssetType, Colorspace};

use crate::AssetServer;
use crate::gpu::GpuUploader;
use crate::model::ByteSource;

use super::colorspace_from_flags;

impl AssetServer {
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
    /// through [`GpuUploader::upload_height_texture`] (building the min/max pyramid) and caches into
    /// the separate `height_texture_by_uuid` map, so a displacement height map is a distinct GPU
    /// resource from the same image used as a plain texture.
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

    /// The role-aware core behind [`Self::resolve_texture`]: `as_height` resolves the embedded
    /// texture as a displacement height map (building its min/max pyramid, caching separately).
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
    /// tessellation factor kernel) and the result is cached separately. The draw path calls this
    /// for a [`saffron_core::HeightMode::Displacement`] material's height slot; a `None` falls back
    /// to default-white (no displacement).
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

    /// The role-aware core behind [`Self::load_texture_asset`] /
    /// [`Self::load_height_texture_asset`]: one resolution (catalog → embedded/standalone fork,
    /// colorspace), with `as_height` selecting the pyramid-building upload + the separate height
    /// cache.
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
}

/// Reads + decodes + uploads the texture (the colorspace selects the uploader), or returns `None`
/// (with a warn) on any failure; the caller caches the outcome. `as_height` routes through
/// [`GpuUploader::upload_height_texture`], which builds the min/max pyramid alongside the texture,
/// and skips the `Hdr` float path — a height map is linear data, never an HDR panorama.
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
    use std::sync::atomic::Ordering;

    use ash::vk;
    use saffron_geometry::{ChunkKind, ContainerChunk, write_container};
    use saffron_rendering::validation_issue_count;
    use saffron_scene::AssetEntry;

    use super::*;

    use crate::{ContainerMetadata, RendererUploader};

    use super::super::test_support::{
        encode_meta, gpu_or_skip, png_2x2, scratch, write_standalone_texture,
    };

    #[test]
    fn decode_failure_negative_caches_and_does_not_retry() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("decodefail");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
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
        assert!(assets.load_texture_asset(&gpu, id).is_none());
        assert_eq!(gpu.texture_uploads.load(Ordering::SeqCst), 0);

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
        let id = Uuid(5300);

        let gpu = fx.counting();
        assert!(assets.load_texture_asset(&gpu, id).is_none());
        assert!(matches!(
            assets.texture_by_uuid.get(&id.value()),
            Some(None)
        ));
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

    /// An explicit `.smeta` colorspace (Linear) on a row that also carries the `hdr` provenance
    /// flag: the explicit colorspace wins, so the upload is unorm, not float.
    #[test]
    fn smeta_colorspace_overrides_the_hdr_linear_provenance() {
        let Some(fx) = gpu_or_skip() else {
            return;
        };
        let dir = scratch("override");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);
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
                flags: 2,
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
}
