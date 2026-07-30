//! The node-forest rig tick: scoped name binding and per-node override writes.

use glam::Quat;
use saffron_geometry::{AnimClip, AnimPath, AnimTarget};
use saffron_scene::{
    Entity, MorphComponent, MorphWeightOverride, Name, PoseOverride, Relationship, Scene,
    Transform, quat_from_euler_xyz,
};

use crate::pose::JointPose;
use crate::sample::{sample_track, sample_weights};

use super::*;

/// A node entity's authored rest local TRS, read from its [`Transform`] (Euler → quat).
fn node_rest_pose(scene: &Scene, entity: Entity) -> JointPose {
    if entity == Entity::NULL || !scene.valid(entity) {
        return JointPose::default();
    }
    scene
        .with_component::<Transform, _>(entity, |t| JointPose {
            translation: t.translation,
            rotation: quat_from_euler_xyz(t.rotation),
            scale: t.scale,
        })
        .unwrap_or_default()
}

/// First-match pre-order descendant of `root` whose [`Name`] equals `name`, scoped to
/// `root`'s subtree (the player's forest). Never a global scan, so two instances of the
/// same-named forest each bind their own subtree.
fn find_named_descendant(scene: &Scene, root: Entity, name: &str) -> Option<Entity> {
    let children = scene
        .with_component::<Relationship, _>(root, |r| r.children.clone())
        .unwrap_or_default();
    for child in children {
        if !scene.valid(child) {
            continue;
        }
        let matches = scene
            .with_component::<Name, _>(child, |n| n.name == name)
            .unwrap_or(false);
        if matches {
            return Some(child);
        }
        if let Some(found) = find_named_descendant(scene, child, name) {
            return Some(found);
        }
    }
    None
}

/// Resolve each node-track target name to a forest [`Entity`], using the cached binding
/// when its handle is still valid and re-resolving by the scoped name walk on a stale
/// (destroyed / reordered) handle. The cache (`node_bindings[key]`) is rebuilt parallel to
/// `names` when its length disagrees (the clip changed).
pub(super) fn resolve_node_targets(
    runtime: &mut AnimationRuntime,
    scene: &Scene,
    key: u64,
    root: Entity,
    names: &[String],
) -> Vec<Entity> {
    let slots = runtime
        .node_bindings
        .entry(key)
        .or_insert_with(|| vec![None; names.len()]);
    if slots.len() != names.len() {
        *slots = vec![None; names.len()];
    }
    let mut out = Vec::with_capacity(names.len());
    for (i, name) in names.iter().enumerate() {
        let cached = slots[i].filter(|&e| scene.valid(e));
        let resolved = cached
            .or_else(|| find_named_descendant(scene, root, name))
            .unwrap_or(Entity::NULL);
        slots[i] = (resolved != Entity::NULL).then_some(resolved);
        out.push(resolved);
    }
    out
}

/// Drop a node-forest rig's `PoseOverride` + `MorphWeightOverride` from its bound entities
/// and forget its bindings, so the forest reverts to rest and the durable
/// `MorphComponent.weights`, and a re-entered player re-resolves fresh.
pub(super) fn clear_node_overrides(runtime: &mut AnimationRuntime, scene: &mut Scene, key: u64) {
    if let Some(slots) = runtime.node_bindings.remove(&key) {
        for entity in slots.into_iter().flatten() {
            if entity != Entity::NULL && scene.valid(entity) {
                scene.remove_component::<PoseOverride>(entity);
                scene.remove_component::<MorphWeightOverride>(entity);
            }
        }
    }
}

/// Process a node-forest rig: bind each node track to a forest entity by name, seed rest
/// from those entities' transforms, sample node-TRS tracks into `PoseOverride`s and
/// morph-weight tracks into `MorphWeightOverride`s, with the full transition path over the
/// driven node entities.
pub(super) fn tick_node_rig(
    runtime: &mut AnimationRuntime,
    scene: &mut Scene,
    dt: f32,
    rig: &Rig,
    clip: &AnimClip,
) {
    // Distinct node-track target names (TRS + weights), in first-appearance order.
    let mut names: Vec<String> = Vec::new();
    for track in &clip.tracks {
        if track.target == AnimTarget::Node && !names.iter().any(|n| n == &track.target_name) {
            names.push(track.target_name.clone());
        }
    }
    if names.is_empty() {
        runtime.last_pose.remove(&rig.key);
        return;
    }

    let targets = resolve_node_targets(runtime, scene, rig.key, rig.entity, &names);
    let time = advance_playback(scene, rig.entity, clip.duration, dt);

    let count = targets.len();
    let mut rest: Vec<JointPose> = Vec::with_capacity(count);
    for &entity in &targets {
        rest.push(node_rest_pose(scene, entity));
    }
    let mut final_local = rest.clone();
    let mut pose_driven = vec![false; count];

    // Node-TRS tracks write the bound entity's pose; weight tracks are handled after.
    for track in &clip.tracks {
        if track.target != AnimTarget::Node {
            continue;
        }
        let Some(idx) = names.iter().position(|n| n == &track.target_name) else {
            continue;
        };
        match track.path {
            AnimPath::Translation => {
                pose_driven[idx] = true;
                final_local[idx].translation = sample_track(track, time).truncate();
            }
            AnimPath::Rotation => {
                pose_driven[idx] = true;
                final_local[idx].rotation = Quat::from_vec4(sample_track(track, time));
            }
            AnimPath::Scale => {
                pose_driven[idx] = true;
                final_local[idx].scale = sample_track(track, time).truncate();
            }
            AnimPath::Weights => {}
        }
    }

    apply_transition(
        runtime,
        scene,
        rig.entity,
        rig.key,
        &targets,
        &rest,
        &mut final_local,
        dt,
    );

    for (i, &entity) in targets.iter().enumerate() {
        if !pose_driven[i] || entity == Entity::NULL || !scene.valid(entity) {
            continue;
        }
        let pose = final_local[i];
        let _ = scene.add_component(
            entity,
            PoseOverride {
                translation: pose.translation,
                rotation: pose.rotation,
                scale: pose.scale,
            },
        );
    }

    // Morph-weight tracks: seed from the durable `MorphComponent` weights (rest), sample,
    // and write the runtime-only `MorphWeightOverride`.
    for track in &clip.tracks {
        if track.path != AnimPath::Weights {
            continue;
        }
        let Some(idx) = names.iter().position(|n| n == &track.target_name) else {
            continue;
        };
        let entity = targets[idx];
        if entity == Entity::NULL || !scene.valid(entity) {
            continue;
        }
        let mut weights = scene
            .with_component::<MorphComponent, _>(entity, |m| m.weights.clone())
            .unwrap_or_default();
        if weights.len() < track.morph_count as usize {
            weights.resize(track.morph_count as usize, 0.0);
        }
        sample_weights(track, time, &mut weights);
        let _ = scene.add_component(entity, MorphWeightOverride { weights });
    }

    runtime.last_pose.insert(rig.key, final_local);
}
