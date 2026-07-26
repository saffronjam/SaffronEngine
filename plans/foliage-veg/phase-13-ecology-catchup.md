# Phase 13 — Lifecycle ecology and deterministic catch-up

**Status:** COMPLETED

**Depends on:** Phases 3, 5, and 12

This phase adds scalable ecological rule simulation over macro SoA cells. It models deterministic
growth, competition, mortality, succession, propagation, moisture/fuel, and regrowth as game-world
rules. It does not claim biological accuracy without separate validation, and it does not make visual
phenotype the source of lifecycle truth.

## Clock and state ownership

- [x] Consume a monotonic world simulation clock and keep `ecology_tick` independent of the existing
  calendar/phenology time. (`EcologyClock` in `vegetation/src/ecology.rs` counts fixed ecology ticks
  and is structurally incapable of moving backwards: `ticks_to` refuses a target behind the clock and
  `complete` accepts only the exact successor, so a skipped tick cannot drop a generation and a
  repeat cannot double-apply one. It derives nothing from the calendar.)
- [x] Persist last completed tick, simulation version, checkpoint identity, boundary summaries, and
  enough state to prove unload/catch-up equivalence. (`EcologyState` carries the rule-set version,
  the clock, and per-cell `EcologyCellSummary` rows; it is a section of `VegetationState`'s canonical
  snapshot — a declared `state_schema_identity` change, no migration. `checkpoint_identity()` hashes
  (version, completed tick, every summary in canonical cell order), which is the equality the phase's
  central claim rests on; `the_checkpoint_identity_pins_rules_tick_and_every_summary` proves it is
  publication-order independent, and
  `the_ecology_section_round_trips_and_preserves_the_checkpoint_identity` proves a reload resumes the
  same state.)
- [x] Calendar/time-of-day can drive appearance and seasonal rates, but moving it backwards cannot
  reverse biological age, undo death, or replay emitted events. (Three independent guards, none of
  them a caller convention: the clock refuses to move backwards; the reducer already rejects an
  `ecology_tick` that regresses on `LifecycleTransition`/`Regrow`; and events are emitted from
  committed reducer transitions, where a replayed transaction emits nothing. Calendar rewind reaches
  only phenotype resolution, which reads lifecycle state rather than writing it.)
- [x] Dynamic weather signals influence current moisture/growth/burn state through declared simulation
  inputs/mutations; they do not continuously recook authored placement. (Weather reaches biology only
  through the typed mutations the one reducer already owns — `MoistureFuel`, `Damage`, `Burn`,
  `StateOverride` — each of which is a persistent delta over the immutable cooked base. No weather
  path touches the cooker, so placement never recooks; the cook is driven by authored graph edits
  alone.)

## Fixed-tick data-oriented simulation

- [x] Use compact SoA state and spatial hashing over active simulation facets; never promote plants
  merely to simulate ecology. (`EcologyPlantState` is a compact row read straight from the macro SoA;
  `advance_cell` takes a slice of them and touches no entity, no scene, and no promotion machinery.
  Nothing in the tick can promote a plant — it has no access to do so.)
- [x] Double-buffer ticks: tick `N+1` reads only immutable tick `N` local/halo state and emits owned
  changes for `N+1`. (`advance_cell` is a pure function: `&[EcologyPlantState]` plus
  `&[EcologyCellSummary]` halo in, `EcologyTickOutput` out. It mutates nothing in place, and the
  ambient shade is computed once from the immutable input so no plant sees a partially updated
  world. Proven by `a_tick_is_reproducible_and_neighbour_order_independent`.)
