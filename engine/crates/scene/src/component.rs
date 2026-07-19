//! The component structs the world holds.
//!
//! The 24 serialized components plus the runtime-only caches. Vectors use the matching
//! `glam` type, with `Vec3` pinned at 12 bytes (the geometry-area pin) so the downstream
//! std430/byte layouts stay correct.
//!
//! The data-carrying enums (`Wrap`, `Transition`, `Motion`, `Shape`, `Joint`) are plain
//! Rust enums; their wire spelling lives in the serde phase, not on the enum repr. The
//! in-struct member initializers are the `Default` impls — a wrong default silently
//! changes loaded data, so the values here are load-bearing.
//!
//! The runtime-only components (the [`Relationship`] caches, [`WorldTransform`],
//! [`PoseOverride`], [`SkinnedMesh::bone_handles`]) are a distinct set: they never
//! serialize and never copy, encoded by simply not registering them in the registry
//! phase.

use glam::{BVec3, Mat4, Quat, Vec3};
use serde_json::Value;

use saffron_core::Uuid;

use crate::scene::Entity;

/// A human-readable display name for an entity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Name {
    /// The entity's name (UTF-8, editor-editable).
    pub name: String,
}

/// The stable 64-bit identity carried by every authored entity.
///
/// ECS handles are not stable across a load and can alias between worlds, so every
/// cross-entity reference resolves through this id, never through a raw handle. Left
/// unregistered (like [`ComponentOrder`]): the id is written by the document assembler,
/// not by a registered component serializer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct IdComponent {
    /// The entity's stable identity.
    pub id: Uuid,
}

impl IdComponent {
    /// Wraps a freshly minted or loaded id.
    #[must_use]
    pub fn new(id: Uuid) -> Self {
        Self { id }
    }
}

/// Local TRS. Rotation is Euler XYZ in radians — the editor edits these directly, and
/// the world matrix is `T · R · S` with `R` built from the Euler triple.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Local position.
    pub translation: Vec3,
    /// Local scale (per-axis).
    pub scale: Vec3,
    /// Local rotation as Euler XYZ radians.
    pub rotation: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            scale: Vec3::ONE,
            rotation: Vec3::ZERO,
        }
    }
}

/// A node in the scene tree.
///
/// `parent` (a [`Uuid`]; `0` == root) is the only durable field; `parent_handle` and
/// `children` are runtime caches rebuilt by the hierarchy relink after any structural
/// change — never serialized or copied (ECS ids are not stable across a load).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Relationship {
    /// The parent's stable id; `Uuid(0)` means this is a root.
    pub parent: Uuid,
    /// Resolved cache of the parent handle (rebuilt by the relink; never serialized).
    pub parent_handle: Option<Entity>,
    /// Derived cache of child handles (rebuilt by the relink; never serialized).
    pub children: Vec<Entity>,
}

/// The cached world matrix, overwritten each frame by the world-transform update.
///
/// Runtime-only — stays unregistered (like [`IdComponent`]), so entity serialization
/// skips it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldTransform {
    /// The composed world matrix.
    pub matrix: Mat4,
}

impl Default for WorldTransform {
    fn default() -> Self {
        Self {
            matrix: Mat4::IDENTITY,
        }
    }
}

/// The authored order of an entity's component rows, driving the inspector list and the
/// serialized `componentOrder` array. Left unregistered: written by the document
/// assembler, not by a registered component serializer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComponentOrder {
    /// Component names in their authored order.
    pub names: Vec<String>,
}

/// Tags a skeleton joint (set by the glTF skin import) so the outliner can filter bone
/// rows. Serialized as an empty object; a bone is otherwise an ordinary entity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bone {
    /// A single byte of storage so the generic component access can bind a non-empty
    /// type (the ECS elides storage for zero-sized types).
    pub tag: u8,
}

/// Tags an entity (and every node of an instantiated model's subtree) as a not-yet-committed
/// asset-placement preview. Left unregistered: a ghost is never serialized, never listed in the
/// outliner, and never hit by the placement pick — it only renders, so the user sees where the
/// drop will land. Committing the placement removes the tag; the entity becomes ordinary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PreviewGhost {
    /// A single byte of storage so the generic component access can bind a non-empty type
    /// (the ECS elides storage for zero-sized types).
    pub tag: u8,
}

