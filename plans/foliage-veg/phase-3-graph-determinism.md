# Phase 3 — Typed biome graph and determinism gate

**Status:** IN PROGRESS

**Depends on:** Phases 1–2

This phase builds the one evaluator used by editor preview, offline cell compilation, and runtime
generation. It combines field-driven distribution, explicit authoring, competition/claim rules, and
recursive communities without allowing GPU scheduling or floating-point variation to define
persistent plants.

## Typed graph IR

- [x] Define typed domains for scalar/vector/surface fields, candidate streams, accepted macro
  points, cosmetic micro fields, regions, splines, species/community tables, and diagnostics.
- [x] Give every node a stable GUID, semantic/config revision, typed pins, parameter schema,
  cardinality/bounds estimator, spatial level, finite influence radius, dependency sources,
  deterministic seed namespaces, authority class, and execution-domain capabilities.
- [x] Make nodes pure by default. Stateful lifecycle simulation is a later fixed-tick system, not a
  hidden graph side effect.
- [x] Compile `.sbiome` roots and modules into one validated IR with typed module interfaces, cycle
  rejection, bounded recursion, strict current-version validation, dependency hashes, and stable
  debug symbols. Noncurrent graph and node versions are rejected; there is no migration path.
- [x] Show predicted candidate/accepted counts, retained and evaluator-generated input bytes,
  preflight and execution allocation peaks, `memoryBytes` as their maximum, transfer cost, and hard
  caps before execution. Unbounded operators are rejected; global operators run at an ancestor/global
  stage and emit tiled immutable results.

## Authority and taint

Define at least:

- `Authoritative`: canonical numerics; may affect macro identity, persistence, collision, nav,
  queries, ecology, or gameplay;
- `EquivalentGpu`: GPU implementation proven byte-equivalent to the reference for this node/profile;
  and
- `Cosmetic`: spatially stable visual output only.

Taint flows through graph edges. The compiler rejects any cosmetic/noncanonical value reaching a
macro accept, stable ID, collision/nav output, persistent mutation, ecology input, or gameplay query.
GPU micro grass is reconstructed from authoritative quantized density/attribute tiles, but individual
blades are cosmetic and need not be cross-vendor bit-identical.

## Required operator vocabulary

- [x] Surface projection in arbitrary direction, provider/tag/material selection, and attachment.
- [x] Deterministic noise/field math, images/painted tiles, gradients, curves, remap/combine/clamp.
- [x] Altitude, slope, curvature, drainage, moisture, temperature, precipitation, exposure,
  sunlight/shade, water/spline/shape/blocker distance.
- [x] Stratified/jittered coverage, Bridson-style blue-noise/Poisson, weighted elimination,
  variable prototype-aware spacing, field importance, cluster/patch/colony, spline/edge following,
  explicit anchors, and recursive child/companion placement.
- [x] Transform/orient-to-surface/random yaw/scale/variation, with canonical distributions.
- [x] Priority/exclusion, bounds-aware overlap, crown/root competition, deterministic claims,
  suitability curves, community blending, shade-tolerant undergrowth, and succession inputs.
- [x] Macro point output and micro density/attribute tile output through the canonical schemas.

## Borders, hierarchy, and ordering

- [x] Each partitioned node declares its support radius; evaluator jobs read an immutable halo and
  publish only points owned by the canonical output cell.
- [x] Cross-cell competition reads immutable previous-stage candidates and uses stable IDs plus exact
  tie-breaks. It cannot observe neighbouring job completion order.
- [x] Coarse plants are produced once at their declared hierarchy level and referenced by finer cells.
- [x] Candidate identities are derived before acceptance and do not depend on accepted-array order.
- [x] Changing seed/topology is an explicit destructive edit with previewed accepted/removed/moved
  ID diff and override conflict report.

## Executors

- [x] Implement a canonical Rust reference interpreter using the Phase-1 numeric vocabulary.
- [x] Add parallel CPU execution whose result is canonical-sorted before publication.
- [x] Add a Slang compute executor for cosmetic nodes and for authoritative nodes only after per-node
  CPU/GPU byte-equivalence qualification.
- [x] Compile one IR; CPU/GPU scheduling groups are execution plans, not different semantics. Transfer
  boundaries and estimated/actual bytes/timing are visible diagnostics.
