//! The declarative `sa.*` binding-descriptor table — the single source that both registers the API
//! with the VM and feeds the Luau type emitter. A binding's argument and return types are first-class
//! Rust data, so there is no second hand-written copy to drift.
//!
//! The `sa.Entity` methods live on the [`EntityHandle`] userdata rather than in this table. The xtask
//! emitter reads [`BINDINGS`] through the shared `map_type` mapper, so the type tokens here are
//! spelled in that mapper's wire vocabulary (`"number"`, `"Vec3"`, `"string"`, …).

use mlua::{Lua, Table, Value as LuaValue};

use saffron_core::Uuid;
use saffron_scene::{Camera, Name, Transform};

use crate::bridge::ScriptRayHit;
use crate::entity::EntityHandle;
use crate::error::{Error, Result};
use crate::session;
use crate::value::{self, SaVec3};

/// Where a binding lives in the `sa.*` surface — the shape the emitter renders and
/// the registration walk dispatches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    /// A free function on the `sa` namespace table (`sa.vec3`, `sa.lerp`, `sa.log`).
    Free,
    /// A static constructor on a value class (`sa.Vec3.new`).
    Static,
    /// An instance method on a value class (`Vec3:length`, `Vec3:dot`).
    Method,
    /// A read-write field on a value class (`Vec3.x`).
    Field,
    /// A metamethod on a value class (`Vec3.__add`, `Vec3.__mul`).
    Meta,
}

/// One typed argument: the script-facing name and its wire-type token (the
/// `map_type` vocabulary — `"number"`, `"Vec3"`, `"string"`, …).
#[derive(Clone, Copy, Debug)]
pub struct Arg {
    /// The parameter name as it reads in the generated `fun(...)` signature.
    pub name: &'static str,
    /// The wire-type token, fed verbatim to area 10's `map_type`.
    pub ty: &'static str,
}

/// One `sa.*` binding descriptor: name, owning class (when a class member), kind,
/// argument list, return-type token, and doc. Expressed as Rust data, not parsed
/// from source — so the registration walk and the typegen emitter read the same
/// ordered set.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// The binding's script name (`vec3`, `length`, `__add`, `x`).
    pub name: &'static str,
    /// The owning value class for a member binding (`"Vec3"`), or `None` for a free
    /// `sa.` function.
    pub class: Option<&'static str>,
    /// Where the binding lives in the surface.
    pub kind: BindingKind,
    /// The typed argument list (excluding the implicit `self` of a [`BindingKind::Method`]).
    pub args: &'static [Arg],
    /// The return-type token, or `None` for a binding that returns nothing.
    pub ret: Option<&'static str>,
    /// A one-line doc rendered into the emitted defs.
    pub doc: &'static str,
}

/// Trims the per-entry boilerplate without hiding the set: each `binding!` is one
/// row of [`BINDINGS`]. The table is explicit and ordered so the emit order is
/// deterministic and the whole set is visible at once.
macro_rules! binding {
    (
        $name:literal,
        $class:expr,
        $kind:expr,
        [ $( ($an:literal : $at:literal) ),* $(,)? ],
        $ret:expr,
        $doc:literal $(,)?
    ) => {
        Binding {
            name: $name,
            class: $class,
            kind: $kind,
            args: &[ $( Arg { name: $an, ty: $at } ),* ],
            ret: $ret,
            doc: $doc,
        }
    };
}

