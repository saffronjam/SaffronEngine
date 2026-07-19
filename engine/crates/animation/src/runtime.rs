//! The per-session player runtime: the clip cache + injected loader, transition and
//! last-pose state, the playhead advance, the foot-IK producer, and the `tick_animation`
//! driver that samples + advances every rig and writes a `PoseOverride` onto each driven
//! bone.

use std::collections::HashMap;

use glam::{Quat, Vec3};
use saffron_core::Uuid;
use saffron_geometry::{AnimClip, AnimPath, AnimTarget};
use saffron_scene::{
    AnimationPlayer, Entity, FootIk, IdComponent, MorphComponent, MorphWeightOverride, Name,
    PoseOverride, Relationship, Scene, SkinnedMesh, Transform, Transition, Wrap,
    quat_from_euler_xyz,
};

use crate::AnimMode;
use crate::algebra::{apply_delta, blend_joint, pose_diff, quintic_decay, smoothstep01};
use crate::error::Result;
use crate::ik::solve_two_bone_ik;
use crate::pose::{JointPose, PoseBuffer, PoseDelta};
use crate::sample::{sample_track, sample_weights};

/// An in-flight clip switch, captured once at the switch frame and decayed over the
/// transition.
///
/// Keyed by the rig entity's [`IdComponent`] uuid in [`AnimationRuntime::transitions`].
/// `outgoing` is the frozen outgoing pose a cross-fade blends toward the incoming pose;
/// `offset` is `outgoing − incoming-at-switch`, the inertialization delta `quintic_decay`
/// runs out.
#[derive(Clone, Debug, Default, PartialEq)]
struct TransitionState {
    /// The frozen outgoing pose (cross-fade).
    outgoing: Vec<JointPose>,
    /// `outgoing − incoming-at-switch` (inertialization).
    offset: Vec<PoseDelta>,
}

/// Resolves a clip [`Uuid`] to its CPU [`AnimClip`], passed into [`tick_animation`] per call.
///
/// Clip bytes live in a `.smodel` SANM chunk whose reader is in `saffron-assets`, and the
/// animation crate must not depend on assets (the DAG forbids it), so the host hands this
/// closure in at tick time, borrowing the **live** asset catalog. It is the only injected
/// dependency, so a `&mut dyn FnMut`
/// borrowed for the call is the right shape — not a stored `'static` closure that would have to
/// own its own server. `FnMut` because the asset resolve mutates the server's load caches.
/// `tick_animation` runs on the main thread, so no `Send` bound.
pub type ClipLoader<'a> = &'a mut dyn FnMut(Uuid) -> Result<AnimClip>;

/// Per-session animation state: the negative clip cache and the transition / last-pose maps.
///
/// The host owns one and clears it on project (re)load so a reimported clip is picked up
/// fresh. The clip cache is a negative cache by construction: a broken asset is cached as
/// [`AnimClip::default`] so it is not re-read every frame.
#[derive(Default)]
pub struct AnimationRuntime {
    /// Loaded clips by uuid; a failed load is negative-cached as an empty clip.
    clip_cache: HashMap<u64, AnimClip>,
    /// Active clip switches by entity uuid.
    transitions: HashMap<u64, TransitionState>,
    /// Each rig's previous-frame final local pose by entity uuid; snapshotted at the end
    /// of every tick. The host's play tick reads it to motor each active ragdoll toward this
    /// frame's animated pose.
    last_pose: HashMap<u64, Vec<JointPose>>,
    /// Node-forest player bindings by player entity uuid: parallel to the player clip's
    /// distinct node-track targets, each the resolved forest [`Entity`] (`None` until
    /// resolved). Re-resolved by the scoped name walk on a stale handle.
    node_bindings: HashMap<u64, Vec<Option<Entity>>>,
}

impl AnimationRuntime {
    /// An empty runtime.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drops every cached clip and all transition / last-pose state.
    ///
    /// The host calls this on project (re)load so a reimported clip is re-resolved fresh
    /// and no stale per-entity state leaks across the reload.
    pub fn clear(&mut self) {
        self.clip_cache.clear();
        self.transitions.clear();
        self.last_pose.clear();
        self.node_bindings.clear();
    }

    /// Drops the per-entity transition and last-pose state, keeping the clip cache.
    ///
    /// The host calls this on an asset-preview enter/leave edge: the preview swaps the
    /// active scene to a fresh entity set, so a re-entered preview must start with no
    /// stale per-entity transition / pose entries, while the loaded clips (keyed by id,
    /// still valid) stay cached.
    pub fn prune_session(&mut self) {
        self.transitions.clear();
        self.last_pose.clear();
        self.node_bindings.clear();
    }

    /// The number of live per-entity session entries (transitions + last-pose keys),
    /// for tests that assert the prune edge cleared them.
    #[must_use]
    pub fn session_entry_count(&self) -> usize {
        self.transitions.len() + self.last_pose.len()
    }

    /// A rig's previous-frame final local pose, by entity uuid.
    ///
    /// The source the host's play tick motors each active ragdoll toward; snapshotted at the
    /// end of every tick for a driven rig.
    #[must_use]
    pub fn last_pose(&self, key: u64) -> Option<&[JointPose]> {
        self.last_pose.get(&key).map(Vec::as_slice)
    }