/// A skinned renderable: the mesh asset plus the ordered joint list by uuid.
///
/// `bones[i]` drives `joint_matrices()[i]` through `inverse_bind[i]` — glTF joint order,
/// defined solely by the import. `bone_handles` is a runtime cache rebuilt by the
/// hierarchy relink; like the [`Relationship`] caches it is never serialized or copied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkinnedMesh {
    /// The skinned mesh asset id.
    pub mesh: Uuid,
    /// The root bone's id.
    pub root_bone: Uuid,
    /// The ordered joint ids (glTF skin order).
    pub bones: Vec<Uuid>,
    /// Per-joint inverse-bind matrices (parallel to `bones`).
    pub inverse_bind: Vec<Mat4>,
    /// Resolved cache of the joint handles (rebuilt by the relink; never serialized).
    pub bone_handles: Vec<Entity>,
}

/// How an animation clip wraps at its end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wrap {
    /// Play once and hold the final pose.
    Once,
    /// Restart from the beginning (the default).
    #[default]
    Loop,
    /// Reverse direction at each end.
    PingPong,
}

/// How an active clip switch blends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Transition {
    /// Decay the pose offset (the default).
    #[default]
    Inertialize,
    /// Sustain a two-clip cross-fade.
    CrossFade,
}

/// Drives a skinned rig from an animation clip.
///
/// Dumb data — the evaluator and serde live elsewhere. `autoplay` is the only authored
/// playback intent that persists; `time`, `playing`, `preview_in_edit`, `ping_forward` and
/// the transition trio (`prev_clip`/`transition`/`transition_duration`) are pure runtime
/// state (not serialized). Entering Play resets every player to `time = 0`,
/// `playing = autoplay`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationPlayer {
    /// The animation catalog entry to play.
    pub clip: Uuid,
    /// The deliberate authored auto-play flag: scene Play starts this clip iff set (the
    /// editor Timeline play/pause is a transient preview and never sets this).
    pub autoplay: bool,
    /// Playhead, seconds (runtime — not serialized).
    pub time: f32,
    /// Playback speed multiplier.
    pub speed: f32,
    /// How the clip wraps at its end.
    pub wrap: Wrap,
    /// Whether time advances (runtime — not serialized; the game loop in Play / the timeline
    /// preview in Edit).
    pub playing: bool,
    /// Runtime: is this entity previewed in Edit? (serialize as false).
    pub preview_in_edit: bool,
    /// Runtime: ping-pong direction state.
    pub ping_forward: bool,
    /// How an active clip switch blends.
    pub transition_mode: Transition,
    /// Seconds of wrap-blend for a Loop clip (`0` = hard cut).
    pub loop_blend: f32,
    /// Active-transition state (runtime; idle/`0` at rest).
    pub prev_clip: Uuid,
    /// Active-transition progress (runtime).
    pub transition: f32,
    /// Active-transition duration (runtime).
    pub transition_duration: f32,
}

impl Default for AnimationPlayer {
    fn default() -> Self {
        Self {
            clip: Uuid(0),
            autoplay: false,
            time: 0.0,
            speed: 1.0,
            wrap: Wrap::Loop,
            playing: false,
            preview_in_edit: false,
            ping_forward: true,
            transition_mode: Transition::Inertialize,
            loop_blend: 0.0,
            prev_clip: Uuid(0),
            transition: 0.0,
            transition_duration: 0.0,
        }
    }
}

/// The animated local TRS the evaluator writes onto a driven bone each frame.
///
/// Runtime-only (never serialized). World-transform composition prefers it over the
/// bone's [`Transform`], so the authored rest pose stays untouched and Edit preview is
/// non-destructive. Uses a [`Quat`] directly (no Euler round-trip). Removed from a bone
/// when its rig stops animating (reverts to rest).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PoseOverride {
    /// Overriding local translation.
    pub translation: Vec3,
    /// Overriding local rotation (quaternion, no Euler round-trip).
    pub rotation: Quat,
    /// Overriding local scale.
    pub scale: Vec3,
}

impl Default for PoseOverride {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

/// The per-entity morph (blend-shape) weight vector and the target names that label it.
///
/// Durable and import-managed: seeded at spawn from the mesh's authored rest weights, it
/// round-trips through the registry as `"Morph"`. The weight-vector length must equal the
/// mesh's morph-target count, so the editor treats it as non-addable / non-removable.
/// `weights` are canonical `0..1`. The animation evaluator does not write here — it writes
/// the runtime-only [`MorphWeightOverride`], so a stopped morph reverts to these weights.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphComponent {
    /// One weight per morph target (canonical `0..1`).
    pub weights: Vec<f32>,
    /// One name per morph target, parallel to `weights` (the editor's slider labels).
    pub names: Vec<String>,
}

