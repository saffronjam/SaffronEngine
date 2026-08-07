//! The passive/active/partial ragdoll blend layer.

use std::collections::HashMap;

use glam::{Mat4, Quat, Vec3};

use saffron_core::Uuid;
use saffron_physics_sys::{self as sys, BonePart, INVALID_BODY_ID};
use saffron_scene::{
    BonePhysicsComponent, Entity, IdComponent, PoseOverride, Relationship, Scene, SkinnedMesh,
};

use crate::error::{Error, Result};
use crate::types::{PoseTarget, RagdollState};

use super::*;

/// The weight units/sec the eased per-bone physics weight approaches its target, so the
/// animation↔physics blend ramps without a pop.
const RAGDOLL_WEIGHT_RATE: f32 = 6.0;

/// At/above this eased weight the physics pose overwrites the bone's [`PoseOverride`] outright;
/// below it the physics pose blends over the animation pose by the weight.
const PURE_PHYSICS_WEIGHT: f32 = 0.999;

impl World {
    /// Build a **passive** SwingTwist ragdoll on the rig `entity`: parts mirror its
    /// [`SkinnedMesh::bones`] 1:1, sized from the [`BonePhysicsComponent`], seeded at each bone's
    /// current world pose, with the constraint kind each bone's [`Joint`](saffron_scene::Joint)
    /// selects. Motors are attached but `Off` ([`World::set_ragdoll_blend`] drives them).
    /// Idempotent: a re-enable on a rig that already has a ragdoll rebuilds it.
    ///
    /// # Errors
    ///
    /// [`Error::RagdollMissingComponents`] if the rig lacks a `SkinnedMesh` + `BonePhysics` pair,
    /// [`Error::RagdollMismatch`] if the `BonePhysics` array length does not match the bone count,
    /// or [`Error::RagdollCreate`] if `CreateRagdoll` failed.
    pub fn enable_ragdoll(&mut self, scene: &Scene, entity: Entity) -> Result<()> {
        if !scene.has_component::<SkinnedMesh>(entity)
            || !scene.has_component::<BonePhysicsComponent>(entity)
        {
            return Err(Error::RagdollMissingComponents);
        }
        let bone_handles = scene
            .with_component::<SkinnedMesh, _>(entity, |s| s.bone_handles.clone())
            .unwrap_or_default();
        let bones = scene
            .with_component::<BonePhysicsComponent, _>(entity, |p| p.bones.clone())
            .unwrap_or_default();
        let count = bone_handles.len();
        if count == 0 || bones.len() != count {
            return Err(Error::RagdollMismatch {
                expected: count,
                got: bones.len(),
            });
        }

        let rig_uuid = scene
            .component::<IdComponent>(entity)
            .map(|c| c.id)
            .unwrap_or(Uuid(0));
        // Idempotent re-enable: tear down any existing ragdoll for this rig first.
        self.disable_ragdoll(rig_uuid);

        // uuid → bone index, then per-bone parent index + current world pose.
        let mut bone_index_by_uuid: HashMap<Uuid, i32> = HashMap::new();
        for (i, &bone) in bone_handles.iter().enumerate() {
            if scene.valid(bone)
                && let Ok(id) = scene.component::<IdComponent>(bone)
            {
                bone_index_by_uuid.insert(id.id, i32::try_from(i).unwrap_or(-1));
            }
        }
        let mut parent_index = vec![-1i32; count];
        let mut world_pos = vec![Vec3::ZERO; count];
        let mut world_rot = vec![Quat::IDENTITY; count];
        for (i, &bone) in bone_handles.iter().enumerate() {
            if !scene.valid(bone) {
                continue;
            }
            let (translation, rotation) = fresh_world_pose(scene, bone);
            world_pos[i] = translation;
            world_rot[i] = rotation;
            if let Ok(rel) = scene.with_component::<Relationship, _>(bone, |r| r.parent)
                && let Some(&parent) = bone_index_by_uuid.get(&rel)
            {
                parent_index[i] = parent;
            }
        }

        let parts: Vec<BonePart> = (0..count)
            .map(|i| {
                let bone = &bones[i];
                BonePart {
                    parent_index: parent_index[i],
                    position: world_pos[i].to_array(),
                    rotation: world_rot[i].to_array(),
                    radius: bone.shape_half_extents.x,
                    half_height: bone.shape_half_extents.y,
                    mass: bone.mass,
                    joint: joint_raw(bone.joint),
                    swing_twist_limits: bone.swing_twist_limits.to_array(),
                    drive_stiffness: bone.drive_stiffness,
                    drive_damping: bone.drive_damping,
                    drive_max_force: bone.drive_max_force,
                }
            })
            .collect();

        let index = sys::add_ragdoll(&mut self.world, rig_uuid.0, &parts);
        if index == INVALID_BODY_ID {
            return Err(Error::RagdollCreate);
        }
        self.ragdolls.push(RagdollEntry {
            rig: rig_uuid,
            rig_entity: entity,
            index,
            parent_index,
            weight_target: vec![1.0; count], // pure ragdoll: physics wins outright
            weight_current: vec![1.0; count],
            weight_rate: RAGDOLL_WEIGHT_RATE,
            motors_active: false,
        });
        Ok(())
    }

