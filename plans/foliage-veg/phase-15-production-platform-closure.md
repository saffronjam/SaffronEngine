# Phase 15 — Production, export, platform, and scale closure

**Status:** IN PROGRESS

**Depends on:** Phases 1–14

This phase proves that the complete system is deterministic, debuggable, packageable, and
quality-equivalent at production scale. It closes every inventory, command, editor, documentation,
platform, performance, and failure-mode gate; it does not lower density or semantics to make a test
pass.

## Export and cook closure

- [x] Extend project/export cooking to build exact plant/biome/map dependency DAGs, platform-profiled
  `.splantc`, sectioned `.svegcell`, manifests, initial persistent-state baseline, shaders/PSOs,
  textures/materials, collision/nav contributions, and license attribution. (The cooker already builds
  the dependency DAG (`CookGraph`, `cook_graph_hash`, `CookDependency` on every manifest cell), the
  platform-profiled `.splantc`, the sectioned `.svegcell` including its `CollisionInputs` and
  `NavigationContributions` facets, and the manifest; `export-app` copies the shaders and the
  textures/materials. Added here: the INITIAL PERSISTENT-STATE BASELINE — a `Baseline` artifact kind
  keyed by the manifest it belongs to (a generation has exactly one starting state), published by
  `vegetation-state-baseline` behind the same promoted-state flush a save takes, carried by the export
  closure, and imported by the runtime when it binds that generation, so a package boots into the world
  the author saw. A baseline that does not decode against the generation is a hard error rather than a
  silent skip. And LICENSE ATTRIBUTION: `export-app` writes `ATTRIBUTION.txt` with one line per
  packaged plant source whose provenance requires attribution — an obligation that lives only in the
  editor is one the shipped product breaks.)
- [x] Package only required content-addressed roots/pages/cells plus dependency closure. Runtime never
  scans authored source directories or source-format files. (`vegetation_export_closure` walks the
  manifest — generation root, manifest, every named `.splantc` and `.svegcell` — and `export-app`
  copies exactly that into the package's store, which it previously omitted ENTIRELY: the store lives
  beside `assets/` and the asset copy never saw it, so an exported player bound no manifest and came up
  bare. The closure is computed from the manifest rather than a directory scan, so a superseded
  artifact stays out. The authored `.splant`/`.sbiome`/`.svegmap` files and their sidecar packages are
  excluded from the packaged `assets/` by `is_authored_vegetation`; that is safe because the project
  loader treats the filesystem as the source of truth and drops a catalog row whose file is absent,
  and because vegetation binds by identity through the artifact store rather than through the catalog.)
- [ ] Add parallel/distributed-safe work-item manifests, cancellation/resume, cache sharing, atomic
  publish, corruption repair, and deterministic final package ordering. (ATOMIC PUBLISH: every artifact
  and every generation root writes through `AtomicWriteFile` and is re-read and rehashed before the
  publication is reported. CANCELLATION: `GraphCancellationToken` plus the cook queue's
  cancel/supersede transitions, now counted. RESUME: content addressing gives it by construction — a
  re-run of an interrupted cook hits the cache for every node that published and re-does only the rest,
  which the cook statistics report as hits against misses. CACHE SHARING: the store is
  content-addressed and lock-guarded, so two cooks over one project share every hit.
  CORRUPTION REPAIR: `verify_vegetation_artifacts` rehashes every artifact the current generations name
  — the file's NAME is the hash of its bytes, so verification needs no side table that could itself rot
  — and `vegetation-verify-artifacts {repair}` removes the corrupt ones so the next cook republishes
  them. Repair deletes rather than rewrites: the bytes are the only copy, and the cooker's cache-miss
  path is already the thing that produces them. DETERMINISTIC PACKAGE ORDERING: the export closure is a
  `BTreeSet` walked in canonical path order, so the same generation packages the same file sequence.
  NOT YET: parallel/DISTRIBUTED work-item manifests. A serializable claim/publish unit that separate
  machines can take is its own design, and the cook queue is single-process today.)
