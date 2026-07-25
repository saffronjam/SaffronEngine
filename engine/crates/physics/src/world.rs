//! The per-play physics world: the body bookkeeping, the populate walk, the fixed-step loop, and
//! the read-only + mutator surface.
//!
//! The world is split across the crate boundary: the Jolt-owning half is the `-sys` `JoltWorld`
//! (the `PhysicsSystem` + the four shim classes), and the Rust-side bookkeeping lives here —
//! [`World`] holds the `UniquePtr<JoltWorld>` plus the body table, the body-id index, and the
//! fixed-step accumulator. `World`'s [`Drop`] is just dropping the `UniquePtr`; the Jolt teardown
//! *order* is the shim destructor's job, so the Rust side never sequences Jolt destruction.
//!
//! `bodies` is a `Vec<BodyEntry>` in **creation order** (never a map iteration) because that order
//! is load-bearing for the deterministic sim; the `HashMap<u32, usize>` is only for the hit →
//! entity lookup the contact drain needs.

use std::collections::{HashMap, HashSet, VecDeque};

use glam::{Mat4, Quat, Vec3};

use saffron_core::Uuid;
use saffron_geometry::Mesh;
use saffron_physics_sys::{
    self as sys, BodyCreate, BonePart, CharacterCreate, INVALID_BODY_ID, JoltWorld,
};
use saffron_scene::{
    BonePhysics, BonePhysicsComponent, CharacterController, Collider, Entity, IdComponent,
    KinematicBones, Mesh as MeshComponent, PoseOverride, Relationship, Rigidbody, Scene, Shape,
    SkinnedMesh, Transform,
};

use crate::error::{Error, Result};
use crate::types::{
    BodyInfo, CONTACT_RING_CAP, ContactDrain, ContactEvent, ContactKind, FIXED_STEP, MotionType,
    ObjectLayer, PoseTarget, RagdollState, RayHit, StaticTargetBodyCreate, WorldStats,
};

/// The weight units/sec the eased per-bone physics weight approaches its target, so the
/// animation↔physics blend ramps without a pop.
const RAGDOLL_WEIGHT_RATE: f32 = 6.0;

/// At/above this eased weight the physics pose overwrites the bone's [`PoseOverride`] outright;
/// below it the physics pose blends over the animation pose by the weight.
const PURE_PHYSICS_WEIGHT: f32 = 0.999;

/// The Jolt `EAllowedDOFs::All` bitmask: all six translation+rotation axes free.
const ALLOWED_DOFS_ALL: u8 = 0b0011_1111;
const DOF_TRANSLATION_X: u8 = 0b0000_0001;
const DOF_TRANSLATION_Y: u8 = 0b0000_0010;
const DOF_TRANSLATION_Z: u8 = 0b0000_0100;
const DOF_ROTATION_X: u8 = 0b0000_1000;
const DOF_ROTATION_Y: u8 = 0b0001_0000;
const DOF_ROTATION_Z: u8 = 0b0010_0000;

/// The accumulator backstop: at most this many fixed substeps run per [`World::step`], so a runaway
/// `dt` cannot spiral into an unbounded catch-up.
const MAX_SUBSTEPS: u32 = 8;

/// One body created from an entity's components, tracked for transform write-back, scene queries,
/// and contact events. Stored in creation order so the sim stays reproducible run-to-run.
#[derive(Clone, Copy, Debug)]
struct BodyEntry {
    /// The owning entity handle, for the per-step transform write-back.
    entity: saffron_scene::Entity,
    /// The owner's stable id, surfaced in [`BodyInfo`] and the contact mapping.
    target: crate::WorldHitTarget,
    /// The raw Jolt `BodyID` (index + sequence) the bridge round-trips.
    id: u32,
    /// The body's motion type.
    motion: MotionType,
    /// Whether the collider is a sensor (trigger volume); sets [`ContactEvent::sensor`] when this
    /// body is one half of a contact pair.
    sensor: bool,
}

/// One `CharacterVirtual` sweep object, paired with its owner entity. The shim owns the Jolt
/// `Ref<CharacterVirtual>` (in `JoltWorld.characters`); this records the owner + the shim slot so
/// the step loop can resolve the [`CharacterController`] each substep.
#[derive(Clone, Copy, Debug)]
struct CharacterEntry {
    /// The owning entity handle, for the per-step controller read + position write-back.
    entity: Entity,
    /// The character's slot in the shim's `JoltWorld.characters` vector.
    index: u32,
    /// The velocity the last step commanded, for the interaction-field emitters.
    last_velocity: Vec3,
}

/// One live ragdoll's Rust-side bookkeeping. The Jolt `Ragdoll`/`RagdollSettings` live in the
/// shim (`JoltWorld.ragdolls`); this holds the rig identity, the parent-index map for the
/// world→local pose conversion, and the eased per-bone blend state. Built passive
/// (`motors_active = false`, weights `1` = pure physics); [`World::set_ragdoll_blend`] turns the
/// motors on and retargets the weights.
#[derive(Clone, Debug)]
struct RagdollEntry {
    /// The rig mesh entity's stable id (the `CreateRagdoll` user-data + the lookup key).
    rig: Uuid,
    /// The rig mesh entity handle, the parent of the root bone in the world→local pose conversion.
    rig_entity: Entity,
    /// The ragdoll's slot in the shim's `JoltWorld.ragdolls` vector.
    index: u32,
    /// Bone i → parent bone index (`-1` = root), for the world→local `PoseOverride` conversion.
    parent_index: Vec<i32>,
    /// Per-bone desired physics weight (`1` = pure physics); the eased `weight_current` approaches
    /// it each step.
    weight_target: Vec<f32>,
    /// Per-bone eased weight (`1` in a pure ragdoll), the weight the pose write-back blends by.
    weight_current: Vec<f32>,
    /// Weight units/sec the eased weight approaches the target.
    weight_rate: f32,
    /// Active (motor-driven) vs passive ragdoll.
    motors_active: bool,
}

