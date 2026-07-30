# Audit — did the planset actually land?

**Status:** COMPLETED (audit of 2026-07-29)

Every box in every phase file is ticked and every phase header says COMPLETED. This file records
what an independent read of the code found instead. Nine auditors took the phase files apart
claim by claim and looked for the symbol, test, control command, schema field or docs page that
proves each one; a tenth checked the cross-cutting README claims. Only problems are listed. A
confirmed box appears nowhere below.

The short answer: the code is substantially built. Most of what follows is drift in the record
rather than absent implementation — stale test counts, boxes that contradict themselves,
superseded notes left sitting under newer ones, and plan prose naming symbols that no longer
exist. Ten claims are genuinely unbuilt and five describe a superseded path still alive beside
its replacement, which this repository treats as not done.

## Per-phase count

Confirmed boxes over total, as judged against the tree.

| Claim group | Confirmed | Verdict |
|---|---|---|
| phase-1-spatial-numeric-foundation | 29 / 34 | mostly-complete |
| phase-2-domain-assets-mutations | 22 / 25 | mostly-complete |
| phase-3-graph-determinism | 29 / 31 | mostly-complete |
| phase-4-cooker-cell-artifacts | 20 / 23 | mostly-complete |
| phase-5-runtime-cells-persistence | 23 / 28 | mostly-complete |
| phase-6-virtual-geometry-render-substrate | 29 / 32 | mostly-complete |
| phase-7-gpu-scene-visibility-cutover | 20 / 26 | mostly-complete |
| phase-8-vegetation-rendering | 23 / 26 | mostly-complete |
| phase-9-editor-authoring-debug | 18 / 21 | mostly-complete |
| phase-10-wind-deformation-phenology | 26 / 31 | mostly-complete |
| phase-11-virtual-shadows-lighting-rt | 18 / 24 | mostly-complete |
| phase-12-interaction-physics-queries-nav | 18 / 26 | mostly-complete |
| phase-13-ecology-catchup | 22 / 28 | mostly-complete |
| phase-14-botanical-authoring-interchange | 22 / 25 | mostly-complete |
| phase-15-production-platform-closure | 16 / 20 | partial |
| (a) One ownership model — README Outcome bullets, precedence chain, canonical-data table | 14 / 15 | mostly-complete |
| (b) Forbidden constructs — the planset's "does not exist anywhere" list | 12 / 12 | complete |
| (c) DAG tripwires — AGENTS.md invariants | 3 / 3 | complete |
| (d) Docs currency — pages + hub _index.md rows for every vegetation concept | 18 / 21 | mostly-complete |
| (e) AGENTS.md Status section vs the tree | 6 / 11 | partial |
| (f) Planset self-report consistency — README.md / READMESUMMARY.md internal coherence and gate evidence | 4 / 6 | mostly-complete |

## Findings

### Superseded path still alive (5)

**phase-4-cooker-cell-artifacts** — Content-addressed cook graph: "A source edit invalidates exactly intersecting downstream cells plus declared support halos/ancestor dependencies."

Two mechanisms exist for one concern and one of them is dead. `CookGraph::invalidated_nodes` is
the API that literally names the behaviour, but a repo-wide grep finds exactly two references:
its own definition and its own unit test. It is never called from any production path, from
`saffron-control`, from the cooker, or from the editor. Real invalidation is cook-key
recomputation in `vegetation_cooker.rs` (`previous_node.cook_key != cook_key ||
previous_node.dependencies != …`). Worse, `invalidated_nodes` does not implement the cited
semantics: it is a pure reverse-edge BFS that ignores `CookDependency::bounds` and
`CookDependency::halo` entirely, so it would invalidate every downstream node regardless of
spatial intersection. Because `saffron-vegetation`'s `lib.rs` glob-re-exports the `cook` module,
this is published crate API. Per the repo's NO-LEGACY rule this dead second path should have
been deleted with the cutover to cook-key comparison.

*Evidence:* engine/crates/vegetation/src/cook.rs:866 `pub fn invalidated_nodes` (bounds/halo never read, lines 870-892); only caller engine/crates/vegetation/src/cook.rs:1218 (`invalidation_follows_exact_node_edges`); live mechanism engine/crates/assets/src/vegetation_cooker.rs:246-252

**phase-6-virtual-geometry-render-substrate** — Add count→scan→scatter building blocks and overflow reporting. No visible list, bin, page request, or work queue silently truncates; a coarser resident parent remains drawable under pressure.

Two independent implementations of the same idea exist side by side. The production
count→scan→scatter is the GPU binning chain (`scene_bin_count/seed/scatter.slang` driven from
`visibility.rs`), whose overflow words and eviction/guaranteed-root rules do satisfy the
substance of the box. But `engine/crates/rendering/src/count_scan_scatter.rs` adds a second,
CPU-side `CountScanScatterPlan`/`CountScanScatterOutcome`/`CountScanScatterOverflow` API that is
publicly re-exported from `lib.rs` and has ZERO callers anywhere in the workspace — only its own
unit tests. It is a stubbed parallel path that duplicates an existing purpose, which this repo
treats as not-done rather than as harmless dead code.

*Evidence:* engine/crates/rendering/src/count_scan_scatter.rs:63 `pub struct CountScanScatterPlan`; engine/crates/rendering/src/lib.rs:94-95 re-export; `grep -rn 'CountScanScatterPlan|CountScanScatterOutcome|CountScanScatterOverflow' engine/` returns only count_scan_scatter.rs and lib.rs. Production overflow lives in engine/crates/rendering/src/visibility.rs:40/:44 counter words and page_residency.rs `GPU_PAGE_PAYLOAD_FLAG_GUARANTEED_ROOT`

**phase-6-virtual-geometry-render-substrate** — Focused verification: The portable executor can render every cooked representation in an isolated test on MoltenVK. (second, superseded executor depth path)

The isolated executor test renders through a shader pair that exists only for that test.
`engine/assets/shaders/scene_executor_depth.slang` and `scene_executor_depth_mesh.slang`,
together with `Pipelines::request_scene_executor_depth` / `request_scene_executor_depth_mesh`,
have no production caller — the sole call sites are inside `#[cfg(test)]`. Production's executor
depth path is the `vertexMainExecutor` entry of `mesh.slang` via
`request_depth_prepass_executor`. So two executor depth paths ship side by side, and the one the
verification box relies on is not the one production runs — notably it performs no canonical-
coverage classification at all (only a `gpuTransitionCovered` discard), whereas
`mesh.slang::depthPrepassFragment` does.

*Evidence:* engine/crates/rendering/src/pipelines.rs:1553 `request_scene_executor_depth` / :1675 `request_scene_executor_depth_mesh`, called only from engine/crates/rendering/src/visibility.rs:3348 and :3354 (both inside `mod tests`); engine/assets/shaders/scene_executor_depth.slang:84 `fragmentMain` has no coverage sample; production path engine/crates/rendering/src/renderer.rs:6226 `request_depth_prepass_executor` → engine/assets/shaders/mesh.slang:313 `depthPrepassFragment` (calls `sampleCanonicalCoverage`)

**phase-7-gpu-scene-visibility-cutover** — Acceptance: "No old gather/batcher/env toggle symbol remains (the draw-path tripwire is step 4b of `tools/ci/check.sh`)"; and the Rehome section's "Then delete `DrawItem`, `DrawBatch`, `SceneDrawList`, …".

`SceneDrawList` is named explicitly in the plan's own delete list and still exists under that
name, and the tripwire's grep pattern omits it — so the gate is constructed around the surviving
symbol rather than catching it. Separately, the retired `SAFFRON_MESH_SHADER` raster toggle has
been replaced by `SAFFRON_MESH_EXECUTOR`, a functionally equivalent environment toggle that
selects a raster path; the tripwire greps the old name only, so the class of thing the box says
is gone is back under a new spelling. The type is genuinely repurposed (deformation + tess-seam
state, not draws), so this is a naming/gate-coverage problem rather than a live legacy draw
list, but the box's wording claims more than the tripwire enforces.

*Evidence:* engine/crates/rendering/src/draw_list.rs:348 `pub struct SceneDrawList`; tools/ci/check.sh:162 pattern `DrawItem|DrawBatch|submit_draw_list|gather_static_draw_list|record_scene_draw_list|record_transparent_draw_list|SAFFRON_MESH_SHADER|MeshletRaster|record_meshlet_draws` — no `SceneDrawList`, no `SAFFRON_MESH_EXECUTOR`.

**phase-7-gpu-scene-visibility-cutover** — NO-LEGACY gate: "There is exactly one production scene-render path after this phase" / checkpoint: "every raster pass body records the executor draws".

A second production draw path survives for displaced instances: a CPU-built, one-entry-per-
instance list of `TessSceneDraw` rows, each carrying its own vertex-input PSO
(`request_mesh_pipeline`, not the executor family), cloned into and replayed by the depth-
prepass, scene, gbuffer and motion pass bodies alongside the executor buckets, and folded into
`stats.draw_calls`. This is per-instance CPU draw-list construction of the kind the phase
declares deleted — the traversal even skips `GPU_MATERIAL_TABLE_FLAG_TESSELLATED` records to
make room for it. The plan discloses the seam in prose, but the acceptance box and the NO-LEGACY
gate are ticked as though one path exists.

*Evidence:* engine/crates/rendering/src/renderer.rs:2270 `list.tess_draws.push(crate::TessSceneDraw { pso: self.pipelines.request_mesh_pipeline(&item.material, false, self.wireframe), base_instance: row as u32, … })` per displaced instance; replayed at renderer.rs:8596/8621 (depth-prepass), 8779/8814 (scene), 10345/10371 (gbuffer), 11107/11133 (motion); stat fold at renderer.rs:8055.

### Not built (10)

**phase-4-cooker-cell-artifacts** — Acceptance: "Editing one bounded field/source invalidates only its dependency region and declared halos."

No test enforces the scoping. The cooker's six unit tests cover repeat/expand cooks, cache
deletion, staged cancellation, staged-input change and the transaction journal codec — none
edits a bounded field or anchor chunk and then asserts that a non-intersecting cell stayed a
cache hit. No e2e test asserts on cook cache statistics at all (a grep for cacheHit/cacheMiss
across tests/e2e returns only a `cacheHit: false` assertion on the read-back manifest, which is
a deliberately zeroed field). `vegetation-graph.test.ts` commits an anchor-override chunk and
recooks, but only over the single cell CELL, and asserts only that the manifest identity changed
— the opposite direction from the claim. The scoping does exist by construction (per-cell
dependencies intersect `cell.bounds()` against the halo-expanded `read_bounds`), but nothing
locks it, so a regression to a whole-map dependency set would pass the entire suite.

*Evidence:* engine/crates/assets/src/vegetation_cooker.rs:2556-2763 (the complete cooker test module); engine/crates/assets/src/vegetation_cooker.rs:1877-1883 (`bounds_intersect(cell.bounds(), read_bounds)` — the untested scoping); tests/e2e/vegetation-graph.test.ts:914-926

**phase-6-virtual-geometry-render-substrate** — `.splantc` render artifact: Complete the derived plant TOC with … texture mip/KTX2-derived payloads where applicable …

Everything else this paragraph enumerates is present and verifiable (16
`PlantCompiledSectionKind` variants covering part/prototype, triangle hierarchy, voxel
hierarchy, materials/coverage, deformation, page directory, RT derivation, provenance, plus
platform profile / source hashes / checksums / the pinned zstd profile). KTX2 is the one item
with no implementation anywhere: `grep -rni ktx engine/crates/{geometry,assets,rendering}/src`
returns nothing in the whole tree. The paragraph carries no checkbox and hedges with 'where
applicable', so this is scope drift rather than a falsely ticked box — but the phase reads as
complete on a contract it does not implement.

*Evidence:* engine/crates/vegetation/src/artifact.rs:163-198 `PlantCompiledSectionKind` (no texture-container section); no `ktx`/`KTX2` symbol anywhere under engine/crates

**phase-8-vegetation-rendering** — Every representation and fixture renders at full quality on MoltenVK's indexed executor. *(The `moltenvk_renders_every_cooked_representation_through_indexed_draws` device test plus the stress matrix ... all execute on MoltenVK)*

The device test named as the primary evidence for this acceptance box does not exist anywhere in
the repository. There is no `portable_executor` module in `saffron-rendering` either (the plan's
own verification recipe in READMEFABLE.md invokes `portable_executor::moltenvk_renders_...`).
The box is ticked on a test that was never written, or was written and removed without updating
the box. The remaining evidence (the stress matrix, the canonical vegetation e2e) is real but is
not MoltenVK-specific — nothing in the tree pins the four representations to an indexed-
executor-only device.

*Evidence:* `grep -rn "moltenvk_renders" --include="*.rs" --include="*.ts" .` returns only plans/foliage-veg/phase-8-vegetation-rendering.md:203 and plans/foliage-veg/READMEFABLE.md:6775. `grep -rn "portable_executor" --include="*.rs" .` returns nothing. The only MoltenVK symbol in rendering is engine/crates/rendering/src/device.rs:277 `is_molten_vk()`.

**phase-8-vegetation-rendering** — Stress fixtures: "Add checked-in project/scene definitions for meadow micro-density, mixed shrub/tree woodland, geometry-first broad leaves, remaining masked serrations, dense conifer needles, seasonal variations, extreme scale, negative cells, rapid camera traversal, and asset-preview views."

Four of the ten named fixture categories are checked in. The three leaf-content rows (geometry-
first broad leaves, masked serrations, dense conifer needles) were explicitly deferred on the
phase note to "the phase-14 interchange content" — but phase 14 is itself marked COMPLETED and
those fixtures still do not exist anywhere in the tree. An asset-preview-view stress fixture is
also absent. Under this repo's rules a deferral recorded in a plan note is not done; the phase
is marked COMPLETED with the deferral unresolved by its own successor.

*Evidence:* tests/e2e/fixtures/ contains only vegetation-stress-{meadow,woodland,scale,traversal}.json plus vegetation-canopy.json (`grep -rn '"stress"' tests/e2e/fixtures/*.json`). Repo-wide `grep -rln -i "serration|conifer|broadleaf|broad-leaf" --include=*.json --include=*.ts --include=*.rs .` finds no fixture content. plans/foliage-veg/phase-14-botanical-authoring-interchange.md:3 reads `**Status:** COMPLETED`.

**phase-9-editor-authoring-debug** — Dock/graph/store/client unit tests, editor E2E, standard gate, and authoring docs are green.

No docs page describes the Vegetation authoring surface this phase builds. The `ui-and-editor`
explanations hub carries a page per editor surface (hierarchy-panel, inspector, physics-panel,
profiler-panel, script-logs-panel, undo-redo, selection, dock-system…) but none for Vegetation
mode, the tool palette, the brush model, the layer list, the cook/review sections, or any of the
eleven registered vegetation panels — and there is no hub `_index.md` row for them. AGENTS.md
makes the matching explanation page plus its hub row part of "done" for a change that adds a
concept. The engine-side vegetation concepts are well documented (nine pages under geometry-and-
assets / scene-and-ecs / physics); it is specifically the phase-9 authoring UX that has no page.

*Evidence:* `ls docs/content/explanations/ui-and-editor/` — 25 pages, none vegetation. `grep -rn "Vegetation mode|VegetationPanel|vegetation panel|Vegetation panel" docs/content/` returns nothing. `grep -rn -i vegetation docs/content/explanations/ui-and-editor/*.md` hits only debug-visualization.md (overlay flags) and assets-panel-and-thumbnails.md (import/thumbnail rows).

**phase-13-ecology-catchup** — Consume a monotonic world simulation clock and keep `ecology_tick` independent of the existing calendar/phenology time.