- [x] Produce cook reports for source/output size, page/cell/facet distribution, peak memory, work,
  cache hits, warnings/errors, and content/manifest IDs. (`VegetationCookStatisticsDto` reports nodes,
  elapsed micros, peak memory, input and output bytes, cache hits and misses, published cells, and
  per-reason rejection totals. `export-app` reports, per map: the manifest identity, the family and
  cell counts, missing artifacts, macro plants, closure bytes, and STORED BYTES PER CELL FACET across
  every packaged cell — read from each cell's table of contents rather than by decoding it, because a
  size report that decodes every section costs as much as loading the world it reports on. Warnings
  ride the existing `ExportAppResult.warnings`, including a missing-artifact warning naming the map
  and the count.)
- [ ] Boot `saffron-player` from a clean exported package and verify editor/host/player share formats
  and render semantics without editor-only fallbacks. (The PACKAGE half is verified end to end by
  `tests/e2e/vegetation-export.test.ts`: a real cook, a real `export-app`, then assertions that the
  staged package carries `.svegcell`/`.svegmanifest`/`.splantc` and none of `.splant`/`.sbiome`/
  `.svegmap`, with the report naming the manifest identity and zero missing artifacts. The BOOT half
  is blocked on this machine: `saffron-player` creates its renderer and then hangs on frame 1 under
  MoltenVK in a non-interactive context — the frame watchdog reports `GPU submission 'frame 1' has
  been in flight 119s`, with no project and no vegetation involved, so it is the player's windowed
  present path rather than anything the export does. Reproduce with
  `SAFFRON_EXIT_AFTER_FRAMES=5 ./engine/target/debug/saffron-player`.)

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

- [x] Expose CPU evaluator/cook/runtime/simulation time; resident macro/micro/cell/page bytes by facet;
  job queues/cancellation/latency; mutation/snapshot size; and query/promotion/Jolt/nav counts.
  (`vegetation-telemetry`: per-stage synchronization durations — residency, promotion, collision,
  navigation, ecology — as the last sample and an eighth-weighted exponential average; resident bytes
  by facet from the residency report; the cook queue's live/submitted/completed/cancelled/superseded/
  failed counts with summed acceptance-to-terminal latency; canonical mutation bytes via
  `VegetationMutationRecord::canonical_byte_len` and snapshot bytes; and query, query-hit, promoted,
  Jolt body, and navigation-contribution counts. `sa vegetation-telemetry` formats all of it.)
- [ ] Expose GPU instance/node/cluster/triangle/voxel counts, cull stages, HZB retests, bins/indirect
  draws, page faults/latency, overdraw/quad utilization, deformation, VSM pages/cache/dirty work,
  GI/RT/BLAS/OMM metrics, and every pressure/overflow flag. (Already on the wire through
  `render-stats`: instances, triangles, semantic records, aggregate-voxel records, max cut depth,
  frustum and occlusion cull counts, HZB retests, transparent draws, micro candidates, sub-quad
  triangles, draw calls and batches, RT instances, VRAM usage against budget, per-pass timings, a
  pipeline-stats profiler mode, the VSM page/cache/dirty/evict/overflow set, page residency
  registered/resident/bytes/budget/requested/loading/ready/evictions, and BOTH the overflow and
  pressure flag words — a silent capacity clamp is how geometry disappears, so those flags are the
  point. Added here: PAGE FAULTS AND LATENCY, priced from demand to the moment the payload can be
  drawn rather than to when the bytes arrived, kept as a count plus a summed microsecond total so the
  counter stays additive and the caller picks its window. NOT YET: a distinct bin count, overdraw,
  deformation counts, and BLAS build/memory metrics; OMM metrics cannot exist because
  `VK_EXT_opacity_micromap` derivation is not built and this machine has no ray-tracing hardware to
  exercise it.)
- [ ] Add Perfetto/capture integration, `sa` inspection/export, editor overlays/tables, and actionable
  budget alarms with cell/family/provenance ownership.
- [ ] No diagnostic reads back per-instance data every frame; instrumentation uses compact counters
  and explicit capture modes. (Holds for the CPU vegetation path: every counter is incremented where
  the work happens, the stage timing is five durations, and per-plant detail is an explicit request
  through `vegetation-runtime-inspect`/`-query`/`vegetation-cell-inspect`. The box stays open until
  the GPU telemetry above is built, since it is a claim about EVERY diagnostic.)

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
  (HARDWARE-GATED. This machine enumerates exactly one Vulkan device — `Apple M4` through
  `MoltenVK`, api 1.4.334 — so there is no NVIDIA or AMD adapter to validate against here, and no code
  change closes it. Recorded rather than claimed.)