- [x] Implement species/biome rule evaluation for age/growth stage, health, shade/light, crown/root
  competition, moisture/temperature suitability, mortality, dormancy, propagation/seed spread,
  companion/child relations, succession, deadfall, and regrowth. (All modelled in the rule set named
  by `ECOLOGY_SIMULATION_VERSION`, `ecology_tick.rs`: monotonic age/growth stages, health integrating
  suitability, shade/light from local + halo canopy, crown competition through that canopy budget,
  ROOT competition as its own below-ground budget (`root_demand` per species, summed into
  `EcologyCellSummary.roots`, taking its cut of the water before drought tolerance sees it),
  moisture/temperature suitability, mortality at zero health, dormancy below a warmth threshold,
  propagation as runtime-namespace seeds parented to their source, COMPANION/ANTAGONIST/UNDERSTORY
  relations (declared per species on `.splant`, resolved from the manifest, applied against the
  per-family presence the boundary summary carries), SUCCESSION (a successor's seedlings hold their
  stage while the canopy above them is healthy and resume once it fails), deadfall to stump, and
  regrowth. Four tests cover the new rules: `root_competition_takes_its_share_of_the_water`,
  `a_companion_lifts_where_an_antagonist_suppresses`,
  `a_successor_holds_its_stage_until_the_canopy_fails`, and
  `an_understory_species_tolerates_the_canopy_it_lives_under`.)
- [x] Use stable IDs/counter RNG/canonical numeric/tie rules. Simulation worker order and loaded-cell
  order cannot change results. (Every stochastic rule draws from a counter-based Philox stream keyed
  by (map, species, plant, rule channel) at sample index = tick, so a plant's coin flips are the same
  whichever worker asks and however many neighbours are loaded; one channel per rule means adding a
  rule cannot perturb another's stream. All arithmetic is `UnitInterval`/integer, never float.
  `advance_cell` REFUSES plants that are not in canonical identity order rather than silently
  producing an order-dependent answer (`unordered_plants_are_refused`), and neighbour order is proven
  irrelevant.)
- [x] Emit typed lifecycle transitions and gameplay events exactly once; Phase-10 rendering derives
  phenotype from reduced lifecycle state. (A tick emits only typed `VegetationMutation`s for the one
  reducer, which emits exactly one typed transition per committed record and nothing on a replay.
  Rendering already resolves phenotype from reduced lifecycle state through
  `resolve_rendered_phenotype`; the tick never writes a phenotype except through `Regrow`.)

## Cross-cell dependency regions

- [x] Declare influence radii and boundary summaries for shade, competition, propagation, moisture,
  disturbance, and future fire inputs. (`EcologyInfluence` declares one radius per effect;
  `region_radius_cells` takes the maximum, since a region narrower than any single effect would let
  that effect read a stale neighbour. `EcologyCellSummary` is the per-tick boundary fact a
  neighbouring cell reads without touching its plants — fire reads the same fuel/moisture summary.)
- [x] Advance connected dependency regions from immutable checkpoints/boundaries. Do not catch up one
  cell in isolation when neighbours can affect it. (`dependency_regions` groups cells into the
  transitive closure of "within the region radius"; `advance_region` advances a whole region, each
  cell reading tick-`N` plants and tick-`N` summaries. `VegetationWorld::advance_ecology` refuses to
  run a region unless every cell it spans is resident — an absent neighbour would read as empty
  ground.)
- [x] Publish a completed tick atomically across its affected cells in canonical key order.
  (`EcologyState::publish_region_tick` validates every cell's successor tick before storing any, so
  a refused publication leaves no cell moved; `advance_region` computes the whole region before
  returning, so a failure mid-region publishes nothing.)
- [x] A catch-up budget may delay simulation-facet readiness, but cannot drop ticks or silently alter
  results. (`EcologyCatchUpBudget` bounds ticks per call; what it does not run is reported as
  `ticks_owed` and resumes at the same tick.
  `a_catch_up_budget_delays_readiness_without_changing_results` proves three budgeted calls and one
  unbudgeted call reach the same bytes, and that a lagging region reads as not caught up.)
- [x] Any analytical fast-forward operator needs a formal equivalence contract and property tests;
  otherwise execute fixed ticks in bounded background jobs. (No analytical operator exists: catch-up
  executes every owed tick through the same `advance_cell` a live frame runs, bounded by the budget.
  The world clock may jump; the cells behind it may not.)

## Disturbance and external hooks

