//! The Jolt-free POD vocabulary the world surfaces.
//!
//! `MotionType` mirrors Jolt's `EMotionType` 1:1 and is the raw discriminant the bridge's
//! `BodyCreate.motion` carries; `ObjectLayer` is the fixed v1 layer set whose raw discriminant
//! keys the collision matrix. The result structs (`WorldStats`, `BodyInfo`, `RayHit`) are the
//! read-only snapshots the control plane returns.

use glam::Vec3;

use saffron_animation::JointPose;
use saffron_core::Uuid;

/// The tagged world target a physics interaction resolves to. Every query, contact,
/// and body snapshot names its subject through this one type: a scene entity by stable
/// uuid, or an authoritative macro plant by [`PlantId`] — never a forged uuid, never a
/// truncated id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorldHitTarget {
    /// A hecs scene entity, by its stable uuid.
    SceneEntity(Uuid),
    /// An authoritative macro plant, by its full 128-bit identity.
    Vegetation(saffron_spatial::PlantId),
}

/// The deterministic fixed substep the world advances by, matching SceneEdit's `PlayFixedStep`
/// (`1/60`). The accumulator advances the sim in fixed increments so it is frame-rate independent
/// and stays bit-exact under the cross-platform-deterministic build.
pub const FIXED_STEP: f32 = 1.0 / 60.0;

/// One batched static/sensor body row created directly against a tagged world target (a
/// vegetation collision proxy) rather than derived from a scene entity's components. Analytic
/// shapes only: Box half-extents in `half_extents`, Sphere radius in `.x`, Capsule radius `.x` +
/// cylinder half-height `.y`; a cooked-geometry shape row yields the invalid-id sentinel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StaticTargetBodyCreate {
    /// The tagged owner every query and contact on the body reports.
    pub target: WorldHitTarget,
    /// Analytic shape family.
    pub shape: saffron_scene::Shape,
    /// Per-shape size, in the `Collider` convention.
    pub half_extents: Vec3,
    /// World-space position.
    pub position: Vec3,
    /// World-space rotation.
    pub rotation: glam::Quat,
    /// Overlap-only trigger body: queries and contact events report it, the solver never
    /// pushes against it.
    pub sensor: bool,
    /// Surface friction.
    pub friction: f32,
}

/// How a body participates in the simulation. Mirrors Jolt `EMotionType` 1:1 and is the raw
/// discriminant the bridge carries.
///
/// A [`Collider`](saffron_scene::Collider) without a [`Rigidbody`](saffron_scene::Rigidbody) is an
/// implicit Static body; a present rigidbody's motion wins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum MotionType {
    /// Never moves (floors/walls); the default for a lone collider.
    #[default]
    Static = 0,
    /// Script/animation-driven (infinite mass, pushes dynamics).
    Kinematic = 1,
    /// Moves under forces.
    Dynamic = 2,
}

impl MotionType {
    /// The raw discriminant the bridge's `BodyCreate.motion` field carries.
    #[must_use]
    pub fn raw(self) -> u8 {
        self as u8
    }

    /// Map the scene component's [`Motion`](saffron_scene::Motion) to a Jolt motion type.
    #[must_use]
    pub fn from_scene(motion: saffron_scene::Motion) -> Self {
        match motion {
            saffron_scene::Motion::Static => MotionType::Static,
            saffron_scene::Motion::Kinematic => MotionType::Kinematic,
            saffron_scene::Motion::Dynamic => MotionType::Dynamic,
        }
    }
}

/// The object-layer slots a body lives in. v1 is a fixed set; its raw discriminant keys
/// [`layers_collide`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectLayer {
    /// Immovable world geometry: the implicit layer of a lone collider.
    #[default]
    Static = 0,
    /// Dynamic + kinematic bodies (the default for a rigidbody).
    Moving = 1,
    /// The character controller's body.
    Character = 2,
    /// Dynamic bodies that collide with world/character but not each other.
    Debris = 3,
    /// Trigger volumes: overlap-only, never solved.
    Sensor = 4,
}

impl ObjectLayer {
    /// The raw discriminant the bridge's `BodyCreate.object_layer` field carries.
    #[must_use]
    pub fn raw(self) -> u8 {
        self as u8
    }
}

/// Whether two object layers may collide. Symmetric — the whole v1 collision policy is this table.
/// The same matrix lives in the shim; this copy is the orchestration-side reference (and lets a
/// test pin the policy without the FFI).
#[must_use]
pub fn layers_collide(a: ObjectLayer, b: ObjectLayer) -> bool {
    if a == ObjectLayer::Sensor || b == ObjectLayer::Sensor {
        return !(a == ObjectLayer::Sensor && b == ObjectLayer::Sensor);
    }
    if a == ObjectLayer::Static && b == ObjectLayer::Static {
        return false;
    }
    if a == ObjectLayer::Debris && b == ObjectLayer::Debris {
        return false;
    }
    true
}

/// A summary of the live world, surfaced over the control plane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldStats {
    /// `true` while a world exists. Always `true` from a live [`World`](crate::World) (the
    /// `Option<World>` lives in the host), kept so the wire DTO shape is unchanged.
    pub active: bool,
    /// The live body count (`PhysicsSystem::GetNumBodies`).
    pub body_count: i32,
    /// How many of the tracked bodies are Dynamic.
    pub dynamic_count: i32,
}

