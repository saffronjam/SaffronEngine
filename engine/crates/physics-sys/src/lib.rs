//! FFI seam: vendored Jolt 5.3.0 + the C++ shim (the `unsafe` Jolt boundary).
//!
//! One of the three FFI crates that opt back into `unsafe` (the foundations lints policy; every
//! other crate keeps `#![deny(unsafe_code)]`). The seam named here is the Jolt FFI: `build.rs`
//! compiles every Jolt TU and the shim with the determinism flag set
//! (`JPH_CROSS_PLATFORM_DETERMINISTIC`, single precision, `-ffp-model=precise`,
//! `-ffp-contract=off`, confined `-mavx2`), and links the static archive into this crate. The
//! flags are confined here so the rest of the workspace is never recompiled with arch flags that
//! would change its float results.
//!
//! The FFI ABI is the `cxx` bridge in [`bridge::ffi`]: POD-only across the wire (scalars and the
//! shared `PendingContact` struct), with the opaque `JoltWorld` holding the `PhysicsSystem` plus
//! the four virtual shim classes `cxx` cannot synthesize (the three layer filters and the
//! `ContactListener`). This crate re-exports a thin safe surface so `saffron-physics` never names
//! the `unsafe` bridge module directly.
#![allow(unsafe_code)]

mod bridge;

use cxx::UniquePtr;

pub use bridge::ffi::{BodyCreate, BonePart, CharacterCreate, JoltWorld, PendingContact, RayHit};

/// The raw `BodyID` the bridge hands back for a failed create (`JPH::BodyID::cInvalidBodyID`).
/// The safe layer treats it as "no body" rather than tracking it.
pub const INVALID_BODY_ID: u32 = u32::MAX;

/// The deterministic fixed substep — the step the safe layer's accumulator advances by. Kept
/// here so the bridge-level step test uses the real cadence; the safe `saffron-physics` layer
/// owns the accumulator.
pub const FIXED_STEP: f32 = 1.0 / 60.0;

/// Initialize the Jolt globals: the default allocator, trace/assert hooks, the type `Factory`,
/// and `RegisterTypes`. Idempotent — calling it twice is a no-op on the second call.
///
/// # Errors
/// Returns `Err` if the shim reports the Jolt globals failed to install.
pub fn init() -> Result<(), &'static str> {
    if bridge::ffi::jolt_init() {
        Ok(())
    } else {
        Err("Jolt global init failed")
    }
}

/// Tear down the Jolt globals: `UnregisterTypes` and destroy the `Factory`. Idempotent — safe to
/// call without a prior [`init`] or twice in a row.
pub fn shutdown() {
    bridge::ffi::jolt_shutdown();
}

/// The compiled-in Jolt version, encoded as `(major << 16) | (minor << 8) | patch`. A non-zero
/// value proves the Jolt static archive linked.
pub fn jolt_version() -> u32 {
    bridge::ffi::jolt_version()
}

/// `true` if the shim TU was compiled with `JPH_CROSS_PLATFORM_DETERMINISTIC`. Guaranteed by a
/// build-time `#error` guard, surfaced so a test can assert the contract end-to-end.
pub fn is_deterministic() -> bool {
    bridge::ffi::jolt_is_deterministic()
}

/// `true` if the shim TU was compiled in single precision (`JPH_DOUBLE_PRECISION` not defined).
/// Guaranteed by a build-time `#error` guard, surfaced so a test can assert the contract.
pub fn is_single_precision() -> bool {
    bridge::ffi::jolt_is_single_precision()
}

/// Whether two object layers may collide, by the fixed v1 collision matrix implemented in the C++
/// shim. `a` and `b` are `ObjectLayer` raw discriminants (`Static=0`, `Moving=1`, `Character=2`,
/// `Debris=3`, `Sensor=4`). Symmetric. Surfaced so a test can assert the matrix.
pub fn layers_collide(a: u8, b: u8) -> bool {
    bridge::ffi::jolt_layers_collide(a, b)
}

/// Allocate a fresh, uninitialized [`JoltWorld`] — the `TempAllocator`, the `JobSystem`, the four
/// shim instances, and a bare `PhysicsSystem`. Call [`world_init`] before use. Returns `None` if
/// the C++ side handed back a null pointer (an allocation failure).
pub fn world_new() -> Option<UniquePtr<JoltWorld>> {
    let world = bridge::ffi::jolt_world_new();
    if world.is_null() { None } else { Some(world) }
}

