use std::collections::HashSet;

use saffron_core::Uuid;
use saffron_geometry::ChunkKind;
use saffron_geometry::glam::Mat4;
use saffron_scene::{AssetType, Colorspace};

use crate::material::MaterialAsset;
use crate::{AssetServer, Error, Result};

use super::hash::{content_hash_from_content, thumbnail_material_hash};
use super::{ModelMeshChunk, ThumbnailContent, ThumbnailJob, ThumbnailTextureSource};

/// Builds the resolved [`ThumbnailJob`] for `{id, size}` from the catalog/material/container state,
/// plus its cache stamp. Returns the job alone — the caller decides cache-hit vs. enqueue.
///
/// # Errors
///
/// [`Error::NotInCatalog`] for a missing id, [`Error::Thumbnail`] for an asset with no
/// thumbnail or an unloadable container/mesh chunk.
pub(super) fn build_thumbnail_job(
    assets: &mut AssetServer,
    id: Uuid,
    size: u32,
) -> Result<ThumbnailJob> {
    let entry = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?
        .clone();

    // Materials key on their resolved state (a live hash); every other kind keys on the
    // stored catalog `content_hash`, so the arm returns `Some(hash)` only for a material.
    let (content, material_hash): (ThumbnailContent, Option<u64>) = match entry.asset_type {
        AssetType::Material => {
            // The main-graph render re-resolves the material + its textures from the catalog by id,
            // so no payload is gathered here.
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

    // Resolve the content-addressed key: a material's live hash, else the stored catalog hash, else
    // one derived from the gathered bytes (flagged so the caller persists it). A `0` hash is
    // uncacheable (unreadable bytes) — generated but not cached.
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
    let world = crate::model::imported_node_world_transforms(&nodes)
        .map_err(|error| Error::Thumbnail(error.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::{isolated_server, put_embedded_material_model, temp_root};

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

        let loaded = crate::material::load_catalog_material_asset(&mut assets, Uuid(13_002))
            .expect("embedded material resolves");
        assert_eq!(loaded.base_color, material.base_color);
        assert_eq!(loaded.unlit, material.unlit);

        let job = build_thumbnail_job(&mut assets, Uuid(13_002), 64).expect("job");
        assert!(matches!(job.content, ThumbnailContent::Material));
        assert_eq!(job.content_hash, thumbnail_material_hash(&loaded));
    }
}