/// The animated morph weights the evaluator writes each frame.
///
/// Runtime-only (never serialized, never registered — the twin of [`PoseOverride`]); the
/// GPU morph deform reads it, and it is removed when the rig stops animating so the mesh
/// reverts to the durable [`MorphComponent::weights`]. Weights are canonical `0..1`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MorphWeightOverride {
    /// One animated weight per morph target (canonical `0..1`).
    pub weights: Vec<f32>,
}

/// One leg/arm chain for kinematic foot IK.
///
/// `upper`/`mid`/`end` are joint indices into [`SkinnedMesh::bones`]
/// (upper→mid→end, e.g. thigh→shin→foot); `pole_vector` orients the knee plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FootChain {
    /// Upper joint index (`-1` = unset).
    pub upper: i32,
    /// Middle joint index (`-1` = unset).
    pub mid: i32,
    /// End joint index (`-1` = unset).
    pub end: i32,
    /// The pole vector orienting the knee plane.
    pub pole_vector: Vec3,
}

impl Default for FootChain {
    fn default() -> Self {
        Self {
            upper: -1,
            mid: -1,
            end: -1,
            pole_vector: Vec3::new(0.0, 0.0, 1.0),
        }
    }
}

/// Foot-IK config on the rig entity (beside [`SkinnedMesh`]).
///
/// When enabled, the animation evaluator runs a two-bone IK solve per chain and feeds
/// the result through the pose blend layer, never the bones' [`Transform`]s. v1 ground
/// is a horizontal plane at `ground_height`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FootIk {
    /// Whether foot IK is active.
    pub enabled: bool,
    /// The ground plane height (v1: a horizontal plane).
    pub ground_height: f32,
    /// The IK chains to solve.
    pub chains: Vec<FootChain>,
}

/// How a ragdoll joint constrains its bone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Joint {
    /// A rigid weld.
    Fixed,
    /// A 1-DOF hinge.
    Hinge,
    /// A swing-twist cone (the default).
    #[default]
    SwingTwist,
    /// An unconstrained joint.
    Free,
}

/// Reserved per-bone metadata for the eventual Jolt powered-ragdoll.
///
/// No runtime use yet — purely the schema the physics phase reads to build collider
/// bodies and constraints. Authored once, mapped 1:1 later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BonePhysics {
    /// Capsule/box collider half-size.
    pub shape_half_extents: Vec3,
    /// Body mass.
    pub mass: f32,
    /// The joint type constraining this bone.
    pub joint: Joint,
    /// Swing/twist limits (radians).
    pub swing_twist_limits: Vec3,
    /// PD motor drive stiffness.
    pub drive_stiffness: f32,
    /// PD motor drive damping.
    pub drive_damping: f32,
    /// PD motor maximum force.
    pub drive_max_force: f32,
}

impl Default for BonePhysics {
    fn default() -> Self {
        Self {
            shape_half_extents: Vec3::ZERO,
            mass: 1.0,
            joint: Joint::SwingTwist,
            swing_twist_limits: Vec3::ZERO,
            drive_stiffness: 0.0,
            drive_damping: 0.0,
            drive_max_force: 0.0,
        }
    }
}

/// A sidecar on the rig entity: a parallel array to [`SkinnedMesh::bones`] of the
/// reserved per-bone physics metadata. Serialized through the component path; inert.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BonePhysicsComponent {
    /// Per-bone physics metadata (parallel to the rig's `bones`).
    pub bones: Vec<BonePhysics>,
}

/// How the solver treats a simulated body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Never moves (floors/walls).
    Static,
    /// Script/animation-driven (infinite mass, pushes dynamics).
    Kinematic,
    /// Moves under forces (the default).
    #[default]
    Dynamic,
}

/// A simulated body.
///
/// The motion type decides how the solver treats it. A [`Collider`] without a
/// `Rigidbody` is an implicit static body; with one present, this motion type wins.
/// Per-axis locks freeze a DOF on a dynamic body.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rigidbody {
    /// How the solver treats this body.
    pub motion: Motion,
    /// Mass in kg (ignored for static/kinematic).
    pub mass: f32,
    /// Per-second linear velocity decay.
    pub linear_damping: f32,
    /// Per-second angular velocity decay.
    pub angular_damping: f32,
    /// Gravity scale (`0` = float, `1` = full gravity).
    pub gravity_factor: f32,
    /// Freeze X/Y/Z translation.
    pub lock_position: BVec3,
    /// Freeze X/Y/Z rotation.
    pub lock_rotation: BVec3,
    /// Index into the layer table (`0` = the default moving layer).
    pub collision_layer: i32,
}