/// Wire the filters into `PhysicsSystem::Init`, set gravity, and install the contact listener.
/// Call once on a freshly [`world_new`]ed world.
pub fn world_init(world: &mut UniquePtr<JoltWorld>) {
    bridge::ffi::jolt_world_init(world.pin_mut());
}

/// The live body count (`PhysicsSystem::GetNumBodies`).
pub fn world_body_count(world: &UniquePtr<JoltWorld>) -> u32 {
    bridge::ffi::jolt_world_body_count(world)
}

/// Advance the world by one substep of `dt` with `collision_steps` solver iterations. The safe
/// layer's fixed-step accumulator drives this; exposed here so the bridge round-trips a step.
pub fn world_step(world: &mut UniquePtr<JoltWorld>, dt: f32, collision_steps: i32) {
    bridge::ffi::jolt_world_step(world.pin_mut(), dt, collision_steps);
}

/// Swap-and-clear the contact listener's mutex-guarded buffer, handing the buffered POD records to
/// Rust. Called on the sim thread, never from a Jolt callback.
pub fn drain_contacts(world: &mut UniquePtr<JoltWorld>) -> Vec<PendingContact> {
    bridge::ffi::jolt_drain_contacts(world.pin_mut())
}

/// Build the body `create.shape` selects and add it to the world, returning its raw `BodyID`.
/// Analytic shapes (Box/Sphere/Capsule) ignore the geometry slices; ConvexHull builds from
/// `hull_points` (flattened `xyz`, index order) and Mesh from `mesh_vertices` (flattened `xyz`) +
/// `mesh_indices` (flat triangle list). Returns [`INVALID_BODY_ID`] when the shape or body create
/// failed (an empty cook, a degenerate hull). Mesh-on-Dynamic is rejected on the safe side, so the
/// shim never sees it.
pub fn create_body(
    world: &mut UniquePtr<JoltWorld>,
    create: &BodyCreate,
    hull_points: &[f32],
    mesh_vertices: &[f32],
    mesh_indices: &[u32],
) -> u32 {
    bridge::ffi::jolt_create_body(
        world.pin_mut(),
        create,
        hull_points,
        mesh_vertices,
        mesh_indices,
    )
}

/// Create every body in `creates` (analytic shapes only) and insert them through Jolt's batched
/// broadphase path (`AddBodiesPrepare` + `AddBodiesFinalize`, kept asleep). Returns one raw
/// `BodyID` per input row, position-aligned; a failed create yields [`INVALID_BODY_ID`] in its
/// slot while the rest of the batch still lands.
pub fn create_static_batch(world: &mut UniquePtr<JoltWorld>, creates: &[BodyCreate]) -> Vec<u32> {
    bridge::ffi::jolt_create_static_batch(world.pin_mut(), creates)
}

/// Remove and destroy every listed body in one batch. [`INVALID_BODY_ID`] sentinels are skipped.
pub fn remove_bodies(world: &mut UniquePtr<JoltWorld>, ids: &[u32]) {
    bridge::ffi::jolt_remove_bodies(world.pin_mut(), ids);
}

/// A body's world position (`xyz`) and rotation (`xyzw`), read each step for the dynamic
/// transform write-back.
pub fn body_position_rotation(world: &UniquePtr<JoltWorld>, id: u32) -> ([f32; 3], [f32; 4]) {
    let mut position = [0.0f32; 3];
    let mut rotation = [0.0f32; 4];
    bridge::ffi::jolt_body_position_rotation(world, id, &mut position, &mut rotation);
    (position, rotation)
}

/// A body's world position (`xyz`), for the read-only body list.
pub fn body_position(world: &UniquePtr<JoltWorld>, id: u32) -> [f32; 3] {
    bridge::ffi::jolt_body_position(world, id)
}

/// Whether a body is awake, for the read-only body list.
pub fn body_is_active(world: &UniquePtr<JoltWorld>, id: u32) -> bool {
    bridge::ffi::jolt_body_is_active(world, id)
}

/// A body's current linear velocity (`xyz`).
pub fn body_linear_velocity(world: &UniquePtr<JoltWorld>, id: u32) -> [f32; 3] {
    bridge::ffi::jolt_body_linear_velocity(world, id)
}

