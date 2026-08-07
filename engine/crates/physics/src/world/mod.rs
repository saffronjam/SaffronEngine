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

pub(crate) mod bodies;
mod fit;
mod query;
mod ragdoll;
mod step;

use std::collections::{HashMap, VecDeque};

use glam::{Quat, Vec3};

use saffron_core::Uuid;
use saffron_geometry::Mesh;
use saffron_physics_sys::{self as sys, JoltWorld};
use saffron_scene::{Entity, IdComponent, Scene};
use saffron_wind::{LocalWindSource, WindProfile, WindSample};

use crate::error::{Error, Result};
use crate::types::{ContactEvent, MotionType};

pub use fit::{fit_bone_capsules, fit_collider_to_mesh};

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
    /// Reference cross-section in square metres the wind pushes on: the collider's mean
    /// axis-aligned face area scaled by the body's authored wind factor, zero on a body that
    /// authored no aerodynamic coupling. A per-orientation projection would cost a shape query
    /// per body per substep for a force this coarse.
    drag_area: f32,
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
    /// The global wind parameters this world samples, and the local sources composited over
    /// them. A default profile has zero speed, so a world nobody set wind on takes no wind
    /// force at all and steps exactly as it would without the seam.
    wind: WindProfile,
    /// Placed local wind sources, in scene order.
    wind_sources: Vec<LocalWindSource>,
    /// The monotonic simulation clock the field is sampled at, in seconds.
    wind_time_s: f64,
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
            wind: WindProfile {
                speed: 0.0,
                ..WindProfile::default()
            },
            wind_sources: Vec::new(),
            wind_time_s: 0.0,
        })
    }
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

/// An entity's stable id, or `Uuid(0)` when it carries none.
fn id_of(scene: &Scene, entity: Entity) -> Uuid {
    scene
        .component::<IdComponent>(entity)
        .map(|c| c.id)
        .unwrap_or(Uuid(0))
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