impl Default for Rigidbody {
    fn default() -> Self {
        Self {
            motion: Motion::Dynamic,
            mass: 1.0,
            linear_damping: 0.05,
            angular_damping: 0.05,
            gravity_factor: 1.0,
            lock_position: BVec3::FALSE,
            lock_rotation: BVec3::FALSE,
            collision_layer: 0,
        }
    }
}

/// Friction/restitution for a collider's surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicsMaterial {
    /// Surface friction (`0` = ice, `1` = rubber).
    pub friction: f32,
    /// Bounciness, `0..1`.
    pub restitution: f32,
}

impl Default for PhysicsMaterial {
    fn default() -> Self {
        Self {
            friction: 0.5,
            restitution: 0.0,
        }
    }
}

/// The collision-geometry shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    /// An axis-aligned box (the default).
    #[default]
    Box,
    /// A sphere.
    Sphere,
    /// A capsule.
    Capsule,
    /// A cooked convex hull.
    ConvexHull,
    /// A triangle mesh.
    Mesh,
}

/// The collision geometry for an entity.
///
/// Dimensions are interpreted per-shape; the box half-extents auto-fit to the entity
/// mesh AABB on add, editable after. `offset` places the shape in the body's local space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Collider {
    /// The collision shape.
    pub shape: Shape,
    /// Box half-size; radius/height packing for other shapes.
    pub half_extents: Vec3,
    /// ConvexHull/Mesh cook source (`0` = none; Box ignores it).
    pub source_mesh: Uuid,
    /// Local-space shape centre offset.
    pub offset: Vec3,
    /// The surface material.
    pub material: PhysicsMaterial,
    /// Trigger volume: reports overlaps, no contact response.
    pub is_sensor: bool,
}

impl Default for Collider {
    fn default() -> Self {
        Self {
            shape: Shape::Box,
            half_extents: Vec3::splat(0.5),
            source_mesh: Uuid(0),
            offset: Vec3::ZERO,
            material: PhysicsMaterial::default(),
            is_sensor: false,
        }
    }
}

/// Opt a [`SkinnedMesh`] rig into kinematic-bone physics.
///
/// Each driven joint gets a kinematic body that follows the animated pose each step, so
/// a moving character shoves the world (no pose write-back). Per-bone collider sizes come
/// from [`BonePhysicsComponent::bones`], auto-fit on add.
#[derive(Clone, Debug, PartialEq)]
pub struct KinematicBones {
    /// Whether the kinematic bodies are active.
    pub enabled: bool,
    /// Indices into [`SkinnedMesh::bones`]; empty = every joint.
    pub driven: Vec<i32>,
}

impl Default for KinematicBones {
    fn default() -> Self {
        Self {
            enabled: true,
            driven: Vec::new(),
        }
    }
}

/// Marks an entity as a walking capsule character driven by a Jolt `CharacterVirtual`.
///
/// The capsule is the entity's [`Collider`] (`Shape::Capsule`) — this carries only
/// movement params. The runtime state (`desired_velocity`/`vertical_velocity`/`on_ground`)
/// serializes at zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterController {
    /// Horizontal walk-speed cap, m/s.
    pub max_speed: f32,
    /// Maximum walkable ground angle (radians); steeper is a wall.
    pub max_slope_angle: f32,
    /// Ledges/stairs up to this height are stepped over.
    pub max_step_height: f32,
    /// Scales world gravity applied each step.
    pub gravity_factor: f32,
    /// Runtime: the desired horizontal velocity (serialize as zero).
    pub desired_velocity: Vec3,
    /// Runtime: the integrated vertical velocity (serialize as zero).
    pub vertical_velocity: f32,
    /// Runtime: the last step's ground state (serialize as false).
    pub on_ground: bool,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            max_speed: 4.0,
            // The literal `0.785398` (~45°), not `FRAC_PI_4`, so a loaded default reads
            // byte-identically.
            #[allow(clippy::approx_constant)]
            max_slope_angle: 0.785_398,
            max_step_height: 0.3,
            gravity_factor: 1.0,
            desired_velocity: Vec3::ZERO,
            vertical_velocity: 0.0,
            on_ground: false,
        }
    }
}

/// References a mesh asset by stable id; the asset server resolves it to a GPU mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mesh {
    /// The mesh asset id.
    pub mesh: Uuid,
}

