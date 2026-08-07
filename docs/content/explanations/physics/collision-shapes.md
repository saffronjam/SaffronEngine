+++
title = 'Collision shapes and materials'
weight = 3
math = true
+++

# Collision shapes and materials

A collider combines geometry with friction and restitution. Anima provides three shapes defined
by dimensions and two shapes built from mesh data, with different motion constraints and fitting
rules for each.

## Shape selection

[Jolt's shape system](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/md__docs__architecture.html#autotoc_md12)
distinguishes convex shapes from triangle geometry. Anima maps `Collider.shape` to five Jolt shape
types:

| Shape | Collider fields | Suitable motion |
|---|---|---|
| `Box` | Local half-size in `halfExtents.xyz` | Static, kinematic, or dynamic |
| `Sphere` | Radius in `halfExtents.x` | Static, kinematic, or dynamic |
| `Capsule` | Radius in `.x`, cylinder half-height in `.y`, Y-up | Static, kinematic, or dynamic |
| `ConvexHull` | Vertices from `sourceMesh` | Static, kinematic, or dynamic |
| `Mesh` | Vertices and triangle indices from `sourceMesh` | Static or kinematic |

The bridge clamps box, sphere, and capsule dimensions to at least `0.01` before constructing a Jolt
shape. Boxes also receive a convex radius equal to half their smallest half-extent, capped at
`0.05`.

A triangle-mesh collider on a dynamic body is invalid. `cook_shape_geometry` returns
`Error::MeshShapeOnDynamic`, and `World::populate` logs the error and skips that body while it
continues building the rest of the world. A moving object that needs mesh-derived geometry uses a
`ConvexHull`.

## Mesh cooking

`ConvexHull` and `Mesh` require a nonzero `sourceMesh`. `AssetServer::load_mesh_cpu_asset` resolves
that asset through the catalog and decodes its `.smesh` bytes into a CPU `Mesh` without uploading it
to the GPU or entering the render-resource cache.

Convex-hull cooking sends every source vertex position to `ConvexHullShapeSettings` in stored vertex
order. Triangle-mesh cooking sends the stored vertex array and flat index list to
`MeshShapeSettings`. Stable ordering gives Jolt the same cooking input on each run.

The safe physics crate obtains mesh data through `MeshCook`, a callback with this boundary:

```rust
pub type MeshCook<'a> =
    dyn FnMut(Uuid) -> Result<saffron_geometry::Mesh, String> + 'a;
```

An absent source produces `Error::NoCookSource`. Asset-read failures and empty source geometry
produce `Error::CookFailed`. A Jolt shape-construction failure returns an invalid body ID and omits
that body from the live world.

## Shape-aware fitting

Adding a `Collider` through the control plane calls `fit_collider_to_mesh`. The same helper backs
`fit-collider`, which recomputes dimensions after a shape, transform, or model change.

The fitter examines the entity's mesh, or every mesh entity beneath a model root. It transforms the
eight corners of each mesh AABB into the collider body's local frame and unions them. This includes
hierarchy transforms and world scale while leaving the Jolt body itself scale-free. The first
resolvable mesh becomes `sourceMesh` for cooked shapes.

For local bounds with half-size $h$ and center $c$, the selected shape receives:

| Shape | Fitted values |
|---|---|
| `Box` | `halfExtents = h`, `offset = c` |
| `Sphere` | Radius $= \max(h_x, h_y, h_z)$ in all three extent fields |
| `Capsule` | Radius $= \max(h_x, h_z)$; half-height $= \max(0, h_y - radius)$ |
| `ConvexHull`, `Mesh` | `halfExtents = h`; cooking uses `sourceMesh` |

The capsule axis remains Y-up regardless of which AABB axis is longest. Fitting returns without
changing the collider when it finds no collider, no resolvable mesh, or only a single-point bound.
A planar mesh remains valid because at least one extent has positive length.

For example, a fitted collider reports the values written back to the component:

```console
$ sa fit-collider --entity 42
fitted capsule  entity=42  halfExtents=(0.420, 0.780, 0.420)  offset=(0.000, 1.200, 0.000)
```

## Surface material

`PhysicsMaterial` contributes `friction` and `restitution` to Jolt's `BodyCreationSettings`.
Friction defaults to `0.5`; restitution defaults to `0.0`. Lower friction permits more sliding,
while higher restitution preserves more separating speed after impact. Authored restitution uses
the `0` to `1` range described by the component model.

Material values belong to the collider, so a collider-only static surface and a collider paired
with a rigidbody use the same material path. Sensor colliders also carry these fields, although
their contacts do not apply impulses.

## In the code

| What | File | Symbols |
|---|---|---|
| Shape and material components | `engine/crates/scene/src/component.rs`, `serde.rs` | `Shape`, `Collider`, `PhysicsMaterial`, `SceneSerialize for Collider` |
| Shape cooking and body population | `engine/crates/physics/src/world/` | `cook_shape_geometry`, `CookedGeometry`, `World::populate`, `MeshCook` |
| Typed cooking errors | `engine/crates/physics/src/error.rs` | `Error::MeshShapeOnDynamic`, `Error::NoCookSource`, `Error::CookFailed` |
| Jolt shape construction | `engine/crates/physics-sys/shim/jolt_bridge.cpp` | `build_collider_shape`, `jolt_create_body` |
| CPU mesh loading | `engine/crates/assets/src/load/` | `AssetServer::load_mesh_cpu_asset` |
| Auto-fit and control command | `engine/crates/physics/src/world/`, `engine/crates/control/src/commands_physics.rs` | `fit_collider_to_mesh`, `register_physics_commands`, `FitColliderResult` |

## Related

- [Rigidbody and collider](../rigidbody-and-collider/) explains how motion settings combine with a shape.
- [Collision layers, sensors, and contact events](../collision-layers-and-triggers/) covers contact filtering and sensors.
- [Asset server and catalog](../../geometry-and-assets/asset-server-and-catalog/) explains stable mesh identifiers.
