//! The skinned-rig tick: clip sampling into the bone pose buffer plus the foot-IK pass.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use saffron_geometry::{AnimClip, AnimPath, AnimTarget};
use saffron_scene::{Entity, FootIk, Name, PoseOverride, Relationship, Scene, SkinnedMesh};

use crate::ik::solve_two_bone_ik;
use crate::pose::{JointPose, PoseBuffer};
use crate::sample::sample_track;

use super::*;

/// Drop every pose override on a rig's bones so they revert to the rest pose.
pub(super) fn clear_overrides(scene: &mut Scene, bone_handles: &[Entity]) {
    for &handle in bone_handles {
        if handle != Entity::NULL && scene.valid(handle) {
            scene.remove_component::<PoseOverride>(handle);
        }
    }
}

/// by the durable node name when the index is stale (out of range, or names disagree).
///
/// The durable-name re-bind keeps a clip playing across a reimport that reorders joints.
fn sample_clip_resolved(
    clip: &AnimClip,
    t: f32,
    bone_names: &[String],
    name_to_index: &HashMap<String, i32>,
    out: &mut PoseBuffer,
) {
    let joint_count = out.local.len() as i32;
    for track in &clip.tracks {
        // Only bone tracks write the joint pose buffer. Node-TRS and morph-weight tracks
        // bind by name to their own write seams in the runtime evaluator.
        if track.target != AnimTarget::Bone {
            continue;
        }
        let mut joint = track.index;
        let stale = joint < 0
            || joint >= joint_count
            || (!track.target_name.is_empty()
                && (joint as usize) < bone_names.len()
                && bone_names[joint as usize] != track.target_name);
        if stale {
            joint = name_to_index.get(&track.target_name).copied().unwrap_or(-1);
        }
        if joint < 0 || joint >= joint_count {
            continue;
        }
        let v = sample_track(track, t);
        let j = joint as usize;
        match track.path {
            AnimPath::Translation => out.local[j].translation = v.truncate(),
            AnimPath::Rotation => out.local[j].rotation = Quat::from_vec4(v),
            AnimPath::Scale => out.local[j].scale = v.truncate(),
            AnimPath::Weights => {}
        }
    }
}

/// Sample `source` at `time` into a fresh pose seeded with the rest pose, durable-name
/// resolving each track's joint binding.
fn sample_into(
    source: &AnimClip,
    time: f32,
    rest: &[JointPose],
    bone_names: &[String],
    name_to_index: &HashMap<String, i32>,
) -> Vec<JointPose> {
    let mut pose = PoseBuffer {
        local: rest.to_vec(),
        ..Default::default()
    };
    sample_clip_resolved(source, time, bone_names, name_to_index, &mut pose);
    pose.local
}

/// Foot-IK blend-layer producer: for each enabled chain, resolve the chain by forward
/// kinematics from this frame's sampled `final_local` (deliberately not the cached world
/// transform — that is last frame's post-IK output and would feed the solver its own
/// result), lift the foot target up to `ground_height`, solve, and write the new local
/// rotations back into `final_local`. Never touches a bone's [`Transform`].
fn apply_foot_ik(scene: &Scene, skin: &SkinnedMesh, ik: &FootIk, final_local: &mut [JointPose]) {
    let joint_count = final_local.len() as i32;
    let handle_of = |idx: i32| -> Entity {
        if idx < 0 || idx >= skin.bone_handles.len() as i32 {
            return Entity::NULL;
        }
        skin.bone_handles[idx as usize]
    };

    for chain in &ik.chains {
        let upper_h = handle_of(chain.upper);
        let mid_h = handle_of(chain.mid);
        let end_h = handle_of(chain.end);
        if upper_h == Entity::NULL || mid_h == Entity::NULL || end_h == Entity::NULL {
            continue;
        }
        if chain.upper >= joint_count || chain.mid >= joint_count || chain.end >= joint_count {
            continue;
        }
        if !scene.valid(upper_h) || !scene.valid(mid_h) || !scene.valid(end_h) {
            continue;
        }

        // Resolve the chain from this frame's animated pose by forward kinematics rather than the
        // cached world transform, which is a frame stale. The chain is directly parented
        // (upper→mid→end) at unit bone scale, as the foot-chain config describes.
        let ui = chain.upper as usize;
        let mi = chain.mid as usize;
        let ei = chain.end as usize;
        let mut parent_pos = Vec3::ZERO;
        let mut parent_rot = Quat::IDENTITY;
        let upper_parent = scene
            .with_component::<Relationship, _>(upper_h, |rel| rel.parent_handle)
            .unwrap_or(None);
        if let Some(parent) = upper_parent
            && scene.valid(parent)
        {
            parent_pos = scene.world_translation(parent);
            parent_rot = scene.world_rotation(parent);
        }
        let w_upper_rot = (parent_rot * final_local[ui].rotation).normalize();
        let root_pos = parent_pos + parent_rot * final_local[ui].translation;
        let w_mid_rot = (w_upper_rot * final_local[mi].rotation).normalize();
        let mid_pos = root_pos + w_upper_rot * final_local[mi].translation;
        let end_pos = mid_pos + w_mid_rot * final_local[ei].translation;
        let upper_len = (mid_pos - root_pos).length();
        let lower_len = (end_pos - mid_pos).length();
        if upper_len < 1e-5 || lower_len < 1e-5 {
            continue;
        }

        // Plant the foot by lifting its world Y up to the ground plane. A foot already above the
        // plane is never pulled down to it.
        let mut target = end_pos;
        target.y = target.y.max(ik.ground_height);

        let solved = solve_two_bone_ik(
            root_pos,
            mid_pos,
            end_pos,
            target,
            chain.pole_vector,
            upper_len,
            lower_len,
        );

        // The solved quats are world deltas: the upper swings the whole chain, the mid
        // additionally bends (it inherits the upper's swing as the upper's child). Strip
        // the parent world rotation to land each in local space.
        let new_upper_world = (solved.upper * w_upper_rot).normalize();
        let new_mid_world = (solved.upper * solved.lower * w_mid_rot).normalize();
        final_local[ui].rotation = (parent_rot.inverse() * new_upper_world).normalize();
        final_local[mi].rotation = (new_upper_world.inverse() * new_mid_world).normalize();
    }
}

