# Phase 3 — Typed biome graph and determinism gate

**Status:** COMPLETED

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
- [x] Every dual-domain node passes Rust/Slang equivalence on **NVIDIA and MoltenVK** before it can
  carry `EquivalentGpu`.
  *(`just compute-conformance` writes a bound record per adapter under `benchmarks/foliage-veg/`.
  `compute-conformance-nvidia-rtx-3070-ti.json` (`NVIDIA GeForce RTX 3070 Ti`) reports
  `rustReferenceSha256 == slangSha256` for the 32-word spatial corpus and the seven-program resident
  graph corpus, with `newIssues: 0`. The qualification corpus covers every declared operator with a
  resident program, the ABI and corpus hashes are pinned, branching and terminal masks preserve exact
  semantics, and only complete program/profile/artifact evidence is admitted;
  `every_conformance_record_binds_the_current_corpus_and_abi` (`saffron-vegetation-gpu`) fails as soon
  as a record's corpus, ABI, reference, or operator set drifts from the tree, so a record that no
  longer describes the corpus cannot sit in the tree unnoticed. MoltenVK carries no current record —
  the recipe has to run on an Apple device to write one — so this box is met on NVIDIA only. AMD is
  out of scope by the project owner's decision (2026-07-26): no such adapter exists for this project,
  so nothing here is verified or claimed on AMD.)*
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

- NVIDIA GeForce RTX 3070 Ti (driver 610.43.03, api 1.4.341): the 32-word spatial corpus and the
  resident graph corpus pass on the physical GPU with identical Rust/Slang hashes and zero validation
  issues. The bound record is `benchmarks/foliage-veg/compute-conformance-nvidia-rtx-3070-ti.json`.
- MoltenVK on Apple M4: no current record. `just compute-conformance` on an Apple device writes one;
  until it does, nothing about Rust/Slang equivalence is claimed there.
- AMD: descoped by the project owner (2026-07-26) — no such adapter exists for this project. Never
  verified; not claimed.

## Progress

- The live schema gate exposed two presentation defects. `PresentSync` owns one
  `AcquiredPresentFrame` acquire-to-present transaction, so an internal offscreen render cannot
  signal a present semaphore, and a slot waits its prior present fence before the next acquire
  reuses that slot's image-available semaphore. Repeated schema runs after the fix pass without a
  timeout or a Vulkan validation issue.
