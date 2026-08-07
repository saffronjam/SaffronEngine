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
//! [`Colorspace::Linear`] → unorm; [`Colorspace::Srgb`]/[`Colorspace::Auto`] → sRGB.
//! A standalone texture's explicit `.smeta` colorspace wins, else the row's `hdr`/`linear`
//! provenance.
//!
//! [`AssetServer::load_anim_clip`] and [`AssetServer::load_mesh_cpu_asset`] are
//! `Result`-returning one-shots (not cache-backed) used by the animation runtime and
//! physics cooking: they resolve the same embedded/standalone fork but read CPU data.

mod builtin;
mod mesh;
mod texture;

#[cfg(test)]
mod test_support;

pub(crate) use builtin::editor_camera_material_asset;
pub(crate) use mesh::CpuMeshSource;

use std::path::{Path, PathBuf};

use saffron_core::Uuid;
use saffron_geometry::{
    AnimClip, ChunkKind, Mesh, PortableHierarchyInput, PortableVirtualHierarchy, VertexSkin,
    cook_portable_virtual_hierarchy, load_animation, load_animation_from_bytes,
};
use saffron_scene::{AssetType, Colorspace};

use crate::AssetServer;
use crate::error::{Error, Result};
use crate::gpu::GpuUploader;

fn hierarchy_for_generated_mesh(
    mesh: &Mesh,
    skin: &[VertexSkin],
) -> saffron_geometry::Result<PortableVirtualHierarchy> {
    let input = PortableHierarchyInput::from_mesh(mesh, skin)?;
    cook_portable_virtual_hierarchy(&input)
}

/// The `Colorspace` a `.smodel` texture chunk's `flags` word encodes: the container writes the
/// [`Colorspace`] discriminant straight into the chunk flags (`Auto = 0`, `Srgb = 1`,
/// `Linear = 2`, `Hdr = 3`). An unknown value falls back to [`Colorspace::Srgb`].
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
/// `target/<profile>/`). An absolute `relative` is returned as-is.
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

/// One unit of the project loader's residency prefetch: the GPU state one scene reference
/// warms ahead of the draw path.
#[derive(Clone, Debug, PartialEq)]
pub enum WarmItem {
    /// A `Mesh.mesh` reference: the GPU mesh (with its SDF sidecar).
    Mesh(Uuid),
    /// A directly referenced texture (the environment sky panorama).
    Texture(Uuid),
    /// One `MaterialSet` slot: the resolved `.smat` (overrides applied) and every texture it
    /// binds, each through its canonical role (plain / height pyramid / coverage mips).
    MaterialSlot {
        /// The referenced `.smat` id (`0` = the built-in default).
        material: Uuid,
        /// The slot's sparse parameter overrides.
        overrides: saffron_json::Value,
    },
}

impl AssetServer {
    /// Warms one residency item into its GPU caches, for the project loader's prefetch.
    /// Idempotent (cache-hit fast path), so the loader can call it once per residency step
    /// without re-uploading.
    pub fn warm(&mut self, gpu: &dyn GpuUploader, item: &WarmItem) {
        match item {
            WarmItem::Mesh(id) => {
                self.load_mesh_asset(gpu, *id);
            }
            WarmItem::Texture(id) => {
                self.load_texture_asset(gpu, *id);
            }
            WarmItem::MaterialSlot {
                material,
                overrides,
            } => {
                let resolved = self.resolve_slot_material(*material, overrides);
                self.resolve_material_asset(gpu, &resolved);
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use saffron_geometry::{ContainerChunk, save_mesh_to_buffer, write_container};
    use saffron_scene::AssetEntry;

    use crate::ContainerMetadata;

    use super::test_support::{encode_meta, scratch, triangle_mesh, write_standalone_mesh};

    #[test]
    fn colorspace_from_flags_maps_the_chunk_flag_word() {
        assert_eq!(colorspace_from_flags(0), Colorspace::Auto);
        assert_eq!(colorspace_from_flags(1), Colorspace::Srgb);
        assert_eq!(colorspace_from_flags(2), Colorspace::Linear);
        assert_eq!(colorspace_from_flags(3), Colorspace::Hdr);
        assert_eq!(colorspace_from_flags(99), Colorspace::Srgb);
    }

    #[test]
    fn load_anim_clip_resolves_standalone_and_errors_on_missing_or_wrong_type() {
        let dir = scratch("animclip");
        let root = dir.join("project").join("assets");
        let mut assets = AssetServer::new(&root);

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

        assert!(matches!(
            assets.load_anim_clip(Uuid(9999)),
            Err(Error::NotInCatalog(9999))
        ));

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

        let id = Uuid(8300);
        write_standalone_mesh(&mut assets, id, "tri");
        let cpu = assets
            .load_mesh_cpu_asset(id)
            .expect("decodes the standalone mesh");
        assert_eq!(cpu.vertices.len(), 3);
        assert_eq!(cpu.indices, vec![0, 1, 2]);

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

        assert!(matches!(
            assets.load_mesh_cpu_asset(Uuid(1)),
            Err(Error::NotInCatalog(1))
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