- [ ] Validate AMD Vulkan required+mesh where present+KHR RT, including subgroup/workgroup variation.
  (HARDWARE-GATED. This machine enumerates exactly one Vulkan device — `Apple M4` through
  `MoltenVK`, api 1.4.334 — so there is no NVIDIA or AMD adapter to validate against here, and no code
  change closes it. Recorded rather than claimed.)
- [x] Validate Apple through MoltenVK using required indexed MDI, portable voxel representation,
  physical-atlas VSM, and any-hit/available KHR features without mesh-shader quality loss. (Every gate
  in this plan runs on exactly this configuration: `Apple M4` through `MoltenVK`, api 1.4.334, the only
  device the machine enumerates. `just e2e` at 328/328 across 51 files, `just schema` with all 249
  manifest-driven checks, `cargo test --workspace`, and the standard gate all pass there. MoltenVK does
  not expose `VK_KHR_draw_indirect_count`, so every fixed-slice indirect draw is bounded by the real
  record count instead — `ExecutorDrawInputs::draw_bound`, clamped to the live bound the mirror
  publishes — which is the required indexed-MDI path rather than a lesser one, and the transparent pass
  takes the same bound. The portable aggregate-voxel representation and the physical-atlas VSM are
  exercised by `tests/e2e/vsm.test.ts` (4/4) and the vegetation matrix. Mesh shaders are absent here and
  nothing degrades for it: the executor path is the one path, not a fallback.)
- [ ] Validate software/headless correctness where GPU capabilities are absent, with explicit test
  scope and no claim that software performance is representative. (HEADLESS is validated: the whole e2e
  suite runs offscreen (`SAFFRON_EDITOR_NATIVE_VIEWPORT=1`, no compositor surface) at 328/328, and every
  device-requiring unit test states its scope explicitly by skipping when `Device::new` fails rather
  than passing vacuously. CAPABILITY-ABSENCE correctness is validated for the two capabilities this
  device actually lacks — no `VK_KHR_draw_indirect_count` and no mesh shaders — with the record-bounded
  indirect path and the executor path serving both. NOT validated here: a SOFTWARE rasterizer.
  `just run-software` forces llvmpipe, which is a Mesa driver with no macOS equivalent, so the software
  arm needs the Linux toolbox. No performance claim is made either way.)
- [x] Query and record individual feature bits/limits; extension names and vendor IDs do not select
  semantic content. (Audited: `vendor_id`, `device_id`, `driver_id`, both UUIDs, and the `molten_vk`
  flag are RECORDED into `VulkanProfileEvidence` and `GpuExecutionProfile` and read by nothing that
  chooses behaviour — `is_molten_vk()` has exactly three callers and all three only stamp it into an
  evidence record. Behaviour keys on FEATURE BITS AND LIMITS instead: `capabilities.draw_indirect_count`
  from `features12.draw_indirect_count`, `mesh_shader` from the extension's own feature struct, and the
  advertised limits. That is what lets one code path serve a device with a missing feature rather than a
  vendor-shaped branch.)
- [ ] Compare representative images and error metrics across executors/platforms. Capability tiers may
  change cost, never authored species, LOD meaning, material response, shadow/GI representation, or
  persistent state.
  (Blocked by the same gate: a cross-platform image comparison needs at least two platforms. The
  invariant it protects IS enforced in code and tested here — capability tiers change cost only, because
  behaviour keys on feature bits rather than vendor identity (the box above) and the executor path is
  the one path rather than a per-tier variant.)

## Performance closure

Use the Phase-1 baselines to set project-owned budgets for editor stroke latency, incremental cook,
cell publication, source travel/prefetch, frame CPU, GPU visibility/deformation/main/VSM/GI/RT,
memory/residency, promotion/Jolt, simulation/catch-up, and export. Check in representative stress
worlds and camera/simulation paths. If a budget fails, optimize data/algorithms/scheduling; do not
lower default vegetation density, disable distant motion, remove shadows/GI/RT, or introduce a
lower-quality content path.

## Final repository closure