/// One material binding for a submesh: a reference to a `.smat` material asset plus a
/// sparse per-object override map applied over the referenced material's resolved params.
///
/// `material == 0` resolves to the built-in default material. `overrides` is opaque,
/// editor-shaped JSON `{ paramName: value }` (empty in the common case); the engine applies
/// only the recognized exposed parameters at resolve time (see the assets crate's material
/// schema). This is the single per-object material shape — a single-material mesh is a
/// [`MaterialSet`] with one slot.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialSlot {
    /// The referenced `.smat` material asset id (`0` = the built-in default material).
    pub material: Uuid,
    /// Sparse `{ paramName: value }` overrides applied over the referenced material.
    /// Opaque editor-shaped JSON, defaulted to an empty object `{}`.
    pub overrides: Value,
}

impl Default for MaterialSlot {
    fn default() -> Self {
        Self {
            material: Uuid(0),
            overrides: Value::Object(serde_json::Map::new()),
        }
    }
}

/// An entity's material bindings, one slot per submesh.
///
/// Each submesh's `material_slot` indexes `slots` (clamped to the slot count); a slot
/// references a `.smat` material asset and layers sparse per-object overrides on top. This
/// is the single per-entity material component — a single-material mesh is one slot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialSet {
    /// The ordered material slots, indexed by submesh `material_slot`.
    pub slots: Vec<MaterialSlot>,
}

/// Marks an entity (the root of an expanded model) as an instance of a `.smodel` asset.
///
/// Lets the editor show it as a model instance and lets reimport find live instances.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModelInstance {
    /// The `.smodel` asset id.
    pub model_id: Uuid,
}

/// One script attached to an entity: a `.lua` path relative to the project `src/` plus
/// per-instance field overrides (filled by the editor; empty until then).
///
/// `overrides` is opaque JSON passed through verbatim — the editor fills it; the engine
/// never interprets it. Defaulted to an empty object `{}`.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptSlot {
    /// The `.lua` path relative to the project `src/`.
    pub script_path: String,
    /// Opaque per-instance field overrides (defaulted to `{}`).
    pub overrides: Value,
}

impl Default for ScriptSlot {
    fn default() -> Self {
        Self {
            script_path: String::new(),
            overrides: Value::Object(serde_json::Map::new()),
        }
    }
}

/// An entity's scripts, run top-to-bottom each play tick.
///
/// Multiple scripts per entity is this vector, never two components. Data only — the
/// Lua runtime lives in the script crate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    /// The ordered scripts.
    pub scripts: Vec<ScriptSlot>,
}

/// A perspective camera; its view comes from the entity's [`Transform`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// Near clip plane.
    pub near_plane: f32,
    /// Far clip plane.
    pub far_plane: f32,
    /// The scene renders through the first primary camera.
    pub primary: bool,
    /// Whether the camera's placeholder model shows in Edit.
    pub show_model: bool,
    /// Whether the camera frustum draws in Edit.
    pub show_frustum: bool,
    /// The drawn frustum's far extent.
    pub frustum_max_distance: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov: 45.0,
            near_plane: 0.1,
            far_plane: 100.0,
            primary: true,
            show_model: true,
            show_frustum: true,
            frustum_max_distance: 10.0,
        }
    }
}

/// A directional light's role in the atmosphere model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AtmosphereRole {
    /// Drives the sky-view LUT, solar disc, and primary directional light.
    #[default]
    Sun,
    /// Drives the lunar disc and the secondary directional light.
    Moon,
}

/// A directional light. Its atmosphere role selects the solar or lunar light slot.
///
/// `direction` points the way the light travels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionalLight {
    /// The celestial light slot this light drives.
    pub atmosphere_role: AtmosphereRole,
    /// The direction the light travels.
    pub direction: Vec3,
    /// Light color.
    pub color: Vec3,
    /// Light intensity.
    pub intensity: f32,
    /// Ambient contribution.
    pub ambient: f32,
    /// Per-light fog in-scatter multiplier (the light's shaft brightness in volumetric fog).
    pub volumetric_scattering: f32,
    /// Whether the light's shadow gates its in-scatter in volumetric fog (god-rays).
    pub cast_volumetric_shadow: bool,
}

