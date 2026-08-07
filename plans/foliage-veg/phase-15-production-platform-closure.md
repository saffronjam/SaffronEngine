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
  textures/materials, collision/nav contributions, and license attribution. (The cooker builds
  the dependency DAG (`CookGraph`, `cook_graph_hash`, `CookDependency` on every manifest cell), the
  platform-profiled `.splantc`, the sectioned `.svegcell` including its `CollisionInputs` and
  `NavigationContributions` facets, and the manifest; `export-app` copies the shaders and the
  textures/materials. THE INITIAL PERSISTENT-STATE BASELINE is a `Baseline` artifact kind
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
  copies exactly that into the package's store. THE STORE IS NOT UNDER `assets/` — it sits beside it —
  so a package built by copying assets alone binds no manifest and comes up bare. The closure is
  computed from the manifest rather than a directory scan, so a superseded artifact stays out. The
  authored `.splant`/`.sbiome`/`.svegmap` files and their sidecar packages are
  excluded from the packaged `assets/` by `is_authored_vegetation`; that is safe because the project
  loader treats the filesystem as the source of truth and drops a catalog row whose file is absent,
  and because vegetation binds by identity through the artifact store rather than through the catalog.)
- [x] Add parallel/distributed-safe work-item manifests, cancellation/resume, cache sharing, atomic
  publish, corruption repair, and deterministic final package ordering.
  (WORK-ITEM MANIFESTS: `vegetation/src/cook_work.rs`
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
  the single committer reads completions in item order. The manifest is the only execution path, and
  `cell_dependencies` is split into `cell_own_dependencies` + `cell_ancestor_dependencies`.
  THE PLAN/EXECUTE SPLIT IS FORCED BY THE KEY CHAIN: a descendant's cook key folds in its ANCESTORS'
  OUTPUT HASHES, so that key does not exist until the ancestors publish and a manifest cannot pin
  every item's key up front. An item therefore carries the key of its OWN evaluated inputs plus its
  ancestor list, the claimant composes the full key once the ancestors complete, and a claim is
  refused while any ancestor is outstanding — otherwise an item produces a cell referencing an
  ancestor nobody cooked. `Arc<dyn SurfaceField>` never crosses the wire either: the surface snapshot
  already participates in the cook key as a `ContentHash`, so a manifest names its inputs by hash and
  a claimant that cannot resolve one refuses the item rather than publishing under a key it did not
  satisfy.
  Proven: unit tests for the wire round-trips, the key-domain separation, claim exclusivity,
  sweep-vs-completion precedence, and four claimants racing 64 items without sharing one; the
  vegetation e2e suites (graph/ecology/stress/export) cook real worlds through the claims path.
  A remote claimant binary is out of scope here: it needs transport and a project-mount contract, and
  changes nothing about the format.
  ATOMIC PUBLISH: every artifact
  and every generation root writes through `AtomicWriteFile` and is re-read and rehashed before the
  publication is reported. CANCELLATION: `GraphCancellationToken` plus the cook queue's
  cancel/supersede transitions, each counted. RESUME: content addressing gives it by construction — a
  re-run of an interrupted cook hits the cache for every node that published and re-does only the rest,
  which the cook statistics report as hits against misses. CACHE SHARING: the store is
  content-addressed and lock-guarded, so two cooks over one project share every hit.
  CORRUPTION REPAIR: `verify_vegetation_artifacts` rehashes every artifact the current generations name
  — the file's NAME is the hash of its bytes, so verification needs no side table that could itself rot
  — and `vegetation-verify-artifacts {repair}` removes the corrupt ones so the next cook republishes
  them. Repair deletes rather than rewrites: the bytes are the only copy, and the cooker's cache-miss
  path is already the thing that produces them. DETERMINISTIC PACKAGE ORDERING: the export closure is a
  `BTreeSet` walked in canonical path order, so the same generation packages the same file sequence.)
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

- [x] Cell-interest keys and facets. (`CellInterestKey` is the exact cell/facet pair in
  `network/interest.rs` — a 26-byte canonical encoding plus a `routing_word` that is documented as a
  routing hint and never an identity, because a truncated name that collided would ship one peer
  another peer's cell. `CellInterestSet` is the per-cell facet mask in canonical cell order, with an
  empty mask refused so a withdrawal encodes exactly one way. Proven by
  `interest_declaration_round_trips_and_rejects_an_empty_mask`,
  `a_declaration_out_of_canonical_cell_order_is_rejected`, and
  `interest_keys_enumerate_in_canonical_cell_then_facet_order`.)
- [x] Exact base-manifest handshake and rejection. (`BaseManifestOffer` carries the protocol version,
  the `VegetationStateBinding` — generation, cook graph, `CookVersionSet`, seed namespaces — and the
  25-column point-schema identity; `PeerBaseIdentity::accept` matches all six exactly and returns the
  first dimension that differs as a typed `ManifestHandshakeRejection`. There is no negotiation and
  no partial acceptance: two peers reducing the same operation under different contracts diverge
  silently instead of failing. Proven by `a_handshake_names_the_first_dimension_that_differs`, which
  perturbs each dimension in turn.)
