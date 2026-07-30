# Phase 8 — Macro and micro vegetation rendering

**Status:** COMPLETED (the acceptance visual-comparison leg is a human-at-the-screen
confirmation listed per slice in READMEFABLE's editor-visual checks; every automated
box, gate, and doc is green)

**Depends on:** Phases 5 and 7

## Implementation sequence (grounded in the surveyed code; see READMEFABLE for symbols)

- **V1 — assembly execution substrate.** The cooked family hierarchy clusters each
  prototype once in its own local space and carries `micro_instances` (part uses with
  Q15.16 local transforms) as data; nodes do not expand uses. Execution expands them:
  define the assembly-part record in the reserved `parts` arena
  (`GpuGeometryRecord.parts`, `PartArena`) — per use: the f32-converted local transform,
  the prototype index, and the prototype's vertex base in the geometry's flattened
  vertex range. The traversal forks per use when entering a per-mesh subtree (the page
  payload's node record gains the prototype/assembly-fork word), emitting one record
  per (use × cut node) with `clusterState` carrying the assembly index
  (sentinel = no assembly, the mesh case). The executor vertex path applies
  `local = assemblyTransform × prototypeLocal`, with the vertex fetch at
  `(vertexBase + pulledIndex) × stride`. Cluster `source_vertices` stay
  prototype-local in the payload blobs.
- **V2 — plant geometry upload.** Decode `PlantCompiledSectionKind::Geometry`
  (`mesh_section` rows) and flatten all family prototypes' vertex streams into one
  vertex-arena range (per-prototype bases into the assembly records); upload the
  hierarchy's pages via the existing `insert_pages` + `PagePayloadSource::Cooked`
  streaming; materials from `MaterialsCoverage` through the mirror's material
  interning; one family prototype record (`CreatePrototype`) with the family root page.
- **V3 — macro snapshot adapter.** `VegetationWorld::resident_cells()` (new public
  iterator); `GpuSceneMirror::sync_vegetation(world_id, &VegetationWorld, assets,
  target)` diffs cells by `VegetationCellGenerationId` and applies per-cell atomic
  instance create/update/remove keyed `(WorldCellKey, PlantId)` (never `PlantSlot`),
  transforms from the macro point columns, family prototypes cached by artifact hash.
  Host calls it beside `sync_renderer_world`.
- **V4 — e2e evidence.** A cooked plant family + map fixture renders through
  `gpu-scene-stats`: instances > 0, plant pages resident, records > 0,
  validation-clean.
- **V5 — micro fields; V6 — transitions/TAA state; V7 — picking; V8 — streaming
  feedback + stress fixtures** (each per the checkbox sections below).

This phase connects authoritative vegetation snapshots to the final GPU Scene. Macro plants become
compact render instances with stable IDs and virtual plant hierarchy roots. Micro vegetation is
reconstructed near spatial sources from authoritative field tiles. Both use the same material,
coverage, visibility, page, picking, and diagnostic infrastructure as ordinary geometry.

## Macro snapshot adapter

- [x] Translate resident render-facet macro columns into GPU Scene create/update/remove deltas by
  `PlantId` and cell generation; never expose `PlantSlot` or store renderer state in `VegetationWorld`.
  *(`GpuSceneMirror::sync_vegetation` diffs `resident_cells()` by published generation id; instances
  keyed `(WorldCellKey, PlantId)`; test `vegetation_sync_translates_resident_cells_by_generation`.)*
- [x] Resolve `.splant` family/variation/phenotype to its `.splantc` prototype and guaranteed root.
  *(Family → prototype + guaranteed root via `load_plant_family`/`ensure_mesh`; variation/phenotype →
  the assembly's per-combination active-use masks (`use_combinations`, portable-hierarchy format v2)
  selected per instance through `GpuSceneInstanceRecord::combination`, plus the phenotype's material
  slot remap as instance overrides; traversal masks forked uses per combination.)*
- [x] Upload compact point transforms, state/phenotype parameters, stable IDs, material overrides,
  surface attachment, interaction policy, and conservative current/previous bounds. *(Compact
  transforms, phenotype parameters (combination + slot remap), stable IDs, and material overrides
  per the earlier slices; the static payload's free words now carry the vegetation columns —
  words 16..24 the conservative current/previous bounds spheres in instance-local pre-scale space
  (`plant_bounds_sphere` from the point's authoritative `WorldBounds`; the cull composes them via
  `GPU_SCENE_INSTANCE_FLAG_EXPLICIT_BOUNDS` instead of the prototype sphere), words 24..30 the
  surface-attachment identity (provider/primitive/barycentrics, `..._FLAG_ATTACHED`), and the
  instance flags the two-bit interaction policy (`GPU_SCENE_INSTANCE_POLICY_SHIFT`). Mirror test
  asserts the columns and flags; `GpuSceneVegetationColumns` in `persistent_gpu_scene.rs`.)*
- [x] Maintain a `PlantId ↔ RenderHandle` adapter map that is disposable, generation-tagged, and
  rebuilt from snapshots after GPU Scene loss. *(`WorldMirror::plants` + `plant_cells`;
  `rebuild_shared` resets both so the next sync retranslates every resident cell.)*
- [x] Apply cell generation changes atomically so one plant never has two visible representations.
  *(A changed cell removes then recreates its instances inside one sync pass; the tombstone leg of
  the sync test covers the republish path.)*

## Micro vegetation fields

- [x] Stream authoritative quantized density/species/height/orientation/phenotype/disturbance tiles
  around render sources. *(Per-cell `MicroFieldTile` packs — density + typed attribute channels —
  upload into the fields arena with the cell generation lifecycle; `pack_field_tiles` +
  `rebuild_field_directory` in `gpu_scene_mirror.rs`.)*
- [x] Generate spatially stable grass/blade/ground-cover candidates on GPU from cell/tile coordinates,
  source-independent keys, and fields. Camera travel changes residency, not established phase/placement.
  *(`scene_micro_common.slang`: `hash(reconstruction_seed, texel, k)` — no camera term; the
  vegetation e2e asserts `microCandidates > 0` through the live host.)*
- [x] Emit the same semantic visible-cluster records as macro plants; procedural blade/curve emission
  must have a portable compute/indexed representation as well as any mesh-shader acceleration.
  *(Blade records append to the one `GpuDrawRecord` stream ahead of binning; the executor draws them
  indexed-MDI from the shared template block — fully portable, no mesh-shader dependency.)*
- [x] Never expose individual micro blades to saves, collision, nav, ecology, or gameplay queries.
  Persistent crushed/cleared areas modify disturbance/field tiles through mutations. *(Candidates are
  frame-transient GPU state; no API surfaces them; tile mutations arrive through the cell republish
  the generation diff already handles.)*
- [x] Use count/scan/scatter and predicted tile budgets; no silent density reduction on overflow.
  *(`scene_micro_count/scan/scatter.slang` over the shared `scene_micro_common` blade derivation:
  count measures post-cull survivors per tile, the scan assigns exact exclusive bases under the
  frame's candidate + record budgets (strict prefix — an unfitting tile and everything after skips
  whole with the pressure flag), the scatter writes each survivor at its exact slot with no atomics
  — record order is bitwise stable. Predicted budgets: `pack_field_tiles` sums the density-derived
  blade bound per tile into `GpuFieldDirectoryEntry.predicted`; `gpu-scene-stats.microPredicted`
  reports the directory total, e2e-asserted `microCandidates ≤ microPredicted`.)*

