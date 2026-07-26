//! Bake / import / catalog-rows tests (GPU-free: bake is pure disk + catalog).

use super::*;
use crate::model::read_container_metadata;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{
    AnimClip, AnimTrack, ImportedMaterial, ImportedModel, Mesh, Submesh, TextureSource, Vertex,
    load_mesh_from_bytes,
};
use std::path::PathBuf;

/// A unique scratch dir under the system temp, removed and recreated per test.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "saffron-assets-import-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A two-submesh quad mesh (4 verts, 6 indices) so a baked mesh chunk decodes to a known
/// shape.
fn quad_mesh() -> Mesh {
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
                position: Vec3::new(1.0, 1.0, 0.0),
                normal: Vec3::Z,
                uv0: Vec2::ONE,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::Y,
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 1.0),
                ..Vertex::default()
            },
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        submeshes: vec![
            Submesh {
                first_index: 0,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 0,
            },
            Submesh {
                first_index: 3,
                index_count: 3,
                vertex_offset: 0,
                material_slot: 1,
            },
        ],
    }
}

/// The bake round-trip graph: a quad mesh, two materials (one with an albedo + normal
/// texture, one with a metallic-roughness texture), and one clip — all riding the skin
/// payload so the clip is baked (clips live on the skin).
fn town_graph() -> ImportedModel {
    let stone = ImportedMaterial {
        name: "stone".to_owned(),
        albedo: Some(TextureSource {
            bytes: vec![1, 2, 3, 4],
            ext: "png".to_owned(),
        }),
        normal: Some(TextureSource {
            bytes: vec![5, 6, 7],
            ext: "png".to_owned(),
        }),
        ..ImportedMaterial::default()
    };
    let metal = ImportedMaterial {
        name: "metal".to_owned(),
        metallic_roughness: Some(TextureSource {
            bytes: vec![8, 9],
            ext: "png".to_owned(),
        }),
        ..ImportedMaterial::default()
    };
    let clip = AnimClip {
        name: "idle".to_owned(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            index: 1,
            target_name: "joint".to_owned(),
            ..AnimTrack::default()
        }],
    };
    ImportedModel {
        origin: Default::default(),
        nodes: vec![
            // The skinned mesh node (mesh_node 0) carries the mesh node-locally.
            saffron_geometry::ImportedNode {
                name: "root".to_owned(),
                mesh: Some(quad_mesh()),
                ..saffron_geometry::ImportedNode::default()
            },
            saffron_geometry::ImportedNode {
                name: "joint".to_owned(),
                parent: 0,
                ..saffron_geometry::ImportedNode::default()
            },
        ],
        materials: vec![stone, metal],
        animations: vec![clip],
        skin: Some(saffron_geometry::SkinPayload {
            desc: saffron_geometry::ImportedSkin {
                joints: vec![1],
                inverse_bind: vec![saffron_geometry::glam::Mat4::IDENTITY],
                skeleton_root: 0,
                mesh_node: 0,
            },
            // A skin influence per vertex (4) so the skinned mesh serializes.
            stream: vec![saffron_geometry::VertexSkin::default(); 4],
        }),
        morph: None,
    }
}

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
    // Every row is flagged rigged (the container carries a skin).
    assert!(bake.rows.iter().all(|r| r.rigged));

    // The mesh chunk decodes to the quad's shape.
    let reader = saffron_geometry::read_container(&full).expect("read_container");
    // The mesh sub-id is keyed by the mesh-bearing node ("root", index 0).
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
    // The parent + the non-remapped path is the container.
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
    // 1 mesh + 1 material, no textures, no clips = 2 sub-assets; 3 rows.
    assert_eq!(bake.rows.len(), 3);
    assert_eq!(meta.source_format, "obj");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_options_json_round_trips() {
    let options = ImportOptions {
        scale: 2.5,
        axis: Axis::ZUp,
        gen_tangents: false,
        embed_textures: true,
    };
    let restored = ImportOptions::from_json(&options.to_json());
    assert_eq!(restored, options);
    // The default options round-trip and the axis name is the wire string.
    let default_json = ImportOptions::default().to_json();
    assert_eq!(
        default_json
            .get("axis")
            .and_then(saffron_json::Value::as_str),
        Some("y-up")
    );
    assert_eq!(
        ImportOptions::from_json(&default_json),
        ImportOptions::default()
    );
}

#[test]
fn colorspace_for_role_keys_albedo_and_emissive_srgb() {
    let options = ImportOptions::default();
    use saffron_geometry::MaterialMapRole as Role;
    assert_eq!(options.colorspace_for(Role::Albedo), Colorspace::Srgb);
    assert_eq!(options.colorspace_for(Role::Emissive), Colorspace::Srgb);
    assert_eq!(options.colorspace_for(Role::Normal), Colorspace::Linear);
    assert_eq!(
        options.colorspace_for(Role::MetallicRoughness),
        Colorspace::Linear
    );
    assert_eq!(options.colorspace_for(Role::Occlusion), Colorspace::Linear);
    assert_eq!(options.colorspace_for(Role::Height), Colorspace::Linear);
}

#[test]
fn hash_file_fnv_is_content_addressed() {
    let dir = scratch("hash");
    let a = dir.join("a.bin");
    let b = dir.join("b.bin");
    std::fs::write(&a, b"hello world").unwrap();
    std::fs::write(&b, b"hello world").unwrap();
    let ha = hash_file_fnv(a.to_str().unwrap());
    let hb = hash_file_fnv(b.to_str().unwrap());
    assert_eq!(ha, hb, "identical content hashes identically");
    assert!(!ha.is_empty());
    // A different content hashes differently.
    std::fs::write(&b, b"goodbye world").unwrap();
    assert_ne!(ha, hash_file_fnv(b.to_str().unwrap()));
    // A missing file hashes to the empty string.
    assert_eq!(hash_file_fnv("/nonexistent/path/xyz"), "");
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

#[test]
fn import_options_default_matches_the_documented_defaults() {
    let options = ImportOptions::default();
    assert_eq!(options.scale, 1.0);
    assert_eq!(options.axis, Axis::YUp);
    assert!(options.gen_tangents);
    assert!(options.embed_textures);
    assert_eq!(IMPORTER_VERSION, 1);
}