impl DirectionalLight {
    /// The canonical sun aim: the default light's travel direction, and the placeholder
    /// direction the renderer uses to keep sky/LUT math finite when a scene has no
    /// directional light (at zero intensity it contributes nothing).
    pub const DEFAULT_DIRECTION: Vec3 = Vec3::new(-0.5, -1.0, -0.3);
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            atmosphere_role: AtmosphereRole::Sun,
            direction: Self::DEFAULT_DIRECTION,
            color: Vec3::ONE,
            intensity: 1.0,
            ambient: 0.15,
            volumetric_scattering: 1.0,
            cast_volumetric_shadow: true,
        }
    }
}

/// An omnidirectional light positioned at the entity's [`Transform`] translation, with
/// smooth distance falloff out to `range`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointLight {
    /// Light color.
    pub color: Vec3,
    /// Light intensity.
    pub intensity: f32,
    /// Falloff range.
    pub range: f32,
    /// Per-light fog in-scatter multiplier (the light's shaft brightness in volumetric fog).
    pub volumetric_scattering: f32,
    /// Whether the light's shadow gates its in-scatter in volumetric fog (god-rays).
    pub cast_volumetric_shadow: bool,
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 5.0,
            range: 10.0,
            volumetric_scattering: 1.0,
            cast_volumetric_shadow: true,
        }
    }
}

/// A cone light at the entity's [`Transform`] translation, aimed by `direction`.
///
/// Falls off by distance (`range`) and by angle between `inner_angle` and `outer_angle`
/// (degrees).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotLight {
    /// The direction the cone aims.
    pub direction: Vec3,
    /// Light color.
    pub color: Vec3,
    /// Light intensity.
    pub intensity: f32,
    /// Falloff range.
    pub range: f32,
    /// Full intensity inside this half-angle (degrees).
    pub inner_angle: f32,
    /// Zero past this half-angle (degrees).
    pub outer_angle: f32,
    /// Per-light fog in-scatter multiplier (the light's shaft brightness in volumetric fog).
    pub volumetric_scattering: f32,
    /// Whether the light's shadow gates its in-scatter in volumetric fog (god-rays).
    pub cast_volumetric_shadow: bool,
}

impl Default for SpotLight {
    fn default() -> Self {
        Self {
            direction: Vec3::new(0.0, -1.0, 0.0),
            color: Vec3::ONE,
            intensity: 5.0,
            range: 10.0,
            inner_angle: 20.0,
            outer_angle: 30.0,
            volumetric_scattering: 1.0,
            cast_volumetric_shadow: true,
        }
    }
}

/// A reflection probe at the entity's [`Transform`] translation.
///
/// Captures a local cubemap, prefilters it like the global IBL, and supplies specular
/// ambient to meshes inside `influence_radius`. `box_projection` re-projects the
/// reflection ray against the influence box for parallax-correct local reflections.
/// `dirty` is runtime-only (capture pending).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReflectionProbe {
    /// Sphere of effect around the probe origin.
    pub influence_radius: f32,
    /// Probe specular multiplier.
    pub intensity: f32,
    /// Parallax-correct against the influence box.
    pub box_projection: bool,
    /// Half-extents for box projection (used when `box_projection`).
    pub box_extent: Vec3,
    /// Capture pending; set on add/edit, cleared after capture (runtime only).
    pub dirty: bool,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            influence_radius: 10.0,
            intensity: 1.0,
            box_projection: false,
            box_extent: Vec3::splat(10.0),
            dirty: true,
        }
    }
}

/// The bounds primitive a [`FogVolume`] injects density within. Deliberately just the two closed
/// primitives the froxel injection can evaluate — a box (oriented half-extents) or a sphere — so the
/// Inspector picker and the injection loop stay exhaustive with no unsupported case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FogShape {
    /// Oriented box, bounded by `extents` (local-space half-extents).
    #[default]
    Box,
    /// Sphere of `radius` around the entity origin.
    Sphere,
}