/// A body's current angular velocity (`xyz`, radians per second about each world axis).
pub fn body_angular_velocity(world: &UniquePtr<JoltWorld>, id: u32) -> [f32; 3] {
    bridge::ffi::jolt_body_angular_velocity(world, id)
}

/// Activate the body and apply a center-of-mass impulse (`xyz`).
pub fn body_add_impulse(world: &mut UniquePtr<JoltWorld>, id: u32, impulse: [f32; 3]) {
    bridge::ffi::jolt_body_add_impulse(world.pin_mut(), id, &impulse);
}

/// Activate the body and add a force (`xyz`) applied over the next step.
pub fn body_add_force(world: &mut UniquePtr<JoltWorld>, id: u32, force: [f32; 3]) {
    bridge::ffi::jolt_body_add_force(world.pin_mut(), id, &force);
}

/// Activate the body and set its linear velocity (`xyz`).
pub fn body_set_linear_velocity(world: &mut UniquePtr<JoltWorld>, id: u32, velocity: [f32; 3]) {
    bridge::ffi::jolt_body_set_linear_velocity(world.pin_mut(), id, &velocity);
}

/// Move a Kinematic body toward `position` (`xyz`) / `rotation` (`xyzw`) over `dt` via
/// `BodyInterface::MoveKinematic` — the swept motion imparts contact velocity to the dynamics it
/// hits (never a teleport). `dt` must be the fixed substep that feeds the step so the derived
/// velocity matches it. `id` is a raw `BodyID`; Kinematic-only at the call site.
pub fn move_kinematic(
    world: &mut UniquePtr<JoltWorld>,
    id: u32,
    position: [f32; 3],
    rotation: [f32; 4],
    dt: f32,
) {
    bridge::ffi::jolt_move_kinematic(world.pin_mut(), id, &position, &rotation, dt);
}

/// Create a `CharacterVirtual` from a capsule + slope angle and store it in the world, returning
/// its index. Returns [`INVALID_BODY_ID`] if the capsule shape create failed. The controller
/// logic (gravity integration, speed clamp) is the safe layer's; the shim owns only the sweep
/// object.
pub fn add_character(world: &mut UniquePtr<JoltWorld>, create: &CharacterCreate) -> u32 {
    bridge::ffi::jolt_add_character(world.pin_mut(), create)
}

/// Set a character's linear velocity (`xyz`) for the next [`character_extended_update`].
pub fn character_set_linear_velocity(
    world: &mut UniquePtr<JoltWorld>,
    index: u32,
    velocity: [f32; 3],
) {
    bridge::ffi::jolt_character_set_linear_velocity(world.pin_mut(), index, &velocity);
}

/// Advance one character by `dt` (`CharacterVirtual::ExtendedUpdate`) with the Character-layer
/// filters, the supplied applied `gravity` (`xyz`, already scaled by the gravity factor), and a
/// `step_up` walk-stairs height. `index` is a character slot.
pub fn character_extended_update(
    world: &mut UniquePtr<JoltWorld>,
    index: u32,
    dt: f32,
    gravity: [f32; 3],
    step_up: f32,
) {
    bridge::ffi::jolt_character_extended_update(world.pin_mut(), index, dt, &gravity, step_up);
}

/// `true` if the character is resting on walkable ground (`GetGroundState() == OnGround`).
pub fn character_on_ground(world: &UniquePtr<JoltWorld>, index: u32) -> bool {
    bridge::ffi::jolt_character_on_ground(world, index)
}

/// A character's resolved world position (`xyz`), for the per-step transform write-back.
pub fn character_position(world: &UniquePtr<JoltWorld>, index: u32) -> [f32; 3] {
    bridge::ffi::jolt_character_position(world, index)
}

/// The world's gravity vector (`xyz`). The safe layer integrates the controller's vertical
/// velocity against this each substep (`PhysicsSystem::GetGravity`).
pub fn world_gravity(world: &UniquePtr<JoltWorld>) -> [f32; 3] {
    bridge::ffi::jolt_world_gravity(world)
}

/// Build a passive SwingTwist ragdoll from the per-bone `parts` (skeleton + capsule parts + the
/// four constraint kinds), add it to the world, and return its index. Returns [`INVALID_BODY_ID`]
/// if `CreateRagdoll` failed. Built motors-off; setting the motor state drives it.
pub fn add_ragdoll(world: &mut UniquePtr<JoltWorld>, rig_uuid: u64, parts: &[BonePart]) -> u32 {
    bridge::ffi::jolt_add_ragdoll(world.pin_mut(), rig_uuid, parts)
}