/// A mesh cook callback the host supplies so the asset reader stays out of the physics crate.
///
/// ConvexHull/Mesh colliders read their `source_mesh` `.smesh` through this; the host binds it to
/// the asset reader. A Jolt-free [`Mesh`](saffron_geometry::Mesh) crosses the seam, never a Jolt
/// type. The closure's `String` error becomes [`Error::CookFailed`].
pub type MeshCook<'a> = dyn FnMut(Uuid) -> std::result::Result<Mesh, String> + 'a;

/// The per-play physics world.
///
/// Owns the Jolt world handle and the Rust-side bookkeeping. There is one world type and one code
/// path per operation; the `Option<World>` (no-world-yet) lives in the host, so every method here
/// assumes a live world.
pub struct World {
    /// The Jolt world handle; its `Drop` runs the shim's teardown-ordered destructor.
    world: cxx::UniquePtr<JoltWorld>,
    /// Every created body, in creation order (deterministic iteration).
    bodies: Vec<BodyEntry>,
    /// Raw `BodyID` → index into `bodies`, for the contact drain's hit → entity lookup.
    index_by_body_id: HashMap<u32, usize>,
    /// Every `CharacterVirtual` controller, in creation order.
    characters: Vec<CharacterEntry>,
    /// Every live ragdoll, in creation order.
    ragdolls: Vec<RagdollEntry>,
    /// The bounded, seq-stamped contact-event ring (cap [`CONTACT_RING_CAP`]); the oldest is
    /// evicted at the cap.
    contact_ring: VecDeque<ContactEvent>,
    /// The monotonic contact sequence counter; `++`-stamped onto each event entering the ring.
    contact_seq: u64,
    /// Of `bodies`, how many are Dynamic.
    dynamic_body_count: i32,
    /// Physics steps run so far (the `ContactEvent::tick` stamp).
    step_count: i64,
    /// The fixed-step accumulator (seconds of unspent `dt`).
    accumulator: f32,
}

/// Tear down the process-global Jolt state ([`saffron_physics_sys::shutdown`]): `UnregisterTypes`
/// then destroy the `Factory`. This pairs with the implicit global init [`World::new`] runs through
/// `sys::init`.
///
/// The `Factory`/registered types are a process global that outlives every [`World`], so this must
/// run **only after the last world has dropped** — calling it while a live world still holds Jolt
/// bodies is a use-after-free. The host sequences this in its teardown (drop the play world, then
/// shut down the globals). Idempotent: safe with no prior world and safe to call twice.
pub fn shutdown_physics() {
    sys::shutdown();
}

impl World {
    /// Initialize the Jolt globals (idempotent) and allocate + init a fresh world.
    ///
    /// # Errors
    ///
    /// [`Error::GlobalInit`] if the Jolt globals fail to install, or [`Error::WorldCreate`] if the
    /// world allocation returns null.
    pub fn new() -> Result<World> {
        sys::init().map_err(Error::GlobalInit)?;
        let mut world = sys::world_new().ok_or(Error::WorldCreate)?;
        sys::world_init(&mut world);
        Ok(World {
            world,
            bodies: Vec::new(),
            index_by_body_id: HashMap::new(),
            characters: Vec::new(),
            ragdolls: Vec::new(),
            contact_ring: VecDeque::new(),
            contact_seq: 0,
            dynamic_body_count: 0,
            step_count: 0,
            accumulator: 0.0,
        })
    }

    /// Walk the scene's colliders and create a body for each, in deterministic entity-iteration
    /// order. Builds all five shapes: the analytic Box/Sphere/Capsule size from `half_extents`,
    /// and ConvexHull/Mesh cook their `source_mesh` `.smesh` through `cook` (vertices/indices fed
    /// in index order for a reproducible cooked shape). A per-collider failure — a `Mesh` on a
    /// Dynamic body, a missing cook source, a cook error, or a degenerate shape — is logged and
    /// the body skipped; the world still builds.
    pub fn populate(&mut self, scene: &mut Scene, cook: &mut MeshCook<'_>) {
        // Gather the collider rows first so the body-creation loop can borrow `scene` immutably for
        // the world-pose composition (the `for_each` closure holds a mutable borrow).
        let mut rows: Vec<(saffron_scene::Entity, Collider)> = Vec::new();
        scene.for_each::<&Collider, _>(|entity, collider| {
            rows.push((entity, *collider));
        });

        for (entity, _) in rows {
            if let Err(err) = self.add_entity_body(scene, entity, cook) {
                tracing::warn!(
                    "physics: skipping body for {}: {err}",
                    id_of(scene, entity).0
                );
            }
        }
    }