    /// Remove the live ragdoll for `rig` (detach from the physics system, drop the handles),
    /// rebasing the shim-slot indices of the ragdolls that outlived it. A rig with no ragdoll is a
    /// no-op.
    pub fn disable_ragdoll(&mut self, rig: Uuid) {
        let Some(pos) = self.ragdolls.iter().position(|r| r.rig == rig) else {
            return;
        };
        let removed_index = self.ragdolls[pos].index;
        sys::remove_ragdoll(&mut self.world, removed_index);
        self.ragdolls.remove(pos);
        // The shim compacts its ragdoll vector on removal, so every slot above the removed one
        // shifts down by one — mirror that on the Rust side so the indices stay in lockstep.
        for entry in &mut self.ragdolls {
            if entry.index > removed_index {
                entry.index -= 1;
            }
        }
    }

    /// Whether `rig` has a live ragdoll.
    #[must_use]
    pub fn has_ragdoll(&self, rig: Uuid) -> bool {
        self.ragdolls.iter().any(|r| r.rig == rig)
    }

    /// Drive every active ragdoll's SwingTwist motors toward its rig's animation target: set the
    /// swing + twist motor states to `Position` and the body-space target orientation to the
    /// per-joint rotation. A passive ragdoll, a rig with no target this frame, the root bone (no
    /// parent constraint), and a non-SwingTwist joint are all left to swing freely. Call once per
    /// fixed step **before** [`World::step`] so the motors are read during the solve. The glam
    /// quaternion (`xyzw`) feeds `SetTargetOrientationBS` directly (glam == Jolt order, no swizzle).
    pub fn drive_ragdolls_to_pose(&mut self, targets: &[PoseTarget]) {
        // Resolve each active ragdoll's per-part target rotation first so the motor loop can take
        // `&mut self.world` without aliasing the `&self.ragdolls` read.
        let mut drives: Vec<(u32, u32, [f32; 4])> = Vec::new();
        for entry in &self.ragdolls {
            if !entry.motors_active {
                continue; // a passive ragdoll swings under gravity + limits alone
            }
            let Some(target) = targets.iter().find(|t| t.rig == entry.rig) else {
                continue; // no animation target this frame: let the bodies swing freely
            };
            let count = entry.parent_index.len().min(target.local.len());
            for i in 0..count {
                let part = u32::try_from(i).unwrap_or(u32::MAX);
                // Only a SwingTwist bone carries the motors; a Free/Hinge or root bone stays limp.
                if !sys::ragdoll_part_is_swing_twist(&self.world, entry.index, part) {
                    continue;
                }
                drives.push((entry.index, part, target.local[i].rotation.to_array()));
            }
        }
        for (index, part, target) in drives {
            sys::ragdoll_set_swing_twist_motor(&mut self.world, index, part, true, target);
        }
    }

