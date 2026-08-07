//! Baking an imported model (or a folder of material maps) into one self-contained container.

use saffron_core::Uuid;
use saffron_geometry::{
    ChunkKind, ContainerChunk, ImportedModel, MaterialMapRole, MorphData, VertexSkin,
    save_animation_to_buffer, save_mesh_to_buffer, sub_id_for, translate_model, write_container,
};
use saffron_json::{Value, uuid_to_json};
use saffron_scene::{AssetType, Colorspace};

use crate::AssetServer;
use crate::error::Result;
use crate::model::{ContainerMetadata, Import, METADATA_SCHEMA_VERSION, SubAsset};
use crate::names::colorspace_name;

use super::meta::{
    MaterialTextureIds, Pending, imported_nodes_to_json, imported_skin_to_json,
    material_chunk_json, morph_to_json,
};
use super::{
    BakeResult, IMPORTER_VERSION, ImportOptions, MaterialBakeResult, MaterialMap,
    catalog_rows_for_container, hash_bytes_fnv, hash_file_fnv,
};

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
    /// [`Error::Geometry`](crate::Error::Geometry) if a skinned mesh fails to serialize;
    /// [`Error::Io`](crate::Error::Io) / [`Error::Geometry`](crate::Error::Geometry) if the container
    /// cannot be written.
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
            // A standalone occlusion map is not embedded: the `.smat` format packs AO into orm and
            // has no dedicated slot to bind it to.

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
    /// [`Error::Io`](crate::Error::Io) if the container file cannot be written.
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
    /// [`Error::Geometry`](crate::Error::Geometry) if the source cannot be translated, or any
    /// [`Self::bake_model`] error.
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
mod tests {
    use saffron_core::Uuid;
    use saffron_geometry::{ImportedMaterial, load_mesh_from_bytes};

    use super::*;

    use crate::model::read_container_metadata;

    use super::super::test_support::{quad_mesh, scratch, town_graph};

