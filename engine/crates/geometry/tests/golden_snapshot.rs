//! Byte-exact golden snapshots for the `.smesh` / `.sanim` / `.smodel` formats.
//!
//! Each test bakes a deterministic fixture input with the writer and asserts the bytes
//! are byte-identical to a committed fixture under `fixtures/golden/`, so a drift in
//! either the writer or the input surfaces as a byte mismatch with a windowed hexdump
//! from [`assert_bytes_match_golden`].
//!
//! Reseed with `UPDATE_GOLDEN=1` only to land an intentional, writer-and-fixture-together
//! format change — never to quiet a real drift.

use saffron_geometry::glam::{Vec2, Vec3};
use saffron_geometry::{
    AnimClip, AnimInterp, AnimPath, AnimTarget, AnimTrack, ChunkKind, ContainerChunk, Mesh,
    Submesh, Vertex, read_container, save_animation_to_buffer, save_mesh_to_buffer,
    write_container,
};
use saffron_test_support::assert_bytes_match_golden;

/// The canonical unit cube: 24 vertices (per-face normals + uvs), 36 indices, one submesh.
fn cube_mesh() -> Mesh {
    // (normal, four corners) per face, in order: +Z, -Z, +X, -X, +Y, -Y.
    let faces: [(Vec3, [Vec3; 4]); 6] = [
        (
            Vec3::new(0.0, 0.0, 1.0),
            [
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::new(0.0, 0.0, -1.0),
            [
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::new(1.0, 0.0, 0.0),
            [
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(1.0, 1.0, 1.0),
            ],
        ),
        (
            Vec3::new(-1.0, 0.0, 0.0),
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(-1.0, -1.0, 1.0),
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::new(0.0, 1.0, 0.0),
            [
                Vec3::new(-1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(1.0, 1.0, -1.0),
                Vec3::new(-1.0, 1.0, -1.0),
            ],
        ),
        (
            Vec3::new(0.0, -1.0, 0.0),
            [
                Vec3::new(-1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, -1.0),
                Vec3::new(1.0, -1.0, 1.0),
                Vec3::new(-1.0, -1.0, 1.0),
            ],
        ),
    ];
    let uv = [
        Vec2::new(0.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(0.0, 1.0),
    ];
    let mut mesh = Mesh::default();
    for (normal, corners) in faces {
        let base = mesh.vertices.len() as u32;
        for i in 0..4 {
            mesh.vertices.push(Vertex {
                position: corners[i],
                normal,
                uv0: uv[i],
                ..Vertex::default()
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh.submeshes.push(Submesh {
        first_index: 0,
        index_count: mesh.indices.len() as u32,
        vertex_offset: 0,
        material_slot: 0,
    });
    mesh
}

/// The canonical clip: one rotation track and one translation track.
fn cube_clip() -> AnimClip {
    AnimClip {
        name: "CubeSpin".to_owned(),
        duration: 2.0,
        tracks: vec![
            AnimTrack {
                target: AnimTarget::Bone,
                index: 0,
                target_name: "Root".to_owned(),
                path: AnimPath::Rotation,
                interp: AnimInterp::Linear,
                morph_count: 0,
                times: vec![0.0, 1.0, 2.0],
                values: vec![
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                    0.0,
                    0.707_106_77,
                    0.0,
                    0.707_106_77,
                    0.0,
                    1.0,
                    0.0,
                    0.0,
                ],
            },
            AnimTrack {
                target: AnimTarget::Bone,
                index: 1,
                target_name: "Lid".to_owned(),
                path: AnimPath::Translation,
                interp: AnimInterp::Step,
                morph_count: 0,
                times: vec![0.0, 2.0],
                values: vec![0.0, 0.0, 0.0, 0.0, 0.5, 0.0],
            },
        ],
    }
}

#[test]
fn cube_smesh_bytes_match_cpp_golden() {
    let bytes = save_mesh_to_buffer(&cube_mesh(), &[], None).unwrap();
    assert_bytes_match_golden("cube.smesh", &bytes);
}

#[test]
fn cube_sanim_bytes_match_cpp_golden() {
    let bytes = save_animation_to_buffer(&cube_clip());
    assert_bytes_match_golden("cube.sanim", &bytes);
}

#[test]
fn cube_smodel_bytes_match_cpp_golden() {
    // A self-contained container: a small fixed META JSON + the cube MESH chunk. The META
    // string is two-space-indented with sorted keys.
    let meta = concat!(
        "{\n",
        "  \"materialCount\": 0,\n",
        "  \"meshSubId\": 1,\n",
        "  \"name\": \"cube\",\n",
        "  \"schemaVersion\": 1\n",
        "}"
    );
    let mesh_bytes = save_mesh_to_buffer(&cube_mesh(), &[], None).unwrap();
    let chunks = [
        ContainerChunk {
            kind: ChunkKind::Meta,
            sub_id: 0,
            flags: 0,
            bytes: meta.as_bytes(),
        },
        ContainerChunk {
            kind: ChunkKind::Mesh,
            sub_id: 1,
            flags: 0,
            bytes: &mesh_bytes,
        },
    ];

    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "saffron-golden-smodel-{}.smodel",
        std::process::id()
    ));
    write_container(&path, &chunks).expect("write the cube container");
    let bytes = std::fs::read(&path).expect("read back the container");
    let _ = std::fs::remove_file(&path);

    assert_bytes_match_golden("cube.smodel", &bytes);

    // Re-opening the committed bytes must round-trip the MESH chunk, proving the golden is a
    // valid container the reader accepts (not just a byte blob).
    let golden = saffron_test_support::golden_dir().join("cube.smodel");
    let reader = read_container(&golden).expect("open the golden container");
    let mesh_entry = reader
        .find(ChunkKind::Mesh, 1)
        .expect("the golden carries a MESH chunk at sub_id 1");
    let chunk = reader.read_chunk(mesh_entry).expect("read the MESH chunk");
    assert_eq!(
        chunk, mesh_bytes,
        "the embedded mesh chunk reads back exactly"
    );
}
