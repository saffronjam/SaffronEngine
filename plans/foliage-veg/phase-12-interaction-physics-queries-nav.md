# Phase 12 — Interaction, physics, queries, navigation, and promotion

**Status:** NOT STARTED

**Depends on:** Phases 5, 8, and 10

This phase connects authoritative macro plants to collision/gameplay without turning them all into
entities. Collision-relevant cells receive batched simplified Jolt proxies, queries return a tagged
world target, and a sparse subset atomically promotes to hecs/Jolt entities for falling, harvesting,
scripting, or other full behavior. State always returns through the vegetation reducer.

## Collision facet residency

- [ ] Derive broadphase/index and simplified collision shapes from `.splant` proxies plus immutable
  `.svegcell` base contributions and live delta overlays.
- [ ] Batch Jolt body create/add/remove around physics `SpatialSource`s; separate query-only,
  near-field collision, full breakable/harvestable, and never-collide policies.
- [ ] Maintain a Rust-side `BodyID → WorldHitTarget` registry carrying full `PlantId`; never truncate
  into Jolt user data or forge scene UUIDs.
- [ ] Generation-tag bodies and remove old generations at a fixed synchronization point.
- [ ] Grass/tiny cosmetic micro vegetation never receives individual bodies.

## Atomic tagged-target cutover

Define one public target:

```text
WorldHitTarget = SceneEntity(Uuid) | Vegetation(PlantId)
```

Make one breaking migration across:

- physics `BodyEntry`, `RayHit`, shapecast, contacts/sensors, damage/contact event rings, and tests;
- protocol DTOs and command fixtures for raycast/shapecast/contact/target values;
- `sa` output/inputs and editor picking/selection/inspector routing;
- Luau `sa.raycast`, `sa.spherecast`, contact callbacks, damage/interaction APIs, and docs; and
- host/runtime bridge traits and every caller.

Physics casts still report only collision-resident targets. Vegetation spatial queries from Phase 5
can report any CPU-resident macro plant. Editor picking may merge GPU selection IDs and CPU/collision
queries explicitly; it may not silently change physics semantics.

## Promotion/demotion state machine

Use one explicit transition:

```text
Bulk → Promoting → Promoted(EntityUuid) → Demoting → Bulk/Removed
```

- [ ] Commit transitions at a fixed runtime synchronization point with exactly one visible
  representation, one collision owner, and one simulation owner throughout.
- [ ] Add non-removable runtime `PlantOrigin(PlantId)` to promoted entities and keep the macro SoA as
  authority.
- [ ] Copy transform, velocity/impulses, lifecycle/health, material/phenotype, collision/breakage,
  script fields, and source generation into promotion; write changed state through the reducer before
  demotion.
- [ ] Define save-during-promotion: snapshot promoted state with `PlantId` at a save barrier or reduce
  it deterministically first. Saving never waits for a fallen object to settle.
- [ ] Distinguish the rooted plant from products. Felling can mutate the plant to stump/removed and
  spawn a separate log/loot entity; the product does not inherit rooted-plant identity by accident.
- [ ] Handle source-cell unload, recook, undo, and network-authority changes without duplicate ownership.

## Damage, harvest, disturbance, and events

- [ ] Route apply-damage, harvest, uproot/fell, plant/remove, wet/dry, ignite/extinguish hook, trample,
  and regrow through typed mutations with idempotency/preconditions.
- [ ] Emit typed lifecycle/interaction events once per reducer transition for scripts, VFX, audio,
  quests, future fire, and nav dirtying.
- [ ] Keep cosmetic bend prediction in the Phase-10 interaction field; persist only confirmed crushed/
  cleared/damaged masks/state.
- [ ] Add native vegetation AABB/radius/ray/nearest APIs to Luau/control with family/tag/state filters.

## Navigation contribution seam

Each plant declares one of: no nav effect, traversal-cost field, static simplified obstacle, or dynamic
obstacle while promoted.

- [ ] Emit cell-addressed obstacle/cost payloads and dirty-world-bounds notifications from base+delta
  state.
- [ ] Remove/replace affected contributions after harvest/fall, using a dynamic obstacle while a
  future nav tile rebuild is pending.
- [ ] Visualize contributions and dirty regions in editor/`sa`.
- [ ] Do not implement pathfinding, Recast, or a foliage-private navmesh; the pending navigation
  planset consumes this contract.

## Acceptance

- [ ] Batched collision residency follows sources without one entity/body per decorative plant.
- [ ] Raycasts, shapecasts, contacts, scripts, protocol, `sa`, and editor all round-trip the same tagged
  target; no UUID sentinel/truncation remains.
- [ ] Promotion stress proves exactly one render/collision/simulation owner at every synchronization
  point, including save/unload/recook during transition.
- [ ] Demotion writes state back and reload preserves it; cache recook cannot resurrect removed plants.
- [ ] Grass has no body/nav-object explosion; macro nav contributions update only affected regions.
- [ ] Duplicate damage/harvest mutations and replayed events are idempotent.
- [ ] Standard gate, Jolt determinism tests, Luau/control E2E, and interaction/query docs are green.

## NO-LEGACY gate

The old entity-only hit/contact type is deleted in this phase. Promoted entities are transient views,
not a second plant database, and navigation receives contributions rather than a private system.