- [x] Regenerate protocol TypeScript from Rust and verify every DTO/command/component/asset inventory,
  schema fixture, control/client helper, editor panel, create/inspect route, and `sa` help entry.
  (`cargo run -p xtask -- gen-protocol` regenerates `sa-types.ts`, the envelope schema, the OpenRPC
  document, the command manifest, and the Luau defs; five `xtask` byte-identity tests then refuse any
  drift between what the DTOs generate and what is committed. The inventories are enforced rather than
  reviewed: `saffron-protocol` 675 tests pin `DTO_TYPE_NAMES`, the frozen command list, and the domain
  ordering; `saffron-control` 107 include `registry_covers_the_protocol_manifest`, which fails on a
  registered command the manifest does not name and vice versa; `sa` 66 cover the help entries and the
  text formatters; `just schema` runs 249 manifest-driven live-vs-schema checks. Editor side:
  `bun run check` regenerates and typechecks, so a panel or client helper referencing a DTO that no
  longer exists fails the build — which is how this session's `variation`/`grafts` and tagged-enum
  codegen facts surfaced.)
- [ ] Run `just engine`, `just prepare-for-commit`, `just schema`, `just test`, `just e2e`, export/player
  smoke, headless validation, and platform suites. (GREEN on 2026-07-26: `just engine` EXIT=0,
  `just prepare-for-commit` EXIT=0, `just schema` EXIT=0 with 249 manifest-driven checks, `just test`
  EXIT=0 — which required fixing a committed stale reference to the deleted `point_shadow.slang`,
  see the box below — `just e2e` 328/328 across 51 files, the export smoke through both
  `tests/e2e/export-app.test.ts` and `tests/e2e/vegetation-export.test.ts`, and headless validation as
  the mode the whole suite runs in. TWO ARMS OUTSTANDING: the PLAYER smoke, blocked by
  `saffron-player` hanging on frame 1 under MoltenVK in a non-interactive context (reproduce with
  `SAFFRON_EXIT_AFTER_FRAMES=5 ./engine/target/debug/saffron-player`; the frame watchdog reports it, and
  it happens with no project and no vegetation, so it is the windowed present path); and the PLATFORM
  suites, which need the NVIDIA and AMD adapters this machine does not have.)
- [x] Complete docs for spatial cells, plant assets, biomes, authoring, rendering, wind/phenology,
  VSM/lighting/RT, interaction/physics/queries, persistence, ecology, botanical authoring, and tooling;
  update every hub row using the docs-page skill. (One page per concept, each with its hub row: spatial
  cells `spatial-world.md`; plant assets `vegetation-assets.md`; biomes `biome-graph-evaluation.md`;
  botanical authoring `botanical-graph.md`; interchange `point-interchange.md`; cooking
  `vegetation-cooking.md`; rendering `plant-rendering.md` with `virtual-geometry.md`,
  `persistent-gpu-scene.md`, `hierarchical-visibility.md`, and `page-residency.md`; wind/phenology
  `wind-field.md`; VSM `virtual-shadow-maps.md`; interaction/physics/queries `vegetation-collision.md`,
  `vegetation-navigation.md`, and `plant-promotion.md`; persistence `vegetation-state.md`; ecology
  `ecology-catchup.md`; tooling `vegetation-telemetry.md`. Verified by the docs-page skill's three
  checks together: `hugo --gc` EXIT=0, `check_links.py` reporting no broken links across 241 pages, and
  `check_style.py` at 0 errors and 0 warnings — the style checker is what enforces the timeless-present
  rule, so a stale status claim in any of these pages would fail it.)
- [ ] Remove stale pending-plan claims and verify no superseded foliage/wind/renderer/shadow path or
  documentation survives. (One real instance found and fixed: the `xtask`
  `geometry_passes_use_the_canonical_coverage_module` test still listed `point_shadow.slang`, deleted by
  the committed refactor that retired the meshlet and point-shadow RASTER paths — so `just test` had
  been failing on a reference to a file the tree no longer has. The list now names the three geometry
  passes that exist. The docs style checker enforces the no-stale-claims rule on prose continuously
  (0 errors across 241 pages). The box stays open until the remaining phases stop carrying open work of
  their own: a "no superseded path survives" claim is only meaningful once there is nothing left to
  supersede.)
- [ ] Mark every phase and this README `COMPLETED` only after the integrated destination is green.

## NO-LEGACY gate

The final tree contains one vegetation architecture and one production renderer. Experimental work
graphs remain a future executor over the preserved IR, not shipped alongside as another system.