- [x] Canonical cell state snapshots plus sequenced/idempotent mutation tails.
  (`VegetationState::scope_to_interest` projects the authoritative state onto one declaration —
  a cell's delta crosses under any facet, an ecology summary only under `Simulation`, and
  applied-transaction signatures never cross because a scoped peer recomputing one would reject a
  legitimate replay. `NetworkMutationEnvelope` carries a monotonic sequence, the manifest identity,
  the sequenced operations, and an optional authoritative snapshot;
  `VegetationWorld::receive_network_envelope` drops a retransmission at the sequence gate, refuses an
  operation outside the seated declaration, and rebases from a carried snapshot before its operations
  reduce. Proven by `scoping_drops_an_undeclared_cell_and_a_summary_no_facet_asked_for`,
  `a_network_envelope_advances_once_per_sequence_and_corrects_from_a_snapshot`, and
  `snapshot_compaction_and_duplicate_tail_replay_are_equivalent`.)
- [x] Transaction/precondition/authority/tick fields and duplicate/reorder-safe reduction.
  (`MutationHeader` carries `cell`, `transaction`, `authority`, `logical_tick`, `idempotency_key`,
  and the optional `base_revision` precondition. `shuffled_transactions_and_exact_replays_reduce_identically`
  is the reorder and duplicate proof: two reductions of the same records in different orders, with
  exact replays interleaved, produce identical `canonical_bytes()`.)
- [x] Promoted-entity `PlantOrigin` handoff and snapshot during promotion. (`PlantOrigin` is the
  immutable scene component a promoted entity carries — `remove_component` panics with "PlantOrigin
  is immutable; demote the plant instead" — and the felled product deliberately carries none.
  `VegetationMutation::PromotionOriginState` is the return leg: pose plus linear and angular velocity
  in Q15.16, reduced into the cell delta's `promotion_origin` and lifted back into the published
  generation by `CellPersistentOverlay::from_state`. Ownership is the network-facing half:
  `plant_simulation_authority` and `authority_epoch` record who last claimed the plant, neither is
  persisted, and both are cleared by `replace_persistent_state` — which is how a save load, a
  snapshot import, and a network join are all noticed by one path. Proven by
  `momentum_round_trips_between_the_live_body_and_the_reducer`,
  `a_re_promoted_plant_continues_the_motion_its_view_ended_with`,
  `a_foreign_claim_releases_the_view_and_the_authority_s_own_claim_keeps_it`, and
  `promotion_origin_velocity_survives_into_the_next_snapshot`.)
- [x] Periodic checkpoint hashes. (`VegetationCheckpoint` fingerprints the scoped state at a
  sequence — sequence, ecology tick, manifest identity, interest identity, state identity — and
  `reconcile` returns a typed `CheckpointReconciliation`: `InSync`, `Diverged`, `Behind`, `Ahead`,
  `InterestMismatch`, or `ManifestMismatch`, with `requires_snapshot` saying which of those
  operations cannot repair. Both sides fingerprint the *same* scope, so a peer holding a subset
  reconciles against the authority's projection of that subset rather than a world-wide digest it
  could never reproduce. Proven by
  `a_checkpoint_over_the_same_scope_agrees_and_over_a_different_one_does_not`.)
- [ ] Deterministic late-join state fixtures. The seating path is built —
  `LateJoinRequest`/`LateJoinGrant` in `network/session.rs`, `issue_late_join`/`accept_late_join` in
  `runtime_world/network.rs`, with the snapshot travelling *inside* an envelope so a joiner and a
  peer being corrected take the identical receive path, and the joiner proving it landed where the
  authority said by reproducing the granted checkpoint. `a_late_joiner_reproduces_the_authority_checkpoint_for_its_declared_scope`
  covers it. What does not exist is a fixture corpus: the only late-join world is that test's
  in-code one, and no `tests/e2e` file drives a join.
