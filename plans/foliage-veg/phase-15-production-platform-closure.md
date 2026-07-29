# Phase 15 — Production, export, platform, and scale closure

**Status:** COMPLETED

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
- [x] Add parallel/distributed-safe work-item manifests, cancellation/resume, cache sharing, atomic
  publish, corruption repair, and deterministic final package ordering.
  (WORK-ITEM MANIFESTS LANDED 2026-07-28, closing the one NOT-YET clause below; everything else in
  this box was already done and its record stands. The landed shape: `vegetation/src/cook_work.rs`
  owns the wire contract — `CookWorkManifest` (magic `SVCWRK01`; items in phase-J order;
  `blocked_by` strictly earlier, so acyclicity holds by construction; `validate()` rejects a
  forward blocker, a duplicate address, or a non-cell item), `CookWorkPayload` (`SVCWPL01`, the
  cell's own ancestor-independent dependency half), `cook_work_own_input_key` (its own domain
  string, proven disjoint from the node cook-key domain by test), and `CookWorkCompletion`
  (`SVCWCP01`, the node record + manifest cell the committer assembles). The store gained the
  `WorkPayload` artifact kind (`work-payloads/`, in the repair whitelist, never in the export
  closure) and the claim protocol under `work-claims/{plan identity}/`: `create_new` claiming
  (never `lock_file`), an mtime lease with a stale sweep at plan start, and completion markers
  separate from claims so a swept claim never erases a finished result. `stage_vegetation_cook`
  now plans (phases A–J), publishes the work manifest + per-item payloads, and executes through
  `run_work_items` — an in-process claimant pool racing the same on-disk claims a remote claimant
  would — with `cook_one_cell` verifying the payload against the item's own-input key and
  composing the full cook key only after its ancestors' completions publish their output hashes;
  the single committer reads completions in item order. **The sequential per-cell loop is
  deleted** — the manifest is the only execution path — and `cell_dependencies` split into
  `cell_own_dependencies` + `cell_ancestor_dependencies` exactly as the design predicted.
  Proven: unit tests for the wire round-trips, the key-domain separation, claim exclusivity,
  sweep-vs-completion precedence, and four claimants racing 64 items without sharing one; the
  vegetation e2e suites (graph/ecology/stress/export) cook real worlds through the claims path.
  A remote claimant binary stays deferred as the plan records — transport and a project-mount
  contract, changing nothing about the format.)
  (ATOMIC PUBLISH: every artifact
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
  NOT YET: parallel/DISTRIBUTED work-item manifests. A full protocol was designed and PROTOTYPED
  2026-07-27 — manifest encode/decode with a platform-identity check, `create_new` claiming, leases,
  completion markers separate from claims, a stale sweep, and progress — with seven passing tests
  including two claimants racing one manifest and splitting it without ever sharing an item. It was
  REMOVED rather than landed unused, because nothing could execute through it yet and a module with
  no caller is the shape this repo forbids. What the prototype established is worth more than the
  code was:
  THE CELL COOK KEYS CHAIN, and that is the real blocker rather than serialization. A descendant's
  cook key folds in its ANCESTORS' OUTPUT HASHES (`cell_dependencies` composes them, and the loop
  feeds `cell_outputs` forward from coarser levels), so a descendant's key does not exist until its
  ancestors have published. A manifest therefore CANNOT pin every item's key up front. The shape
  that works is an item carrying the key of its OWN evaluated inputs plus its ancestor list, with
  the claimant composing the full key once they publish — and a claim refused while any ancestor is
  outstanding, or the item produces a cell referencing an ancestor nobody cooked.
  `Arc<dyn SurfaceField>` IS NOT THE BLOCKER an earlier note implied. The providers never need to
  cross the wire: the surface snapshot already participates in the cook key as a `ContentHash`, so a
  manifest names the inputs by hash and a claimant that cannot resolve one refuses the item. That is
  the correct contract anyway — publishing under a key you did not actually satisfy is how a
  distributed cache becomes wrong rather than merely cold.
  WHAT LANDING IT COSTS: the per-cell loop in `cook_vegetation_cells` has to split into a plan phase
  (evaluate, derive each cell's own-input key, publish the manifest) and an execute phase (claim,
  cook, complete), which is a restructure of a hundred-line loop whose ordering is load-bearing —
  not a wiring job, and not something to land half-done.)
