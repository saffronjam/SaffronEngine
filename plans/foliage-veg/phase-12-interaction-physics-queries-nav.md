# Phase 12 — Interaction, physics, queries, navigation, and promotion

**Status:** COMPLETED

**Depends on:** Phases 5, 8, and 10

This phase connects authoritative macro plants to collision/gameplay without turning them all into
entities. Collision-relevant cells receive batched simplified Jolt proxies, queries return a tagged
world target, and a sparse subset atomically promotes to hecs/Jolt entities for falling, harvesting,
scripting, or other full behavior. State always returns through the vegetation reducer.

## Collision facet residency

- [x] Derive broadphase/index and simplified collision shapes from `.splant` proxies plus immutable
  `.svegcell` base contributions and live delta overlays. (Cooked `CollisionInputs` rows — the
  published generation already composes base + reducer deltas — join the family's
  `PlantCollisionProxy` set in `runtime/src/vegetation_collision.rs::derive_proxy_bodies`;
  convex-hull proxies are counted and skipped, they have no cooked hull geometry.)
- [x] Batch Jolt body create/add/remove around physics `SpatialSource`s; separate query-only,
  near-field collision, full breakable/harvestable, and never-collide policies. (One batched
  `AddBodiesPrepare`/`AddBodiesFinalize` create + one batched remove per cell generation, driven
  by source-claimed physics-facet residency; `body_class` maps Decorative→none,
  Interactive→sensor, Structural/Harvestable→solid — the breakable *behavior* on Harvestable
  arrives with promotion.)
- [x] Maintain a Rust-side `BodyID → WorldHitTarget` registry carrying full `PlantId`; never truncate
  into Jolt user data or forge scene UUIDs. (`World::add_static_target_bodies` registers each body
  as `BodyEntry { target: WorldHitTarget::Vegetation(PlantId), .. }`; casts/contacts/body list all
  resolve through it — proven by `static_target_batch_round_trips_vegetation_hits`.)
- [x] Generation-tag bodies and remove old generations at a fixed synchronization point.
  (`VegetationCollisionResidency::advance` inside `RuntimeSession::synchronize_vegetation`:
  superseded generations remove their batch before new generations create theirs.)
- [x] Grass/tiny cosmetic micro vegetation never receives individual bodies. (Micro fields never
  reach `CollisionInputs`; Decorative macro rows return no body class.)

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

- [x] Commit transitions at a fixed runtime synchronization point with exactly one visible
  representation, one collision owner, and one simulation owner throughout.
  (`VegetationPromotion::advance` runs inside `RuntimeSession::synchronize_vegetation` *ahead of*
  collision residency; `VegetationWorld::promote_plant/demote_plant` suppress the bulk render
  instance and the batched collision proxy, and both adapters key their per-cell cache on
  `(generation, cell_bulk_revision)` so the suppression takes effect in the same pass. The entity
  view carries the only dynamic body, created through `World::add_entity_body`.)
- [x] Add non-removable runtime `PlantOrigin(PlantId)` to promoted entities and keep the macro SoA as
  authority. (`saffron_scene::PlantOrigin`, unregistered so it is never serialized, and immutable
  like `IdComponent`: `add_component` rejects a replacement, `with_component_mut` rejects a mutable
  borrow, `remove_component` asserts. Tests: `plant_origin_is_immutable_once_set`,
  `plant_origin_cannot_be_removed`.)
- [x] Copy transform, velocity/impulses, lifecycle/health, material/phenotype, collision/breakage,
  script fields, and source generation into promotion; write changed state through the reducer before
  demotion. (Promotion copies: transform (render-relative position + ZYX Euler + per-axis scale),
  phenotype via `PlantVariant`, materials via a `MaterialSet` from the family's `material_slots`,
  collision/breakage via the largest analytic proxy as one dynamic body, lifecycle/health/moisture/
  fuel/ecology-tick via the new runtime-only `PlantVitals`, and the source generation on
  `PlantOrigin`. Demotion (and the save barrier) write back `PromotionOriginState` (transform +
  linear velocity in m/tick + angular in turns/tick) AND a `StateOverride` from the settled vitals,
  in ONE transaction. SCRIPT FIELDS: `PlantFamilyAsset` declares no script, so there are no script
  fields to copy — when plant families gain a script declaration, the copy belongs beside the
  vitals copy in `spawn_view`.)
- [x] Define save-during-promotion: snapshot promoted state with `PlantId` at a save barrier or reduce
  it deterministically first. Saving never waits for a fallen object to settle.
  (`VegetationPromotion::flush_state` reduces every promoted plant's live transform + velocity as ONE
  transaction whose id and operation keys are `ContentHash`-derived from the records — an unchanged
  flush is an exact idempotent replay, a changed one a fresh transaction. `save-project` and
  `vegetation-state-export` call it before reading the snapshot; the entity keeps living, so nothing
  waits for a settle.)