- [x] Add cancellation and deadline checks plus hard count/memory/transfer/time safety caps without
  silently lowering density or quality. Recheck the matching job before final publication;
  cancellation or failure publishes nothing.

## Initial surface inputs

Use the Phase-1 mesh `SurfaceField` provider and authored map fields. Precompute canonical quantized
surface tiles for runtime-authoritative evaluation. Heightfield terrain later publishes the same
channels; no terrain node or terrain grass output is added here.

## Control and diagnostics seam

Add generated commands and `sa` surfaces to compile a biome, preflight a bounded region into a
prepared job, explicitly start/status/cancel that exact job, inspect a node's typed input/output
schema, list dependencies/halo, return candidate/accepted/rejected counts, and explain one point's
provenance/rejection. Preflight reports retained and generated input bytes, separate symbolic
admission and execution peaks, and their maximum as the concrete-job memory bound. These are real
evaluator calls shared with the future editor, not a second debug interpreter.

## Acceptance

- [x] Identical graphs produce byte-identical macro columns under shuffled inputs, worker counts,
  job schedules, cell request orders, origin rebases, and repeated cooks.
- [x] Halo/seam fixtures produce no duplicate or missing macro plants at cell faces/corners.
- [x] An unrelated node edit preserves untouched random streams and plant IDs.
- [x] Cross-cell competition is identical whether neighbours cook serially, reversed, or in parallel.
- [ ] Every dual-domain node passes Rust/Slang equivalence on NVIDIA, AMD, and MoltenVK before it can
  carry `EquivalentGpu`.
  (HARDWARE-GATED, and the gate is the only thing holding it. This machine enumerates exactly one
  Vulkan device — `Apple M4` through `MoltenVK`, api 1.4.334 — so the NVIDIA and AMD arms cannot be run
  here at all, and no amount of code closes them. What IS verified on MoltenVK: the dual-domain
  qualification corpus covers every declared operator with resident programs, the ABI and corpus hashes
  are pinned, branching and terminal masks preserve exact semantics, and only complete
  program/profile/artifact evidence is admitted — `saffron-vegetation` `graph_gpu` 7/7. Recorded rather
  than claimed: a box that says three vendors is not closed by one.)
- [x] The compiler rejects cosmetic-to-authoritative dependencies, unbounded local influence, cycles,
  and NaN/overflow inputs with typed diagnostics.
- [x] Concrete-job preflight exposes retained/generated inputs and admission/execution peaks, defines
  memory as their maximum, and rejects every count, memory, transfer, worker, tile, and deadline
  excess. The matching evaluator publishes only a complete result within the admitted boundary.
- [x] `sa` provenance/rejection output traces map→layer→biome→node→candidate→plant.
- [x] Standard milestone gate, generated protocol checks, real-host preflight lifecycle, and
  graph/evaluator docs are green.

## NO-LEGACY gate

Preview, offline cook, and runtime generation invoke the same compiled IR and evaluator. Brush output,
explicit anchors, procedural placement, and micro fields do not gain separate placement algorithms or
point formats.

## Platform conformance

- MoltenVK on Apple M4: the 32-word spatial corpus and the resident graph corpus pass on the physical
  GPU with identical Rust/Slang hashes and zero validation issues. The bound record is
  `benchmarks/foliage-veg/compute-conformance-apple-m4-moltenvk.json`.
- NVIDIA: pending access to a physical supported GPU.
- AMD: pending access to a physical supported GPU.

## Progress

- 2026-07-21: All locally executable Phase-3 work and gates are green: `just engine`,
  `just prepare-for-commit`, the 207-command live schema contract, `just test`, the 301-case live
  E2E suite, the schema-harness unit tests, strict all-target rendering Clippy, and the docs
  build/link/style checks.
- The live schema gate exposed and verified two presentation defects. Presentation semaphores are
  owned by a typed acquire-to-present transaction, internal offscreen renders cannot signal them,
  and aliased slot/image fences are deduplicated and waited before reset. Seven consecutive
  post-fix schema runs passed 207/207 without a timeout or Vulkan validation issue.
- Physical NVIDIA and AMD CPU/Slang conformance records remain deferred verification work. They do
  not block feature implementation. The next implementation step is Phase 4, followed by every
  remaining phase in dependency order; the deferred platform records stay visible and unchecked
  until the required hardware is available.
- Work paused at the Codex weekly usage guard (90%). The reported reset is
  `2026-07-27T22:26:57+02:00`.