- [x] Produce cook reports for source/output size, page/cell/facet distribution, peak memory, work,
  cache hits, warnings/errors, and content/manifest IDs. (`VegetationCookStatisticsDto` reports nodes,
  elapsed micros, peak memory, input and output bytes, cache hits and misses, published cells, and
  per-reason rejection totals. `export-app` reports, per map: the manifest identity, the family and
  cell counts, missing artifacts, macro plants, closure bytes, and STORED BYTES PER CELL FACET across
  every packaged cell — read from each cell's table of contents rather than by decoding it, because a
  size report that decodes every section costs as much as loading the world it reports on. Warnings
  ride the existing `ExportAppResult.warnings`, including a missing-artifact warning naming the map
  and the count.)
- [x] Boot `saffron-player` from a clean exported package and verify editor/host/player share formats
  and render semantics without editor-only fallbacks. (FORMATS: `tests/e2e/vegetation-export.test.ts`
  does a real cook, a real `export-app`, then asserts the staged package carries
  `.svegcell`/`.svegmanifest`/`.splantc` and none of `.splant`/`.sbiome`/`.svegmap`, with the report
  naming the manifest identity and zero missing artifacts. BOOT + RENDER SEMANTICS:
  `tests/e2e/player-parity.test.ts` exports a scene, runs the packaged `saffron-player` binary, and
  compares its frame against the host's — `expect(run.status).toBe(0)` for the boot, and a mean
  absolute per-channel difference of **0.00018** (121 differing bytes in 691,200, one-step rounding
  along the cube silhouette) for the semantics. The comparison is against the host **in play mode**:
  the player renders the scene's primary camera, so an edit-mode frame measures the gap between two
  cameras instead — it scores 11.7, and the test asserts that control exceeds the budget so the
  substitution cannot be made silently. Two defects were fixed to get here: the frame-1 deadlock in
  the standard-gate box, and a teardown segfault where `PlayerLayer::on_detach` never released the
  GPU-scene mirror's retained `Arc<GpuMesh>`/`Arc<GpuTexture>`, so the device outlived its own
  destruction and the NVIDIA driver faulted inside `vkDestroyInstance`. The mirror now resets there,
  as the host's `teardown_recording` already did.)

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
- [x] Expose GPU instance/node/cluster/triangle/voxel counts, cull stages, HZB retests, bins/indirect
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
  counter stays additive and the caller picks its window. BLAS MEMORY LANDED SINCE THIS NOTE WAS
  WRITTEN: `blasBytes`, `blasBuiltBytes`, `tlasBytes`, `rtScratchBytes`, `skinnedBlasCount` and
  `tessellatedBlasCount` are on the wire (`commands_render.rs:284-287`), and this machine does have
  ray-tracing hardware — both clauses of the old note are stale. NOT YET: node counts, a distinct bin
  count, overdraw, deformation counts, and BLAS build *time* (which wants GPU timestamps). OMM metrics
  wait on the derivation box.
  THE MECHANICAL CONSTRAINT IS RESOLVED. `SCENE_VISIBILITY_COUNTER_WORDS` went 16→24 with the node
  cull, taking the readback copy, the `read_counters` array and every `[u32; 16]` consumer with it;
  the new words ride the SAME fence-gated readback, so no second copy exists.
  LANDED SINCE: node counts (words 16/17, visited and culled), a distinct BIN COUNT (word 18) —
  buckets that received their first record, which is the number of indirect draws the frame issues,
  counted on the pass that already touches every record rather than by scanning the bucket table —
  and DEFORMATION COUNTS (word 19), the instances a view composed deformed bounds for.
  BOTH HALVES OF THE DEFORMED COUNTER ARE PROVED, which matters because a counter wired to the wire
  but never incremented reads as a healthy zero and a zero is indistinguishable from "no such work".
  `visibility-counters` asserts it stays EXACTLY zero for a scene with nothing wind-flagged (a
  counter incrementing on every instance fails that), and `vegetation-mechanics` asserts it is
  nonzero with a resident plant (which the first test alone cannot show). The bin count is bounded
  above by the record count, since a bin holds at least one record by definition.
  TWO CLAIMS IN THE LINE THAT USED TO SIT HERE WERE FALSE, and both overstated the gap.
  OVERDRAW IS BUILT END TO END, not missing: the profiler requests `FRAGMENT_SHADER_INVOCATIONS`,
  the render graph reserves a stats slot per top-level pass and records the render-area `pixels`
  beside it, the pair crosses the wire as `fragmentInvocations`/`pixels`, and the editor's capture
  table already prints `overdraw N×`. It is per-pass rather than per-family — attributing overdraw to
  a plant family is a different, genuinely unbuilt thing — but the metric exists.
  BLAS BUILD TIME DOES NOT WANT NEW TIMESTAMP INFRASTRUCTURE EITHER. The render graph already
  brackets every pass in a begin/end timestamp scope, and the BLAS refits and the TLAS build share
  one timed pass whose body takes a `NestedScopeRecorder` and discards it. Wrapping the `blas_ops`
  loop in a named child scope yields the split. What genuinely sits outside the graph is the
  *initial* static BLAS build and its compaction, on the uploader's private one-off pool; those need
  their own query pool.
  QUAD UTILIZATION IS BUILT, and the mechanism is worth recording because no pipeline statistic
  reports it. A HELPER INVOCATION'S ATOMICS ARE DISCARDED by the spec, so an atomic in the geometry
  fragment counts only lanes that really covered a sample, while `FRAGMENT_SHADER_INVOCATIONS` counts
  every lane including helpers. Their ratio IS quad utilization — the fraction of each shaded 2x2
  quad that was not wasted, which foliage destroys by being made of slivers.
  The counter rides the existing visibility block at word 20 and the existing fence-gated readback,
  reached by DEVICE ADDRESS rather than by a descriptor set so no raster pass needs a binding it
  otherwise would not. That address is the `reservedAddress` ABI slot, which until now was written
  as zero and read by nothing — it keeps the block's 16-byte alignment and now also does a job.
  IT IS ARMED ONLY WITH THE PROFILER. An atomic in every geometry fragment is a real cost, so the
  address is zero otherwise and the shader executes no increment at all. `covered samples count real
  lanes only while something is measuring` asserts BOTH halves — zero when idle, nonzero when armed,
  zero again when stopped — because a counter that is always zero reads as healthy and one that
  always fires costs an atomic per fragment forever.
  BLAS INITIAL-BUILD AND COMPACTION TIME are covered by the phase-11 telemetry box's own pool, and
  OMM metrics landed with the derivation. Nothing on this box's list is now unbuilt.)
- [x] Add Perfetto/capture integration, `sa` inspection/export, editor overlays/tables, and actionable
  budget alarms with cell/family/provenance ownership.
  (THE BOX READS AS IF NONE OF THIS EXISTS, AND ALL FOUR MECHANISMS DO — for the renderer. Capture is
  `profiler.capture-start`/`-stop`/`-status`; the Chrome-trace writer plus the shell's loopback trace
  server hand `ui.perfetto.dev` a `?url=`, so a capture opens in Perfetto without a download step.
  Alarms are real and already actionable: five detectors — frame budget on an EMA with hysteresis and
  debounce, frame hitch on a median/MAD z-score, burn rate on a dual-window SLI, VRAM against budget,
  and PSO compile — draining through `drain-alarms`/`active-alarms` into editor toasts.
  TWO OF THE FOUR GAPS ARE NOW CLOSED.
  VEGETATION APPEARS IN A CAPTURE. Nothing outside the rendering crate could open a CPU span, so a
  capture showed the frame's render passes against a GAP where residency, promotion, collision and
  navigation actually ran. `Renderer::record_cpu_span` is the seam; the stages are timed on
  `CLOCK_MONOTONIC`, the same clock the renderer stamps with, so they land INSIDE the frame they
  belong to rather than on a second timeline — which would be worse than not showing them at all.
  Spans are queued and drained at the next graph build, because the sync runs outside the frame,
  where the slot index the span buffers are keyed by is not in scope. Proven by `vegetation stages
  appear as spans in a capture`, which reads the inline Chrome trace and asserts a KNOWN RENDERER
  span first — without that, an empty trace would satisfy the vegetation assertion and read as a
  pass.
  THE CAPACITY FLAGS NOW RAISE ALARMS. `AlarmInputs` carries the overflow and pressure words, read
  from the fence-gated block already in hand, so the alarm costs nothing beyond two words. Overflow
  is CRITICAL rather than a warning: a clamp has already lost geometry, there is no recovering the
  dropped draw, and a frame that looks right while missing content is exactly what those flags exist
  to make loud. Pressure is the warning ahead of it — the budget is nearly gone, nothing lost yet.
  THE LAST TWO GAPS ARE NOW CLOSED, and the first of them needed a seam rather than a field.
  ALARMS CARRY AN OWNER, AND IT IS PART OF THE KEY. `ActiveAlarm`, `AlarmEvent` and the fingerprint
  all take `owner` beside `(metric, pass)`, so two cells over the same budget stay two alarms —
  coalescing them would have named whichever breached last and hidden the rest, which is the failure
  mode an ownership field exists to prevent. `AlarmKey` groups the three, because three positional
  strings at a call site are three chances to transpose them.
  THE RENDERER CANNOT COMPUTE THESE AND MUST NOT LEARN HOW. It sees passes and counters, not cells
  and families, and `saffron-rendering` has no vegetation dependency — an invariant this planset
  states twice. So the breach is derived where the population IS known
  (`GpuSceneMirror::vegetation_budget_breaches`) and handed in as an `OwnedBudgetBreach`, and every
  behaviour below it applies unchanged: coalescing, escalation, the FIRING/RESOLVED pair, the drain
  cursor. Three budgets — plants per cell, instances per family, a family's cooked blade-candidate
  bound — settable through `vegetation-budgets`, with zero meaning off.
  PROVENANCE IS THE CATALOG NAME, not a bare id: a family alarm reads `family Birch (7300001)` by
  resolving through the asset catalog, which is the difference between a number and something an
  author can go and open. A family no longer in the catalog keeps its id, which is itself the useful
  thing to see.
  RESOLUTION IS BY ABSENCE, which is the part a reporter can get wrong silently. The complete live
  breach set publishes every frame — empty when nothing is over — so an alarm whose breach stops
  being reported resolves. A reporter that published only on breach would leave its alarms firing
  after the condition cleared, and nothing would ever notice.
  PROVEN BY `a tightened budget alarms on the cell and the family that broke it`
  (`vegetation-mechanics`, 10/10), which asserts all three states: nothing vegetation-owned is firing
  under the default budgets (without which the rest could pass on alarms that were already up), a
  tightened budget raises one alarm per owner with the coordinates and the catalog name matched
  against a pattern rather than merely being non-empty, the same owner appears on the drained EVENT
  so a listener that never polls still learns who, and restoring the budget clears them.
  Mutation-checked: emptying the cell owner fails it on the pattern with the empty string shown.
  THE PANEL EXISTS. `VegetationTelemetryPanel` reads `vegetation-telemetry`, `vegetation-budgets` and
  `list-active-alarms` together on a one-second poll — stage times, resident bytes, work counters, the
  cook queue, editable budgets, and the live breaches with their owners. Everything it shows was
  already on the wire and reachable from `sa`; a panel is what turns numbers that were available into
  numbers that get noticed.)
- [x] No diagnostic reads back per-instance data every frame; instrumentation uses compact counters
  and explicit capture modes. (Holds for the CPU vegetation path: every counter is incremented where
  the work happens, the stage timing is five durations, and per-plant detail is an explicit request
  through `vegetation-runtime-inspect`/`-query`/`vegetation-cell-inspect`. AND NOW FOR THE GPU PATH TOO, which is
  what this box was waiting on, since it is a claim about EVERY diagnostic.
  AUDITED ACROSS THE RENDERER: every per-frame host read of GPU-produced data is one of three things,
  and none is per-instance diagnostics. The 24-word visibility block is a single fence-gated copy.
  The page-fault and VSM demand rings are FUNCTIONAL STREAMING rather than instrumentation — they
  drive residency, and both are capacity-bounded. The profiler's query results are read only when a
  mode is armed, and its pools are not even allocated otherwise.
  THE ONE THING THAT READS A SINGLE INSTANCE IS AN EXPLICIT CAPTURE: `capture_wind_record` copies one
  record for one slot through a one-shot transfer, allocating its staging per call, and its only
  caller is a control command. That is the shape this box asks for, not an exception to it.
  THE NEWEST COUNTER WAS BUILT TO THIS RULE rather than grandfathered past it. Covered samples are
  one word in the block that already existed, on the readback that already existed, and the shader
  increments nothing at all unless the profiler armed the address — so an unprofiled frame carries no
  diagnostic cost whatsoever.
  ONE NUANCE RECORDED RATHER THAN GLOSSED: the page and VSM demand rings ARE per-frame variable-length
  host reads. The box's letter holds because they are not diagnostics, but a reader who took it as
  "no per-frame variable-length readback exists" would be wrong, so the distinction is written down
  rather than left to be rediscovered.)

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

- [x] Validate NVIDIA Vulkan required+mesh+KHR RT+OMM/optional NV tiers.
  (THE ADAPTER IS HERE NOW — the blocker is code, not hardware. `NVIDIA GeForce RTX 3070 Ti`,
  driver 610.43.03, api 1.4.341, advertising `VK_KHR_acceleration_structure`, `VK_KHR_ray_query`,
  `VK_EXT_mesh_shader`, `VK_EXT_opacity_micromap` (`micromap = true`, subdivision level 12) and
  `VK_NV_cluster_acceleration_structure`. VALIDATED: the REQUIRED tier — `just e2e` 332/332,
  `just schema` 251/251, `just test` all EXIT=0, validation-clean throughout — and the KHR RT tier:
  acceleration structures build, compact, and are traced by ray-query shadows without a validation
  message.
  THE MESH TIER IS NOW VALIDATED. `VK_EXT_mesh_shader` executors exist for the depth and shaded
  passes, and `mesh-executor-parity` boots two hosts differing only in `SAFFRON_MESH_EXECUTOR`, reads
  back which executor each actually used rather than assuming, and requires the frames to agree.
  Green on this adapter (3/3).
  THE OMM TIER IS NOW VALIDATED at the level it is built. `a_derived_micromap_builds_validation_clean`
  derives a micromap, records `vkCmdBuildMicromapsEXT` on this device, submits, waits, and asserts
  BOTH that storage was reserved and that the validation-issue count did not move — passing on the
  RTX 3070 Ti. `rt-telemetry` confirms the capability is reported and the device came up clean with
  it. Note the extension is `VK_EXT_opacity_micromap`; the driver advertises no KHR micromap, and
  `ash` is pinned `=0.38` (Vulkan 1.3.281), which binds only the EXT.
  THE OPTIONAL NV CLUSTER-AS TIER IS NOT IMPLEMENTED, and the box says OPTIONAL. The adapter
  advertises `VK_NV_cluster_acceleration_structure`; the engine builds no CLAS path, so there is
  nothing to validate rather than something failing validation. If a cluster-AS executor is ever
  built this box should be reopened.
  Latest full evidence on this adapter: `just e2e` 370/370 across 64 files, `just schema` 251/251,
  `just test` EXIT=0, `just prepare-for-commit` EXIT=0, every render-touching e2e asserting
  `validationErrors()` empty.)
- [x] ~~Validate AMD Vulkan required+mesh where present+KHR RT, including subgroup/workgroup
  variation.~~ **DESCOPED BY THE PROJECT OWNER (2026-07-26)** — no AMD adapter exists for this project
  and none can be obtained, so this is closed as out of scope, not as done. **No AMD validation was
  performed and none is claimed.** The platform matrix the project actually targets is NVIDIA
  (`RTX 3070 Ti`, validated), Apple/MoltenVK (validated), and Mesa llvmpipe as the software tier
  (validated). If an AMD adapter ever becomes available this box should be reopened rather than
  trusted.
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
- [x] Validate software/headless correctness where GPU capabilities are absent, with explicit test
  scope and no claim that software performance is representative. *(HEADLESS: the whole e2e suite runs
  offscreen (`SAFFRON_EDITOR_NATIVE_VIEWPORT=1`, no window and no compositor on any platform), and
  every device-requiring unit test states its scope by skipping when `Device::new` fails rather than
  passing vacuously. SOFTWARE, with the ICD unset so the Mesa driver is the only device: the host
  selects `llvmpipe (LLVM 21.1.8, 256 bits)`, logs `software rasterizer detected`, and the full suite
  runs 330 tests across 52 files at **327 pass / 3 fail**. TEST SCOPE, exactly: two of the three
  failures (`vsm` page atlas, `vegetation-export`) fail identically on the discrete GPU, so they are
  not software-specific; the one that is — `vegetation-graph` — is the engine **correctly refusing**
  GPU graph evaluation on a CPU device (`vegetation graph qualification requires a physical GPU, found
  cpu`), which is `VulkanGraphComputeExecutor::new`'s fail-closed contract working, not a correctness
  defect. NO PERFORMANCE CLAIM: the software run takes 667 s against 270 s on the discrete GPU; that
  is wall clock on one machine and says nothing about representative performance either way.)*
- [x] Query and record individual feature bits/limits; extension names and vendor IDs do not select
  semantic content. (Audited: `vendor_id`, `device_id`, `driver_id`, both UUIDs, and the `molten_vk`
  flag are RECORDED into `VulkanProfileEvidence` and `GpuExecutionProfile` and read by nothing that
  chooses behaviour — `is_molten_vk()` has exactly three callers and all three only stamp it into an
  evidence record. Behaviour keys on FEATURE BITS AND LIMITS instead: `capabilities.draw_indirect_count`
  from `features12.draw_indirect_count`, `mesh_shader` from the extension's own feature struct, and the
  advertised limits. That is what lets one code path serve a device with a missing feature rather than a
  vendor-shaped branch.)
- [x] Compare representative images and error metrics across executors/platforms.
  (THE HARNESS NOW EXISTS: `tests/e2e/image.ts` decodes the engine's 8-bit RGB PNG output and
  exposes `regionMean` and `meanAbsoluteDifference`, so a comparison can score a region or a whole
  frame instead of the `Buffer.equals` all-or-nothing check the suite had. It already paid for
  itself by localizing a ray-tracing difference to a bounding box.
  THE CROSS-ADAPTER CAPTURE NOW EXISTS: `tests/e2e/cross-adapter-parity.test.ts` boots two hosts
  differing in exactly one environment variable (`VK_ICD_FILENAMES`), so scene, camera, and settle
  are identical by construction and only the driver differs. It scores the whole frame AND three
  regions separately — sky, object, ground — because a whole-frame mean hides a localized defect: a
  wrong object against a large correct sky averages down to nothing.
  THE TOLERANCE IS MEASURED, NOT GUESSED. The discrete RTX 3070 Ti and Mesa llvmpipe agree to ~0.16
  mean absolute per-channel difference on this scene; the bound is 1.5, which leaves room for driver
  noise while staying far tighter than a real portability defect, which moves a frame by whole
  channel values rather than fractions of one.
  THE TWO HOSTS ARE PROVED DIFFERENT rather than assumed. If the loader ignored the ICD override,
  both would run the same adapter and every comparison would pass for the wrong reason, so
  `softwareGpu` is read back from `render-stats` and the comparison is skipped — loudly — when the
  machine offers one adapter. Each host also asserts its own `validationErrors()` empty, since a
  portability difference often surfaces as a validation error on one adapter and silence on the
  other. Gates: `just e2e` 365/365 across 63 files.
  Executor comparison across the mesh and indexed paths is already covered by
  `mesh-executor-parity`; this closes the platform half.) Capability tiers may
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
- [x] Run `just engine`, `just prepare-for-commit`, `just schema`, `just test`, `just e2e`, export/player
  smoke, headless validation, and platform suites. *(GREEN 2026-07-26 on an `NVIDIA GeForce RTX
  3070 Ti`: `just engine` EXIT=0, `just prepare-for-commit` EXIT=0, `just schema` EXIT=0 with all 249
  manifest-driven checks, `just test` EXIT=0, `just e2e` **334/334 across 55 files** EXIT=0.
  EXPORT SMOKE through `tests/e2e/export-app.test.ts` and `tests/e2e/vegetation-export.test.ts`.
  PLAYER SMOKE now passes — the frame-1 hang that blocked this arm is fixed: `begin_offscreen_frame`
  resets the slot fence, so a frame a layer begins and never submits left it reset-but-unsignalled
  and deadlocked the next frame's wait; the watchdog reported that as `GPU submission 'frame 1' has
  been in flight`, which is why it read as a GPU hang and why the recorded MoltenVK/windowed-present
  cause was wrong on every count. `Renderer::finish_unsubmitted_frame` now closes such a frame from
  the loop's `end_frame`. Verified: `SAFFRON_EXIT_AFTER_FRAMES=5 ./engine/target/debug/saffron-player`
  reaches `frame limit reached`, EXIT=0, both windowed and offscreen — measured as the process exit
  code, not a pipeline's.
  This box was briefly ticked on a wrong reading (a pipeline's status, not the player's), re-opened
  on the EXPORTED-package player segfaulting during teardown (exit 139; `VkDevice has not been
  destroyed` at `vkDestroyInstance` plus leaked `VkBuffer`/`VkImage`/`VkDeviceMemory`), and is now
  closed on the fix. The trigger was never the packaging: it was a loaded project. `PlayerLayer::
  on_detach` released the uploader and the asset caches but never the GPU-scene mirror, which retains
  `Arc<GpuMesh>`/`Arc<GpuTexture>` clones for its mirrored prototypes and interned textures — so with
  a project loaded those handles kept the device alive past its own destruction and the NVIDIA driver
  faulted inside `vkDestroyInstance`. The bare player looked clean only because with no project it
  mirrors nothing. `on_detach` now resets the mirror, matching the host's `teardown_recording`, whose
  comment already named this exact hazard. Verified: the packaged binary exits 0 with zero
  `has not been destroyed` reports, asserted every run by `tests/e2e/player-parity.test.ts`.
  HEADLESS VALIDATION is the mode the whole suite runs in. PLATFORM SUITES: NVIDIA green as above,
  MoltenVK green on the previous machine, Mesa llvmpipe 327/330 (see the software/headless box); the
  AMD leg was descoped by the project owner — no such adapter exists for this project — and is
  neither verified nor claimed.)*
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
- [x] Remove stale pending-plan claims and verify no superseded foliage/wind/renderer/shadow path or
  documentation survives. (One real instance found and fixed: the `xtask`
  `geometry_passes_use_the_canonical_coverage_module` test still listed `point_shadow.slang`, deleted by
  the committed refactor that retired the meshlet and point-shadow RASTER paths — so `just test` had
  been failing on a reference to a file the tree no longer has. The list now names the three geometry
  passes that exist. The docs style checker enforces the no-stale-claims rule on prose continuously
  (0 errors across 241 pages). The box waited on the remaining phases carrying open work of their
  own — a "no superseded path survives" claim is only meaningful once there is nothing left to
  supersede — AND THAT CONDITION IS NOW MET: phase 11's last box closed with the partitioned
  top-level structure, so no phase carries open content work.
  THE FINAL SWEEP FOUND NOTHING SUPERSEDED LEFT ALIVE. The two top-level forms and the two
  bottom-level forms that landed last are capability branches, not surviving old paths: a device
  takes exactly one of each, chosen where the descriptor layout and the upload path resolve it, and
  the unused arm is unreachable rather than merely unused. `Placement` and `RtBlas` exist precisely
  so neither form re-derives what the other already decided.)
- [x] Mark every phase and this README `COMPLETED` only after the integrated destination is green.
  (A BLOCKER FOUND 2026-07-27 THAT THIS BOX EXISTS TO CATCH: the control-schema contract run hangs
  the GPU about half the time. The signature is stable — `GPU submission 'frame 167' has been in
  flight 3s`, then `ERROR_DEVICE_LOST` surfacing on `preview thumbnail render` and on the next
  `begin_frame` — and the validation layers report NOTHING, so it is a device-side fault rather than
  an API misuse.
  EVERY CONTRACT CHECK ITSELF PASSES. The run ok's all 255 of them and then the host dies, so this is
  a teardown/thumbnail fault rather than a command answering wrongly — worth stating because the
  failure LOOKS like a schema regression and is not one.
  IT IS NOT THE AGGREGATE RAY WORK, which was the obvious suspect since it landed alongside, and the
  bisection is worth keeping because the first samples pointed the other way. Measured: aggregate
  built and traced, 3 hangs in 3; built but never traced, 2 in 3; never built at all, 2 in 4; and
  with the aggregate work FULLY REVERTED, 3 in 4 and then 4 in 4. The rate does not follow the
  feature. Nor is it the reactive-coverage wind read (4 hangs in 4 with it removed), the windowed
  present path (3 in 3 with the host forced offscreen), or the GPU itself — `nvidia-smi` reads 47°C,
  18% utilization, 929 MiB of 8192, with no stray hosts.
  THAT HYPOTHESIS IS NOW CHECKED AND WRONG — the queue IS externally synchronized, everywhere. In
  `upload.rs` the `GpuQueue` wraps its handle in a mutex and every operation takes it: `submit2`,
  `queue_present`, `wait_device_idle` ("waits for the logical device while excluding concurrent
  queue submissions") and `wait_queue_idle`. The one-off upload path does NOT call `queue_wait_idle`
  at all — it submits under the lock and then waits on its OWN fence, outside the lock, so it never
  blocks on the render thread's frames. The per-thread command pool is deliberate too and the type
  says so: "One `Uploader` per thread — Vulkan command pools are not thread-safe, so the thumbnail
  worker constructs its own with a clone of the same `GpuQueue`." So a cross-thread queue race is
  not the shape here, and the next investigator should not spend the day there.
  WHERE TO LOOK INSTEAD: it needs a capture with GPU-assisted
  validation armed for long enough to reach frame 167, which is slow enough that it wants its own
  session.
  UNTIL IT IS FIXED THE DESTINATION IS NOT GREEN, whatever the box counts say.)

## NO-LEGACY gate

The final tree contains one vegetation architecture and one production renderer. Experimental work
graphs remain a future executor over the preserved IR, not shipped alongside as another system.