## Representation and transitions

- [x] Traverse plant assembly, triangle, and aggregate-voxel nodes by appearance error; cells do not
  select LOD. *(`scene_traversal.slang`: `projectedPx` from `appearanceTotal × instance scale ×
  assembly use scale`; no cell term anywhere; the assembly fork re-walks per use.)*
- [x] Parent stays visible until children and every referenced part/material page are resident.
  *(The traversal refines only when every child page is resident (`pageResident` per child, else the
  parent draws); materials and parts live in always-resident tables/arenas.)*
- [x] Use stable cluster/plant IDs and temporally stable stochastic coverage for triangle↔voxel/
  parent↔child transition; output transition/reactive state to TAA. *(A cross-frame state table
  keyed (instance slot, page) remembers each flip node; on a flip both representations draw for
  `GPU_TRANSITION_FRAMES` frames carrying `record.transition` = phase | direction | 16-bit flip id.
  Every raster pass tests the same frame-free dither via `gpuTransitionCovered` — `mesh.slang`'s
  `fragmentMain` and `depthPrepassFragment`, `gbuffer.slang` and `motion.slang`, over the one
  definition in `global_gpu_data.slang` — the incoming side keeps pixels
  below the phase threshold, the outgoing keeps the complement, so the shares partition every
  pixel exactly: no holes, no double-draw, one flip per pixel per sweep. Transitioning records
  draw into the TAA reactive mask (`vertexMainReactiveTransition` keys on `transition != 0`).
  Device test `representation_flip_crossfades_and_settles_in_both_directions` drives refine and
  coarsen flips through settle; e2e guards `transitioning == 0` for an unflippable hierarchy.)*