/// One live body's read-only snapshot for the editor's physics panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyInfo {
    /// The body's owner (`None` when the body carried no owner identity).
    pub target: Option<WorldHitTarget>,
    /// The body's motion type.
    pub motion: MotionType,
    /// Whether the body is awake.
    pub active: bool,
    /// World-space position.
    pub position: Vec3,
}

/// One ray/shape query hit against the live world.
///
/// Returned by [`World::raycast`](crate::World::raycast) and
/// [`World::sphere_cast`](crate::World::sphere_cast) with the struck body already mapped to its
/// owner entity uuid.
///
/// This is the source POD for the `sa.raycast` / `sa.spherecast` script seam. To keep
/// `saffron-script` free of a `saffron-physics` dependency (it must not import this crate), the
/// host owns the bridge: `saffron-script` declares a `raycast`/`sphere_cast` callback trait, and the
/// host implements it over the live `Option<World>`, calling these two methods and flattening the
/// result into the script-side POD. That conversion is a plain field copy — `hit`, `entity`,
/// `point.x/y/z`, `normal.x/y/z`, `distance`; nothing here reorders or reinterprets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// Whether the ray hit anything.
    pub hit: bool,
    /// The struck body's owner (`None` on a miss or an unowned body).
    pub target: Option<WorldHitTarget>,
    /// World-space contact point.
    pub point: Vec3,
    /// World-space surface normal at the hit.
    pub normal: Vec3,
    /// Distance along the ray from the origin (fraction × max distance).
    pub distance: f32,
}

impl Default for RayHit {
    fn default() -> Self {
        Self {
            hit: false,
            target: None,
            point: Vec3::ZERO,
            normal: Vec3::ZERO,
            distance: 0.0,
        }
    }
}

/// The bounded contact ring's capacity: the oldest event is evicted past this many entries. A stale
/// drain cursor older than the retained tail learns it missed evictions via
/// [`ContactDrain::overflowed`].
pub const CONTACT_RING_CAP: usize = 256;

/// Whether a contact transition began or ended. Sensor overlaps and solid touches share the ring;
/// [`ContactEvent::sensor`] distinguishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactKind {
    /// `OnContactAdded`: the bodies started touching/overlapping.
    Begin,
    /// `OnContactRemoved`: the bodies stopped touching/overlapping.
    End,
}

/// One contact/overlap transition, seq-stamped and drained over a non-blocking cursor. Sensor
/// overlaps and solid touches share one ring; [`sensor`](Self::sensor) distinguishes them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactEvent {
    /// The monotonic sequence number stamped when the event entered the ring (`1`-based).
    pub seq: u64,
    /// Whether the contact began or ended.
    pub kind: ContactKind,
    /// One body's owner (`None` when the body has no owner).
    pub target_a: Option<WorldHitTarget>,
    /// The other body's owner (`None` when none).
    pub target_b: Option<WorldHitTarget>,
    /// Either body is a sensor — a trigger overlap, not a solid touch.
    pub sensor: bool,
    /// A representative world-space contact point; zero for an `End` event.
    pub point: Vec3,
    /// World-space contact normal (`entity_a` → `entity_b`); zero for an `End` event.
    pub normal: Vec3,
    /// The physics step the contact fired on.
    pub tick: i64,
}

/// A rig's per-frame animation target: the post-IK local TRS pose the evaluator produced, indexed
/// 1:1 with the rig's [`SkinnedMesh`](saffron_scene::SkinnedMesh) bones (the same order the ragdoll
/// skeleton was built from). Keyed by the rig mesh entity uuid; drives an active ragdoll's motors
/// toward the animation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PoseTarget {
    /// The rig mesh entity's stable id (the lookup key against each ragdoll's rig).
    pub rig: Uuid,
    /// The animated local TRS per joint, in bone-index order.
    pub local: Vec<JointPose>,
}

/// A rig's live ragdoll state: presence, the motor-active flag, the mean target weight across
/// bones, and the bone count. All-default (absent) when the rig has no ragdoll.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RagdollState {
    /// `true` when the rig has a live ragdoll instance this play session.
    pub present: bool,
    /// `true` when the ragdoll's motors are driving toward the animation (active vs passive).
    pub active: bool,
    /// The mean per-bone target weight (`0` = pure animation, `1` = pure physics).
    pub body_weight: f32,
    /// The ragdoll's bone count.
    pub bones: i32,
}

/// A snapshot of contact events with `seq > since` (non-blocking), plus cursor metadata that lets a
/// stale cursor detect it missed evicted events — the same drain-cursor shape the alarms /
/// script-errors rings use.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContactDrain {
    /// The events newer than the cursor, in seq order.
    pub events: Vec<ContactEvent>,
    /// The highest seq the ring has ever stamped (the cursor to pass next drain).
    pub high_water_seq: u64,
    /// The lowest seq still retained in the ring (`0` when empty).
    pub oldest_seq: u64,
    /// `true` when the cursor is older than the oldest retained event, so it missed evictions and
    /// should resync.
    pub overflowed: bool,
}