/// A placeable box/sphere of local participating media, injected into the froxel fog grid during
/// density evaluation. Positioned by the entity's [`Transform`]; `extents` are box half-extents in
/// local space, `radius` is the sphere radius. It contributes to the *same* `sigma_t`/`sigma_s` the
/// analytic height base and global density write, so a local volume is lit, shadowed, temporally
/// reprojected, and integrated by the identical stages — there is no special-case local-fog path.
/// All optical fields default to a light, drifting-off (`noise_intensity: 0`) static haze.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogVolume {
    /// Box or sphere bounds.
    pub shape: FogShape,
    /// Box half-extents (local space).
    pub extents: Vec3,
    /// Sphere radius.
    pub radius: f32,
    /// Soft-edge width (world units) — density smoothsteps to zero over this margin inside the bound.
    pub edge_falloff: f32,
    /// `sigma_t` at the full interior.
    pub density: f32,
    /// Single-scatter albedo (`sigma_s = albedo * sigma_t`).
    pub albedo: Vec3,
    /// Added in-scatter radiance.
    pub emissive: Vec3,
    /// Henyey-Greenstein anisotropy (-1..1).
    pub phase_g: f32,
    /// Per-volume exponential height slab (0 = uniform).
    pub height_falloff: f32,
    /// World→noise frequency.
    pub noise_scale: f32,
    /// Erosion strength (0 = off, 1 = full).
    pub noise_intensity: f32,
    /// Detail-octave weight (subtractive erosion).
    pub noise_detail: f32,
    /// Advection direction.
    pub wind: Vec3,
    /// Advection speed (`wind * speed * time`).
    pub speed: f32,
}