/// Remove a ragdoll from the physics system and drop its handles, compacting the world's ragdoll
/// vector (subsequent indices shift down by one). A stale `index` is a no-op.
pub fn remove_ragdoll(world: &mut UniquePtr<JoltWorld>, index: u32) {
    bridge::ffi::jolt_remove_ragdoll(world.pin_mut(), index);
}

/// The number of parts (bodies) in a ragdoll; `0` for an out-of-range slot
/// (`Ragdoll::GetBodyCount`).
pub fn ragdoll_body_count(world: &UniquePtr<JoltWorld>, index: u32) -> u32 {
    bridge::ffi::jolt_ragdoll_body_count(world, index)
}

/// A ragdoll part's world transform: translation (`xyz`) + rotation (`xyzw`), for the per-bone
/// pose write-back.
pub fn ragdoll_part_transform(
    world: &UniquePtr<JoltWorld>,
    index: u32,
    part: u32,
) -> ([f32; 3], [f32; 4]) {
    let mut position = [0.0f32; 3];
    let mut rotation = [0.0f32, 0.0, 0.0, 1.0];
    bridge::ffi::jolt_ragdoll_part_transform(world, index, part, &mut position, &mut rotation);
    (position, rotation)
}

/// `true` if a ragdoll part's parent constraint is a `SwingTwist` (the only kind carrying motors).
/// `false` for a root part, a non-SwingTwist joint, or an out-of-range slot.
pub fn ragdoll_part_is_swing_twist(world: &UniquePtr<JoltWorld>, index: u32, part: u32) -> bool {
    bridge::ffi::jolt_ragdoll_part_is_swing_twist(world, index, part)
}

/// Set a SwingTwist part's motor state and body-space target orientation (`xyzw`). `active`
/// selects drive (`Position`) vs passive (`Off`). A no-op for a non-SwingTwist part or an
/// out-of-range slot.
pub fn ragdoll_set_swing_twist_motor(
    world: &mut UniquePtr<JoltWorld>,
    index: u32,
    part: u32,
    active: bool,
    target: [f32; 4],
) {
    bridge::ffi::jolt_ragdoll_set_swing_twist_motor(world.pin_mut(), index, part, active, &target);
}

/// Cast a ray `origin + dir * max_dist` through the narrow-phase query and return the closest
/// [`RayHit`]: world-space point/normal, the distance, and the struck body's raw `BodyID`
/// (`u32::MAX` / [`INVALID_BODY_ID`] on a miss). Read-only — it takes `&world`, never perturbing
/// the deterministic step. The safe layer maps `RayHit::body` back to its owner entity.
pub fn raycast(
    world: &UniquePtr<JoltWorld>,
    origin: [f32; 3],
    dir: [f32; 3],
    max_dist: f32,
) -> RayHit {
    bridge::ffi::jolt_raycast(world, &origin, &dir, max_dist)
}

/// Sweep a sphere of `radius` along `origin + dir * max_dist` (a thicker probe than a ray) and
/// return the closest [`RayHit`]. Read-only (`&world`). Returns a miss when the sweep clears
/// everything or the query sphere could not be built.
pub fn sphere_cast(
    world: &UniquePtr<JoltWorld>,
    origin: [f32; 3],
    dir: [f32; 3],
    radius: f32,
    max_dist: f32,
) -> RayHit {
    bridge::ffi::jolt_sphere_cast(world, &origin, &dir, radius, max_dist)
}

// The Jolt determinism flag set lives in one file shared with `build.rs` (which `include!`s it).
// It is build-only data with no runtime use, so it is compiled only for the tests that assert
// the determinism contract.
#[cfg(test)]
mod jolt_build_flags;

#[cfg(test)]
mod tests {
    use super::jolt_build_flags::JoltBuildFlags;
    use std::sync::Mutex;

    // Jolt's `Factory::sInstance` is a process-global, so the lifecycle tests must not run
    // concurrently against it. Serialize them through one lock.
    static JOLT_GLOBAL: Mutex<()> = Mutex::new(());

