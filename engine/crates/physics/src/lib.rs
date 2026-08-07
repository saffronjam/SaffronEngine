//! The safe Jolt wrapper above `saffron-physics-sys`: the per-play [`World`], body creation from
//! the scene's collider/rigidbody components, the deterministic fixed-step loop with dynamic
//! transform write-back, the character controller, the ragdoll blend layer, and the read-only
//! query + impulse/force/velocity surface.
//!
//! The `unsafe` Jolt boundary lives entirely in `saffron-physics-sys`; this crate speaks safe Rust
//! and the POD bridge. Spatial queries take `&self`, so a query can never perturb the step.

#![deny(unsafe_code)]

mod error;
mod types;
mod world;

pub use error::{Error, Result};
pub use saffron_physics_sys::INVALID_BODY_ID;
pub use types::{
    BodyInfo, CONTACT_RING_CAP, ContactDrain, ContactEvent, ContactKind, FIXED_STEP, MotionType,
    ObjectLayer, PoseTarget, RagdollState, RayHit, StaticTargetBodyCreate, WorldHitTarget,
    WorldStats, layers_collide,
};
pub use world::{MeshCook, World, fit_bone_capsules, fit_collider_to_mesh, shutdown_physics};

#[cfg(test)]
mod tests;