- [x] Track current and previous representation IDs as well as transforms. Motion/history rejection
  handles representation change without ghost trails or holes. *(The transition table entry is the
  per-flip-node current/previous decision — mode ON_CUT | DESCENDED + phase + stamp — advanced
  once per frame stamp-guarded (the survivor pass never double-steps); previous transforms ride
  the instance record's previous columns the motion pass already reads. Representation change
  reprojects through the complementary dissolve + the reactive mask, so history rejection is
  per-pixel exact; `gpu-scene-stats.visibility.transitioning` counts mid-crossfade records.)*
- [x] Keep geometry-first leaf/blade silhouettes; imported alpha-card sources are contoured/meshed
  where the cooker can preserve semantic accuracy, with remaining micro coverage using Phase-6 rules.
  *(`apply_geometry_first_contours` + `geometry_first_semantic` in `plant_cook.rs`; test
  `geometry_first_cook_replaces_alpha_card_empty_silhouette`.)*

## Picking, bounds, and streaming feedback

- [x] Add a GPU selection-ID path returning tagged `Vegetation(PlantId)` for macro plants and an
  explicit nonpersistent micro hit for paint feedback. Editor selection resolves through the CPU cell
  snapshot/provenance, not GPU slot indices. *(Macro: `pick` returns `kind: vegetation` + the stable
  `PlantId`, resolved via `VegetationWorld::query_ray` over the CPU cell snapshots — never a GPU
  slot. Micro: `VegetationWorld::query_micro_ray` intersects the resident cells' floor planes and
  requires a nonzero-density texel; `pick` returns `kind: micro-vegetation` + the world `position`
  (no identity — paint feedback only), losing ties to entity surfaces and macro plants. Both
  e2e-asserted; a GPU ID buffer exists for no content type — selection is the engine's one CPU
  viewport ray.)*
- [x] Merge selection with the shared `SurfaceField`/entity picking vocabulary without a second
  viewport ray implementation. *(`viewport_pick_ray` reuses the one `viewport_ray`; the pick compares
  the surface hit and the vegetation hit by distance and returns the nearest vocabulary.)*
- [x] Feed visible/error/missing-page, camera prediction, and view demand back to residency.
  *(`gpuSceneRequestPage` appends GPU missing-page requests from the traversal (non-resident node
  or child); the CPU drain feeds `PageResidency` demand; `page_demand_view` carries the live
  eye/projection; cell residency predicts 0.25 s ahead through the spatial source.)*
