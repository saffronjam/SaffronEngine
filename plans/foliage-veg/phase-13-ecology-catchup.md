# Phase 13 — Lifecycle ecology and deterministic catch-up

**Status:** NOT STARTED

**Depends on:** Phases 3, 5, and 12

This phase adds scalable ecological rule simulation over macro SoA cells. It models deterministic
growth, competition, mortality, succession, propagation, moisture/fuel, and regrowth as game-world
rules. It does not claim biological accuracy without separate validation, and it does not make visual
phenotype the source of lifecycle truth.

## Clock and state ownership

- [ ] Consume a monotonic world simulation clock and keep `ecology_tick` independent of the existing
  calendar/phenology time.
- [ ] Persist last completed tick, simulation version, checkpoint identity, boundary summaries, and
  enough state to prove unload/catch-up equivalence.
- [ ] Calendar/time-of-day can drive appearance and seasonal rates, but moving it backwards cannot
  reverse biological age, undo death, or replay emitted events.
- [ ] Dynamic weather signals influence current moisture/growth/burn state through declared simulation
  inputs/mutations; they do not continuously recook authored placement.

## Fixed-tick data-oriented simulation

- [ ] Use compact SoA state and spatial hashing over active simulation facets; never promote plants
  merely to simulate ecology.
- [ ] Double-buffer ticks: tick `N+1` reads only immutable tick `N` local/halo state and emits owned
  changes for `N+1`.
- [ ] Implement species/biome rule evaluation for age/growth stage, health, shade/light, crown/root
  competition, moisture/temperature suitability, mortality, dormancy, propagation/seed spread,
  companion/child relations, succession, deadfall, and regrowth.
- [ ] Use stable IDs/counter RNG/canonical numeric/tie rules. Simulation worker order and loaded-cell
  order cannot change results.
- [ ] Emit typed lifecycle transitions and gameplay events exactly once; Phase-10 rendering derives
  phenotype from reduced lifecycle state.

## Cross-cell dependency regions

- [ ] Declare influence radii and boundary summaries for shade, competition, propagation, moisture,
  disturbance, and future fire inputs.
- [ ] Advance connected dependency regions from immutable checkpoints/boundaries. Do not catch up one
  cell in isolation when neighbours can affect it.
- [ ] Publish a completed tick atomically across its affected cells in canonical key order.
- [ ] A catch-up budget may delay simulation-facet readiness, but cannot drop ticks or silently alter
  results.
- [ ] Any analytical fast-forward operator needs a formal equivalence contract and property tests;
  otherwise execute fixed ticks in bounded background jobs.

## Disturbance and external hooks

- [ ] Fold persistent harvest, damage, clearing, planting, moisture/fuel, burn phenotype, and products
  into the same reducer/state machine.
- [ ] Vegetation exposes fuel/moisture/health/occupancy queries and typed ignite/burn/extinguish/wet
  mutations. The future fire system owns heat propagation and smoke.
- [ ] Future weather owns precipitation/weather dynamics; vegetation consumes sampled inputs and owns
  persistent biological response.
- [ ] Nav/physics/render facets observe committed tick generations only.

## Debug and control

Add timeline/tick pause-step-run controls and views for age, life stage, health, moisture, fuel,
competition/shade, suitability, seed/propagation candidates, succession, dependency region,
checkpoint/boundary generation, pending catch-up, and emitted transitions. `sa` can deterministically
advance a bounded fixture and dump canonical cell/checkpoint hashes.

## Acceptance

- [ ] Continuous simulation equals unload→time advance→dependency-region catch-up byte-for-byte.
- [ ] Results are identical across worker counts, shuffled plant/cell order, different residency paths,
  and origin rebasing.
- [ ] Cross-cell shade/competition/propagation seam fixtures have no double update or border artifact.
- [ ] Calendar rewind affects phenology preview only; biological state remains monotonic.
- [ ] Lifecycle events are emitted exactly once across snapshot, compaction, unload, and replay.
- [ ] Catch-up cancellation/supersession cannot publish a partial tick generation.
- [ ] CPU cost/memory scales with active simulation macro cells, not micro vegetation or render detail.
- [ ] Standard gate, determinism/property tests, and lifecycle/ecology docs are green.

## NO-LEGACY gate

There is one macro lifecycle state and one fixed-tick reducer path. Visual season tint, mesh choice,
promoted entity fields, or fire/nav adapters never become alternate lifecycle authorities.

