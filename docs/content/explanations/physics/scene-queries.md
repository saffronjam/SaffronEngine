+++
title = 'Scene queries'
weight = 7
+++

# Scene queries

Scene queries test the live physics world without advancing it. A ray or swept sphere reports the
closest body along a path, which supports sight tests, weapon impacts, and probes that need more
width than a line.

## Ray and sphere casts

Both operations use [Jolt's narrow-phase query](https://jrouwe.github.io/JoltPhysicsDocs/5.3.0/class_narrow_phase_query.html)
and return `RayHit`:

| Field | Meaning on a hit |
|---|---|
| `hit` | `true` when the cast found a body |
| `target` | The struck body's tagged owner: a scene entity (`kind: "scene-entity"`, `id`) or a macro plant (`kind: "vegetation"`, `plant`); absent for an unowned body |
| `point` | World-space point on the struck body |
| `normal` | World-space surface normal |
| `distance` | Hit fraction multiplied by `maxDist` |

`World::raycast` constructs the path as `origin + dir * maxDist` and calls
`NarrowPhaseQuery::CastRay`. It obtains the hit normal under a Jolt read lock. `World::sphere_cast`
uses `NarrowPhaseQuery::CastShape` with a sphere whose radius has a `0.001` minimum and a closest-hit
collector.

Direction is not normalized by the engine. With a unit direction, `distance` is the physical
distance from the origin. With a direction of length `L`, the physical displacement to the hit is
`distance * L`. Callers that need world-unit distances therefore pass a unit vector.

A miss returns `RayHit::default()`: `hit` is false and every numeric or vector field is zero. The
safe world maps body IDs through its body → target registry. An untracked Jolt body carries no
target; Lua then omits both the `entity` and `plant` fields.

Queries use Jolt's default query filters, so the API has no per-call layer mask. A
`CharacterVirtual` has no broad-phase body and does not appear in these casts. The
[character controller](../character-controller/) performs its own narrow-phase sweeps when it
updates.

## Query timing

`World::raycast` and `World::sphere_cast` take `&self`, and the FFI accepts a shared world reference.
They do not alter body state. Calls run between physics updates rather than from Jolt's worker-thread
callbacks.

Control commands execute on the main thread before the frame's runtime update. Gameplay queries run
after physics has stepped: `RuntimeSession::step` releases its mutable world borrow before it
dispatches contact handlers and script `on_update`. Both `on_contact` and `on_update` may therefore
query the settled world through the script bridge.

The editor's `pick` command has a different data source. It tests editor billboards and render mesh
AABBs at viewport coordinates, while physics casts test live collision shapes at their simulated
transforms.

## Control and script surfaces

The control plane exposes `raycast` and `shapecast`; `shapecast` is the sphere-sweep operation. Both
default `maxDist` to `1000` and require a live physics world, which exists while the scene is Playing
or Paused.

For example, this ray starts two metres above the origin and casts ten metres downward:

```console
$ sa raycast --origin '{"x":0,"y":2,"z":0}' --dir '{"x":0,"y":-1,"z":0}' --maxDist 10
hit entity=42  point=(0.000, 0.100, 0.000)  normal=(0.00, 1.00, 0.00)  dist=1.900
```

A hit on an authoritative macro plant prints its identity instead:

```console
hit plant=1f3a…  point=(4.100, 0.000, 7.250)  normal=(0.00, 1.00, 0.00)  dist=3.200
```

The Lua functions take scalar coordinates. `sa.raycast` accepts origin, direction, and maximum
distance; `sa.spherecast` inserts radius before maximum distance:

```lua
local hit = sa.raycast(0, 2, 0, 0, -1, 0, 10)
if hit.hit and hit.entity then
    hit.entity:send("ground_hit", { distance = hit.distance })
elseif hit.hit and hit.plant then
    -- A macro plant: `hit.plant` is its canonical hex identity string.
end
```

`saffron-script` declares `ScriptHostBridge` so it does not depend on the physics crate.
`RuntimeScriptBridge` implements that trait over the session's shared `Option<World>`, copies
`RayHit` into `ScriptRayHit`, and returns a miss when no world exists.

## In the code

| What | File | Symbols |
|---|---|---|
| Public query API and hit mapping | `engine/crates/physics/src/world/`, `src/types.rs` | `World::raycast`, `World::sphere_cast`, `World::map_ray_hit`, `RayHit` |
| Jolt narrow-phase bridge | `engine/crates/physics-sys/src/lib.rs`, `shim/jolt_bridge.cpp` | `raycast`, `sphere_cast`, `jolt_raycast`, `jolt_sphere_cast` |
| Control protocol | `engine/crates/control/src/commands_physics.rs`, `engine/crates/protocol/src/dto/` | `register_physics_commands`, `RaycastParams`, `ShapecastParams`, `RaycastResult` |
| Lua bindings and POD seam | `engine/crates/script/src/bindings.rs`, `bridge.rs` | `sa.raycast`, `sa.spherecast`, `ScriptHostBridge`, `ScriptRayHit` |
| Runtime bridge | `engine/crates/runtime/src/bridge.rs`, `session.rs` | `RuntimeScriptBridge`, `RuntimeSession::step` |

## Related

- [Character controller](../character-controller/) explains the controller's separate sweep path.
- [Collision layers, sensors, and contact events](../collision-layers-and-triggers/) covers body filtering and callback dispatch.
- [Tooling and control](../../tooling-and-control/) explains the JSON control plane used by `sa`.
