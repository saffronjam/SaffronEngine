use saffron_core::Uuid;
use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{ImportedMaterial, ImportedModel, Mesh, Submesh, TextureSource, Vertex};
use saffron_scene::AssetType;

use crate::AssetServer;
use crate::import::{ImportOptions, catalog_rows_for_container};
use crate::model::read_container_metadata;

/// A unique scratch dir under the system temp, removed and recreated per test.
pub(super) fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "saffron-assets-manage-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A single-triangle mesh so a baked mesh chunk decodes.
pub(super) fn tri_mesh() -> Mesh {
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
    }
}

/// A static model with one material that references an embedded albedo texture.
pub(super) fn one_material_graph() -> ImportedModel {
    ImportedModel {
        origin: Default::default(),
        nodes: vec![saffron_geometry::ImportedNode {
            name: "mesh".to_owned(),
            mesh: Some(tri_mesh()),
            ..saffron_geometry::ImportedNode::default()
        }],
        materials: vec![ImportedMaterial {
            name: "paint".to_owned(),
            albedo: Some(TextureSource {
                bytes: vec![0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4],
                ext: "png".to_owned(),
            }),
            ..ImportedMaterial::default()
        }],
        animations: Vec::new(),
        skin: None,
        morph: None,
    }
}

/// Bakes `graph` from `source_path`, registers its catalog rows, and returns the container's
/// model id.
pub(super) fn bake_and_register(
    assets: &mut AssetServer,
    graph: &ImportedModel,
    source_path: &str,
) -> Uuid {
    let bake = assets
        .bake_model(graph, ImportOptions::default(), source_path, Uuid(0))
        .expect("bake");
    let full = format!("{}/{}", assets.root.display(), bake.path);
    let meta = read_container_metadata(&full).expect("meta");
    for row in catalog_rows_for_container(&meta, &bake.path, AssetType::Model) {
        assets.catalog.put(row);
    }
    bake.model_id
}

/// The first sub-asset of `asset_type` in the catalog (by container id).
pub(super) fn first_sub(assets: &AssetServer, container: Uuid, asset_type: AssetType) -> Uuid {
    assets
        .catalog
        .entries
        .iter()
        .find(|e| e.container.value() == container.value() && e.asset_type == asset_type)
        .map(|e| e.id)
        .expect("sub-asset present")
}

pub(super) fn png_2x2() -> Vec<u8> {
    let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba([180, 120, 60, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    buffer
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}