    /// Every driven rig's previous-frame final local pose, by entity uuid.
    ///
    /// The host snapshots this into the play tick's ragdoll `PoseTarget` list each frame,
    /// after [`tick_animation`](crate::tick_animation) and before the physics step, so an
    /// active ragdoll's motors read this frame's animated pose during the solve.
    pub fn last_poses(&self) -> impl Iterator<Item = (u64, &[JointPose])> {
        self.last_pose
            .iter()
            .map(|(key, pose)| (*key, pose.as_slice()))
    }

    /// Resolves (and caches) a clip uuid to its loaded [`AnimClip`] through `load`.
    ///
    /// `Uuid(0)` short-circuits to "no clip" before any lookup. A broken asset is
    /// negative-cached as an empty clip (with a one-time `tracing::warn!`) so it is not re-read
    /// every frame. Returns `None` only for the unset (`Uuid(0)`) clip.
    fn load_clip(&mut self, clip: Uuid, load: ClipLoader<'_>) -> Option<&AnimClip> {
        if clip.0 == 0 {
            return None;
        }
        let entry = self.clip_cache.entry(clip.0).or_insert_with(|| {
            load(clip).unwrap_or_else(|err| {
                tracing::warn!("clip {} failed to load: {err}", clip.0);
                AnimClip::default()
            })
        });
        Some(entry)
    }
}

/// Advance the playhead by `dt * speed` under the wrap mode.
///
/// `Once` clamps + stops at an end; `Loop` wraps; `PingPong` bounces and flips direction.
fn advance_time(player: &mut AnimationPlayer, duration: f32, dt: f32) {
    let delta = dt * player.speed;
    if player.wrap == Wrap::PingPong {
        if player.ping_forward {
            player.time += delta;
        } else {
            player.time -= delta;
        }
        if player.time >= duration {
            player.time = 2.0 * duration - player.time;
            player.ping_forward = false;
        }
        if player.time <= 0.0 {
            player.time = -player.time;
            player.ping_forward = true;
        }
        player.time = player.time.clamp(0.0, duration);
        return;
    }
    player.time += delta;
    if player.wrap == Wrap::Loop {
        player.time %= duration;
        if player.time < 0.0 {
            player.time += duration;
        }
        return;
    }
    if player.time >= duration {
        player.time = duration;
        player.playing = false;
    } else if player.time < 0.0 {
        player.time = 0.0;
        player.playing = false;
    }
}

/// A bone's authored rest local TRS, read from its [`Transform`] (Euler → quat matching
/// [`saffron_scene::transform_matrix`]). Identity-rest if the handle is stale.
fn rest_pose_of(scene: &Scene, skin: &SkinnedMesh, i: usize) -> JointPose {
    let Some(&bone) = skin.bone_handles.get(i) else {
        return JointPose::default();
    };
    if bone == Entity::NULL || !scene.valid(bone) {
        return JointPose::default();
    }
    scene
        .with_component::<Transform, _>(bone, |t| JointPose {
            translation: t.translation,
            rotation: quat_from_euler_xyz(t.rotation),
            scale: t.scale,
        })
        .unwrap_or_default()
}

/// Drop every pose override on a rig's bones so they revert to the rest pose.
fn clear_overrides(scene: &mut Scene, bone_handles: &[Entity]) {
    for &handle in bone_handles {
        if handle != Entity::NULL && scene.valid(handle) {
            scene.remove_component::<PoseOverride>(handle);
        }
    }
}

/// `sample_clip`, but each track is bound to its joint by index when sound and re-resolved
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