/// The ordered `sa.*` binding-descriptor table — the single source of truth.
///
/// Carries the value-type surface (`sa.Vec3` + its members), the no-scene free
/// functions (`sa.vec3`, `sa.lerp`, `sa.look_at`, `sa.log`), the `sa.Entity` methods
/// and the scene-dependent globals. The physics-reaching surface lives here too: the
/// `sa.Entity` `move_character`/`apply_impulse`/`add_force`/`set_velocity`/
/// `get_velocity`/ragdoll methods, and the `sa.raycast`/`sa.spherecast` free functions
/// (whose `RayHit`/`RagdollState` return tokens the emitter maps to the `sa.RayHit`/
/// `sa.RagdollState` classes).
pub const BINDINGS: &[Binding] = &[
    binding!(
        "x",
        Some("Vec3"),
        BindingKind::Field,
        [],
        Some("number"),
        "The X component."
    ),
    binding!(
        "y",
        Some("Vec3"),
        BindingKind::Field,
        [],
        Some("number"),
        "The Y component."
    ),
    binding!(
        "z",
        Some("Vec3"),
        BindingKind::Field,
        [],
        Some("number"),
        "The Z component."
    ),
    binding!(
        "new", Some("Vec3"), BindingKind::Static,
        [("x": "number"), ("y": "number"), ("z": "number")], Some("Vec3"),
        "Construct a Vec3 from components."
    ),
    binding!(
        "length",
        Some("Vec3"),
        BindingKind::Method,
        [],
        Some("number"),
        "The Euclidean length."
    ),
    binding!(
        "normalized",
        Some("Vec3"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "A unit-length copy (zero stays zero)."
    ),
    binding!("dot", Some("Vec3"), BindingKind::Method, [("other": "Vec3")], Some("number"), "The dot product with another Vec3."),
    binding!("cross", Some("Vec3"), BindingKind::Method, [("other": "Vec3")], Some("Vec3"), "The cross product with another Vec3."),
    binding!(
        "lerp", Some("Vec3"), BindingKind::Method,
        [("other": "Vec3"), ("t": "number")], Some("Vec3"),
        "Linear interpolation toward another Vec3."
    ),
    binding!("__add", Some("Vec3"), BindingKind::Meta, [("other": "Vec3")], Some("Vec3"), "Component-wise addition."),
    binding!("__sub", Some("Vec3"), BindingKind::Meta, [("other": "Vec3")], Some("Vec3"), "Component-wise subtraction."),
    binding!("__mul", Some("Vec3"), BindingKind::Meta, [("scalar": "number")], Some("Vec3"), "Scalar multiplication (either operand order)."),
    binding!(
        "__unm",
        Some("Vec3"),
        BindingKind::Meta,
        [],
        Some("Vec3"),
        "Negation."
    ),
    binding!("__eq", Some("Vec3"), BindingKind::Meta, [("other": "Vec3")], Some("boolean"), "Component-wise equality."),
    binding!(
        "__tostring",
        Some("Vec3"),
        BindingKind::Meta,
        [],
        Some("string"),
        "The `Vec3(x, y, z)` string form."
    ),
    binding!(
        "vec3", None, BindingKind::Free,
        [("x": "number"), ("y": "number"), ("z": "number")], Some("Vec3"),
        "Construct a Vec3 from components."
    ),
    binding!(
        "lerp", None, BindingKind::Free,
        [("a": "Vec3"), ("b": "Vec3"), ("t": "number")], Some("Vec3"),
        "Linear interpolation between two Vec3 values."
    ),
    binding!(
        "look_at", None, BindingKind::Free,
        [("eye": "Vec3"), ("target": "Vec3"), ("up": "Vec3")], Some("Vec3"),
        "A look rotation as engine ZYX-Euler radians, for set_rotation."
    ),
    binding!("log", None, BindingKind::Free, [("message": "string")], None, "Log a message to the engine console."),
    binding!(
        "valid",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("boolean"),
        "Whether the entity is live in the current scene (false outside a callback)."
    ),
    binding!(
        "name",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("string"),
        "The entity's name, or an empty string when unavailable."
    ),
    binding!(
        "uuid",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("string"),
        "The entity's uuid as a decimal string, or \"0\" when unavailable."
    ),
    binding!(
        "get_position",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "Local position; zero when unavailable."
    ),
    binding!(
        "get_rotation",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "Local rotation as euler radians; zero when unavailable."
    ),
    binding!(
        "get_scale",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "Local scale; one when unavailable."
    ),
    binding!(
        "get_world_position",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "World-space position composed through the hierarchy; zero when unavailable."
    ),
    binding!(
        "get_world_rotation",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "World-space rotation as euler radians; round-trips through set_rotation."
    ),
    binding!(
        "set_position", Some("Entity"), BindingKind::Method, [("value": "Vec3")], None,
        "Set the local position."
    ),
    binding!(
        "set_rotation", Some("Entity"), BindingKind::Method, [("value": "Vec3")], None,
        "Set the local rotation (euler radians)."
    ),
    binding!(
        "set_scale", Some("Entity"), BindingKind::Method, [("value": "Vec3")], None,
        "Set the local scale."
    ),
    binding!(
        "get_component", Some("Entity"), BindingKind::Method, [("name": "ComponentName")],
        Some("table"),
        "A read-only snapshot of a registered component (wire shape), or nil when absent."
    ),
    binding!(
        "set_component", Some("Entity"), BindingKind::Method,
        [("name": "ComponentName"), ("value": "table")], Some("boolean"),
        "Merge a patch onto a registered component; false for an unknown or structural component."
    ),
    binding!(
        "add_component", Some("Entity"), BindingKind::Method, [("name": "ComponentName")],
        Some("boolean"),
        "Default-construct a registered component; false if present, unknown, or structural."
    ),
    binding!(
        "remove_component", Some("Entity"), BindingKind::Method, [("name": "ComponentName")],
        Some("boolean"),
        "Remove a removable registered component; false if absent, unknown, or non-removable."
    ),
    binding!(
        "has_component", Some("Entity"), BindingKind::Method, [("name": "ComponentName")],
        Some("boolean"),
        "Whether the entity carries a registered component."
    ),
    binding!(
        "destroy",
        Some("Entity"),
        BindingKind::Method,
        [],
        None,
        "Queue this entity for destruction at the end of the current instance loop."
    ),
    binding!(
        "set_parent", Some("Entity"), BindingKind::Method, [("parent": "Entity")], Some("boolean"),
        "Reparent under another entity (guarded against cycles); false on a failed guard."
    ),
    binding!(
        "parent",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Entity"),
        "The parent handle, or an invalid handle at the root (check :valid())."
    ),
    binding!(
        "children",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Entity[]"),
        "The child entity handles, in order."
    ),
    binding!(
        "send", Some("Entity"), BindingKind::Method,
        [("handler": "string"), ("payload": "any")], None,
        "Queue self:<handler>(sender, payload) on this entity's scripts after the loop."
    ),
    binding!(
        "move_character", Some("Entity"), BindingKind::Method,
        [("velocity": "Vec3"), ("jump": "boolean")], None,
        "Drive a CharacterController capsule (horizontal velocity + optional jump)."
    ),
    binding!(
        "apply_impulse", Some("Entity"), BindingKind::Method, [("impulse": "Vec3")], None,
        "Apply a center-of-mass impulse to this entity's Dynamic rigidbody."
    ),
    binding!(
        "add_force", Some("Entity"), BindingKind::Method, [("force": "Vec3")], None,
        "Add a continuous force (over the next step) to this entity's Dynamic rigidbody."
    ),
    binding!(
        "set_velocity", Some("Entity"), BindingKind::Method, [("velocity": "Vec3")], None,
        "Set the absolute linear velocity of this entity's Dynamic rigidbody."
    ),
    binding!(
        "set_morph_weights", Some("Entity"), BindingKind::Method, [("weights": "number[]")], None,
        "Set this entity's morph-target (blend-shape) weights (canonical 0..1)."
    ),
    binding!(
        "get_velocity",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("Vec3"),
        "The linear velocity of this entity's Dynamic rigidbody; zero when none."
    ),
    binding!(
        "enable_ragdoll",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("boolean"),
        "Make this rig go limp (a passive ragdoll); false on failure."
    ),
    binding!(
        "disable_ragdoll",
        Some("Entity"),
        BindingKind::Method,
        [],
        None,
        "Restore this rig from a ragdoll."
    ),
    binding!(
        "set_ragdoll_blend", Some("Entity"), BindingKind::Method,
        [("active": "boolean"), ("weight": "number")], None,
        "Blend this rig between physics (1) and animation (0); active arms the motors."
    ),
    binding!(
        "ragdoll_state",
        Some("Entity"),
        BindingKind::Method,
        [],
        Some("RagdollState"),
        "This rig's live ragdoll state (present/active/body_weight/bones)."
    ),
    binding!(
        "is_key_down", None, BindingKind::Free, [("key": "string")], Some("boolean"),
        "Whether a key is held this tick (case-insensitive)."
    ),
    binding!(
        "is_key_pressed", None, BindingKind::Free, [("key": "string")], Some("boolean"),
        "Whether a key went down this tick (the press edge)."
    ),
    binding!(
        "is_key_up", None, BindingKind::Free, [("key": "string")], Some("boolean"),
        "Whether a key went up this tick (the release edge)."
    ),
    binding!(
        "mouse_position",
        None,
        BindingKind::Free,
        [],
        Some("Vec3"),
        "The viewport-relative pointer position as a Vec3 (z is 0)."
    ),
    binding!(
        "mouse_delta",
        None,
        BindingKind::Free,
        [],
        Some("Vec3"),
        "The per-tick pointer delta as a Vec3 (z is 0)."
    ),
    binding!(
        "is_mouse_down", None, BindingKind::Free, [("button": "string")], Some("boolean"),
        "Whether a mouse button is held this tick (\"left\"/\"right\"/\"middle\")."
    ),
    binding!(
        "is_mouse_pressed", None, BindingKind::Free, [("button": "string")], Some("boolean"),
        "Whether a mouse button went down this tick (the press edge)."
    ),
    binding!(
        "is_mouse_up", None, BindingKind::Free, [("button": "string")], Some("boolean"),
        "Whether a mouse button went up this tick (the release edge)."
    ),
    binding!(
        "mouse_scroll",
        None,
        BindingKind::Free,
        [],
        Some("number"),
        "The accumulated scroll this tick."
    ),
    binding!(
        "get_entity_by_name", None, BindingKind::Free, [("name": "string")], Some("Entity"),
        "The first entity with this name, or an invalid handle (check :valid())."
    ),
    binding!(
        "find_all_by_name", None, BindingKind::Free, [("name": "string")], Some("Entity[]"),
        "Every entity with this name."
    ),
    binding!(
        "find_by_uuid", None, BindingKind::Free, [("uuid": "string")], Some("Entity"),
        "The entity with this uuid (decimal string), or an invalid handle."
    ),
    binding!(
        "primary_camera",
        None,
        BindingKind::Free,
        [],
        Some("Entity"),
        "The scene's primary camera entity, or an invalid handle."
    ),
    binding!(
        "spawn", None, BindingKind::Free, [("name": "string")], Some("Entity"),
        "Mint a new root entity (Name + Transform + Relationship) in the play scene."
    ),
    binding!(
        "broadcast", None, BindingKind::Free, [("handler": "string"), ("payload": "any")], None,
        "Queue handler(self, sender, payload) on every script instance after the loop."
    ),
    binding!(
        "raycast", None, BindingKind::Free,
        [
            ("ox": "number"), ("oy": "number"), ("oz": "number"),
            ("dx": "number"), ("dy": "number"), ("dz": "number"),
            ("max_dist": "number"),
        ],
        Some("RayHit"),
        "Cast a ray against the live physics world; { hit, distance, point, normal, entity }."
    ),
    binding!(
        "spherecast", None, BindingKind::Free,
        [
            ("ox": "number"), ("oy": "number"), ("oz": "number"),
            ("dx": "number"), ("dy": "number"), ("dz": "number"),
            ("radius": "number"), ("max_dist": "number"),
        ],
        Some("RayHit"),
        "Sweep a sphere against the live physics world (a thicker probe than raycast)."
    ),
    binding!(
        "vegetation_raycast", None, BindingKind::Free,
        [
            ("ox": "number"), ("oy": "number"), ("oz": "number"),
            ("dx": "number"), ("dy": "number"), ("dz": "number"),
            ("max_dist": "number"),
        ],
        Some("PlantHit"),
        "Closest macro plant whose bounds the ray enters (bounds-level, not a physics cast)."
    ),
    binding!(
        "vegetation_nearest", None, BindingKind::Free,
        [("x": "number"), ("y": "number"), ("z": "number"), ("radius": "number")],
        Some("PlantHit"),
        "The macro plant nearest a point within `radius`."
    ),
    binding!(
        "vegetation_in_radius", None, BindingKind::Free,
        [
            ("x": "number"), ("y": "number"), ("z": "number"),
            ("radius": "number"), ("limit": "number"),
        ],
        Some("any"),
        "Macro plants within `radius`, nearest first, capped at `limit` (default 64)."
    ),
    binding!(
        "vegetation_damage", None, BindingKind::Free,
        [("plant": "string"), ("amount": "number")], Some("boolean"),
        "Apply 0..1 damage to a plant through the reducer; returns whether it committed."
    ),
    binding!(
        "vegetation_harvest", None, BindingKind::Free,
        [("plant": "string"), ("phenotype": "number")], Some("boolean"),
        "Harvest a plant into a phenotype through the reducer; returns whether it committed."
    ),
    binding!(
        "spawn_task", None, BindingKind::Free, [("fn": "any")], Some("any"),
        "Start a coroutine task; returns the coroutine (resumes immediately)."
    ),
    binding!(
        "wait", None, BindingKind::Free, [("seconds": "number")], Some("number"),
        "Yield the running task for at least `seconds` of accumulated tick time."
    ),
    binding!(
        "delay", None, BindingKind::Free, [("seconds": "number"), ("fn": "any")], Some("any"),
        "Run `fn` after `seconds` of accumulated tick time (wait + call)."
    ),
];

/// Registers the value types into a VM (the `sa.Vec3` userdata metatable).
///
/// Bound into both the runtime VM and the throwaway schema VM, so a `properties`
/// default of `sa.vec3(0, 1, 0)` resolves at edit time too. `mlua` registers the
/// `SaVec3` `UserData` impl lazily on first use, so this is the no-op companion to
/// [`register_no_scene_globals`] — the metatable is materialized when `sa.vec3` first
/// constructs one. An explicit entry point so the schema VM has the same two-call shape
/// as the runtime VM.
pub fn register_value_types(_lua: &Lua) -> Result<()> {
    Ok(())
}

/// Binds every no-scene `sa.*` free function onto the `sa` namespace table: the value
/// constructors/helpers (`vec3`, `lerp`, `look_at`) and the base `log`.
///
/// The scene-dependent globals (`spawn`, `broadcast`, `raycast`, …) and the
/// `sa.Entity` methods are registered onto the same table by
/// [`register_scene_globals`]. Under Luau's sandbox the main thread carries a writable
/// globals table that shadows the frozen base, so installing a fresh `sa` table here is
/// safe after `sandbox(true)`.
pub fn register_no_scene_globals(lua: &Lua) -> Result<()> {
    register_value_types(lua)?;

    let sa = lua.create_table().map_err(runtime)?;

    let vec3 = lua
        .create_function(|_, (x, y, z): (f32, f32, f32)| Ok(value::vec3(x, y, z)))
        .map_err(runtime)?;
    sa.set("vec3", vec3).map_err(runtime)?;

    let lerp = lua
        .create_function(|_, (a, b, t): (SaVec3, SaVec3, f32)| Ok(value::lerp(a, b, t)))
        .map_err(runtime)?;
    sa.set("lerp", lerp).map_err(runtime)?;

    let look_at = lua
        .create_function(|_, (eye, target, up): (SaVec3, SaVec3, SaVec3)| {
            Ok(value::look_at(eye, target, up))
        })
        .map_err(runtime)?;
    sa.set("look_at", look_at).map_err(runtime)?;

    // The base `sa.log`; the play VM overrides it with the host log-sink variant.
    let log = lua
        .create_function(|_, message: String| {
            tracing::info!("{message}");
            Ok(())
        })
        .map_err(runtime)?;
    sa.set("log", log).map_err(runtime)?;

    lua.globals().set("sa", sa).map_err(runtime)?;
    Ok(())
}

/// Lowercases an input key for a case-insensitive held/edge lookup.
fn normalize_input_key(key: &str) -> String {
    key.to_ascii_lowercase()
}

/// Binds the scene-dependent `sa.*` free functions onto the already-installed `sa`
/// table: the input reads (held/edge keys + mouse), the hierarchy/query helpers
/// (`get_entity_by_name`/`primary_camera`/`spawn`/`find_all_by_name`/`find_by_uuid`),
/// and `sa.broadcast`.
///
/// Every binding reads/writes through the [session guard](crate::session): with no
/// session open (or no input lent) each returns its documented default. Called once per
/// session after [`register_no_scene_globals`]; the entity-side hierarchy methods
/// (`parent`/`children`/`set_parent`/`send`) live on the [`EntityHandle`] userdata, not
/// here.
pub fn register_scene_globals(lua: &Lua) -> Result<()> {
    let sa: Table = lua.globals().get("sa").map_err(runtime)?;

    // Input: held keys + derived edges, read through the lent ScriptInputState.
    let is_key_down = lua
        .create_function(|_, key: String| {
            let key = normalize_input_key(&key);
            Ok(session::with_input(|i| i.held.contains(&key)).unwrap_or(false))
        })
        .map_err(runtime)?;
    sa.set("is_key_down", is_key_down).map_err(runtime)?;

    let is_key_pressed = lua
        .create_function(|_, key: String| {
            let key = normalize_input_key(&key);
            Ok(session::with_input(|i| i.pressed.contains(&key)).unwrap_or(false))
        })
        .map_err(runtime)?;
    sa.set("is_key_pressed", is_key_pressed).map_err(runtime)?;

    let is_key_up = lua
        .create_function(|_, key: String| {
            let key = normalize_input_key(&key);
            Ok(session::with_input(|i| i.released.contains(&key)).unwrap_or(false))
        })
        .map_err(runtime)?;
    sa.set("is_key_up", is_key_up).map_err(runtime)?;

    let mouse_position = lua
        .create_function(|_, ()| {
            Ok(session::with_input(|i| value::vec3(i.mouse_x, i.mouse_y, 0.0)).unwrap_or_default())
        })
        .map_err(runtime)?;
    sa.set("mouse_position", mouse_position).map_err(runtime)?;

    let mouse_delta = lua
        .create_function(|_, ()| {
            Ok(
                session::with_input(|i| value::vec3(i.mouse_dx, i.mouse_dy, 0.0))
                    .unwrap_or_default(),
            )
        })
        .map_err(runtime)?;
    sa.set("mouse_delta", mouse_delta).map_err(runtime)?;

    let is_mouse_down = lua
        .create_function(|_, button: String| {
            let button = normalize_input_key(&button);
            Ok(session::with_input(|i| i.mouse_buttons.contains(&button)).unwrap_or(false))
        })
        .map_err(runtime)?;
    sa.set("is_mouse_down", is_mouse_down).map_err(runtime)?;

    let is_mouse_pressed = lua
        .create_function(|_, button: String| {
            let button = normalize_input_key(&button);
            Ok(session::with_input(|i| i.mouse_pressed.contains(&button)).unwrap_or(false))
        })
        .map_err(runtime)?;
    sa.set("is_mouse_pressed", is_mouse_pressed)
        .map_err(runtime)?;

    let is_mouse_up = lua
        .create_function(|_, button: String| {
            let button = normalize_input_key(&button);
            Ok(session::with_input(|i| i.mouse_released.contains(&button)).unwrap_or(false))
        })
        .map_err(runtime)?;
    sa.set("is_mouse_up", is_mouse_up).map_err(runtime)?;

    let mouse_scroll = lua
        .create_function(|_, ()| Ok(session::with_input(|i| i.scroll).unwrap_or(0.0)))
        .map_err(runtime)?;
    sa.set("mouse_scroll", mouse_scroll).map_err(runtime)?;

    // Hierarchy / queries: forEach scans + a root mint, through the session guard.
    let get_entity_by_name = lua
        .create_function(|_, name: String| Ok(find_first_by_name(&name)))
        .map_err(runtime)?;
    sa.set("get_entity_by_name", get_entity_by_name)
        .map_err(runtime)?;

    let find_all_by_name = lua
        .create_function(|_, name: String| Ok(find_all_named(&name)))
        .map_err(runtime)?;
    sa.set("find_all_by_name", find_all_by_name)
        .map_err(runtime)?;

    let find_by_uuid = lua
        .create_function(|_, uuid: String| Ok(find_by_uuid(&uuid)))
        .map_err(runtime)?;
    sa.set("find_by_uuid", find_by_uuid).map_err(runtime)?;

    let primary_camera = lua
        .create_function(|_, ()| Ok(primary_camera()))
        .map_err(runtime)?;
    sa.set("primary_camera", primary_camera).map_err(runtime)?;

    let spawn = lua
        .create_function(|_, name: String| Ok(spawn_root(&name)))
        .map_err(runtime)?;
    sa.set("spawn", spawn).map_err(runtime)?;

    let broadcast = lua
        .create_function(|lua, (handler, payload): (String, LuaValue)| {
            broadcast(lua, &handler, payload);
            Ok(())
        })
        .map_err(runtime)?;
    sa.set("broadcast", broadcast).map_err(runtime)?;

    // Physics queries: route through the host-installed bridge, shaping the POD hit
    // into the result table.
    let raycast = lua
        .create_function(
            |lua, (ox, oy, oz, dx, dy, dz, max_dist): (f32, f32, f32, f32, f32, f32, f32)| {
                let origin = glam::Vec3::new(ox, oy, oz);
                let dir = glam::Vec3::new(dx, dy, dz);
                let hit = session::with_bridge(|bridge| bridge.raycast(origin, dir, max_dist));
                ray_hit_table(lua, hit)
            },
        )
        .map_err(runtime)?;
    sa.set("raycast", raycast).map_err(runtime)?;

    let spherecast = lua
        .create_function(
            |lua,
             (ox, oy, oz, dx, dy, dz, radius, max_dist): (
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
                f32,
            )| {
                let origin = glam::Vec3::new(ox, oy, oz);
                let dir = glam::Vec3::new(dx, dy, dz);
                let hit = session::with_bridge(|bridge| {
                    bridge.sphere_cast(origin, dir, radius, max_dist)
                });
                ray_hit_table(lua, hit)
            },
        )
        .map_err(runtime)?;
    sa.set("spherecast", spherecast).map_err(runtime)?;

    // Vegetation: bounds-level macro queries and the two interaction mutations a script drives.
    // A query answers from the bound authority synchronously; a mutation reduces through the one
    // reducer and reports whether it committed.
    let vegetation_raycast = lua
        .create_function(
            |lua, (ox, oy, oz, dx, dy, dz, max_dist): (f32, f32, f32, f32, f32, f32, f32)| {
                let origin = glam::Vec3::new(ox, oy, oz);
                let dir = glam::Vec3::new(dx, dy, dz);
                let hit =
                    session::with_bridge(|bridge| bridge.vegetation_raycast(origin, dir, max_dist))
                        .flatten();
                plant_hit_table(lua, hit)
            },
        )
        .map_err(runtime)?;
    sa.set("vegetation_raycast", vegetation_raycast)
        .map_err(runtime)?;

    let vegetation_nearest = lua
        .create_function(|lua, (x, y, z, radius): (f32, f32, f32, f32)| {
            let position = glam::Vec3::new(x, y, z);
            let hit = session::with_bridge(|bridge| bridge.vegetation_nearest(position, radius))
                .flatten();
            plant_hit_table(lua, hit)
        })
        .map_err(runtime)?;
    sa.set("vegetation_nearest", vegetation_nearest)
        .map_err(runtime)?;

    let vegetation_in_radius = lua
        .create_function(
            |lua, (x, y, z, radius, limit): (f32, f32, f32, f32, Option<u32>)| {
                let position = glam::Vec3::new(x, y, z);
                let limit = limit.unwrap_or(64) as usize;
                let hits = session::with_bridge(|bridge| {
                    bridge.vegetation_in_radius(position, radius, limit)
                })
                .unwrap_or_default();
                let list = lua.create_table()?;
                for (index, hit) in hits.into_iter().enumerate() {
                    list.set(index + 1, plant_hit_table(lua, Some(hit))?)?;
                }
                Ok(list)
            },
        )
        .map_err(runtime)?;
    sa.set("vegetation_in_radius", vegetation_in_radius)
        .map_err(runtime)?;

    let vegetation_damage = lua
        .create_function(|_, (plant, amount): (String, f32)| {
            Ok(
                session::with_bridge(|bridge| bridge.vegetation_damage(&plant, amount))
                    .unwrap_or(false),
            )
        })
        .map_err(runtime)?;
    sa.set("vegetation_damage", vegetation_damage)
        .map_err(runtime)?;

    let vegetation_harvest = lua
        .create_function(|_, (plant, phenotype): (String, u32)| {
            Ok(
                session::with_bridge(|bridge| bridge.vegetation_harvest(&plant, phenotype))
                    .unwrap_or(false),
            )
        })
        .map_err(runtime)?;
    sa.set("vegetation_harvest", vegetation_harvest)
        .map_err(runtime)?;

    // Override the no-scene `sa.log` with the play VM's log-sink variant: the line still
    // hits the engine log, then routes to the host's script-log ring tagged with the
    // running instance's uuid.
    let log = lua
        .create_function(|_, message: String| {
            tracing::info!("{message}");
            let sender = session::current_sender();
            session::with_bridge(|bridge| bridge.log_sink(sender, &message));
            Ok(())
        })
        .map_err(runtime)?;
    sa.set("log", log).map_err(runtime)?;

    Ok(())
}

/// Shapes a [`ScriptRayHit`] POD into the `{hit, distance, point, normal, entity | plant}` Lua
/// table. `point`/`normal` are `sa.Vec3`; `entity` is the resolved [`EntityHandle`] only
/// on a hit with an owner entity (a miss / unmapped body has no `entity` key — `nil`). A
/// `None` hit (no bridge lent) is the miss table `{hit = false}`.
/// Shapes one vegetation hit into its result table. A miss is `{ hit = false }`, mirroring the
/// physics-cast tables so a script reads both the same way.
fn plant_hit_table(lua: &Lua, hit: Option<crate::bridge::ScriptPlantHit>) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let Some(hit) = hit else {
        table.set("hit", false)?;
        return Ok(table);
    };
    table.set("hit", true)?;
    table.set("plant", hit.plant)?;
    table.set("position", SaVec3::new(hit.position))?;
    table.set("distance", hit.distance)?;
    table.set("lifecycle", hit.lifecycle)?;
    table.set("health", hit.health)?;
    table.set("interaction_policy", hit.interaction_policy)?;
    Ok(table)
}