Nothing in the engine consumes a world simulation clock to drive ecology.
`VegetationWorld::advance_ecology` has exactly one caller in the whole tree — the `vegetation-
advance-ecology` control command — which takes `targetTick`, `maxTicks`, `water`, and `warmth`
from the request. There is no call from `RuntimeSession::synchronize_vegetation`, from the host
layer, or from `saffron-player`. Two dead seams confirm the wiring was never finished:
`VegetationTelemetry::record_ecology_ticks` has zero production callers, and
`VegetationStage::Ecology` appears only in its own match arm — so the `ecologyUs` /
`ecologyTicks` telemetry the `sa` formatter prints is permanently zero. Biology only ages when a
human presses Run in the Ecology Timeline panel or invokes the CLI; it does not advance with the
world.

*Evidence:* engine/crates/control/src/commands_vegetation_runtime.rs:489 (sole advance_ecology caller); `rg advance_ecology engine/crates/runtime/src engine/crates/host/src engine/crates/player/src` → empty; engine/crates/runtime/src/vegetation_telemetry.rs:144 record_ecology_ticks (only caller is its own unit test at :234); `rg 'VegetationStage::Ecology'` → only vegetation_telemetry.rs:94

**(d) Docs currency — pages + hub _index.md rows for every vegetation concept** — AGENTS.md keep-current rule: "a change that adds/alters an engine concept updates the matching explanation page under docs/content/ and its hub _index.md row" — applied to phase 9 ("the dedicated Vegetation mode, plant and biome asset workspaces, tiled brush/layer tools, graph editing, transactions, undo, rejection diagnostics, provenance, and cost overlays")

Phase 9's entire editor deliverable is undocumented on the docs site. There is no page for the
Vegetation mode, its floating viewport toolbar, the brush/layer painting transactions, or any of
the eleven registered vegetation/plant/biome panels, and no other page mentions them. Every
comparable panel family has a ui-and-editor page (physics-panel, material-graph-live-preview,
profiler-panel, script-logs-panel, metrics-dashboard, environment-and-presentation-panels), so
this is an omission rather than a documented convention.

*Evidence:* `rg -ni "vegetation panel|vegetation mode|plant graph panel|biome graph panel|ecology timeline" docs/content/` returns nothing. Built: editor/src/components/dock/panelRegistry.tsx registers `vegetation`, `vegetationTelemetry`, `ecologyTimeline`, `vegSummary`, `plantGraph`, `plantWind`, `plantAtlas`, `plantHierarchy`, `plantSeason`, `plantProxies`, `biomeGraph`; editor/src/panels/VegetationViewportToolbar.tsx ("The Vegetation mode's floating viewport toolbar"), VegetationAssetWorkspace.tsx, vegetationPainting.ts. docs/content/explanations/ui-and-editor/ holds 25 pages, none vegetation.

**(d) Docs currency — pages + hub _index.md rows for every vegetation concept** — the components reference tracks the registered component set

docs/content/reference/components.md states that it lists "the built-in components the ECS world
holds ... the registered set is `BUILTIN_COMPONENT_NAMES` in `registry.rs`", then omits four of
the twenty-five — including `VegetationField`, the planset's one scene-level component, and
`WindSource`, phase 10's placeable wind influence. `FogVolume` and `Morph` are also absent. The
explanation page (scene-and-ecs/built-in-components.md:37) does name both vegetation entries, so
only the field-level reference table is stale — but that table is the canonical per-field
listing a caller would consult for `VegetationField { map, enabled }`.

*Evidence:* engine/crates/scene/src/registry.rs:421-448 `BUILTIN_COMPONENT_NAMES` (25 entries incl. VegetationField, WindSource, FogVolume, Morph) vs docs/content/reference/components.md — `grep -c '`VegetationField`'` → 0, same for WindSource, FogVolume, Morph. Component definition: engine/crates/scene/src/component.rs:656.

**phase-15-production-platform-closure** — Phase status "COMPLETED"; three whole requirement sections — "Future networking contract closure", "Determinism and failure matrix", "Performance closure" — carry named deliverables and ZERO checkboxes.

The phase file has 20 `- [x]` boxes and no boxes at all under these three headings, so their
scope is neither ticked nor tracked, yet the phase is marked COMPLETED. The networking section
is not merely untracked, it is unbuilt: `rg -i "late.join|checkpoint hash|interest
key|CellInterest|base-manifest handshake" engine/crates -g '*.rs'` returns nothing, so none of
"cell-interest keys/facets and exact base-manifest handshake/rejection", "deterministic late-
join state fixtures and periodic checkpoint hashes", or "local reconstruction of wind/micro bend
while only persistent macro state is transmitted" has an artifact. The determinism matrix has
partial ad-hoc coverage (`evaluator.rs:19414
worker_counts_and_cell_request_order_are_byte_identical`, `residency.rs:677
worker_count_cannot_change_canonical_result_bytes`, `coordinate.rs:772
origin_rebasing_preserves_identity`, several corrupt/truncated codec tests) but nothing for
disk-full/interrupted writes, cross-cell transaction competition, camera cut/teleport/resize, or
late join. The performance section asks for project-owned budgets across twelve axes and stress
worlds: the stress worlds exist (`tests/e2e/vegetation-stress.test.ts` + four checked-in
fixtures) but no budget is asserted anywhere — `rg -i budget tests/e2e/perf.test.ts` returns
nothing. A section with no boxes cannot fail an audit of boxes, which is exactly how this scope
disappeared.

*Evidence:* plans/foliage-veg/phase-15-production-platform-closure.md:126-138 (networking), :287-304 (determinism matrix), :401-408 (performance); no matches for the networking vocabulary in engine/crates; tests/e2e/perf.test.ts has no budget assertion

**phase-15-production-platform-closure** — "Mark every phase and this README `COMPLETED` only after the integrated destination is green."

