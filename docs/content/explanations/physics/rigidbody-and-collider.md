+++
title = 'Rigidbody and collider'
weight = 2
+++

# Rigidbody and collider

`Collider` describes an entity's collision geometry and surface. `Rigidbody` describes how the
solver may move that geometry. Keeping these concerns separate lets static scene geometry use one
component while simulated objects add only the motion settings they need.

## Component roles

`Collider` selects a shape, local dimensions, a local offset, friction, restitution, and sensor
behavior. `sourceMesh` supplies geometry for convex-hull and triangle-mesh shapes. Adding the
component through the control plane also attempts to fit it to the entity's mesh bounds.

`Rigidbody` carries the solver settings:

| Field | Default | Meaning |
|---|---:|---|
| `motion` | `dynamic` | Static, kinematic, or force-driven motion |
| `mass` | `1.0` kg | Dynamic-body mass |
| `linearDamping` | `0.05` | Per-second linear velocity decay |
| `angularDamping` | `0.05` | Per-second angular velocity decay |
| `gravityFactor` | `1.0` | Scale applied to world gravity |
| `lockPosition` | all false | Frozen translation axes for a dynamic body |
| `lockRotation` | all false | Frozen rotation axes for a dynamic body |
| `collisionLayer` | `0` | Moving-layer selection for non-static bodies |

A collider without a rigidbody becomes an implicit static body. If a rigidbody exists, its
`Motion` selects the corresponding Jolt motion type. Static bodies never move; kinematic bodies
follow scene transforms; dynamic bodies respond to gravity, contacts, forces, and impulses.

The collision layer applies to non-static rigidbodies. Values `0`, `1`, and `2` select `Moving`,
`Character`, and `Debris`; other values select `Moving`. A sensor collider selects the `Sensor`
layer regardless of its rigidbody field. The [collision-layer page](../collision-layers-and-triggers/)
defines the resulting pair matrix.

## Building the live bodies

`RuntimeSession::start` creates a [Jolt Physics](https://github.com/jrouwe/JoltPhysics) world from
the duplicated play scene. `World::populate` walks its colliders, resolves each motion and object
layer, prepares any cooked geometry, and constructs one Jolt body per accepted collider.

`BodyCreate` crosses the FFI boundary with shape data, world position and rotation, material values,
motion, and layer. Dynamic bodies also use mass, damping, gravity scale, and an `EAllowedDOFs`
bitmask derived from the six axis locks. Jolt calculates inertia from the authored mass and shape.

The initial transform omits scale. Collider fitting bakes hierarchy scale into shape dimensions and
the local shape offset, while body position and rotation come from the entity's composed world
transform.

## Fixed stepping and transform flow

The play gate passes a frame delta only while Playing or for an explicit paused step. It clamps that
delta to one third of a second. `World::step` accumulates the value and advances Jolt in `1/60`
second increments, with at most eight substeps in one call.

Before each Jolt update, kinematic bodies receive their entity's fresh composed transform through
`MoveKinematic`. Jolt derives velocity over the fixed timestep, allowing the kinematic motion to
push dynamic bodies. Static bodies require no per-step transform update.

After at least one substep, every dynamic body's world position and quaternion are read from Jolt.
Anima converts the quaternion to the scene's ZYX Euler convention, then writes translation and
rotation into the entity's local `Transform`. Dynamic rigidbodies therefore use root entities, where
the local transform equals the simulated world transform.

`RuntimeSession::step` completes physics write-back before it dispatches contacts and calls script
`on_update` handlers. A script that inspects an entity transform sees the settled result from that
tick. The [physics world lifecycle](../physics-world-lifecycle/) covers the deterministic Jolt build
and play-state gate in detail.

This sequence summarizes the body path:

```text
Collider + optional Rigidbody
  -> BodyCreate
  -> fixed Jolt substeps
  -> dynamic world pose
  -> local Transform
```

## Play-scene lifetime

The physics world and its bodies exist for the play session. Pausing retains the world without
advancing it; stopping drops the world and discards the duplicated play scene. Physics never writes
the authored scene, so a subsequent play session builds new bodies from the authored component and
transform values.

`physics-state` exposes the live body totals. For a scene containing a collider-only floor and one
dynamic crate, it reports:

```console
$ sa physics-state
physics=active  bodies=2  dynamic=1
```

In Edit, the same command returns `physics=inactive  bodies=0  dynamic=0`.

## In the code

| What | File | Symbols |
|---|---|---|
| Scene components and serialization | `engine/crates/scene/src/component.rs`, `serde.rs` | `Collider`, `Rigidbody`, `Motion`, `PhysicsMaterial`, `SceneSerialize for Rigidbody` |
| Body creation and stepping | `engine/crates/physics/src/world/` | `World::populate`, `body_create`, `allowed_dofs`, `World::step`, `BodyEntry` |
| Physics vocabulary | `engine/crates/physics/src/types.rs` | `MotionType`, `MotionType::from_scene`, `ObjectLayer`, `FIXED_STEP` |
| Runtime lifecycle and tick order | `engine/crates/runtime/src/session.rs`, `engine/crates/host/src/layer/` | `RuntimeSession::start`, `RuntimeSession::step`, `HostLayer::reconcile_play_edge` |
| Shape fitting | `engine/crates/physics/src/world/` | `fit_collider_to_mesh` |
| World inspection | `engine/crates/control/src/commands_physics.rs` | `register_physics_commands`, `PhysicsStateResult`, `PhysicsBodiesResult` |

## Related

- [Collision shapes and materials](../collision-shapes/) details analytic shapes, mesh cooking, and fitting.
- [Collision layers, sensors, and contact events](../collision-layers-and-triggers/) defines filtering and contact delivery.
- [Physics world lifecycle](../physics-world-lifecycle/) explains world creation, teardown, and deterministic stepping.