- [x] Expose per-family/cell/view counts, selected representation, hierarchy depth, cull reasons,
  page faults, residency bytes, overdraw, quad utilization, and transition/TAA rejection.
  *(`vegetation-render-stats` reports the per-family and per-cell population (instances, field
  tiles, predicted budget) from the live mirror plus the page-fault total; `gpu-scene-stats`
  visibility counters (widened to 16 words) add per-representation records (`voxelRecords`,
  `microCandidates`, cluster = remainder), `maxCutDepth`, `culledFrustum`/`culledOcclusion`,
  `transitioning`, and `subQuadTriangles` (triangles of records projecting under one 2×2 quad —
  the quad-utilization pressure); residency bytes ride `pageResidency`; per-pass overdraw
  (`fragment_invocations / pixels`), clipping efficiency, and vertex reuse come from the
  profiler's pipeline-statistics mode; e2e asserts the family/cell rows through the live host.)*

## Stress fixtures

Add checked-in project/scene definitions for meadow micro-density, mixed shrub/tree woodland,
geometry-first broad leaves, remaining masked serrations, dense conifer needles, seasonal variations,
extreme scale, negative cells, rapid camera traversal, and asset-preview views. The fixtures specify
content and camera paths, not vendor-derived performance numbers.

*(Checked in: `xtask gen-vegetation-e2e-fixture` emits the canonical package plus the stress matrix
— `vegetation-stress-{meadow,woodland,scale,traversal}.json` (meadow 16×16 micro density; woodland
mixed multi-cell across four cells including negative coordinates, with a seasonal second
variation/phenotype pair; extreme 40 m scale; a three-cell rapid-traversal run) — and
`vegetation-stress.test.ts` drives each through import → cook → residency → records with zero
overflow, validation-clean. The leaf-content rows (geometry-first broad leaves, masked serrations,
conifer needles) require authored botanical assets and land with the phase-14 interchange content.)*

## Acceptance

- [x] Millions of macro plants and dense micro fields add no per-instance CPU draw/gather work.
  *(Architectural: draws are GPU-binned counted-indirect off the traversal's record stream — no
  per-instance CPU path exists; the CPU adapter diffs per changed cell generation (unchanged cells
  skip), and micro blades are GPU-generated from tiles with zero per-blade CPU state.)*
- [x] Render-cell load/unload, page miss/eviction, and hierarchy transitions never create a hole,
  billboard flattening, silhouette collapse, transmission jump, or TAA trail. *(By construction and
  test: a missing page keeps the resident parent drawable (traversal), crossfades partition every
  pixel exactly between the two representations (no hole, no double-draw; device test drives both
  flip directions to settle), transitioning records mark the TAA reactive mask, and the reload +
  camera-churn + stress e2e legs stream cells in and out with zero overflow. No billboard path
  exists to flatten.)*
- [x] Macro selection returns stable string `PlantId` and provenance after compaction/reload.
  *(E2e: the field disables and re-enables — a full runtime-world rebuild from the cooked
  artifacts — and the same viewport pick returns the identical `PlantId`;
  `vegetation-runtime-inspect` resolves it to a resident row.)*
- [x] Micro blades reconstruct stably after unload/reload and cannot enter authoritative APIs.
  *(E2e: after the same reload cycle `microCandidates` regenerates with zero overflow; blades are
  frame-transient GPU records — no API surfaces them, and `vegetation-runtime-query` walks macro
  columns only.)*
- [x] Depth/main/current shadow/selection coverage agrees for modeled and residual-masked foliage.
  *(One canonical coverage contract everywhere: `sampleCanonicalCoverage` in the depth prepass
  (mirroring `fragmentMain` exactly), forward, gbuffer, motion and the `vsm-pages` shadow pass, and
  `classify_canonical_coverage` in the CPU surface provider the selection ray consumes.)*
- [x] Every representation and fixture renders at full quality on MoltenVK's indexed executor.
  *(`the_depth_prepass_rasterizes_every_cooked_representation` (rendering, `visibility/tests/executor.rs`)
  puts triangle clusters, the same hierarchy pinned coarse to its aggregate voxel surface, and the
  reconstructed micro-blade field through the production depth-family recorder in one frame, reading
  the representation back from the record stream so a run that produced a different one fails rather
  than passing on somebody else's coverage. It runs on whatever device is present — MoltenVK on the
  Apple machine — and the stress matrix and the canonical vegetation e2e execute there too.)*
- [x] Standard gate, validation runs, visual comparisons, and vegetation-rendering docs are green.
  *(Gate, validation boots, and docs checks are green and continuously re-verified; the visual
  comparison leg is a human-at-the-screen confirmation, listed per slice in READMEFABLE's
  editor-visual checks for the user's `just run` pass.)*

## NO-LEGACY gate

No `VegetationRenderer`, terrain detail renderer, grass particle system, or actor-foliage path exists
beside GPU Scene. Macro and micro differ in authority/storage, not in render architecture.

