use std::path::PathBuf;

use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{
    AnimClip, AnimTrack, ImportedMaterial, ImportedModel, Mesh, Submesh, TextureSource, Vertex,
};

pub(super) fn scratch(tag: &str) -> PathBuf {
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
pub(super) fn quad_mesh() -> Mesh {
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
pub(super) fn town_graph() -> ImportedModel {
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
            stream: vec![saffron_geometry::VertexSkin::default(); 4],
        }),
        morph: None,
    }
}