/// Process a skinned rig: seed rest from the bones, sample bone-TRS tracks, apply the
/// transition + foot-IK, and write a `PoseOverride` per bone.
pub(super) fn tick_skinned_rig(
    runtime: &mut AnimationRuntime,
    scene: &mut Scene,
    dt: f32,
    rig: &Rig,
    clip: &AnimClip,
    joint_count: usize,
) {
    // Seed each bone's rest local TRS so untracked joints (and untracked channels of a
    // tracked joint) keep their authored value, and collect the name↔index maps for
    // durable track resolution.
    let skin = scene
        .with_component::<SkinnedMesh, _>(rig.entity, Clone::clone)
        .unwrap_or_default();
    let mut rest: Vec<JointPose> = vec![JointPose::default(); joint_count];
    let mut bone_names: Vec<String> = vec![String::new(); joint_count];
    let mut name_to_index: HashMap<String, i32> = HashMap::new();
    for i in 0..joint_count {
        rest[i] = rest_pose_of(scene, &skin, i);
        if let Some(&bone) = skin.bone_handles.get(i)
            && bone != Entity::NULL
            && scene.valid(bone)
            && let Ok(name) = scene.with_component::<Name, _>(bone, |n| n.name.clone())
        {
            bone_names[i] = name.clone();
            name_to_index.insert(name, i as i32);
        }
    }

    let time = advance_playback(scene, rig.entity, clip.duration, dt);
    let mut final_local = sample_into(clip, time, &rest, &bone_names, &name_to_index);
    apply_transition(
        runtime,
        scene,
        rig.entity,
        rig.key,
        &skin.bone_handles,
        &rest,
        &mut final_local,
        dt,
    );

    // External pose producer: kinematic foot IK feeds the same override/weight blend
    // layer ragdoll will use, mixed into final_local before the bones are written. Gated
    // on the component so non-IK rigs pay nothing.
    let foot_ik = scene
        .with_component::<FootIk, _>(rig.entity, Clone::clone)
        .ok()
        .filter(|ik| ik.enabled);
    if let Some(ik) = foot_ik {
        apply_foot_ik(scene, &skin, &ik, &mut final_local);
    }

    for (i, pose) in final_local.iter().enumerate().take(joint_count) {
        let Some(&handle) = skin.bone_handles.get(i) else {
            continue;
        };
        if handle == Entity::NULL || !scene.valid(handle) {
            continue;
        }
        let _ = scene.add_component(
            handle,
            PoseOverride {
                translation: pose.translation,
                rotation: pose.rotation,
                scale: pose.scale,
            },
        );
    }

    // Snapshot this frame's final pose: the active ragdoll reads it as the per-bone target
    // its constraint motors drive toward (the physics handoff). Cheap.
    runtime.last_pose.insert(rig.key, final_local);
}