- [x] Distinguish the rooted plant from products. Felling can mutate the plant to stump/removed and
  spawn a separate log/loot entity; the product does not inherit rooted-plant identity by accident.
  (`VegetationPromotion::request_felling` → `commit_felling` at the sync point: a promoted view
  demotes WITH write-back first, the rooted plant takes a `LifecycleTransition` to `Stump` under its
  own identity, and `spawn_product` creates a plain dynamic entity with the same mesh/variant/
  materials/collider but deliberately NO `PlantOrigin` and NO `PlantVitals` — proven by
  `felling_requests_queue_once_and_products_carry_no_plant_identity`. Driven by
  `sa vegetation-fell`.)
- [x] Handle source-cell unload, recook, undo, and network-authority changes without duplicate ownership.
  (Suppression is keyed by `PlantId` on the world rather than held in an immutable published
  generation, so a cell unload/republication can neither lose nor duplicate it; a rebind whose
  manifest identity changed calls `VegetationPromotion::abandon`, which drops the views instead of
  writing their state into a different world; `clear_vegetation` demotes WITH write-back while the
  authority is still live. Editor undo is a control-call inverse in Edit mode and promotion is
  play-only, so no undo path can observe a view; network authority has no implementation to
  reconcile with yet — the write-back already carries the authority id every record is stamped with.)

## Damage, harvest, disturbance, and events

- [x] Route apply-damage, harvest, uproot/fell, plant/remove, wet/dry, ignite/extinguish hook, trample,
  and regrow through typed mutations with idempotency/preconditions. (Every operation is a typed
  `VegetationMutation` reduced through the one reducer with a non-zero transaction/authority/
  idempotency triple and per-cell `base_revision` preconditions: `Damage`, `Harvest`,
  `Tombstone` (uproot/remove), `Planting`/`AnchorAddition`, `MoistureFuel` (wet/dry), `Burn`
  (the ignite/extinguish hook), `DisturbanceMask` (trample), `Regrow`, plus `LifecycleTransition`
  and `StateOverride`; all of them are routed over the wire by `vegetation-mutate`'s DTO. A
  transaction id replayed with identical contents is an ignored replay and with different contents
  a hard error.)
- [x] Emit typed lifecycle/interaction events once per reducer transition for scripts, VFX, audio,
  quests, future fire, and nav dirtying. (`VegetationTransitionKind` {Damaged, Harvested, Burned,
  Removed, Planted, Regrew, LifecycleChanged, Wetted, StateReplaced, Moved, Disturbed} +
  `VegetationTransition` {transaction, cell, plant}; `reduce_mutations` emits ONE per committed
  record into `MutationReduction::transitions`, and `apply_confirmed_mutations` stamps them into a
  `VEGETATION_EVENT_RING_CAP` ring with per-consumer cursor delivery (`drain_events(since)` →
  events + high_water/oldest/overflowed, the same contract as physics contacts). A prediction and
  an idempotent replay both emit nothing — proven by
  `committed_records_emit_one_typed_transition_and_replays_emit_none`. Exposed as
  `vegetation-drain-events` + an `sa` formatter.)
- [x] Keep cosmetic bend prediction in the Phase-10 interaction field; persist only confirmed crushed/
  cleared/damaged masks/state. (The interaction field is a per-wind-world GPU buffer
  (`Renderer::interaction_fields`) with no serialization path; the reducer's only disturbance input
  is the confirmed `DisturbanceMask` tile, and only that emits a `Disturbed` transition — cosmetic
  bend produces neither state nor an event. Micro blades have no identity, so they cannot become
  runtime records.)
- [x] Add native vegetation AABB/radius/ray/nearest APIs to Luau/control with family/tag/state filters.
  (CONTROL: `vegetation-runtime-query` serves bounds/radius/ray/nearest with the closed
  `VegetationQueryFilter` (families, required tags, lifecycles, interaction policies). LUAU: the
  vegetation world moved behind `SharedVegetation = Rc<RefCell<Option<VegetationWorld>>>` — the
  same shared-cell seam physics already uses — so `RuntimeScriptBridge` answers
  `sa.vegetation_raycast` / `sa.vegetation_nearest` / `sa.vegetation_in_radius` synchronously from
  the one authority, and `sa.vegetation_damage` / `sa.vegetation_harvest` reduce a typed mutation
  through it and report whether it committed. Hit tables carry plant/position/distance/lifecycle/
  health/interaction_policy, with a `{hit=false}` miss shaped like the physics casts. The bindings
  are registered in the one `BINDINGS` table, so `schemas/control/sa.generated.luau` gained the
  five functions and the synthetic `sa.PlantHit` class from the same emit.
  NOT script-side yet, deliberately: per-query family/tag filters (the Luau calls use the default
  filter) and an event callback — both want a Luau table→filter marshaller, which belongs with the
  script-facing event delivery slice.)

## Navigation contribution seam

Each plant declares one of: no nav effect, traversal-cost field, static simplified obstacle, or dynamic
obstacle while promoted.

- [x] Emit cell-addressed obstacle/cost payloads and dirty-world-bounds notifications from base+delta
  state. (`VegetationNavigationSeam::advance` publishes per resident navigation-facet cell from the
  cooked `NavigationContribution` rows — already base+delta reduced by the published generation —
  crossed with the family's `PlantNavigationProxy` footprints; every retire/publish marks the
  affected `WorldBounds`, coalescing into an overlapping region rather than growing a list.)
