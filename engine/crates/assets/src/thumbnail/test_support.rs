use std::path::{Path, PathBuf};

use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{
    ChunkKind, ContainerChunk, Mesh, Submesh, Vertex, save_mesh_to_buffer, write_container,
};
use saffron_scene::AssetType;

use crate::AssetServer;
use crate::material::MaterialAsset;

/// A baked `.smesh` byte image (a single-triangle mesh) used to seed catalog rows.
fn smesh_bytes() -> Vec<u8> {
    let mesh = Mesh {
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
                position: Vec3::Y,
                normal: Vec3::Z,
                uv0: Vec2::new(0.0, 1.0),
                ..Vertex::default()
            },
        ],
        indices: vec![0, 1, 2],
        submeshes: vec![Submesh {
            first_index: 0,
            index_count: 3,
            vertex_offset: 0,
            material_slot: 0,
        }],
    };
    save_mesh_to_buffer(&mesh, &[], None).unwrap()
}

pub(super) fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "saffron-thumb-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("project").join("assets")
}

/// An asset server whose content-addressed thumbnail cache is isolated to a unique temp
/// dir. The production cache is app-level (shared), so tests must point it at their own
/// dir to get a cold miss and exercise the resolve/enqueue path deterministically.
pub(super) fn isolated_server(root: &Path) -> AssetServer {
    let mut assets = AssetServer::new(root);
    assets.thumbnail_cache_root = root.parent().unwrap_or(root).join("thumbnail-cache");
    let _ = std::fs::remove_dir_all(&assets.thumbnail_cache_root);
    assets
}

pub(super) fn put_embedded_material_model(
    assets: &mut AssetServer,
    model_id: Uuid,
    material_id: Uuid,
    material: &MaterialAsset,
) {
    let mesh_id = Uuid(model_id.value() + 1);
    let rel = format!("models/{}.smodel", model_id.value());
    std::fs::create_dir_all(assets.root.join("models")).expect("models dir");

    let mut meta = crate::model::ContainerMetadata {
        model_id,
        name: "embedded".to_owned(),
        ..crate::model::ContainerMetadata::default()
    };
    meta.sub_assets.push(crate::model::SubAsset {
        sub_id: mesh_id,
        asset_type: AssetType::Mesh,
        name: "mesh".to_owned(),
        chunk: 1,
        ..crate::model::SubAsset::default()
    });
    meta.sub_assets.push(crate::model::SubAsset {
        sub_id: material_id,
        asset_type: AssetType::Material,
        name: "mat".to_owned(),
        chunk: 2,
        ..crate::model::SubAsset::default()
    });

    let meta_bytes = crate::model::encode_container_metadata(&meta);
    let mesh_bytes = smesh_bytes();
    let material_doc = crate::material::material_asset_to_json(material);
    let material_bytes = saffron_json::dump_json(&material_doc, -1).into_bytes();
    let chunks = [
        ContainerChunk {
            kind: ChunkKind::Meta,
            sub_id: 0,
            flags: 0,
            bytes: &meta_bytes,
        },
        ContainerChunk {
            kind: ChunkKind::Mesh,
            sub_id: mesh_id.value(),
            flags: 0,
            bytes: &mesh_bytes,
        },
        ContainerChunk {
            kind: ChunkKind::Material,
            sub_id: material_id.value(),
            flags: 0,
            bytes: &material_bytes,
        },
    ];
    write_container(assets.root.join(&rel), &chunks).expect("smodel");
    for row in crate::import::catalog_rows_for_container(&meta, &rel, AssetType::Model) {
        assets.catalog.put(row);
    }
}
