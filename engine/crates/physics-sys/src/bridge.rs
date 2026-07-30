//! The `cxx` bridge surface: the FFI ABI `saffron-physics` speaks through.
//!
//! Everything crossing the wire is POD — plain scalars and the `#[cxx::bridge]` shared structs
//! below. `shim/jolt_bridge.cpp` owns the Jolt-specific work; the opaque [`JoltWorld`] declares
//! its members in the teardown order Jolt requires.

/// The `cxx` bridge module. The C++ counterpart is generated into `bridge.rs.h` and implemented
/// against vendored Jolt by `shim/jolt_bridge.cpp`.
#[cxx::bridge(namespace = "saffron::physics")]
pub mod ffi {
    /// A raw contact transition captured on a Jolt job thread, buffered C++-side and handed to
    /// Rust by [`jolt_drain_contacts`]. The body-id to entity mapping is the safe layer's job on
    /// the sim thread.
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

    /// The `BodyCreationSettings` fields the safe layer resolves from the `Collider`/`Rigidbody`
    /// components, flattened to scalars. Quaternions cross as `xyzw`, which is glam's storage order
    /// and Jolt's alike. The damping, mass, and DOF fields apply only when `motion` is Dynamic.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct BodyCreate {
        /// The raw discriminant of the scene `Shape` enum: `0` Box, `1` Sphere, `2` Capsule,
        /// `3` ConvexHull, `4` Mesh.
        shape: u8,
        /// Per-shape size: Box half-extents `xyz`; Sphere radius in `.x`; Capsule radius `.x` plus
        /// cylinder half-height `.y` (Y-up). The shim re-clamps above the degenerate floor, so any
        /// input is safe. Ignored for ConvexHull/Mesh, whose geometry is the cooked slices.
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

    /// The capsule dimensions, max walkable slope, and spawn position a `CharacterVirtual` is
    /// created from. The controller params stay on the safe-layer `CharacterController` and reach
    /// the shim per step, not at create time.
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