fn ray_hit_table(lua: &Lua, hit: Option<ScriptRayHit>) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let Some(hit) = hit else {
        table.set("hit", false)?;
        return Ok(table);
    };
    table.set("hit", hit.hit)?;
    table.set("distance", hit.distance)?;
    table.set("point", SaVec3::new(hit.point))?;
    table.set("normal", SaVec3::new(hit.normal))?;
    match hit.target {
        Some(crate::bridge::ScriptHitTarget::SceneEntity(uuid)) if hit.hit => {
            let resolved = session::with_scene(|scene| scene.find_entity_by_uuid(uuid)).flatten();
            if let Some(entity) = resolved {
                table.set("entity", EntityHandle::new(entity))?;
            }
        }
        Some(crate::bridge::ScriptHitTarget::Vegetation(plant)) if hit.hit => {
            table.set("plant", plant.canonical_hex())?;
        }
        _ => {}
    }
    Ok(table)
}

/// The first entity matching `name` (names are not unique), or the invalid handle when
/// absent / outside a session.
fn find_first_by_name(name: &str) -> EntityHandle {
    let found = session::with_scene_mut(|scene| {
        let mut hit = None;
        scene.for_each::<&Name, _>(|entity, n| {
            if hit.is_none() && n.name == name {
                hit = Some(entity);
            }
        });
        hit
    })
    .flatten();
    EntityHandle::new(found.unwrap_or(saffron_scene::Entity::NULL))
}

