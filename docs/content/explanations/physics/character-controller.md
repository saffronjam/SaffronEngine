+++
title = 'Character controller'
weight = 6
+++

# Character controller

A character controller moves an upright capsule through collision geometry without giving its
orientation to the rigid-body solver. The engine uses Jolt's `CharacterVirtual` sweep controller
for wall sliding, walkable-slope tests, ground adhesion, and stair stepping.

## Character assembly

A controlled entity carries a `Transform`, a capsule `Collider`, and `CharacterController`. It does
not need a `Rigidbody`: `World::populate` excludes controller colliders from rigid-body creation, and
`RuntimeSession::populate_world` creates one `CharacterVirtual` for each controller instead.

[`CharacterVirtual`](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/class_character_virtual.html)
uses narrow-phase collision queries rather than joining the rigid-body simulation. Anima creates
its Jolt capsule from `Collider.half_extents.x` as radius and `.y` as cylinder half-height. Each
dimension has a `0.05` minimum; an entity without a collider uses radius `0.3` and half-height `0.6`.

`CharacterController` separates authored settings from live movement state:

| Field | Default | Role |
|---|---:|---|
| `maxSpeed` | `4.0` m/s | Caps horizontal desired velocity |
| `maxSlopeAngle` | `0.785398` rad | Marks steeper contacts as walls |
| `maxStepHeight` | `0.3` m | Sets the upward stair sweep |
| `gravityFactor` | `1.0` | Scales world gravity |
| `desiredVelocity` | zero | Runtime horizontal input |
| `verticalVelocity` | `0.0` | Runtime gravity and jump state |
| `onGround` | `false` | Runtime result from the last step |

Only the four authored settings serialize. Loading a scene resets desired velocity, vertical
velocity, and ground state.

## Fixed-step movement

`World::step` updates each `CharacterVirtual` after the rigid-body solve in every fixed substep.
`World::step_characters` first reads Jolt's ground state. A grounded character with no upward
velocity rests at zero vertical speed; otherwise world gravity accumulates into its vertical
velocity.

The controller ignores the Y component of `desiredVelocity`. It clamps the XZ vector to `maxSpeed`,
combines it with vertical velocity, and passes the result to `SetLinearVelocity`. The bridge then
calls `CharacterVirtual::ExtendedUpdate` with gravity, the `Character` object layer, and
`maxStepHeight` as the upward stair-sweep distance.

`ExtendedUpdate` handles collision response, steep slopes, its default stick-to-floor sweep, and
stair walking. After the update, Anima stores the new `onGround` state. Once all fixed substeps
finish, the resolved world position is written to the entity's local `Transform.translation`.
Controller entities therefore use a root transform, where local and world positions are equal.

The controller writes position only. Rotation and skeletal animation remain independent, so an
animation player can drive the visible pose while the controller moves the entity root.

## Movement command

`move-character` sets the XZ desired velocity for the next physics step. Passing `jump=true` assigns
a vertical velocity of `5.0` m/s. The response contains the position and ground state visible when
the command runs; subsequent fixed steps consume the new values.

For example, this starts a three-metre-per-second walk along positive X:

```console
$ sa move-character --entity 42 --velocity '{"x":3,"y":0,"z":0}'
position=(0.000, 1.200, 0.000)  onGround=yes
```

Gameplay scripts reach the same operation through the entity movement binding. Repeated updates can
change direction or set the desired velocity to zero without rebuilding the controller.

## In the code

| What | File | Symbols |
|---|---|---|
| Component and authored defaults | `engine/crates/scene/src/component.rs`, `serde.rs` | `CharacterController`, `SceneSerialize for CharacterController` |
| Creation, stepping, and write-back | `engine/crates/physics/src/world.rs` | `World::add_character`, `World::step_characters`, `World::step` |
| Jolt bridge | `engine/crates/physics-sys/src/lib.rs`, `shim/jolt_bridge.cpp` | `add_character`, `character_set_linear_velocity`, `character_extended_update`, `character_on_ground` |
| Play-world population | `engine/crates/runtime/src/session.rs` | `RuntimeSession::populate_world` |
| Control protocol | `engine/crates/control/src/commands_physics.rs`, `engine/crates/protocol/src/dto.rs` | `register_physics_commands`, `MoveCharacterParams`, `MoveCharacterResult` |

## Related

- [Collision layers, sensors, and contact events](../collision-layers-and-triggers/) defines the `Character` query layer.
- [Rigidbody and collider](../rigidbody-and-collider/) explains the body path skipped by controller entities.
- [Script components and the play runtime](../../scripting/script-components-and-runtime/) covers per-tick gameplay scripts.