    /// Ease every ragdoll's per-bone physics weight toward its target by `weight_rate * dt`
    /// (clamped so it never overshoots), so the animation↔physics blend ramps without a pop. Call
    /// once per fixed step **before** [`World::write_ragdoll_poses`].
    pub fn advance_ragdoll_blend(&mut self, dt: f32) {
        for entry in &mut self.ragdolls {
            let step = entry.weight_rate * dt;
            let count = entry.weight_current.len().min(entry.weight_target.len());
            for i in 0..count {
                let delta = entry.weight_target[i] - entry.weight_current[i];
                entry.weight_current[i] = if delta.abs() <= step {
                    entry.weight_target[i]
                } else {
                    entry.weight_current[i] + step.copysign(delta)
                };
            }
        }
    }

    /// After [`World::step`]: for each live ragdoll, read every part's world transform, convert it
    /// to the bone's LOCAL TRS (`inverse(parent_world) * part_world`, the inverse of the joint
    /// matrices' composition), and write it into the bone's [`PoseOverride`] blended by the eased
    /// per-bone weight. At weight ≥ [`PURE_PHYSICS_WEIGHT`] the physics pose overwrites outright;
    /// below it the physics pose blends over the animation pose the evaluator wrote earlier this
    /// frame (`mix`/`slerp`). A bone with no `PoseOverride` gets one added.
    pub fn write_ragdoll_poses(&mut self, scene: &mut Scene) {
        // Resolve every (bone entity, local TRS, weight) write first against an immutable scene
        // borrow + the world read, then apply the component writes — the read of each part's world
        // transform and the scene mutation cannot overlap.
        struct PoseWrite {
            bone: Entity,
            translation: Vec3,
            rotation: Quat,
            scale: Vec3,
            weight: f32,
        }
        let mut writes: Vec<PoseWrite> = Vec::new();

        for entry in &self.ragdolls {
            if !scene.valid(entry.rig_entity)
                || !scene.has_component::<SkinnedMesh>(entry.rig_entity)
            {
                continue;
            }
            let bone_handles = scene
                .with_component::<SkinnedMesh, _>(entry.rig_entity, |s| s.bone_handles.clone())
                .unwrap_or_default();
            let count = bone_handles.len();
            let parts = usize::try_from(sys::ragdoll_body_count(&self.world, entry.index))
                .unwrap_or(usize::MAX);

            // Read every part's world transform up front (a part is 1:1 with a bone index).
            let mut part_world = vec![Mat4::IDENTITY; count];
            for (i, slot) in part_world.iter_mut().enumerate().take(count.min(parts)) {
                let part = u32::try_from(i).unwrap_or(u32::MAX);
                let (position, rotation) =
                    sys::ragdoll_part_transform(&self.world, entry.index, part);
                *slot = Mat4::from_rotation_translation(
                    Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
                    Vec3::from_array(position),
                );
            }
            let rig_world = scene.compose_world_matrix(entry.rig_entity);

            for (i, &bone) in bone_handles.iter().enumerate() {
                if !scene.valid(bone) || i >= parts {
                    continue;
                }
                // Local = inverse(parent world) * world, the inverse of jointMatrices' composition.
                let parent = entry.parent_index[i];
                let parent_world = if parent >= 0 {
                    part_world[parent as usize]
                } else {
                    rig_world
                };
                let local = parent_world.inverse() * part_world[i];
                let (scale, rotation, translation) = local.to_scale_rotation_translation();
                writes.push(PoseWrite {
                    bone,
                    translation,
                    rotation: rotation.normalize(),
                    scale,
                    weight: entry.weight_current[i],
                });
            }
        }

        for write in writes {
            if !scene.has_component::<PoseOverride>(write.bone) {
                let _ = scene.add_component(write.bone, PoseOverride::default());
            }
            let _ = scene.with_component_mut::<PoseOverride, _>(write.bone, |over| {
                if write.weight >= PURE_PHYSICS_WEIGHT {
                    over.translation = write.translation;
                    over.rotation = write.rotation;
                    over.scale = write.scale;
                } else {
                    // Blend the physics pose over the animation pose the evaluator wrote earlier.
                    over.translation = over.translation.lerp(write.translation, write.weight);
                    over.rotation = over
                        .rotation
                        .slerp(write.rotation, write.weight)
                        .normalize();
                    over.scale = over.scale.lerp(write.scale, write.weight);
                }
            });
        }
    }

