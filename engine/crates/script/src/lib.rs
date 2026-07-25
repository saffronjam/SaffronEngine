//! The mlua/Luau VM, the typed `sa.*` bindings, and the generated Luau type defs.
//!
//! Depends on `saffron-core`, `saffron-scene` only. `mlua` confines every `lua_State`
//! unsafety internally, so this crate is `#![deny(unsafe_code)]`.
//!
//! The VM primitive is a sandboxed [`ScriptVm`] with an instruction/memory budget and a
//! typed [`Error`] carrying the Luau traceback. The [`SaVec3`] value type and the
//! declarative [`BINDINGS`] table are the single source that both registers the no-scene
//! `sa.*` surface and feeds the Luau type emitter. The scoped [session guard](session)
//! holds the live-scene invariant, and the [`EntityHandle`] scene-only surface
//! (transforms, name/uuid, valid) is built on it. The [`ScriptHost`] lifecycle
//! (`start_scripts`/`tick_scripts`/`stop_scripts`) drives the class cache, the instance
//! build with field injection + overrides, pause-on-error, and the deferred destroy +
//! relink. The coroutine [scheduler](scheduler), the inter-script [`ScriptMessage`]
//! queue (`entity:send`/`sa.broadcast`, drained after the loop), the input reads (held +
//! derived edges + mouse, lent through the session guard), and the hierarchy/query
//! bindings (`parent`/`children`/`set_parent`/`spawn`/`get_entity_by_name`/
//! `find_all_by_name`/`find_by_uuid`/`primary_camera`) round out the runtime surface.
//! The [`ScriptHostBridge`] is the POD seam the host implements for the physics reach
//! (`sa.raycast`/`apply_impulse`/ragdoll control + the `sa.log` sink); `move_character`
//! is a pure-Scene write; [`ScriptHost::dispatch_contact`] routes the contact-event ring
//! to `on_trigger_enter`/`on_trigger_exit`/`on_contact` handlers. [`read_script_schema`]
//! is the Inspector field contract: a throwaway sandboxed VM reads a script's declared
//! `properties` and infers each [`ScriptField`]'s [`ScriptFieldType`] from its default —
//! the shape the host's `get-script-schema` command maps to the `GetScriptSchemaResult`
//! DTO.

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
    NoopBridge, ScriptHitTarget, ScriptHostBridge, ScriptPlantHit, ScriptRagdollState, ScriptRayHit,
};
pub use entity::EntityHandle;
pub use error::{Error, Result};
pub use runtime::{ContactInfo, ScriptHost, ScriptRunError};
pub use schema::{ScriptField, ScriptFieldType, read_script_schema};
pub use session::{
    DeferredOps, ScopedSession, ScriptMessage, current_sender, defer_destroy, enter_session,
    queue_message, session_active, set_bridge, set_sender, take_deferred, take_messages,
    with_bridge, with_input, with_registry, with_scene, with_scene_mut,
};
pub use structural::{STRUCTURAL_COMPONENTS, is_structural_component};
pub use value::{SaVec3, lerp, look_at, vec3};
pub use vm::{DEFAULT_INSTRUCTION_BUDGET, DEFAULT_MEMORY_LIMIT, ScriptVm};