    /// Create the one body `entity`'s [`Collider`] (plus any [`Rigidbody`]) describes, registered
    /// under the entity's stable uuid, and return its raw `BodyID`. The per-entity path
    /// [`World::populate`] walks, and the way an entity that appears mid-play (a promoted macro
    /// plant) gains collision.
    ///
    /// # Errors
    ///
    /// [`Error::MissingCollider`] when the entity carries no collider or owns a
    /// [`CharacterController`](saffron_scene::CharacterController) instead (its capsule is a
    /// `CharacterVirtual`, never a world body — a static body there would block the sweep), the
    /// cook error for a `ConvexHull`/`Mesh` shape that could not be built, or
    /// [`Error::BodyCreate`] when Jolt rejected the shape or hit its body limit.
    pub fn add_entity_body(
        &mut self,
        scene: &Scene,
        entity: Entity,
        cook: &mut MeshCook<'_>,
    ) -> Result<u32> {
        if scene.has_component::<saffron_scene::CharacterController>(entity) {
            return Err(Error::MissingCollider);
        }
        let collider = scene
            .component::<Collider>(entity)
            .map_err(|_| Error::MissingCollider)?;

        // A collider with no rigidbody is an implicit Static body; with one, its motion wins.
        let rigidbody = scene.component::<Rigidbody>(entity).ok();
        let motion = rigidbody
            .map(|rb| MotionType::from_scene(rb.motion))
            .unwrap_or(MotionType::Static);

        // Cook the ConvexHull/Mesh geometry (and reject a Mesh on a Dynamic body) before touching
        // Jolt, so a typed cause reaches the caller.
        let geometry = cook_shape_geometry(&collider, motion, cook)?;

        // World translation/rotation compose on a cache miss (the play scene's caches may be cold
        // here), scale-free.
        let position = scene.world_translation(entity);
        let rotation = scene.world_rotation(entity);
        let object_layer = resolve_object_layer(rigidbody.as_ref(), motion, collider.is_sensor);

        let create = body_create(
            &collider,
            rigidbody.as_ref(),
            motion,
            object_layer,
            position,
            rotation,
        );
        let id = sys::create_body(
            &mut self.world,
            &create,
            &geometry.hull_points,
            &geometry.mesh_vertices,
            &geometry.mesh_indices,
        );
        if id == INVALID_BODY_ID {
            // The shim already logged the shape/body create failure.
            return Err(Error::BodyCreate);
        }

        let target = crate::WorldHitTarget::SceneEntity(id_of(scene, entity));
        self.index_by_body_id.insert(id, self.bodies.len());
        self.bodies.push(BodyEntry {
            entity,
            target,
            id,
            motion,
            sensor: collider.is_sensor,
        });
        if motion == MotionType::Dynamic {
            self.dynamic_body_count += 1;
        }
        Ok(id)
    }

    /// Remove and destroy every body registered to `entity`, in one batch. The counterpart of
    /// [`World::add_entity_body`] for an entity that leaves mid-play (a demoted macro plant).
    pub fn remove_entity_bodies(&mut self, entity: Entity) {
        let ids: Vec<u32> = self
            .bodies
            .iter()
            .filter(|body| body.entity == entity)
            .map(|body| body.id)
            .collect();
        self.remove_bodies(&ids);
    }

    /// Create one Kinematic capsule body per driven joint of every enabled
    /// [`KinematicBones`] rig, so the animated pose shoves the dynamic world via
    /// `MoveKinematic` each step (binding mode b, animation→physics, no pose write-back). A rig is
    /// skipped when its bones are disabled or it has no [`SkinnedMesh`]; the `driven` list selects
    /// the joints (empty = every joint). Each capsule is sized from the matching
    /// [`BonePhysics::shape_half_extents`] (radius `.x`, half-height `.y`, with a `0.03` floor so a
    /// leaf/unfitted bone is never a degenerate capsule), seeded at the joint's fresh world pose,
    /// on the Moving layer. The bodies join `bodies` keyed by the joint entity in creation order
    /// and tear down with the world.
    pub fn build_bone_bodies(&mut self, scene: &mut Scene) {
        // Gather the enabled rigs first so the body-creation loop can read `scene` immutably for
        // the per-joint world-pose composition (the `for_each` closure holds a mutable borrow).
        let mut rigs: Vec<(Entity, Vec<i32>)> = Vec::new();
        scene.for_each::<&KinematicBones, _>(|rig, bones| {
            if bones.enabled {
                rigs.push((rig, bones.driven.clone()));
            }
        });

        for (rig, driven) in rigs {
            if !scene.has_component::<SkinnedMesh>(rig) {
                continue;
            }
            let bone_handles = scene
                .with_component::<SkinnedMesh, _>(rig, |s| s.bone_handles.clone())
                .unwrap_or_default();
            let bones = scene
                .with_component::<BonePhysicsComponent, _>(rig, |p| p.bones.clone())
                .ok();

            for (index, &joint) in bone_handles.iter().enumerate() {
                if !is_driven(&driven, index) || !scene.valid(joint) {
                    continue;
                }
                // Capsule from the per-bone shape_half_extents (radius .x, half-height .y), Y-up;
                // a small default for a leaf/unfitted bone so Jolt never rejects a degenerate one.
                let extents = bones
                    .as_ref()
                    .and_then(|b| b.get(index))
                    .map_or(Vec3::ZERO, |b| b.shape_half_extents);
                let (position, rotation) = fresh_world_pose(scene, joint);
                let create = BodyCreate {
                    shape: shape_raw(Shape::Capsule),
                    half_extents: [extents.x.max(0.03), extents.y.max(0.03), extents.z],
                    offset: [0.0; 3],
                    position: position.to_array(),
                    rotation: rotation.to_array(),
                    motion: MotionType::Kinematic.raw(),
                    object_layer: ObjectLayer::Moving.raw(),
                    is_sensor: false,
                    friction: 0.2,
                    restitution: 0.0,
                    linear_damping: 0.0,
                    angular_damping: 0.0,
                    gravity_factor: 1.0,
                    mass: 1.0,
                    allowed_dofs: ALLOWED_DOFS_ALL,
                };
                let id = sys::create_body(&mut self.world, &create, &[], &[], &[]);
                if id == INVALID_BODY_ID {
                    continue;
                }
                let uuid = id_of(scene, joint);
                let target = crate::WorldHitTarget::SceneEntity(uuid);
                self.index_by_body_id.insert(id, self.bodies.len());
                self.bodies.push(BodyEntry {
                    entity: joint,
                    target,
                    id,
                    motion: MotionType::Kinematic,
                    sensor: false,
                });
            }
        }
    }

