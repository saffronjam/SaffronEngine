//! The `cxx` bridge surface: the FFI ABI `saffron-physics` speaks through.
//!
//! Everything crossing the wire is POD — plain scalars and the `#[cxx::bridge]` shared structs
//! below — so the seam is auditable in isolation with no engine-crate coupling. The C++ side
//! (`shim/jolt_bridge.cpp`) owns the Jolt-specific work: the opaque [`JoltWorld`] holds the
//! `PhysicsSystem` + `TempAllocator` + `JobSystem` + the four virtual shim classes `cxx` cannot
//! synthesize (the three layer filters and the `ContactListener`), in the teardown order Jolt
//! requires.

/// The `cxx` bridge module. The C++ counterpart is generated into `bridge.rs.h` and implemented
/// against vendored Jolt by `shim/jolt_bridge.cpp`.
#[cxx::bridge(namespace = "saffron::physics")]
pub mod ffi {
    /// A raw contact transition captured on a Jolt job thread, buffered C++-side and handed to
    /// Rust by [`jolt_drain_contacts`]. Only `BodyID`s (raw `u32` index+sequence) and a
    /// representative point/normal are recorded; the body-id → entity mapping is the safe layer's
    /// job on the sim thread. `point`/`normal` are plain `xyz`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct PendingContact {
        /// Raw `BodyID` of the first body (`GetIndexAndSequenceNumber()`).
        a: u32,
        /// Raw `BodyID` of the second body.
        b: u32,
        /// A representative world-space contact point; zero for an end (`begin == false`) event.
        point: [f32; 3],
        /// World-space contact normal (body1 → body2); zero for an end event.
        normal: [f32; 3],
        /// `true` for `OnContactAdded` (begin), `false` for `OnContactRemoved` (end).
        begin: bool,
    }

    /// The Jolt-free POD a body is created from — every `BodyCreationSettings` field the safe
    /// layer resolves from the `Collider`/`Rigidbody` components, flattened to scalars so the shim
    /// never sees a glam type. The quaternion crosses as plain `xyzw` (glam's storage order *is*
    /// Jolt's). `motion` and `object_layer` are raw discriminants; `allowed_dofs` is the Jolt
    /// `EAllowedDOFs` bitmask (`0b111111` = all). `shape` selects the per-shape build;
    /// `half_extents`/`offset` size and place the analytic shapes (the convex-radius/degenerate
    /// clamps are the shim's job), and the ConvexHull/Mesh cooked geometry is fed alongside as
    /// index-ordered slices. The damping/mass/DOF fields apply only when `motion` is Dynamic.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct BodyCreate {
        /// The collider shape (`0` Box, `1` Sphere, `2` Capsule, `3` ConvexHull, `4` Mesh) — the
        /// raw discriminant of the scene `Shape` enum.
        shape: u8,
        /// Per-shape size: Box half-extents `xyz`; Sphere radius in `.x`; Capsule radius `.x` +
        /// cylinder half-height `.y` (Y-up). Already clamped above the degenerate floor by the
        /// caller; the shim re-clamps so any input is safe. Ignored for ConvexHull/Mesh (their
        /// geometry is the cooked slices).
        half_extents: [f32; 3],
        /// Local-space shape centre offset; non-zero wraps the shape in a `RotatedTranslatedShape`.
        offset: [f32; 3],
        /// World-space body position.
        position: [f32; 3],
        /// World-space body rotation, `xyzw`.
        rotation: [f32; 4],
        /// Raw `MotionType` discriminant (`0` Static, `1` Kinematic, `2` Dynamic).
        motion: u8,
        /// Raw `ObjectLayer` discriminant the layer matrix keys on.
        object_layer: u8,
        /// Trigger volume: overlaps report, contacts do not solve.
        is_sensor: bool,
        /// Surface friction.
        friction: f32,
        /// Surface restitution.
        restitution: f32,
        /// Per-second linear velocity decay (Dynamic only).
        linear_damping: f32,
        /// Per-second angular velocity decay (Dynamic only).
        angular_damping: f32,
        /// Gravity scale (Dynamic only).
        gravity_factor: f32,
        /// Body mass in kg, fed through `CalculateInertia` (Dynamic only).
        mass: f32,
        /// Jolt `EAllowedDOFs` bitmask from the per-axis locks (Dynamic only; `0b111111` = all).
        allowed_dofs: u8,
    }

    /// The Jolt-free POD a `CharacterVirtual` is created from — the capsule dimensions, the max
    /// walkable slope, and the world-space spawn position. The controller params
    /// (`max_speed`/`gravity_factor`/`max_step_height`) stay on the safe-layer
    /// `CharacterController` and reach the shim per-step, not at create time.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct CharacterCreate {
        /// Capsule radius (the collider's `half_extents.x`), clamped above `0.05`.
        radius: f32,
        /// Capsule half-height (the collider's `half_extents.y`), clamped above `0.05`.
        half_height: f32,
        /// Maximum walkable ground angle in radians; steeper is treated as a wall.
        max_slope_angle: f32,
        /// World-space spawn position (`xyz`); the sweep starts here.
        position: [f32; 3],
    }

    /// One closest hit from a read-only scene query ([`jolt_raycast`] / [`jolt_sphere_cast`]),
    /// flattened to POD. `hit` is `false` (and every other field zero) when the ray/sweep struck
    /// nothing along its length. `body` is the raw `BodyID` of the struck body — the safe layer maps
    /// it back to its owner entity via `index_by_body_id`; the shim never sees an entity uuid.
    /// `point`/`normal` cross as plain `xyz`.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct RayHit {
        /// Whether the ray/sweep hit anything.
        hit: bool,
        /// The struck body's raw `BodyID` (`GetIndexAndSequenceNumber`); `u32::MAX`
        /// (`cInvalidBodyID`) on a miss. The safe layer resolves it to an owner entity.
        body: u32,
        /// World-space contact point.
        point: [f32; 3],
        /// World-space surface normal at the hit.
        normal: [f32; 3],
        /// Distance along the ray from the origin (`fraction * max_dist`).
        distance: f32,
    }

    /// One ragdoll bone's part + its parent constraint, flattened to POD. The shim builds a
    /// `RagdollSettings` skeleton joint, a capsule part, and (for a non-root) the constraint its
    /// `joint` kind selects, seeded at this bone's current world pose. Rotation is `xyzw`. The
    /// parts cross as a contiguous slice in bone-index order — that order is load-bearing for
    /// determinism (it fixes the part/constraint indices).
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct BonePart {
        /// Parent bone index, or `-1` for the root (drives `Skeleton::AddJoint` + whether a
        /// `mToParent` constraint is built).
        parent_index: i32,
        /// World-space part position (`xyz`) at build time (the bone's current world pose).
        position: [f32; 3],
        /// World-space part rotation (`xyzw`) at build time.
        rotation: [f32; 4],
        /// Capsule radius (`shape_half_extents.x`), clamped above `0.03`.
        radius: f32,
        /// Capsule half-height (`shape_half_extents.y`), clamped above `0.03`.
        half_height: f32,
        /// Part mass in kg, clamped above `0.01`, fed through `CalculateInertia`.
        mass: f32,
        /// The joint kind for the parent constraint (`0` Fixed, `1` Hinge, `2` SwingTwist,
        /// `3` Free) — the raw discriminant of the scene `Joint` enum.
        joint: u8,
        /// Swing/twist cone limits in radians (`normal`, `plane`, `twist`); a `~0` component falls
        /// back to the `0.7` rad default so an unfitted bone is floppy, not rigid.
        swing_twist_limits: [f32; 3],
        /// PD motor spring frequency (Hz); `~0` falls back to `8.0` (a SwingTwist carries the motor
        /// *settings* even while passive).
        drive_stiffness: f32,
        /// PD motor spring damping; `~0` falls back to `1.0`.
        drive_damping: f32,
        /// PD motor torque limit; `~0` falls back to `1000.0`.
        drive_max_force: f32,
    }

    unsafe extern "C++" {
        include!("jolt_bridge.h");

        /// The opaque shim world: owns the `PhysicsSystem`, the `TempAllocator`, the
        /// `JobSystemThreadPool`, and the four virtual shim instances, declared so the filters and
        /// the contact listener outlive `system` (Jolt borrows them for the world's lifetime). Its
        /// C++ destructor owns the Jolt teardown order; the Rust side only drops the `UniquePtr`.
        type JoltWorld;

        /// Install the Jolt globals: the default allocator, the trace/assert hooks, the type
        /// `Factory`, and `RegisterTypes`. Idempotent — a second call is a no-op. Returns `true`
        /// on success.
        fn jolt_init() -> bool;

        /// Tear down the Jolt globals: `UnregisterTypes` and destroy the `Factory`. Idempotent —
        /// safe without a prior [`jolt_init`] or twice in a row.
        fn jolt_shutdown();

        /// The compiled-in Jolt version, `(major << 16) | (minor << 8) | patch`. Non-zero proves
        /// the static archive linked.
        fn jolt_version() -> u32;

        /// `true` if the shim TU compiled with `JPH_CROSS_PLATFORM_DETERMINISTIC`. Guaranteed by a
        /// build-time `#error` guard; surfaced so a test can assert the contract end-to-end.
        fn jolt_is_deterministic() -> bool;

        /// `true` if the shim TU compiled in single precision (`JPH_DOUBLE_PRECISION` not defined).
        fn jolt_is_single_precision() -> bool;

        /// The v1 collision matrix (symmetric), implemented C++-side in the shim. `a`/`b` are
        /// `ObjectLayer` raw discriminants. Exposed so a test can assert the matrix.
        fn jolt_layers_collide(a: u8, b: u8) -> bool;

        /// Allocate a fresh world: the `TempAllocator` (10 MiB), the `JobSystemThreadPool` (the
        /// canonical Jolt bounds, auto thread count), the four shim instances, and an
        /// uninitialized `PhysicsSystem`. Call [`jolt_world_init`] before use. The returned
        /// pointer is non-null on success.
        fn jolt_world_new() -> UniquePtr<JoltWorld>;

        /// Wire the filters into `system.Init` (1024 bodies / 1024 body pairs / 1024 contact
        /// constraints), set gravity `(0, -9.81, 0)`, and install the contact listener.
        fn jolt_world_init(world: Pin<&mut JoltWorld>);

        /// The live body count (`PhysicsSystem::GetNumBodies`).
        fn jolt_world_body_count(world: &JoltWorld) -> u32;

        /// Advance the world by one fixed substep with `collision_steps` solver iterations,
        /// running the `JobSystem` + `TempAllocator`. Exposed here so the bridge round-trips a
        /// step; the safe layer's fixed-step accumulator drives it.
        fn jolt_world_step(world: Pin<&mut JoltWorld>, dt: f32, collision_steps: i32);

        /// Swap-and-clear the contact listener's mutex-guarded buffer and hand the records to Rust.
        /// Called on the sim thread, never from a Jolt callback.
        fn jolt_drain_contacts(world: Pin<&mut JoltWorld>) -> Vec<PendingContact>;

        /// Build the collision shape `create.shape` selects, make a `BodyCreationSettings`, and
        /// `CreateAndAddBody`. The analytic shapes (Box/Sphere/Capsule) size from
        /// `create.half_extents` (with the convex-radius/degenerate clamps); ConvexHull builds from
        /// `hull_points` (flattened `xyz`, in index order) and Mesh from `mesh_vertices` (flattened
        /// `xyz`) + `mesh_indices` (flat triangle list). Every shape is offset-wrapped when
        /// `create.offset` is non-zero. Returns the new body's raw id
        /// (`GetIndexAndSequenceNumber`), or `u32::MAX` (`cInvalidBodyID`) when the shape or body
        /// create failed (an empty cook, a degenerate hull). Mesh-on-Dynamic is rejected on the
        /// safe side, so the shim never sees it.
        fn jolt_create_body(
            world: Pin<&mut JoltWorld>,
            create: &BodyCreate,
            hull_points: &[f32],
            mesh_vertices: &[f32],
            mesh_indices: &[u32],
        ) -> u32;

        /// Create every body in `creates` (analytic shapes only — the geometry slices of
        /// [`jolt_create_body`] never apply) and insert them through Jolt's batched broadphase
        /// path (`BodyInterface::AddBodiesPrepare` + `AddBodiesFinalize`, `DontActivate`).
        /// Returns one raw `BodyID` per input row, position-aligned; a failed create yields
        /// the invalid id sentinel in its slot and the rest of the batch still lands.
        fn jolt_create_static_batch(world: Pin<&mut JoltWorld>, creates: &[BodyCreate])
        -> Vec<u32>;

        /// Remove and destroy every listed body in one batch (`BodyInterface::RemoveBodies` +
        /// `DestroyBodies`). Invalid-id sentinels in `ids` are skipped.
        fn jolt_remove_bodies(world: Pin<&mut JoltWorld>, ids: &[u32]);

        /// Read a body's world position+rotation (`BodyInterface::GetPositionAndRotation`) into
        /// `position` (`xyz`) and `rotation` (`xyzw`). The dynamic write-back reads this each step.
        /// `id` is a raw `BodyID`.
        fn jolt_body_position_rotation(
            world: &JoltWorld,
            id: u32,
            position: &mut [f32; 3],
            rotation: &mut [f32; 4],
        );

        /// A body's world position (`BodyInterface::GetPosition`), for the read-only body list.
        /// `id` is a raw `BodyID`.
        fn jolt_body_position(world: &JoltWorld, id: u32) -> [f32; 3];

        /// Whether a body is awake (`BodyInterface::IsActive`), for the read-only body list. `id`
        /// is a raw `BodyID`.
        fn jolt_body_is_active(world: &JoltWorld, id: u32) -> bool;

        /// A body's current linear velocity (`BodyInterface::GetLinearVelocity`). `id` is a raw
        /// `BodyID`.
        fn jolt_body_linear_velocity(world: &JoltWorld, id: u32) -> [f32; 3];

        /// A body's current angular velocity (`BodyInterface::GetAngularVelocity`, radians per
        /// second about each world axis). `id` is a raw `BodyID`.
        fn jolt_body_angular_velocity(world: &JoltWorld, id: u32) -> [f32; 3];

        /// Activate the body, then apply a center-of-mass impulse (`ActivateBody` + `AddImpulse`).
        /// `id` is a raw `BodyID`; the safe layer only calls this for a Dynamic body.
        fn jolt_body_add_impulse(world: Pin<&mut JoltWorld>, id: u32, impulse: &[f32; 3]);

        /// Activate the body, then add a force for the next step (`ActivateBody` + `AddForce`).
        /// `id` is a raw `BodyID`; Dynamic-only at the call site.
        fn jolt_body_add_force(world: Pin<&mut JoltWorld>, id: u32, force: &[f32; 3]);

        /// Activate the body, then set its linear velocity (`ActivateBody` + `SetLinearVelocity`).
        /// `id` is a raw `BodyID`; Dynamic-only at the call site.
        fn jolt_body_set_linear_velocity(world: Pin<&mut JoltWorld>, id: u32, velocity: &[f32; 3]);

        /// Move a Kinematic body toward `position`/`rotation` (`xyzw`) over `dt` via
        /// `BodyInterface::MoveKinematic`, which derives the body's velocity from the swept motion
        /// so it imparts contact velocity to the dynamics it hits — never a teleport (which gives
        /// zero contact velocity). `dt` must be the same `PhysicsFixedStep` that feeds `Update`, so
        /// the derived velocity matches the step. `id` is a raw `BodyID`; the safe layer only calls
        /// this for a Kinematic body.
        fn jolt_move_kinematic(
            world: Pin<&mut JoltWorld>,
            id: u32,
            position: &[f32; 3],
            rotation: &[f32; 4],
            dt: f32,
        );

        /// Create a `CharacterVirtual` from a capsule + slope angle, seeded at `position`, and store
        /// it in the world's character vector. Returns the new character's index (its slot in that
        /// vector), or `u32::MAX` if the capsule shape create failed. The controller logic
        /// (gravity/clamp) is the safe layer's job; the shim only owns the sweep object.
        fn jolt_add_character(world: Pin<&mut JoltWorld>, create: &CharacterCreate) -> u32;

        /// Set a character's linear velocity for the next [`jolt_character_extended_update`]
        /// (`CharacterVirtual::SetLinearVelocity`). `index` is a character slot.
        fn jolt_character_set_linear_velocity(
            world: Pin<&mut JoltWorld>,
            index: u32,
            velocity: &[f32; 3],
        );

        /// Advance one character by `dt` against the just-settled world: `ExtendedUpdate` with the
        /// Character-layer broad-phase + layer filters, `mWalkStairsStepUp = (0, step_up, 0)`, and
        /// the supplied applied gravity. `gravity` is the gravity vector already scaled by the
        /// controller's gravity factor.
        fn jolt_character_extended_update(
            world: Pin<&mut JoltWorld>,
            index: u32,
            dt: f32,
            gravity: &[f32; 3],
            step_up: f32,
        );

        /// `true` if the character is resting on walkable ground (`GetGroundState() == OnGround`).
        /// `index` is a character slot.
        fn jolt_character_on_ground(world: &JoltWorld, index: u32) -> bool;

        /// A character's resolved world position (`CharacterVirtual::GetPosition`). `index` is a
        /// character slot.
        fn jolt_character_position(world: &JoltWorld, index: u32) -> [f32; 3];

        /// The world's gravity vector (`PhysicsSystem::GetGravity`). The safe layer integrates the
        /// controller's vertical velocity against this each substep.
        fn jolt_world_gravity(world: &JoltWorld) -> [f32; 3];

        /// Build a `RagdollSettings` from the per-bone parts (skeleton joints + capsule parts +
        /// the four constraint kinds via the bone's joint type), `Stabilize`,
        /// `CalculateBodyIndexToConstraintIndex`, `CreateRagdoll(0, rig_uuid, &system)`, then
        /// `AddToPhysicsSystem(Activate)`. Built **passive**: a SwingTwist part carries its motor
        /// *settings*, but the motor *state* stays `Off`. Returns the new ragdoll's index (its slot
        /// in the world's ragdoll vector), or `u32::MAX` if `CreateRagdoll` failed.
        fn jolt_add_ragdoll(world: Pin<&mut JoltWorld>, rig_uuid: u64, parts: &[BonePart]) -> u32;

        /// Remove a ragdoll from the physics system (`Ragdoll::RemoveFromPhysicsSystem`) and drop
        /// its handles, compacting the world's ragdoll vector. Subsequent ragdoll indices shift
        /// down by one; the safe layer rebuilds its index map after a removal. A stale `index` is
        /// a no-op.
        fn jolt_remove_ragdoll(world: Pin<&mut JoltWorld>, index: u32);

        /// The number of bodies (parts) in a ragdoll (`Ragdoll::GetBodyCount`). `index` is a
        /// ragdoll slot; an out-of-range slot returns `0`.
        fn jolt_ragdoll_body_count(world: &JoltWorld, index: u32) -> u32;

        /// A ragdoll part's world transform, flattened to a translation (`xyz`) + rotation
        /// (`xyzw`). Reads `BodyInterface::GetWorldTransform(GetBodyID(part))` and decomposes it.
        /// `index` is a ragdoll slot, `part` a part index `< jolt_ragdoll_body_count`.
        fn jolt_ragdoll_part_transform(
            world: &JoltWorld,
            index: u32,
            part: u32,
            position: &mut [f32; 3],
            rotation: &mut [f32; 4],
        );

        /// The `SwingTwist` constraint subtype check for a part's parent constraint
        /// (`GetConstraintIndexForBodyIndex` + `GetSubType() == SwingTwist`). Only a SwingTwist
        /// bone carries the per-bone motors the drive sets. Returns `false` for a root part (no
        /// parent constraint), a non-SwingTwist joint, or an out-of-range slot.
        fn jolt_ragdoll_part_is_swing_twist(world: &JoltWorld, index: u32, part: u32) -> bool;

        /// Set a SwingTwist part's swing + twist motor state and target orientation (body-space).
        /// `active` selects `Position` (drive) vs `Off` (passive); the glam quaternion (`xyzw`)
        /// feeds `SetTargetOrientationBS` directly. A no-op when the part is not a SwingTwist or
        /// the slot is out of range.
        fn jolt_ragdoll_set_swing_twist_motor(
            world: Pin<&mut JoltWorld>,
            index: u32,
            part: u32,
            active: bool,
            target: &[f32; 4],
        );

        /// Cast a ray `origin + dir * max_dist` through the narrow-phase query and return the
        /// closest hit: the world-space point (`GetPointOnRay(fraction)`), the surface normal read
        /// under a `BodyLockRead` (`GetWorldSpaceSurfaceNormal`), the distance (`fraction *
        /// max_dist`), and the struck body's raw `BodyID`. Read-only — it does not perturb the
        /// deterministic step, so the safe layer takes `&self`. Returns a miss
        /// (`RayHit { hit: false, .. }`) when nothing lies along the ray.
        fn jolt_raycast(
            world: &JoltWorld,
            origin: &[f32; 3],
            dir: &[f32; 3],
            max_dist: f32,
        ) -> RayHit;

        /// Sweep a sphere of `radius` along `origin + dir * max_dist` (a thicker probe than a ray —
        /// it catches an edge a thin ray of the same origin/dir grazes) and return the closest hit
        /// via a `ClosestHitCollisionCollector<CastShapeCollector>`: the world-space contact point
        /// (`origin + mContactPointOn2`, relative to the sweep's base offset), the normal
        /// (`-mPenetrationAxis.Normalized()`), the distance (`fraction * max_dist`), and the struck
        /// body's raw `BodyID`. Read-only (`&self` on the safe layer). Returns a miss when the
        /// sweep clears everything, or when the query sphere shape could not be built.
        fn jolt_sphere_cast(
            world: &JoltWorld,
            origin: &[f32; 3],
            dir: &[f32; 3],
            radius: f32,
            max_dist: f32,
        ) -> RayHit;
    }
}