- [ ] Local reconstruction of wind/micro bend while only persistent macro/disturbance/ecology state
  is transmitted. It holds structurally — `VegetationState` is a manifest identity, cell deltas,
  applied-transaction signatures and ecology state, with no wind, bend or micro field anywhere in it,
  and `saffron-vegetation` does not depend on `saffron-wind` at all, so no wind value can reach a
  transmitted byte — but nothing asserts the other half: no test reconstructs bend
  on a receiver from the immutable base plus a received state and compares it against the authority's.

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
  GI/RT/BLAS/OMM metrics, and every pressure/overflow flag. (All of it is on the wire through
  `render-stats` (`control/src/commands_render/stats.rs` over `RenderStatsDto`): instances, triangles,
  semantic records, aggregate-voxel records, max cut depth, frustum and occlusion cull counts, HZB
  retests, transparent draws, micro candidates, sub-quad triangles, draw calls and batches, RT
  instances, VRAM usage against budget, per-pass timings, a pipeline-stats profiler mode, the VSM
  page/cache/dirty/evict/overflow set, page residency
  registered/resident/bytes/budget/requested/loading/ready/evictions, and BOTH the overflow and
  pressure flag words — a silent capacity clamp is how geometry disappears, so those flags are the
  point.
  PAGE FAULTS AND LATENCY are priced from demand to the moment the payload can be drawn rather than
  to when the bytes arrived, kept as `faults` plus a summed `faultLatencyUs` so the counter stays
  additive and the caller picks its window.
  NODE, BIN, DEFORMATION AND COVERED-SAMPLE COUNTS ride the visibility block, whose
  `SCENE_VISIBILITY_COUNTER_WORDS` is 24 on the one fence-gated readback — no second copy exists.
  Words 16/17 are culled and visited nodes; word 18 is the bin count — buckets that received their
  first record, which is the number of indirect draws the frame issues, counted on the pass that
  already touches every record rather than by scanning the bucket table, and bounded above by the
  record count since a bin holds at least one record; word 19 is deformed instances; word 20 is
  covered samples.
  BOTH HALVES OF THE DEFORMED COUNTER ARE PROVED, which matters because a counter wired to the wire
  but never incremented reads as a healthy zero and a zero is indistinguishable from "no such work".
  `visibility-counters` asserts it stays EXACTLY zero for a scene with nothing wind-flagged (a
  counter incrementing on every instance fails that), and `vegetation-mechanics` asserts it is
  nonzero with a resident plant (which the first test alone cannot show).
  OVERDRAW is per-pass: the profiler requests `FRAGMENT_SHADER_INVOCATIONS`, the render graph reserves
  a stats slot per top-level pass and records the render-area `pixels` beside it, the pair crosses the
  wire as `fragmentInvocations`/`pixels`, and the editor's capture table prints `overdraw N×`.
  Attributing overdraw to a plant family is a different thing and is not built.
  QUAD UTILIZATION needs the covered-sample counter because no pipeline statistic reports it. A HELPER
  INVOCATION'S ATOMICS ARE DISCARDED by the spec, so an atomic in the geometry fragment counts only
  lanes that really covered a sample while `FRAGMENT_SHADER_INVOCATIONS` counts every lane including
  helpers; their ratio is the fraction of each shaded 2x2 quad that was not wasted, which foliage
  destroys by being made of slivers. The counter is reached by DEVICE ADDRESS rather than by a
  descriptor set, so no raster pass needs a binding it otherwise would not: the address block's
  `quadCounters` word, which `gpuSceneCountCoveredSample` reads and which keeps the block's 16-byte
  alignment. IT IS ARMED ONLY WITH THE PROFILER: an atomic in
  every geometry fragment is a real cost, so the address is zero otherwise and the shader executes no
  increment at all. `covered samples count real lanes only while something is measuring` asserts BOTH
  halves — zero when idle, nonzero when armed, zero again when stopped.
  RT MEMORY AND TIME: `blasBytes`, `blasBuiltBytes` (the gap to `blasBytes` is what compaction saved),
  `tlasBytes`, `rtScratchBytes`, `blasCount`, `skinnedBlasCount`, `tessellatedBlasCount`, and
  `accelBuildUs` — session-cumulative GPU microseconds in the structure builds outside the render
  graph, the static build and its compaction on the uploader's private pool, while `blas-refit` is a
  named nested scope inside the graph's per-pass timestamps.
  CLUSTER COUNTS are `clusterAsSupported`, `clusterBlasCount` and `clasCount` over the
  `VK_NV_cluster_acceleration_structure` path. OMM METRICS are `ommSupported`, `ommMicromaps`,
  `ommOpaque`, `ommTransparent` and `ommUnknown`, the last three counting micro-triangles a derivation
  proved covered, cut out, or left for the coverage classifier.)
