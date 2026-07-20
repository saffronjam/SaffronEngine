# Phase 5 — Runtime cell store, queries, and persistence baseline

**Status:** NOT STARTED

**Depends on:** Phases 1 and 4

This phase loads immutable cell bases into a compact authoritative runtime world, overlays persistent
state through the Phase-2 reducer, exposes gameplay/editor queries, and proves unload/reload and
snapshot/compaction correctness before rendering or physics can create competing state.

## `VegetationWorld`

- [ ] Add a data-oriented runtime owner in `saffron-vegetation` or `saffron-runtime` over immutable
  cell generations plus reduced deltas. Rendering and Jolt access snapshots/adapters, never its
  mutable internals.
- [ ] Store macro state as typed SoA columns keyed externally by `PlantId`. Internal `PlantSlot` is
  ephemeral and generation-tagged; compaction/reordering never escapes.
- [ ] Store micro vegetation as quantized density/attribute field tiles plus persistent disturbance
  masks, not one record per blade.
- [ ] Maintain a cell-local spatial hash/BVH for macro radius/AABB/ray/nearest queries by plant,
  family, tag, lifecycle state, or interaction policy.
- [ ] Carry source cell generation through every derived index and snapshot; stale handles fail.

## Facet residency and scheduling

- [ ] Use Phase-1 `SpatialSource` demand to load render, physics, simulation, editing, nav, and
  network-interest sections independently with separate refcounts, budgets, and hysteresis.
- [ ] Support multiple simultaneous sources and predictive prefetch from velocity/camera motion.
- [ ] Load/generate into private staging, validate manifest/hash/version, then atomically publish a
  complete cell generation.
- [ ] Discard late async work through `GenerationToken`; fence/read guards keep old generations alive
  until every reader releases them.
- [ ] Coarse/global outputs are referenced, not regenerated in each fine cell.

## State reduction and persistence

- [ ] Apply state precedence exactly: authored/cooked base, confirmed persistent runtime delta,
  transient prediction/cosmetic.
- [ ] Implement compact per-cell canonical snapshots plus ordered mutation tails using the one reducer.
  Snapshot+tail and uncompacted history must reduce identically.
- [ ] Bind state headers to exact world manifest, graph/compiler/numeric/simulation versions, and seed
  namespaces. Reject mismatch instead of trying to migrate clean-slate data.
- [ ] Keep editor-authoring journals separate from runtime state storage while sharing mutations and
  reducer semantics.
- [ ] Provide serialization codecs for future savegame/network containers without implementing a
  second game-save framework or network transport in this phase.
- [ ] Confirmed prediction enters persistent state only after authority confirmation.

## Query semantics

- [ ] Add read-only vegetation AABB/radius/ray/nearest queries over any CPU-resident macro cell,
  independent of renderer visibility or collision residency.
- [ ] Return `PlantId`, position/bounds, family/tags/state, and provenance; never `PlantSlot`.
- [ ] Keep physics raycast semantics separate: it returns only collision-resident objects after the
  Phase-12 tagged-target cutover. `sa.raycast` must not silently start hitting non-collidable grass.
- [ ] Add control/`sa` commands to query cells/plants, inspect state/provenance, export/import a
  vegetation state snapshot for tests, and report queues/residency/budgets.

## Runtime generation

Runtime generation calls the same Phase-3 evaluator and Phase-4 artifact contract. It may generate a
missing cell asynchronously under a source radius, but publishes canonical cell bytes and then loads
them through the same path as offline cook. Cosmetic micro reconstruction can happen later on the GPU;
macro generation remains authoritative.

## Acceptance

- [ ] Load/unload/reload preserves exact macro state and query results.
- [ ] Cache recook under an existing runtime delta cannot resurrect tombstoned plants or remove
  runtime additions.
- [ ] Snapshot compaction, duplicate idempotent tails, and interrupted writes preserve reducer output.
- [ ] Manifest mismatch, corrupt section, stale generation, and canceled work fail deterministically.
- [ ] Multiple moving spatial sources produce stable refcounts and no unload/load thrash beyond the
  specified hysteresis.
- [ ] CPU memory and query work scale with resident cell facets/macro plants, never micro blade count.
- [ ] `sa` query/state commands and protocol DTOs use opaque string `PlantId`s and generated TS.
- [ ] Standard gate and runtime vegetation/persistence docs are green.

## NO-LEGACY gate

The macro SoA plus reducer is the plant authority. Render instances, physics bodies, promoted entities,
editor selection, and future replication may only reference or derive from its stable IDs and
generation-tagged snapshots.