/// Every entity matching `name`, in scan order. An empty list outside a session.
fn find_all_named(name: &str) -> Vec<EntityHandle> {
    session::with_scene_mut(|scene| {
        let mut hits = Vec::new();
        scene.for_each::<&Name, _>(|entity, n| {
            if n.name == name {
                hits.push(EntityHandle::new(entity));
            }
        });
        hits
    })
    .unwrap_or_default()
}

/// Resolves a uuid (decimal string, matching `Entity:uuid()`) to its entity, or the
/// invalid handle. A `0` / unparsable id, or no session, yields the invalid handle.
fn find_by_uuid(uuid: &str) -> EntityHandle {
    let id: u64 = uuid.trim().parse().unwrap_or(0);
    if id == 0 {
        return EntityHandle::default();
    }
    let found = session::with_scene(|scene| scene.find_entity_by_uuid(Uuid(id))).flatten();
    EntityHandle::new(found.unwrap_or(saffron_scene::Entity::NULL))
}

/// The scene's first primary camera entity, or the invalid handle. Moving its transform
/// IS "move camera".
fn primary_camera() -> EntityHandle {
    let found = session::with_scene_mut(|scene| {
        let mut hit = None;
        scene.for_each::<(&Transform, &Camera), _>(|entity, (_, camera)| {
            if hit.is_none() && camera.primary {
                hit = Some(entity);
            }
        });
        hit
    })
    .flatten();
    EntityHandle::new(found.unwrap_or(saffron_scene::Entity::NULL))
}