    /// Create one batch of static tagged-target bodies through Jolt's batched broadphase
    /// insertion, registering each created body under its row's [`WorldHitTarget`] so queries
    /// and contacts report the tagged owner. Returns one raw `BodyID` per input row,
    /// position-aligned; a failed row yields [`INVALID_BODY_ID`] and registers nothing.
    pub fn add_static_target_bodies(&mut self, rows: &[StaticTargetBodyCreate]) -> Vec<u32> {
        let creates: Vec<BodyCreate> = rows
            .iter()
            .map(|row| BodyCreate {
                shape: shape_raw(row.shape),
                half_extents: row.half_extents.to_array(),
                offset: [0.0; 3],
                position: row.position.to_array(),
                rotation: row.rotation.to_array(),
                motion: MotionType::Static.raw(),
                object_layer: if row.sensor {
                    ObjectLayer::Sensor
                } else {
                    ObjectLayer::Static
                }
                .raw(),
                is_sensor: row.sensor,
                friction: row.friction,
                restitution: 0.0,
                linear_damping: 0.0,
                angular_damping: 0.0,
                gravity_factor: 1.0,
                mass: 1.0,
                allowed_dofs: ALLOWED_DOFS_ALL,
            })
            .collect();
        let ids = sys::create_static_batch(&mut self.world, &creates);
        for (row, &id) in rows.iter().zip(&ids) {
            if id == INVALID_BODY_ID {
                continue;
            }
            self.index_by_body_id.insert(id, self.bodies.len());
            self.bodies.push(BodyEntry {
                entity: saffron_scene::Entity::NULL,
                target: row.target,
                id,
                motion: MotionType::Static,
                sensor: row.sensor,
            });
        }
        ids
    }

    /// Remove and destroy the listed bodies in one batch, dropping their registry rows.
    /// [`INVALID_BODY_ID`] sentinels are skipped. A contact already buffered for a removed body
    /// drains with a `None` target.
    pub fn remove_bodies(&mut self, ids: &[u32]) {
        let removed: HashSet<u32> = ids
            .iter()
            .copied()
            .filter(|&id| id != INVALID_BODY_ID)
            .collect();
        if removed.is_empty() {
            return;
        }
        sys::remove_bodies(&mut self.world, ids);
        self.dynamic_body_count -= self
            .bodies
            .iter()
            .filter(|entry| removed.contains(&entry.id) && entry.motion == MotionType::Dynamic)
            .count() as i32;
        self.bodies.retain(|entry| !removed.contains(&entry.id));
        self.index_by_body_id.clear();
        for (index, entry) in self.bodies.iter().enumerate() {
            self.index_by_body_id.insert(entry.id, index);
        }
    }

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

        // Write each Dynamic body's world pose back into its entity's LOCAL Transform. v1 scopes
        // bodies to root entities (world == local); the parented-body local rebase is later. The
        // rotation is stored as the Euler the Transform's convention round-trips the quaternion to.
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

    /// A summary of the live world. `active` is always `true` from a live world (the
    /// `Option<World>` is the host's), kept for the wire DTO shape.
    #[must_use]
    pub fn stats(&self) -> WorldStats {
        WorldStats {
            active: true,
            body_count: i32::try_from(sys::world_body_count(&self.world)).unwrap_or(i32::MAX),
            dynamic_count: self.dynamic_body_count,
        }
    }

    /// Every tracked body's read-only snapshot, in creation order.
    #[must_use]
    pub fn list_bodies(&self) -> Vec<BodyInfo> {
        self.bodies
            .iter()
            .map(|entry| BodyInfo {
                target: Some(entry.target),
                motion: entry.motion,
                active: sys::body_is_active(&self.world, entry.id),
                position: Vec3::from_array(sys::body_position(&self.world, entry.id)),
            })
            .collect()
    }

    /// The live world's moving emitters for the cosmetic interaction field: every
    /// awake dynamic body and every character as (world position, world velocity).
    pub fn motion_emitters(&self) -> Vec<(Vec3, Vec3)> {
        let mut emitters = Vec::new();
        for entry in &self.bodies {
            if entry.motion != MotionType::Dynamic || !sys::body_is_active(&self.world, entry.id) {
                continue;
            }
            emitters.push((
                Vec3::from_array(sys::body_position(&self.world, entry.id)),
                Vec3::from_array(sys::body_linear_velocity(&self.world, entry.id)),
            ));
        }
        for entry in &self.characters {
            emitters.push((
                Vec3::from_array(sys::character_position(&self.world, entry.index)),
                entry.last_velocity,
            ));
        }
        emitters
    }

