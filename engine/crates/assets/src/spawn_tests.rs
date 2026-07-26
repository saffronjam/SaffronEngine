//! Spawn / instantiate tests (GPU-free: spawning is pure scene-ECS mutation over the
//! container META; no upload).

use crate::error::Error;
use crate::{AssetServer, ImportOptions};
use saffron_core::Uuid;
use saffron_geometry::glam::{Mat4, Quat, Vec3};
use saffron_geometry::{
    ImportedMaterial, ImportedModel, ImportedNode, ImportedSkin, Mesh, SkinPayload, Submesh,
    Vertex, VertexSkin,
};
use saffron_scene::{
    AnimationPlayer, AssetType, Bone, BonePhysicsComponent, MaterialSet, ModelInstance, Scene,
    SkinnedMesh,
};
use std::path::PathBuf;

/// A unique scratch dir under the system temp, removed and recreated per test.
fn scratch(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("saffron-assets-spawn-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A one-submesh triangle mesh (the minimal renderable shape).
fn tri_mesh() -> Mesh {
    Mesh {
        vertices: vec![
            Vertex {
                position: Vec3::ZERO,
                normal: Vec3::Z,
                uv0: saffron_geometry::glam::Vec2::ZERO,
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::X,
                normal: Vec3::Z,
                uv0: saffron_geometry::glam::Vec2::new(1.0, 0.0),
                ..Vertex::default()
            },
            Vertex {
                position: Vec3::Y,
                normal: Vec3::Z,
                uv0: saffron_geometry::glam::Vec2::new(0.0, 1.0),
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

/// A two-submesh quad (slots 0 and 1) so a multi-material spawn produces a `MaterialSet`.
fn quad_mesh() -> Mesh {
    let mut mesh = tri_mesh();
    mesh.vertices.push(Vertex {
        position: Vec3::ONE,
        normal: Vec3::Z,
        uv0: saffron_geometry::glam::Vec2::ONE,
        ..Vertex::default()
    });
    mesh.indices = vec![0, 1, 2, 0, 2, 3];
    mesh.submeshes = vec![
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
    ];
    mesh
}

/// Bakes `graph` into the server's catalog, returning the model id ready to instantiate.
fn bake_into_catalog(assets: &mut AssetServer, graph: &ImportedModel, source: &str) -> Uuid {
    let bake = assets
        .bake_model(graph, ImportOptions::default(), source, Uuid(0))
        .expect("bake");
    for row in &bake.rows {
        assets.catalog.put(row.clone());
    }
    bake.model_id
}

#[test]
fn instantiate_flat_model_spawns_one_mesh_entity_referencing_its_material() {
    let dir = scratch("flat");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let graph = ImportedModel {
        nodes: vec![ImportedNode {
            name: "mesh".to_owned(),
            mesh: Some(tri_mesh()),
            ..ImportedNode::default()
        }],
        materials: vec![ImportedMaterial {
            name: "flat".to_owned(),
            base_color: saffron_geometry::glam::Vec4::new(0.25, 0.5, 0.75, 1.0),
            metallic: 0.3,
            roughness: 0.4,
            ..ImportedMaterial::default()
        }],
        animations: Vec::new(),
        skin: None,
        morph: None,
        origin: Default::default(),
    };
    let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/flat.obj");

    let mut scene = Scene::new();
    let entity = assets
        .instantiate_model(&mut scene, model_id, "Cube")
        .expect("instantiate");

    // One material -> a `MaterialSet` with a single slot *referencing* the baked `.smat`
    // sub-id (no inline factor copy).
    assert!(scene.has_component::<saffron_scene::Mesh>(entity));
    assert!(scene.has_component::<MaterialSet>(entity));
    assert!(scene.has_component::<ModelInstance>(entity));

    let model = assets.load_model_asset(model_id).unwrap();
    let mesh_id = scene.component::<saffron_scene::Mesh>(entity).unwrap().mesh;
    let baked_mesh = model
        .meta
        .sub_assets
        .iter()
        .find(|s| s.asset_type == AssetType::Mesh)
        .unwrap()
        .sub_id;
    assert_eq!(
        mesh_id, baked_mesh,
        "the spawned mesh id is the baked sub-id"
    );

    let baked_material = model
        .meta
        .sub_assets
        .iter()
        .find(|s| s.asset_type == AssetType::Material)
        .unwrap()
        .sub_id;
    let slots = scene
        .with_component::<MaterialSet, _>(entity, |s| s.slots.clone())
        .expect("a material set");
    assert_eq!(slots.len(), 1, "one slot for the single source material");
    assert_ne!(
        slots[0].material.value(),
        0,
        "the slot references a real id"
    );
    assert_eq!(
        slots[0].material, baked_material,
        "the slot references the baked `.smat` sub-id"
    );

    let instance = scene.component::<ModelInstance>(entity).unwrap();
    assert_eq!(instance.model_id, model_id);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An *animated* single identity node must NOT collapse to one entity: the clip needs an
/// `AnimationPlayer` on a container root, so a collapsed (player-less) entity would lose the
/// animation. This is the glTF `SimpleMorph` shape — one identity node carrying a morph mesh
/// with a morph-weights clip.
#[test]
fn instantiate_animated_single_morph_node_keeps_its_player() {
    let dir = scratch("morphclip");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let clip = saffron_geometry::AnimClip {
        name: "morph".to_owned(),
        duration: 1.0,
        tracks: vec![saffron_geometry::AnimTrack {
            target: saffron_geometry::AnimTarget::Node,
            index: -1,
            target_name: "morphMesh".to_owned(),
            path: saffron_geometry::AnimPath::Weights,
            morph_count: 1,
            times: vec![0.0, 1.0],
            values: vec![0.0, 1.0],
            ..saffron_geometry::AnimTrack::default()
        }],
    };
    let graph = ImportedModel {
        // One node, identity transform, parent -1 — the exact collapse predicate, but animated.
        nodes: vec![ImportedNode {
            name: "morphMesh".to_owned(),
            mesh: Some(tri_mesh()),
            ..ImportedNode::default()
        }],
        materials: vec![ImportedMaterial {
            name: "m".to_owned(),
            ..ImportedMaterial::default()
        }],
        animations: vec![clip],
        skin: None,
        morph: Some(saffron_geometry::MorphData {
            targets: vec![saffron_geometry::MorphTarget {
                name: "bulge".to_owned(),
                rest_weight: 0.0,
                deltas: vec![saffron_geometry::MorphDelta {
                    vertex_index: 0,
                    d_position: Vec3::new(0.0, 1.0, 0.0),
                    d_normal: Vec3::ZERO,
                }],
            }],
        }),
        origin: Default::default(),
    };
    let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/morph.gltf");

    let mut scene = Scene::new();
    let container = assets
        .instantiate_model(&mut scene, model_id, "SimpleMorph")
        .expect("instantiate");
    assert!(scene.has_component::<ModelInstance>(container));

    // The clip survived on exactly ONE player (no rival player on the leaf), stopped,
    // autoplay opt-in (off), with the first clip attached.
    let mut players: Vec<AnimationPlayer> = Vec::new();
    scene.for_each::<&AnimationPlayer, _>(|_, p| players.push(*p));
    assert_eq!(
        players.len(),
        1,
        "exactly one AnimationPlayer for the model (no duplicate on the leaf)"
    );
    let player = players[0];
    assert!(!player.playing, "imported clips spawn stopped");
    assert!(!player.autoplay, "autoplay is opt-in (off on import)");
    assert_ne!(player.clip.value(), 0, "the morph clip id is attached");

    // The durable Morph component seeded on the mesh node (its names from the targets).
    let mut morph_names: Option<Vec<String>> = None;
    scene.for_each::<&saffron_scene::MorphComponent, _>(|_, m| morph_names = Some(m.names.clone()));
    assert_eq!(
        morph_names.as_deref(),
        Some(["bulge".to_owned()].as_slice()),
        "the morph mesh keeps its MorphComponent + target names",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn instantiate_multi_material_model_spawns_a_material_set_in_slot_order() {
    let dir = scratch("multimat");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let graph = ImportedModel {
        nodes: vec![ImportedNode {
            name: "mesh".to_owned(),
            mesh: Some(quad_mesh()),
            ..ImportedNode::default()
        }],
        materials: vec![
            ImportedMaterial {
                name: "a".to_owned(),
                base_color: saffron_geometry::glam::Vec4::new(1.0, 0.0, 0.0, 1.0),
                ..ImportedMaterial::default()
            },
            ImportedMaterial {
                name: "b".to_owned(),
                base_color: saffron_geometry::glam::Vec4::new(0.0, 1.0, 0.0, 1.0),
                ..ImportedMaterial::default()
            },
        ],
        animations: Vec::new(),
        skin: None,
        morph: None,
        origin: Default::default(),
    };
    let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/two.obj");

    let mut scene = Scene::new();
    let entity = assets
        .instantiate_model(&mut scene, model_id, "Two")
        .expect("instantiate");

    // Two source materials -> two slots, each referencing its baked `.smat` sub-id in the
    // baked (slot) order.
    let material_ids: Vec<Uuid> = assets
        .load_model_asset(model_id)
        .unwrap()
        .meta
        .sub_assets
        .iter()
        .filter(|s| s.asset_type == AssetType::Material)
        .map(|s| s.sub_id)
        .collect();
    assert_eq!(material_ids.len(), 2, "two baked material sub-assets");

    let set = scene
        .with_component::<MaterialSet, _>(entity, |s| s.slots.clone())
        .expect("a material set");
    assert_eq!(set.len(), 2, "two slots, in slot order");
    assert_eq!(
        set[0].material, material_ids[0],
        "slot 0 references material a"
    );
    assert_eq!(
        set[1].material, material_ids[1],
        "slot 1 references material b"
    );
    assert!(
        set.iter().all(|slot| slot.material.value() != 0),
        "both slots reference real sub-ids"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A rigged graph: two nodes (root, joint) with a non-identity joint rotation, one joint,
/// one clip — so the skinned spawn path exercises the node forest, bone tagging, the skin
/// descriptor, the animation player, and the META quaternion decode.
fn rigged_graph() -> ImportedModel {
    let clip = saffron_geometry::AnimClip {
        name: "idle".to_owned(),
        duration: 1.0,
        tracks: vec![saffron_geometry::AnimTrack {
            index: 1,
            target_name: "joint".to_owned(),
            ..saffron_geometry::AnimTrack::default()
        }],
    };
    // A 90-degree rotation about Y, stored on the joint node.
    let joint_rotation = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
    ImportedModel {
        origin: Default::default(),
        nodes: vec![
            // The skinned mesh node (mesh_node 0) carries the mesh node-locally.
            ImportedNode {
                name: "root".to_owned(),
                mesh: Some(tri_mesh()),
                ..ImportedNode::default()
            },
            ImportedNode {
                name: "joint".to_owned(),
                parent: 0,
                translation: Vec3::new(0.0, 1.0, 0.0),
                rotation: joint_rotation,
                ..ImportedNode::default()
            },
        ],
        materials: vec![ImportedMaterial {
            name: "skin".to_owned(),
            ..ImportedMaterial::default()
        }],
        animations: vec![clip],
        skin: Some(SkinPayload {
            desc: ImportedSkin {
                joints: vec![1],
                inverse_bind: vec![Mat4::IDENTITY],
                skeleton_root: 0,
                mesh_node: 0,
            },
            stream: vec![VertexSkin::default(); 3],
        }),
        morph: None,
    }
}

#[test]
fn instantiate_skinned_model_spawns_node_forest_bones_and_skin() {
    let dir = scratch("skinned");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let graph = rigged_graph();
    let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/rig.glb");

    let mut scene = Scene::new();
    let container = assets
        .instantiate_model(&mut scene, model_id, "Rig")
        .expect("instantiate");

    // The container root carries ModelInstance.
    assert!(scene.has_component::<ModelInstance>(container));

    // Find the skinned-mesh entity by query.
    let mut skinned: Option<(Uuid, usize, Uuid)> = None;
    scene.for_each::<&SkinnedMesh, _>(|_, skin| {
        skinned = Some((skin.mesh, skin.bones.len(), skin.root_bone));
    });
    let (skin_mesh, bone_count, root_bone) = skinned.expect("a skinned mesh exists");
    assert_eq!(bone_count, 1, "one joint in the skin");
    assert_ne!(skin_mesh.value(), 0, "the skinned mesh has a real sub-id");
    assert_ne!(root_bone.value(), 0, "skeletonRoot resolves to a node uuid");

    // Exactly one bone tag (the single joint).
    let mut bones = 0;
    scene.for_each::<&Bone, _>(|_, _| bones += 1);
    assert_eq!(bones, 1, "the single joint is bone-tagged");

    // The animation player is attached with the first clip, stopped, looping.
    let mut player: Option<AnimationPlayer> = None;
    scene.for_each::<&AnimationPlayer, _>(|_, p| player = Some(*p));
    let player = player.expect("an animation player exists");
    assert!(!player.playing, "imported rigs spawn stopped");
    assert_ne!(player.clip.value(), 0, "the first clip id is attached");

    // Auto-fit ran: a BonePhysicsComponent with one bone entry.
    let mut phys_bones = None;
    scene.for_each::<&BonePhysicsComponent, _>(|_, p| phys_bones = Some(p.bones.len()));
    assert_eq!(phys_bones, Some(1), "auto-fit produced one bone capsule");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn meta_quaternion_decode_reorders_to_glam_xyzw() {
    // The import writes r=[w,x,y,z]; the decode must rebuild glam xyzw.
    let rot = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
    let nodes = serde_json::json!([{
        "name": "j",
        "parent": -1,
        "t": [0.0, 0.0, 0.0],
        "r": [rot.w, rot.x, rot.y, rot.z],
        "s": [1.0, 1.0, 1.0],
    }]);
    let decoded = crate::spawn::imported_nodes_from_json(&nodes);
    assert_eq!(decoded.len(), 1);
    let q = decoded[0].rotation;
    assert!((q.x - rot.x).abs() < 1e-5);
    assert!((q.y - rot.y).abs() < 1e-5);
    assert!((q.z - rot.z).abs() < 1e-5);
    assert!((q.w - rot.w).abs() < 1e-5);
}

#[test]
fn instantiating_twice_yields_stable_soft_references() {
    let dir = scratch("twice");
    let root = dir.join("assets");
    let mut assets = AssetServer::new(&root);
    let graph = rigged_graph();
    let model_id = bake_into_catalog(&mut assets, &graph, "/tmp/rig.glb");

    let mut scene = Scene::new();
    let a = assets
        .instantiate_model(&mut scene, model_id, "Rig A")
        .expect("instantiate a");
    let b = assets
        .instantiate_model(&mut scene, model_id, "Rig B")
        .expect("instantiate b");
    assert_ne!(a, b, "two independent entity trees");

    // Both instances reference the same mesh sub-id (soft references stable).
    let mut meshes: Vec<Uuid> = Vec::new();
    scene.for_each::<&SkinnedMesh, _>(|_, skin| meshes.push(skin.mesh));
    assert_eq!(meshes.len(), 2);
    assert_eq!(
        meshes[0], meshes[1],
        "the same baked sub-id across instances"
    );

    let mut models: Vec<Uuid> = Vec::new();
    scene.for_each::<&ModelInstance, _>(|_, m| models.push(m.model_id));
    assert_eq!(models.len(), 2);
    assert!(models.iter().all(|m| *m == model_id));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_skinned_node_decode_recovers_the_joint_local_transform() {
    let skin = serde_json::json!({
        "joints": [1],
        "inverseBind": [[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]],
        "skeletonRoot": 0,
        "meshNode": 0,
    });
    let decoded = crate::spawn::imported_skin_from_json(&skin);
    assert_eq!(decoded.joints, vec![1]);
    assert_eq!(decoded.skeleton_root, 0);
    assert_eq!(decoded.mesh_node, 0);
    assert_eq!(decoded.inverse_bind.len(), 1);
    assert_eq!(decoded.inverse_bind[0], Mat4::IDENTITY);

    // A null / non-object skin decodes to an empty descriptor.
    assert!(
        crate::spawn::imported_skin_from_json(&serde_json::Value::Null)
            .joints
            .is_empty()
    );
}

/// `instantiate_model` for an id that is not in the catalog returns `NotInCatalog`.
#[test]
fn instantiate_missing_model_errors() {
    let dir = scratch("missing");
    let mut assets = AssetServer::new(dir.join("assets"));
    let mut scene = Scene::new();
    let err = assets
        .instantiate_model(&mut scene, Uuid(424_242), "Nope")
        .unwrap_err();
    assert!(matches!(err, Error::NotInCatalog(424_242)));
    let _ = std::fs::remove_dir_all(&dir);
}
