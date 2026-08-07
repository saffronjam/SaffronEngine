//! The per-session player runtime: the clip cache + injected loader, transition and
//! last-pose state, the playhead advance, the foot-IK producer, and the `tick_animation`
//! driver that samples + advances every rig and writes a `PoseOverride` onto each driven
//! bone.

mod node;
mod skinned;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use saffron_core::Uuid;
use saffron_geometry::AnimClip;
use saffron_scene::{
    AnimationPlayer, Entity, IdComponent, PoseOverride, Scene, SkinnedMesh, Transform, Transition,
    Wrap, quat_from_euler_xyz,
};

use crate::AnimMode;
use crate::algebra::{apply_delta, blend_joint, pose_diff, quintic_decay, smoothstep01};
use crate::error::Result;
use crate::pose::{JointPose, PoseDelta};

use node::{clear_node_overrides, tick_node_rig};
use skinned::{clear_overrides, tick_skinned_rig};

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
/// Clip bytes live in a `.smodel` SANM chunk whose reader lives in `saffron-assets`, which the DAG
/// forbids this crate from depending on, so the host hands the closure in at tick time borrowing
/// the live asset catalog. `FnMut` because the asset resolve mutates the server's load caches;
/// `tick_animation` runs on the main thread, so there is no `Send` bound.
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
/// current `PoseOverride`, so the same core serves both rig kinds.
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