    /// Apply a center-of-mass impulse to the Dynamic body owned by `entity`. A non-Dynamic /
    /// unmapped target is a no-op with a warning (never a panic).
    pub fn apply_impulse(&mut self, entity: Uuid, impulse: Vec3) {
        match self.dynamic_body_id(entity) {
            Some(id) => sys::body_add_impulse(&mut self.world, id, impulse.to_array()),
            None => tracing::warn!(
                "physics: apply-impulse on a non-Dynamic / unmapped body ({})",
                entity.0
            ),
        }
    }

    /// Add a force (applied over the next step) to the Dynamic body owned by `entity`. A
    /// non-Dynamic / unmapped target is a no-op with a warning.
    pub fn add_force(&mut self, entity: Uuid, force: Vec3) {
        match self.dynamic_body_id(entity) {
            Some(id) => sys::body_add_force(&mut self.world, id, force.to_array()),
            None => tracing::warn!(
                "physics: add-force on a non-Dynamic / unmapped body ({})",
                entity.0
            ),
        }
    }

    /// Set the linear velocity of the Dynamic body owned by `entity`. A non-Dynamic / unmapped
    /// target is a no-op with a warning.
    pub fn set_linear_velocity(&mut self, entity: Uuid, velocity: Vec3) {
        match self.dynamic_body_id(entity) {
            Some(id) => sys::body_set_linear_velocity(&mut self.world, id, velocity.to_array()),
            None => tracing::warn!(
                "physics: set-velocity on a non-Dynamic / unmapped body ({})",
                entity.0
            ),
        }
    }

    /// The current linear velocity of the Dynamic body owned by `entity`, or zero when there is no
    /// such body.
    #[must_use]
    pub fn body_linear_velocity(&self, entity: Uuid) -> Vec3 {
        match self.dynamic_body_id(entity) {
            Some(id) => Vec3::from_array(sys::body_linear_velocity(&self.world, id)),
            None => Vec3::ZERO,
        }
    }

    /// A dynamic body's current angular velocity (radians per second about each world axis), for
    /// the promotion write-back. Zero for an entity with no dynamic body.
    #[must_use]
    pub fn body_angular_velocity(&self, entity: Uuid) -> Vec3 {
        match self.dynamic_body_id(entity) {
            Some(id) => Vec3::from_array(sys::body_angular_velocity(&self.world, id)),
            None => Vec3::ZERO,
        }
    }

    /// Cast a ray `origin + dir * max_dist` against the live world and return the closest hit,
    /// mapped back to its owner entity. Read-only: it takes `&self` so it cannot perturb the
    /// deterministic step — run it between steps (a command, or `on_update`), never mid-solve.
    /// `dir` is taken as supplied (not normalized): the hit `distance` is `fraction * max_dist` in
    /// `dir` units. A ray into empty space returns [`RayHit::default`] (`hit == false`).
    #[must_use]
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32) -> RayHit {
        let hit = sys::raycast(&self.world, origin.to_array(), dir.to_array(), max_dist);
        self.map_ray_hit(hit)
    }

    /// Sweep a sphere of `radius` along `origin + dir * max_dist` against the live world — a
    /// thicker probe than [`World::raycast`], so it catches an edge a thin ray of the same
    /// origin/dir grazes — and return the closest hit mapped back to its owner entity. Read-only
    /// (`&self`). A sweep that clears everything returns [`RayHit::default`].
    #[must_use]
    pub fn sphere_cast(&self, origin: Vec3, dir: Vec3, radius: f32, max_dist: f32) -> RayHit {
        let hit = sys::sphere_cast(
            &self.world,
            origin.to_array(),
            dir.to_array(),
            radius,
            max_dist,
        );
        self.map_ray_hit(hit)
    }

    /// Convert a `-sys` [`sys::RayHit`] (POD with a raw `BodyID`) into the public [`RayHit`],
    /// mapping the struck body back to its owner entity uuid via `index_by_body_id`. An unmapped
    /// body (or a miss) yields `Uuid(0)`.
    fn map_ray_hit(&self, hit: sys::RayHit) -> RayHit {
        if !hit.hit {
            return RayHit::default();
        }
        RayHit {
            hit: true,
            target: self.body_target(hit.body),
            point: Vec3::from_array(hit.point),
            normal: Vec3::from_array(hit.normal),
            distance: hit.distance,
        }
    }

    /// Map a raw `BodyID` back to its tagged owner (`None` for an unmapped body). The query
    /// hits return a raw Jolt `BodyID`; the safe layer owns the body → target registry.
    fn body_target(&self, id: u32) -> Option<crate::WorldHitTarget> {
        self.index_by_body_id
            .get(&id)
            .map(|&i| self.bodies[i].target)
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

    /// The raw `BodyID` of the Dynamic body owned by `uuid`, or `None`. Impulses/velocity apply
    /// only to Dynamic bodies — a Static/Kinematic one would silently ignore them, so it is
    /// excluded here.
    fn dynamic_body_id(&self, uuid: Uuid) -> Option<u32> {
        self.bodies
            .iter()
            .find(|e| {
                e.target == crate::WorldHitTarget::SceneEntity(uuid)
                    && e.motion == MotionType::Dynamic
            })
            .map(|e| e.id)
    }
}

