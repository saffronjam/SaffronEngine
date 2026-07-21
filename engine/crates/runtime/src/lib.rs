//! `saffron-runtime`: the shared play-mode simulation spine.
//!
//! [`RuntimeSession`] bundles the per-frame "advance the world" work — build a Jolt world from
//! a scene, tick animation, step physics, dispatch contacts, tick scripts — as one code path
//! consumed by both the editor host (its play mode) and the standalone `saffron-player`. It
//! owns the simulation subsystems but neither the scene nor the asset server (both are handed
//! in per call), so each consumer drives its own scene through the same methods.
//!
//! DAG: depends on `saffron-core`, `saffron-scene`, `saffron-assets`, `saffron-animation`,
//! `saffron-script`, `saffron-physics`. It has no window, renderer, control-plane, or
//! scene-edit dependency — drawing and editing live above it.

#![deny(unsafe_code)]

mod bridge;
mod session;
mod vegetation;

pub use bridge::{
    RuntimeScriptBridge, ScriptLogLine, SharedPhysics, SharedScene, SharedScriptSink,
};
pub use session::RuntimeSession;
pub use vegetation::{
    VegetationRuntimeBindingStatus, VegetationRuntimeError, VegetationRuntimeUnavailableReason,
};
