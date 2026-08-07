use saffron_core::Uuid;
use saffron_geometry::{ChunkKind, ContainerChunk, ContainerReader, write_container};
use saffron_scene::AssetType;

use crate::material::{MaterialAsset, material_asset_from_json};
use crate::model::{ContainerMetadata, encode_container_metadata};
use crate::{AssetServer, Error, Result};

/// The standalone destination path an extracted sub-asset defaults to, by type.
pub(super) fn default_extract_dest(asset_type: AssetType, sub_id: Uuid, image_ext: &str) -> String {
    let id = sub_id.value();
    match asset_type {
        AssetType::Material => format!("materials/{id}.smat"),
        AssetType::Mesh => format!("models/{id}.smesh"),
        AssetType::Animation => format!("models/{id}.sanim"),
        AssetType::Plant => format!("vegetation/plants/{id}.splant"),
        AssetType::Biome => format!("vegetation/biomes/{id}.sbiome"),
        AssetType::VegetationMap => format!("vegetation/maps/{id}.svegmap"),
        _ => {
            let ext = if image_ext.is_empty() {
                "png"
            } else {
                image_ext
            };
            format!("textures/{id}.{ext}")
        }
    }
}

/// The image extension implied by a texture chunk's leading bytes (png/jpg/hdr), default png.
pub(super) fn image_ext_from_bytes(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && bytes[..4] == [0x89, 0x50, 0x4E, 0x47] {
        return "png";
    }
    if bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF] {
        return "jpg";
    }
    if bytes.len() >= 2 && bytes[..2] == [0x23, 0x3F] {
        // "#?" — Radiance HDR.
        return "hdr";
    }
    "png"
}

/// Maps a TOC fourcc back to its [`ChunkKind`], or `None` for an unknown tag.
fn chunk_kind_from_fourcc(fourcc: u32) -> Option<ChunkKind> {
    [
        ChunkKind::Meta,
        ChunkKind::Mesh,
        ChunkKind::Texture,
        ChunkKind::Material,
        ChunkKind::Animation,
        ChunkKind::Thumbnail,
    ]
    .into_iter()
    .find(|&kind| kind as u32 == fourcc)
}

/// Rewrites a container with a fresh META chunk, preserving every payload chunk verbatim, so a
/// metadata edit needs no payload-offset bookkeeping.
///
/// # Errors
///
/// Propagates a chunk-read or container-write failure.
pub(super) fn rewrite_container_meta(
    full_path: &str,
    reader: &ContainerReader,
    new_meta: &ContainerMetadata,
) -> Result<()> {
    let meta_bytes = encode_container_metadata(new_meta);
    let mut payloads: Vec<(ChunkKind, u64, u32, Vec<u8>)> = Vec::new();
    for entry in reader.toc() {
        if entry.fourcc == ChunkKind::Meta as u32 {
            continue;
        }
        let Some(kind) = chunk_kind_from_fourcc(entry.fourcc) else {
            continue;
        };
        let bytes = reader.read_chunk(entry)?;
        payloads.push((kind, entry.sub_id, entry.flags, bytes));
    }
    let mut chunks = Vec::with_capacity(payloads.len() + 1);
    chunks.push(ContainerChunk {
        kind: ChunkKind::Meta,
        sub_id: 0,
        flags: 0,
        bytes: &meta_bytes,
    });
    for (kind, sub_id, flags, bytes) in &payloads {
        chunks.push(ContainerChunk {
            kind: *kind,
            sub_id: *sub_id,
            flags: *flags,
            bytes,
        });
    }
    write_container(full_path, &chunks)?;
    Ok(())
}

/// Rewrites a container in place, replacing only the material sub-asset's `ChunkKind::Material`
/// chunk while preserving the META and every other chunk verbatim. The container-aware edit path
/// for [`crate::material::update_material_asset`]: a `.smatx` self-container's own material, or a
/// material embedded in a model container.
///
/// # Errors
///
/// [`Error::NotInCatalog`] if `id` is absent; [`Error::Io`] if the container is not loadable;
/// [`Error::ContainerMissingSubAsset`] if the container holds no material chunk for `id`;
/// propagates a chunk-read or container-write failure.
pub(crate) fn rewrite_material_chunk(
    assets: &mut AssetServer,
    id: Uuid,
    material_json: Vec<u8>,
) -> Result<()> {
    let container = assets
        .catalog
        .find(id)
        .ok_or(Error::NotInCatalog(id.value()))?
        .container;
    let full_path = container_path(assets, container)
        .map(|rel| format!("{}/{rel}", assets.root.display()))
        .ok_or_else(|| Error::Io(format!("container {} not in catalog", container.value())))?;
    let model = assets.load_model_asset(container).ok_or_else(|| {
        Error::Io(format!(
            "material {}: container {} is not loadable",
            id.value(),
            container.value()
        ))
    })?;

    let mut new_bytes = Some(material_json);
    let mut payloads: Vec<(ChunkKind, u64, u32, Vec<u8>)> =
        Vec::with_capacity(model.reader.toc().len());
    for entry in model.reader.toc() {
        let Some(kind) = chunk_kind_from_fourcc(entry.fourcc) else {
            continue;
        };
        let bytes =
            if kind == ChunkKind::Material && entry.sub_id == id.value() && new_bytes.is_some() {
                new_bytes.take().expect("new_bytes checked is_some above")
            } else {
                model.reader.read_chunk(entry)?
            };
        payloads.push((kind, entry.sub_id, entry.flags, bytes));
    }
    if new_bytes.is_some() {
        return Err(Error::ContainerMissingSubAsset {
            container: container.value(),
            sub: id.value(),
        });
    }

    let chunks: Vec<ContainerChunk> = payloads
        .iter()
        .map(|(kind, sub_id, flags, bytes)| ContainerChunk {
            kind: *kind,
            sub_id: *sub_id,
            flags: *flags,
            bytes,
        })
        .collect();
    write_container(&full_path, &chunks)?;

    // The rewritten container's TOC offsets shifted, so the memoized reader + material resolutions
    // are stale; drop them so the next resolve reopens the container and slices the fresh chunk.
    assets.model_by_uuid.remove(&container.value());
    let _ = assets.asset_edited(id);
    Ok(())
}

/// Resolves an embedded material sub-asset by reading + parsing its container chunk.
/// Standalone materials use [`crate::material::load_material_asset`].
pub(super) fn resolve_container_material(
    assets: &mut AssetServer,
    model_id: Uuid,
    sub_id: Uuid,
) -> Option<MaterialAsset> {
    let model = assets.load_model_asset(model_id)?;
    let source = assets.chunk_source_for(&model, ChunkKind::Material, sub_id);
    if source.is_empty() {
        return None;
    }
    let bytes = source.read().ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let doc = saffron_json::parse_json(&text).ok()?;
    material_asset_from_json(&doc).ok()
}

/// The container file's project-relative path, from its catalog row.
pub(super) fn container_path(assets: &AssetServer, model_id: Uuid) -> Option<String> {
    assets.catalog.find(model_id).map(|e| e.path.clone())
}
