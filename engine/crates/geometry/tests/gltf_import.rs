//! glTF import tests over the real fixtures.
//!
//! The fixtures live in `tests/fixtures/` — copies of the engine's sample models, each
//! with an embedded base64 buffer so the test is self-contained.

use std::path::PathBuf;

use saffron_geometry::{
    AnimInterp, AnimPath, AnimTarget, ImportedModel, Mesh, load_animation_from_bytes,
    load_mesh_from_bytes, load_mesh_skin_from_bytes, save_animation_to_buffer, save_mesh_to_buffer,
    translate_model,
};

/// Path to a fixture under `tests/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn import(name: &str) -> ImportedModel {
    translate_model(fixture(name)).unwrap_or_else(|e| panic!("import {name} failed: {e}"))
}

/// The first mesh-bearing node's node-local mesh — every import now owns its geometry on
/// a forest node, never a top-level model mesh.
fn mesh_of(model: &ImportedModel) -> &Mesh {
    model
        .nodes
        .iter()
        .find_map(|n| n.mesh.as_ref())
        .expect("a mesh-bearing node")
}

#[test]
fn cube_imports_with_expected_counts_and_a_material_slot() {
    let model = import("cube.gltf");
    // cube.gltf: one node carrying a mesh, one primitive (24 verts, 36 indices).
    let mesh = mesh_of(&model);
    assert_eq!(mesh.vertices.len(), 24);
    assert_eq!(mesh.indices.len(), 36);
    assert_eq!(mesh.submeshes.len(), 1);
    // A null source material still yields one default slot.
    assert_eq!(model.materials.len(), 1);
    assert!(model.skin.is_none());
}

#[test]
fn cube_normals_survive_import() {
    // cube.gltf ships normals, so the importer keeps them (never falls back to
    // generate_normals); every vertex normal is unit length.
    let model = import("cube.gltf");
    for (i, v) in mesh_of(&model).vertices.iter().enumerate() {
        let len = v.normal.length();
        assert!((len - 1.0).abs() < 1e-4, "vertex {i} normal length {len}");
    }
}

#[test]
fn animated_strip_imports_a_skin_with_a_decoded_clip() {
    let model = import("animated-strip.gltf");
    let skin = model.skin.as_ref().expect("animated-strip is skinned");
    assert!(!model.animations.is_empty(), "expected at least one clip");

    // The skin stream parallels the skinned node's vertices one-for-one.
    assert_eq!(skin.stream.len(), mesh_of(&model).vertices.len());
    // joints [1, 2] of a 3-node forest.
    assert_eq!(skin.desc.joints, vec![1, 2]);
    assert_eq!(model.nodes.len(), 3);

    let clip = &model.animations[0];
    assert_eq!(clip.name, "Bend");
    let track = &clip.tracks[0];
    // The "Bend" channel targets node 1 (RootJoint), which is joints[0].
    assert_eq!(track.target, AnimTarget::Bone);
    assert_eq!(track.index, 0);
    assert_eq!(track.target_name, "RootJoint");
    assert_eq!(track.path, AnimPath::Rotation);
    assert_eq!(track.interp, AnimInterp::Linear);
    // A rotation track stores 4 floats (xyzw) per key.
    assert_eq!(track.values.len(), track.times.len() * 4);
    assert!(clip.duration > 0.0);
}

#[test]
fn two_materials_yields_two_slots_in_first_seen_order() {
    let model = import("two-materials.gltf");
    assert_eq!(model.materials.len(), 2);
    // First-seen order: ShinyMetal (slot 0), then RoughDielectric (slot 1).
    assert_eq!(model.materials[0].name, "ShinyMetal");
    assert_eq!(model.materials[1].name, "RoughDielectric");
    // Two primitives, two submeshes, each pointing at its own slot.
    let mesh = mesh_of(&model);
    assert_eq!(mesh.submeshes.len(), 2);
    assert_eq!(mesh.submeshes[0].material_slot, 0);
    assert_eq!(mesh.submeshes[1].material_slot, 1);
}

#[test]
fn cube_import_is_deterministic() {
    // Two imports of the same source yield structurally identical graphs.
    let first = import("cube.gltf");
    let second = import("cube.gltf");
    let first_mesh = mesh_of(&first);
    let second_mesh = mesh_of(&second);
    assert_eq!(first_mesh.vertices.len(), second_mesh.vertices.len());
    assert_eq!(first_mesh.indices.len(), second_mesh.indices.len());
    assert_eq!(first_mesh.submeshes.len(), second_mesh.submeshes.len());
    assert_eq!(first.materials.len(), second.materials.len());
    assert_eq!(first.skin.is_some(), second.skin.is_some());
    assert_eq!(
        first_mesh.vertices.first().map(|v| v.position),
        second_mesh.vertices.first().map(|v| v.position),
    );
    assert_eq!(
        first_mesh.vertices.last().map(|v| v.position),
        second_mesh.vertices.last().map(|v| v.position),
    );
    // The whole import graph compares equal (it is `PartialEq`).
    assert_eq!(first, second);
}

#[test]
fn unsupported_extension_is_rejected() {
    let err = translate_model("model.fbx").unwrap_err();
    assert!(
        err.to_string().contains("unsupported model format"),
        "got: {err}"
    );
}

#[test]
fn skinned_strip_round_trips_through_smesh_and_sanim() {
    // Import a skinned model, bake the mesh + skin into a `.smesh` buffer, read it back,
    // and round-trip a clip through `.sanim`.
    let model = import("animated-strip.gltf");
    let skin = model.skin.as_ref().expect("animated-strip is skinned");
    let mesh = mesh_of(&model);

    let baked = save_mesh_to_buffer(mesh, &skin.stream, None)
        .expect("skinned encode parallels the vertices");
    let loaded_mesh = load_mesh_from_bytes(&baked).expect("load mesh from baked skinned .smesh");
    let loaded_skin = load_mesh_skin_from_bytes(&baked).expect("load skin from baked .smesh");

    assert_eq!(loaded_mesh.vertices, mesh.vertices);
    assert_eq!(loaded_mesh.indices, mesh.indices);
    assert_eq!(loaded_mesh.submeshes, mesh.submeshes);
    assert_eq!(loaded_skin, skin.stream);

    let clip = &model.animations[0];
    let anim_bytes = save_animation_to_buffer(clip);
    let loaded_clip = load_animation_from_bytes(&anim_bytes).expect("load clip from .sanim");
    assert_eq!(&loaded_clip, clip);

    // An unskinned bake of the same mesh also round-trips.
    let unskinned = save_mesh_to_buffer(mesh, &[], None).expect("unskinned round-trip encode");
    let back = load_mesh_from_bytes(&unskinned).expect("unskinned round-trip");
    assert_eq!(back.vertices, mesh.vertices);
}

#[test]
fn morph_target_names_preserve_mesh_extras() {
    let model = saffron_geometry::translate_model(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/e2e/fixtures/AnimatedMorphCube.gltf"),
    )
    .expect("import named morph fixture");
    let morph = model.morph.expect("fixture has one morph target");

    assert_eq!(morph.targets.len(), 1);
    assert_eq!(morph.targets[0].name, "bulge");
    assert_eq!(morph.targets[0].rest_weight, 0.0);
}