    /// One closest hit from a read-only scene query. Every field is zero when `hit` is `false`.
    /// The shim never sees an entity uuid; the safe layer maps `body` back to its owner.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct RayHit {
        /// Whether the ray/sweep hit anything.
        hit: bool,
        /// The struck body's raw `BodyID` (`GetIndexAndSequenceNumber`); `u32::MAX`
        /// (`cInvalidBodyID`) on a miss.
        body: u32,
        /// World-space contact point.
        point: [f32; 3],
        /// World-space surface normal at the hit.
        normal: [f32; 3],
        /// Distance along the ray from the origin (`fraction * max_dist`).
        distance: f32,
    }

    /// One ragdoll bone's part and its parent constraint. The shim builds a `RagdollSettings`
    /// skeleton joint, a capsule part, and, for a non-root, the constraint its `joint` kind
    /// selects, seeded at the bone's current world pose. The parts cross as a contiguous slice in
    /// bone-index order, and that order fixes the part and constraint indices, so it is
    /// load-bearing for determinism.
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
        /// back to `0.7` rad, so an unfitted bone is floppy rather than rigid.
        swing_twist_limits: [f32; 3],
        /// PD motor spring frequency (Hz); `~0` falls back to `8.0`. A SwingTwist carries the
        /// motor settings even while passive.
        drive_stiffness: f32,
        /// PD motor spring damping; `~0` falls back to `1.0`.
        drive_damping: f32,
        /// PD motor torque limit; `~0` falls back to `1000.0`.
        drive_max_force: f32,
    }

    unsafe extern "C++" {
        include!("jolt_bridge.h");

        /// The opaque shim world: the `PhysicsSystem`, the `TempAllocator`, the
        /// `JobSystemThreadPool`, and the four virtual shim instances, declared so the filters and
        /// the contact listener outlive `system` — Jolt borrows them for the world's lifetime. Its
        /// C++ destructor owns the teardown order; the Rust side only drops the `UniquePtr`.
        type JoltWorld;

        /// Installs the default allocator, the trace/assert hooks, the type `Factory`, and
        /// `RegisterTypes`. Idempotent; `true` on success.
        fn jolt_init() -> bool;

        /// `UnregisterTypes` then destroys the `Factory`. Idempotent, and safe without a prior
        /// [`jolt_init`].
        fn jolt_shutdown();

        /// The compiled-in Jolt version, `(major << 16) | (minor << 8) | patch`.
        fn jolt_version() -> u32;

        /// `true` if the shim TU compiled with `JPH_CROSS_PLATFORM_DETERMINISTIC`.
        fn jolt_is_deterministic() -> bool;

        /// `true` if the shim TU compiled in single precision.
        fn jolt_is_single_precision() -> bool;

        /// The symmetric collision matrix over `ObjectLayer` raw discriminants.
        fn jolt_layers_collide(a: u8, b: u8) -> bool;

        /// Allocates the `TempAllocator` (10 MiB), the `JobSystemThreadPool`, the four shim
        /// instances, and an uninitialized `PhysicsSystem`. Call [`jolt_world_init`] before use.
        fn jolt_world_new() -> UniquePtr<JoltWorld>;

        /// Wires the filters into `system.Init` (1024 bodies / body pairs / contact constraints),
        /// sets gravity `(0, -9.81, 0)`, and installs the contact listener.
        fn jolt_world_init(world: Pin<&mut JoltWorld>);

        /// `PhysicsSystem::GetNumBodies`.
        fn jolt_world_body_count(world: &JoltWorld) -> u32;

        /// One fixed substep with `collision_steps` solver iterations, over the `JobSystem` and
        /// `TempAllocator`.
        fn jolt_world_step(world: Pin<&mut JoltWorld>, dt: f32, collision_steps: i32);

        /// Swaps and clears the contact listener's mutex-guarded buffer. Called on the sim thread,
        /// never from a Jolt callback.
        fn jolt_drain_contacts(world: Pin<&mut JoltWorld>) -> Vec<PendingContact>;

        /// Builds the shape `create.shape` selects and `CreateAndAddBody`s it, offset-wrapped when
        /// `create.offset` is non-zero. ConvexHull reads `hull_points` (flattened `xyz` in index
        /// order), Mesh reads `mesh_vertices` plus `mesh_indices` (a flat triangle list), and the
        /// analytic shapes read neither. `u32::MAX` when the shape or body create failed.
        fn jolt_create_body(
            world: Pin<&mut JoltWorld>,
            create: &BodyCreate,
            hull_points: &[f32],
            mesh_vertices: &[f32],
            mesh_indices: &[u32],
        ) -> u32;

        /// Creates every analytic-shape body in `creates` through the batched broadphase path
        /// (`AddBodiesPrepare` + `AddBodiesFinalize`, `DontActivate`), returning one raw `BodyID`
        /// per input row, position-aligned. A failed create yields the invalid sentinel in its slot
        /// while the rest of the batch still lands.
        fn jolt_create_static_batch(world: Pin<&mut JoltWorld>, creates: &[BodyCreate])
        -> Vec<u32>;

        /// `BodyInterface::RemoveBodies` then `DestroyBodies` in one batch, skipping invalid-id
        /// sentinels.
        fn jolt_remove_bodies(world: Pin<&mut JoltWorld>, ids: &[u32]);

        /// `BodyInterface::GetPositionAndRotation` into `position` (`xyz`) and `rotation`
        /// (`xyzw`).
        fn jolt_body_position_rotation(
            world: &JoltWorld,
            id: u32,
            position: &mut [f32; 3],
            rotation: &mut [f32; 4],
        );

        /// `BodyInterface::GetPosition`.
        fn jolt_body_position(world: &JoltWorld, id: u32) -> [f32; 3];

        /// `BodyInterface::IsActive`.
        fn jolt_body_is_active(world: &JoltWorld, id: u32) -> bool;

        /// `BodyInterface::GetLinearVelocity`.
        fn jolt_body_linear_velocity(world: &JoltWorld, id: u32) -> [f32; 3];

        /// `BodyInterface::GetAngularVelocity`, radians per second about each world axis.
        fn jolt_body_angular_velocity(world: &JoltWorld, id: u32) -> [f32; 3];

        /// `ActivateBody` then `AddImpulse` at the center of mass. Dynamic bodies only.
        fn jolt_body_add_impulse(world: Pin<&mut JoltWorld>, id: u32, impulse: &[f32; 3]);

        /// `ActivateBody` then `AddForce` for the next step. Dynamic bodies only.
        fn jolt_body_add_force(world: Pin<&mut JoltWorld>, id: u32, force: &[f32; 3]);

        /// `ActivateBody` then `SetLinearVelocity`. Dynamic bodies only.
        fn jolt_body_set_linear_velocity(world: Pin<&mut JoltWorld>, id: u32, velocity: &[f32; 3]);

        /// `BodyInterface::MoveKinematic`, which derives the body's velocity from the swept motion
        /// so it imparts contact velocity where a teleport would impart none. `dt` must be the same
        /// fixed step that feeds `Update`, or the derived velocity will not match it. Kinematic
        /// bodies only.
        fn jolt_move_kinematic(
            world: Pin<&mut JoltWorld>,
            id: u32,
            position: &[f32; 3],
            rotation: &[f32; 4],
            dt: f32,
        );

        /// Creates a `CharacterVirtual` seeded at `position` and stores it in the world's character
        /// vector, returning its slot or `u32::MAX` if the capsule shape create failed.
        fn jolt_add_character(world: Pin<&mut JoltWorld>, create: &CharacterCreate) -> u32;

        /// `CharacterVirtual::SetLinearVelocity` for the next extended update.
        fn jolt_character_set_linear_velocity(
            world: Pin<&mut JoltWorld>,
            index: u32,
            velocity: &[f32; 3],
        );

        /// `ExtendedUpdate` against the just-settled world, with the Character-layer filters,
        /// `mWalkStairsStepUp = (0, step_up, 0)`, and a `gravity` already scaled by the
        /// controller's gravity factor.
        fn jolt_character_extended_update(
            world: Pin<&mut JoltWorld>,
            index: u32,
            dt: f32,
            gravity: &[f32; 3],
            step_up: f32,
        );

        /// `GetGroundState() == OnGround`.
        fn jolt_character_on_ground(world: &JoltWorld, index: u32) -> bool;

        /// `CharacterVirtual::GetPosition`.
        fn jolt_character_position(world: &JoltWorld, index: u32) -> [f32; 3];

        /// `PhysicsSystem::GetGravity`.
        fn jolt_world_gravity(world: &JoltWorld) -> [f32; 3];

        /// Builds a `RagdollSettings` from the per-bone parts, `Stabilize`s,
        /// `CalculateBodyIndexToConstraintIndex`, `CreateRagdoll(0, rig_uuid, &system)`, then
        /// `AddToPhysicsSystem(Activate)`. Built passive: a SwingTwist part carries its motor
        /// settings but the motor state stays `Off`. Returns the ragdoll slot, or `u32::MAX` if
        /// `CreateRagdoll` failed.
        fn jolt_add_ragdoll(world: Pin<&mut JoltWorld>, rig_uuid: u64, parts: &[BonePart]) -> u32;

        /// `Ragdoll::RemoveFromPhysicsSystem` then drops its handles, compacting the world's
        /// ragdoll vector — every later index shifts down by one, so the safe layer must rebuild
        /// its index map. A stale `index` is a no-op.
        fn jolt_remove_ragdoll(world: Pin<&mut JoltWorld>, index: u32);

        /// `Ragdoll::GetBodyCount`, or `0` for an out-of-range slot.
        fn jolt_ragdoll_body_count(world: &JoltWorld, index: u32) -> u32;

        /// `BodyInterface::GetWorldTransform(GetBodyID(part))`, decomposed into a translation
        /// (`xyz`) and rotation (`xyzw`).
        fn jolt_ragdoll_part_transform(
            world: &JoltWorld,
            index: u32,
            part: u32,
            position: &mut [f32; 3],
            rotation: &mut [f32; 4],
        );

        /// Whether a part's parent constraint is a `SwingTwist`, the only kind carrying the
        /// per-bone motors the drive sets. `false` for a root part or an out-of-range slot.
        fn jolt_ragdoll_part_is_swing_twist(world: &JoltWorld, index: u32, part: u32) -> bool;

        /// Sets a SwingTwist part's motor state and body-space target orientation. `active`
        /// selects `Position` over `Off`; the `xyzw` quaternion feeds `SetTargetOrientationBS`
        /// directly. A no-op for a non-SwingTwist part or an out-of-range slot.
        fn jolt_ragdoll_set_swing_twist_motor(
            world: Pin<&mut JoltWorld>,
            index: u32,
            part: u32,
            active: bool,
            target: &[f32; 4],
        );

        /// The closest narrow-phase hit along `origin + dir * max_dist`: `GetPointOnRay(fraction)`,
        /// the `GetWorldSpaceSurfaceNormal` read under a `BodyLockRead`, `fraction * max_dist`, and
        /// the struck body's raw `BodyID`. Does not perturb the step, so the safe layer takes
        /// `&self`.
        fn jolt_raycast(
            world: &JoltWorld,
            origin: &[f32; 3],
            dir: &[f32; 3],
            max_dist: f32,
        ) -> RayHit;

        /// The closest `ClosestHitCollisionCollector<CastShapeCollector>` hit from sweeping a
        /// sphere of `radius` along `origin + dir * max_dist`: the contact point
        /// `origin + mContactPointOn2`, the normal `-mPenetrationAxis.Normalized()`,
        /// `fraction * max_dist`, and the struck body's raw `BodyID`. A miss when the sweep clears
        /// everything or the query sphere could not be built.
        fn jolt_sphere_cast(
            world: &JoltWorld,
            origin: &[f32; 3],
            dir: &[f32; 3],
            radius: f32,
            max_dist: f32,
        ) -> RayHit;
    }
}
