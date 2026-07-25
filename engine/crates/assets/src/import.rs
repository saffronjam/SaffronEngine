//! The disk-side import pipeline: `bake_model` / `import_model` and the shared
//! `catalog_rows_for_container`.
//!
//! Bake is pure disk + catalog — no GPU, no spawn. It turns an [`ImportedModel`] (from
//! geometry's `translate_model`) into one self-contained `assets/models/<uuid>.smodel`:
//! the mesh chunk, each material as a `.smat`-JSON chunk, each texture as a raw chunk
//! (colorspace in the chunk flags), each clip as a `.sanim` chunk, and a META chunk
//! ([`ContainerMetadata`]) carrying the node/skin hierarchy plus the deterministic
//! reimport recipe (source path, **content hash — not mtime**, [`IMPORTER_VERSION`],
//! and the recorded [`ImportOptions`]).
//!
//! Sub-ids are stable via geometry's `sub_id_for`, keyed by source name, so a re-bake of
//! the same source resolves every sub-asset to its prior identity. `model_id` is reused
//! on reimport (`0` mints a fresh one). [`catalog_rows_for_container`] is shared by bake and
//! the scan so a freshly-baked container and a rediscovered one yield identical rows.

use saffron_core::{HeightMode, Uuid};
use saffron_geometry::{
    AlphaMode, ChunkKind, ContainerChunk, ImportedMaterial, ImportedModel, ImportedNode,
    ImportedSkin, MaterialMapRole, MorphData, VertexSkin, save_animation_to_buffer,
    save_mesh_to_buffer, sub_id_for, translate_model, write_container,
};
use saffron_json::{
    Value, dump_json_sorted, json_bool_or, json_f32_or, json_string_or, uuid_to_json,
};
use saffron_scene::{AssetEntry, AssetType, Colorspace};

use crate::AssetServer;
use crate::error::Result;
use crate::material::{MaterialAsset, material_asset_to_text};
use crate::model::{ContainerMetadata, Import, METADATA_SCHEMA_VERSION, SubAsset};
use crate::names::{colorspace_from_name, colorspace_name};

/// The bump-on-incompatible-translator version stamped into a container's import recipe;
/// a reimport whose stored value differs is re-baked rather than skipped.
pub const IMPORTER_VERSION: u32 = 1;

/// Every decision an import makes, in one serializable place.
///
/// Stored verbatim in a container's META chunk so a reimport replays the same options
/// rather than today's defaults. For v1 `scale`/`axis`/`gen_tangents` are recorded intent;
/// `embed_textures` is always true. [`ImportOptions::colorspace_for`] is the single source
/// of truth for per-role texture colorspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImportOptions {
    /// Uniform import scale (recorded intent for v1).
    pub scale: f32,
    /// The source up-axis (recorded intent for v1).
    pub axis: Axis,
    /// Whether to generate tangents (recorded intent for v1).
    pub gen_tangents: bool,
    /// Whether textures are embedded in the container (always true for v1).
    pub embed_textures: bool,
}

/// The source model's up-axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    /// Y-up (the glTF default).
    #[default]
    YUp,
    /// Z-up.
    ZUp,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            axis: Axis::YUp,
            gen_tangents: true,
            embed_textures: true,
        }
    }
}

impl ImportOptions {
    /// The colorspace a material map of `role` is imported with: albedo/emissive are
    /// sRGB color; normal / metallic-roughness / occlusion / height are linear data maps.
    #[must_use]
    pub fn colorspace_for(self, role: MaterialMapRole) -> Colorspace {
        match role {
            MaterialMapRole::Albedo | MaterialMapRole::Emissive => Colorspace::Srgb,
            _ => Colorspace::Linear,
        }
    }

    /// The options as the META `import.options` JSON (stored verbatim).
    #[must_use]
    pub fn to_json(self) -> Value {
        let axis = match self.axis {
            Axis::YUp => "y-up",
            Axis::ZUp => "z-up",
        };
        serde_json::json!({
            "scale": self.scale,
            "axis": axis,
            "genTangents": self.gen_tangents,
            "embedTextures": self.embed_textures,
        })
    }