/// Auto-fit an entity's [`Collider`] to its mesh AABB, baking the entity's world scale into the
/// fitted extents/offset so the scale-free Jolt body matches the scaled visual mesh. Returns
/// `false` (leaving the collider unchanged) when the entity has no collider, no mesh to size
/// against, or a degenerate (single-point) mesh.
///
/// `cook` reads the entity's `Mesh`/`SkinnedMesh` `.smesh` (the same source cooking uses, so the
/// fit needs no GPU upload and works identically in Edit and headless) — it is the seam that keeps
/// the asset reader out of this crate. The control crate calls this from add-component + the
/// `fit-collider` command.
pub fn fit_collider_to_mesh(scene: &mut Scene, entity: Entity, cook: &mut MeshCook<'_>) -> bool {
    if !scene.has_component::<Collider>(entity) {
        return false;
    }
    // The mesh-bearing entities to size against: the collider entity itself when it carries a
    // mesh, else its forest — the meshes of a multi-node model ride child nodes under the
    // container the collider sits on, so probing only `entity` finds nothing.
    let mesh_entities: Vec<Entity> = if scene.has_component::<MeshComponent>(entity)
        || scene.has_component::<SkinnedMesh>(entity)
    {
        vec![entity]
    } else {
        scene.model_mesh_entities(entity)
    };
    if mesh_entities.is_empty() {
        return false; // no mesh to size against — keep the collider's defaults
    }

    // The Jolt body is built scale-free (world translation + rotation only). Union every mesh's
    // AABB in world space, then express it in the body's local frame `inv(T·R)`; for a single
    // mesh on the collider entity this reduces to the mesh-local box scaled by the entity's world
    // scale (since `inv(T·R) · (T·R·S) = S`), matching the prior single-mesh fit exactly.
    let body = scene.world_matrix(entity);
    let (_, body_rot, body_pos) = body.to_scale_rotation_translation();
    let to_body = Mat4::from_rotation_translation(body_rot, body_pos).inverse();
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    let mut source_mesh = Uuid(0);
    let mut found = false;
    for mesh_entity in mesh_entities {
        let mesh_id = scene
            .with_component::<MeshComponent, _>(mesh_entity, |m| m.mesh)
            .ok()
            .or_else(|| {
                scene
                    .with_component::<SkinnedMesh, _>(mesh_entity, |s| s.mesh)
                    .ok()
            })
            .unwrap_or(Uuid(0));
        if mesh_id.0 == 0 {
            continue;
        }
        let Ok(mesh) = cook(mesh_id) else {
            continue;
        };
        let Some((mlo, mhi)) = mesh_aabb(&mesh) else {
            continue;
        };
        if source_mesh.0 == 0 {
            source_mesh = mesh_id;
        }
        let to_local = to_body * scene.world_matrix(mesh_entity);
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { mlo.x } else { mhi.x },
                if i & 2 == 0 { mlo.y } else { mhi.y },
                if i & 4 == 0 { mlo.z } else { mhi.z },
            );
            let p = to_local.transform_point3(corner);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        found = true;
    }
    if !found {
        return false;
    }
    if (hi - lo).cmple(Vec3::ZERO).all() {
        return false; // a single degenerate point — nothing to size against (a planar mesh is fine)
    }

    let half = (hi - lo) * 0.5;
    let offset = (lo + hi) * 0.5;
    let half_extents = match scene.with_component::<Collider, _>(entity, |c| c.shape) {
        Ok(Shape::Box | Shape::ConvexHull | Shape::Mesh) => {
            // Hull/mesh fit a fallback box into half_extents; the cook uses the actual geometry.
            half
        }
        Ok(Shape::Sphere) => {
            // Bounding sphere of the box (never smaller than the mesh); radius packed in .x.
            Vec3::splat(half.x.max(half.y).max(half.z))
        }
        Ok(Shape::Capsule) => {
            // Y-up capsule: long axis = Y, radius = the larger of X/Z, half-height excludes the caps.
            let radius = half.x.max(half.z);
            let half_height = (half.y - radius).max(0.0);
            Vec3::new(radius, half_height, radius)
        }
        Err(_) => return false,
    };
    scene
        .with_component_mut::<Collider, _>(entity, |c| {
            c.offset = offset;
            c.source_mesh = source_mesh; // cook source for hull/mesh; analytic shapes ignore it
            c.half_extents = half_extents;
        })
        .is_ok()
}