/// Mints a new root entity (Name + Transform + Relationship) in the play duplicate. The
/// invalid handle outside a session.
fn spawn_root(name: &str) -> EntityHandle {
    let entity = session::with_scene_mut(|scene| scene.create_entity(name));
    EntityHandle::new(entity.unwrap_or(saffron_scene::Entity::NULL))
}

/// Queues a broadcast message to every script instance: `handler(self, sender, payload)`
/// runs after the loop.
fn broadcast(lua: &Lua, handler: &str, payload: LuaValue) {
    let payload_ref = match payload {
        LuaValue::Nil => None,
        other => lua.create_registry_value(other).ok(),
    };
    session::queue_message(session::ScriptMessage {
        target: Uuid(0),
        sender: session::current_sender(),
        handler: handler.to_owned(),
        payload: payload_ref,
    });
}

/// `FromLua` for [`SaVec3`] so the free `sa.lerp`/`sa.look_at` functions accept the
/// value type directly: any `SaVec3` userdata argument is borrowed and copied out.
impl mlua::FromLua for SaVec3 {
    fn from_lua(value: mlua::Value, _lua: &Lua) -> mlua::Result<Self> {
        match value {
            mlua::Value::UserData(ud) => Ok(*ud.borrow::<SaVec3>()?),
            other => Err(mlua::Error::FromLuaConversionError {
                from: other.type_name(),
                to: "Vec3".to_owned(),
                message: Some("expected a Vec3".to_owned()),
            }),
        }
    }
}