- [x] Add Perfetto/capture integration, `sa` inspection/export, editor overlays/tables, and actionable
  budget alarms with cell/family/provenance ownership.
  (Capture is
  `profiler.capture-start`/`-stop`/`-status`; the Chrome-trace writer plus the shell's loopback trace
  server hand `ui.perfetto.dev` a `?url=`, so a capture opens in Perfetto without a download step.
  Alarms are actionable: five detectors — frame budget on an EMA with hysteresis and
  debounce, frame hitch on a median/MAD z-score, burn rate on a dual-window SLI, VRAM against budget,
  and PSO compile — draining through `drain-alarms`/`list-active-alarms` into editor toasts.
  VEGETATION APPEARS IN A CAPTURE. Nothing outside the rendering crate can open a CPU span, so a
  capture would otherwise show the frame's render passes against a GAP where residency, promotion,
  collision and navigation actually ran. `Renderer::record_cpu_span` is the seam; the stages are timed on
  `CLOCK_MONOTONIC`, the same clock the renderer stamps with, so they land INSIDE the frame they
  belong to rather than on a second timeline — which would be worse than not showing them at all.
  Spans are queued and drained at the next graph build, because the sync runs outside the frame,
  where the slot index the span buffers are keyed by is not in scope. Proven by `vegetation stages
  appear as spans in a capture`, which reads the inline Chrome trace and asserts a KNOWN RENDERER
  span first — without that, an empty trace would satisfy the vegetation assertion and read as a
  pass.
  THE CAPACITY FLAGS RAISE ALARMS. `AlarmInputs` carries the overflow and pressure words, read
  from the fence-gated block already in hand, so the alarm costs nothing beyond two words. Overflow
  is CRITICAL rather than a warning: a clamp has already lost geometry, there is no recovering the
  dropped draw, and a frame that looks right while missing content is exactly what those flags exist
  to make loud. Pressure is the warning ahead of it — the budget is nearly gone, nothing lost yet.
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
  (`vegetation-mechanics`), which asserts all three states: nothing vegetation-owned is firing
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
  through `vegetation-runtime-inspect`/`-query`/`vegetation-cell-inspect`. AND FOR THE GPU PATH, which
  the box's word EVERY reaches too.
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

- [x] Different worker counts, job/input/cell/source order, origin rebasing, negative coordinates,
  and unrelated graph edits. (`worker_counts_and_cell_request_order_are_byte_identical` and
  `catch_up_is_identical_across_worker_counts` for the worker dimension;
  `graph_bytes_ignore_input_and_dependency_order`, `source_order_cannot_change_resolved_bytes`,
  `manifest_identity_is_schedule_and_input_order_independent` and
  `shuffled_snapshots_reach_one_admission_order` for order; `origin_rebasing_preserves_identity`,
  `render_origin_rebasing_does_not_change_plant_identity` and
  `surface_hit_is_identical_after_render_origin_rebase` for rebasing;
  `negative_parent_uses_floor_division` and
  `covering_cells_are_exact_for_negative_half_open_bounds_and_limits` for negative coordinates; and
  `unrelated_node_revision_does_not_change_retained_node_identity`, which is the one that matters
  most — a content hash may invalidate cache but must never reshuffle a node it did not touch.)
- [ ] Cell faces/corners, large halos, hierarchy levels, cross-cell transactions and competition.
  Faces and corners are covered (`cell_faces_have_one_half_open_owner`,
  `halo_faces_and_corners_publish_every_owned_point_exactly_once`), as are cross-cell transactions
  (`cross_cell_transaction_is_atomic`) and competition (`root_competition_takes_its_share_of_the_water`,
  `propagating_operators_require_global_policy_but_competition_is_partitionable`). Two legs have no
  named determinism test: a halo spanning several cells at once, and evaluation at more than one
  hierarchy level — `every_level_boundary_key_round_trips_and_orders_big_endian` pins the key
  encoding at every level, which is not the same as pinning published bytes across levels.
- [ ] Cancellation/supersession, atomic publication, corrupt/truncated/unknown artifacts, disk-full
  and interrupted writes. Everything here is covered except disk-full: cancellation and supersession
  by `cancellation_and_deadline_abort_deterministically_at_every_publication_phase`,
  `canceled_generation_never_publishes`, `cancelled_staged_cook_never_advances_the_generation_root`,
  `newer_same_map_job_supersedes_the_running_job` and
  `poll_ready_finalizes_a_superseded_worker_as_superseded`; atomic publication by
  `publication_is_content_addressed_atomic_and_idempotent` and
  `late_generation_is_rejected_under_concurrent_publication`; corrupt, truncated and unknown bytes by
  `corrupt_and_truncated_manifest_are_typed_failures`,
  `zstd_checksum_corruption_and_truncation_are_typed_failures`,
  `toc_rejects_unknown_duplicate_and_unsupported_entries` and
  `corrupt_artifact_never_reaches_publication`; and an interrupted write by
  `interrupted_frames_are_rejected_at_every_boundary`, which truncates a state container at every
  offset and requires each prefix to be refused. A full disk is not simulated anywhere.
- [ ] Cache deletion/recook under authored overrides and runtime tombstones. The persistence half is
  covered — `load_query_unload_and_reload_preserve_persistent_tombstones` and
  `persistent_state_rebases_onto_a_new_base_and_still_removes_what_it_removed`, which is the recook
  rebase — and `a_flipped_byte_is_found_and_repaired` covers repair of a corrupt artifact. What no
  test drives is the whole loop: delete `<project>/cache/vegetation/`, recook, and require the
  authored overrides and runtime tombstones to land on the republished base unchanged.
- [x] Snapshot/compaction, duplicate/reordered mutation envelopes, manifest mismatch, and late join.
  (`snapshot_tail_compaction_matches_full_reduction` and
  `complete_reduced_snapshot_round_trips_every_delta_family` for the containers;
  `shuffled_transactions_and_exact_replays_reduce_identically` and
  `snapshot_compaction_and_duplicate_tail_replay_are_equivalent` for duplicates and reordering;
  `state_rejects_a_different_manifest` and `every_header_binding_mismatch_is_rejected` for the
  mismatch, plus the envelope's own `ManifestMismatch` arm in
  `a_network_envelope_advances_once_per_sequence_and_corrects_from_a_snapshot`; and
  `a_late_joiner_reproduces_the_authority_checkpoint_for_its_declared_scope` for late join.)
- [ ] Promotion ownership/save/unload/recook races, Jolt batch churn, contacts, nav dirtying, and
  products. Ownership is covered by
  `a_foreign_claim_releases_the_view_and_the_authority_s_own_claim_keeps_it` (a recook restore, an
  editor undo, or a network snapshot takes the view down without a write-back) and
  `demotion_of_a_live_view_is_cancelled_by_a_new_promotion`; products by
  `felling_requests_queue_once_and_products_carry_no_plant_identity`; nav dirtying by
  `one_promotion_dirties_only_that_plant` and `dirty_regions_coalesce_and_drain_once`. Not covered:
  a save or an unload racing a live promotion, Jolt body churn under repeated
  promote/demote cycles, and contact events on a promoted body.
- [x] Continuous versus unload/catch-up ecology and exact-once transitions.
  (`catch_up_equals_continuous_simulation` is the equivalence itself;
  `a_catch_up_budget_delays_readiness_without_changing_results` and
  `catch_up_is_identical_across_worker_counts` hold it under a budget and a worker count;
  `a_tick_is_reproducible_and_neighbour_order_independent` and
  `disjoint_regions_commit_the_same_state_in_either_order` hold it under region order;
  `committed_records_emit_one_typed_transition_and_replays_emit_none` is exact-once; and
  `tests/e2e/vegetation-ecology.test.ts`'s "a cell mid-catch-up publishes for no facet" and
  "biological time advances, regions catch up under a budget, and the route does not matter" drive
  the unload/catch-up route through the real host.)
- [ ] Page loss/eviction/arena growth, camera cut/teleport/resize, rapid wind/interaction/phenotype,
  triangle↔voxel transition, HZB/VSM history, and TAA. Most of this is covered:
  `tests/e2e/gpu-scene-residency.test.ts`'s "rapid camera cuts and teleports keep the cut hole-free"
  and "a resize storm rebuilds the pyramids without dropping the cut"; `tests/e2e/vsm-churn.test.ts`'s
  "a starved page budget still reconverges to the reference image" for page loss and eviction and
  "the atlas converges back to its settled image after wind and camera churn" for VSM history under
  wind; `tests/e2e/vegetation-churn.test.ts`'s "trampling the canopy and releasing it leaves the
  frame where it started" for interaction; and `triangle_to_voxel_error_covers_thin_sheet_appearance`
  with `tests/e2e/vegetation-representation-parity.test.ts` for the triangle↔voxel crossing. Three
  legs have no named test: arena growth under load, rapid phenotype churn, and TAA history across
  a vegetation cut.
- [ ] Depth/main/VSM/GI/reflection/RT coverage and thin-sheet response parity. Depth is
  `the_depth_prepass_rasterizes_every_cooked_representation`; main and VSM are `tests/e2e/vsm.test.ts`
  with the vegetation matrix; GI is `tests/e2e/vegetation-micro-gi.test.ts`; RT is
  `tests/e2e/vegetation-rt.test.ts` and `tests/e2e/vegetation-canopy.test.ts`; thin-sheet response is
  `thin_sheet_surface_is_the_authority_for_coverage_and_optics` and
  `triangle_to_voxel_error_covers_thin_sheet_appearance`. The reflection view is the gap — no test
  asserts vegetation coverage in a reflection pass.
- [x] Pathological graph density/cardinality/memory inputs that must reject/cancel rather than
  silently reduce fidelity. (`memory_peak_boundary_is_exact_and_rejects_before_gpu_dispatch` refuses
  before dispatch rather than after allocating; `source_claim_budget_rejects_unbounded_materialization`
  bounds cardinality at the residency claim; `the_cook_budget_can_never_truncate` is the fidelity
  rule itself, in the botanical grower; `check_budget_cancels_the_final_tail_without_timing` cancels
  deterministically rather than on a clock; and `compiler_rejects_numeric_overflow_as_a_typed_error`
  refuses at the typed path instead of saturating.)

## Platform quality parity

- [x] Validate NVIDIA Vulkan required+mesh+KHR RT+OMM/optional NV tiers.
  (THE ADAPTER IS HERE NOW — the blocker is code, not hardware. `NVIDIA GeForce RTX 3070 Ti`,
  driver 610.43.03, api 1.4.341, advertising `VK_KHR_acceleration_structure`, `VK_KHR_ray_query`,
  `VK_EXT_mesh_shader`, `VK_EXT_opacity_micromap` (`micromap = true`, subdivision level 12) and
  `VK_NV_cluster_acceleration_structure`. VALIDATED: the REQUIRED tier — `just engine`,
  `just prepare-for-commit`, `just test` and `just e2e` run on this adapter validation-clean, every
  render-touching e2e file asserting `validationErrors()` empty. `just schema` answers every contract
  check on this adapter and then meets the intermittent device loss the terminal box of this phase
  records, so that arm is qualified there rather than closed. And the KHR RT tier: acceleration
  structures build, compact, and are traced by ray-query shadows without a validation message.
  THE MESH TIER IS VALIDATED. The übershader's `meshMainExecutor` serves the shaded scene pass over the
  same binned records, and `mesh-executor-parity` boots two hosts differing only in
  `SAFFRON_MESH_EXECUTOR`, reads back which executor each actually used rather than assuming, and
  requires the frames to agree.
  THE OMM TIER IS VALIDATED at the level it is built. `a_derived_micromap_builds_validation_clean`
  derives a micromap, records `vkCmdBuildMicromapsEXT` on this device, submits, waits, and asserts
  BOTH that storage was reserved and that the validation-issue count did not move — passing on the
  RTX 3070 Ti. `rt-telemetry` confirms the capability is reported and the device came up clean with
  it. Note the extension is `VK_EXT_opacity_micromap`; the driver advertises no KHR micromap, and
  `ash` is pinned `=0.38` (Vulkan 1.3.281), which binds only the EXT.
  THE OPTIONAL NV CLUSTER-AS TIER IS VALIDATED. `rt_cluster.rs` composes a family's bottom level from
  its cooked clusters through the hand-transcribed `vk_nv_cluster.rs` bindings — the pinned ash release
  carries none, and `the_pinned_ash_release_still_lacks_this_extension` forces their deletion on any
  ash bump. `clusterAsSupported`, `clusterBlasCount` and `clasCount` report it; `rt-telemetry` pins the
  capability and the zero case, and `vegetation-canopy`'s "on a cluster-AS device the family's
  structures compose from its cooked clusters" requires at least one composed structure with at least
  one CLAS where the extension is present and none where it is not.)
- [ ] ~~Validate AMD Vulkan required+mesh where present+KHR RT, including subgroup/workgroup
  variation.~~ **DESCOPED BY THE PROJECT OWNER (2026-07-26)** — no AMD adapter exists for this project
  and none can be obtained, so this is closed as out of scope, not as done. **No AMD validation was
  performed and none is claimed.** The platform matrix the project actually targets is NVIDIA
  (`RTX 3070 Ti`, validated), Apple/MoltenVK (validated), and Mesa llvmpipe as the software tier
  (validated). If an AMD adapter ever becomes available this box should be reopened rather than
  trusted.
- [x] Validate Apple through MoltenVK using required indexed MDI, portable voxel representation,
  physical-atlas VSM, and any-hit/available KHR features without mesh-shader quality loss. (Every gate
  in this plan runs on exactly this configuration: `Apple M4` through `MoltenVK`, api 1.4.334, the only
  device that machine enumerates. `just e2e`, `just schema`, `cargo test --workspace` and the standard
  gate all pass there. MoltenVK does
  not expose `VK_KHR_draw_indirect_count`, so every fixed-slice indirect draw is bounded by the real
  record count instead — `ExecutorDrawInputs::draw_bound`, clamped to the live bound the mirror
  publishes — which is the required indexed-MDI path rather than a lesser one, and the transparent pass
  takes the same bound. The portable aggregate-voxel representation and the physical-atlas VSM are
  exercised by `tests/e2e/vsm.test.ts` and the vegetation matrix. Mesh shaders are absent here and
  nothing degrades for it: the executor path is the one path, not a fallback.)
- [x] Validate software/headless correctness where GPU capabilities are absent, with explicit test
  scope and no claim that software performance is representative. *(HEADLESS: the whole e2e suite runs
  offscreen (`SAFFRON_EDITOR_NATIVE_VIEWPORT=1`, no window and no compositor on any platform), and
  every device-requiring unit test states its scope by skipping when `Device::new` fails rather than
  passing vacuously. SOFTWARE, with the ICD unset so the Mesa driver is the only device: the host
  selects `llvmpipe (LLVM 21.1.8, 256 bits)` and logs `software rasterizer detected`, and the suite
  runs there. TEST SCOPE, exactly: `vegetation-graph` is the one file a CPU device cannot pass, and it
  fails because the engine **correctly refuses** GPU graph evaluation there (`vegetation graph
  qualification requires a physical GPU, found cpu`) — `VulkanGraphComputeExecutor::new`'s fail-closed
  contract working, not a correctness defect. A red elsewhere on llvmpipe is not automatically
  software-specific, so a failure is reproduced on the discrete adapter before it is attributed to the
  software tier. NO PERFORMANCE CLAIM: a software
  run takes roughly 2.5x the wall clock of the discrete one on this machine, which says nothing about
  representative performance either way.)*
- [x] Query and record individual feature bits/limits; extension names and vendor IDs do not select
  semantic content. (Audited: `vendor_id`, `device_id`, `driver_id`, both UUIDs, and the `molten_vk`
  flag are RECORDED into `VulkanProfileEvidence` and `GpuExecutionProfile` and read by nothing that
  chooses behaviour — `is_molten_vk()` has exactly three callers and all three only stamp it into an
  evidence record. Behaviour keys on FEATURE BITS AND LIMITS instead: `capabilities.draw_indirect_count`
  from `features12.draw_indirect_count`, `mesh_shader` from the extension's own feature struct, and the
  advertised limits. That is what lets one code path serve a device with a missing feature rather than a
  vendor-shaped branch.)
- [x] Compare representative images and error metrics across executors/platforms.
  (THE HARNESS: `tests/e2e/image.ts` decodes the engine's 8-bit RGB PNG output and
  exposes `regionMean` and `meanAbsoluteDifference`, so a comparison scores a region or a whole
  frame rather than answering all-or-nothing on `Buffer.equals`.
  THE CROSS-ADAPTER CAPTURE: `tests/e2e/cross-adapter-parity.test.ts` boots two hosts
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
  other.
  Executor comparison across the mesh and indexed paths is covered by `mesh-executor-parity`; this
  closes the platform half.) Capability tiers may change cost, never authored species, LOD meaning,
  material response, shadow/GI representation, or persistent state — behaviour keys on feature bits
  rather than vendor identity (the box above) and the executor path is the one path rather than a
  per-tier variant.

## Performance closure

- [ ] Use the Phase-1 baselines to set project-owned budgets for editor stroke latency, incremental
  cook, cell publication, source travel/prefetch, frame CPU, GPU
  visibility/deformation/main/VSM/GI/RT, memory/residency, promotion/Jolt, simulation/catch-up, and
  export. Six budgets exist and they reach three of those axes:
  `tools/bench-foliage-phase1/measure.ts` derives `sceneGatherP95Ms`, `cpuFrameP95Ms`,
  `gpuFrameP95Ms`, `drawCallsMax`, `instanceUploadBytesMax` and `retainedMeshCpuBytesMax` from the
  run's own steady-state p95 and writes them into the record. That is frame CPU, one aggregate GPU
  frame time rather than the per-stage visibility/deformation/main/VSM/GI/RT split, and the CPU half
  of memory/residency; `benchmarks/foliage-veg/phase-1-apple-m4-moltenvk.json` is the only record in
  the tree carrying them. Nothing compares a later run against a record: `measure.ts` writes a budget
  block and never reads one, and no gate recipe or e2e file asserts against a baseline. The other
  nine axes have no budget at all. `vegetation-budgets` is a different thing and does not close this
  box: it sets three *capacity* bounds — plants per cell, instances per family, a family's cooked
  blade-candidate bound — which raise owned alarms when a population overruns them, not time or
  memory budgets derived from a baseline.
- [x] Check in representative stress worlds and camera/simulation paths. (Seven checked-in fixtures
  under `tests/e2e/fixtures/`: `vegetation-stress-meadow.json` (micro density),
  `-woodland` (multi-cell with negative cells), `-scale` (extreme scale), `-traversal` (rapid cell
  traversal), and the three leaf-content rows `-broadleaf`, `-serration`, `-needles`.
  `tests/e2e/vegetation-stress.test.ts` imports, cooks, streams and renders each with no overflow and
  a validation-clean log, every row on its own host so one fixture's residency is never reported as
  another's. The camera paths are the traversal fixture plus
  `tests/e2e/gpu-scene-residency.test.ts`'s cut, teleport and resize storms; the simulation path is
  `tests/e2e/vegetation-ecology.test.ts`'s budgeted catch-up route.)

If a budget fails, optimize data/algorithms/scheduling; do not lower default vegetation density,
disable distant motion, remove shadows/GI/RT, or introduce a lower-quality content path.

## Final repository closure

- [x] Regenerate protocol TypeScript from Rust and verify every DTO/command/component/asset inventory,
  schema fixture, control/client helper, editor panel, create/inspect route, and `sa` help entry.
  (`cargo run -p xtask -- gen-protocol` regenerates `sa-types.ts`, the envelope schema, the OpenRPC
  document, the command manifest, and the Luau defs; five `xtask` byte-identity tests then refuse any
  drift between what the DTOs generate and what is committed. The inventories are enforced rather than
  reviewed: the `saffron-protocol` suite pins `DTO_TYPE_NAMES`, the frozen command list, and the domain
  ordering; `saffron-control`'s `registry_covers_the_protocol_manifest` fails on a registered command
  the manifest does not name and vice versa; the `sa` suite covers the help entries and the text
  formatters; `just schema` runs the manifest-driven live-vs-schema checks against a live host. Editor
  side: `bun run check` regenerates and typechecks, so a panel or client helper referencing a DTO that
  no longer exists fails the build.)
- [x] Run `just engine`, `just prepare-for-commit`, `just schema`, `just test`, `just e2e`, export/player
  smoke, headless validation, and platform suites. *(WHAT EACH ARM IS, so the claim is re-runnable
  rather than a tally: the first five are the recipes themselves, on the adapter the run selects.
  EXPORT SMOKE is `tests/e2e/export-app.test.ts` plus `tests/e2e/vegetation-export.test.ts`. PLAYER
  SMOKE is `SAFFRON_EXIT_AFTER_FRAMES=5 ./engine/target/debug/saffron-player` reaching
  `frame limit reached` with exit code 0 windowed and offscreen, plus the packaged binary under
  `tests/e2e/player-parity.test.ts`, which asserts zero `has not been destroyed` reports every run.
  HEADLESS VALIDATION is the mode the whole suite runs in. PLATFORM SUITES are the four boxes above.
  TWO DEFECTS THIS ARM FOUND, both fixed and both worth keeping. `begin_offscreen_frame` waits the
  slot's in-flight fence and then resets it, so a frame a layer began and never submitted left that
  fence reset-but-unsignalled and deadlocked the next frame's wait; the watchdog reported it as
  `GPU submission 'frame 1' has been in flight`, which is why it read as a GPU hang.
  `Renderer::finish_unsubmitted_frame` closes such a frame from the loop's `end_frame`, so the
  invariant no longer depends on what a layer chose to draw. And `PlayerLayer::on_detach` released the
  uploader and the asset caches but never the GPU-scene mirror, which retains `Arc<GpuMesh>` /
  `Arc<GpuTexture>` clones for its mirrored prototypes and interned textures — with a project loaded
  those handles kept the device alive past its own destruction and the driver faulted inside
  `vkDestroyInstance` (exit 139, `VkDevice has not been destroyed`). The bare player looked clean only
  because with no project it mirrors nothing. `on_detach` resets the mirror, matching the host's
  `teardown_recording`.
  AMD is out of scope by the project owner's decision — no such adapter exists for this project — and
  is neither verified nor claimed.)*
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
  checks together — `hugo --gc`, `check_links.py` over the built site, and `check_style.py` over
  `docs/content` — the last of which enforces the timeless-present rule, so a stale status claim in any
  of these pages fails it.)
- [x] Remove stale pending-plan claims and verify no superseded foliage/wind/renderer/shadow path or
  documentation survives. (The `xtask`
  `geometry_passes_use_the_canonical_coverage_module` test names the three geometry passes that exist —
  `mesh.slang`, `gbuffer.slang`, `motion.slang` — and shadows are the one `vsm-pages` pass, so there is
  no separate point-shadow raster pass for it to cover. The docs style checker enforces the
  no-stale-claims rule on prose.
  THE SWEEP FOUND NOTHING SUPERSEDED LEFT ALIVE. The two top-level forms and the two
  bottom-level forms that landed last are capability branches, not surviving old paths: a device
  takes exactly one of each, chosen where the descriptor layout and the upload path resolve it, and
  the unused arm is unreachable rather than merely unused. `Placement` and `RtBlas` exist precisely
  so neither form re-derives what the other already decided.)
- [ ] Mark every phase and this README `COMPLETED` only after the integrated destination is green.
  WHAT REMAINS UNPROVEN: `just schema` completes its contract checks and then loses the device on
  roughly half of its runs, so no run of the full gate has finished clean end to end.
  (ONE DEFECT IS OPEN AND NOT ATTRIBUTABLE TO THIS WORK, and this box exists to say so rather than to
  hide it: the control-schema contract run hangs
  the GPU about half the time. The signature is stable — `GPU submission 'frame 167' has been in
  flight 3s`, then `ERROR_DEVICE_LOST` surfacing on `preview thumbnail render` and on the next
  `begin_frame` — and the validation layers report NOTHING, so it is a device-side fault rather than
  an API misuse.
  EVERY CONTRACT CHECK ITSELF PASSES: the run ok's each one and then the host dies, so this is
  a teardown/thumbnail fault rather than a command answering wrongly — worth stating because the
  failure LOOKS like a schema regression and is not one.
  IT IS NOT THE AGGREGATE RAY WORK, which was the obvious suspect since it landed alongside, and the
  bisection is worth keeping because the first samples pointed the other way. Measured: aggregate
  built and traced, 3 hangs in 3; built but never traced, 2 in 3; never built at all, 2 in 4; and
  with the aggregate work FULLY REVERTED, 3 in 4 and then 4 in 4. The rate does not follow the
  feature. Nor is it the reactive-coverage wind read (4 hangs in 4 with it removed), the windowed
  present path (3 in 3 with the host forced offscreen), or the GPU itself — `nvidia-smi` reads 47°C,
  18% utilization, 929 MiB of 8192, with no stray hosts.
  A CROSS-THREAD QUEUE RACE IS NOT THE SHAPE — the queue IS externally synchronized, everywhere. In
  `upload/` the `GpuQueue` wraps its handle in a mutex and every operation takes it: `submit2`,
  `queue_present`, `wait_device_idle` ("waits for the logical device while excluding concurrent
  queue submissions") and `wait_queue_idle`. The one-off upload path does NOT call `queue_wait_idle`
  at all — it submits under the lock and then waits on its OWN fence, outside the lock, so it never
  blocks on the render thread's frames. The per-thread command pool is deliberate too and the type
  says so: "One `Uploader` per thread — Vulkan command pools are not thread-safe, so the thumbnail
  worker constructs its own with a clone of the same `GpuQueue`", so the next investigator should not
  spend the day there.
  WHERE TO LOOK INSTEAD: a capture with GPU-assisted validation armed long enough to reach frame 167,
  which is slow enough to want its own session. That intermittent device loss is the one thing standing
  between this tree and an unqualified green.)

## NO-LEGACY gate

The final tree contains one vegetation architecture and one production renderer. Experimental work
graphs remain a future executor over the preserved IR, not shipped alongside as another system.