/// The bone's current pose — last frame's [`PoseOverride`] if present, else its rest pose.
///
/// The outgoing pose a just-started transition freezes at the switch frame.
fn outgoing_at(scene: &Scene, targets: &[Entity], rest: &[JointPose], i: usize) -> JointPose {
    if let Some(&handle) = targets.get(i)
        && handle != Entity::NULL
        && scene.valid(handle)
        && let Ok(over) = scene.component::<PoseOverride>(handle)
    {
        return JointPose {
            translation: over.translation,
            rotation: over.rotation,
            scale: over.scale,
        };
    }
    rest[i]
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

        // Resolve the chain from THIS frame's animated pose by forward kinematics, NOT the
        // cached world transform. v1 assumes a directly-parented chain (upper→mid→end,
        // unit bone scale), which the foot-chain config describes.
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

        // v1 ground = a horizontal plane at ground_height: plant the foot by lifting its
        // world Y up to the plane (never pull it below — a foot already above stays).
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

/// The kind of rig a player drives.
enum RigKind {
    /// A skinned rig: the bone handles drive a joint palette.
    Skinned {
        bone_handles: Vec<Entity>,
        joint_count: usize,
    },
    /// A node-forest rig at the container root: tracks bind to forest entities by name.
    Node,
}

/// The per-rig data gathered from a single `for_each` pass, then processed with full
/// scene access (the `for_each` query borrows the world exclusively, so the per-entity
/// work cannot run inside the closure).
struct Rig {
    entity: Entity,
    key: u64,
    clip_id: Uuid,
    kind: RigKind,
}

/// Sample and (when playing) advance every rig with both an [`AnimationPlayer`] and a
/// [`SkinnedMesh`], writing a [`PoseOverride`] onto each driven bone — and removing it
/// from an inactive rig's bones so they fall back to the authored rest pose.
///
/// In `Play` every rig animates; in `Edit` only a `preview_in_edit` rig does. A clip uuid
/// resolves through `load` (loaded once into the cache), which the host backs with the live
/// asset catalog. Never writes a bone's [`Transform`], so the rest pose and the project's
/// dirty state stay untouched. Infallible: a loader error is swallowed into the negative cache.
pub fn tick_animation(
    runtime: &mut AnimationRuntime,
    scene: &mut Scene,
    dt: f32,
    mode: AnimMode,
    load: ClipLoader<'_>,
) {
    let mut rigs: Vec<Rig> = Vec::new();
    scene.for_each::<(&AnimationPlayer, Option<&SkinnedMesh>, Option<&IdComponent>), _>(
        |entity, (player, skin, id)| {
            let kind = match skin {
                Some(skin) => RigKind::Skinned {
                    bone_handles: skin.bone_handles.clone(),
                    joint_count: skin.bones.len(),
                },
                None => RigKind::Node,
            };
            rigs.push(Rig {
                entity,
                key: id.map_or(0, |id| id.id.0),
                clip_id: player.clip,
                kind,
            });
        },
    );

    for rig in rigs {
        tick_rig(runtime, scene, dt, mode, &rig, &mut *load);
    }
}

/// Process one rig: resolve its clip, sample + advance, apply transitions and foot-IK, and
/// write the per-bone overrides (or clear them when the rig is inactive / has no clip).
fn tick_rig(
    runtime: &mut AnimationRuntime,
    scene: &mut Scene,
    dt: f32,
    mode: AnimMode,
    rig: &Rig,
    load: ClipLoader<'_>,
) {
    // Play animates every rig; Edit previews only the timeline-selected one.
    let preview = scene
        .with_component::<AnimationPlayer, _>(rig.entity, |p| p.preview_in_edit)
        .unwrap_or(false);
    let active = mode == AnimMode::Play || preview;

    // Resolve the clip through the cache/loader. A clone keeps the borrow of `runtime`
    // from colliding with the `&mut scene` writes below; clips are read by value into a
    // per-frame pose anyway.
    let clip = if active {
        runtime.load_clip(rig.clip_id, load).cloned()
    } else {
        None
    };

    let Some(clip) = clip else {
        // No clip (inactive rig, unset uuid, or no loader): clear overrides and drop the
        // per-entity transition / last-pose state. A negative-cached (empty) clip is NOT
        // this case — it resolves to a valid empty clip and drives the rig to rest.
        match &rig.kind {
            RigKind::Skinned { bone_handles, .. } => clear_overrides(scene, bone_handles),
            RigKind::Node => clear_node_overrides(runtime, scene, rig.key),
        }
        runtime.transitions.remove(&rig.key);
        runtime.last_pose.remove(&rig.key);
        return;
    };

    match &rig.kind {
        RigKind::Skinned { joint_count, .. } => {
            tick_skinned_rig(runtime, scene, dt, rig, &clip, *joint_count);
        }
        RigKind::Node => tick_node_rig(runtime, scene, dt, rig, &clip),
    }
}

/// Advance the player's playhead (when playing), opening a Loop-wrap blend across the seam.
/// Returns the post-advance clip time. Shared by both rig kinds.
fn advance_playback(scene: &mut Scene, entity: Entity, duration: f32, dt: f32) -> f32 {
    let (prev_time, playing) = scene
        .with_component::<AnimationPlayer, _>(entity, |p| (p.time, p.playing))
        .unwrap_or((0.0, false));
    if playing && duration > 0.0 {
        let _ = scene.with_component_mut::<AnimationPlayer, _>(entity, |p| {
            advance_time(p, duration, dt);
        });
    }
    let (time, wrap, loop_blend, transition, transition_duration) = scene
        .with_component::<AnimationPlayer, _>(entity, |p| {
            (
                p.time,
                p.wrap,
                p.loop_blend,
                p.transition,
                p.transition_duration,
            )
        })
        .unwrap_or((0.0, Wrap::Loop, 0.0, 0.0, 0.0));
    let wrapped = wrap == Wrap::Loop && time < prev_time;
    if wrapped && loop_blend > 0.0 && transition >= transition_duration {
        // A Loop wrap is a transition from the end pose to the start pose.
        let _ = scene.with_component_mut::<AnimationPlayer, _>(entity, |p| {
            p.prev_clip = p.clip;
            p.transition = 0.0;
            p.transition_duration = p.loop_blend;
        });
    }
    time
}

/// Apply the in-flight transition (cross-fade or inertialize) to `final_local` over the
/// driven `targets`, advancing the player's transition clock. Generalized off bone handles:
/// `targets[i]` is the i-th driven entity (a bone for a skinned rig, a node entity for a
/// node-forest rig), `rest[i]` its rest pose. The frozen outgoing pose reads each target's
/// current `PoseOverride`, so the same core serves both rig kinds (decision #12).
#[allow(clippy::too_many_arguments)]
fn apply_transition(
    runtime: &mut AnimationRuntime,
    scene: &mut Scene,
    entity: Entity,
    key: u64,
    targets: &[Entity],
    rest: &[JointPose],
    final_local: &mut [JointPose],
    dt: f32,
) {
    let (transition, transition_duration, transition_mode) = scene
        .with_component::<AnimationPlayer, _>(entity, |p| {
            (p.transition, p.transition_duration, p.transition_mode)
        })
        .unwrap_or((0.0, 0.0, Transition::Inertialize));

    let transitioning = transition_duration > 0.0 && transition < transition_duration;
    if !transitioning {
        runtime.transitions.remove(&key);
        return;
    }

    let count = final_local.len();
    // Freeze the outgoing pose + capture the offset once, at the switch frame.
    if transition <= 0.0 || !runtime.transitions.contains_key(&key) {
        let mut state = TransitionState {
            outgoing: vec![JointPose::default(); count],
            offset: vec![PoseDelta::default(); count],
        };
        for (i, incoming) in final_local.iter().enumerate() {
            state.outgoing[i] = outgoing_at(scene, targets, rest, i);
            state.offset[i] = pose_diff(&state.outgoing[i], incoming);
        }
        runtime.transitions.insert(key, state);
    }
    let state = &runtime.transitions[&key];
    let x = (transition / transition_duration).clamp(0.0, 1.0);
    let n = count.min(state.offset.len());
    for (joint, (outgoing, offset)) in final_local
        .iter_mut()
        .zip(state.outgoing.iter().zip(state.offset.iter()))
        .take(n)
    {
        *joint = if transition_mode == Transition::CrossFade {
            blend_joint(outgoing, joint, smoothstep01(x))
        } else {
            apply_delta(joint, offset, quintic_decay(x))
        };
    }
    let done = scene
        .with_component_mut::<AnimationPlayer, _>(entity, |p| {
            p.transition += dt;
            p.transition >= p.transition_duration
        })
        .unwrap_or(false);
    if done {
        runtime.transitions.remove(&key);
        let _ = scene.with_component_mut::<AnimationPlayer, _>(entity, |p| {
            p.prev_clip = Uuid(0);
            p.transition = 0.0;
            p.transition_duration = 0.0;
        });
    }
}

/// Process a skinned rig: seed rest from the bones, sample bone-TRS tracks, apply the
/// transition + foot-IK, and write a `PoseOverride` per bone.
fn tick_skinned_rig(
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
fn resolve_node_targets(
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
fn clear_node_overrides(runtime: &mut AnimationRuntime, scene: &mut Scene, key: u64) {
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
fn tick_node_rig(
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

#[cfg(test)]
mod tests {
    use saffron_geometry::{AnimInterp, AnimTrack};
    use saffron_scene::{Bone, FootChain};
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

    #[test]
    fn tick_writes_pose_override_on_driven_bone() {
        // The acceptance smoke: one rig + a clip loader, ticked once in Play, leaves a
        // PoseOverride on the driven bone holding the sampled value.
        let clip_id = Uuid(7);
        let (mut scene, _rig, bones) = rig_scene(2, clip_id);
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(translate_clip(0, 10.0));

        // Half a second into the 1s clip: the joint-0 translation lerps to 5 on +X.
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);

        let over = scene
            .component::<PoseOverride>(bones[0])
            .expect("a driven bone gets a PoseOverride");
        assert!(over.translation.distance(Vec3::new(5.0, 0.0, 0.0)) < 1e-4);
        // The untracked second bone keeps its authored rest translation.
        let over1 = scene
            .component::<PoseOverride>(bones[1])
            .expect("every bone gets a PoseOverride seeded from rest");
        assert!(over1.translation.distance(Vec3::new(1.0, 0.0, 0.0)) < 1e-4);
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

    #[test]
    fn node_player_writes_pose_override_on_bound_node() {
        // A skinless node-forest player ticks in Play and writes a PoseOverride onto the
        // node bound by name; the world transform reflects it.
        let clip_id = Uuid(20);
        let (mut scene, _c, nodes) = node_forest_scene(clip_id);
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(node_translate_clip("NodeA", 4.0));

        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);

        let over = scene
            .component::<PoseOverride>(nodes[0])
            .expect("the bound node gets a PoseOverride");
        assert!(over.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
        // The unbound nested node is untouched.
        assert!(!scene.has_component::<PoseOverride>(nodes[1]));
        scene.update_world_transforms();
        assert!(
            scene
                .world_translation(nodes[0])
                .distance(Vec3::new(2.0, 0.0, 0.0))
                < 1e-4
        );
    }

    #[test]
    fn node_rig_ignores_bone_tracks_no_cross_routing() {
        // The joint-index-coupling hazard: a Bone track on a node-forest player must never
        // be interpreted as a node index. The node track drives its node; the bone track is
        // skipped (no panic, no stray override).
        let clip_id = Uuid(22);
        let (mut scene, _c, nodes) = node_forest_scene(clip_id);
        let mut clip = node_translate_clip("NodeA", 4.0);
        clip.tracks.push(AnimTrack {
            target: AnimTarget::Bone,
            index: 0,
            target_name: "NodeB".to_string(),
            path: AnimPath::Translation,
            interp: AnimInterp::Linear,
            morph_count: 0,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, 9.0, 0.0, 0.0],
        });
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
        assert!(scene.has_component::<PoseOverride>(nodes[0]));
    }

    #[test]
    fn weights_track_writes_override_and_clears_on_stop() {
        let clip_id = Uuid(21);
        let (mut scene, _c, nodes) = node_forest_scene(clip_id);
        scene
            .add_component(
                nodes[0],
                MorphComponent {
                    weights: vec![0.0, 0.0],
                    names: vec!["a".to_string(), "b".to_string()],
                },
            )
            .unwrap();
        let clip = AnimClip {
            name: "w".to_string(),
            duration: 1.0,
            tracks: vec![AnimTrack {
                target: AnimTarget::Node,
                index: -1,
                target_name: "NodeA".to_string(),
                path: AnimPath::Weights,
                interp: AnimInterp::Linear,
                morph_count: 2,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 1.0, 0.5],
            }],
        };
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);

        // Half a second into the 1s clip: lane 0 lerps 0→1 to 0.5, lane 1 lerps 0→0.5 to 0.25.
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
        let (w0, w1) = scene
            .with_component::<MorphWeightOverride, _>(nodes[0], |m| (m.weights[0], m.weights[1]))
            .expect("a weights track writes a MorphWeightOverride");
        assert!((w0 - 0.5).abs() < 1e-4 && (w1 - 0.25).abs() < 1e-4);

        // Inactive (Edit, no preview) clears the runtime-only override + the binding, so the
        // mesh reverts to the durable MorphComponent weights.
        tick_animation(&mut runtime, &mut scene, 0.0, AnimMode::Edit, &mut load);
        assert!(!scene.has_component::<MorphWeightOverride>(nodes[0]));
        assert_eq!(runtime.node_bindings.len(), 0);
    }

    #[test]
    fn node_binding_is_scoped_per_instance() {
        // Two instances of the same-named forest: each player binds its OWN NodeA, never the
        // other instance's (the global-scan cross-instance hazard).
        let clip_id = Uuid(23);
        let (mut scene, _c0, nodes0) = node_forest_scene(clip_id);
        // A second forest in the same scene.
        let container1 = scene.create_entity("Forest");
        let node_a1 = scene.create_entity("NodeA");
        scene.set_parent(node_a1, Some(container1), false).unwrap();
        scene
            .add_component(
                container1,
                AnimationPlayer {
                    clip: clip_id,
                    playing: true,
                    wrap: Wrap::Loop,
                    ..AnimationPlayer::default()
                },
            )
            .unwrap();
        scene.relink_hierarchy();

        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(node_translate_clip("NodeA", 4.0));
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);

        // Each instance's own NodeA is driven; neither leaks into the other.
        assert!(scene.has_component::<PoseOverride>(nodes0[0]));
        assert!(scene.has_component::<PoseOverride>(node_a1));
        let a0 = scene.component::<PoseOverride>(nodes0[0]).unwrap();
        let a1 = scene.component::<PoseOverride>(node_a1).unwrap();
        assert!(a0.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
        assert!(a1.translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
    }

    #[test]
    fn no_clip_clears_overrides_and_drops_state() {
        // Seed an override + transition/last-pose state on a rig with an unset clip
        // (`Uuid(0)` ⇒ no clip resolves): the overrides are removed and the maps cleared.
        let (mut scene, rig, bones) = rig_scene(1, Uuid(0));
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(AnimClip::default()); // never consulted for the unset clip
        scene
            .add_component(
                bones[0],
                PoseOverride {
                    translation: Vec3::splat(9.0),
                    ..PoseOverride::default()
                },
            )
            .unwrap();
        let key = scene.component::<IdComponent>(rig).unwrap().id.0;
        runtime.transitions.insert(key, TransitionState::default());
        runtime.last_pose.insert(key, vec![JointPose::default()]);

        tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Play, &mut load);

        assert!(!scene.has_component::<PoseOverride>(bones[0]));
        assert!(!runtime.transitions.contains_key(&key));
        assert!(runtime.last_pose(key).is_none());
    }

    #[test]
    fn edit_without_preview_is_inert() {
        // Edit with no preview rig: nothing animates, so no override appears.
        let (mut scene, _rig, root_bone, clip) = spin_preview_scene();
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load);
        assert!(
            !scene.has_component::<PoseOverride>(root_bone),
            "edit without preview is inert"
        );
    }

    #[test]
    fn preview_writes_override_and_advances() {
        // Edit + preview + playing: the playhead reaches 0.5 and the override holds the 45°
        // Y rotation, while the rest-pose Transform stays at identity and world composition
        // prefers the override.
        let (mut scene, rig, root_bone, clip) = spin_preview_scene();
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);
        scene
            .with_component_mut::<AnimationPlayer, _>(rig, |p| {
                p.preview_in_edit = true;
                p.playing = true;
                p.time = 0.0;
            })
            .unwrap();

        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load);

        let time = scene
            .with_component::<AnimationPlayer, _>(rig, |p| p.time)
            .unwrap();
        assert!(
            (time - 0.5).abs() < EPS,
            "edit preview advances the playhead"
        );

        let q45 = Quat::from_axis_angle(Vec3::Y, 45.0_f32.to_radians());
        let over = scene
            .component::<PoseOverride>(root_bone)
            .expect("preview writes a pose override");
        assert!(
            quat_close(over.rotation, q45),
            "override holds the sampled 45° rotation"
        );

        // The rest-pose Transform stays at identity (non-destructive Edit preview).
        let rest_rotation = scene
            .with_component::<Transform, _>(root_bone, |t| t.rotation)
            .unwrap();
        assert!(
            rest_rotation.length() < EPS,
            "rest-pose Transform stays at identity"
        );

        // World composition prefers the override.
        scene.update_world_transforms();
        assert!(
            quat_close(scene.world_rotation(root_bone), q45),
            "world transform reflects the override"
        );
    }

    #[test]
    fn clearing_preview_reverts_to_rest() {
        // After previewing the override, clearing preview removes it and the bone reverts to
        // rest on the next tick.
        let (mut scene, rig, root_bone, clip) = spin_preview_scene();
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);
        scene
            .with_component_mut::<AnimationPlayer, _>(rig, |p| {
                p.preview_in_edit = true;
                p.playing = true;
                p.time = 0.0;
            })
            .unwrap();
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load);
        assert!(scene.has_component::<PoseOverride>(root_bone));

        scene
            .with_component_mut::<AnimationPlayer, _>(rig, |p| p.preview_in_edit = false)
            .unwrap();
        tick_animation(&mut runtime, &mut scene, 0.0, AnimMode::Edit, &mut load);
        assert!(
            !scene.has_component::<PoseOverride>(root_bone),
            "clearing preview removes the override"
        );
        scene.update_world_transforms();
        assert!(
            quat_close(scene.world_rotation(root_bone), Quat::IDENTITY),
            "bone reverts to rest after preview clears"
        );
    }

    #[test]
    fn play_animates_without_preview() {
        // Play animates every rig regardless of preview_in_edit.
        let (mut scene, rig, root_bone, clip) = spin_preview_scene();
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);
        scene
            .with_component_mut::<AnimationPlayer, _>(rig, |p| {
                p.preview_in_edit = false;
                p.playing = true;
                p.time = 0.0;
            })
            .unwrap();
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
        assert!(
            scene.has_component::<PoseOverride>(root_bone),
            "play animates without preview"
        );
    }

    #[test]
    fn last_pose_snapshot_records_the_driven_pose() {
        let clip_id = Uuid(5);
        let (mut scene, rig, _bones) = rig_scene(2, clip_id);
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(translate_clip(0, 8.0));
        let key = scene.component::<IdComponent>(rig).unwrap().id.0;

        tick_animation(&mut runtime, &mut scene, 0.25, AnimMode::Play, &mut load);
        let snapshot = runtime.last_pose(key).expect("last_pose snapshot exists");
        assert_eq!(snapshot.len(), 2);
        // Joint 0 lerps to 0.25 * 8 = 2 on +X at t = 0.25s.
        assert!(snapshot[0].translation.distance(Vec3::new(2.0, 0.0, 0.0)) < 1e-4);
    }

    #[test]
    fn negative_cache_loads_a_broken_clip_once() {
        // A loader that fails is negative-cached: it is called exactly once, then the rig
        // drives to rest (an empty clip), and no override is dropped on later ticks.
        use std::cell::Cell;
        use std::rc::Rc;

        let calls = Rc::new(Cell::new(0u32));
        let calls_inner = Rc::clone(&calls);
        let (mut scene, _rig, bones) = rig_scene(1, Uuid(99));
        let mut runtime = AnimationRuntime::new();
        let mut load = move |_id| {
            calls_inner.set(calls_inner.get() + 1);
            Err(Error::ClipLoad("boom".to_string()))
        };

        tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Play, &mut load);
        tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Play, &mut load);
        assert_eq!(
            calls.get(),
            1,
            "a failed load is negative-cached, not retried"
        );
        // An empty clip resolves: the bone is driven to its rest pose, not cleared.
        let over = scene
            .component::<PoseOverride>(bones[0])
            .expect("an empty (negative-cached) clip still drives the rig to rest");
        assert!(over.translation.distance(Vec3::new(1.0, 0.0, 0.0)) < 1e-4);
    }

    #[test]
    fn advance_time_once_clamps_and_stops() {
        let mut p = AnimationPlayer {
            time: 0.9,
            speed: 1.0,
            playing: true,
            wrap: Wrap::Once,
            ..AnimationPlayer::default()
        };
        advance_time(&mut p, 1.0, 0.5);
        assert_eq!(p.time, 1.0);
        assert!(!p.playing, "Once stops at the clip end");
    }

    #[test]
    fn advance_time_loop_wraps() {
        let mut p = AnimationPlayer {
            time: 0.8,
            speed: 1.0,
            playing: true,
            wrap: Wrap::Loop,
            ..AnimationPlayer::default()
        };
        advance_time(&mut p, 1.0, 0.5);
        // 0.8 + 0.5 = 1.3, wrapped into [0, 1) = 0.3.
        assert!((p.time - 0.3).abs() < 1e-6);
        assert!(p.playing, "Loop keeps playing across the seam");
    }

    #[test]
    fn advance_time_pingpong_bounces() {
        let mut p = AnimationPlayer {
            time: 0.8,
            speed: 1.0,
            playing: true,
            wrap: Wrap::PingPong,
            ping_forward: true,
            ..AnimationPlayer::default()
        };
        advance_time(&mut p, 1.0, 0.5);
        // 0.8 + 0.5 = 1.3 → reflected to 2 - 1.3 = 0.7, direction flips to backward.
        assert!((p.time - 0.7).abs() < 1e-6);
        assert!(!p.ping_forward, "PingPong flips direction at the end");
    }

    /// Runs the transition oracle for one mode: a single-bone rig switched in from the
    /// rest (identity) outgoing pose to a clip holding 90° about Y, over a 1s transition.
    /// Ticks the switch frame (`x=0`), then runs the transition out, returning the switch
    /// rotation and the steady-state incoming rotation.
    fn run_transition(mode: Transition) -> (Quat, Quat) {
        let s = 0.5_f32.sqrt();
        let clip_id = Uuid(9001);
        let mut scene = Scene::new();
        let bone = scene.create_entity("J0");
        scene.add_component(bone, Bone::default()).unwrap();
        let rig = scene.create_entity("Rig");
        let bone_id = scene.component::<IdComponent>(bone).unwrap().id;
        scene
            .add_component(
                rig,
                SkinnedMesh {
                    bones: vec![bone_id],
                    ..SkinnedMesh::default()
                },
            )
            .unwrap();
        scene
            .add_component(
                rig,
                AnimationPlayer {
                    clip: clip_id,
                    preview_in_edit: true,
                    playing: true,
                    transition_mode: mode,
                    // A distinct outgoing clip id; its pose comes from the bone's rest.
                    prev_clip: Uuid(1),
                    transition: 0.0,
                    transition_duration: 1.0,
                    ..AnimationPlayer::default()
                },
            )
            .unwrap();
        scene.relink_hierarchy();

        // A clip holding 90° about Y at every key (xyzw constant).
        let clip = AnimClip {
            name: "spin90".to_string(),
            duration: 1.0,
            tracks: vec![AnimTrack {
                index: 0,
                target_name: "J0".to_string(),
                path: AnimPath::Rotation,
                interp: AnimInterp::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, s, 0.0, s, 0.0, s, 0.0, s],
                ..Default::default()
            }],
        };
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);

        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load); // switch frame, x=0
        let switch = scene.component::<PoseOverride>(bone).unwrap().rotation;
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load); // x=0.5 -> 1, ends
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Edit, &mut load); // steady incoming
        let end = scene.component::<PoseOverride>(bone).unwrap().rotation;
        (switch, end)
    }

    #[test]
    fn crossfade_starts_outgoing_ends_incoming() {
        // The cross-fade begins at the outgoing (rest identity) pose at the switch frame and
        // settles on the incoming 90° clip once the transition runs out.
        let q90 = Quat::from_axis_angle(Vec3::Y, 90.0_f32.to_radians());
        let (switch, end) = run_transition(Transition::CrossFade);
        assert!(
            quat_close(switch, Quat::IDENTITY),
            "crossfade starts at the outgoing pose"
        );
        assert!(quat_close(end, q90), "crossfade ends at the incoming clip");
    }

    #[test]
    fn inertialize_c0_at_switch() {
        // Inertialization is C0 at the switch (no pop): it starts at the outgoing pose and
        // decays the offset to the incoming 90° clip.
        let q90 = Quat::from_axis_angle(Vec3::Y, 90.0_f32.to_radians());
        let (switch, end) = run_transition(Transition::Inertialize);
        assert!(
            quat_close(switch, Quat::IDENTITY),
            "inertialization is C0 at the switch (no pop)"
        );
        assert!(
            quat_close(end, q90),
            "inertialization ends at the incoming clip"
        );
    }

    #[test]
    fn loop_wrap_holds_pre_wrap_pose() {
        // A clip ramping 0°→90° about Y would pop at the loop seam; loop_blend > 0
        // inertializes across it, so the wrap frame holds the pre-wrap (end) pose rather
        // than snapping to the start. A hard cut would jump ~72°, which quat_close rejects.
        let s = 0.5_f32.sqrt();
        let clip_id = Uuid(9002);
        let mut scene = Scene::new();
        let bone = scene.create_entity("J0");
        scene.add_component(bone, Bone::default()).unwrap();
        let rig = scene.create_entity("Rig");
        let bone_id = scene.component::<IdComponent>(bone).unwrap().id;
        scene
            .add_component(
                rig,
                SkinnedMesh {
                    bones: vec![bone_id],
                    ..SkinnedMesh::default()
                },
            )
            .unwrap();
        scene
            .add_component(
                rig,
                AnimationPlayer {
                    clip: clip_id,
                    preview_in_edit: true,
                    playing: true,
                    wrap: Wrap::Loop,
                    loop_blend: 0.5,
                    time: 0.8,
                    ..AnimationPlayer::default()
                },
            )
            .unwrap();
        scene.relink_hierarchy();

        let clip = AnimClip {
            name: "ramp".to_string(),
            duration: 1.0,
            tracks: vec![AnimTrack {
                index: 0,
                target_name: "J0".to_string(),
                path: AnimPath::Rotation,
                interp: AnimInterp::Linear,
                times: vec![0.0, 1.0],
                // identity -> 90° about Y.
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, s, 0.0, s],
                ..Default::default()
            }],
        };
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(clip);

        tick_animation(&mut runtime, &mut scene, 0.1, AnimMode::Edit, &mut load); // time -> 0.9
        let pre_wrap = scene.component::<PoseOverride>(bone).unwrap().rotation;
        tick_animation(&mut runtime, &mut scene, 0.2, AnimMode::Edit, &mut load); // wraps past the end
        let wrap_frame = scene.component::<PoseOverride>(bone).unwrap().rotation;
        assert!(
            quat_close(wrap_frame, pre_wrap),
            "loop wrap holds the pre-wrap pose (no pop)"
        );
    }

    #[test]
    fn skinning_seam_palette_reflects_animation() {
        // The cross-area contract animation → rendering: a ticked rig writes a PoseOverride
        // that flows through update_world_transforms + joint_matrices into the joint palette
        // the renderer consumes — so the palette must reflect the animated pose, not the
        // rest pose. No GPU here: the prepass that blends this palette lives in 06-rendering.
        let clip_id = Uuid(4242);
        let (mut scene, rig, _bones) = rig_scene(2, clip_id);
        scene
            .with_component_mut::<AnimationPlayer, _>(rig, |p| p.time = 0.0)
            .unwrap();

        // Capture the rest-pose palette (no override yet) for the baseline comparison.
        let skin = scene
            .with_component::<SkinnedMesh, _>(rig, Clone::clone)
            .unwrap();
        scene.update_world_transforms();
        let rest_palette = scene.joint_matrices(&skin);

        // Tick the rig with a clip that moves joint 0 far down +X, then recompose world
        // matrices and rebuild the palette.
        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(translate_clip(0, 10.0));
        tick_animation(&mut runtime, &mut scene, 0.5, AnimMode::Play, &mut load);
        scene.update_world_transforms();
        let animated_palette = scene.joint_matrices(&skin);

        // The animated palette must differ from the rest palette by more than 1e-3,
        // confirming the override flowed into world composition and the palette.
        let drift = (animated_palette[0] - rest_palette[0])
            .to_cols_array()
            .iter()
            .fold(0.0_f32, |acc, c| acc.max(c.abs()));
        assert!(
            drift > 1e-3,
            "the joint palette must reflect the animated pose, got drift {drift}"
        );
    }

    #[test]
    fn foot_ik_plants_the_foot_on_the_ground_plane() {
        // A three-bone chain hanging below a parent; with foot IK enabled and the ground at
        // y = 0, the solved foot end lands on (or above) the plane.
        let clip_id = Uuid(31);
        let mut scene = Scene::new();
        let rig = scene.create_entity("Rig");
        // A root above the chain so the foot starts below ground.
        let hip = scene.create_entity("hip");
        scene
            .with_component_mut::<Transform, _>(hip, |t| t.translation = Vec3::new(0.0, 1.0, 0.0))
            .unwrap();
        let upper = scene.create_entity("upper");
        scene
            .with_component_mut::<Transform, _>(upper, |t| {
                t.translation = Vec3::new(0.0, -0.5, 0.0);
            })
            .unwrap();
        scene.set_parent(upper, Some(hip), false).unwrap();
        let mid = scene.create_entity("mid");
        scene
            .with_component_mut::<Transform, _>(mid, |t| t.translation = Vec3::new(0.0, -0.5, 0.0))
            .unwrap();
        scene.set_parent(mid, Some(upper), false).unwrap();
        let end = scene.create_entity("end");
        scene
            .with_component_mut::<Transform, _>(end, |t| t.translation = Vec3::new(0.0, -0.5, 0.0))
            .unwrap();
        scene.set_parent(end, Some(mid), false).unwrap();

        let bone_ids: Vec<Uuid> = [upper, mid, end]
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
                    clip: clip_id,
                    playing: false,
                    ..AnimationPlayer::default()
                },
            )
            .unwrap();
        scene
            .add_component(
                rig,
                FootIk {
                    enabled: true,
                    ground_height: 0.0,
                    chains: vec![FootChain {
                        upper: 0,
                        mid: 1,
                        end: 2,
                        pole_vector: Vec3::new(0.0, 0.0, 1.0),
                    }],
                },
            )
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();

        let mut runtime = AnimationRuntime::new();
        // An empty clip: the rig drives to rest, then foot IK runs on the rest pose.
        let mut load = clip_loader(AnimClip {
            name: "rest".to_string(),
            duration: 1.0,
            tracks: Vec::new(),
        });

        tick_animation(&mut runtime, &mut scene, 0.016, AnimMode::Play, &mut load);

        // After the solve, the chain's overrides changed the joint rotations so the foot
        // reaches up toward the ground plane. Recompose the foot world Y by FK from the
        // written overrides and assert it is at/above the ground (it started at y = -0.5).
        scene.update_world_transforms();
        let foot_y = scene.world_translation(end).y;
        assert!(
            foot_y > -0.5 + 1e-3,
            "foot IK should lift the foot toward the ground plane (foot_y = {foot_y})"
        );
        assert!(foot_y.is_finite(), "the solve must not produce NaN");
    }

    #[test]
    fn foot_ik_reaches_a_nearby_target_from_a_bent_pose() {
        let clip_id = Uuid(32);
        let (mut scene, rig, bones) = rig_scene(3, clip_id);
        scene
            .with_component_mut::<Transform, _>(bones[0], |transform| {
                transform.translation = Vec3::ZERO;
            })
            .unwrap();
        scene
            .with_component_mut::<Transform, _>(bones[1], |transform| {
                transform.translation = Vec3::Y;
                transform.rotation.z = 67.5_f32.to_radians();
            })
            .unwrap();
        scene
            .with_component_mut::<Transform, _>(bones[2], |transform| {
                transform.translation = Vec3::Y;
            })
            .unwrap();
        scene.relink_hierarchy();
        scene.update_world_transforms();
        let target_y = scene.world_translation(bones[2]).y + 0.08;
        scene
            .add_component(
                rig,
                FootIk {
                    enabled: true,
                    ground_height: target_y,
                    chains: vec![FootChain {
                        upper: 0,
                        mid: 1,
                        end: 2,
                        pole_vector: -Vec3::X,
                    }],
                },
            )
            .unwrap();

        let mut runtime = AnimationRuntime::new();
        let mut load = clip_loader(AnimClip {
            name: "rest".to_string(),
            duration: 1.0,
            tracks: Vec::new(),
        });
        tick_animation(&mut runtime, &mut scene, 0.016, AnimMode::Play, &mut load);
        scene.update_world_transforms();
        let foot_y = scene.world_translation(bones[2]).y;

        assert!(
            (foot_y - target_y).abs() < 1.0e-3,
            "foot y {foot_y} should reach target {target_y}"
        );
    }
}