/// Lifts an `mlua::Error` raised during registration into [`Error::Runtime`].
fn runtime(err: mlua::Error) -> Error {
    Error::Runtime(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table must carry every value-type/no-scene/Entity entry, each with a
    /// resolved argument/return type, in a stable order across builds.
    #[test]
    fn table_carries_every_no_scene_entry() {
        let names: Vec<(Option<&str>, &str)> = BINDINGS.iter().map(|b| (b.class, b.name)).collect();
        // The no-scene free functions.
        for free in ["vec3", "lerp", "look_at", "log"] {
            assert!(
                names.contains(&(None, free)),
                "missing free function sa.{free}"
            );
        }
        // The Vec3 value-class surface.
        for member in [
            "x",
            "y",
            "z",
            "new",
            "length",
            "normalized",
            "dot",
            "cross",
            "lerp",
            "__add",
            "__sub",
            "__mul",
            "__unm",
            "__eq",
            "__tostring",
        ] {
            assert!(
                names.contains(&(Some("Vec3"), member)),
                "missing Vec3 member {member}"
            );
        }
        // The sa.Entity scene surface, the component bridge, and the hierarchy/message
        // methods.
        for member in [
            "valid",
            "name",
            "uuid",
            "get_position",
            "get_rotation",
            "get_scale",
            "get_world_position",
            "get_world_rotation",
            "set_position",
            "set_rotation",
            "set_scale",
            "get_component",
            "set_component",
            "add_component",
            "remove_component",
            "has_component",
            "destroy",
            "set_parent",
            "parent",
            "children",
            "send",
            "move_character",
            "apply_impulse",
            "add_force",
            "set_velocity",
            "set_morph_weights",
            "get_velocity",
            "enable_ragdoll",
            "disable_ragdoll",
            "set_ragdoll_blend",
            "ragdoll_state",
        ] {
            assert!(
                names.contains(&(Some("Entity"), member)),
                "missing Entity member {member}"
            );
        }
        // The scene-dependent free functions: input, hierarchy queries, and the
        // scheduler/broadcast surface.
        for free in [
            "is_key_down",
            "is_key_pressed",
            "is_key_up",
            "mouse_position",
            "mouse_delta",
            "is_mouse_down",
            "is_mouse_pressed",
            "is_mouse_up",
            "mouse_scroll",
            "get_entity_by_name",
            "find_all_by_name",
            "find_by_uuid",
            "primary_camera",
            "spawn",
            "broadcast",
            "raycast",
            "spherecast",
            "spawn_task",
            "wait",
            "delay",
        ] {
            assert!(
                names.contains(&(None, free)),
                "missing free function sa.{free}"
            );
        }
    }

    #[test]
    fn every_binding_has_resolved_types() {
        for b in BINDINGS {
            for arg in b.args {
                assert!(
                    !arg.ty.is_empty(),
                    "{} arg {} has empty type",
                    b.name,
                    arg.name
                );
                assert!(!arg.name.is_empty(), "{} has an unnamed arg", b.name);
            }
            assert!(!b.doc.is_empty(), "{} has no doc", b.name);
        }
    }

    /// The table order is fixed (the emitter needs a deterministic order), so the
    /// row count and the first/last rows are pinned.
    #[test]
    fn table_order_is_stable() {
        assert_eq!(BINDINGS.len(), 75);
        assert_eq!((BINDINGS[0].class, BINDINGS[0].name), (Some("Vec3"), "x"));
        let last = BINDINGS[BINDINGS.len() - 1];
        assert_eq!((last.class, last.name), (None, "delay"));
    }
}
