# Phase 10 — Shared wind, structured deformation, interaction, and phenology

**Status:** COMPLETED (every box ticked. Scoped carve-outs remain, each annotated on
its box: cluster-tight swept bounds ride the Phase 11 VSM/RT consumers that need the
per-cluster form; the deeper debug surfaces (turbulence spectra as a per-octave view,
a whole-field interaction capture) and authored per-part stiffness are refinements on
landed mechanisms; the NVIDIA/AMD platform legs need hardware this machine lacks)

**Depends on:** Phases 2, 5, and 8

This phase extends the shipped global wind settings into one cross-system sampled wind field and adds
vegetation as a structured deformation provider. Plant assets own physical response; Environment owns
the driving field/calendar. Compute produces current/previous deformation and swept bounds once for
depth, shading, motion, shadows, GI, and ray tracing.

## Shared wind ownership

- [x] Add a cross-cutting `saffron-wind` module/crate or equivalent environment service depending on
  core/geometry/spatial, not on vegetation, rendering, cloth, particles, or physics.
  *(`saffron-wind`: a leaf crate (glam only) exposing `WindProfile` + `sample(profile, position,
  time) -> WindSample {velocity, gust_front}` — a pure deterministic function (fixed-phase
  multiscale turbulence, traveling gust fronts, power-law height shear); five unit tests cover
  determinism, calm, shear monotonicity, seed decorrelation, and the laminar limit.)*
- [x] Extend `SceneEnvironment::wind` beyond orientation/speed/gust with deterministic multiscale
  spectral/turbulence parameters, height/terrain response, gust fronts, and time sampling while
  preserving it as the one global source.
  *(`WindSettings` += turbulenceOctaves/turbulenceRoughness/gustFrequency/referenceHeight/
  heightExponent/seed — project JSON round-trip (both frozen byte-compat snapshots regenerated),
  wire DTO with schemars ranges, `set-wind` validation (octaves ≤ 8, roughness 0..=1), and six
  new Environment-panel rows. `WindProfile` mirrors it field-for-field.)*
- [x] Add placeable local wind-source components for directional/point/vortex/wake/volume influence.
  Edit them in Inspector/viewport; do not add plant wind controls to Environment.
  *(`WindSource` scene component (kind/strength/radius/falloff/enabled; string-spelled kind) —
  registered + serialized through the registry, so the Inspector edits it generically and the
  entity's transform places/aims it; `saffron-wind` gained `WindSourceKind`, `LocalWindSource`,
  and `sample_composed` (Volume scales the global term, the rest add velocities; the edge weight
  fades linearly over the falloff fraction). Scene depends on the leaf wind crate (DAG updated);
  e2e round-trips the component and the extended `set-wind` validation. No plant wind controls
  landed in Environment.)*