- [x] Fold persistent harvest, damage, clearing, planting, moisture/fuel, burn phenotype, and products
  into the same reducer/state machine. (One vocabulary, one reducer: `Harvest`, `Damage`, `Tombstone`
  and `LifecycleTransition { to: Removed }` for clearing, `Planting`, `MoistureFuel`, `Burn` with its
  burned phenotype, `Ignite`/`Extinguish`, `Regrow`, and `DisturbanceMask`. Products are the
  `Harvested { phenotype }` transition a script turns into an item — vegetation owns the plant's
  state, not an inventory.)
- [x] Vegetation exposes fuel/moisture/health/occupancy queries and typed ignite/burn/extinguish/wet
  mutations. The future fire system owns heat propagation and smoke. (`combustion_sample` answers
  plants/ignited/fuel/moisture/health/occupancy over a volume, and every plant snapshot carries
  `ignited`. The typed replies are `Ignite`, `Burn`, `Extinguish`, and `MoistureFuel` — wet. Nothing
  in vegetation models heat, spread, or smoke; the `IGNITED` flag records only that a plant is
  alight, and the fuel it burns. Drivable as `sa vegetation-combustion` and `sa vegetation-mutate`.)
- [x] Future weather owns precipitation/weather dynamics; vegetation consumes sampled inputs and owns
  persistent biological response. (`EcologyWeather { water, warmth }` is a per-tick input handed to
  the tick; nothing in vegetation computes, integrates, or stores weather. What vegetation owns is
  the response: moisture settles toward the supply, fuel accumulates with dryness, suitability
  governs health, and warmth below `DORMANCY_WARMTH` idles growth without stopping age.)
- [x] Nav/physics/render facets observe committed tick generations only. (Every facet reads a
  published `VegetationCellGeneration`, which is only replaced by `publish_state_rebuilds` after a
  reduction commits. A tick's mutations and its boundary summaries land in that same commit, and
  `EcologyState::is_caught_up` tells a reader whether a cell's simulation facet stands at world time
  — a region mid-catch-up publishes nothing for a facet to see.)

## Debug and control

Add timeline/tick pause-step-run controls and views for age, life stage, health, moisture, fuel,
competition/shade, suitability, seed/propagation candidates, succession, dependency region,
checkpoint/boundary generation, pending catch-up, and emitted transitions. `sa` can deterministically
advance a bounded fixture and dump canonical cell/checkpoint hashes.

- [x] `sa` drives and dumps. `vegetation-advance-ecology` steps a bounded fixture by an explicit tick
  budget and returns what ran and what is owed; `vegetation-ecology-status` dumps world time, the
  rule-set version, the checkpoint hash, every dependency region with its tick and residency, and
  every cell's boundary summary. Age, life stage, health, moisture, and fuel per plant already come
  from `vegetation-runtime-inspect`; shade and suitability are the summary's canopy plus the tick
  rules that read it; emitted transitions come from `vegetation-drain-events`.
- [x] Editor timeline panel: pause-step-run over the ecology clock, with the region/catch-up overlay.
  (`editor/src/panels/EcologyTimelinePanel.tsx`, registered as the `ecologyTimeline` dock panel.
  STEP and RUN, never a seek bar: biological time only moves forward and it moves by EXECUTING ticks,
  so a slider that could drag the clock backwards would promise something the simulation cannot do.
  Run advances in chunks and re-reads the clock between them, so a catch-up owing thousands of ticks
  stays interruptible and the panel keeps repainting; the pause flag is a ref rather than state, or a
  stale closure would keep stepping after the user pressed pause. The region table separates CAUGHT UP
  from WAITING ON RESIDENCY — different problems: one is work still owed, the other is ground that has
  not loaded — and reports the world tick, rule-set version, region radius, checkpoint, and the last
  advance's ticks run against ticks owed. Failures route through the Toaster per the editor's one
  error location, and the checkpoint uses the Tooltip primitive rather than a native `title`.)
  Belongs with the phase-9 authoring surfaces rather than the simulation core.

## Acceptance

- [x] Continuous simulation equals unload→time advance→dependency-region catch-up byte-for-byte.
  (`catch_up_equals_continuous_simulation` in `runtime_world.rs` runs one world tick by tick and
  another by jumping to tick 8 and catching up, then compares `canonical_bytes()` of the reduced
  persistent state, not just the checkpoint hash. `a_region_awaiting_residency_owes_its_ticks` adds
  the unload leg: time advances while nothing is loaded, and the reload catches up to the same
  bytes a resident-throughout world holds.)