    /// Parses options back from the stored `import.options` JSON (the reimport replay).
    /// Lenient: missing keys take the defaults.
    #[must_use]
    pub fn from_json(doc: &Value) -> Self {
        let axis = if json_string_or(doc, "axis", "y-up".to_owned()) == "z-up" {
            Axis::ZUp
        } else {
            Axis::YUp
        };
        Self {
            scale: json_f32_or(doc, "scale", 1.0),
            axis,
            gen_tangents: json_bool_or(doc, "genTangents", true),
            embed_textures: json_bool_or(doc, "embedTextures", true),
        }
    }
}

/// What [`AssetServer::bake_model`] produces: the new container's id, its project-relative
/// path, and the catalog rows it contributes (one [`AssetType::Model`] parent + one row
/// per embedded sub-asset). No GPU, no spawn.
#[derive(Clone, Debug)]
pub struct BakeResult {
    /// The baked container's model id.
    pub model_id: Uuid,
    /// Project-relative path to the `.smodel`.
    pub path: String,
    /// The catalog rows the container contributes.
    pub rows: Vec<AssetEntry>,
}

/// One role-tagged map fed to [`AssetServer::bake_material_container`].
#[derive(Clone, Debug)]
pub struct MaterialMap {
    /// The canonical role (`albedo`/`normal`/`orm`/`roughness`/`metallic`/`ao`/`height`/
    /// `emissive`) that selects the material slot + colorspace.
    pub role: String,
    /// For a `height` role, the technique the source implies (from the filename —
    /// [`detect_height_mode`](crate::detect_height_mode)); ignored for every other role.
    pub height_mode: HeightMode,
    /// The raw encoded image bytes (png/jpg/…); the loader sniffs the format on decode.
    pub bytes: Vec<u8>,
}

/// The result of baking a material container: the parent material id, its `.smatx` path,
/// the catalog rows it contributes (the self-container material + its hidden texture
/// sub-rows), and the space-joined role summary.
pub struct MaterialBakeResult {
    /// The baked material's id (also its own container id).
    pub material_id: Uuid,
    /// Project-relative path to the `.smatx`.
    pub path: String,
    /// The catalog rows the container contributes.
    pub rows: Vec<AssetEntry>,
    /// The space-joined roles that were slotted (e.g. `"albedo normal roughness height "`).
    pub roles: String,
}

/// What a scan changed relative to the live catalog: rows added (newly discovered on
/// disk) and ids removed (their backing file is gone).
///
/// The filesystem is the source of truth, so an unsaved import can never become a dead
/// orphan — its `.smodel` is rediscovered on the next scan.
#[derive(Clone, Debug, Default)]
pub struct ScanDelta {
    /// Catalog rows newly discovered on disk.
    pub added: Vec<AssetEntry>,
    /// Ids whose backing file vanished.
    pub removed: Vec<Uuid>,
}