/// Size each rest-pose bone capsule of a rig's [`BonePhysicsComponent`] from the distance to its
/// farthest child joint, writing the radius/half-height into `shape_half_extents`. Adds an empty
/// `BonePhysicsComponent` (then sizes it) when the rig has none. Returns `false` (unchanged) when
/// the entity has no `SkinnedMesh` or its bone list is empty.
pub fn fit_bone_capsules(scene: &mut Scene, rig: Entity) -> bool {
    let Ok(bone_handles) = scene.with_component::<SkinnedMesh, _>(rig, |s| s.bone_handles.clone())
    else {
        return false;
    };
    let count = bone_handles.len();
    if count == 0 {
        return false;
    }
    if !scene.has_component::<BonePhysicsComponent>(rig) {
        let _ = scene.add_component(rig, BonePhysicsComponent::default());
    }

    // Rest-pose world positions + ids per joint (Edit reads the authored rest skeleton).
    let mut rest_pos = vec![Vec3::ZERO; count];
    let mut uuid = vec![Uuid(0); count];
    for (i, &joint) in bone_handles.iter().enumerate() {
        if scene.valid(joint) {
            rest_pos[i] = scene.world_translation(joint);
            uuid[i] = id_of(scene, joint);
        }
    }

    let mut sized = vec![BonePhysics::default(); count];
    for i in 0..count {
        // Capsule half-height spans toward the farthest child joint; radius a fraction of that.
        let mut length = 0.0f32;
        for (child, &child_joint) in bone_handles.iter().enumerate() {
            if child == i || !scene.valid(child_joint) {
                continue;
            }
            let Ok(parent) = scene.with_component::<Relationship, _>(child_joint, |r| r.parent)
            else {
                continue;
            };
            if uuid[i].0 != 0 && parent == uuid[i] {
                length = length.max((rest_pos[child] - rest_pos[i]).length());
            }
        }
        let half_height = if length > 0.001 { length * 0.5 } else { 0.05 }; // leaf default
        let radius = (half_height * 0.3).max(0.03);
        sized[i].shape_half_extents = Vec3::new(radius, half_height, radius);
    }

    // Preserve any authored per-bone fields (mass/joint/limits/drive) the size pass does not touch:
    // resize to the bone count, then overwrite only `shape_half_extents`.
    scene
        .with_component_mut::<BonePhysicsComponent, _>(rig, |phys| {
            phys.bones.resize(count, BonePhysics::default());
            for (slot, sized) in phys.bones.iter_mut().zip(sized.iter()) {
                slot.shape_half_extents = sized.shape_half_extents;
            }
        })
        .is_ok()
}

/// The axis-aligned bounds of a cooked mesh's vertex positions, or `None` for an empty mesh.
fn mesh_aabb(mesh: &Mesh) -> Option<(Vec3, Vec3)> {
    let first = mesh.vertices.first()?.position;
    let mut lo = first;
    let mut hi = first;
    for vertex in &mesh.vertices {
        lo = lo.min(vertex.position);
        hi = hi.max(vertex.position);
    }
    Some((lo, hi))
}

/// An entity's fresh world position + rotation (scale divided out), composed from the parent chain
/// rather than read from the possibly-stale cached `WorldTransform` — the cache can lag a frame
/// during a sim tick, so the seed pose composes.
fn fresh_world_pose(scene: &Scene, entity: Entity) -> (Vec3, Quat) {
    let (_scale, rotation, translation) = scene
        .compose_world_matrix(entity)
        .to_scale_rotation_translation();
    (translation, rotation)
}

/// An entity's fresh world translation (the composed world matrix's translation), for the
/// character spawn seed.
fn fresh_world_translation(scene: &Scene, entity: Entity) -> Vec3 {
    scene.compose_world_matrix(entity).w_axis.truncate()
}

/// Whether a joint at `index` is driven by a [`KinematicBones`] rig: an empty `driven` list means
/// every joint, otherwise the index must appear in the list.
fn is_driven(driven: &[i32], index: usize) -> bool {
    driven.is_empty() || driven.iter().any(|&w| usize::try_from(w) == Ok(index))
}

/// The raw constraint-kind discriminant the bridge's `BonePart.joint` carries, mapping the scene
/// [`Joint`](saffron_scene::Joint) enum to the shim's switch (`0` Fixed, `1` Hinge, `2` SwingTwist,
/// `3` Free). The shim builds the joint constraint by this discriminant.
fn joint_raw(joint: saffron_scene::Joint) -> u8 {
    match joint {
        saffron_scene::Joint::Fixed => 0,
        saffron_scene::Joint::Hinge => 1,
        saffron_scene::Joint::SwingTwist => 2,
        saffron_scene::Joint::Free => 3,
    }
}

/// Resolve a body's object layer. Precedence: sensor > the moving slot the rigidbody's
/// `collision_layer` selects (0 = Moving, 1 = Character, 2 = Debris) > implicit Static (a lone
/// collider or an explicit Static rigidbody).
fn resolve_object_layer(
    rigidbody: Option<&Rigidbody>,
    motion: MotionType,
    is_sensor: bool,
) -> ObjectLayer {
    if is_sensor {
        return ObjectLayer::Sensor;
    }
    match rigidbody {
        Some(rb) if motion != MotionType::Static => match rb.collision_layer {
            1 => ObjectLayer::Character,
            2 => ObjectLayer::Debris,
            _ => ObjectLayer::Moving, // 0 = default moving; unknown clamps to Moving
        },
        _ => ObjectLayer::Static,
    }
}

/// The body's allowed degrees of freedom from its per-axis position/rotation locks, as the Jolt
/// `EAllowedDOFs` bitmask.
fn allowed_dofs(rb: &Rigidbody) -> u8 {
    let mut dofs = ALLOWED_DOFS_ALL;
    if rb.lock_position.x {
        dofs &= !DOF_TRANSLATION_X;
    }
    if rb.lock_position.y {
        dofs &= !DOF_TRANSLATION_Y;
    }
    if rb.lock_position.z {
        dofs &= !DOF_TRANSLATION_Z;
    }
    if rb.lock_rotation.x {
        dofs &= !DOF_ROTATION_X;
    }
    if rb.lock_rotation.y {
        dofs &= !DOF_ROTATION_Y;
    }
    if rb.lock_rotation.z {
        dofs &= !DOF_ROTATION_Z;
    }
    dofs
}

