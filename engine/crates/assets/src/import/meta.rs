//! The container META blocks: the node forest, the skin descriptor, the morph deltas, and each
//! material's `.smat` chunk document.

use saffron_core::Uuid;
use saffron_geometry::{
    AlphaMode, ChunkKind, ImportedMaterial, ImportedNode, ImportedSkin, MorphData,
};
use saffron_json::{Value, uuid_to_json};

use crate::material::{MaterialAsset, material_asset_to_text};

pub(super) fn imported_nodes_to_json(nodes: &[ImportedNode], node_mesh_ids: &[u64]) -> Value {
    let array = nodes
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let mesh_id = node_mesh_ids.get(i).copied().unwrap_or(0);
            serde_json::json!({
                "name": node.name,
                "parent": node.parent,
                "t": [node.translation.x, node.translation.y, node.translation.z],
                "r": [node.rotation.w, node.rotation.x, node.rotation.y, node.rotation.z],
                "s": [node.scale.x, node.scale.y, node.scale.z],
                "mesh": uuid_to_json(mesh_id),
            })
        })
        .collect();
    Value::Array(array)
}

/// The morph data as the META `morph` block: the durable target names, the target count,
/// and the authored rest weights. The sparse deltas themselves live in the `.smesh` morph
/// section, not META.
pub(super) fn morph_to_json(morph: &MorphData) -> Value {
    let names: Vec<Value> = morph
        .targets
        .iter()
        .map(|t| Value::String(t.name.clone()))
        .collect();
    let rest: Vec<Value> = morph
        .targets
        .iter()
        .map(|t| Value::from(t.rest_weight))
        .collect();
    serde_json::json!({
        "targetNames": names,
        "targetCount": morph.targets.len() as u32,
        "restWeights": rest,
    })
}

/// The skin descriptor as the META `skin` block; inverse-bind matrices are 16 floats
/// each, column-major (the glam layout) so the reader can memcpy them straight back.
pub(super) fn imported_skin_to_json(skin: &ImportedSkin) -> Value {
    let inverse_bind: Vec<Value> = skin
        .inverse_bind
        .iter()
        .map(|matrix| {
            let cols = matrix.to_cols_array();
            Value::Array(cols.iter().map(|&f| Value::from(f)).collect())
        })
        .collect();
    serde_json::json!({
        "joints": skin.joints,
        "inverseBind": inverse_bind,
        "skeletonRoot": skin.skeleton_root,
        "meshNode": skin.mesh_node,
    })
}

/// The `.smat`-JSON bytes for one baked material chunk.
///
/// Emits the frozen `.smat` document shape: a `factors`
/// block from the imported PBR factors, a `textures` block of the assigned sub-ids
/// (decimal strings; `"0"` for an absent slot), and the defaults for the remaining
/// fields. The byte format is the contract the material loader reads back.
pub(super) fn material_chunk_json(
    material: &ImportedMaterial,
    textures: &MaterialTextureIds,
) -> Vec<u8> {
    let blend = match material.alpha_mode {
        AlphaMode::Opaque => "opaque",
        AlphaMode::Mask => "masked",
        AlphaMode::Blend => "translucent",
    };
    let asset = MaterialAsset {
        blend: blend.to_owned(),
        double_sided: material.double_sided,
        base_color: material.base_color,
        metallic: material.metallic,
        roughness: material.roughness,
        emissive: material.emissive,
        emissive_strength: material.emissive_strength,
        alpha_cutoff: material.alpha_cutoff,
        albedo_texture: textures.albedo,
        orm_texture: textures.orm,
        normal_texture: textures.normal,
        emissive_texture: textures.emissive,
        ..MaterialAsset::default()
    };
    material_asset_to_text(&asset, -1).into_bytes()
}

/// The texture sub-ids assigned to a baked material's slots (`0` for an absent slot).
#[derive(Default)]
pub(super) struct MaterialTextureIds {
    pub(super) albedo: Uuid,
    pub(super) orm: Uuid,
    pub(super) normal: Uuid,
    pub(super) emissive: Uuid,
}

/// A chunk staged for the container write: its bytes are owned until `write_container`
/// frames them, and META sits at index `0` (front-loaded), its bytes filled in last once
/// every sub-asset's TOC index is known.
pub(super) struct Pending {
    pub(super) kind: ChunkKind,
    pub(super) sub_id: u64,
    pub(super) flags: u32,
    pub(super) bytes: Vec<u8>,
}