/// The catalog rows a container contributes: one `parent_type` parent + one row per
/// embedded sub-asset (container linkage + chunk index + colorspace).
///
/// Shared by [`AssetServer::bake_model`] / [`AssetServer::bake_material_container`] and the
/// scan so a freshly-baked container and a rediscovered one yield **identical** rows. A
/// `Model` parent is a standalone container (`container == 0`); a texture-embedding
/// `Material` parent points its own `container` at itself so its `.smat` chunk resolves
/// through the shared container path while the frontend still shows it (its `container`
/// equals its `id`, and only sub-assets — `container != id` — are hidden). A rigged
/// container (its META carries a skin) flags every row so the editor routes a rigged mesh
/// to the rig editor without a per-click probe. An extracted (remapped) sub-asset is a
/// standalone file: its row points at the external path with `container == 0` / `chunk ==
/// -1`, so the scan agrees with the resolver and the ids never alias.
#[must_use]
pub fn catalog_rows_for_container(
    meta: &ContainerMetadata,
    relative_path: &str,
    parent_type: AssetType,
) -> Vec<AssetEntry> {
    let rigged = !meta.skin.is_null();
    let parent_container = if parent_type == AssetType::Material {
        meta.model_id
    } else {
        Uuid(0)
    };
    let mut rows = Vec::with_capacity(meta.sub_assets.len() + 1);
    rows.push(AssetEntry {
        id: meta.model_id,
        name: meta.name.clone(),
        asset_type: parent_type,
        path: relative_path.to_owned(),
        rigged,
        container: parent_container,
        content_hash: model_content_hash(meta),
        ..AssetEntry::default()
    });
    for sub in &meta.sub_assets {
        let mut row = AssetEntry {
            id: sub.sub_id,
            name: sub.name.clone(),
            asset_type: sub.asset_type,
            rigged,
            colorspace: colorspace_from_name(&sub.colorspace),
            duration: sub.duration,
            tracks: sub.tracks,
            content_hash: sub.content_hash,
            ..AssetEntry::default()
        };
        let key = sub.sub_id.value().to_string();
        let remapped = meta
            .remap
            .as_object()
            .and_then(|m| m.get(&key))
            .and_then(|entry| entry.get("external"))
            .and_then(Value::as_str);
        if let Some(external) = remapped {
            row.path = external.to_owned();
            row.container = Uuid(0);
            row.chunk = -1;
        } else {
            row.path = relative_path.to_owned();
            row.container = meta.model_id;
            row.chunk = sub.chunk as i32;
        }
        rows.push(row);
    }
    rows
}

/// The FNV-1a offset basis (64-bit).
const FNV_OFFSET: u64 = 1469598103934665603;
/// The FNV-1a prime (64-bit).
const FNV_PRIME: u64 = 1099511628211;

/// One FNV-1a step: mix `v` into the running `hash`.
#[must_use]
fn fnv_mix(hash: u64, v: u64) -> u64 {
    (hash ^ v).wrapping_mul(FNV_PRIME)
}

/// The FNV-1a fold over a byte slice — the content hash of a baked chunk (mesh/texture/
/// material bytes), used as the content-addressed thumbnail cache key.
#[must_use]
pub fn hash_bytes_fnv(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &byte in bytes {
        hash = fnv_mix(hash, u64::from(byte));
    }
    hash
}

/// The FNV-1a fold over a source file's bytes, as a decimal string. The reimport recipe
/// stores this — a **content** hash, not the mtime — so a touched-but-unchanged source is
/// a content-addressed skip. An unreadable path hashes to the empty string.
#[must_use]
pub fn hash_file_fnv(path: &str) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    hash_bytes_fnv(&bytes).to_string()
}

/// The model preview's content hash: a fold of its embedded mesh/material/texture sub-hashes
/// plus the canonical (sorted) node block, so the whole-forest thumbnail key changes only
/// when the baked geometry, materials, or layout change. Deterministic across bake and scan
/// (sub-hashes live in META; nodes fold via [`dump_json_sorted`]). Returns `0` for a legacy
/// container whose sub-assets carry no per-chunk hash, so the model row self-heals.
#[must_use]
fn model_content_hash(meta: &ContainerMetadata) -> u64 {
    let mut hash = FNV_OFFSET;
    let mut folded = 0usize;
    for sub in &meta.sub_assets {
        if sub.content_hash != 0 {
            hash = fnv_mix(hash, sub.sub_id.value());
            hash = fnv_mix(hash, sub.content_hash);
            folded += 1;
        }
    }
    if folded == 0 {
        return 0;
    }
    for byte in dump_json_sorted(&meta.nodes, -1).into_bytes() {
        hash = fnv_mix(hash, u64::from(byte));
    }
    hash
}

