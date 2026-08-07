+++
title = 'Vegetation collision residency'
weight = 10
+++

# Vegetation collision residency

Macro plants collide without becoming entities. When a vegetation cell's physics facet is
resident during play, the runtime materializes one batch of simplified static Jolt bodies for the
cell's plants; when the cell's generation is superseded or leaves residency, that batch is removed
in the same pass. Bodies never trickle in per frame and never outlive their generation.

## Derivation inputs

Each cooked cell carries per-plant collision rows (`VegetationCollisionInput`: identity, world
pose, scale, interaction policy). The plant family's `.splant` asset declares the local proxy
shapes (`PlantCollisionProxy`: box, sphere, or capsule with a center and dimensions). The runtime
composes each proxy into world space through the plant's quantized orientation and per-axis scale
and creates one static body per proxy. A convex-hull proxy has no cooked hull geometry and is
skipped, counted in the status report.

The interaction policy selects the body class:

| Policy | Body |
|---|---|
| `Decorative` | none — grass and cosmetic micro vegetation never receive bodies |
| `Interactive` | query-only sensor: casts and overlaps report it, the solver never pushes against it |
| `Structural` | solid static |
| `Harvestable` | solid static |

## Generation tagging and the synchronization point

`RuntimeSession::synchronize_vegetation` is the one point bodies change. After the cell scheduler
publishes generations, the collision residency diffs every physics-resident cell generation
against its tracked bodies: superseded generations remove their batch first, then newly resident
generations create theirs through the batched broadphase path
(`BodyInterface::AddBodiesPrepare`/`AddBodiesFinalize` — the
[Jolt bulk-insertion API](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/class_body_interface.html)).
A republished cell therefore has exactly one collision owner per plant at every point the
simulation can observe.

Every body registers under `WorldHitTarget::Vegetation(PlantId)` — the full 128-bit identity,
never truncated into Jolt user data and never a forged scene uuid — so ray casts, sphere casts,
and contact events report the plant directly (see [Scene queries](../scene-queries/)).

## Inspection

`vegetation-runtime-status` reports the collision block while a play world is live:

```json
"collision": {
  "residentCells": "3",
  "residentBodies": "412",
  "createdTotal": "540",
  "removedTotal": "128",
  "hullSkippedTotal": "0",
  "failedFamilies": "0"
}
```

## In the code

| What | File | Symbols |
|---|---|---|
| Residency diff and body derivation | `runtime/src/vegetation_collision.rs` | `VegetationCollisionResidency`, `derive_proxy_bodies`, `body_class` |
| Batched tagged-body world API | `physics/src/world.rs` | `World::add_static_target_bodies`, `World::remove_bodies` |
| Batched bridge calls | `physics-sys/shim/jolt_bridge.cpp` | `jolt_create_static_batch`, `jolt_remove_bodies` |
| Cooked per-plant rows | `vegetation/src/cell_facet.rs` | `VegetationCollisionInput` |
| Family proxy declarations | `vegetation/src/asset.rs` | `PlantCollisionProxy`, `PlantCollisionShape` |
| Synchronization point | `runtime/src/session.rs` | `RuntimeSession::synchronize_vegetation` |

## Related

- [Scene queries](../scene-queries/) — tagged targets on ray and sphere casts
- [Collision layers, sensors, and contact events](../collision-layers-and-triggers/) — the sensor and static layers the bodies use
- [Vegetation state](../../scene-and-ecs/vegetation-state/) — cell generations and facet residency