The box is ticked, the phase header says COMPLETED, and plans/foliage-veg/README.md:3 says
COMPLETED — while the box's own body documents an unresolved, ~50%-reproducible GPU hang in the
control-schema contract run (`GPU submission 'frame 167' has been in flight 3s`, then
`ERROR_DEVICE_LOST` on `preview thumbnail render`), records that the bisection ruled out every
suspect, hands the next investigator a starting point ("it needs a capture with GPU-assisted
validation armed"), and closes with the literal sentence "UNTIL IT IS FIXED THE DESTINATION IS
NOT GREEN, whatever the box counts say." This is the one box whose entire purpose is to gate the
COMPLETED marks, and it was ticked against its own stated criterion. Nothing elsewhere in the
tree or the planset records the hang as fixed (the same signature is still open in phase-11's
notes and READMESUMMARY.md:281).

*Evidence:* plans/foliage-veg/phase-15-production-platform-closure.md:480-508; plans/foliage-veg/README.md:3; plans/foliage-veg/phase-11-virtual-shadows-lighting-rt.md:426; plans/foliage-veg/READMESUMMARY.md:281

### Claim does not match the code (28)

**phase-3-graph-determinism** — Progress log (2026-07-21) vs. the Acceptance box and the "Platform conformance" section

The same file states two contradictory things about the same fact. The Progress bullet says
"Physical NVIDIA and AMD CPU/Slang conformance records remain deferred verification work… the
deferred platform records stay visible and unchecked until the required hardware is available."
The Acceptance box and the Platform conformance section (both dated 2026-07-26) say NVIDIA
GeForce RTX 3070 Ti was verified with a checked-in record. The NVIDIA record does exist and its
digests do match the MoltenVK one, so the acceptance box is the accurate statement and the
Progress bullet is stale text never updated when the hardware run landed. An auditor reading the
plan hits the stale claim first.

*Evidence:* plans/foliage-veg/phase-3-graph-determinism.md:152-155 vs :105-114 and :131-140; benchmarks/foliage-veg/compute-conformance-nvidia-rtx-3070-ti.json (graphProgram.rustReferenceSha256 == slangSha256 == fb44dca4…, identical to the MoltenVK record)

**phase-1-spatial-numeric-foundation** — Add small seam/negative-coordinate/origin-rebase fixtures and a large heterogeneous forest specification used by later phases.

`SpatialFixture::canonical()` and `FOREST_BASELINE_SPEC` exist and are re-exported from
`saffron-spatial`'s lib.rs, but nothing in the workspace consumes them outside the module's own
`#[cfg(test)] mod tests`. `grep -rn "SpatialFixture|FOREST_BASELINE_SPEC" engine/ tests/ tools/`
returns exactly one hit outside fixture.rs — the re-export line. The 'used by later phases' half
of the box is false, and the later-phase stress work (`tests/e2e/vegetation-stress.test.ts`)
uses its own checked-in JSON fixtures (`vegetation-
stress-{meadow,woodland,scale,traversal}.json`) with no reference to the 64 km² / 10M-plant
baseline spec. `engine/crates/spatial/AGENTS.md` repeats the same false claim: 'fixture.rs holds
the shared seam cases both this crate and saffron-vegetation test against' — saffron-vegetation
never touches it.

*Evidence:* engine/crates/spatial/src/fixture.rs:19 `SpatialFixture::canonical`, :59 `FOREST_BASELINE_SPEC`; engine/crates/spatial/src/lib.rs:24 (only external reference); engine/crates/spatial/AGENTS.md:23

**phase-2-domain-assets-mutations** — Add `engine/crates/vegetation/` depending on `saffron-core`, `saffron-json`, `saffron-geometry`, and `saffron-spatial` only.

The crate additionally depends on `saffron-material` (path dep), plus `glam`, `sha2`, `zstd`.
The extra Saffron edge is deliberate and documented elsewhere (root AGENTS.md's DAG lists
`saffron-vegetation → {core, json, material, geometry, spatial}`, and the crate AGENTS.md
explains that vegetation re-exports `saffron_material::*` rather than restating surface/coverage
vocabulary), so the code is not wrong — the plan box is. The word 'only' was never updated when
the material crate was split out, leaving a ticked box that contradicts the shipped manifest.

*Evidence:* engine/crates/vegetation/Cargo.toml `[dependencies]` (saffron-core, saffron-json, saffron-material, saffron-geometry, saffron-spatial, glam, sha2, thiserror, zstd); AGENTS.md crate DAG; engine/crates/vegetation/AGENTS.md 'Value contracts only' bullet

**phase-5-runtime-cells-persistence** — Acceptance: Standard gate and runtime vegetation/persistence docs are green … the run exercises residency, the reducer, snapshot export/import, and the state baseline.

Three problems. (a) The measured numbers are out of date: the box records '328/328 across 51
files' but the suite now has 69 `*.test.ts` files, and '249 manifest-driven control checks'
against a manifest that now carries 252 commands. (b) The '249 checks' figure is much weaker
than it reads: 116 of the 252 manifest commands carry a per-command `skip` and are never
dispatched by `tools/check-control-schema/check.ts` — every single `vegetation-*` and `plant-*`
command is among them, yet the loop still pushes them onto the `checked` list. (c) The specific
claim that the run 'exercises … snapshot export/import' is not true of any harness in the tree:
`vegetation-state-export` and `vegetation-state-import` are `skip`ped in the manifest, appear in
no `tests/e2e/*.test.ts`, and the control crate's own tests only round-trip the hex helper,
never dispatch the commands. The underlying
`VegetationWorld::export_state_snapshot`/`import_state_snapshot` ARE unit-tested; the command
layer over them is not.

*Evidence:* schemas/control/command-manifest.generated.json — `vegetation-state-export` / `vegetation-state-import` both carry `"skip": "requires an exact bound vegetation runtime generation…"`; tools/check-control-schema/check.ts:694-697 `if (command.skip) { checked.push(...); continue; }`; engine/crates/control/src/commands_vegetation_runtime.rs:1297 `snapshot_hex_is_strict_and_roundtrips` (helper only); `ls tests/e2e/*.test.ts | wc -l` = 69

**phase-7-gpu-scene-visibility-cutover** — "the mesh path rasterizes the quad to 144 depth texels against the indexed path's 144 — an exact match".

The test does not assert an exact match and its own comment explains why it cannot: the mesh
path emits three vertices per triangle without deduplication, so shared-edge texels may resolve
either way. The assertion is a 2% proportional bound (`spread * 50 <= max`) plus a `> 100` floor
on each side. The named measurement is a one-off observation dressed as the test's guarantee.

*Evidence:* engine/crates/rendering/src/visibility.rs:3826-3836 — `assert!(spread * 50 <= written.max(mesh_written), "indexed and mesh executors disagree on coverage …")`, preceded by the comment "The bound is a proportion rather than an exact match".

**phase-7-gpu-scene-visibility-cutover** — Acceptance: "Standard gate and GPU Scene/visibility docs are green (docs sweep to the executor architecture: hugo, link, and style checkers all clean)".

The docs pages are substantial and mostly accurate, but the mesh-executor section describes
behaviour the code does not have: it says the second executor runs "where the device offers it",
when in fact it requires `SAFFRON_MESH_EXECUTOR=1` on top of the capability.
`SAFFRON_MESH_EXECUTOR` appears nowhere under docs/. The page's "In the code" row for the mesh
executor points at `scene_executor_depth_mesh.slang` (the test-only path) and omits
`mesh.slang`'s `meshMainExecutor`, which is what a shipped frame actually runs. Separately, the
"hugo, link, and style checkers" are not in the tree: `tools/ci/check.sh` steps 1-10 contain no
docs step and the justfile has only `run-docs`, so nothing reproduces that claim.

*Evidence:* docs/content/explanations/frame-and-render-graph/hierarchical-visibility.md:132 "A second executor reaches the same records through `VK_EXT_mesh_shader` where the device offers it."; :206 code-pointer row naming `scene_executor_depth_mesh.slang`; `rg SAFFRON_MESH_EXECUTOR docs/` returns nothing; tools/ci/check.sh steps at lines 99/110/117/128/135/157/172/197/208/219/232/245.

**phase-7-gpu-scene-visibility-cutover** — Acceptance / Active checkpoint numeric claims: "rendering 294, assets 260, control 104, e2e 304/304", "e2e 341/341", "e2e 332/332", "e2e 303/303".

The file cites four mutually inconsistent e2e totals for the same suite in four places, all
presented as the evidence that a box is closed. None is reproducible from the tree and at most
one can have been current. Test-count evidence of this shape decays silently; the boxes it backs
cannot be re-verified without a run.

*Evidence:* plans/foliage-veg/phase-7-gpu-scene-visibility-cutover.md lines 74 ("e2e 304/304"), 185 ("341/341"), 276 ("e2e 303/303"), 299 ("`just e2e` 332/332").

**phase-7-gpu-scene-visibility-cutover** — Checkpoint: "one-PSO depth family for prepass/shadow/point-shadow/G-buffer/motion".

There is no point-shadow raster pass in the renderer — shadows are the single `vsm-pages` pass,
and the point-shadow pass was retired (its removal is visible in the recent history as "drop the
retired point-shadow pass from the coverage test"). The claim, and the same wording in the
`record_executor_depth_family` doc comment, describe a pass that no longer exists. Cosmetic
relative to the other findings, but it is the kind of stale text that makes a box look broader
than it is.

*Evidence:* `rg 'RgPass::graphics("' engine/crates/rendering/src/renderer.rs` yields depth-prepass, depth-upscale, editor-overlay, gbuffer, grid, lit-wireframe, motion, reactive-coverage, scene, scene-survivors, seed, sky, stars, vsm-pages — no point/spot shadow pass; stale wording at engine/crates/rendering/src/scene_pass.rs:128.

**phase-11-virtual-shadows-lighting-rt** — Track BLAS/TLAS build/update/compaction time, memory, selected representation, OMM hit classes, and page demand without copying vendor performance thresholds.

The code is complete — this is a plan-prose defect, not a code gap. The note's lead sentence
says "FIVE OF SIX CLAUSES ARE BUILT; the box stays open on COMPACTION TIME alone", and its
"STILL OPEN" block names three unfinished clauses (build/update/compaction time, OMM hit
classes, page demand). All three are in fact built and on the wire, in paragraphs of the same
note. A reader auditing this box has to guess which half of the note is current, which is
precisely the failure mode a ticked box is supposed to remove.

*Evidence:* contradicted by engine/crates/rendering/src/renderer.rs:4336 `rt_accel_build_us` (fed by upload.rs:159 `accel_timestamp_pool` → resources.rs:62 `add_accel_build_nanos`), engine/crates/rendering/src/rt.rs `distinct_micromap_classes` → `ommMicromaps/ommOpaque/ommTransparent/ommUnknown` in protocol/src/dto.rs, and engine/crates/rendering/src/page_residency.rs:596 `SceneViewClass::ALL` / `page_demand_priority`

**phase-11-virtual-shadows-lighting-rt** — docs track the built behaviour (AGENTS.md keep-current rule) for the GI occluder path

The GI box's own change deleted `set_sdf_scene` outright (the CPU occluder upload was replaced
by `gi_occluder_scatter.slang`), but the docs page still names it as a live symbol in a `What |
File | Symbols` pointer table, and a doc comment in `lighting.rs` still describes the buffer as
"its prefix rewritten each frame by `set_sdf_scene`". `rg -n "fn set_sdf_scene" engine/` returns
nothing. Per AGENTS.md the docs page and the code comments are part of the same change, so the
box is not finished while both point at a deleted function.

*Evidence:* docs/content/explanations/global-illumination-and-raytracing/software-ray-trace.md:150 `| Occluder-list pressure | rendering/src/renderer.rs | set_sdf_scene, MAX_SDF_INSTANCES, sdf_instances_dropped |`; engine/crates/rendering/src/lighting.rs:517; `rg -n "fn set_sdf_scene" engine/` → no match

**phase-11-virtual-shadows-lighting-rt** — READMESUMMARY.md: "All fifteen phases complete; no boxes open."

The summary the orchestrator asked me to check against the code is internally contradictory and
its hardware-gated section is badly stale. Its header table says phase 11 is 24/24 with no boxes
open; forty lines later the same file says of the RT telemetry box "That box stays open on its
time, OMM-hit-class and page-demand clauses", says "Derivation stays open", lists "Shared
compacted BLAS, deformation materialization, voxel clusters as AABBs, the KHR any-hit baseline,
OMM derivation, VK_NV_cluster_acceleration_structure, BLAS/TLAS tracking, and the any-hit/OMM
parity acceptance all need building", and claims "`render-stats` reports `blasCount: 0` while
`rtInstances: 2`, so that counter is not wired". Every one of those is now built in the tree.
Anyone using this file as the map (which is what it says it is) would be misled in both
directions at once.

*Evidence:* plans/foliage-veg/READMESUMMARY.md:27 vs :33-51; contradicted by engine/crates/rendering/src/rt.rs:332 `blas_count` (derived from distinct AS device addresses), upload.rs `compact_mesh_blas` / `build_aggregate_blas` / `build_cooked_micromaps`, rt_cluster.rs + vk_nv_cluster.rs, plant_cook.rs:2415 `derive_family_micromaps`

**phase-10-wind-deformation-phenology** — Share one result with depth, main, motion, selection, fixed shadows, aggregate voxels, and later VSM/RT. No shader independently re-evaluates wind. ("every raster pass applies the stored record through `gpuSceneWindSway`")

The mechanism is real and correct — one prepass record, applied at seven call sites across mesh,
executor depth (both variants), gbuffer, motion (current and previous) and the wireframe
overlay, with the aggregate-voxel branch falling through to the same call. But the symbol the
box and two docs pages name, `gpuSceneWindSway`, does not exist anywhere in the tree; the live
function is `gpuSceneWindDeform`. AGENTS.md requires docs pointer tables to carry real symbols,
so both pages are currently unfollowable.

*Evidence:* engine/assets/shaders/global_gpu_data.slang:897 `public float3 gpuSceneWindDeform(...)`; `rg -n "gpuSceneWindSway" engine/` → no match; cited as live in docs/content/explanations/frame-and-render-graph/persistent-gpu-scene.md:140 and docs/content/explanations/scene-and-ecs/wind-field.md:157

**phase-8-vegetation-rendering** — Depth/main/current shadow/selection coverage agrees ... *(One canonical coverage contract everywhere: `sampleCanonicalCoverage` in the depth prepass ..., forward, gbuffer, motion, and point-shadow passes)*

The point-shadow leg of this claim is dead text: there is no point-shadow raster pass or shader
in the tree (only `cloud_shadow.slang`), and commit 67389f21 is literally titled "drop the
retired point-shadow pass from the coverage test". The enforcing xtask guard covers three
shaders, not the five the box names. The depth-prepass leg IS satisfied — via
`depthPrepassFragment` in mesh.slang, not via the `scene_executor_depth*` shaders the phase body
cites.

*Evidence:* engine/xtask/src/shaders.rs:1125 `geometry_passes_use_the_canonical_coverage_module` asserts `sampleCanonicalCoverage(` only in ["mesh.slang", "gbuffer.slang", "motion.slang"]. `rg -ln sampleCanonicalCoverage engine/assets/shaders/*.slang` → coverage.slang, mesh.slang, gbuffer.slang, motion.slang. mesh.slang:313-341 `depthPrepassFragment` does call it. No `*point*shadow*.slang` exists.

**phase-9-editor-authoring-debug** — *(Closing sweep: editor unit tests 425/425 ... full e2e 308/308 across 44 files (3007 expects) ...)*

The cited closing-sweep tally does not describe the tree as it stands: the e2e suite is 69 test
files, not 44. The numbers are a snapshot of a run that has since been overtaken, so they cannot
be used as evidence that the gate is green for the current tree. (The 23 editor unit-test files
are plausible for a 425-assertion count; only the e2e figure is checkably wrong.)

*Evidence:* `ls tests/e2e/*.test.ts | wc -l` → 69. `find editor/src -name "*.test.ts*" | wc -l` → 23.

**phase-9-editor-authoring-debug** — Plant preview can scrub variation, life stage, season, wind strength ... *(Season and life stage scrub through the same phenotype selector ... Wind strength scrubs live ... Open: season/wind scrubs ride phase 10 ...)*

The note on this ticked box contradicts itself: it opens by stating that season and wind scrub
live through `set-asset-preview-options` and `set-wind`, then closes with "Open: season/wind
scrubs ride phase 10 (they scrub systems that phase builds)". The implementation side is real
and verified — `set-wind` is wired from PlantWindPanel and PlantSeasonPanel exists — so the
"Open:" sentence is leftover text on a box that was later completed. Harmless to the code, but
it means the box's stated status cannot be read at face value.

*Evidence:* plans/foliage-veg/phase-9-editor-authoring-debug.md:108-126. Implementation confirmed: editor/src/panels/PlantWindPanel.tsx:51 `client.setWind(settings)`; `set-wind` present in schemas/control/command-manifest.generated.json; engine/crates/protocol/src/dto.rs:3974 `SetAssetPreviewOptionsParams` carries `variation`/`phenotype`; editor/src/panels/AssetEditorWorkspace.tsx:161 reads `entered.plantCombinations`.

**phase-12-interaction-physics-queries-nav** — Acceptance: Standard gate ... `just e2e` at 328/328 across 51 files, the first fully clean full-suite run

The recorded gate numbers no longer describe the tree: `ls tests/e2e/*.test.ts | wc -l` is 69,
not 51. The same box also contradicts the preceding acceptance box, which says the suite is
'green apart from the unrelated alpha_blend hang'. Per instructions I did not run the gate, so
the pass/fail claim itself is unverified; the file count is verifiably stale.

*Evidence:* tests/e2e/ contains 69 *.test.ts files; plans/foliage-veg/phase-12-interaction-physics-queries-nav.md:193 vs :215-220

**phase-13-ecology-catchup** — (same box) `EcologyClock` ... `ticks_to` refuses a target behind the clock and `complete` accepts only the exact successor

Neither symbol exists. `EcologyClock` exposes `at`, `tick`, `ticks_from`, and `advance_to`;
there is no `ticks_to` and no `complete`. The guarantee itself holds, but through different
code: `advance_to` refuses a backwards target and `EcologyState::publish_region_tick` enforces
the exact-successor rule. The box cites named evidence that is not in the tree.

*Evidence:* engine/crates/vegetation/src/ecology.rs:36-71 (EcologyClock impl), :222-251 publish_region_tick; `rg 'fn ticks_to|fn complete' engine/crates/vegetation/src/ecology.rs` → empty

**(b) Forbidden constructs — the planset's "does not exist anywhere" list** — "no CPU foliage draw loop"; phase 7 "deletes the old static gather ... in the same phase"

The code invariant genuinely holds — `gather_static_draw_list`, `DrawItem`, `DrawBatch`,
`submit_draw_list`, `meshlet_raster.rs` and `meshlet.slang` are all gone, and tools/ci/check.sh
carries a tripwire that fails the gate if any of those identifiers reappears. What survives is
the prose: six doc comments still name a "draw-list batcher" that no longer exists, and one of
them asserts two live consumers where there is one. `wire_gathered_deformations` has exactly one
caller (renderer.rs:2226). This is the change-journey / retired-path narration AGENTS.md
forbids, and it is what a future reader would use to conclude a second batching path still
exists.

*Evidence:* engine/crates/rendering/src/instancing.rs:75 ("the per-frame instance + material storage and the draw-list batcher"), :117 ("shared by the draw-list batcher and the record-driven frame driver"), :159, :387 ("the shared consumer half behind both the draw-list batcher and the record-driven frame driver"); engine/crates/rendering/src/skinning.rs:80, :231. Tripwire: tools/ci/check.sh:158-170. Sole caller: engine/crates/rendering/src/renderer.rs:2226.

**(e) AGENTS.md Status section vs the tree** — "**Not yet:** ... the optional `VK_EXT_mesh_shader` executor"

The mesh executor is built end to end, runtime-selected, and covered by an image-parity e2e test
— it is not "not yet". It is a full second executor over the shaded übershader pass (not depth-
only), selected by `SAFFRON_MESH_EXECUTOR=1` on a device advertising a mesh stage, and reported
on `render-stats`. This also contradicts READMESUMMARY's own "still open on scope — depth-only
... and no runtime selection yet".

*Evidence:* engine/crates/rendering/src/renderer.rs:756 `mesh_executor` field, :1751 `device.capabilities.mesh_shader && std::env::var("SAFFRON_MESH_EXECUTOR") == Ok("1")`, :8029 `scene_mesh_dispatch`, :8769/:9131 pass wiring; engine/crates/rendering/src/scene_pass.rs:92 `record_executor_bucket_draw_mesh`; engine/crates/rendering/src/visibility.rs:2132/:2206; engine/crates/rendering/src/pipelines.rs:1675 `request_scene_executor_depth_mesh`; engine/assets/shaders/scene_executor_depth_mesh.slang; engine/crates/control/src/commands_render.rs:281 `mesh_executor: renderer.mesh_executor_active()`; tests/e2e/mesh-executor-parity.test.ts. AGENTS.md:367.

**(e) AGENTS.md Status section vs the tree** — "**Not yet:** ... hardware GPU in the toolbox"

The same file contradicts this at length ~250 lines earlier, and the planset's gate record was
taken on that hardware. The Status line is the stale half of a self-contradiction.

*Evidence:* AGENTS.md:112-125 ("**The real GPU is available inside the toolbox** — the host's NVIDIA card enumerates fine (e.g. a discrete RTX) ... `VK_ADD_DRIVER_FILES=/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json`") vs AGENTS.md:367-368. plans/foliage-veg/READMESUMMARY.md:177-192 records a full green gate on `NVIDIA GeForce RTX 3070 Ti`.

**(e) AGENTS.md Status section vs the tree** — "**Built:** ... shadows (directional/spot/point/contact/ray-traced)"

The Status enumerates the pre-phase-11 shadow architecture and never mentions the physical-atlas
virtual shadow-map system that replaced it — the word VSM does not appear in AGENTS.md at all.
Directional, spot and point shadows are now all VSM page-atlas paths (the docs pages were
rewritten accordingly; a retired point-shadow pass was even removed from the xtask coverage test
in commit 67389f21). Also absent from "Built": the opacity-micromap layer,
`VK_NV_cluster_acceleration_structure`, and the partitioned TLAS — all of which the planset
closes and all of which have shipped symbols, `render-stats` fields, and e2e coverage.

*Evidence:* `rg -ni "virtual shadow|vsm" AGENTS.md` → empty. Built: engine/crates/rendering/src/vsm.rs (`VsmResidency`, `VsmDirectionalSpace`, `VsmGpu`, `VsmDemand`), engine/assets/shaders/vsm_demand.slang + vsm_demand_compact.slang, docs/content/explanations/shadows-and-culling/virtual-shadow-maps.md, directional-shadows.md:9 ("eight camera-snapped clip levels of virtual shadow pages"), point-light-cube-shadows.md:9; tests/e2e/vsm.test.ts, vsm-churn.test.ts. OMM: engine/crates/rendering/src/device.rs:715 `omm_dispatch`, upload.rs:1249 per-submesh `micromap`, `ommSupported` in schemas/control/openrpc.generated.json. Cluster/PTLAS: engine/crates/rendering/src/rt_ptlas.rs, `clusterAsSupported`/`clusterBlasCount`/`clasCount`, tests/e2e/rt-ptlas.test.ts + rt-telemetry.test.ts.

**(e) AGENTS.md Status section vs the tree** — "... and the editor's five vegetation panels"

Eleven vegetation/plant/biome panels are registered in the dock registry, plus the asset
workspace and the mode's viewport toolbar. "Five" matches no natural subgrouping I could find
(the scene-mode set is three, the plant-workspace set is six).

*Evidence:* AGENTS.md:365. editor/src/components/dock/panelRegistry.tsx registry keys: `vegetation`, `vegetationTelemetry`, `ecologyTimeline`, `vegSummary`, `plantGraph`, `plantWind`, `plantAtlas`, `plantHierarchy`, `plantSeason`, `plantProxies`, `biomeGraph`. Plus editor/src/panels/VegetationAssetWorkspace.tsx and VegetationViewportToolbar.tsx.

**(f) Planset self-report consistency — README.md / READMESUMMARY.md internal coherence and gate evidence** — READMESUMMARY.md is "the map" of where every phase stands

The document contradicts itself three times and a reader who stops before the last section gets
the wrong answer. The header table and line 27 say all fifteen phases are complete with no boxes
open; a later section still asserts twelve of fifteen with twenty open boxes and tabulates
which; an earlier section describes the mesh executor as depth-only with no runtime selection,
which the tree contradicts. The document is written as a chronological log without marking the
superseded sections as historical, which is exactly the shape of claim this audit is meant to
catch.

*Evidence:* plans/foliage-veg/READMESUMMARY.md:27 ("All fifteen phases complete; no boxes open") vs :236-256 ("Twelve of fifteen phases are `COMPLETED`. Three carry the remaining **20 open boxes**") vs :51-68 ("Still open on scope — depth-only, no mesh counterpart for the shaded übershader pass and no runtime selection yet") — refuted by engine/crates/rendering/src/renderer.rs:1751 and scene_pass.rs:92. Also :165 and :189 report `just schema` as 249 checks while :286 reports 251.

**phase-14-botanical-authoring-interchange** — "Atlases and coverage-preserving textures, aggregate-voxel appearance error, and RT/OMM derivation inputs. Each needs a generator of its own; none is built, and none is faked." — the box body then states both "ALL FOUR GENERATORS NOW REACH A COOK, which is what closes this box" and, at the end, "THE BOX STAYS OPEN on the wiring: nothing yet drives the packer, the texture generator, the calibrator, or the plane from a cook stage."

The code is done; the plan text is not. Three mutually contradictory strata sit inside one
ticked box: the requirement line still says "none is built", a middle paragraph says all four
reach a cook, and the closing paragraph says nothing drives them from a cook stage. The tree
agrees with the middle paragraph only — `build_plant_sections` calls all three wiring points in
the stated dependency order. A reader trusting the last paragraph would re-implement work that
exists; a reader trusting the first would think none of it exists. Same append-only-strata
problem as phase-15 boxes 38/150/372.

*Evidence:* engine/crates/assets/src/plant_cook.rs:2381 `atlas_normalized_family`, :2408 `calibrate_voxel_appearance_error`, :2415 `derive_family_micromaps` (all inside `build_plant_sections`); generators at engine/crates/assets/src/atlas.rs:225 `generate_family_atlas`, :293 `alpha_plane`, engine/crates/geometry/src/hierarchy_reference.rs:585 `calibrate_voxel_appearance_error`; plans/foliage-veg/phase-14-botanical-authoring-interchange.md:112-190

**phase-15-production-platform-closure** — "Run `just engine`, `just prepare-for-commit`, `just schema`, `just test`, `just e2e`, export/player smoke, headless validation, and platform suites" — evidence "`just e2e` **334/334 across 55 files** EXIT=0".

Contradicted by a sibling ticked box in the same phase, which records that two e2e suites are
red on the project's primary adapter: "two of the three failures (`vsm` page atlas, `vegetation-
export`) fail identically on the discrete GPU, so they are not software-specific". Both boxes
are ticked. Either the suite is green (424) or two suites fail on the discrete GPU (351); they
cannot both hold. The file counts are also stale in every direction — the boxes variously claim
328/51, 332, 334/55, 361/62, 365/63, and 370/64 files, while `ls tests/e2e/*.test.ts | wc -l` is
69 today, so no box's evidence describes the current suite. Per the repo gate rule these are the
numbers the phase rests on, and none of them can be reconciled by reading.

*Evidence:* plans/foliage-veg/phase-15-production-platform-closure.md:424-451 vs :351-363; `ls tests/e2e/*.test.ts | wc -l` = 69

**phase-15-production-platform-closure** — "Add parallel/distributed-safe work-item manifests, cancellation/resume, cache sharing, atomic publish, corruption repair, and deterministic final package ordering."

The work IS landed and verified — but the box keeps the superseded record alive directly beneath
the new one, which is the shape this repo's NO-LEGACY rule forbids in code and which is just as
misleading in a plan. Under the 2026-07-28 "LANDED" paragraph the box still carries "NOT YET:
parallel/DISTRIBUTED work-item manifests. A full protocol was designed and PROTOTYPED... It was
REMOVED rather than landed unused", plus a "WHAT LANDING IT COSTS" paragraph describing the
restructure as future work. Every clause of that block is now false: the module exists, the
claim protocol exists, the split it predicted happened, and the sequential path it describes as
still-present is gone (`cook_vegetation_cells` no longer exists anywhere).

*Evidence:* engine/crates/vegetation/src/cook_work.rs (476 lines; magic SVCWRK01/SVCWPL01/SVCWCP01, `cook_work_own_input_key`); engine/crates/assets/src/vegetation_cooker.rs:303 `stage_vegetation_cook`, :813 `run_work_items`, :999 `cook_one_cell`, :1691 `cell_own_dependencies`, :1750 `cell_ancestor_dependencies`; engine/crates/assets/src/vegetation_store.rs:32 `WorkPayload`, :211 `work-claims`; plans/foliage-veg/phase-15-production-platform-closure.md:77-99

**phase-15-production-platform-closure** — "Expose GPU instance/node/cluster/triangle/voxel counts, cull stages, HZB retests, bins/indirect draws, page faults/latency, overdraw/quad utilization, deformation, VSM pages/cache/dirty work, GI/RT/BLAS/OMM metrics, and every pressure/overflow flag."

Same append-only-strata problem: the box states "NOT YET: node counts, a distinct bin count,
overdraw, deformation counts, and BLAS build *time*... OMM metrics wait on the derivation box",
and every clause of that sentence is contradicted by paragraphs below it AND by the tree.
Verified built: counter words 16 (culled nodes), 17 (visited nodes), 18 (bins), 19 (deformed),
20 (covered samples), with `SCENE_VISIBILITY_COUNTER_WORDS = 24`; OMM metrics on the wire.
Separately, the box's own title names CLUSTER counts, and no paragraph in the box addresses them
— the phase's platform box states plainly that no CLAS path is built, so that clause of the box
title is silently unmet rather than carved out. The `commands_render.rs:284-287` citation for
BLAS memory has also drifted (`blas_bytes` is at :301), and AGENTS.md's own convention is to
cite symbols rather than line numbers for exactly this reason.

*Evidence:* engine/crates/rendering/src/visibility.rs:48 `SCENE_VISIBILITY_COUNTER_WORDS = 24`, :63-81 CULLED_NODES/VISITED_NODES/BINS/DEFORMED/COVERED_SAMPLES; engine/crates/protocol/src/dto.rs:653-661 `omm_micromaps`/`omm_opaque`/`omm_transparent`/`omm_unknown`; engine/crates/control/src/commands_render.rs:287-304; plans/foliage-veg/phase-15-production-platform-closure.md:163-166 vs :170-207

**phase-15-production-platform-closure** — "Alarms ... draining through `drain-alarms`/`active-alarms` into editor toasts."

There is no `active-alarms` command. The registered name is `list-active-alarms`; `grep -c
'"active-alarms"' schemas/control/command-manifest.generated.json` returns 0 and `"list-active-
alarms"` returns 1. Everything the box describes is built (the panel calls the right name), so
this is a naming error in the record rather than a gap — but it is the kind a reader would try
verbatim from `sa` and get a rejection from.

*Evidence:* schemas/control/command-manifest.generated.json (no `active-alarms`; `list-active-alarms` present); editor/src/panels/VegetationTelemetryPanel.tsx:69 `client.listActiveAlarms()`; plans/foliage-veg/phase-15-production-platform-closure.md:215

### Partly built (32)

**phase-1-spatial-numeric-foundation** — Provide deterministic priority queues whose scheduling changes latency, not output.

`SpatialJobQueue` / `JobPriorityKey` are defined and unit-tested but have zero consumers
anywhere in the workspace (`grep -rn 'SpatialJobQueue|JobPriorityKey' engine/` outside
engine/crates/spatial/src returns nothing). The code that actually orders vegetation streaming
work implements its own ordering inline — `VegetationWorld::admitted_masks` sorts residency
snapshots by `(priority desc, cell asc)` — so the 'one deterministic priority queue' the phase
provides is not the one production uses, and the repo now carries two independent orderings for
the same job.

*Evidence:* engine/crates/spatial/src/residency.rs:431 `JobPriorityKey`, :487 `SpatialJobQueue`; engine/crates/vegetation/src/runtime_world.rs:1694 `admitted_masks` (inline `sort_unstable_by` on priority/cell)

**phase-1-spatial-numeric-foundation** — Capture current CPU scene-gather, draw count, `InstanceData` upload, shadow, RT, and memory metrics before renderer changes.

Only one baseline record exists: `benchmarks/foliage-veg/phase-1-apple-m4-moltenvk.json`. On
that device `rtSupported: false`, so `rtInstances: 0` is an absence, not a measurement, and
`vramUsageBytes: 0` / `vramBudgetBytes: 0` mean no GPU-memory figure was captured at all. The
phase's own `quality-invariants.md` states 'NVIDIA, AMD, and MoltenVK keep separate records; a
result from one class is not relabelled as another class's acceptance threshold' — yet no NVIDIA
Phase 1 baseline was recorded even though the RTX 3070 Ti is demonstrably reachable (it produced
`compute-conformance-nvidia-rtx-3070-ti.json`). The RT and memory legs of this box are
unmeasured.

*Evidence:* benchmarks/foliage-veg/phase-1-apple-m4-moltenvk.json (`platform.rtSupported: false`, `observed.rtInstances: 0`, `observed.vramUsageBytes: 0`); benchmarks/foliage-veg/quality-invariants.md 'Budget evidence' section

**phase-2-domain-assets-mutations** — Register it in `register_builtin_components`, `BUILTIN_COMPONENT_NAMES`, `COMPONENT_NAMES`, schema fragments, inventories, fixtures, inspector order, create/inspect/remove paths, and scene serde.

Every engine-side registration is real (`register_component!(reg, VegetationField,
"VegetationField")`, `BUILTIN_COMPONENT_NAMES`, `COMPONENT_NAMES`, `codegen.rs` decl+frag
entries, scene document serde, add/inspect/remove control commands with the singleton rejection
test). The 'inspector order' leg is not done: `VegetationField` is absent from `COMPONENT_ORDER`
in the editor. Two consequences follow. (1) `canonicalComponentNames` puts it in the unordered
`extra` tail rather than a canonical slot. (2) `ADDABLE_COMPONENTS = COMPONENT_ORDER.filter((c)
=> !NON_ADDABLE.has(c))`, so `VegetationField` never appears in the Inspector's Add-component
menu — and `addComponent` is the editor's only add path (`grep -rn 'addComponent|add-component'
editor/src` yields only InspectorPanel + client.ts). There is no drag-a-.svegmap-into-the-scene
alternative. The same list also still carries `"MaterialAsset"` and `"Material"`, neither of
which is in `COMPONENT_NAMES`, so it is stale in both directions despite its own header comment
saying 'A regenerated schema with new components extends COMPONENT_ORDER'.

*Evidence:* editor/src/lib/componentOrder.ts:6 `COMPONENT_ORDER` (no VegetationField; contains stale "MaterialAsset"/"Material"); editor/src/panels/InspectorPanel.tsx:110 `ADDABLE_COMPONENTS`; engine/crates/protocol/src/scene_dto.rs:693 `COMPONENT_NAMES`; engine/crates/scene/src/registry.rs:390

**phase-2-domain-assets-mutations** — Cache deletion cannot remove any map, plant, biome, or persistent state bytes.

Persistent state bytes do live inside the disposable cache. `vegetation-state-baseline` takes
the runtime's reduced persistent state (`world.persistent_state().canonical_bytes()`) and writes
it through `VegetationArtifactStore::publish_baseline`, which resolves to
`<project>/cache/vegetation/baselines/<manifest>.svegstate` — the same root the crate AGENTS.md
calls 'disposable: deleting the cache loses no authored and no persistent runtime state'. That
baseline is a snapshot of authored runtime mutations; no authored source can regenerate it, so
`rm -rf <project>/cache/vegetation` destroys it. The cited round-trip test does not test on-disk
cache deletion at all: it deletes the thumbnail cache directory and calls
`clear_asset_caches()`, which is an in-memory handle drop (`clear_loaded_asset_state`), then
asserts authored bytes are unchanged. Nothing anywhere removes `assets.vegetation_cache_root`
and asserts state survival.

*Evidence:* engine/crates/control/src/commands_vegetation_runtime.rs:214-232 `vegetation-state-baseline` → `publish_baseline`; engine/crates/assets/src/vegetation_store.rs:29/42/51 (`Baseline` → `baselines` → `svegstate`), :147 `publish_baseline`; engine/crates/assets/src/lib.rs:300 `project_vegetation_cache_root`, :806 `clear_asset_caches`; engine/crates/assets/src/vegetation.rs:2503 `all_authored_asset_kinds_round_trip_and_chunk_writes_are_sparse` (lines ~2668-2672); engine/crates/vegetation/AGENTS.md 'Formats' section

**phase-2-domain-assets-mutations** — Use distinct envelopes over the same mutation/reducer: editor journal — gesture grouping plus inverse/preimage for undo/redo.

`EditorJournalEnvelope { gesture, records, inverse }` is declared and never used: `grep -rn
'EditorJournalEnvelope' engine/ editor/` returns only its own definition. It has no producer, no
consumer, no codec, and no test. The editor's actual vegetation undo/redo is reconstructed from
inverse `vegetation-mutate` control calls in the panel layer, matching the repo-wide editor undo
design. The save-state envelope (`SaveStateEnvelope`) and its compaction are genuinely
implemented and tested; the journal envelope is a declaration standing in for an implementation.
Note this section of the phase file carries no checkbox, but the phase is marked COMPLETED.

*Evidence:* engine/crates/vegetation/src/mutation.rs:553-561 `EditorJournalEnvelope` (sole occurrence in the tree); editor/src/panels/ViewportPanel.tsx:171-180 (undo via inverse `vegetationMutate` tombstone/planting calls); engine/crates/vegetation/src/mutation.rs:1517 `snapshot_tail_compaction_matches_full_reduction` (the save envelope, which is tested)

**phase-5-runtime-cells-persistence** — Support multiple simultaneous sources and predictive prefetch from velocity/camera motion.

The prefetch mechanism exists (`SpatialSource::velocity_mps` / `prediction_seconds` ->
`SpatialSource::predicted_position`, which offsets the claim centre by velocity*prediction), but
nothing in the tree ever supplies a non-zero velocity on a production path. The host's only
production source (the editor viewpoint) hardcodes `velocity_mps: glam::DVec3::ZERO`, and so
does the player. The only non-zero velocity anywhere is inside a control-crate unit test. With
velocity always zero, `predicted_position()` always returns the current position and the
predictive half of the box is dead code in production. Multiple simultaneous sources ARE real
(ResidencyManager keys claims per SpatialSourceId).

*Evidence:* engine/crates/spatial/src/residency.rs:153 `SpatialSource::predicted_position`; engine/crates/host/src/layer.rs:652 `velocity_mps: glam::DVec3::ZERO`; engine/crates/player/src/main.rs:283 `velocity_mps: ...DVec3::ZERO`; only non-zero use is engine/crates/control/src/commands_scene.rs:2883 inside `#[test] spatial_residency_reports_sources_and_facet_counts`

**phase-6-virtual-geometry-render-substrate** — Add async-compute scheduling as a render-graph queue assignment for the same declared pass. Devices without useful overlap execute the identical pass on graphics.

The scheduling machinery is real and wired into the live frame
(`graph.submission_plan(self.device.render_graph_queue_families())` →
`record_submission_plan_profiled`, with release/acquire pairs and cross-queue timelines, plus
eight dedicated tests). What is missing is any consumer: `RgQueuePreference::AsyncCompute` is
never set by a production pass — the only construction site in the workspace is a test helper —
so `plan.compute_batch_count()` is always 0 in a real frame and the async lane is never executed
outside unit tests. AGENTS.md's own status line still lists 'async compute' under 'Not yet',
which contradicts this box being ticked; one of the two has to move.

*Evidence:* engine/crates/rendering/src/render_graph.rs:2110 `RgPass::compute(name).queue(RgQueuePreference::AsyncCompute)` inside `mod tests` is the sole construction; engine/crates/rendering/src/renderer.rs:9375 `graph.submission_plan(...)` / :9419 `record_submission_plan_profiled`; AGENTS.md 'Not yet:' bullet

**phase-7-gpu-scene-visibility-cutover** — Add `VK_EXT_mesh_shader` execution when individual feature bits/limits qualify.

The box asks for capability-gated execution; the built path is an opt-in environment toggle
layered on top of the capability. Every production run — including on the RTX 3070 Ti the plan
cites as qualifying — executes the indexed path unless the operator sets
`SAFFRON_MESH_EXECUTOR=1`. The mesh executor is therefore dead in the default configuration, and
only the scene + scene-survivor passes ever consult it (the depth-prepass, gbuffer, motion, vsm-
pages and reactive-coverage bodies call `record_executor_depth_family`, which has no mesh
branch). The scope narrowing is recorded honestly inside the box text, but the box is ticked as
if the capability gate alone drove it.

*Evidence:* engine/crates/rendering/src/renderer.rs:1751 `mesh_executor: device.capabilities.mesh_shader && std::env::var("SAFFRON_MESH_EXECUTOR").as_deref() == Ok("1")`; only consumers are renderer.rs:8769 (`scene`) and renderer.rs:9131 (`scene-survivors`) via `scene_pass.rs:78`.

**phase-7-gpu-scene-visibility-cutover** — Acceptance: "Static-scene CPU render preparation scales with changes/residency, not total visible instances or draw count (`instanceUploadBytes` stays (near-)zero on a steady scene — asserted in e2e)".

The parenthetical measures only GPU-scene table upload bytes, which is not the same thing as CPU
render preparation. `gather_static_frame_facts` still walks every `Transform + MeshComponent`
entity every single frame — loading the mesh asset, calling `resolve_entity_materials`,
computing the world matrix and world AABB, and pushing one `RtInstanceInput` per entity — and it
is called unconditionally from `render_scene`. That is O(total instances) per frame, exactly
what the box says no longer happens. The single e2e assertion cannot detect it: it runs on a
one-cube scene, bounds a different metric, and its bound is 64 KiB rather than near-zero. No
test compares preparation cost between a small scene and a large one.

*Evidence:* engine/crates/assets/src/render_scene.rs:1295 `fn gather_static_frame_facts` (`scene.for_each::<(&Transform, &MeshComponent), _>` then a per-entity loop with `resolve_entity_materials` / `world_matrix` / `build.rt_instances.push`), called at render_scene.rs:863 inside `pub fn render_scene` (:790). Sole e2e assertion: tests/e2e/perf.test.ts:56 `expect(stats.instanceUploadBytes).toBeLessThan(65536);`

**phase-7-gpu-scene-visibility-cutover** — "Prioritize projected error, visibility probability, motion/prefetch, shadow/GI/RT demand, and source priority rather than distance alone."

The implemented score is projected transition error x a frustum/GI reach weight x a flat 2.0
boost when the instance moved, thresholded at 0.25. Two named inputs are absent: there is no
source priority term anywhere in the demand computation (the payload source is consulted only
when building the load request, never when ranking), and there is no prefetch beyond the moved-
instance multiplier. The box's parenthetical addresses only the shadow/GI/RT-demand input, so
these two go unmentioned. View-class priority bands (`SceneViewClass::page_demand_priority`) do
exist and are tested, so the class half of the claim holds.

*Evidence:* engine/crates/assets/src/gpu_scene_mirror.rs:939-948 — `error_metres * view.proj_scale / distance`, `*= page_demand_reach_weight(stats.in_frustum, stats.gi_reachable)`, `if stats.moved { projected *= 2.0; }`; source is read only at gpu_scene_mirror.rs:965-971 when enqueuing `PageLoadRequest`.

**phase-11-virtual-shadows-lighting-rt** — For structured deformation, materialize the selected assembly hierarchy into GPU geometry for BLAS build/update through the shared deformation output and cache policy.

The assembly half is genuinely built (per-prototype BLAS + one TLAS instance per active use).
The "through the shared deformation output" half is not: wind and interaction deformation never
reach any acceleration structure. `GpuSceneMirror::vegetation_ray_instances` builds each plant's
TLAS transform from `static_placement_matrix(&placement)` — the rest pose — and
`Rt::plan_skinned_blas_refits` only refits instances backed by the deformed-vertex arena
(skinning/morph) plus `plan_tessellated_blas_builds`. There is no wind refit path and no
deformed geometry for a plant. So a swaying canopy casts ray-traced shadows and appears in
reflections in its bind pose while every raster pass sways it, which is exactly the cross-
consumer disagreement Phase 10's provider contract exists to prevent. The box's own final
parenthetical admits this in as many words ("Still unbuilt for this box.") yet the box is
ticked.

*Evidence:* engine/crates/assets/src/gpu_scene_mirror.rs:517 `vegetation_ray_instances` → `model: static_placement_matrix(&placement)`; engine/crates/rendering/src/rt.rs:739 `plan_skinned_blas_refits` (guarded on `deformed_buffer`), rt.rs:509 `plan_tessellated_blas_builds`; no wind/interaction refit exists (`rg -n "wind" engine/crates/rendering/src/rt.rs` returns nothing); plans/foliage-veg/phase-11-virtual-shadows-lighting-rt.md:343 "Still unbuilt for this box."

**phase-11-virtual-shadows-lighting-rt** — Standard KHR any-hit over canonical coverage remains baseline-correct.

The claim is proven only for ECS-mirrored meshes. Vegetation is placed into the TLAS with
`custom_index: RT_UNMIRRORED_INSTANCE` (0x00FFFFFF), and `gpuSceneResolveCandidate` rejects any
slot at or above `addresses.instanceCapacity`, after which `gpuSceneRayCandidateCovered` returns
`true` unconditionally. So every non-opaque candidate on a plant auto-commits: a masked leaf
card casts a solid quad shadow in ray-traced shadows, and the canonical classifier the box names
never runs for vegetation. This is conservative (no light leak) but it is not coverage
agreement, and no box or note discloses it. The e2e proof (`rt-anyhit`) uses a built-in cube
with a runtime thin-sheet material — a mirrored entity with a real slot — so it cannot catch
this.

*Evidence:* engine/crates/assets/src/gpu_scene_mirror.rs:533 `custom_index: saffron_rendering::RT_UNMIRRORED_INSTANCE`; engine/crates/rendering/src/rt.rs:87 `RT_UNMIRRORED_INSTANCE: u32 = 0x00FF_FFFF`; engine/assets/shaders/global_gpu_data.slang:1259 `if (instanceSlot >= addresses.instanceCapacity) return surface;` (surface.valid=false) and :1350 `if (!surface.valid) { return true; }`; tests/e2e/rt-anyhit.test.ts:125 uses a cube blocker, not a plant

**phase-11-virtual-shadows-lighting-rt** — Split cache policy into stable/static and dynamic layers without duplicating shadow content. Exact current/previous swept cluster bounds dirty only intersecting pages.

The static/dynamic split, the level cap and the budgeted drain are all real. The second sentence
is not: dirtying runs at whole-instance granularity, not cluster granularity.
`note_instances_moved` resolves one `instance_world_bounds` AABB per moved instance, and
`dirty_vsm_swept_bounds(min, max)` projects that single box (via its bounding sphere
`half_diag`) into every clip level, marking the whole footprint dirty. Phase 10's own box
explicitly deferred cluster-tight bounds to "the Phase 11 VSM/RT consumers that need the per-
cluster form" and stated "CLUSTER granularity remains open — `GpuPageClusterRecord` would go
48→80 B and the cluster stage does not exist"; Phase 11 then ticks this box without noting that
the deferred work never arrived. The deferral closes a loop with nothing built in either phase.

*Evidence:* engine/crates/rendering/src/renderer.rs:6877 `fn dirty_vsm_swept_bounds(&mut self, min: [f32;3], max: [f32;3])` (called once at renderer.rs:7059); engine/crates/rendering/src/persistent_gpu_scene.rs:1793 `note_instances_moved` → `instance_world_bounds(record)`; no per-cluster bounds field exists (`rg -n "GpuPageClusterRecord" engine/` shows a 48-byte record with no deformed/swept extent); plans/foliage-veg/phase-10-wind-deformation-phenology.md:136

**phase-11-virtual-shadows-lighting-rt** — Parameterize GI/reflection culling through the same hierarchy and residency demand rather than rebuilding plant-specific lists.

Most of this really did land (the GPU `gi_occluder_scatter` pass, `SCENE_VISIBILITY_PASS_REACH`,
per-class page demand, plants baking a `DistanceField` cook section,
`set_sdf_scene`/`record_sdf_culled` deleted). Two residuals stated inside the note are still
true in code and were not re-checked before ticking: (a) the RT instance list is still a per-
frame unculled full-ECS scan — every `(Transform, MeshComponent)` entity is collected into a
`Vec` before any cut is applied; (b) micro-field grass blades contribute to neither GI occlusion
nor ray tracing, because they exist only as GPU-reconstructed `GpuMicroCandidate`s with no CPU
instance and no prototype SDF. A dense grass field therefore occludes nothing in DDGI/GDF and
appears in no reflection or ray shadow.

*Evidence:* engine/crates/assets/src/render_scene.rs:1330 `scene.for_each::<(&Transform, &MeshComponent), _>(|entity, ...| meshes.push(...))` (collect-then-cut at :1372); engine/crates/assets/src/gpu_scene_mirror.rs:517 `vegetation_ray_instances` iterates `world.plants` only — no micro-candidate path; `rg -n "micro|blade" engine/crates/rendering/src/rt.rs` returns only doc-comment hits

**phase-10-wind-deformation-phenology** — Derive current phenology from existing date/latitude/time plus plant intrinsic curves and persistent lifecycle/health/moisture state.

The resolution function takes four inputs — the authored phenotype list, the cooked default, the
typed `PlantLifecycle`, and the season per-mille. Health and moisture are not parameters and are
read nowhere in the resolution, so two of the four state sources the box names do not influence
rendered phenology at all; the `Wet` role exists but nothing can ever select it. "Plant
intrinsic curves" is also delivered as a wrapping integer window (`season_window` /
`role_season_window`), not a curve. The note discloses the health/moisture gap honestly, which
is the right instinct — but a box whose requirement names four inputs and whose implementation
reads two is not complete.

*Evidence:* engine/crates/vegetation/src/season.rs:65 `pub fn resolve_rendered_phenotype(phenotypes, cooked: u32, lifecycle: PlantLifecycle, season_mille: u16) -> u32`; `role_season_window` at season.rs; engine/crates/vegetation/src/asset.rs:386 `PhenotypeRole::Wet` has no producer

**phase-10-wind-deformation-phenology** — Compile global and local sources into a clipmapped/vector field sampled identically by clouds, fog, future cloth/particles/weather, physics queries, and vegetation. ("every consumer migrates to this seam")

Clouds, fog, vegetation instances, micro blades, the `sample-wind` command and the editor
overlay all genuinely go through `sample_composed`/`sampleComposedWindVelocity`, and the bespoke
fog gust sine is really gone. Physics is the exception the note skips: `saffron-physics` carries
no `saffron-wind` dependency and contains no wind sampling of any kind, so the "physics queries"
consumer the requirement names has no access to the seam at all. Unlike cloth/particles/weather
the requirement does not mark physics as future work, and the note's blanket "every consumer
migrates to this seam" reads as covering it.

*Evidence:* engine/crates/physics/Cargo.toml has no `saffron-wind` entry (only scene, rendering, control, host do); `rg -n "saffron_wind|WindProfile|sample_composed" engine/crates/physics/src/` returns nothing

**phase-10-wind-deformation-phenology** — Deform assembly parts without expanding authored structure; compute tight node/cluster swept bounds rather than inflating whole-tree bounds.

The node-granularity half is unusually well built and well proven (the cooker's
`close_subtree_bounds` closure, a format-version bump to force a reseed, and `tests/e2e/node-
cull-parity.test.ts` running two hosts across four poses with a calibrated mutation check). The
cluster granularity the box's own text asks for is openly not built, and the carve-out is
deferred to Phase 11's consumers — where it is then not built either (see the VSM cache-split
finding). Judged on its own the box is honestly annotated; judged across the two phases the
deferral has no destination.

*Evidence:* engine/crates/geometry/src/virtual_hierarchy.rs `close_subtree_bounds`, `validate_portable_virtual_hierarchy`, `PORTABLE_HIERARCHY_FORMAT_VERSION`; tests/e2e/node-cull-parity.test.ts:147-174; plans/foliage-veg/phase-10-wind-deformation-phenology.md:136 "CLUSTER granularity remains open"

**phase-10-wind-deformation-phenology** — Debug wind vectors/spectra, local source influence, branch modes, stiffness, current/previous bounds, interaction displacement/velocity/recovery, and phenotype weights in `sa` and editor.

`vegetation-wind-record` is a real one-shot GPU readback of the prepass record (not a CPU re-
derivation), registered in `saffron-control`, present in the generated manifest, and covering
most of the list. Two named items in the requirement have no implementation and the box says so:
turbulence spectra as a decomposed per-octave view, and a whole-field interaction capture.
Disclosed carve-out rather than a false tick, but the box is ticked over an unimplemented
clause.

*Evidence:* engine/crates/control/src/commands_vegetation_runtime.rs:118 `vegetation-wind-record` → `ctx.renderer.capture_plant_wind_record(cell, plant)`; present in schemas/control/command-manifest.generated.json; plans/foliage-veg/phase-10-wind-deformation-phenology.md:323 "NOT COVERED: turbulence spectra as a decomposed per-octave view ... and a whole-field interaction capture"

**phase-8-vegetation-rendering** — Add a GPU selection-ID path returning tagged `Vegetation(PlantId)` for macro plants and an explicit nonpersistent micro hit for paint feedback.

The literal requirement — a GPU selection-ID path — is not implemented, and the box's own
parenthetical concedes it: "a GPU ID buffer exists for no content type — selection is the
engine's one CPU viewport ray." The tagged `vegetation` / `micro-vegetation` pick kinds and the
stable `PlantId` return are real and e2e-asserted, so the second sentence of the box ("Editor
selection resolves through the CPU cell snapshot/provenance, not GPU slot indices") is
satisfied; the first sentence is not. The box is ticked on a design the box asked to be
replaced, without the box text being rewritten.

*Evidence:* engine/crates/control/src/commands_scene.rs:1155 calls `world.query_ray(...)` and :1183 `world.query_micro_ray(ray)`, both CPU snapshot walks in engine/crates/vegetation/src/runtime_world.rs:1137 and :1166; `PickKind::Vegetation` at commands_scene.rs:1171, `PickKind::MicroVegetation` at :1195. No selection-ID image or readback exists for vegetation.

**phase-9-editor-authoring-debug** — Provide Select/Lasso, Paint, Erase, Density, Reapply, Single/Anchor, Fill, Spline, Volume, Exclude/Block, Pin, and Promote tools in the viewport toolbar ... *(the 13-tool row from the shared `vegetationTools.ts` vocabulary)*

Seven of the thirteen tools are inert palette entries: selecting lasso, density, fill, spline,
volume, exclude, or promote changes `vegetationTool` in the store and highlights a toolbar
button, but no code anywhere reads those values, so the viewport behaves exactly as it did
before the click. Only select, paint, erase, reapply, single, and pin are dispatched. Promote is
the sharpest case — the engine has a `vegetation-promote` control command, but the editor has no
typed wrapper for it and never calls it, so the Promote tool cannot reach the feature it names.
`isBrushTool` even returns true for density/fill/spline/volume/exclude/promote, so the brush HUD
renders parameters that feed nothing.

*Evidence:* editor/src/panels/vegetationTools.ts:29-43 declares all 13. The complete set of `vegetationTool` readers (grep across editor/src) is: ViewportPanel.tsx:680 ("reapply"), :684 ("paint"/"erase"), :794 ("select"), :1021 ("single"), :1025 ("pin"); plus store.ts:391/780/1498, VegetationViewportToolbar.tsx:14, VegetationPanel.tsx:305, useVegetationShortcuts.ts:97 — all presentation/state only. `grep -rn "vegetationPromote|vegetation-promote" editor/src --include=*.ts --include=*.tsx` hits only the generated protocol/sa-types.ts:4587,4842.

**phase-9-editor-authoring-debug** — Add ordered named layers with mute/solo/lock, blend/operator, coordinate space, provenance, bounds, dirty/cook state, drag reorder, and conflict badges.

Drag reorder is not implemented. The layer list reorders through up/down buttons that swap the
`order` field in a two-row transaction; there is no drag gesture, no `draggable` attribute, and
no drop handling on the layer rows. The box's own parenthetical concedes it ("up/down buttons
drive the same wire a drag gesture would") yet the box is ticked with "drag reorder" still in
its text. Everything else in the box checks out: mute/solo/lock and reorder all commit through
`vegetation-map-layer-commit` with `pushEdit` inverses, the dirty dot reads the engine-computed
`dirtyLayers`, and conflict badges come from `vegetation-topology-diff`.

*Evidence:* editor/src/panels/VegetationPanel.tsx:407 `commitLayersPatch`, :440-456 mute, :465-490 solo, :502-515 reorder (swap with neighbour, `pushEdit({label:"Reorder layer", undo: swap, redo: swap})`). No `draggable` / `onDragStart` / `onDrop` in VegetationPanel.tsx. Dirty dot at :699; topology diff at :326/:857-875.

**phase-12-interaction-physics-queries-nav** — Copy transform, velocity/impulses, lifecycle/health, material/phenotype, collision/breakage, script fields, and source generation into promotion; write changed state through the reducer before demotion.

Velocity/impulses travel one way only. `commit_demotion`/`flush_state` write a
`PromotionOriginState` carrying linear+angular velocity through the reducer, and
`apply_mutation` stores it as `PlantDelta::promotion_origin`. But nothing ever reads that
velocity back: the only reader of `promotion_origin` in the whole tree is
`commands_vegetation_runtime.rs:975` (`promoted: state.promotion_origin.is_some()`), a boolean.
`spawn_view` builds the entity view from `VegetationPlantSnapshot`, which has no velocity field
at all (runtime_world.rs:102-138), so a re-promoted plant always starts at rest. A demote→re-
promote cycle silently drops the momentum the box says is copied.

*Evidence:* engine/crates/runtime/src/vegetation_promotion.rs:601 spawn_view (no velocity), :737 origin_state (writes velocity); engine/crates/vegetation/src/mutation.rs:337 promotion_origin, :973 write; engine/crates/control/src/commands_vegetation_runtime.rs:975 only reader; engine/crates/vegetation/src/runtime_world.rs:102 VegetationPlantSnapshot

**phase-12-interaction-physics-queries-nav** — Emit typed lifecycle/interaction events once per reducer transition for scripts, VFX, audio, quests, future fire, and nav dirtying.

The typed transition ring is real and correct (`VegetationTransitionKind`,
`VEGETATION_EVENT_RING_CAP`, `drain_events(since)`), but the only consumer is the `vegetation-
drain-events` control command. There is no script-side delivery: `ScriptHostBridge`
(script/src/bridge.rs) declares no vegetation-event method and `RuntimeScriptBridge` implements
none — grepping `drain_events|vegetation_event|on_vegetation` under engine/crates/script/src and
engine/crates/runtime/src returns nothing. The box's own sibling admits this ('an event callback
... NOT script-side yet'), but this box is ticked naming scripts as a consumer.

*Evidence:* engine/crates/vegetation/src/runtime_world.rs:589 VEGETATION_EVENT_RING_CAP, :951 drain_events; engine/crates/script/src/bridge.rs:138-155 (bridge trait, no event hook); engine/crates/control/src/commands_vegetation_runtime.rs:404 vegetation-drain-events

**phase-12-interaction-physics-queries-nav** — Add native vegetation AABB/radius/ray/nearest APIs to Luau/control with family/tag/state filters.

Control has the closed filter (`VegetationQueryFilter` with
families/required_tags/lifecycles/interaction_policies, mapped by `query_filter`), but the Luau
side has none: `vegetation_raycast/nearest/in_radius` on `ScriptHostBridge` take only geometry
and a limit, and `RuntimeScriptBridge` passes the default filter. The generated
`sa.generated.luau` signatures confirm no filter parameter. The box is ticked with the deferral
recorded in its own note, so the 'with family/tag/state filters' half is unimplemented for the
Luau surface.

*Evidence:* engine/crates/script/src/bridge.rs:138-149 (no filter params); engine/crates/runtime/src/bridge.rs:287-340; schemas/control/sa.generated.luau:109-111; engine/crates/vegetation/src/runtime_world.rs:160 VegetationQueryFilter

**phase-12-interaction-physics-queries-nav** — Handle source-cell unload, recook, undo, and network-authority changes without duplicate ownership.

The mechanisms exist (`VegetationPromotion::abandon` on a manifest-identity change, suppression
keyed by `PlantId` on `VegetationWorld`, `demote_all` before `clear_vegetation`), but no test —
unit or e2e — exercises unload, republication, or recook while a plant is promoted. The box's
own note concedes network authority 'has no implementation to reconcile with yet'. The promotion
unit tests (vegetation_promotion.rs:1027-1143) cover only request-state transitions, the product
identity, collider selection, quantization, and key stability; none drives `abandon` or a
rebind.

*Evidence:* engine/crates/runtime/src/vegetation_promotion.rs:380 abandon; engine/crates/runtime/src/session.rs:432-439 rebind path; engine/crates/runtime/src/vegetation_promotion.rs:929-1144 (test module, no rebind/unload case)

**phase-13-ecology-catchup** — Nav/physics/render facets observe committed tick generations only ... `EcologyState::is_caught_up` tells a reader whether a cell's simulation facet stands at world time.

There is no such reader. `is_caught_up` is called only from its own unit test (ecology.rs:351,
:361) and from the `vegetation-ecology-status` DTO builder; no facet — collision residency, the
navigation seam, the GPU scene mirror — consults it before publishing. Nothing gates a facet on
simulation readiness; the claim's mechanism exists but is never exercised on a production path.
(The narrower claim that facets read a published generation is true.)

*Evidence:* engine/crates/vegetation/src/ecology.rs:208 is_caught_up; `rg is_caught_up engine/crates` → ecology.rs tests plus the status command only; engine/crates/runtime/src/vegetation_collision.rs:65 and vegetation_navigation.rs:86 make no readiness check

**phase-13-ecology-catchup** — Use compact SoA state and spatial hashing over active simulation facets.

The compact-row half is real (`EcologyPlantState` read straight from `macro_points`, no
scene/promotion access). There is no spatial hashing: `region_state` builds a
`Vec<EcologyPlantState>` per cell into a `BTreeMap`, and `dependency_regions` is a transitive
closure over a `BTreeSet` of cell keys. Neighbour lookup is a linear scan of the region's cells.
The box's own evidence note quietly drops the spatial-hashing clause rather than satisfying it.

*Evidence:* engine/crates/vegetation/src/runtime_world.rs:1565-1595 region_state; engine/crates/vegetation/src/ecology_region.rs dependency_regions (BTreeSet closure)

**(a) One ownership model — README Outcome bullets, precedence chain, canonical-data table** — "all geometry passes consume one semantic visible-cluster stream" and the ownership diagram's `H[visibility hierarchy and page requests] --> I[raster / VSM / GI / RT]`

Raster, VSM and the GI/SDF occluder feed do consume the hierarchy: `gi_occluder_scatter.slang`
turns the GI reach view's visible slot list into the frame's SdfInstance stream on device. Ray
tracing does not. The TLAS instance list is still assembled on the CPU by a full-ECS scan plus a
separate CPU materialization of the vegetation mirror, cut against the distance-field cascade
window (`gi_reachable`) rather than against the traversal cut. So one of the four consumers
named in the diagram reads a parallel list, and the READMESUMMARY's own 2026-07-27 note about
`rt_instances` being CPU-gathered is only half retired — the `sdf_instances` half was fixed, the
`rt_instances` half was not. Whether that satisfies phase 11's box text is that phase's
auditor's call; against the README's cross-cutting diagram it is a mismatch.

*Evidence:* engine/crates/assets/src/render_scene.rs:1295 `gather_static_frame_facts` — `scene.for_each::<(&Transform, &MeshComponent), _>` at :1330 builds `build.rt_instances`; `mirror.vegetation_ray_instances()` at :1314; both gated only by `gi_reachable` (:1307 `gi_occluder_bounds`). Contrast engine/assets/shaders/gi_occluder_scatter.slang ("turns the reach view's visible list into this frame's SDF occluder instances, on device") and engine/crates/assets/src/gpu_scene_mirror.rs:517 `vegetation_ray_instances`. Plan text: plans/foliage-veg/README.md:46 and the mermaid at :57-72.

**(e) AGENTS.md Status section vs the tree** — "**Not yet:** transient render-graph resources (graph-created images + aliasing) + async compute"

Half of this line is stale. Graph-created transient images (2D and 3D, keyed per frame-in-
flight) exist and are used from the production frame. Memory aliasing between transients is
genuinely absent. Async compute is genuinely not exercised: the queue-family selection,
`RgQueuePreference`/`RgQueueAssignment` resolution, and the cross-queue release/acquire +
timeline plumbing are all built, but the only caller that sets `RgQueuePreference::AsyncCompute`
is a unit test, so no production pass runs off the graphics queue.

*Evidence:* Built: engine/crates/rendering/src/transient.rs:292 `acquire_image`, :333 `acquire_image_3d`, held as `Renderer::transient` (renderer.rs:1125, used at :3239-:3260). Async-compute machinery: render_graph.rs:143 `RgQueuePreference`, :178 `resolve`, frame.rs:307-320 `reserve_timeline`, device.rs:1587 `choose_async_compute_queue`. Only `AsyncCompute` setter: render_graph.rs:2110, inside `#[cfg(test)]`. AGENTS.md:367-368.

**phase-14-botanical-authoring-interchange** — Authoring UX: "Add structure tree/graph, 3D preview, semantic selection, parameter inspector, family variation browser, wind/interaction preview, lifecycle/season timeline, materials/atlas view, collision/nav, hierarchy/voxel/error, and validation/cook-stat panels" — box asserts "ALL ELEVEN SURFACES THE BOX NAMES NOW EXIST".

Three of the eleven named surfaces do not exist. (1) STRUCTURE TREE/GRAPH: PlantGraphPanel
renders a flat list of scalar Stat rows and a read-only axis list truncated with `.slice(0,
24)`. There is no tree and no node-graph canvas: the shared canvas component
`editor/src/components/graph/GraphCanvas.tsx` is imported only by `MaterialGraphEditor.tsx` and
`BiomeGraphPanel.tsx` (`rg -l xyflow editor/src` returns exactly those two plus GraphCanvas
itself); the plant panel imports none of it. (2) PARAMETER INSPECTOR: no botanical operator
parameter (trunk taper/segments, branch length_ratio/radius_ratio/declination/jitter,
phyllotaxis pattern/divergence, tropism kind/strength, prune rule/threshold, module scale) is
displayed or editable anywhere in the editor. The panel's only two mutations are `addVariation`
and `setAge`; `graph.graph.nodes` is never rendered. The panel's own header comment cites an
edit label "Set trunk length" that no control in the file produces. (3) SEMANTIC SELECTION: the
element rows are plain `<div>`s with no onClick/selection state, so nothing selects an element
identity — and consequently none of the nondestructive manual edits the phase built
(`botanical_edit.rs` Transform/Trim/Remove/Graft) can be authored from the editor at all, even
though the wire carries them (`BotanicalGraphDto.edits`). The panel is a readout plus a
variation list, not the authoring surface this box claims.

*Evidence:* editor/src/panels/PlantGraphPanel.tsx (275 lines; addVariation/setAge are the only apply() callers; `elements?.axes ?? []).slice(0, 24)` read-only rows; header comment names "Set trunk length"); editor/src/components/graph/GraphCanvas.tsx used only by editor/src/panels/MaterialGraphEditor.tsx and editor/src/panels/BiomeGraphPanel.tsx; engine/crates/protocol/src/vegetation_dto.rs:3017-3019 `BotanicalGraphDto.edits`; engine/crates/vegetation/src/botanical_edit.rs (Transform/Trim/Remove/Graft)

**phase-14-botanical-authoring-interchange** — "Provide generators for branching families, phyllotaxis, profile/taper/cross-section, tropisms, gravity, light/obstacle response, pruning, graft/attachment, roots, vines, and surface detail."

The "light/obstacle response" clause is a label, not a generator. `bend()` gives
`TropismKind::Thigmotropism` the SAME sign and the SAME quadratic arc as `Gravitropism`
(`TropismKind::Gravitropism | TropismKind::Thigmotropism => -1`), so thigmotropism is byte-
identical to gravitropism at every input; there is no obstacle geometry, plane, or collider
input anywhere in the botanical evaluator (`rg -i obstacle engine/crates/vegetation/src` returns
only navigation-proxy and cell-facet hits). The variant's own doc comment at botanical.rs:199
says "Away from an obstacle plane", which no code implements — a comment describing behaviour
the code does not have. `Phototropism` is likewise a fixed +Y bend with no light direction or
light-source input, so it does not respond to light either. The box's other carve-out (host-
following vine operator) is honestly annotated; this one is not.

*Evidence:* engine/crates/vegetation/src/botanical.rs:2056-2087 `fn bend` (sign match at 2060-2063); engine/crates/vegetation/src/botanical.rs:199 doc comment "Away from an obstacle plane"

**phase-15-production-platform-closure** — "~~Validate AMD Vulkan required+mesh where present+KHR RT, including subgroup/workgroup variation.~~ DESCOPED BY THE PROJECT OWNER" — ticked `[x]`.

The box body is honest ("closed as out of scope, not as done. No AMD validation was performed
and none is claimed"), but it carries a `[x]` and is counted among the phase's completed boxes,
so any tally of "every box ticked" overstates what was validated. The audit-visible effect is
that the phase's platform-parity section reads as four platforms validated when it is three
(NVIDIA, MoltenVK, llvmpipe) with one unvalidated. Flagged for the tick, not for the descope
decision, which is the owner's to make.

*Evidence:* plans/foliage-veg/phase-15-production-platform-closure.md:333-339

### Ticked on evidence that cannot fail (28)

**phase-3-graph-determinism** — Acceptance: "Every dual-domain node passes Rust/Slang equivalence on NVIDIA and MoltenVK before it can carry `EquivalentGpu`."

Qualification is corpus-wide, not per-operator-behaviour. `GpuQualificationRegistry::qualify`
runs the 4-program / 13-invocation corpus, compares the aggregate SHA-256 against the Rust
reference, then emits evidence for EVERY operator where `has_slang_executor()` is true —
regardless of which code paths the corpus actually drove. The corpus exercises
`GraphCombineOperation::Add` (the `branch` program) and `Multiply` (the `multiply` program)
only; `vegetation_graph.slang` also implements combine codes 2 (`min`) and 3 (`max`), and no
invocation reaches them. An authored `EquivalentGpu` Combine node using Min/Max therefore runs
on the GPU on the strength of evidence that never executed that branch. `node_gpu_admitted`
gates on `qualifications.contains(operator, version, profile)` — operator granularity, so
nothing finer catches it. The gate machinery itself is real and fail-closed and the pinned
digest fb44dca4… is asserted in `abi_and_corpus_hashes_are_pinned`; the gap is corpus breadth,
not architecture.

*Evidence:* engine/crates/vegetation/src/graph_gpu.rs:746 `qualification_corpus` (Add at :844, Multiply at :944; no Min/Max); engine/crates/vegetation/src/graph.rs:1421 `GpuQualificationRegistry::qualify` emits evidence via `.filter(|operator| operator.has_slang_executor())`; engine/assets/shaders/vegetation_graph.slang:512-519 (`combine == 2u` / `== 3u`); engine/crates/vegetation/src/graph_gpu.rs:1593 `node_gpu_admitted`

**phase-3-graph-determinism** — Acceptance: "Halo/seam fixtures produce no duplicate or missing macro plants at cell faces/corners."

The only test covering this asserts ownership (`owner_cells[row] == result.cell`), ID uniqueness
across the four cells (`ids.insert(...)`), and a pairwise minimum XZ spacing of 2 m. It never
asserts that no plant is MISSING: there is no expected count and no comparison against a
reference evaluation of the same region. A halo bug that silently dropped candidates at a seam
would leave every assertion green — both the uniqueness and the spacing check get easier as
points disappear. The e2e counterpart cannot cover it either: `vegetation-graph.test.ts` asserts
`compiled.requiredHaloBits` is 0, so that fixture graph has no halo at all.

*Evidence:* engine/crates/vegetation/src/evaluator.rs:19633 `halo_faces_and_corners_publish_unique_spaced_owned_points` (assertions at :19654-19667); tests/e2e/vegetation-graph.test.ts:162 `expect(compiled.requiredHaloBits).toBe(0)`

**phase-3-graph-determinism** — Platform conformance: "both digests match the MoltenVK record byte for byte."

True of the result digests, but the two records were produced by different Slang toolchains:
`compilerIdentity` is "2026.10" for the NVIDIA record and "2026.12.2" for the MoltenVK one, so
`compileInputSha256`, `spirvSha256` and `recordSha256` all differ. The qualified
`GpuShaderArtifactIdentity` — what `VulkanGraphComputeExecutor` binds its evidence to, and what
licenses the GPU path per platform — is therefore not the same artifact on the two platforms,
and the NVIDIA record is against an older shader compiler. Re-running conformance on NVIDIA with
the current toolchain would produce a different artifact identity; the operator result hashes
should be unaffected, but that is untested on that adapter.

*Evidence:* benchmarks/foliage-veg/compute-conformance-nvidia-rtx-3070-ti.json (`compilerIdentity` "2026.10") vs benchmarks/foliage-veg/compute-conformance-apple-m4-moltenvk.json ("2026.12.2"); engine/crates/vegetation-gpu/src/executor.rs:67-72 (artifact identity feeds `GpuQualificationRegistry::qualify`)

**phase-4-cooker-cell-artifacts** — Acceptance: "Repeated full/incremental cooks produce identical manifests and cell bytes under different worker counts and schedules."

The only cooker-level test of this runs against an empty map. `save_empty_map` builds a
`VegetationMapAsset` with `inventory: Vec::new()`, so there are no graph-instance chunks, no
biome instances get evaluated, and the two published cells contain zero macro points — byte-
identity across 1/4/2 workers is asserted on effectively empty artifacts. Every e2e cook that
does produce real content passes `workers: 1` (16 call sites across the vegetation suites; no
e2e cook uses more than one worker), so multi-worker cook determinism over populated cells is
never exercised end to end. The substantive risk is mitigated one layer down —
`worker_counts_and_cell_request_order_are_byte_identical` compares 1 vs 4 workers over four
populated cells with a partitioned Competition node — but the claim as written at the
cooker/manifest level rests on an empty fixture.

*Evidence:* engine/crates/assets/src/vegetation_cooker.rs:2567 `repeated_incremental_cooks_preserve_identity_and_prior_cells`, using `save_empty_map` at :2512 (`inventory: Vec::new()`); mitigating coverage engine/crates/vegetation/src/evaluator.rs:19414; tests/e2e/*.test.ts all cook with `workers: 1`

**phase-1-spatial-numeric-foundation** — Shuffled job order, worker count, source order, and cancellation cannot change published bytes.

The worker-count leg is enforced by `worker_count_cannot_change_canonical_result_bytes`, which
round-robins popped payloads across N worker vectors, flattens them, then calls
`merged.sort_unstable()` before comparing. Sorting the merged output makes the assertion
insensitive to any ordering the scheduler produced — the test passes for any worker count even
if the queue were nondeterministic, as long as the same multiset of jobs is popped. Source-order
and shuffled-insertion legs are genuinely tested (`source_order_cannot_change_resolved_bytes`,
`shuffled_queue_insertion_has_one_pop_order`); the worker-count leg is not.

*Evidence:* engine/crates/spatial/src/residency.rs:677 `worker_count_cannot_change_canonical_result_bytes` (line 700 `merged.sort_unstable()`)

**phase-1-spatial-numeric-foundation** — A late result with an old generation token is discarded in a named race test.

The test that discards a stale token — `late_generation_is_rejected_without_partial_publication`
— is fully sequential: it calls `begin(10)`, then `begin(11)`, then `try_publish(old, …)` on one
thread. No concurrency is involved, so it is not a race test. The only multi-threaded test in
the module, `readers_observe_only_complete_generations`, spawns a writer but asserts torn-read
freedom, not late-token rejection. `grep -rn 'thread::spawn'
engine/crates/{spatial,vegetation}/src` returns exactly that one site. The behaviour is covered;
the 'race test' wording on the box is not backed by a concurrent test.

*Evidence:* engine/crates/spatial/src/residency.rs:574 `late_generation_is_rejected_without_partial_publication`; :594 `readers_observe_only_complete_generations`

**phase-2-domain-assets-mutations** — Integer-exact placement/growth determinism (cross-target bit-exactness of the integer trigonometry).

Two independent integer trig routines both feed cooked/persisted decisions, and neither has a
byte-golden test. `turn_sin_cos` (botanical.rs) is a 17-entry quarter-turn table with truncating
slot selection, called from botanical.rs, botanical_compile.rs, botanical_edit.rs.
`cordic_sin_cos` (evaluator.rs) is a separate 16-iteration Q30 CORDIC, called from
`yaw_quaternion_q15` (which produces the `QuantizedOrientation` written into the cooked point
orientation column) and from the random-direction path at evaluator.rs:14029. Neither has a
pinned-literal test — the closest, `a_graph_grows_the_same_plant_every_time`, only asserts
`grow(doc) == grow(doc)` within one process, which proves purity, not cross-target byte
equality. Contrast Philox and plant identity, which do carry pinned digests
(`domain_golden_is_pinned`, `procedural_id_golden_is_pinned_and_round_trips`). Separately,
`engine/crates/vegetation/AGENTS.md` states 'Trigonometry is an integer table' and names only
`turn_sin_cos`, and the `CookVersionSet` bump guidance likewise names only `turn_sin_cos` — the
CORDIC is undocumented in both (its identity is covered incidentally by the `evaluator` version
field). No box in phases 1–2 literally claims a trig golden, so this is a gap in the determinism
foundation those phases establish rather than a broken checkbox.

*Evidence:* engine/crates/vegetation/src/botanical.rs:1992 `turn_sin_cos`; engine/crates/vegetation/src/evaluator.rs:14428 `cordic_sin_cos`, :14418 `yaw_quaternion_q15`, :14029; engine/crates/vegetation/src/botanical.rs:2288 `a_graph_grows_the_same_plant_every_time`; engine/crates/vegetation/AGENTS.md 'Trigonometry is an integer table' and 'CookVersionSet::current()' bullets

**phase-5-runtime-cells-persistence** — Acceptance: Multiple moving spatial sources produce stable refcounts and no unload/load thrash beyond the specified hysteresis.

Hysteresis is implemented (`ResidencyManager::update_source` retains previous claims that still
fall inside `cleanup_radius_cells`), and there is a determinism test for two static sources. But
the only motion test drives a SINGLE source, and its helper pins `velocity_mps: DVec3::ZERO` /
`prediction_seconds: 0.0`. No test moves two or more sources and asserts refcount stability or
absence of thrash, which is exactly what the box claims.

*Evidence:* engine/crates/spatial/src/residency.rs:557 `cleanup_radius_retains_then_releases_cells` (one source), :545 `source_order_cannot_change_resolved_bytes` (two static sources), test helper `fn source(...)` at :527 hardcodes zero velocity

**phase-5-runtime-cells-persistence** — Acceptance: CPU memory and query work scale with resident cell facets/macro plants, never micro blade count.

The design supports it — micro vegetation is stored as `MicroFieldTile` quantized tiles plus
disturbance masks, and `MacroBvh` is built only over macro rows — but there is no test,
benchmark, or telemetry assertion anywhere that pins the scaling claim. It is ticked on the
strength of the data layout alone.

*Evidence:* engine/crates/vegetation/src/runtime_world.rs:2106 `struct MacroBvh` (built from `macro_points.bounds` only, :413); `micro_fields()` at :452; no test in the module's `mod tests` (11 tests, listed at :2561-:2989) touches memory/scale

**phase-5-runtime-cells-persistence** — Keep physics raycast semantics separate: `sa.raycast` must not silently start hitting non-collidable grass.

The two query surfaces are genuinely separate (`VegetationWorld::query_ray` vs the physics
`raycast` command / Luau `sa.raycast`), and collision proxies are gated on `InteractionPolicy`
in `saffron-runtime`. But the box states a negative that nothing enforces: no test asserts that
a raycast into a cell full of non-collidable micro/decorative vegetation returns no hit. A
regression that started registering grass proxies would pass the whole suite.

*Evidence:* engine/crates/vegetation/src/runtime_world.rs:1137 `query_ray`; engine/crates/runtime/src/vegetation_collision.rs:161/:190 (policy gating); tests/e2e/physics-query.test.ts contains no vegetation case (no `vegetation` reference in the file)

**phase-6-virtual-geometry-render-substrate** — Focused verification: The portable executor can render every cooked representation in an isolated test on MoltenVK.

The isolated GPU test `executor_draws_the_binned_cut_depth_only` cooks `cooked_quad()` — a
contiguous 2-triangle quad — so it exercises only the TriangleCluster representation.
`GpuRepresentation::AggregateVoxel` is proven to reach a draw record in
`traversal_emits_cut_records_and_requests_missing_children`, but nothing rasterizes it in an
isolated test; its only rendered coverage is the full-frame e2e capture `vegetation-
representation-parity.test.ts`, which needs a booted host and a pinned `SAFFRON_CUT_OVERRIDE`.
`GpuRepresentation::MicroBlade` is rendered in neither. The test also skips silently when no
Vulkan device is obtainable, so a green `cargo test` proves nothing here.

*Evidence:* engine/crates/rendering/src/visibility.rs:2542 `fn cooked_quad()` (contiguous mesh → triangle path), :3311 `executor_draws_the_binned_cut_depth_only`, :2571 `traversal_emits_cut_records_and_requests_missing_children` (record assertion only); tests/e2e/vegetation-representation-parity.test.ts:128

**phase-7-gpu-scene-visibility-cutover** — "`scene_executor_depth_mesh.slang` compiling to SPIR-V (mesh + fragment) … `Pipelines::request_scene_executor_depth_mesh` building a mesh-stage PSO … and `record_executor_bucket_draw_mesh` issuing the counted dispatch" — listed as BUILT.

The depth-mesh shader and its PSO exist but are never reached from a production path: the sole
caller of `request_scene_executor_depth_mesh` is the in-crate unit test. The shipped mesh
executor is `mesh.slang`'s `meshMainExecutor`, selected through `PsoKey.mesh_shader` in
`request_executor_mesh_pipeline`. A shader + pipeline that exists only to make one
`#[cfg(test)]` fixture render is not production code, and the docs table names it as "The mesh
executor".

*Evidence:* engine/crates/rendering/src/pipelines.rs:1675 `request_scene_executor_depth_mesh`; its only call site is engine/crates/rendering/src/visibility.rs:3354 inside `fn executor_draws_the_binned_cut_depth_only`. Production selection is pipelines.rs:2510 `if key.mesh_shader { (MESH_EXT, c"meshMainExecutor") }`.

**phase-7-gpu-scene-visibility-cutover** — "rendering frames bit-identical to the indexed path" / acceptance: "measures meanAbs 0 — bit-identical, not merely within tolerance".

The e2e test enforces a mean-absolute-difference tolerance of 1.0, not zero. A future divergence
up to ~1 LSB per channel would pass while the plan records the result as bit-identity. The rest
of the test (executor-actually-differed control, non-flat-frame control) is genuinely well
built; only the bit-identity wording overstates what is enforced.

*Evidence:* tests/e2e/mesh-executor-parity.test.ts:33 `const PARITY_TOLERANCE = 1.0;` and :103 `expect(meanAbsoluteDifference(indexed, mesh)).toBeLessThan(PARITY_TOLERANCE);`

**phase-7-gpu-scene-visibility-cutover** — Acceptance: "Rapid camera motion, camera cuts, teleports, resize, wind-bound stress, and page churn produce no HZB disappearance, hole, or stale-handle alias (the camera-churn e2e in `gpu-scene-residency.test.ts` … validation-clean)".

Two problems. (1) The cited file never asserts validation cleanliness — it contains no
`validationErrors()` call, and its in-file comment "(the harness asserts the log stays
validation-clean at shutdown)" is factually wrong: `Engine.shutdown()` only sends `quit`,
SIGTERMs the process and removes the temp dir. The e2e AGENTS.md convention ("Assert on
validationErrors()") is unmet for exactly the test the box leans on. (2) The file covers camera
cuts/teleports only — 101 lines, three tests, one cube. Resize, wind-bound stress, and page
churn are not exercised there; wind stress happens to be covered elsewhere (vsm-churn.test.ts,
vegetation-churn.test.ts, which do assert `validationErrors()`), but the box's cited evidence
does not carry the claim it is cited for.

*Evidence:* tests/e2e/gpu-scene-residency.test.ts (101 lines; no `validationErrors` occurrence; comment at :64-67 asserting a harness behaviour that does not exist) vs tests/e2e/harness.ts:346 `async shutdown()` — `call("quit")`, `proc.kill`, `cleanupAppdata()`, no assertion.

**phase-7-gpu-scene-visibility-cutover** — Acceptance: "Transparent sorting is GPU-driven and **stable** under hierarchy/stream changes (stable LSD radix + per-blend-bucket zero-masked streams; known-depth GPU ordering test)".

The GPU sort itself is real and the ordering test is a good one, but it proves back-to-front
ordering of three records at three distinct known depths in a single frame. Nothing asserts the
stability half: no test gives two records an equal sort key and checks their relative order is
preserved, and no test perturbs the hierarchy or the record stream across frames and checks the
order does not shuffle. Stability rests entirely on "LSD radix is stable by construction".

*Evidence:* engine/crates/rendering/src/visibility.rs:3866 `fn transparent_records_sort_back_to_front` — assertions at :4209-4224 are `far_last < middle_first` and `middle_last < near_first` over three distinct depths; no equal-key or cross-frame case anywhere in the file.

**phase-11-virtual-shadows-lighting-rt** — Derive optional `VK_KHR_opacity_micromap` data from the exact coverage texture/mip/classification source and validate conservative/unknown states.

The derivation, the cook stage, the build and the attachment are all genuinely present and the
EXT-instead-of-KHR substitution is argued with measured reasons, so this is not a hollow tick.
The weakness is the proof: every assertion in the e2e that measures the box's claim is behind an
early `return` on `!stats.rtSupported || !stats.ommSupported`, so on MoltenVK (the branch
currently checked out) and on llvmpipe the tests pass while asserting nothing. The box's
headline result (`meanAbsoluteDifference == 0`, `ommMicromaps > 0`) exists only as a one-machine
measurement on the RTX 3070 Ti with no way for the standing gate to detect a regression on any
other target.

*Evidence:* tests/e2e/vegetation-atlas-micromap.test.ts:251-263 (`if (!stats.rtSupported || !stats.ommSupported) { return; }` guarding the `meanAbsoluteDifference(on, off)).toBe(0)` assertion); same guard at :215 and :241; engine/crates/rendering/src/device.rs probes `ext::opacity_micromap` (no KHR path exists)

**phase-8-vegetation-rendering** — Every raster pass (`scene_executor_depth`/`gbuffer`/`motion`/`mesh` forward + masked prepass) tests the same frame-free dither via `gpuTransitionCovered`.

`scene_executor_depth.slang` and `scene_executor_depth_mesh.slang` are not production raster
passes — their pipeline builders are requested only from a `#[cfg(test)]` block. Citing them
first in the list of "every raster pass" overstates the coverage. The substance still holds: the
production passes (gbuffer, motion, mesh forward, and the real masked depth prepass
`depthPrepassFragment`) do all call `gpuTransitionCovered`. Worth noting as a second depth-
executor shader pair kept alive beside the production `depth_prepass_executor` path.

*Evidence:* engine/crates/rendering/src/pipelines.rs:1553 `request_scene_executor_depth` and :1675 `request_scene_executor_depth_mesh` have exactly two call sites — engine/crates/rendering/src/visibility.rs:3348 and :3354 — both inside `mod tests` which opens at visibility.rs:2302. The production prepass is renderer.rs:6226 `request_depth_prepass_executor` / :6235 `request_depth_prepass`, built from mesh.spv (pipelines.rs `build_depth_prepass`).

**phase-12-interaction-physics-queries-nav** — ...lifecycle/health/moisture/fuel/ecology-tick via the new runtime-only `PlantVitals` ... Demotion ... write back ... a `StateOverride` from the settled vitals

`PlantVitals` has no writer anywhere in the tree except the promotion module that creates it.
Grepping `PlantVitals` across engine/crates and editor/src returns only its definition
(scene/component.rs:157), the scene re-export (scene/lib.rs:33), and vegetation_promotion.rs. It
is unregistered so `set-component` cannot reach it, there is no script binding for it, and no
gameplay/ecology path touches it. The write-back therefore always reproduces the exact values
`spawn_view` copied in — 'whatever the view's vitals settled at' can never differ from what
promotion wrote. The round trip is real code but has no producer of change.

*Evidence:* engine/crates/scene/src/component.rs:157 PlantVitals; engine/crates/runtime/src/vegetation_promotion.rs:643 (write) and :484 (read); `rg PlantVitals engine/crates editor/src` returns no other file

**phase-12-interaction-physics-queries-nav** — Acceptance: Promotion stress proves exactly one render/collision/simulation owner at every synchronization point, including save/unload/recook during transition.

The e2e proves the promote/demote leg only. `tests/e2e/vegetation-interaction.test.ts` asserts
residentBodies drops on promotion, nav flips to dynamic, and residentBodies returns exactly on
demotion — but it never saves during a promotion, never unloads or republishes the cell while a
view is live, and never recooks. The 'including save/unload/recook during transition' clause is
argued structurally in the plan text, not proven by any test in the tree.

*Evidence:* tests/e2e/vegetation-interaction.test.ts:171-230 (promote/demote block); no save-project, no re-cook, no residency drop inside the promoted window anywhere in the file

**phase-12-interaction-physics-queries-nav** — Acceptance: Demotion writes state back and reload preserves it; cache recook cannot resurrect removed plants.

Only the first clause is tested: the e2e polls until `inspect.persistent.some(entry =>
entry.promoted)`. 'Reload preserves it' and 'cache recook cannot resurrect removed plants' are
asserted by argument in the plan note; no test reloads the project after a demotion and re-reads
the state, and no test recooks a cell carrying a tombstone/stump delta and checks the plant
stays gone (`rg -l 'tombstone|resurrect|recook' tests/e2e/*.test.ts` matches only vegetation-
graph.test.ts, which does not cover this).

*Evidence:* tests/e2e/vegetation-interaction.test.ts:204-230; no reload/recook assertion in tests/e2e/

**phase-12-interaction-physics-queries-nav** — Acceptance: Grass has no body/nav-object explosion; macro nav contributions update only affected regions ... e2e sees dirty regions appear on the promotion of ONE plant, not a whole-cell invalidation.

The grass half is solid (collision/navigation sections are encoded from `macro_points` only, and
Decorative yields no body and no contribution). The granularity half does not match the code:
`VegetationNavigationSeam::advance` retires the WHOLE `NavCell` whenever `cell_bulk_revision`
moves, marks every one of its contributions' bounds dirty, re-derives all of them, and marks
them all dirty again. Promoting one plant therefore dirties every contribution's bounds in that
cell — it is a whole-cell re-publication, coalesced. The e2e only asserts
`nav.dirtyRegions.length > 0`, which cannot distinguish the two.

*Evidence:* engine/crates/runtime/src/vegetation_navigation.rs:102-146 (whole-cell retire + re-mark); tests/e2e/vegetation-interaction.test.ts:199 `expect(nav.dirtyRegions.length).toBeGreaterThan(0)`; engine/crates/vegetation/src/evaluator.rs:1388 encode_collision_inputs (macro_points only)

**phase-13-ecology-catchup** — Acceptance: Results are identical across worker counts, shuffled plant/cell order, different residency paths, and origin rebasing.

Region/plant/neighbour order and the residency path are genuinely covered by real tests. 'Across
worker counts' is vacuous rather than proven — there is no parallel execution path at all:
`advance_ecology` loops regions on the calling thread. 'Origin rebasing' is argued from the type
vocabulary with no test. Both are reasoning, not coverage, on a box whose whole point is
determinism enforcement.

*Evidence:* engine/crates/vegetation/src/runtime_world.rs:1363-1387 (single-threaded region loop); disjoint_regions_commit_the_same_state_in_either_order (ecology_region.rs:546) covers order only; no origin-rebasing test in the crate

**phase-13-ecology-catchup** — `sa` drives and dumps. `vegetation-advance-ecology` ... `vegetation-ecology-status` dumps world time, the rule-set version, the checkpoint hash, every dependency region ... and every cell's boundary summary.

Both commands are reachable through `sa`'s generic passthrough and the data is all in the reply,
but neither has a text formatter — they fall through to `_ => vec![pretty(result)]`, a raw JSON
dump. Phase 12 built dedicated formatters for its equivalents (`format_vegetation_nav`,
`format_vegetation_events`, `format_promotion`); the ecology commands got none, so 'dumps ...
every dependency region with its tick and residency' is satisfied only as unformatted JSON.

*Evidence:* engine/crates/sa/src/main.rs:550 `_ => vec![pretty(result)]`; `rg 'ecology|combustion' engine/crates/sa/src/main.rs` matches only the telemetry formatter fields at :849 and :867

**phase-13-ecology-catchup** — Acceptance: Standard gate, determinism/property tests, and lifecycle/ecology docs are green.

The determinism coverage is real and better than claimed (24 tests across
ecology.rs/ecology_tick.rs/ecology_region.rs plus the three catch-up acceptance tests over a
live world, and `catch_up_equals_continuous_simulation` really does compare `canonical_bytes()`,
not just the checkpoint hash). The gate claim itself is unverifiable here and already self-
qualified ('green apart from one xtask shader test'). Separately the docs it calls green are out
of step with the code: `vegetation-telemetry.md` states 'an ecology tick records itself as it
executes' and shows `ecologyTicks=96`, but no production code calls `record_ecology_ticks`, so
that counter is always zero.

*Evidence:* docs/content/explanations/scene-and-ecs/vegetation-telemetry.md:34, :48; engine/crates/runtime/src/vegetation_telemetry.rs:144 (no production caller); engine/crates/vegetation/src/runtime_world.rs:2944-2948 (byte comparison, confirmed)

**(d) Docs currency — pages + hub _index.md rows for every vegetation concept** — hub rows carry accurate `What | File | Symbols` code pointers

The geometry-and-assets hub row for `plant-rendering` attributes both symbols to one file, but
only one lives there. `sync_vegetation` is a `GpuSceneMirror` method in gpu_scene_mirror.rs, not
a plant_render.rs function. The page body itself gets it right, so the hub row is the drifted
copy. Every other hub row I checked resolves, and every docs page under
docs/content/explanations/ has a hub row (checked across all 21 hubs).

*Evidence:* docs/content/explanations/geometry-and-assets/_index.md:39 cites "`assets/src/plant_render.rs` · `load_plant_family`, `sync_vegetation`". Actual: engine/crates/assets/src/plant_render.rs:109 `load_plant_family`; engine/crates/assets/src/gpu_scene_mirror.rs:1073 `pub fn sync_vegetation`. The page body is correct: docs/content/explanations/geometry-and-assets/plant-rendering.md:226.

**(f) Planset self-report consistency — README.md / READMESUMMARY.md internal coherence and gate evidence** — the Milestone rule — every phase ends with `just engine` / `prepare-for-commit` / `schema` / `test` / `e2e`

No recorded gate covers the tree as it now stands. The last one is dated 2026-07-27 at "just e2e
370/370 across 64 files"; the suite now holds 69 `.test.ts` files, five of them written after
that record, and the final "closed" section (2026-07-29) — which claims the last three boxes
shut, including the partitioned-TLAS work — records no gate figure at all. So the closing claims
rest on prose rather than on a run. (I did not run the gate; the orchestrator does that.)

*Evidence:* plans/foliage-veg/READMESUMMARY.md:286 (last gate line) and :288-316 (final section, no gate). `ls tests/e2e/*.test.ts | wc -l` → 69. Written after 2026-07-27: tests/e2e/rt-ptlas.test.ts (07-29 15:47), vegetation-canopy.test.ts (07-29 07:24), rt-telemetry.test.ts (07-29 07:23), vegetation-churn.test.ts (07-28 19:47), visibility-counters.test.ts (07-28 08:59).

**(b) Forbidden constructs — the planset's "does not exist anywhere" list** — engine/crates/vegetation/AGENTS.md: "Thirty-two modules, all declared private in `lib.rs`" with a cluster table covering them

There are thirty-three modules, and the cluster table lists thirty-one — `cell_facet` and
`cook_work` appear in neither the count nor the table (`cell_facet` is named later in the rules
section, `cook_work` is not named at all). Every hard tripwire this file states does hold: no
HashMap/HashSet, no `rand::`, no `SystemTime` in the crate, and no `saffron_vegetation`
reference under engine/crates/rendering/.

*Evidence:* engine/crates/vegetation/AGENTS.md:19-33. `ls engine/crates/vegetation/src/*.rs | wc -l` → 34 (33 modules + lib.rs). Missing from the table: engine/crates/vegetation/src/cell_facet.rs, engine/crates/vegetation/src/cook_work.rs.

**phase-15-production-platform-closure** — "Compare representative images and error metrics across executors/platforms" — evidence `tests/e2e/cross-adapter-parity.test.ts`.

Two problems. (1) The box ends with a trailing parenthetical from an earlier stratum — "(Blocked
by the same gate: a cross-platform image comparison needs at least two platforms...)" — that
flatly contradicts "THE CROSS-ADAPTER CAPTURE NOW EXISTS" four paragraphs above it, inside the
same ticked box. (2) The evidence self-disables: when `bothAdaptersPresent` is false the
comparison tests `return` early after a `console.warn`, so all four tests report PASS on a
single-adapter machine. A green `just e2e` therefore does not prove the cross-adapter comparison
ever ran, which means the box's headline number ("365/365") cannot distinguish "portability
verified" from "portability skipped". The box calls the skip "loud", but a bun `test()` that
returns is green, not loud.

*Evidence:* tests/e2e/cross-adapter-parity.test.ts:75-129 (`bothAdaptersPresent` computed in beforeAll; `if (!bothAdaptersPresent) { return; }` in the three comparison tests); tests/e2e/image.ts:124 `regionMean`, :147 `meanAbsoluteDifference`; plans/foliage-veg/phase-15-production-platform-closure.md:396-399

## Corrections to this planset

The Status lines are left as they stand; changing fifteen COMPLETED markers is the project
owner's call, not the auditor's.

Done, in the planset prose:

1. The superseded strata are gone. Every box that carried an older paragraph beneath a newer one
   now states one answer: phase 3's acceptance and progress notes, phase 11's GI/reflection
   culling, RT telemetry and opacity-micromap boxes, phase 12's suite-state clause, phase 14's
   generator box, and phase 15's work-item-manifest, GPU-telemetry, image-comparison and
   platform-tier boxes.
2. No box quotes a test total. Each claim names the gate recipe or the test that carries it —
   `load_query_unload_and_reload_preserve_persistent_tombstones` for the snapshot round-trip,
   `mesh-executor-parity` for executor agreement, `the_depth_prepass_rasterizes_every_cooked_representation`
   for representation coverage, and so on. `READMEFABLE.md` keeps its dated per-slice seals: they
   are a log of what a run measured on a day, not a claim about the suite today.
3. The invented symbols are corrected. `EcologyClock::advance_to` plus
   `EcologyState::publish_region_tick` carry the monotonic-tick guarantee (there is no `ticks_to`
   and no `complete`); the command is `list-active-alarms`; phase 8 cites the device test that
   exists; the depth family covers the `vsm-pages` shadow pass rather than a point-shadow raster
   pass that was retired; phase 2 lists `saffron-material` among the vegetation crate's
   dependencies; phase 10 names `gpuSceneWindDeform`.
4. Claims the code contradicts are corrected: the mesh executor is opt-in through
   `SAFFRON_MESH_EXECUTOR=1` on a device whose `Capabilities::mesh_shader` holds, and the optional
   `VK_NV_cluster_acceleration_structure` tier is built (`rt_cluster.rs`, `vk_nv_cluster.rs`,
   `clusterAsSupported`/`clusterBlasCount`/`clasCount`) rather than absent.
5. `READMESUMMARY.md` is a map again — phase table, platform coverage, the opt-in switches and
   their harnesses, how to verify, and the one open defect — instead of a chronological log whose
   later sections contradicted its header.

Done outside the planset, where the same claims sat in code comments and docs pages:

6. `record_executor_depth_family`'s doc comment (`rendering/src/scene_pass.rs`) names the
   `vsm-pages` shadow pass instead of the retired point-shadow raster pass, and the per-bucket
   index stream `bucket_index_buffer` selects instead of the pages arena alone.
7. `hierarchical-visibility.md`'s mesh-executor section states the real selection —
   `SAFFRON_MESH_EXECUTOR=1` plus a device that offers a mesh stage, which MoltenVK does not — and
   the page's code-pointer row names `mesh.slang`, `meshMainExecutor`, `PsoKey::mesh_shader` and
   `SAFFRON_MESH_EXECUTOR`.

Still open, and none of it is prose:

1. `vegetation-state-export` / `vegetation-state-import` have no harness. Both carry a manifest
   `skip`, no `tests/e2e` file dispatches them, and the control crate's own tests round-trip only
   the hex helper. The world-level `export_state_snapshot`/`import_state_snapshot` pair is covered;
   the command layer over it is not.
2. `tools/check-control-schema/check.ts` pushes a skipped command onto its `checked` list, so the
   run's own count overstates what it dispatched — every `vegetation-*` and `plant-*` command is
   skipped.
3. Capability-gated e2e tests return early instead of skipping, so a green run does not distinguish
   "verified" from "not attempted". Twelve files carry the shape, and the planset cites eleven of
   them as hardware-tier evidence: `cross-adapter-parity`, `mesh-executor-parity`, `rt-telemetry`,
   `vegetation-rt`, `vegetation-atlas-micromap`, `vegetation-canopy`, `rt-anyhit`, `rt-blas`,
   `rt-ptlas`, `skinned-rt`, `perf`, `toggles`. None uses `test.skipIf`; each gates inside the test
   body and returns. On MoltenVK, where most of the planset's seals were taken, every one of those
   bodies passes having asserted nothing. The fix has two halves: declare the comparison with
   `test.skipIf(capability)` so the runner reports it skipped, and add a test that always runs and
   asserts the capability gate itself — for `mesh-executor-parity`, that booting with
   `SAFFRON_MESH_EXECUTOR=1` on a device without a mesh stage leaves `meshExecutor` false and still
   draws.
4. Two dead symbols still cited by docs pages. `software-ray-trace.md`'s occluder-pressure row
   points at `set_sdf_scene` and `MAX_SDF_INSTANCES`, both deleted with the CPU occluder upload
   (`sdf_instances_dropped` is what survives). `wind-field.md` and `persistent-gpu-scene.md` name
   `gpuSceneWindSway`, which is `gpuSceneWindDeform`.
5. Phase 15's three box-less sections (networking contract, determinism and failure matrix,
   performance closure) carry requirements with nothing to tick against.
6. `just schema` intermittently loses the NVIDIA device in the thumbnail render path. Phase 15's
   terminal box carries the bisection.

## Already acted on

Five dead or duplicated paths this audit found were removed in the same sweep, each confirmed
unreferenced by a repo-wide grep first: rendering's count_scan_scatter.rs (a second CPU
implementation of the GPU binner's job), spatial's SpatialJobQueue/JobPriorityKey/JobEntry,
spatial's fixture.rs and its two self-affirming tests,
evaluator::precompute_surface_projection_tile, and CookGraph::invalidated_nodes with its test.
The admission order the queue was meant to provide now lives once, as
ResidencySnapshot::admission_order in saffron-spatial, and vegetation's admitted_masks calls it
instead of re-sorting inline. AGENTS.md's Status section and the components reference were
corrected against the tree, and 232 docs code-pointers that named files the module splits turned
into directories were repointed.

One finding was left deliberately: tess_draws is a genuine second production draw path, one CPU-
built row per displaced instance carrying its own vertex-input PSO and replayed by every raster
pass after its executor buckets. Routing displaced instances through the executor is a design
change, not a cleanup. SceneDrawList, which the plan's delete list also names, turned out to be
the per-frame dispatch container rather than a resurrected CPU draw list; that line is stale
prose.