impl Default for FogVolume {
    fn default() -> Self {
        Self {
            shape: FogShape::Box,
            extents: Vec3::splat(5.0),
            radius: 5.0,
            edge_falloff: 1.0,
            density: 0.5,
            albedo: Vec3::splat(0.9),
            emissive: Vec3::ZERO,
            phase_g: 0.0,
            height_falloff: 0.0,
            noise_scale: 0.2,
            noise_intensity: 0.0,
            noise_detail: 0.5,
            wind: Vec3::ZERO,
            speed: 0.1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_defaults() {
        let t = Transform::default();
        assert_eq!(t.translation, Vec3::ZERO);
        assert_eq!(t.scale, Vec3::ONE);
        assert_eq!(t.rotation, Vec3::ZERO);
    }

    #[test]
    fn relationship_default_is_root_with_empty_caches() {
        let r = Relationship::default();
        assert_eq!(r.parent, Uuid(0));
        assert!(r.parent_handle.is_none());
        assert!(r.children.is_empty());
    }

    #[test]
    fn world_transform_default_is_identity() {
        assert_eq!(WorldTransform::default().matrix, Mat4::IDENTITY);
    }

    #[test]
    fn pose_override_default_is_rest() {
        let p = PoseOverride::default();
        assert_eq!(p.translation, Vec3::ZERO);
        assert_eq!(p.rotation, Quat::IDENTITY);
        assert_eq!(p.scale, Vec3::ONE);
    }

    #[test]
    fn animation_player_defaults() {
        let a = AnimationPlayer::default();
        assert_eq!(a.speed, 1.0);
        assert_eq!(a.wrap, Wrap::Loop);
        assert!(!a.autoplay, "autoplay is opt-in (off by default)");
        assert!(!a.playing);
        assert!(!a.preview_in_edit);
        assert!(a.ping_forward);
        assert_eq!(a.transition_mode, Transition::Inertialize);
        assert_eq!(a.loop_blend, 0.0);
    }

    #[test]
    fn enum_defaults_match_cpp() {
        assert_eq!(Wrap::default(), Wrap::Loop);
        assert_eq!(Transition::default(), Transition::Inertialize);
        assert_eq!(Motion::default(), Motion::Dynamic);
        assert_eq!(Shape::default(), Shape::Box);
        assert_eq!(Joint::default(), Joint::SwingTwist);
    }

    #[test]
    fn foot_chain_defaults() {
        let c = FootChain::default();
        assert_eq!(c.upper, -1);
        assert_eq!(c.mid, -1);
        assert_eq!(c.end, -1);
        assert_eq!(c.pole_vector, Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn bone_physics_defaults() {
        let b = BonePhysics::default();
        assert_eq!(b.shape_half_extents, Vec3::ZERO);
        assert_eq!(b.mass, 1.0);
        assert_eq!(b.joint, Joint::SwingTwist);
        assert_eq!(b.drive_stiffness, 0.0);
        assert_eq!(b.drive_damping, 0.0);
        assert_eq!(b.drive_max_force, 0.0);
    }

    #[test]
    fn rigidbody_defaults() {
        let r = Rigidbody::default();
        assert_eq!(r.motion, Motion::Dynamic);
        assert_eq!(r.mass, 1.0);
        assert_eq!(r.linear_damping, 0.05);
        assert_eq!(r.angular_damping, 0.05);
        assert_eq!(r.gravity_factor, 1.0);
        assert_eq!(r.lock_position, BVec3::FALSE);
        assert_eq!(r.lock_rotation, BVec3::FALSE);
        assert_eq!(r.collision_layer, 0);
    }

    #[test]
    fn physics_material_defaults() {
        let m = PhysicsMaterial::default();
        assert_eq!(m.friction, 0.5);
        assert_eq!(m.restitution, 0.0);
    }

    #[test]
    fn collider_defaults() {
        let c = Collider::default();
        assert_eq!(c.shape, Shape::Box);
        assert_eq!(c.half_extents, Vec3::splat(0.5));
        assert_eq!(c.source_mesh, Uuid(0));
        assert_eq!(c.offset, Vec3::ZERO);
        assert_eq!(c.material, PhysicsMaterial::default());
        assert!(!c.is_sensor);
    }

    #[test]
    fn kinematic_bones_defaults() {
        let k = KinematicBones::default();
        assert!(k.enabled);
        assert!(k.driven.is_empty());
    }

    #[test]
    fn character_controller_defaults() {
        let c = CharacterController::default();
        assert_eq!(c.max_speed, 4.0);
        #[allow(clippy::approx_constant)]
        let expected_slope = 0.785_398_f32;
        assert_eq!(c.max_slope_angle, expected_slope);
        assert_eq!(c.max_step_height, 0.3);
        assert_eq!(c.gravity_factor, 1.0);
        assert_eq!(c.desired_velocity, Vec3::ZERO);
        assert_eq!(c.vertical_velocity, 0.0);
        assert!(!c.on_ground);
    }

    #[test]
    fn material_slot_defaults_to_the_default_material_with_empty_overrides() {
        let s = MaterialSlot::default();
        assert_eq!(s.material, Uuid(0));
        assert_eq!(s.overrides, Value::Object(serde_json::Map::new()));
    }

    #[test]
    fn material_set_defaults_to_no_slots() {
        assert!(MaterialSet::default().slots.is_empty());
    }

    #[test]
    fn camera_defaults() {
        let c = Camera::default();
        assert_eq!(c.fov, 45.0);
        assert_eq!(c.near_plane, 0.1);
        assert_eq!(c.far_plane, 100.0);
        assert!(c.primary);
        assert!(c.show_model);
        assert!(c.show_frustum);
        assert_eq!(c.frustum_max_distance, 10.0);
    }

    #[test]
    fn directional_light_defaults() {
        let d = DirectionalLight::default();
        assert_eq!(d.atmosphere_role, AtmosphereRole::Sun);
        assert_eq!(d.direction, Vec3::new(-0.5, -1.0, -0.3));
        assert_eq!(d.color, Vec3::ONE);
        assert_eq!(d.intensity, 1.0);
        assert_eq!(d.ambient, 0.15);
    }

    #[test]
    fn point_light_defaults() {
        let p = PointLight::default();
        assert_eq!(p.color, Vec3::ONE);
        assert_eq!(p.intensity, 5.0);
        assert_eq!(p.range, 10.0);
    }

    #[test]
    fn spot_light_defaults() {
        let s = SpotLight::default();
        assert_eq!(s.direction, Vec3::new(0.0, -1.0, 0.0));
        assert_eq!(s.color, Vec3::ONE);
        assert_eq!(s.intensity, 5.0);
        assert_eq!(s.range, 10.0);
        assert_eq!(s.inner_angle, 20.0);
        assert_eq!(s.outer_angle, 30.0);
    }

    #[test]
    fn reflection_probe_defaults() {
        let r = ReflectionProbe::default();
        assert_eq!(r.influence_radius, 10.0);
        assert_eq!(r.intensity, 1.0);
        assert!(!r.box_projection);
        assert_eq!(r.box_extent, Vec3::splat(10.0));
        assert!(r.dirty);
    }

    #[test]
    fn bone_carries_one_byte_of_storage() {
        // The ECS elides zero-sized types from generic access; the byte tag keeps the
        // type bindable. Its default is `tag = 0`.
        assert_eq!(Bone::default().tag, 0);
        assert_eq!(std::mem::size_of::<Bone>(), 1);
    }

    #[test]
    fn script_slot_default_overrides_is_empty_object() {
        let s = ScriptSlot::default();
        assert!(s.script_path.is_empty());
        assert_eq!(s.overrides, Value::Object(serde_json::Map::new()));
        assert!(s.overrides.is_object());
    }

    #[test]
    fn vec3_is_twelve_bytes() {
        // The geometry-area pin: `Vec3` is 12 bytes (never `Vec3A`), so downstream
        // std430/file byte layouts stay correct.
        assert_eq!(std::mem::size_of::<Vec3>(), 12);
    }
}