    #[test]
    fn bake_writes_a_container_with_a_model_parent_and_sub_asset_rows() {
        let dir = scratch("roundtrip");
        let root = dir.join("assets");
        let assets = AssetServer::new(&root);

        let graph = town_graph();
        let bake = assets
            .bake_model(&graph, ImportOptions::default(), "/tmp/town.glb", Uuid(0))
            .expect("bake");

        // 1 mesh + 2 materials + 3 textures (albedo, normal, orm) + 1 clip = 7 sub-assets;
        // 8 catalog rows (the Model parent + the 7 sub-assets).
        let full = format!("{}/{}", root.display(), bake.path);
        let meta = read_container_metadata(&full).expect("prefix read");
        assert_eq!(meta.sub_assets.len(), 7);
        assert_eq!(meta.materials.as_array().unwrap().len(), 2);
        assert!(meta.nodes.is_array());
        assert_eq!(meta.nodes.as_array().unwrap().len(), 2);
        assert!(!meta.skin.is_null(), "the rigged graph bakes a skin block");
        assert_eq!(bake.rows.len(), 8);
        assert_eq!(bake.rows[0].asset_type, AssetType::Model);
        assert!(bake.rows.iter().all(|r| r.rigged));

        let reader = saffron_geometry::read_container(&full).expect("read_container");
        let mesh_sub_id = saffron_geometry::sub_id_for("town", "mesh", "root", 0);
        let entry = reader
            .find(saffron_geometry::ChunkKind::Mesh, mesh_sub_id.value())
            .expect("mesh chunk present");
        let bytes = reader.read_chunk(entry).expect("read mesh chunk");
        let mesh = load_mesh_from_bytes(&bytes).expect("decode mesh");
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(mesh.submeshes.len(), 2);

        // Import is GPU-free: it writes no signed-distance-field chunk (the field is GPU
        // jump-flood baked at mesh-upload time and cached to an `assets/cache` sidecar).

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sub_ids_are_stable_across_two_bakes_of_the_same_source() {
        let dir = scratch("stable");
        let root = dir.join("assets");
        let assets = AssetServer::new(&root);
        let graph = town_graph();

        let first = assets
            .bake_model(&graph, ImportOptions::default(), "/tmp/town.glb", Uuid(0))
            .expect("first bake");
        let second = assets
            .bake_model(
                &graph,
                ImportOptions::default(),
                "/tmp/town.glb",
                first.model_id,
            )
            .expect("second bake reuses the model id");

        assert_eq!(
            first.model_id, second.model_id,
            "reimport reuses the model id"
        );
        // The sub-asset rows (everything past the Model parent) match by id one-to-one: the
        // reimport-determinism contract that keeps soft references valid.
        let first_ids: Vec<Uuid> = first.rows[1..].iter().map(|r| r.id).collect();
        let second_ids: Vec<Uuid> = second.rows[1..].iter().map(|r| r.id).collect();
        assert_eq!(first_ids, second_ids);

        // The ids are exactly the `sub_id_for` values keyed by the source-stem model key
        // (the mesh sub-id is keyed by the mesh-bearing node "root").
        let mesh_id = saffron_geometry::sub_id_for("town", "mesh", "root", 0);
        assert!(first_ids.contains(&mesh_id));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_id_zero_mints_a_fresh_id() {
        let dir = scratch("mint");
        let root = dir.join("assets");
        let assets = AssetServer::new(&root);
        let graph = town_graph();
        let bake = assets
            .bake_model(&graph, ImportOptions::default(), "/tmp/town.glb", Uuid(0))
            .expect("bake");
        assert_ne!(bake.model_id, Uuid(0));
        assert!(
            bake.model_id.value() >= 1024,
            "a minted id is past the reserved range"
        );
        assert_eq!(
            bake.path,
            format!("models/{}.smodel", bake.model_id.value())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_rows_from_meta_equal_rows_from_a_reread_container() {
        // The bake/scan agreement: the rows derived from a freshly-baked container's META
        // equal the rows derived from re-reading that container's META off disk.
        let dir = scratch("agreement");
        let root = dir.join("assets");
        let assets = AssetServer::new(&root);
        let graph = town_graph();
        let bake = assets
            .bake_model(&graph, ImportOptions::default(), "/tmp/town.glb", Uuid(0))
            .expect("bake");

        let full = format!("{}/{}", root.display(), bake.path);
        let meta = read_container_metadata(&full).expect("prefix read");
        let scanned_rows = catalog_rows_for_container(&meta, &bake.path, AssetType::Model);
        assert_eq!(
            scanned_rows, bake.rows,
            "bake rows must equal scan-derived rows"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remapped_sub_asset_row_points_at_the_external_path() {
        // A remap entry makes a sub-asset a standalone file: its row points at the external
        // path with container == 0 / chunk == -1.
        let mut meta = ContainerMetadata {
            model_id: Uuid(4242),
            name: "town".to_owned(),
            ..ContainerMetadata::default()
        };
        meta.sub_assets.push(SubAsset {
            sub_id: Uuid(5000),
            asset_type: AssetType::Mesh,
            name: "town_mesh".to_owned(),
            chunk: 1,
            ..SubAsset::default()
        });
        let mut remap = serde_json::Map::new();
        remap.insert(
            "5000".to_owned(),
            serde_json::json!({ "external": "meshes/town_extracted.smesh" }),
        );
        meta.remap = saffron_json::Value::Object(remap);

        let rows = catalog_rows_for_container(&meta, "models/4242.smodel", AssetType::Model);
        let mesh_row = rows.iter().find(|r| r.id == Uuid(5000)).unwrap();
        assert_eq!(mesh_row.path, "meshes/town_extracted.smesh");
        assert_eq!(mesh_row.container, Uuid(0));
        assert_eq!(mesh_row.chunk, -1);
        let model_row = rows.iter().find(|r| r.id == Uuid(4242)).unwrap();
        assert_eq!(model_row.asset_type, AssetType::Model);
        assert_eq!(model_row.path, "models/4242.smodel");
    }

    #[test]
    fn unrigged_graph_bakes_no_skin_and_unrigged_rows() {
        let dir = scratch("unrigged");
        let root = dir.join("assets");
        let assets = AssetServer::new(&root);
        let graph = ImportedModel {
            nodes: vec![saffron_geometry::ImportedNode {
                name: "flat".to_owned(),
                mesh: Some(quad_mesh()),
                ..saffron_geometry::ImportedNode::default()
            }],
            materials: vec![ImportedMaterial {
                name: "flat".to_owned(),
                ..ImportedMaterial::default()
            }],
            animations: Vec::new(),
            skin: None,
            morph: None,
            origin: Default::default(),
        };
        let bake = assets
            .bake_model(&graph, ImportOptions::default(), "/tmp/flat.obj", Uuid(0))
            .expect("bake");
        let full = format!("{}/{}", root.display(), bake.path);
        let meta = read_container_metadata(&full).expect("prefix read");
        assert!(meta.skin.is_null(), "an unrigged graph bakes no skin");
        assert!(bake.rows.iter().all(|r| !r.rigged));
        assert_eq!(bake.rows.len(), 3);
        assert_eq!(meta.source_format, "obj");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn baked_texture_chunk_flags_carry_the_colorspace() {
        // Albedo bakes sRGB (flag 1); normal / orm bake linear (flag 2). The resolve path
        // reads the flag back, so this pins the bake side of that contract.
        let dir = scratch("texflags");
        let root = dir.join("assets");
        let assets = AssetServer::new(&root);
        let graph = town_graph();
        let bake = assets
            .bake_model(&graph, ImportOptions::default(), "/tmp/town.glb", Uuid(0))
            .expect("bake");
        let full = format!("{}/{}", root.display(), bake.path);
        let reader = saffron_geometry::read_container(&full).expect("read_container");

        let albedo_id = saffron_geometry::sub_id_for("town", "texture", "0_albedo", 0);
        let normal_id = saffron_geometry::sub_id_for("town", "texture", "0_normal", 0);
        let albedo = reader
            .find(saffron_geometry::ChunkKind::Texture, albedo_id.value())
            .unwrap();
        let normal = reader
            .find(saffron_geometry::ChunkKind::Texture, normal_id.value())
            .unwrap();
        assert_eq!(albedo.flags, Colorspace::Srgb as u32, "albedo is sRGB");
        assert_eq!(normal.flags, Colorspace::Linear as u32, "normal is linear");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
