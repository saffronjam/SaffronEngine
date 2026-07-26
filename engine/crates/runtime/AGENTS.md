# saffron-runtime — the shared play-mode simulation spine

`RuntimeSession` bundles the per-frame "advance the world" work as one code path: build a Jolt world
from a scene, tick animation, step physics, dispatch contacts, tick scripts. Both the editor host's
play mode and the standalone `saffron-player` consume it, so a behaviour that exists in only one of
them is a bug in this crate's shape rather than a feature of the caller.

Two thirds of the crate is vegetation: promotion, collision residency, the navigation contribution
seam, telemetry, the family cache, and cell-load scheduling. The value contracts those work on
belong to `saffron-vegetation` (`engine/crates/vegetation/AGENTS.md`).

## Layout

| File | Owns |
|---|---|
| `session.rs` | `RuntimeSession` — world build plus the per-frame advance |
| `bridge.rs` | `ScriptHostBridge`: the concrete end of the POD seam `saffron-script` declares, so `sa.*` bindings reach the live world without `saffron-script` importing `saffron-physics` |
| `vegetation.rs` | Runtime vegetation binding and bounded cell-load scheduling |
| `vegetation_promotion.rs` | The transient entity view a sparse subset of macro plants gains, and state write-back |
| `vegetation_collision.rs` | Batched Jolt proxies per physics-facet-resident cell generation |
| `vegetation_navigation.rs` | What vegetation *contributes* to a navigation system, and dirty-region delivery |
| `vegetation_family.rs` | Per-session cache of resolved `.splant` family declarations |
| `vegetation_telemetry.rs` | Counters and durations at the fixed synchronization point |

## Rules that are easy to break

- **Exactly one visible representation, one collision owner, one simulation owner.** A promoted
  plant keeps its authoritative row in the macro SoA and additionally gains a scene entity; the bulk
  representation must be suppressed for exactly that plant, and nothing else may draw or collide for
  it.
- **Bulk suppression lives on `VegetationWorld`, keyed by `PlantId`** (`promote_plant`,
  `demote_plant`, `is_bulk_suppressed`, `cell_bulk_revision` in `saffron-vegetation`'s
  `runtime_world.rs`) — deliberately *not* in the immutable published generation, so it survives a
  cell unload, republish, and reload. Every consumer keys its per-cell cache on
  `(generation, cell_bulk_revision)`; caching on generation alone makes a promotion invisible until
  the next recook. `promote_plant` rejects a plant that is already suppressed.
- **`PlantOrigin` is runtime-only and immutable; `PlantVitals` is runtime-only and mutable.** Both
  live in `saffron-scene`'s `component.rs` and neither is registered for serialization. `PlantOrigin`
  behaves like `IdComponent` — the scene rejects a replacement and rejects a mutable borrow, because
  an entity that changes which plant it views is a different entity. `PlantVitals` is mutable on
  purpose: damage and growth happen to the view.
- **A product is not the plant.** `spawn_product` creates a plain dynamic entity with the same mesh,
  variant, materials, and collider and deliberately no `PlantOrigin` and no `PlantVitals`. Felling
  is an operation, not a state, so its queue stays separate from the promotion state machine.
- **A rebind whose manifest identity changed calls `abandon()`.** Writing back into a different
  world records state against plants that are not the ones observed. Conversely, clearing vegetation
  must demote *with* write-back first, while the world is still live.
- **Transaction ids are content-derived, never a session counter.** The reducer ignores a replay
  whose canonical content is identical and errors on the same id carrying different content — so a
  session-local counter collides across reloads and its writes are then **silently dropped**.
  `flush_state` derives transaction and operation keys from a content hash over the record bytes.
- **State flush is a save barrier, not per-frame work.** The applied-transaction ledger is
  unbounded; flushing promoted state every frame was never viable. Flush at explicit barriers only.
- **Grass never gets a body.** Micro fields never reach collision inputs, and the policy map is
  fixed: `Decorative` gets nothing, `Interactive` a sensor, `Structural` and `Harvestable` a solid
  static body. A scattered plant inherits its family's declared interaction policy — hardcoding
  `Decorative` at the scatter site makes every procedurally placed tree non-collidable, which reads
  as a physics bug and is not one.
- **Physics casts report only collision-resident targets.** `sa.raycast` must not start hitting
  non-collidable grass. Vegetation's own spatial queries may report any CPU-resident macro plant;
  editor picking merges the two explicitly, and that merge must not change physics semantics.
- **Navigation is not implemented here and will not be.** This module publishes contributions —
  cell-addressed obstacle and cost declarations — for whatever consumes them. There is no tile
  builder, no graph, no queries, and no foliage-private navmesh. `take_dirty_regions()` transfers
  ownership and is drained exactly once; `dirty_regions()` peeks.
- **Telemetry never reads back per-instance data** and never walks a resident cell to answer a
  query. Everything is a counter or a duration accumulated at the fixed synchronization point. A
  diagnostic that costs a stall is not a diagnostic.
- **Promotion runs only while play is active.** The host gates it; a promotion that survives into
  edit mode leaves entity views for plants the editor is also drawing in bulk.