- [x] Remove/replace affected contributions after harvest/fall, using a dynamic obstacle while a
  future nav tile rebuild is pending. (A harvest/damage commit republishes the cell, which moves the
  generation and re-derives its contributions with the old bounds marked dirty; a felled plant is
  promoted, and the promotion suppression revision makes its contribution a
  `DynamicObstacle` for exactly as long as the entity view owns it — a static rebuild would be
  stale before it finished.)
- [x] Visualize contributions and dirty regions in editor/`sa`. (`sa`:
  `vegetation-nav-contributions` (+ `drainDirty`) with a formatter printing per-cell contribution and
  obstacle counts plus the dirty/obstacle/dynamic totals. EDITOR: a `vegetationNavigation` debug
  overlay on the existing `debug_overlays` path — `build_navigation_overlay` draws each footprint as
  a closed loop with an upright per vertex for the obstacle height (red static, amber dynamic, blue
  cost) and the dirty regions as white boxes, toggled from the Render panel and persisted in
  `project.json` like its siblings.)
- [x] Do not implement pathfinding, Recast, or a foliage-private navmesh; the pending navigation
  planset consumes this contract. (Held: the seam publishes typed contributions + dirty bounds and
  nothing else — no tile builder, no graph, no queries. The module's own docs state the boundary so a
  later change cannot drift into building one here.)

## Acceptance

- [x] Batched collision residency follows sources without one entity/body per decorative plant.
  (e2e `vegetation-interaction`: with the play viewpoint claiming the Physics facet, the cell's
  structural plants carry batched proxy bodies (`residentBodies > 0`, `residentCells > 0`,
  `failedFamilies == 0`); decorative rows return no body class at all and micro/grass never reaches
  the collision rows. Two bugs this box found are recorded in READMEFABLE: nothing claimed the
  Physics/Navigation facets, and scattered points were hardcoded `Decorative`.)
- [x] Raycasts, shapecasts, contacts, scripts, protocol, `sa`, and editor all round-trip the same tagged
  target; no UUID sentinel/truncation remains. (The `WorldHitTarget` migration slice: physics
  `RayHit`/`ContactEvent`/`BodyInfo`, `ScriptHitTarget` + the Luau `entity`/`plant` marshalling,
  `WorldHitTargetDto`, the `sa` `entity=`/`plant=`/`unowned` formatter, and the editor PhysicsPanel
  labels. `tests/e2e/physics-query.test.ts`, `physics-triggers.test.ts` and
  `physics-falling-box.test.ts` cover the tagged shapes end to end.)
- [x] Promotion stress proves exactly one render/collision/simulation owner at every synchronization
  point, including save/unload/recook during transition. (e2e: promoting a plant DROPS the bulk
  `residentBodies` and flips its nav contribution to `DynamicObstacle` in the same pass the entity
  view appears; demoting restores the exact prior body count. Unload/republication cannot lose or
  duplicate the suppression because it is keyed by `PlantId` on the world rather than held in an
  immutable generation; a recook (manifest identity change) calls `abandon`; a save flushes through
  the reducer without demoting.)
- [x] Demotion writes state back and reload preserves it; cache recook cannot resurrect removed plants.
  (e2e asserts a persistent promotion-origin entry after demotion. The write-back is a reducer
  transaction, so it lives in the persistent state a reload restores, and a tombstone/stump delta
  outlives any recook of the disposable cell artifact — the base is immutable and deltas layer over
  it, so a recook cannot resurrect a removed plant.)
- [x] Grass has no body/nav-object explosion; macro nav contributions update only affected regions.
  (Micro fields carry no identity and never reach the collision or navigation rows; `Decorative`
  macro rows produce neither a body nor a contribution. The nav seam marks only the bounds of the
  contributions it retired or published, coalescing overlaps — e2e sees dirty regions appear on the
  promotion of ONE plant, not a whole-cell invalidation.)
- [x] Duplicate damage/harvest mutations and replayed events are idempotent. (e2e applies the same
  damage record twice: the first commits and emits exactly one `damaged` transition, the replay
  emits none. The reducer errors on a transaction id reused with DIFFERENT contents, and
  `committed_records_emit_one_typed_transition_and_replays_emit_none` pins the emission contract.)
- [x] Standard gate, Jolt determinism tests, Luau/control E2E, and interaction/query docs are green.
  (`just engine`, `just prepare-for-commit`, `just schema`, `cargo test --workspace` and `just e2e`
  are the gate; the docs pages `vegetation-collision.md`, `vegetation-navigation.md` and
  `plant-promotion.md` are checked by the docs-page skill's `hugo --gc` + `check_links.py` +
  `check_style.py`. The Jolt `determinism_gate` holds across repeated fresh processes after the
  scene-ordering fix, and the e2e legs for this phase are `tests/e2e/vegetation-interaction.test.ts`
  as the acceptance driver plus the vegetation graph and stress suites.)

## NO-LEGACY gate

The old entity-only hit/contact type is deleted in this phase. Promoted entities are transient views,
not a second plant database, and navigation receives contributions rather than a private system.