/// The import node forest as the META `nodes` block (glTF-shaped; the quaternion in
/// `w,x,y,z` order). Each entry carries a `mesh` sub-id (decimal string, `"0"` when the
/// node has no mesh) so spawn re-attaches each node's node-local mesh. `node_mesh_ids`
/// parallels `nodes`.
fn imported_nodes_to_json(nodes: &[ImportedNode], node_mesh_ids: &[u64]) -> Value {
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
fn morph_to_json(morph: &MorphData) -> Value {
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
fn imported_skin_to_json(skin: &ImportedSkin) -> Value {
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
fn material_chunk_json(material: &ImportedMaterial, textures: &MaterialTextureIds) -> Vec<u8> {
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
struct MaterialTextureIds {
    albedo: Uuid,
    orm: Uuid,
    normal: Uuid,
    emissive: Uuid,
}

/// A chunk staged for the container write: its bytes are owned until `write_container`
/// frames them, and META sits at index `0` (front-loaded), its bytes filled in last once
/// every sub-asset's TOC index is known.
struct Pending {
    kind: ChunkKind,
    sub_id: u64,
    flags: u32,
    bytes: Vec<u8>,
}

impl AssetServer {
    /// Bakes an [`ImportedModel`] into one self-contained `assets/models/<uuid>.smodel`.
    ///
    /// Writes the mesh chunk, each material as a `.smat`-JSON chunk, each texture as a raw
    /// chunk (colorspace in the chunk flags), each clip as a `.sanim` chunk, and the META
    /// chunk with the node/skin hierarchy + the deterministic reimport recipe. No GPU
    /// upload, no entity spawn. `model_id` is reused on reimport (`0` mints a fresh one);
    /// sub-ids are stable via `sub_id_for`, keyed by source name.
    ///
    /// # Errors
    ///
    /// [`Error::Geometry`] if a skinned mesh fails to serialize; [`Error::Io`] /
    /// [`Error::Geometry`] if the container cannot be written.
    pub fn bake_model(
        &self,
        graph: &ImportedModel,
        options: ImportOptions,
        source_path: &str,
        model_id: Uuid,
    ) -> Result<BakeResult> {
        let model_id = if model_id.value() == 0 {
            Uuid::new()
        } else {
            model_id
        };
        let source = std::path::Path::new(source_path);
        let model_key = source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let source_format = if ext == "gltf" || ext == "glb" {
            "gltf"
        } else {
            "obj"
        };

        let mut pending = vec![Pending {
            kind: ChunkKind::Meta,
            sub_id: 0,
            flags: 0,
            bytes: Vec::new(),
        }];

        let mut meta = ContainerMetadata {
            schema: METADATA_SCHEMA_VERSION,
            model_id,
            name: model_key.clone(),
            source_format: source_format.to_owned(),
            import: Import {
                source_path: source_path.to_owned(),
                source_hash: hash_file_fnv(source_path),
                importer_version: IMPORTER_VERSION,
                options: options.to_json(),
            },
            sub_assets: Vec::new(),
            materials: Value::Array(Vec::new()),
            nodes: Value::Array(Vec::new()),
            skin: Value::Null,
            morph: Value::Null,
            remap: Value::Object(serde_json::Map::new()),
        };

        // One mesh sub-asset per mesh-bearing forest node — the single mesh-ownership
        // shape. The skin's mesh node carries the skin stream (a skinned `.smesh`); every
        // other node's mesh is a plain stream. `node_mesh_ids` parallels `graph.nodes`,
        // `0` where a node has no mesh, and is woven into the META `nodes` block.
        let skin_mesh_node = graph.skin.as_ref().map(|s| s.desc.mesh_node).unwrap_or(-1);
        // The mesh-global morph rides the first mesh-bearing node (the importer attaches
        // its deltas relative to that node's local mesh).
        let morph_node = graph
            .morph
            .as_ref()
            .and_then(|_| graph.nodes.iter().position(|n| n.mesh.is_some()));
        let mut node_mesh_ids = vec![0u64; graph.nodes.len()];
        for (i, node) in graph.nodes.iter().enumerate() {
            let Some(mesh) = node.mesh.as_ref() else {
                continue;
            };
            let mesh_sub_id = sub_id_for(&model_key, "mesh", &node.name, i as u32);
            let stream: &[VertexSkin] = if i as i32 == skin_mesh_node {
                graph
                    .skin
                    .as_ref()
                    .map(|s| s.stream.as_slice())
                    .unwrap_or(&[])
            } else {
                &[]
            };
            let node_morph: Option<&MorphData> = if Some(i) == morph_node {
                graph.morph.as_ref()
            } else {
                None
            };
            let mesh_bytes = save_mesh_to_buffer(mesh, stream, node_morph)?;
            let mesh_hash = hash_bytes_fnv(&mesh_bytes);
            let mesh_chunk = pending.len() as u32;
            pending.push(Pending {
                kind: ChunkKind::Mesh,
                sub_id: mesh_sub_id.value(),
                flags: 0,
                bytes: mesh_bytes,
            });
            meta.sub_assets.push(SubAsset {
                sub_id: mesh_sub_id,
                asset_type: AssetType::Mesh,
                name: format!("{model_key}_{}", node.name),
                chunk: mesh_chunk,
                content_hash: mesh_hash,
                ..SubAsset::default()
            });
            node_mesh_ids[i] = mesh_sub_id.value();
            // The per-mesh signed distance fields are not baked at import (which is GPU-free):
            // they are GPU jump-flood baked at `GpuMesh`-build time — one tight field per
            // primitive — and cached to an `assets/cache/<meshHash>.sdfset` sidecar. See
            // `saffron_rendering::Uploader::bake_sdf`.
        }

        let mut material_summaries = Vec::with_capacity(graph.materials.len());
        for (m, src) in graph.materials.iter().enumerate() {
            let material_name = if src.name.is_empty() {
                format!("material_{m}")
            } else {
                src.name.clone()
            };

            let mut tex_ids = MaterialTextureIds::default();
            let emit_texture = |pending: &mut Vec<Pending>,
                                meta: &mut ContainerMetadata,
                                role,
                                role_name: &str,
                                bytes: &[u8]| {
                let tex_id = sub_id_for(&model_key, "texture", &format!("{m}_{role_name}"), 0);
                let space = options.colorspace_for(role);
                let index = pending.len() as u32;
                pending.push(Pending {
                    kind: ChunkKind::Texture,
                    sub_id: tex_id.value(),
                    flags: space as u32,
                    bytes: bytes.to_vec(),
                });
                meta.sub_assets.push(SubAsset {
                    sub_id: tex_id,
                    asset_type: AssetType::Texture,
                    name: format!("{material_name}_{role_name}"),
                    chunk: index,
                    colorspace: colorspace_name(space).to_owned(),
                    content_hash: hash_bytes_fnv(bytes),
                    ..SubAsset::default()
                });
                tex_id
            };

            if let Some(tex) = src.albedo.as_ref() {
                tex_ids.albedo = emit_texture(
                    &mut pending,
                    &mut meta,
                    MaterialMapRole::Albedo,
                    "albedo",
                    &tex.bytes,
                );
            }
            if let Some(tex) = src.metallic_roughness.as_ref() {
                tex_ids.orm = emit_texture(
                    &mut pending,
                    &mut meta,
                    MaterialMapRole::MetallicRoughness,
                    "orm",
                    &tex.bytes,
                );
            }
            if let Some(tex) = src.normal.as_ref() {
                tex_ids.normal = emit_texture(
                    &mut pending,
                    &mut meta,
                    MaterialMapRole::Normal,
                    "normal",
                    &tex.bytes,
                );
            }
            if let Some(tex) = src.emissive_tex.as_ref() {
                tex_ids.emissive = emit_texture(
                    &mut pending,
                    &mut meta,
                    MaterialMapRole::Emissive,
                    "emissive",
                    &tex.bytes,
                );
            }
            // Occlusion has no dedicated `.smat` slot in v1 (the format packs AO into
            // orm); its bytes are not embedded — a documented v1 gap, not a silent drop.

            let material_id = sub_id_for(&model_key, "material", &material_name, m as u32);
            let material_bytes = material_chunk_json(src, &tex_ids);
            let material_hash = hash_bytes_fnv(&material_bytes);
            let material_chunk_index = pending.len() as u32;
            pending.push(Pending {
                kind: ChunkKind::Material,
                sub_id: material_id.value(),
                flags: 0,
                bytes: material_bytes,
            });
            meta.sub_assets.push(SubAsset {
                sub_id: material_id,
                asset_type: AssetType::Material,
                name: material_name,
                chunk: material_chunk_index,
                content_hash: material_hash,
                ..SubAsset::default()
            });

            material_summaries.push(serde_json::json!({
                "subId": uuid_to_json(material_id.value()),
                "baseColor": [src.base_color.x, src.base_color.y, src.base_color.z, src.base_color.w],
                "metallic": src.metallic,
                "roughness": src.roughness,
            }));
        }
        meta.materials = Value::Array(material_summaries);

        for (a, clip) in graph.animations.iter().enumerate() {
            let clip_name = if clip.name.is_empty() {
                format!("clip_{a}")
            } else {
                clip.name.clone()
            };
            let clip_id = sub_id_for(&model_key, "animation", &clip_name, a as u32);
            let clip_bytes = save_animation_to_buffer(clip);
            let clip_chunk = pending.len() as u32;
            pending.push(Pending {
                kind: ChunkKind::Animation,
                sub_id: clip_id.value(),
                flags: 0,
                bytes: clip_bytes,
            });
            meta.sub_assets.push(SubAsset {
                sub_id: clip_id,
                asset_type: AssetType::Animation,
                name: clip_name,
                chunk: clip_chunk,
                duration: clip.duration,
                tracks: clip.tracks.len() as i32,
                ..SubAsset::default()
            });
        }

        meta.nodes = imported_nodes_to_json(&graph.nodes, &node_mesh_ids);
        if let Some(skin) = graph.skin.as_ref() {
            meta.skin = imported_skin_to_json(&skin.desc);
        }
        if let Some(morph) = graph.morph.as_ref() {
            meta.morph = morph_to_json(morph);
        }

        pending[0].bytes = crate::model::encode_container_metadata(&meta);

        let chunks: Vec<ContainerChunk> = pending
            .iter()
            .map(|p| ContainerChunk {
                kind: p.kind,
                sub_id: p.sub_id,
                flags: p.flags,
                bytes: &p.bytes,
            })
            .collect();

        let relative_path = format!("models/{}.smodel", model_id.value());
        self.ensure_asset_directories();
        write_container(format!("{}/{relative_path}", self.root.display()), &chunks)?;

        let rows = catalog_rows_for_container(&meta, &relative_path, AssetType::Model);
        Ok(BakeResult {
            model_id,
            path: relative_path,
            rows,
        })
    }

    /// Bakes a set of role-tagged maps + their material into one self-contained material
    /// container (`assets/materials/<uuid>.smatx`): the `.smat`-JSON chunk (referencing the
    /// embedded textures by sub-id) plus one texture chunk per map (colorspace in the chunk
    /// flags), and a META listing the texture sub-assets.
    ///
    /// The material is the container **parent** (a self-referencing `Material` row, resolved
    /// from the TOC), so it is not a META sub-asset; each texture is a hidden sub-row keyed
    /// by `container == material_id`. Mirrors [`Self::bake_model`] — pure disk, no GPU.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the container file cannot be written.
    pub fn bake_material_container(
        &self,
        name: &str,
        maps: &[MaterialMap],
    ) -> Result<MaterialBakeResult> {
        let material_id = Uuid::new();
        let mut pending = vec![Pending {
            kind: ChunkKind::Meta,
            sub_id: 0,
            flags: 0,
            bytes: Vec::new(),
        }];
        let mut meta = ContainerMetadata {
            model_id: material_id,
            name: name.to_owned(),
            source_format: "material".to_owned(),
            ..ContainerMetadata::default()
        };
        let mut material = crate::material::MaterialAsset::default();
        let mut roles = String::new();
        for map in maps {
            // Only base color / emissive carry sRGB; every data map (normal, packed ORM,
            // height, AO) is linear — the same policy the single-map import applies.
            let space = match map.role.as_str() {
                "albedo" | "emissive" => Colorspace::Srgb,
                _ => Colorspace::Linear,
            };
            let sub_id = Uuid::new();
            let chunk_index = pending.len() as u32;
            pending.push(Pending {
                kind: ChunkKind::Texture,
                sub_id: sub_id.value(),
                flags: space as u32,
                bytes: map.bytes.clone(),
            });
            meta.sub_assets.push(SubAsset {
                sub_id,
                asset_type: AssetType::Texture,
                name: format!("{name} {}", map.role),
                chunk: chunk_index,
                colorspace: colorspace_name(space).to_owned(),
                content_hash: hash_bytes_fnv(&map.bytes),
                ..SubAsset::default()
            });
            match map.role.as_str() {
                "albedo" => {
                    material.albedo_texture = sub_id;
                    roles.push_str("albedo ");
                }
                "normal" => {
                    material.normal_texture = sub_id;
                    roles.push_str("normal ");
                }
                role @ ("orm" | "roughness" | "metallic") => {
                    material.orm_texture = sub_id;
                    roles.push_str(role);
                    roles.push(' ');
                }
                "ao" => {
                    if material.orm_texture.value() == 0 {
                        material.orm_texture = sub_id;
                    }
                    roles.push_str("ao ");
                }
                "height" => {
                    material.height_texture = sub_id;
                    // The filename picks the technique: a provider Displacement map imports as real
                    // displacement (library intent), a bump map as a shading bump, else parallax.
                    material.height_mode = map.height_mode;
                    roles.push_str("height ");
                }
                "emissive" => {
                    material.emissive_texture = sub_id;
                    roles.push_str("emissive ");
                }
                _ => {}
            }
        }

        let material_bytes = crate::material::material_asset_to_text(&material, 2).into_bytes();
        pending.push(Pending {
            kind: ChunkKind::Material,
            sub_id: material_id.value(),
            flags: 0,
            bytes: material_bytes,
        });

        pending[0].bytes = crate::model::encode_container_metadata(&meta);
        let chunks: Vec<ContainerChunk> = pending
            .iter()
            .map(|p| ContainerChunk {
                kind: p.kind,
                sub_id: p.sub_id,
                flags: p.flags,
                bytes: &p.bytes,
            })
            .collect();

        let relative_path = format!("materials/{}.smatx", material_id.value());
        self.ensure_asset_directories();
        write_container(format!("{}/{relative_path}", self.root.display()), &chunks)?;

        let rows = catalog_rows_for_container(&meta, &relative_path, AssetType::Material);
        Ok(MaterialBakeResult {
            material_id,
            path: relative_path,
            rows,
            roles,
        })
    }

    /// Translates a model source, bakes it into one `.smodel`, and adds the catalog rows
    /// it contributes. Produces an asset; does not upload to the GPU or spawn an entity —
    /// pair with `instantiate_model` to place it.
    ///
    /// # Errors
    ///
    /// [`Error::Geometry`] if the source cannot be translated, or any [`Self::bake_model`]
    /// error.
    pub fn import_model(&mut self, path: &str, options: ImportOptions) -> Result<BakeResult> {
        let graph = translate_model(path)?;
        let bake = self.bake_model(&graph, options, path, Uuid(0))?;
        for row in &bake.rows {
            // Sub-asset ids are stable over the source path, so importing a source whose
            // rows are already catalogued re-registers them as reimported content.
            if self.catalog.find(row.id).is_some() {
                self.register_reimported_asset(row.clone());
            } else {
                self.register_imported_asset(row.clone());
            }
        }
        Ok(bake)
    }
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod tests;
