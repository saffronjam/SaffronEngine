//! The fixed-step loop, the contact ring drain, and the character controllers.

use glam::{Quat, Vec3};

use saffron_physics_sys::{self as sys, CharacterCreate, INVALID_BODY_ID};
use saffron_scene::{CharacterController, Collider, Entity, Scene, Transform};

use crate::error::{Error, Result};
use crate::types::{
    CONTACT_RING_CAP, ContactDrain, ContactEvent, ContactKind, FIXED_STEP, MotionType,
};

use super::*;

/// The accumulator backstop: at most this many fixed substeps run per [`World::step`], so a runaway
/// `dt` cannot spiral into an unbounded catch-up.
const MAX_SUBSTEPS: u32 = 8;

/// Sea-level air density in kg/m³, the medium the wind drag acts through.
const AIR_DENSITY_KG_M3: f32 = 1.225;

/// The drag coefficient every body takes. A cube's, which is the blunt end of the range —
/// collider shapes are approximations, so a per-shape coefficient would be false precision.
const DRAG_COEFFICIENT: f32 = 1.05;

impl World {
    /// Advance the sim by `dt` in fixed substeps, then write every Dynamic body's world pose back
    /// into its entity's [`Transform`].
    ///
    /// `dt` is assumed already clamped by the caller's play loop. Each substep first drives every
    /// Kinematic body toward its entity's fresh world transform via `MoveKinematic` (so the swept
    /// motion imparts contact velocity to the dynamics it hits), then advances the world. After the
    /// substeps settle, the contact transitions Jolt buffered on its job threads are drained into
    /// the seq-stamped ring ([`World::drain_into_ring`]).
    pub fn step(&mut self, scene: &mut Scene, dt: f32) {
        self.accumulator += dt;
        let mut substeps = 0u32;
        while self.accumulator >= FIXED_STEP && substeps < MAX_SUBSTEPS {
            // Drive every Kinematic body (per-bone bodies + free kinematic bodies) toward its
            // entity's fresh world transform via MoveKinematic *before* the step, so the swept
            // motion over this same fixed dt imparts contact velocity to the dynamics it hits
            // (never a teleport, which gives zero contact velocity).
            self.move_kinematic_bodies(scene);
            self.apply_wind_drag();
            sys::world_step(&mut self.world, FIXED_STEP, 1);
            // Advance every CharacterVirtual against the just-settled world: gravity integration +
            // the desired-velocity clamp, then stick-to-floor + WalkStairs via ExtendedUpdate.
            self.step_characters(scene);
            self.accumulator -= FIXED_STEP;
            self.step_count += 1;
            substeps += 1;
        }
        if substeps == 0 {
            return; // no fixed step elapsed this frame — transforms are unchanged
        }

        // Write each Dynamic body's world pose back into its entity's local Transform. Bodies are
        // scoped to root entities, where world equals local. The rotation is stored as the Euler the
        // Transform's convention round-trips the quaternion to.
        for entry in &self.bodies {
            if entry.motion != MotionType::Dynamic
                || !scene.has_component::<Transform>(entry.entity)
            {
                continue;
            }
            let (position, rotation) = sys::body_position_rotation(&self.world, entry.id);
            let translation = Vec3::from_array(position);
            let quat = Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]);
            let euler = saffron_scene::quat_to_euler_zyx(quat);
            let _ = scene.with_component_mut::<Transform, _>(entry.entity, |t| {
                t.translation = translation;
                t.rotation = euler;
            });
        }

        // Write each character's resolved world position back into its entity-root Transform
        // (binding mode a: position only — rotation/animation are independent).
        for entry in &self.characters {
            if !scene.has_component::<Transform>(entry.entity) {
                continue;
            }
            let position = Vec3::from_array(sys::character_position(&self.world, entry.index));
            let _ = scene.with_component_mut::<Transform, _>(entry.entity, |t| {
                t.translation = position;
            });
        }

        self.drain_into_ring();
    }

    /// Push every awake wind-coupled dynamic body with the air it is moving through, sampling the
    /// shared wind field at the body's own position so a body and the vegetation beside it feel
    /// one field.
    ///
    /// The force is the quadratic drag `½ρ·Cd·A·|v_rel|·v_rel` on the body's velocity relative to
    /// the air, so a light wide body is carried and a dense compact one barely moves. `A` is the
    /// body's authored coupling, zero unless a `Rigidbody` asked for it, so an untouched world
    /// takes no wind force and its bodies stay bit-exact across targets.
    fn apply_wind_drag(&mut self) {
        if self.wind.speed <= 0.0 && self.wind_sources.is_empty() {
            return;
        }
        for index in 0..self.bodies.len() {
            let entry = self.bodies[index];
            if entry.motion != MotionType::Dynamic
                || entry.drag_area <= 0.0
                || !sys::body_is_active(&self.world, entry.id)
            {
                continue;
            }
            let position = Vec3::from_array(sys::body_position(&self.world, entry.id));
            let velocity = Vec3::from_array(sys::body_linear_velocity(&self.world, entry.id));
            let relative = self.sample_wind(position).velocity - velocity;
            let speed = relative.length();
            if speed <= f32::EPSILON {
                continue;
            }
            let force =
                relative * (0.5 * AIR_DENSITY_KG_M3 * DRAG_COEFFICIENT * entry.drag_area * speed);
            sys::body_add_force(&mut self.world, entry.id, force.to_array());
        }
    }

    /// Drain the contact transitions Jolt buffered on its job threads (across this frame's
    /// substeps) into the seq-stamped ring. Safe here: single-threaded on the sim thread, and the
    /// body → entity index is stable for the play session. Each pending pair is mapped via
    /// `index_by_body_id`, seq-stamped, given its `sensor` flag from the `BodyEntry` records, and
    /// pushed with cap-[`CONTACT_RING_CAP`] `pop_front` eviction.
    fn drain_into_ring(&mut self) {
        for pending in sys::drain_contacts(&mut self.world) {
            let a = self.index_by_body_id.get(&pending.a).copied();
            let b = self.index_by_body_id.get(&pending.b).copied();
            self.contact_seq += 1;
            let event = ContactEvent {
                seq: self.contact_seq,
                kind: if pending.begin {
                    ContactKind::Begin
                } else {
                    ContactKind::End
                },
                target_a: a.map(|i| self.bodies[i].target),
                target_b: b.map(|i| self.bodies[i].target),
                sensor: a.is_some_and(|i| self.bodies[i].sensor)
                    || b.is_some_and(|i| self.bodies[i].sensor),
                point: Vec3::from_array(pending.point),
                normal: Vec3::from_array(pending.normal),
                tick: self.step_count,
            };
            if self.contact_ring.len() >= CONTACT_RING_CAP {
                self.contact_ring.pop_front(); // evict the oldest at cap
            }
            self.contact_ring.push_back(event);
        }
    }

    /// Snapshot the contact events with `seq > since` (non-blocking), plus the cursor metadata that
    /// lets a stale cursor detect it missed evicted events. `high_water_seq` is the newest seq the
    /// ring has stamped, `oldest_seq` the lowest still retained (`0` when empty), and `overflowed`
    /// is set when the cursor is older than that retained tail so the caller should resync.
    #[must_use]
    pub fn drain_contacts(&self, since: u64) -> ContactDrain {
        let events: Vec<ContactEvent> = self
            .contact_ring
            .iter()
            .filter(|event| event.seq > since)
            .copied()
            .collect();
        let oldest_seq = self.contact_ring.front().map_or(0, |event| event.seq);
        ContactDrain {
            events,
            high_water_seq: self.contact_seq,
            oldest_seq,
            // A cursor older than the oldest retained event missed evictions — signal a resync.
            overflowed: oldest_seq > 0 && since + 1 < oldest_seq,
        }
    }

    /// Drive every Kinematic body toward its entity's fresh world transform via `MoveKinematic`
    /// over one [`FIXED_STEP`], so the swept motion imparts contact velocity to the dynamics it
    /// hits. The pose is composed fresh from the parent chain (not read from the possibly-stale
    /// `WorldTransform` cache — the most likely source of a one-frame follow lag). A body whose
    /// entity is no longer valid is skipped.
    fn move_kinematic_bodies(&mut self, scene: &Scene) {
        // Resolve the (id, fresh pose) of each valid Kinematic body first so the move loop can
        // take `&mut self.world` without aliasing the `&self.bodies` read.
        let moves: Vec<(u32, [f32; 3], [f32; 4])> = self
            .bodies
            .iter()
            .filter(|entry| entry.motion == MotionType::Kinematic && scene.valid(entry.entity))
            .map(|entry| {
                let (position, rotation) = fresh_world_pose(scene, entry.entity);
                (entry.id, position.to_array(), rotation.to_array())
            })
            .collect();
        for (id, position, rotation) in moves {
            sys::move_kinematic(&mut self.world, id, position, rotation, FIXED_STEP);
        }
    }

    /// Advance every `CharacterVirtual` one fixed substep against the just-settled world: integrate
    /// the controller's vertical velocity (resting on the floor when grounded and not moving up),
    /// clamp the desired horizontal velocity to `max_speed`, set the linear velocity, then
    /// `ExtendedUpdate` (stick-to-floor + WalkStairs) and write the resolved ground state back.
    fn step_characters(&mut self, scene: &mut Scene) {
        if self.characters.is_empty() {
            return;
        }
        let gravity = Vec3::from_array(sys::world_gravity(&self.world));
        for entry in &mut self.characters {
            let Ok(mut controller) = scene.component::<CharacterController>(entry.entity) else {
                continue;
            };
            let grounded = sys::character_on_ground(&self.world, entry.index);
            if grounded && controller.vertical_velocity <= 0.0 {
                controller.vertical_velocity = 0.0; // rest on the floor
            } else {
                controller.vertical_velocity += gravity.y * controller.gravity_factor * FIXED_STEP;
            }
            let mut horizontal = Vec3::new(
                controller.desired_velocity.x,
                0.0,
                controller.desired_velocity.z,
            );
            let speed = horizontal.length();
            if speed > controller.max_speed && speed > 1e-5 {
                horizontal *= controller.max_speed / speed;
            }
            sys::character_set_linear_velocity(
                &mut self.world,
                entry.index,
                [horizontal.x, controller.vertical_velocity, horizontal.z],
            );
            entry.last_velocity =
                Vec3::new(horizontal.x, controller.vertical_velocity, horizontal.z);
            let applied_gravity = gravity * controller.gravity_factor;
            sys::character_extended_update(
                &mut self.world,
                entry.index,
                FIXED_STEP,
                applied_gravity.to_array(),
                controller.max_step_height,
            );
            controller.on_ground = sys::character_on_ground(&self.world, entry.index);
            // Persist the integrated runtime state (vertical_velocity + on_ground) back onto the
            // component so the next substep reads the updated values.
            let _ = scene.with_component_mut::<CharacterController, _>(entry.entity, |c| {
                c.vertical_velocity = controller.vertical_velocity;
                c.on_ground = controller.on_ground;
            });
        }
    }

    /// Create a `CharacterVirtual` controller for `entity`: a capsule from its [`Collider`]
    /// (radius `half_extents.x`, half-height `half_extents.y`, with defaults when absent) and the
    /// `max_slope_angle` from its [`CharacterController`], seeded at the entity's fresh world pose.
    ///
    /// # Errors
    ///
    /// [`Error::CharacterCapsule`] if the capsule shape could not be built.
    pub fn add_character(&mut self, entity: Entity, scene: &Scene) -> Result<()> {
        let (radius, half_height) = scene
            .component::<Collider>(entity)
            .map(|c| (c.half_extents.x.max(0.05), c.half_extents.y.max(0.05)))
            .unwrap_or((0.3, 0.6));
        // A controller-less entity falls back to ~45° (`0.785398`), the hand-typed literal kept
        // verbatim for a byte-exact seed.
        #[allow(clippy::approx_constant)]
        let max_slope_angle = scene
            .component::<CharacterController>(entity)
            .map(|c| c.max_slope_angle)
            .unwrap_or(0.785_398);
        let position = fresh_world_translation(scene, entity);
        let create = CharacterCreate {
            radius,
            half_height,
            max_slope_angle,
            position: position.to_array(),
        };
        let index = sys::add_character(&mut self.world, &create);
        if index == INVALID_BODY_ID {
            return Err(Error::CharacterCapsule);
        }
        self.characters.push(CharacterEntry {
            entity,
            index,
            last_velocity: Vec3::ZERO,
        });
        Ok(())
    }
}
