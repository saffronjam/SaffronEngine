//! The mlua/Luau VM, the typed `sa.*` bindings, and the generated Luau type defs. `mlua` confines
//! every `lua_State` unsafety internally, so this crate is `#![deny(unsafe_code)]`.
//!
//! [`ScriptVm`] is sandboxed with an instruction and memory budget, raising a typed [`Error`] that
//! carries the Luau traceback. [`BINDINGS`] is the single source that both registers the `sa.*`
//! surface and feeds the Luau type emitter. The scoped [session guard](session) holds the live-scene
//! invariant that [`EntityHandle`] is built on, and [`ScriptHostBridge`] is the POD seam the host
//! implements for the physics reach, so this crate needs no physics edge.

#![deny(unsafe_code)]

mod bindings;
mod bridge;
mod convert;
mod entity;
mod error;
mod runtime;
mod scheduler;
mod schema;
mod session;
mod structural;
mod value;
mod vm;

pub use bindings::{
    Arg, BINDINGS, Binding, BindingKind, register_no_scene_globals, register_scene_globals,
    register_value_types,
};
pub use bridge::{
    NoopBridge, ScriptHitTarget, ScriptHostBridge, ScriptPlantFilter, ScriptPlantHit,
    ScriptRagdollState, ScriptRayHit,
};
pub use entity::EntityHandle;
pub use error::{Error, Result};
pub use runtime::{ContactInfo, ScriptHost, ScriptRunError, VegetationEventInfo};
pub use schema::{ScriptField, ScriptFieldType, read_script_schema};
pub use session::{
    DeferredOps, ScopedSession, ScriptMessage, current_sender, defer_destroy, enter_session,
    queue_message, session_active, set_bridge, set_sender, take_deferred, take_messages,
    with_bridge, with_input, with_registry, with_scene, with_scene_mut,
};
pub use structural::{STRUCTURAL_COMPONENTS, is_structural_component};
pub use value::{SaVec3, lerp, look_at, vec3};
pub use vm::{DEFAULT_INSTRUCTION_BUDGET, DEFAULT_MEMORY_LIMIT, ScriptVm};
