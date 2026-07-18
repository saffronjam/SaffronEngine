+++
title = 'Active ragdoll'
weight = 9
+++

# Active ragdoll

An active ragdoll uses constraint motors to pull simulated bones toward an animated pose. Per-bone
blend weights then decide how much of the simulated pose reaches the skeleton, which supports limp,
motor-driven, and mixed responses through the same `PoseOverride` path.

## Motors and pose weights

Each live [Jolt ragdoll](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/class_ragdoll.html) has two
independent controls:

- `active` enables the motors on `SwingTwist` joints. An inactive ragdoll moves under gravity and
  its joint limits.
- `bodyWeight` sets the target physics weight for every bone. A per-bone `weight` can override one
  entry. Weight `0` selects animation and weight `1` selects physics.

`World::advance_ragdoll_blend` moves each live weight toward its target at six weight units per
second. The ramp prevents a discontinuity when a limb enters or leaves simulation. Motor state and
pose weight remain separate, so a motor-driven body can influence the world while its rendered bone
uses any blend between animation and physics.

## Motor targets

`BonePhysics` stores each bone's `drive_stiffness`, `drive_damping`, and `drive_max_force`. During
ragdoll creation, the C++ bridge converts those fields into Jolt `MotorSettings` for both axes of a
`SwingTwistConstraint`. Values near zero use an 8 Hz spring, damping `1`, and a torque limit of
`1000`.

Animation records each rig's final local pose in `AnimationRuntime::last_pose`. Before physics
steps, `RuntimeSession::step` copies those poses into `PoseTarget` values. For every active ragdoll,
`World::drive_ragdolls_to_pose` enables the swing and twist position motors and passes each local
bone rotation to `SetTargetOrientationBS`.

Only a `SwingTwist` parent constraint has these motors. Root parts and bones authored as `Fixed`,
`Hinge`, or `Free` do not receive a motor target. A motor changes a joint's relative orientation; it
does not move an unconstrained root body back to its animated world position.

## Physics-to-animation blend

After the physics step, `World::write_ragdoll_poses` converts each part's world transform into a
bone-local transform. At weights below `0.999`, it interpolates translation and scale and uses
quaternion spherical interpolation for rotation. At or above that threshold, the physics transform
overwrites the bone's `PoseOverride`.

The animation evaluator writes a fresh override before physics runs, so a partial blend always
starts from the animated pose for that tick. `RuntimeSession::step` performs the handoff in this
order:

```text
AnimationRuntime::last_poses
  -> World::drive_ragdolls_to_pose
  -> World::advance_ragdoll_blend
  -> World::step
  -> World::write_ragdoll_poses
```

## Authoring and control

Model import creates one `BonePhysics` entry per bone. Its capsule length follows the greatest
rest-pose distance to a direct child, the radius is 30% of the half-height with a `0.03` minimum,
and every joint starts as `SwingTwist`. The generic `set-component-field` command edits an indexed
entry when a rig needs different shapes, limits, masses, or drive values.

`set-ragdoll` creates the live ragdoll on its first call. It can change motor state, the uniform
target weight, or one bone's target. `get-ragdoll` reports whether a ragdoll exists, whether its
motors are active, its mean target weight, and its authored bone count.

For example, this command activates the motors and gives physics 35% of the rendered pose:

```console
$ sa set-ragdoll --entity 42 --active true --bodyWeight 0.35
ragdoll=present  active=yes  bodyWeight=0.35  bones=64
```

The ragdoll commands require a live physics world, so they run while the scene is Playing or Paused.

## In the code

| What | File | Symbols |
|---|---|---|
| Motor drive and blend state | `engine/crates/physics/src/world.rs` | `World::drive_ragdolls_to_pose`, `World::advance_ragdoll_blend`, `World::set_ragdoll_blend`, `World::ragdoll_state` |
| Pose write-back | `engine/crates/physics/src/world.rs` | `World::write_ragdoll_poses`, `PURE_PHYSICS_WEIGHT`, `RAGDOLL_WEIGHT_RATE` |
| Jolt motor bridge | `engine/crates/physics-sys/src/lib.rs`, `shim/jolt_bridge.cpp` | `ragdoll_set_swing_twist_motor`, `bone_motor_settings`, `BonePart` |
| Animation target and tick order | `engine/crates/animation/src/runtime.rs`, `engine/crates/runtime/src/session.rs` | `AnimationRuntime::last_poses`, `RuntimeSession::step`, `PoseTarget` |
| Import defaults | `engine/crates/assets/src/spawn.rs` | `autofit_bone_physics` |
| Control protocol | `engine/crates/control/src/commands_physics.rs`, `engine/crates/protocol/src/dto.rs` | `register_physics_commands`, `SetRagdollParams`, `GetRagdollParams`, `RagdollResult` |

## Related

- [Ragdoll](../ragdoll/) explains ragdoll construction and full-weight pose write-back.
- [Animation data model](../../animation/animation-data-model/) explains local poses and `PoseOverride`.
- [Kinematic bones](../kinematic-bones/) covers the animation-to-physics binding without pose write-back.