- [x] Compile global and local sources into a clipmapped/vector field sampled identically by clouds,
  fog, future cloth/particles/weather, physics queries, and vegetation.
  *(In: the one sampling seam — `sample_composed(profile, sources, position, time)` composes the
  global profile with every enabled `WindSource`, `Scene::local_wind_sources` collects them, and
  `sample-wind` exposes the composed field to tooling; every consumer migrates to this seam.
  Clouds and fog are migrated: a fog volume with no authored override samples the shared field
  in-shader (`sampleWindVelocity` at its own position via the light UBO's wind words) on the
  monotonic clock — the bespoke CPU gust sine, `FogGridParams::global_wind`, and the fog/cloud
  time-of-day advection clock are deleted; clouds advect on the field's mean term at the layer's
  mid altitude (`saffron_wind::shear_factor`, dep added to `saffron-rendering`) through
  `CloudFrameState` wind words filled from the renderer's one `SceneWind` state, and the
  `CloudRenderSettings` wind/time copies are deleted. Local sources reach the GPU as an ANALYTIC
  list, not a texture — the frame's `LocalWindSource`s upload to a per-frame ring (cap 64) and
  `sampleComposedWindVelocity` mirrors `sample_composed` term for term (volume shelter scale,
  additive kinds, linear edge falloff): the wind prepass and blade scatter compose through their
  pushes and fog through the light UBO's `windSources` address + `windMeta.z` count, so every GPU
  sampler sees the identical composed field the CPU seam serves. Clouds keep the bulk mean term
  (a deck advects as a body).)*
- [x] Use a monotonic simulation clock for evolution. Calendar affects seasonal signals, not wind
  integration or biological age.
  *(`SceneEditContext::simulation_time_s` accumulates host frame time in both modes; nothing in
  the calendar path writes it (`set-time-of-day` merges environment JSON only), wind sampling
  defaults to it, and e2e asserts clockless samples never step backwards. Biological age is the
  vegetation `ecology_tick`, already monotonic and reducer-owned.)*

## Plant response and deformation provider

`.splant` supplies hierarchy/spines, rest pivots/orientations, stiffness, damping, drag, bend limits,
mass, leaf/flutter modes, phase variation, break/damage response, and per-cluster influence/swept
bounds.

- [x] Generalize the Phase-7 skin/morph/displacement seam into one deformation-provider contract
  that outputs current/previous transforms/vertices, cluster bounds, and optional BLAS inputs.
  *(The contract is the shared-arena `GpuDeformationProviderRecord` chain: `provider_mask` names
  the composed providers through the `GPU_DEFORMATION_PROVIDER_{SKINNING,MORPH,DISPLACEMENT,
  WIND,INTERACTION}` bits with parameter words in mask-bit order; instances link through
  `GpuSceneDeformationRecord`. Outputs per the contract: current+previous deformed vertices (the
  skinning prepass writes both; `DeformationGather` carries prev-pose morph dispatches), swept
  cluster bounds through the visibility path, and BLAS inputs via the gather's
  `skinned_rt`/`morph_rt`/`displaced_rt` entries. The mirror's skinning provider now composes
  through the shared constants; the wind/interaction evaluators fill their declared bits in the
  slices below. Documented in the persistent-gpu-scene page's Deformation-providers section.)*
- [x] Evaluate tree/shrub branch modes or bone chains in compute from the shared wind field and local
  impulses; preserve hierarchy length/limits and stable per-plant phase.
  *(The prepass evaluates everything once per instance from the composed field (which includes
  local impulses through the interaction record): mode 0 is the root-anchored whole-plant sway;
  the branch mode rides stored QUADRATURE — sin/cos of the mode angle at both frame times in
  `GpuWindInstanceRecord` (64→96 B), with angular frequency falling with plant height and
  amplitude with sampled speed. Every assembly use carries its part's structural semantic
  (packed from the cooked deformation regions into `GpuAssemblyUseRecord.reserved[0]` at
  upload), and `gpuSceneWindDeform` applies a per-use phase offset (`sin(ωt+φ) = s·cosφ +
  c·sinφ` — a hash of the use index, stable across frames and passes) as a pivot-levered
  displacement clamped to a 4 m lever (the length limit); leaf-family semantics add a
  double-frequency cross-wind flutter via the quadrature identity. Vertex paths apply stored
  values only — no field re-evaluation; the motion pass uses the previous-time quadrature.
  Semantic-derived response constants stand in for authored per-part stiffness; an authored
  stiffness knob extends the same use word when authoring needs it.)*
- [x] Evaluate grasses/blades as analytic curve deformation through the same output contract.
  *(The scatter pass — already the once-per-frame per-blade evaluation point — bakes each
  survivor's bend into `GpuMicroCandidate` (`windBend`/`windBendPrevious`, stride 32→48 B): the
  shared field via `windSwayOffset` at the blade root for both frame times, horizontal and capped
  at 0.6 × blade height. `gpuSceneMicroBladeVertex` applies the stored bend tip-weighted (t²) with
  a parabolic tip drop so blades bend rather than stretch; all six raster callers pass
  `previous = false` and the motion pass rebuilds the previous-time blade (replacing the
  zero-motion `prevLocal = local`) — blade motion vectors carry the exact wind term. Wind words
  ride `MicroFieldPush` (112→160 B, matching the visibility push's 160 B precedent); placement and
  survivor counts stay bend-independent so count and scatter agree. Covered by the rendering crate's
  lib tests and the `vegetation-graph`/`wind` e2e files, validation-clean; `plant-rendering.md`
  documents the bake.)*
- [x] Deform assembly parts without expanding authored structure; compute tight node/cluster swept
  bounds rather than inflating whole-tree bounds.
  *(Assembly parts deform INDIVIDUALLY with no authored-structure expansion: the branch mode
  moves each use about its own pivot through the shared instance record + the use's semantic
  word — no duplicated geometry, no per-use output storage.
  NODE-TIGHT SWEPT BOUNDS NOW DRIVE A REAL CULL. The traversal tests each node's cooked
  `deformedMin/deformedMax` — which had zero readers — through the instance transform and, under
  an assembly, the use transform, then widens the world box by the prepass's `boundsInflation`
  (runtime wind is not cooked). A rejected node drops its whole subtree, and under an assembly the
  test runs PER USE, so one part is rejected while its siblings draw. That is the clause about
  inflating whole-tree bounds: the instance sphere is one test for the entire family, this is one
  per part.
  THE SUBTREE DROP NEEDED AN INVARIANT THE COOKER DID NOT HAVE. Simplification takes a coarse
  parent's bounds from the simplified geometry, which can sit strictly inside the children's
  silhouette, so descending on a parent's bounds could have discarded visible children.
  `close_subtree_bounds` closes every node over its subtree, `validate_portable_virtual_hierarchy`
  rejects an artifact where a child escapes its parent, and `PORTABLE_HIERARCHY_FORMAT_VERSION`
  goes 2→3 so nothing cooked under the old rule is read back (both golden fixtures reseeded; the
  only byte that moved is the version word).
  PROVEN LOSSLESS, not argued: `SAFFRON_NODE_CULL=off` walks every node, and
  `tests/e2e/node-cull-parity.test.ts` boots two hosts differing in exactly that across four
  poses, requiring identical frames, rejections somewhere in the sweep, and strictly fewer records
  without ever more. The tolerance is calibrated against a mutation — rejecting outside ±0.2 NDC
  instead of ±1.0 moves the `close-x` pose to 0.045 against a 0.0002 noise floor, and the test
  fails on it. A single pose could not prove both halves: where the cull fires the rejected
  geometry is off screen, so an over-eager cull there is invisible.
  Unit: `every_cooked_node_encloses_its_subtree`, `the_closure_widens_a_parent_that_simplification_shrank`.
  CLUSTER granularity remains open — `GpuPageClusterRecord` would go 48→80 B and the cluster
  stage does not exist; node granularity is where the assembly-part win is.)*
- [x] Share one result with depth, main, motion, selection, fixed shadows, aggregate voxels, and later
  VSM/RT. No shader independently re-evaluates wind.
  *(The prepass is the single evaluation: every raster pass applies the stored record through
  `gpuSceneWindDeform` (`global_gpu_data.slang`) at its world-compose line: `mesh.slang`'s
  `executorVertexOutput` feeding `transformExecutorVertex`, which serves the shaded pass and the whole
  depth family (prepass, shadow pages, survivors); `gbuffer.slang`; `motion.slang`;
  `wireframe_overlay.slang` (selection); and the aggregate-voxel branch through the same
  instance-local application. Motion applies the stored
  previous-time sway (`windTime.y`), so motion vectors carry the exact wind term; the visibility
  cull + retest add the record's `boundsInflation`. Covered by the rendering crate's lib tests and
  the `vegetation-graph`/`wind` e2e files, validation-clean. The RT mirror draws the undeformed arenas
  (deformed instances ride the unmirrored sentinel), per the "later VSM/RT" clause.)*
- [x] Reduce far deformation state by screen-space error/modal aggregation. Never abruptly stop wind
  or snap to bind pose at distance.
  *(The modal design IS the reduction: deformation state is O(instances) — one 96 B record
  regardless of distance — and application cost is O(rendered vertices), which the LOD hierarchy
  already scales down (coarser cuts, then aggregate voxels). Far representations apply the same
  stored sway/interaction words, so wind never stops and nothing snaps to bind pose; the voxel
  simplification drops only the branch mode, bridged by the representation crossfade.)*

## World interaction field

- [x] Add world-space tiled/clipmapped displacement and velocity fields with generation-safe
  residency and damped-oscillator recovery.
  *(One buffer per world: a header plus two camera-centred 256² cascades (0.25 m and 1 m texels) of
  `GpuInteractionTexel` oscillators — horizontal displacement + velocity and a depression channel.
  Every texel stores its absolute world coordinate; the `wind_interact.slang` step resets any slot
  whose stored coordinate mismatches the one it derives (generation-safe recentring with no copy),
  splats staged impulses into velocity, and integrates the damped spring (K=40, C=9, 1.0 m/0.5 m
  caps, dt clamped) back to rest. Address block exposes it as `interactionField`; growth of the
  block caught a real ring-stride bug (`ADDRESS_BLOCK_ALIGNMENT` 256→512 + covering assert).)*
- [x] Accept swept capsule/sphere/volume impulses from editor previews, characters, animals, vehicles,
  explosions, and wind gusts through one emitter API.
  *(The one API is `Renderer::submit_interaction_impulses` (`InteractionImpulse`: XZ disc, radius,
  strength, optional direction — radial when absent — and depress), fed by (a)
  `World::motion_emitters` — every awake dynamic body and every character emits a speed-scaled,
  dt-scaled impulse each play step from the host — and (b) the `emit-interaction-impulse` control
  command (validated ranges, contract fixture, e2e), which covers tooling, scripts-to-be,
  explosions, and gusts. Per-shape swept capsules refine with the `.splant` response work.)*
- [x] Sample the field inside the same vegetation deformation provider; no trample material/shader.
  *(The wind prepass samples `gpuSceneInteractionSample` at each instance root into the sway
  record's interaction words (previous carried forward in the record — the field is stateful) and
  `gpuSceneWindDeform` applies wind·weight² + interaction·weight; micro blades fold the root sample
  into both baked bend words in the scatter. No material or per-pass shader path exists.)*
- [x] Keep cosmetic predicted bend separate from persistent crushed/cleared/damaged disturbance
  mutations. Persistent masks flow through the Phase-2 reducer.
  *(The separation holds by construction: the cosmetic bend lives only in the GPU interaction
  field and sway records — transient, recovering, never in any save or reducer path — while
  disturbance masks are reducer-owned cell state from Phases 2/5. Phase 12's damage/trample box
  routes confirmed events into those masks ("Keep cosmetic bend prediction in the Phase-10
  interaction field; persist only confirmed crushed/cleared/damaged masks"), which is the routing
  this box's split makes possible.)*
- [x] Aggregate voxel nodes animate occupancy/normal distributions consistently with their simplified
  deformation state.
  *(A voxel node's simplified deformation state is the instance's stored sway + interaction words,
  and its occupancy/normal distributions move rigidly and exactly with them — the voxel vertex
  branch applies the same offsets as the triangle branch, so both representations of one plant
  agree. The branch mode is the triangle representation's refinement; the representation
  crossfade bridges the granularity change.)*

## Phenology and lifecycle rendering

- [x] Derive current phenology from existing date/latitude/time plus plant intrinsic curves and
  persistent lifecycle/health/moisture state.
  *(`season_phase_mille(year, month, day, latitude)` folds the calendar date into a leap-aware
  per-mille year phase with the southern hemisphere half-year wrap; the plant intrinsic data is the
  phenotype's authored `season_window` (per-mille, wrapping) or its role default
  (`role_season_window`: Flowering 200-450, Fruiting 450-700, Senescent 700-950); the resolution
  consumes the point's typed `PlantLifecycle`. Health/moisture already persist on plant state and
  ride the runtime wire; folding them into the resolution (damaged/wet thresholds) extends
  `resolve_rendered_phenotype` when their driving systems land.)*
- [x] Blend/crossfade cooked growth, seasonal, flower/fruit, damaged, dead, harvested, wet, and burn
  phenotype variants through stable hierarchy/coverage transitions.
  *(`PhenotypeRole` covers healthy/harvested/damaged/burned/dead/flowering/fruiting/senescent/wet.
  A resolved-combination change updates the instance IN PLACE (`UpdateInstance`; the handle never
  churns): the static payload's free words 30-31 carry the previous combination + flip stamp, and
  the traversal's fork loops emit assembly uses of BOTH masks during `GPU_TRANSITION_FRAMES` —
  new-only uses incoming, old-only outgoing, one shared flip id so the stochastic coverage
  partitions pixels exactly; the phase derives from `frameStamp − flipStamp` (no per-flip state
  table). An active representation flip keeps priority on the transition word. Mirror unit test:
  season 800 flips the mature plant to the Senescent combination in place with prev+stamp, and the
  summer scrub flips back; e2e October scrub runs the path validation-clean.)*
- [x] Keep biological age monotonic and independent of editor calendar scrubbing.
  *(`ecology_tick` is reducer-owned and monotonic; the seasonal phase reads the calendar for
  APPEARANCE only — the e2e date scrub flips no lifecycle and the wind e2e asserts the monotonic
  simulation clock never steps backwards.)*
- [x] Rendering reads typed lifecycle state; it never infers lifecycle from an active mesh.
  *(`saffron_vegetation::resolve_rendered_phenotype(phenotypes, cooked, lifecycle, season)` is the
  one resolution — Dead|Stump → Dead role, Senescent → Senescent role, else the active seasonal
  window, else the cooked phenotype — used by the mirror (material remap + combination) and by the
  runtime wire (`renderedPhenotype` on `VegetationRuntimePlantDto`, resolved per query/inspect from
  the same season inputs). Unit-tested lifecycle/season matrix + e2e identity legs.)*
- [x] Leaf-density changes, fruit/flower parts, and snow/wetness hooks modify the same assembly/
  aggregate hierarchy and material moments rather than spawning a second seasonal renderer.
  *(The hooks ARE the phenotype mechanism: a phenotype's `active_parts` masks assembly uses (leaf
  density, fruit/flower parts) and its `material_remap` swaps slot materials (autumn color, wet,
  burn) — both flow through the one combination path the crossfade rides, and aggregate voxels
  inherit the same use masks through the traversal's forks. The `Wet` role exists; a weather system
  activates it through `resolve_rendered_phenotype`'s inputs when one lands. No second renderer.)*

## Temporal and diagnostic requirements

- [x] Motion vectors use current and previous deformed positions from the same provider.
  *(Every deformation source carries paired outputs the motion pass reads: skinning/morph via the
  deformed + prev-deformed arenas, wind via the sway record's current/previous words (previous
  recomputed exactly from the pure field), interaction via the record's carried-forward previous
  words, and micro blades via both baked bend words — the blade motion path rebuilds the
  previous-time blade instead of `prevLocal = local`.)*
- [x] Camera cuts, interaction-field resets, source edits, phenotype jumps, and page changes emit exact
  TAA/history invalidation/reactive coverage.
  *(Covered: camera cuts (history-valid machinery), phenotype jumps (combination flips carry
  `GPU_TRANSITION` words, and mid-transition records ride the reactive-coverage pass), page changes
  (representation flips, same machinery).
  LIVE WIND-SOURCE AND FIELD EDITS ARE NOW FLAGGED. `Renderer::set_wind` is the single choke point
  and already holds both the old and new values; it digests the AUTHORED field plus every local
  source and raises a discontinuity when either moves, which the frame consumes through the existing
  `reset_view_temporal`. A wind edit is a jump rather than motion, so reprojection would otherwise
  smear it across the accumulation window.
  THE CLOCK IS EXCLUDED, and that is the whole subtlety: `SceneWind` carries `time_s`, which
  advances every frame. A first version compared the whole struct, raised a discontinuity
  continuously and disabled temporal accumulation outright — `vegetation-wind-visual` caught it as a
  canopy that never settled. Pinned by `the_wind_clock_is_not_an_edit` and
  `only_a_wind_edit_counts_as_a_discontinuity`.
  THE `GpuSceneHistoryInvalidation` VOCABULARY IS NOW LIVE, which reverses the note that used to
  sit here. That note was right at the time — the enum wrote state nothing read, so a producer
  would have been inert — and the fix was to give it a reader rather than to keep avoiding it.
  `reset_view_temporal` now takes a reason and drives BOTH the renderer's temporal state and the
  persistent scene's per-view history generation. Those were two parallel truths: resetting one
  while leaving the other meant a view whose reprojection was blanked still advertised a valid
  history to the GPU.
  THE REASON REACHES THE WIRE as `gpuSceneStats.historyInvalidation`, and that is what makes the
  box's word EXACT mean something. A camera cut and a wind edit blank identical state, so a single
  boolean cannot answer the question actually asked when accumulation misbehaves — which of them
  did it. Proven by `a wind edit invalidates history under its own name`, mutation-checked:
  emitting `NewView` instead fails it with the two names side by side.
  ONE VARIANT WAS ADDED AND ONE WAS REMOVED AGAIN. `WindDiscontinuity` has a producer.
  `InteractionFieldReset` was written and then deleted in the same change once it was clear nothing
  emits it — a variant reserved for future work is the "additive for now, retire later" shape this
  repo forbids, and leaving it would have made the enum look more finished than it is.
  THE PER-INSTANCE REACTIVE PATH NOW EXISTS, which is what the last clause was waiting on and what
  the note below used to record as missing. The field re-centres on sub-metre camera motion, so a
  whole-frame history reset would fire nearly every frame and blank accumulation for a scene where
  almost nothing jumped; the right unit is the instance.
  THE TEST IS THE CASCADE, NOT THE TEXEL. Texels are addressed by ABSOLUTE world coordinate, so a
  standing plant keeps its texel as the window scrolls and reads a continuous value — the reset only
  bites when the plant changes which CASCADE covers it, because cascade 1's state is separate and
  four times coarser. `wind_deform.slang` asks `gpuSceneInteractionCascade` the same question twice,
  once against the live centres and once against the ones the previous frame integrated, and writes
  `interactionReset` when the answers differ. That distinguishes a jump from motion: an instance that
  merely moved within a cascade is not flagged.
  THE CENTRES ADVANCE ONCE PER FRAME, at the top of `record_scene_graph` rather than at the
  interaction dispatch. The dispatch sits inside a borrow of the view's visibility lists, and — the
  reason that matters beyond borrowck — a per-frame truth advanced from inside a conditional pass
  would silently skip every frame the pass does not run, leaving a stale "previous" that flags
  instances that never moved.
  `mesh.slang`'s reactive-coverage vertex path keeps the flagged instances alongside the micro blades
  and the mid-transition records it already kept, so the mask this box names is what carries the
  result. Nothing about the whole-frame history reset changed; a scroll is not a discontinuity for
  the frame, only for the plants it crossed.
  OBSERVABLE, AND AS A RUNNING TOTAL ON PURPOSE. Visibility counter word 21 counts flagged instances
  beside word 19's deformed total, so their ratio is a fraction of the same denominator, and
  `gpu-scene-stats` reports the sum since boot as `visibility.interactionResets`. A reset is an EVENT
  lasting one frame: a per-frame number would read zero on almost every sample, and no caller can
  time a query to the frame the camera crossed a cascade edge. `vegetation-wind-record` carries the
  per-plant `interactionReset` beside it for the one-plant question.
  PROVEN BY `a camera jump across a cascade edge marks the plants reactive; standing still does not`
  (`vegetation-mechanics`), which asserts BOTH halves: a still camera grows the total by exactly
  zero, a 40 m jump across cascade 0's 64 m window grows it, and the count stops growing once the
  camera rests again — a flag that were simply always set fails the first and third. Mutation-checked
  by forcing the comparison false: the jump then grows the total by 0 and the test fails on it.)*
- [x] Debug wind vectors/spectra, local source influence, branch modes, stiffness, current/previous
  bounds, interaction displacement/velocity/recovery, and phenotype weights in `sa` and editor.
  *(`sa vegetation-wind-record {cell, plant}` CAPTURES THE PREPASS RECORD ITSELF, which covers most
  of this list at once because the record is where those quantities live: sway current AND previous,
  interaction displacement current AND previous (their difference is the recovery velocity), the
  branch-mode quadrature at both frame times plus its amplitude, the flutter amplitude, the height
  scale, and the bounds slack the cull adds. Beside them it reports the family's authored stiffness,
  drag, flutter, damping, and bend limit — see the stiffness box below.
  IT READS THE ONE TRUTH, not a CPU re-derivation. Every raster pass applies these stored words
  rather than re-evaluating wind, so this is what the plant is actually doing; a second CPU model
  would drift from the shader and lie exactly when it mattered.
  EXPLICIT AND ONE-SHOT, never per frame: the buffer gained `TRANSFER_SRC` and the capture idles the
  queue through the new `Device::one_shot_transfer`, which is affordable when a person asks a
  question and ruinous every frame — the constraint the Phase 15 no-per-frame-readback box also
  states. Already in: the Wind Vectors editor overlay, `sa sample-wind`, `renderedPhenotype` on the
  runtime plant wire, the vegetation bounds overlay.
  Covered by e2e `vegetation-mechanics`, validation-clean.
  NOT COVERED: turbulence spectra as a decomposed per-octave view (the octave count and roughness
  are authored and reported, but no per-octave breakdown exists), and a whole-field interaction
  capture as opposed to the per-instance samples above.)*

## Acceptance

- [x] Wind phase/frequency/amplitude scale plausibly with plant structure and are stable across
  residency, origin rebasing, and executor choice.
  *(STRUCTURE NOW ENTERS THROUGH AUTHORED RESPONSE, not height alone. `MechanicalResponse` reached
  `.splantc` and was read by NOTHING — the prepass derived everything from the plant's height, so a
  stiff sapling and a supple reed of the same height swayed identically whatever the author wrote.
  The chain is now four links: a part-table decoder (`mechanical_response`, whose absence was the
  gap), the family render load, the mirror's prototype record (the previously reserved four words,
  carrying the cooked INTEGER forms so the GPU reads exactly what the cooker wrote), and the
  prepass. Stiffness raises the branch frequency as sqrt(k) — the harmonic-oscillator relation —
  and divides the amplitude; drag scales the push; flutter scales the leaf term; damping bleeds
  amplitude; the bend limit caps it. A zero bend limit reads as unlimited, since a plant that may
  not bend at all is a prop rather than a bend limit.
  PHASE STABILITY IS UNCHANGED AND STILL BY CONSTRUCTION: the per-plant phase hashes the absolute
  world root (positions are signed level-zero cell ticks, and no rebasing exists to shift them), so
  residency churn and executor choice cannot move it.
  PROVED AT ALL FOUR LINKS, and the fourth needed care. Unit:
  `the_cooked_part_table_returns_the_authored_mechanical_response` (writer↔reader) and the mirror's
  `assembly_mesh_mirrors_its_parts_range_and_prototype_count` (record). e2e
  `vegetation-mechanics` reads the captured record back from the GPU. A first version of that test
  asserted only the reported response, which a shader that loaded the struct and ignored it would
  have passed — a mutation confirmed exactly that. The fixture authors stiffness and drag at 1.0, so
  they prove nothing by value; flutter is 0.25, and the flutter-to-branch amplitude ratio separates
  applied from ignored by 4x (0.15 against 0.60). The mutation now fails on it.
  Cook validation also parses the response, so an unparseable part table fails the cook rather than
  reaching the renderer as a plant that will not sway.)*
- [x] Depth/main/motion/current-shadow/selection deformation agrees vertex-for-vertex or by the defined
  aggregate error; no TAA ghost trail follows gusts or phenotype transitions.
  *(Agreement is by construction: every pass applies `gpuSceneWindDeform` on the same stored
  record with the same inputs — identical arithmetic, identical results vertex-for-vertex across
  depth, main, gbuffer, motion, point/fixed shadows, and the wireframe selection path. Motion
  reads the previous-time words exactly (recomputed wind, carried interaction, previous
  quadrature), and phenotype flips ride reactive-flagged transition records, so gusts and
  transitions leave no ghost trail; e2e frames over live wind + sources + flips run
  validation-clean.)*
- [x] Character/vehicle emitter recovery is damped and deterministic; persistent damage survives
  unload/reload while cosmetic bend does not pollute saves.
  *(Recovery is the fixed-constant damped oscillator (K=40, C=9, dt clamped) — deterministic for a
  given impulse sequence; cosmetic bend lives only in the GPU field and sway records, never in any
  save path. Disturbance masks are reducer-owned cell state that already survives unload/reload;
  Phase 12 routes confirmed damage events into them.)*
- [x] Distant vegetation continues moving within the appearance-error bound.
  *(BY CONSTRUCTION the aggregate-voxel branch applies the same stored sway as the triangle path —
  every raster pass adds `gpuSceneWindDeform` at its world-compose line, with no representation
  branch around it — so far plants keep the exact near-field motion.
  AN ATTEMPT TO MEASURE THIS FAILED AND THE TEST WAS DELETED, which is worth recording so the next
  attempt does not repeat it. `vegetation-distant-wind` forced the coarsest cut
  (`SAFFRON_CUT_OVERRIDE=coarse`), confirmed `voxelRecords > 0`, and measured consecutive-frame
  motion under a gale — and passed. It also passed with the aggregate branch's sway MUTATED TO ZERO.
  The counters say why: at that camera the frame draws 6 records, of which 1 is the aggregate voxel
  and 5 are micro-blade grass candidates. The motion being measured was the grass, and the plant
  contributed too few pixels to move the mean.
  THE MOTION IS NOW MEASURED, and the toggle that measurement needed exists.
  `SAFFRON_MICRO_FIELD=off` suppresses the reconstructed blade passes, so the frame contains the
  aggregate and nothing else that moves. `vegetation-distant-wind` pins the cut coarse, turns the
  blades off, and ASSERTS THE PREMISE FIRST — `voxelRecords > 0` and `microCandidates == 0` — so a
  run where the grass survived fails there rather than silently measuring it. A gale then moves the
  frame by 3.99 mean absolute difference where a still field moves it by 0.
  MUTATION-CHECKED, which is the thing the deleted version could not survive: returning zero from
  `gpuSceneWindDeform` fails the gale case at exactly 0.
  TWO SETUP TRAPS COST MOST OF THE EFFORT AND ARE WORTH RECORDING. The runtime query reports one
  plant at (1, 0, 1), and aiming the camera there frames NOTHING — the field scatters its canopy
  across the cell rather than putting it where a single query happens to report; the working
  framing is `vegetation-wind-visual`'s (16, 4, 22). And the camera must be set through
  `set-camera` AFTER the cook rather than through `prepareScene`, because residency is
  camera-driven. Both failures look identical to a frozen aggregate: the plant contributes zero
  pixels, `visible`/`records` still count it, and every frame-difference reads 0. Confirming that
  the TRIANGLE cut was equally frozen is what showed the fault was the harness rather than the
  aggregate branch.
  THE MODAL AGGREGATION IS NOW BOUNDED, which is the clause the note above left open.
  WHAT AGGREGATING TAKES AWAY IS THE POINT, not what it gets wrong standing still. A triangle cut
  swings each assembly use about its pivot and shimmers the leaf parts; an aggregate brick has no
  parts and applies neither, keeping only the whole-plant sway both representations share. That is
  the cheaper far state this clause asks for, and it means a distant plant moves LESS than a near
  one by a knowable amount. Unbounded, that difference is a plant visibly stiffening at the instant
  the cut coarsens.
  THE BOUND IS EXACT AT COOK TIME, which is not obvious and is what makes this measurable at all.
  Both amplitudes are derived from the sampled wind speed and then CLAMPED — `min(speed * 0.02,
  0.5)` for the branch mode, `min(speed * 0.012, 0.2)` for flutter — BEFORE the authored response
  scales them. The largest either can ever reach is therefore a property of the family alone, so
  `modal_aggregation_bound` computes a true supremum rather than a guess at a reference gust. It
  decodes the PACKED mechanics words, not the authored struct, so it reads the same bytes the GPU
  does, including the all-zero case the prepass answers from height alone.
  MEASURED, NOT ASSERTED. `compare_triangle_voxel_transitions` now renders the triangle side
  displaced by the saturated modal amplitude — ACROSS THE VIEW, the direction that moves a
  silhouette most — against the undisplaced aggregate, and takes the component-wise maximum with
  the still comparison. The projection is parallel, so displacing the geometry one way is the same
  as displacing every ray origin the other, and the framing stays on the node's bounds: geometry
  the displacement pushes out of frame reads as lost silhouette, which is what it is. The cook
  passes the family's bound, so every published plant declares an error that covers its own lost
  motion. `CookVersionSet.compiler` went 3 → 4: same schema, different values.
  WIDENING IS THE POINT, NOT A REGRESSION. The cut selector refines when the PROJECTED error
  exceeds its pixel threshold, so a wider declared error means the aggregate is chosen only farther
  away — exactly where the motion it drops is sub-threshold on screen. That is the box's sentence
  read literally: distant vegetation keeps moving, and stops moving only where the loss cannot be
  seen.
  PROVEN BY `the_modes_an_aggregate_drops_widen_its_declared_error` (`saffron-geometry`), which
  calibrates the same hierarchy twice — once with a zero bound, once with real modes — and asserts
  the modal pass is monotone (never narrower, since it only adds measurements) AND strictly wider
  somewhere, because a bound that changes no declared error is inert and proves nothing.
  Mutation-checked: dropping the displaced render from the comparison fails it on that second
  assertion by name.
  ONE HONEST LIMIT ON WHERE IT IS PROVEN. The cooked-plant test
  (`a_cooked_plant_declares_an_error_every_transition_fits_within`) re-measures with the same bound
  the cook used, so it asserts the containment on a real artifact — but it CANNOT discriminate the
  modal term, because the thin-sheet fixture's declared silhouette error already saturates at
  `u32::MAX` and everything is within that. The tetrahedron test is where the term is shown live.
  That saturation is itself the correct outcome for a comb of blades — an aggregate that reads as a
  slab where the triangles read as a comb should never be selected close up — but it makes the
  plant-level assertion the weaker of the two, and it is worth knowing which is load-bearing.)*
- [x] Tight swept bounds prevent HZB false occlusion and are available to VSM/RT phases.
  *(NO FALSE OCCLUSION IS POSSIBLE, and now with far less slack. Every bound in the chain is
  conservative by construction: the instance sphere carries `boundsInflation` (sway + interaction
  + mode amplitudes), and the node cull tests the cooked SWEPT extent — which contains the node's
  rest bounds, its authored deformation, and, since the cooker's closure, its whole subtree —
  widened by the same runtime slack. Nothing tests a bound tighter than the geometry can reach, so
  no HZB comparison can hide something visible; the parity sweep is the measurement.
  AVAILABLE TO EVERY VIEW, because it is the shared traversal that consumes them: the camera view,
  the survivor pass, and the shadow/page views all push their own `viewProj` through
  `SceneTraversalPush` and get the same per-node rejection. A view added later inherits it by
  construction rather than by porting.)*
- [x] Clouds/fog and vegetation sample the same global/local wind field and time.
  *(One authored source (`SceneEnvironment::wind`), one frame state (`SceneWind` + the frame's
  local-source ring), one clock (the monotonic simulation seconds): fog samples the composed field
  in-shader per froxel, clouds advect on its shear-scaled mean term, vegetation instances and
  blades deform from the composed field, and `sample-wind`/the vector overlay serve the same
  composition on the CPU seam.)*
- [x] Standard gate, platform validation/visual tests, and wind/phenology docs are green.
  *(The standard gate (build + shaders + clippy + suites + e2e validation-clean + docs 3×) is green
  at every slice seal on MoltenVK; wind-field/plant-rendering/persistent-gpu-scene/cloud docs are
  current. THE NVIDIA LEG IS GREEN TOO (`NVIDIA GeForce RTX 3070 Ti`): `just engine`,
  `just prepare-for-commit`, `just schema`, `just test`, and `just e2e` all EXIT=0, with every
  render-touching e2e file asserting `validationErrors()` empty.
  THE VISUAL TEST IS BUILT: `tests/e2e/vegetation-wind-visual.test.ts` cooks a real cell,
  waits for residency, and measures MOTION OVER TIME rather than calm-versus-gale — each wind state
  is sampled twice across the same settle and compared to itself, so the calm pair is the control.
  Measured: a still field gives **0.0001** mean absolute per-channel difference between consecutive
  frames, a gale gives **0.408** — a ratio near 3,500x. A third assertion returns the field to calm
  and requires stillness again, which is what would catch a deformation that latched at its last
  displacement. Two traps are recorded in the test: wind displaces only instances carrying
  `GPU_SCENE_INSTANCE_FLAG_WIND` (vegetation points alone, so a cooked cell is required), and the
  test must NOT enter play mode — play renders the scene's primary camera, so `set-camera` is
  ignored and every frame becomes the same picture of nothing.
  AMD was descoped by the project owner (2026-07-26): no such adapter exists for this project and
  none can be obtained. No AMD verification was performed and none is claimed.)*

## NO-LEGACY gate

Delete any plant WPO/wind material node or independent trample implementation if one exists by this
phase. Environment wind, local source components, the sampled field, and the deformation provider are
the only path.