    // The object-layer raw discriminants, mirroring the shim's enum. Used by
    // `layer_matrix_matches` to assert the matrix has not drifted.
    const LAYER_STATIC: u8 = 0;
    const LAYER_MOVING: u8 = 1;
    const LAYER_CHARACTER: u8 = 2;
    const LAYER_DEBRIS: u8 = 3;
    const LAYER_SENSOR: u8 = 4;
    const LAYER_COUNT: u8 = 5;

    // The reference matrix, transcribed directly in Rust so the C++ shim and the expectation are
    // independently authored — a drift in either fails.
    fn expected_layers_collide(a: u8, b: u8) -> bool {
        if a == LAYER_SENSOR || b == LAYER_SENSOR {
            return !(a == LAYER_SENSOR && b == LAYER_SENSOR);
        }
        if a == LAYER_STATIC && b == LAYER_STATIC {
            return false;
        }
        if a == LAYER_DEBRIS && b == LAYER_DEBRIS {
            return false;
        }
        true
    }

    #[test]
    fn deterministic_flags_carry_the_frozen_set() {
        // The test binary is built for the target arch, so `cfg!(target_arch)` names the same
        // variant `build.rs` selects from `CARGO_CFG_TARGET_ARCH` for this build.
        let target_arch = if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "x86_64"
        };
        let flags = JoltBuildFlags::for_arch(target_arch);

        // The determinism define is present and single precision is the *absence* of
        // JPH_DOUBLE_PRECISION (it must never be defined here). This holds on every arch.
        assert!(
            flags
                .defines
                .iter()
                .any(|(k, _)| *k == "JPH_CROSS_PLATFORM_DETERMINISTIC"),
            "the determinism define must be in the flag set"
        );
        assert!(
            !flags
                .defines
                .iter()
                .any(|(k, _)| *k == "JPH_DOUBLE_PRECISION"),
            "single precision is the absence of JPH_DOUBLE_PRECISION; it must never be defined"
        );
        // FMADD is deliberately suppressed under determinism (contracted FMAs diverge across
        // micro-architectures), so its define must be absent, as must its `-mfma` flag.
        assert!(
            !flags.defines.iter().any(|(k, _)| *k == "JPH_USE_FMADD"),
            "JPH_USE_FMADD must be absent under cross-platform determinism"
        );
        assert!(
            !flags.arch_fp_flags.contains(&"-mfma"),
            "-mfma must be absent under cross-platform determinism"
        );

        // The determinism FP pairing is present on every arch.
        for expected in ["-ffp-model=precise", "-ffp-contract=off"] {
            assert!(
                flags.arch_fp_flags.contains(&expected),
                "{expected} must be in the confined arch/FP flag set"
            );
        }

        // The instruction-set selection is arch-specific: x86 carries its `-m*` SIMD flags,
        // while aarch64 gets NEON from Jolt's own `Core.h` and so carries no arch `-m*` flag.
        match target_arch {
            "aarch64" => {
                assert!(
                    !flags.arch_fp_flags.iter().any(|f| f.starts_with("-m")),
                    "aarch64 emits no `-m*` arch flag (NEON is baseline; Jolt defines JPH_USE_NEON)"
                );
                assert!(
                    !flags.defines.iter().any(|(k, _)| k.starts_with("JPH_USE_")),
                    "aarch64 defines no JPH_USE_* SIMD macro (Jolt's Core.h enables NEON itself)"
                );
            }
            _ => assert!(
                flags.arch_fp_flags.contains(&"-mavx2"),
                "x86-64 must carry its `-mavx2` instruction-set flag"
            ),
        }

        // Jolt's own `-Werror` is overridden for its TUs.
        assert!(flags.warning_flags.contains(&"-Wno-error"));

