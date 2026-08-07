mod foot_ik;
mod node_rigs;
mod playback;
mod transitions;

use glam::{Quat, Vec3};
use saffron_geometry::{AnimInterp, AnimPath, AnimTarget, AnimTrack};
use saffron_scene::{
    AnimationPlayer, Bone, FootChain, FootIk, IdComponent, MorphComponent, MorphWeightOverride,
    PoseOverride, Scene, SkinnedMesh, Transform,
};
use saffron_test_support::{EPS, quat_close};

use super::*;
use crate::Error;

/// Builds a one-rig scene: a rig entity with an [`AnimationPlayer`] + [`SkinnedMesh`]
/// over a single chain of `bone_count` directly-parented bones, with the bone handles
/// resolved by the relink. Returns `(scene, rig, bone_entities)`.
fn rig_scene(bone_count: usize, clip: Uuid) -> (Scene, Entity, Vec<Entity>) {
    let mut scene = Scene::new();
    let rig = scene.create_entity("Rig");

    let mut bones: Vec<Entity> = Vec::new();
    let mut parent: Option<Entity> = None;
    for i in 0..bone_count {
        let bone = scene.create_entity(format!("bone{i}"));
        scene.add_component(bone, Bone::default()).unwrap();
        scene
            .with_component_mut::<Transform, _>(bone, |t| {
                // Each bone sits one unit further along +X in its parent's frame.
                t.translation = Vec3::new(1.0, 0.0, 0.0);
            })
            .unwrap();
        if let Some(p) = parent {
            scene.set_parent(bone, Some(p), false).unwrap();
        }
        parent = Some(bone);
        bones.push(bone);
    }

    let bone_ids: Vec<Uuid> = bones
        .iter()
        .map(|&b| scene.component::<IdComponent>(b).unwrap().id)
        .collect();
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_ids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            AnimationPlayer {
                clip,
                playing: true,
                wrap: Wrap::Loop,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    (scene, rig, bones)
}

/// A clip with a single translation track on joint `joint`, two keys 0→1s, the value
/// moving from `0` to `(end_x, 0, 0)`.
fn translate_clip(joint: i32, end_x: f32) -> AnimClip {
    AnimClip {
        name: "test".to_string(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            index: joint,
            path: AnimPath::Translation,
            interp: AnimInterp::Linear,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, end_x, 0.0, 0.0],
            ..Default::default()
        }],
    }
}

/// A loader closure that resolves any clip id to `clip` (the per-call [`ClipLoader`] the
/// host backs with the live catalog; tests back it with a fixed clip).
fn clip_loader(clip: AnimClip) -> impl Fn(Uuid) -> Result<AnimClip> {
    move |_id| Ok(clip.clone())
}

/// The preview-block rig: two bones (`Root`, `Tip`) parented under a `Rig`, joint 0
/// bound to a LINEAR rotation track spinning 0→90° about Y over 1s by durable name.
/// Returns `(scene, rig, root_bone, clip)`; the caller wraps `clip` in a [`clip_loader`].
fn spin_preview_scene() -> (Scene, Entity, Entity, AnimClip) {
    let s = 0.5_f32.sqrt();
    let clip_id = Uuid(1234);
    let mut scene = Scene::new();
    let root_bone = scene.create_entity("Root");
    let tip_bone = scene.create_entity("Tip");
    scene.add_component(root_bone, Bone::default()).unwrap();
    scene.add_component(tip_bone, Bone::default()).unwrap();

    let rig = scene.create_entity("Rig");
    let bone_ids = vec![
        scene.component::<IdComponent>(root_bone).unwrap().id,
        scene.component::<IdComponent>(tip_bone).unwrap().id,
    ];
    scene
        .add_component(
            rig,
            SkinnedMesh {
                bones: bone_ids,
                ..SkinnedMesh::default()
            },
        )
        .unwrap();
    scene
        .add_component(
            rig,
            AnimationPlayer {
                clip: clip_id,
                wrap: Wrap::Loop,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();

    let clip = AnimClip {
        name: "spin".to_string(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            index: 0,
            target_name: "Root".to_string(),
            path: AnimPath::Rotation,
            interp: AnimInterp::Linear,
            times: vec![0.0, 1.0],
            // xyzw: identity, then 90° about Y.
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, s, 0.0, s],
            ..Default::default()
        }],
    };
    (scene, rig, root_bone, clip)
}

/// A node-forest scene: a container root "Forest" carrying an [`AnimationPlayer`] and
/// **no** `SkinnedMesh`, with "NodeA" parented under it and "NodeB" nested under NodeA.
/// Returns `(scene, container, [node_a, node_b])`.
fn node_forest_scene(clip: Uuid) -> (Scene, Entity, Vec<Entity>) {
    let mut scene = Scene::new();
    let container = scene.create_entity("Forest");
    let node_a = scene.create_entity("NodeA");
    let node_b = scene.create_entity("NodeB");
    scene.set_parent(node_a, Some(container), false).unwrap();
    scene.set_parent(node_b, Some(node_a), false).unwrap();
    scene
        .add_component(
            container,
            AnimationPlayer {
                clip,
                playing: true,
                wrap: Wrap::Loop,
                ..AnimationPlayer::default()
            },
        )
        .unwrap();
    scene.relink_hierarchy();
    (scene, container, vec![node_a, node_b])
}

/// A clip with one node-TRS translation track binding by name, 0→`(end_x,0,0)` over 1s.
fn node_translate_clip(name: &str, end_x: f32) -> AnimClip {
    AnimClip {
        name: "node".to_string(),
        duration: 1.0,
        tracks: vec![AnimTrack {
            target: AnimTarget::Node,
            index: -1,
            target_name: name.to_string(),
            path: AnimPath::Translation,
            interp: AnimInterp::Linear,
            morph_count: 0,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, end_x, 0.0, 0.0],
        }],
    }
}