/// The raw shape discriminant the bridge's `BodyCreate.shape` carries, mapping the scene
/// [`Shape`] enum to the shim's switch (`0` Box, `1` Sphere, `2` Capsule, `3` ConvexHull,
/// `4` Mesh). The shim's `Shape` enum is declared in the same order.
fn shape_raw(shape: Shape) -> u8 {
    match shape {
        Shape::Box => 0,
        Shape::Sphere => 1,
        Shape::Capsule => 2,
        Shape::ConvexHull => 3,
        Shape::Mesh => 4,
    }
}

/// An entity's stable id, or `Uuid(0)` when it carries none.
fn id_of(scene: &Scene, entity: Entity) -> Uuid {
    scene
        .component::<IdComponent>(entity)
        .map(|c| c.id)
        .unwrap_or(Uuid(0))
}

/// The cooked geometry a ConvexHull/Mesh body is built from, flattened to the index-ordered slices
/// the bridge feeds Jolt. Empty for the analytic shapes (Box/Sphere/Capsule).
#[derive(Debug, Default)]
pub(crate) struct CookedGeometry {
    /// ConvexHull points, flattened `xyz` in index order.
    hull_points: Vec<f32>,
    /// Mesh vertex positions, flattened `xyz` in index order.
    mesh_vertices: Vec<f32>,
    /// Mesh triangle indices (flat).
    mesh_indices: Vec<u32>,
}

/// Resolve the cooked geometry a collider needs before its Jolt body is built. Analytic shapes
/// need none (an empty [`CookedGeometry`]); ConvexHull/Mesh cook their `source_mesh` through
/// `cook`, feeding vertices/indices in index order so the cooked shape is reproducible run-to-run.
/// A ConvexHull/Mesh with no `source_mesh` is an [`Error::NoCookSource`]; a `Mesh` shape on a
/// Dynamic body is rejected outright (Jolt's `MeshShape` is Static/Kinematic only).
pub(crate) fn cook_shape_geometry(
    collider: &Collider,
    motion: MotionType,
    cook: &mut MeshCook<'_>,
) -> Result<CookedGeometry> {
    match collider.shape {
        Shape::Box | Shape::Sphere | Shape::Capsule => Ok(CookedGeometry::default()),
        Shape::ConvexHull => {
            if collider.source_mesh.0 == 0 {
                return Err(Error::NoCookSource);
            }
            let mesh = cook(collider.source_mesh).map_err(Error::CookFailed)?;
            let mut hull_points = Vec::with_capacity(mesh.vertices.len() * 3);
            for vertex in &mesh.vertices {
                // index order — stable for determinism
                hull_points.extend_from_slice(&vertex.position.to_array());
            }
            if hull_points.is_empty() {
                return Err(Error::CookFailed(
                    "convex-hull source mesh has no vertices".to_owned(),
                ));
            }
            Ok(CookedGeometry {
                hull_points,
                ..CookedGeometry::default()
            })
        }
        Shape::Mesh => {
            if motion == MotionType::Dynamic {
                return Err(Error::MeshShapeOnDynamic);
            }
            if collider.source_mesh.0 == 0 {
                return Err(Error::NoCookSource);
            }
            let mesh = cook(collider.source_mesh).map_err(Error::CookFailed)?;
            let mut mesh_vertices = Vec::with_capacity(mesh.vertices.len() * 3);
            for vertex in &mesh.vertices {
                mesh_vertices.extend_from_slice(&vertex.position.to_array());
            }
            if mesh.indices.len() < 3 {
                return Err(Error::CookFailed("mesh source has no triangles".to_owned()));
            }
            Ok(CookedGeometry {
                mesh_vertices,
                mesh_indices: mesh.indices.clone(),
                ..CookedGeometry::default()
            })
        }
    }
}

/// Flatten a collider + rigidbody into the bridge's `BodyCreate` POD. The damping/mass/DOF fields
/// only matter for a Dynamic body (the shim ignores them otherwise), but they are always filled so
/// the struct is fully initialized.
fn body_create(
    collider: &Collider,
    rigidbody: Option<&Rigidbody>,
    motion: MotionType,
    object_layer: ObjectLayer,
    position: Vec3,
    rotation: Quat,
) -> BodyCreate {
    let dynamic = rigidbody.filter(|_| motion == MotionType::Dynamic);
    BodyCreate {
        shape: shape_raw(collider.shape),
        half_extents: collider.half_extents.to_array(),
        offset: collider.offset.to_array(),
        position: position.to_array(),
        rotation: rotation.to_array(),
        motion: motion.raw(),
        object_layer: object_layer.raw(),
        is_sensor: collider.is_sensor,
        friction: collider.material.friction,
        restitution: collider.material.restitution,
        linear_damping: dynamic.map(|rb| rb.linear_damping).unwrap_or(0.0),
        angular_damping: dynamic.map(|rb| rb.angular_damping).unwrap_or(0.0),
        gravity_factor: dynamic.map(|rb| rb.gravity_factor).unwrap_or(1.0),
        mass: dynamic.map(|rb| rb.mass).unwrap_or(1.0),
        allowed_dofs: dynamic.map(allowed_dofs).unwrap_or(ALLOWED_DOFS_ALL),
    }
}