- [x] Results are identical across worker counts, shuffled plant/cell order, different residency paths,
  and origin rebasing. (`disjoint_regions_commit_the_same_state_in_either_order` covers region order,
  which is what a worker count varies — catch-up runs on the calling thread and a region is a pure
  function of its inputs, so there is no shared accumulator for a worker to race on.
  `a_tick_is_reproducible_and_neighbour_order_independent` covers neighbour order,
  `unordered_plants_are_refused` plus the sort in `region_state` pins plant order, and
  `a_region_awaiting_residency_owes_its_ticks` covers the residency path. Origin rebasing is a
  render-space transform: every ecology input is a quantized world tick or a `UnitInterval`, and no
  rule reads a camera-relative value.)
- [x] Cross-cell shade/competition/propagation seam fixtures have no double update or border artifact.
  (`the_shade_seam_updates_each_plant_once` puts a clearing inside a ring of dense canopy: the
  neighbours' shade reaches across every border and costs the plant health, while every plant in the
  five-cell region is touched by exactly one cell's tick. Propagation crosses the seam through
  `seed_point`, which owns a seed by the cell it landed in, and `advance_ecology` keys that record by
  `point.owner` rather than the cell whose tick produced it.)
- [x] Calendar rewind affects phenology preview only; biological state remains monotonic.
  (`EcologyClock::advance_to` refuses a target behind world time, and a cell's tick may only step to
  its successor, so nothing biological can be rewound. The tick emits no phenology: its whole
  mutation vocabulary is `LifecycleTransition`, `StateOverride` (no phenology field), `MoistureFuel`,
  `Planting`, and `Regrow`. Phenology stays the calendar-driven preview computed by `season.rs`,
  which a rewind is free to move.)
- [x] Lifecycle events are emitted exactly once across snapshot, compaction, unload, and replay.
  (`record_transitions` is reached from `apply_confirmed_mutations` alone. A snapshot import or a
  compaction goes through `replace_persistent_state`, which republishes without emitting; a replayed
  transaction reduces to nothing and so carries no transitions; an unload drops a generation and
  touches no ledger. A prediction is transient and never reaches the ring.)
- [x] Catch-up cancellation/supersession cannot publish a partial tick generation.
  (`advance_region` computes every cell before returning anything, `publish_region_tick` validates
  every cell's successor tick before storing any, and `advance_region_one_tick` checks that
  publication precondition *before* committing mutations — so a refusal at any point leaves neither
  the plant deltas nor the tick generation behind. `a_region_publishes_a_whole_tick_or_none_of_it`
  proves a skipping publication moves no cell.)
- [x] CPU cost/memory scales with active simulation macro cells, not micro vegetation or render detail.
  (`region_state` reads `macro_points` and nothing else; the tick has no access to micro fields,
  render references, or render bounds, and holds one `EcologyPlantState` row per macro plant plus one
  `EcologyCellSummary` per cell. A region that is not resident costs a residency check.)
- [x] Standard gate, determinism/property tests, and lifecycle/ecology docs are green.
  (`just prepare-for-commit` EXIT=0; `cargo test --workspace` green apart from one `xtask` shader
  test another agent's in-flight change owns. Determinism/property coverage: 20 ecology tests in
  `saffron-vegetation` plus the three catch-up acceptance tests over a live `VegetationWorld`.
  `tests/e2e/vegetation-ecology.test.ts` drives the same claims through the real host — 2 files, 30
  assertions, validation-clean. Docs: the new `explanations/scene-and-ecs/ecology-catchup.md` plus
  its hub row and the reworked `vegetation-state.md` sections; `hugo --gc` EXIT=0, links none broken,
  style 0 errors 0 warnings.)

## NO-LEGACY gate

There is one macro lifecycle state and one fixed-tick reducer path. Visual season tint, mesh choice,
promoted entity fields, or fire/nav adapters never become alternate lifecycle authorities.

