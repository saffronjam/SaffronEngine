# Phase 15 — Production, export, platform, and scale closure

**Status:** NOT STARTED

**Depends on:** Phases 1–14

This phase proves that the complete system is deterministic, debuggable, packageable, and
quality-equivalent at production scale. It closes every inventory, command, editor, documentation,
platform, performance, and failure-mode gate; it does not lower density or semantics to make a test
pass.

## Export and cook closure

- [ ] Extend project/export cooking to build exact plant/biome/map dependency DAGs, platform-profiled
  `.splantc`, sectioned `.svegcell`, manifests, initial persistent-state baseline, shaders/PSOs,
  textures/materials, collision/nav contributions, and license attribution.
- [ ] Package only required content-addressed roots/pages/cells plus dependency closure. Runtime never
  scans authored source directories or source-format files.
- [ ] Add parallel/distributed-safe work-item manifests, cancellation/resume, cache sharing, atomic
  publish, corruption repair, and deterministic final package ordering.
- [ ] Produce cook reports for source/output size, page/cell/facet distribution, peak memory, work,
  cache hits, warnings/errors, and content/manifest IDs.
- [ ] Boot `saffron-player` from a clean exported package and verify editor/host/player share formats
  and render semantics without editor-only fallbacks.

## Future networking contract closure

Networking itself remains owned by its planset. Provide and test the vegetation inputs it will use:

- cell-interest keys/facets and exact base-manifest handshake/rejection;
- canonical cell state snapshots plus sequenced/idempotent mutation tails;
- transaction/precondition/authority/tick fields and duplicate/reorder-safe reduction;
- promoted-entity `PlantOrigin` handoff and snapshot during promotion;
- deterministic late-join state fixtures and periodic checkpoint hashes; and
- local reconstruction of wind/micro bend while only persistent macro/disturbance/ecology state is
  transmitted.

Do not implement transport, connection authority, retransmission, or general replication here.

## Integrated observability

- [ ] Expose CPU evaluator/cook/runtime/simulation time; resident macro/micro/cell/page bytes by facet;
  job queues/cancellation/latency; mutation/snapshot size; and query/promotion/Jolt/nav counts.
- [ ] Expose GPU instance/node/cluster/triangle/voxel counts, cull stages, HZB retests, bins/indirect
  draws, page faults/latency, overdraw/quad utilization, deformation, VSM pages/cache/dirty work,
  GI/RT/BLAS/OMM metrics, and every pressure/overflow flag.
- [ ] Add Perfetto/capture integration, `sa` inspection/export, editor overlays/tables, and actionable
  budget alarms with cell/family/provenance ownership.
- [ ] No diagnostic reads back per-instance data every frame; instrumentation uses compact counters
  and explicit capture modes.

## Determinism and failure matrix

Automate named tests for:

- different worker counts, job/input/cell/source order, origin rebasing, negative coordinates, and
  unrelated graph edits;
- cell faces/corners, large halos, hierarchy levels, cross-cell transactions and competition;
- cancellation/supersession, atomic publication, corrupt/truncated/unknown artifacts, disk-full and
  interrupted writes;
- cache deletion/recook under authored overrides and runtime tombstones;
- snapshot/compaction, duplicate/reordered mutation envelopes, manifest mismatch, and late join;
- promotion ownership/save/unload/recook races, Jolt batch churn, contacts, nav dirtying, and products;
- continuous versus unload/catch-up ecology and exact-once transitions;
- page loss/eviction/arena growth, camera cut/teleport/resize, rapid wind/interaction/phenotype,
  triangle↔voxel transition, HZB/VSM history, and TAA;
- depth/main/VSM/GI/reflection/RT coverage and thin-sheet response parity; and
- pathological graph density/cardinality/memory inputs that must reject/cancel rather than silently
  reduce fidelity.

## Platform quality parity

- [ ] Validate NVIDIA Vulkan required+mesh+KHR RT+OMM/optional NV tiers.
- [ ] Validate AMD Vulkan required+mesh where present+KHR RT, including subgroup/workgroup variation.
- [ ] Validate Apple through MoltenVK using required indexed MDI, portable voxel representation,
  physical-atlas VSM, and any-hit/available KHR features without mesh-shader quality loss.
- [ ] Validate software/headless correctness where GPU capabilities are absent, with explicit test
  scope and no claim that software performance is representative.
- [ ] Query and record individual feature bits/limits; extension names and vendor IDs do not select
  semantic content.
- [ ] Compare representative images and error metrics across executors/platforms. Capability tiers may
  change cost, never authored species, LOD meaning, material response, shadow/GI representation, or
  persistent state.

## Performance closure

Use the Phase-1 baselines to set project-owned budgets for editor stroke latency, incremental cook,
cell publication, source travel/prefetch, frame CPU, GPU visibility/deformation/main/VSM/GI/RT,
memory/residency, promotion/Jolt, simulation/catch-up, and export. Check in representative stress
worlds and camera/simulation paths. If a budget fails, optimize data/algorithms/scheduling; do not
lower default vegetation density, disable distant motion, remove shadows/GI/RT, or introduce a
lower-quality content path.

## Final repository closure

- [ ] Regenerate protocol TypeScript from Rust and verify every DTO/command/component/asset inventory,
  schema fixture, control/client helper, editor panel, create/inspect route, and `sa` help entry.
- [ ] Run `just engine`, `just prepare-for-commit`, `just schema`, `just test`, `just e2e`, export/player
  smoke, headless validation, and platform suites.
- [ ] Complete docs for spatial cells, plant assets, biomes, authoring, rendering, wind/phenology,
  VSM/lighting/RT, interaction/physics/queries, persistence, ecology, botanical authoring, and tooling;
  update every hub row using the docs-page skill.
- [ ] Remove stale pending-plan claims and verify no superseded foliage/wind/renderer/shadow path or
  documentation survives.
- [ ] Mark every phase and this README `COMPLETED` only after the integrated destination is green.

## NO-LEGACY gate

The final tree contains one vegetation architecture and one production renderer. Experimental work
graphs remain a future executor over the preserved IR, not shipped alongside as another system.