        // Threads link at link time (the C++ `-pthread`, link-only).
        assert!(flags.link_threads);
    }

    // The build script sets `cfg(jolt_deterministic)` only when both halves of the contract held
    // at compile time. Its absence fails the test build outright — a stronger guarantee than a
    // runtime assert, and proof the determinism flags were active when Jolt + the shim compiled,
    // not merely listed in the data table.
    #[cfg(not(jolt_deterministic))]
    compile_error!(
        "build.rs did not set cfg(jolt_deterministic): the determinism flags \
         (JPH_CROSS_PLATFORM_DETERMINISTIC + single precision) were not active"
    );

    #[test]
    fn jolt_links_at_version_5_3_0() {
        // A non-zero version proves the Jolt static archive linked and its headers compiled.
        let version = super::jolt_version();
        let major = version >> 16;
        let minor = (version >> 8) & 0xff;
        let patch = version & 0xff;
        assert_eq!(
            (major, minor, patch),
            (5, 3, 0),
            "vendored Jolt must be pinned at v5.3.0; got {major}.{minor}.{patch}"
        );
    }

    #[test]
    fn shim_compiled_deterministic_single_precision() {
        // End-to-end: the shim TU itself saw the determinism + single-precision defines (the
        // `#error` guards in `jolt_bridge.cpp` would have failed the build otherwise).
        assert!(super::is_deterministic());
        assert!(super::is_single_precision());
    }

    #[test]
    fn init_shutdown_idempotent() {
        let _guard = JOLT_GLOBAL.lock().unwrap();

        // Init then shutdown twice, both succeed.
        assert!(super::init().is_ok());
        assert!(super::init().is_ok()); // idempotent
        super::shutdown();
        super::shutdown(); // idempotent

        // And it can come back up after a full teardown.
        assert!(super::init().is_ok());
        super::shutdown();
    }

    #[test]
    fn layer_matrix_matches() {
        // The v1 collision matrix is load-bearing for the contact/trigger model. Assert the C++
        // shim agrees with the independently-transcribed reference for every layer pair, and that
        // it is symmetric.
        for a in 0..LAYER_COUNT {
            for b in 0..LAYER_COUNT {
                let got = super::layers_collide(a, b);
                assert_eq!(
                    got,
                    expected_layers_collide(a, b),
                    "layers_collide({a}, {b}) drifted from the v1 matrix"
                );
                assert_eq!(
                    got,
                    super::layers_collide(b, a),
                    "the collision matrix must be symmetric: ({a},{b}) vs ({b},{a})"
                );
            }
        }

        // The load-bearing rows spelled out by name, so the matrix's intent — not just its
        // self-consistency with the transcription above — is pinned: a sensor overlaps solids but
        // never another sensor; static-vs-static and debris-vs-debris are off; everything else on.
        assert!(super::layers_collide(LAYER_SENSOR, LAYER_STATIC));
        assert!(super::layers_collide(LAYER_SENSOR, LAYER_MOVING));
        assert!(super::layers_collide(LAYER_SENSOR, LAYER_CHARACTER));
        assert!(super::layers_collide(LAYER_SENSOR, LAYER_DEBRIS));
        assert!(!super::layers_collide(LAYER_SENSOR, LAYER_SENSOR));
        assert!(!super::layers_collide(LAYER_STATIC, LAYER_STATIC));
        assert!(!super::layers_collide(LAYER_DEBRIS, LAYER_DEBRIS));
        assert!(super::layers_collide(LAYER_MOVING, LAYER_STATIC));
        assert!(super::layers_collide(LAYER_MOVING, LAYER_MOVING));
        assert!(super::layers_collide(LAYER_CHARACTER, LAYER_MOVING));
        assert!(super::layers_collide(LAYER_DEBRIS, LAYER_CHARACTER));
    }

    #[test]
    fn create_empty_world() {
        let _guard = JOLT_GLOBAL.lock().unwrap();

        // Init globals → new world → init world → assert non-null and zero bodies → drop cleanly
        // → shutdown.
        assert!(super::init().is_ok());
        {
            let mut world = super::world_new().expect("world allocation must succeed");
            super::world_init(&mut world);
            assert_eq!(
                super::world_body_count(&world),
                0,
                "a freshly created world should hold no bodies"
            );
        } // world dropped here — frees the Jolt objects before shutdown
        super::shutdown();
    }

    #[test]
    fn drain_empty() {
        let _guard = JOLT_GLOBAL.lock().unwrap();

        // A stepped-zero empty world fires no contacts, so the drain returns an empty Vec — and
        // the call itself proves the C++ buffer ↔ Rust `Vec` round-trip works.
        assert!(super::init().is_ok());
        {
            let mut world = super::world_new().expect("world allocation must succeed");
            super::world_init(&mut world);
            super::world_step(&mut world, super::FIXED_STEP, 1);
            let contacts = super::drain_contacts(&mut world);
            assert!(
                contacts.is_empty(),
                "an empty stepped world must produce no contacts; got {}",
                contacts.len()
            );
        }
        super::shutdown();
    }
}