    /// Set a rig's active-ragdoll blend. `active` toggles the motors (going passive releases every
    /// SwingTwist motor to `Off`, so the bodies fall under gravity + limits alone); `body_weight`
    /// fills every bone's target weight uniformly (`0` = pure animation, `1` = pure physics); a
    /// `bone` ≥ 0 with `weight` retargets one bone (a hit reaction is `bone` + `weight` left to
    /// ease back).
    ///
    /// # Errors
    ///
    /// [`Error::NoRagdoll`] when `rig` has no live ragdoll, or [`Error::BoneOutOfRange`] when a
    /// supplied `bone` index is outside the rig's bone range.
    pub fn set_ragdoll_blend(
        &mut self,
        rig: Uuid,
        active: Option<bool>,
        body_weight: Option<f32>,
        bone: Option<i32>,
        weight: Option<f32>,
    ) -> Result<()> {
        let Some(pos) = self.ragdolls.iter().position(|r| r.rig == rig) else {
            return Err(Error::NoRagdoll);
        };
        if let Some(body_weight) = body_weight {
            let clamped = body_weight.clamp(0.0, 1.0);
            self.ragdolls[pos].weight_target.fill(clamped);
        }
        if let (Some(bone), Some(weight)) = (bone, weight) {
            let target = &mut self.ragdolls[pos].weight_target;
            let Ok(slot) = usize::try_from(bone) else {
                return Err(Error::BoneOutOfRange(bone));
            };
            if slot >= target.len() {
                return Err(Error::BoneOutOfRange(bone));
            }
            target[slot] = weight.clamp(0.0, 1.0);
        }
        if let Some(active) = active {
            self.ragdolls[pos].motors_active = active;
            if !active {
                // Going passive: release every SwingTwist motor so the bodies fall under gravity +
                // limits alone (the drive loop will not re-arm them while inactive).
                let (index, parts) = {
                    let entry = &self.ragdolls[pos];
                    (
                        entry.index,
                        sys::ragdoll_body_count(&self.world, entry.index),
                    )
                };
                for part in 0..parts {
                    if sys::ragdoll_part_is_swing_twist(&self.world, index, part) {
                        sys::ragdoll_set_swing_twist_motor(
                            &mut self.world,
                            index,
                            part,
                            false,
                            Quat::IDENTITY.to_array(),
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// A rig's live ragdoll state: presence, the motor-active flag, the mean target weight across
    /// bones, and the bone count. All-default (absent) when the rig has no ragdoll.
    #[must_use]
    pub fn ragdoll_state(&self, rig: Uuid) -> RagdollState {
        let Some(entry) = self.ragdolls.iter().find(|r| r.rig == rig) else {
            return RagdollState::default();
        };
        let bones = entry.weight_target.len();
        let body_weight = if bones == 0 {
            0.0
        } else {
            entry.weight_target.iter().sum::<f32>() / bones as f32
        };
        RagdollState {
            present: true,
            active: entry.motors_active,
            body_weight,
            bones: i32::try_from(bones).unwrap_or(i32::MAX),
        }
    }

    /// The world transform (translation, rotation `xyzw`) of a ragdoll part. `rig` selects the
    /// ragdoll, `part` the bone index; returns `None` for an unknown rig or out-of-range part.
    #[must_use]
    pub fn ragdoll_part_transform(&self, rig: Uuid, part: u32) -> Option<(Vec3, Quat)> {
        let entry = self.ragdolls.iter().find(|r| r.rig == rig)?;
        if part >= sys::ragdoll_body_count(&self.world, entry.index) {
            return None;
        }
        let (position, rotation) = sys::ragdoll_part_transform(&self.world, entry.index, part);
        Some((
            Vec3::from_array(position),
            Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
        ))
    }

    /// The number of parts (bodies) in `rig`'s ragdoll, or `0` when it has none.
    #[must_use]
    pub fn ragdoll_part_count(&self, rig: Uuid) -> u32 {
        self.ragdolls
            .iter()
            .find(|r| r.rig == rig)
            .map_or(0, |entry| sys::ragdoll_body_count(&self.world, entry.index))
    }
}
