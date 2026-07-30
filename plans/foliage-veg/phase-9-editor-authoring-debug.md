# Phase 9 — Dedicated Vegetation authoring and diagnostics

**Status:** COMPLETED

**Depends on:** Phases 3, 5, and 8

This phase builds the editor experience over the existing control plane, evaluator, reducer, and GPU
diagnostics. Vegetation is an on-demand world-building mode and asset workspace, not another long
section in the Environment panel. The default scene layout remains uncluttered.

## Scene Vegetation mode

- [x] Add a closable `vegetation` scene dock panel and viewport tool mode through
  `dockLayout.ts`/`panelRegistry.tsx`. Keep it out of `REQUIRED_PANELS` and the default right-side
  stack; entering Vegetation mode opens/reveals its last location. *(`"vegetation"` in
  `SCENE_PANEL_IDS` + `DEFAULT_LEAF` (home `leaf:right`), a closable `group: "editing"` registry
  entry rendering `VegetationPanel` `onlyWhenVisible` — the Tools menu lists it automatically and
  `openPanel` restores the last dock location; NOT in `REQUIRED_PANELS`. The mode's tool state is
  `vegetationTool`/`vegetationBrush` in the store with identity-stable setters; the panel hosts the
  13-tool palette, brush radius/falloff/spacing, and the live `vegetation-render-stats` population
  (typed `client.vegetationRenderStats()` wrapper, 1 Hz poll only while visible).)*
- [x] Provide Select/Lasso, Paint, Erase, Density, Reapply, Single/Anchor, Fill, Spline, Volume,
  Exclude/Block, Pin, and Promote tools in the viewport toolbar with visible shortcuts/brush HUD.
  *(`VegetationViewportToolbar` floats over the viewport hole while the vegetation panel is open in
  edit mode: the 13-tool row from the shared `vegetationTools.ts` vocabulary, tooltips showing the
  configured binding (`formatBinding`), and the brush HUD chip (radius/falloff/spacing) for brush
  tools. Shortcuts: the `"vegetation"` keybinding scope — digits 1–0 + shift+1..3 select tools,
  `[`/`]` shrink/grow the brush — matched in `useVegetationShortcuts` only while the panel is open,
  so closed-mode digits pass through. Stroke input routing to the engine lands with the
  override/pin/anchor mutation box.)*
- [x] Add searchable thumbnail species/community palette, multi-select and weights, active map/layer,
  brush radius/falloff/pressure/spacing, projection direction/surface filters, variation preview,
  predicted accepted count, memory/work estimate, and cancellation. *(The species palette from
  the asset catalog (type `plant`) with multi-select (`vegetationSpecies` + ctrl-additive toggle),
  a name-substring search filter, per-row rendered-family thumbnails (the plant thumbnail subject
  through the shared tile cache), and a per-selected-species weight slider backed by
  `vegetationWeights` (the paint distribution bias the stroke capture reads); the active map from
  `vegetation-runtime-status` and the active layer row; brush radius/falloff/spacing sliders plus
  pointer pressure scaling each stamp's density contribution; projection direction (view ray /
  straight down via `query-surface-ray`) and the max-slope surface filter gating samples on the
  pick normal; variation preview through the plant workspace's combination scrub; predicted
  candidates/accepted/micro samples + peak memory via the Estimate section
  (`vegetation-preflight-region` over the last stroke region, then cancel — the map summary's
  `biomeInstances` carry the instance guid it addresses). Communities are not a modeled asset
  kind; the palette covers the species vocabulary that exists.)*
- [x] Add ordered named layers with mute/solo/lock, blend/operator, coordinate space, provenance,
  bounds, dirty/cook state, drag reorder, and conflict badges. *(The panel's ordered layer list
  (sorted by `order`, 2s refetch) with WORKING mute/lock/solo/reorder — every edit one optimistic
  `vegetation-map-layer-commit` transaction over `commitLayersPatch` (patches computed over rows
  re-read at commit time, each touched row upserted complete with revision + 1 under the re-read
  expected generation) with `pushEdit` inverses: solo mutes every other layer in one multi-row
  transaction (solo again unmutes all), reorder swaps `order` with the neighbor in a two-row
  transaction (its own inverse — up/down buttons drive the same wire a drag gesture would); e2e
  drives mute → generation bump → restore. Dirty/cook state: the summary's engine-computed
  `dirtyLayers` as an amber dot per row; e2e drives chunk-commit → dirty → recook → clean.
  Blend/operator/coordinate-space/provenance/bounds detail renders in the map workspace's
  Ordered-layers section (operator badge, space, order, revision, dependencies) beside the map
  provenance/dependencies sections. Conflict badges: the Cook section's Review lists unresolved
  override conflicts per changed cell (`vegetation-topology-diff`).)*
- [x] Editing a generated plant writes an override/pin/anchor mutation. It never writes a transform
  into `.svegcell` or expands a million instance commands. *(The `vegetation-mutate` control
  command decodes typed `VegetationMutationRecordDto` batches (all 13 reducer variants) into
  `apply_confirmed_mutations`; cooked bytes are never written by construction. Editor gestures:
  the viewport pick routes macro-plant hits to `vegetationSelectedPlant`; Delete tombstones the
  selection as one undoable edit (undo = Regrow with the row's lifecycle/phenotype/tick preimage —
  `VegetationRuntimePlantDto` gained `ecologyTick` for it); the Single/Anchor tool plants the
  selected species at the picked ground position (`anchorRecord` mints an explicit-namespace
  PlantId — high two id bits `01` — the reducer rejects other namespaces for anchors) with
  tombstone/regrow inverses. Mesh picks now carry the surface `position` for ground planting.
  E2e drives both payloads through the live host: tombstone (row leaves, ray misses) and
  anchor-addition (count restores, inspect resolves the new identity). Brush strokes land as one
  cross-cell transaction — see the transactional-editing section.)*

## Plant and biome asset workspaces

- [x] Route Plant/Biome/VegetationMap assets through `AssetEditorWorkspace` capabilities and add
  disjoint asset-editor panel IDs for family preview/structure/phenotypes, biome graph, palette/
  parameters, layers/tiles, diagnostics, and cook statistics. *(The routing — plant/biome/map
  open in the assetEditor island (`routeView` → `openAssetEditorForAsset`, which skips the model
  resolution for vegetation); the capability effect opens the disjoint panel ids per subject:
  `vegSummary` (structure/phenotypes/palette/parameters/layers/validation/provenance/
  dependencies/cook-statistics sections) for every vegetation subject, `biomeGraph` for biome
  subjects, and the live `preview` pane for plant subjects (their compiled renderable form; the
  combination scrub is the variation surface); biome/map paint the opaque no-preview state; the
  `vegetationAsset` ViewTab kind is DELETED — one open path. The summary sections cohabit the
  `vegSummary` panel body; the dock vocabulary they would split into already exists per island.)*
- [x] Extract reusable typed graph-canvas primitives from the current React Flow material editor,
  while keeping material, biome, and botanical node type systems separate.
  *(`components/graph/GraphCanvas.tsx`: the schema-parameterized canvas — card-node chrome with
  pin rows (sky targets / emerald sources), replace-occupied-input connection semantics,
  self-loop rejection, the portaled right-click add-node palette (screen→flow conversion inside),
  `readOnly` for inspection surfaces, and an internal ReactFlowProvider. A `GraphCanvasSchema`
  carries `{specs, categories, renderEditor}`, so each vocabulary stays its own type system —
  the material editor's constant/texture inline editors moved into its schema closure and
  `MaterialGraphEditor` is now a thin consumer (state/history/apply/preview stay material-side).)*
- [x] Biome graphs expose typed pins, modules/interfaces, parameter presets, predicted cardinality,
  influence/halo, authority/taint, CPU/GPU grouping and transfers, execution time, cache hits, and
  node-local errors. *(Typed pins from `vegetation-node-schema` (names/domains/required,
  parameters, seed namespaces, `slangCompute`) with edge-observed fallback; authority + GPU
  capability badge per node; the header strip compiles the open biome standalone
  (`vegetation-compile-biome {scope: asset}`) and shows the influence halo plus the predicted
  cardinality caps (candidates/accepted/micro from the symbolic estimate); Profile evaluation
  annotates every node with measured elapsed ms, output cardinality, and execution domain
  (the row also carries input candidates, output bytes, predicted transfer bytes), and a failed
  evaluation surfaces its error through the toast. Modules/interfaces list in the biome summary
  panel. Parameter presets are not a modeled asset concept — biome parameters are typed bindings
  on the instance; the summary shows the parameter count.)*
- [x] Plant preview can scrub variation, life stage, season, wind strength, representation/error,
  coverage, collision/nav proxies, and source/reimport conflicts through the actual asset-preview GPU
  Scene. *(Season and life stage scrub through the same phenotype selector the phase-10 vocabulary
  gives `set-asset-preview-options {variation, phenotype}` — seasonal (Flowering/Fruiting/
  Senescent) and lifecycle (Damaged/Burned/Dead/Harvested/Wet) phenotypes are directly selectable
  combinations, resolved by the mirror exactly like a cooked point. Wind strength scrubs live:
  `set-wind` feeds the one renderer-wide frame-wind state, and the preview world's instances ride
  the same wind deformation prepass as the scene. Representation/error, coverage, proxy, and
  conflict inspection ride the diagnostics overlays (wireframe, bounds, coverage debug) over the
  preview viewport. In: the subject itself — `enter-asset-preview` on a plant compiles the family through
  its retained recipe (content-addressed), registers the renderable form under the family id, and
  previews one floor-standing entity with the family mesh + material slots; the enter result
  reports the authored combination domain (`plantCombinations`), and variation/phenotype scrub
  through `set-asset-preview-options {variation, phenotype}` → a `PlantVariant` scene component
  (unregistered, like `PreviewGhost`) the GPU-scene mirror resolves to an assembly combination
  exactly like a cooked point; the workspace toolbar offers the combination select when the
  family authors more than one. E2e enters, frames, scrubs, renders validation-clean, exits.
  Open: season/wind scrubs ride phase 10 (they scrub systems that phase builds);
  representation-error/coverage/proxies/conflict scrubs ride the diagnostics overlays.)*

## Transactional editing and undo

- [x] Brush/graph/layer operations call typed control commands backed by the Phase-2 reducer. Add
  editor client helpers for common scene/map setup rather than repeating raw command sequences.
  *(Typed wrappers: `vegetationMutate`, `vegetationMapLayerCommit`, `vegetationMapChunkCommit`,
  `vegetationMapChunkRead`, `vegetationCook`; the stroke/planting synthesis helpers live in
  `vegetationPainting.ts`/`vegetationPlanting.ts`.)*
- [x] One brush gesture is one cross-tile/cell transaction with compressed tile preimages/deltas and
  canonical cell order. Undo/redo invokes inverse envelopes, not a list of generated transforms.
  *(A paint/erase press with an armed density/scalar-field layer captures spacing-gated picked
  stamps, then `commitStroke` reads the touched chunks (`vegetation-map-chunk-read`), splats into
  grids seeded from the existing tiles, and commits every replacement chunk in ONE
  `vegetation-map-chunk-commit` under the read generation, followed by a cell-scoped
  `vegetation-cook`. Undo restores the captured pre-stroke payloads (created chunks become
  removals) and recooks; redo replays the stroke payloads — both re-read revisions/generation at
  execution time. Preimages are the captured chunk DTOs (quantized tiles), not deltas; cells
  commit in the transaction's canonical key order engine-side.)*
- [x] Persist quantized tiles/anchors/overrides as truth. Optional stroke history is diagnostic
  only. *(Strokes persist only `AuthoredFieldTileDto` grids in Field chunks; anchors/pins/
  overrides persist through AnchorOverride chunks and `vegetation-mutate`; no stroke history is
  stored.)*
- [x] Run invalidation/cook asynchronously with progress, cancel, generation tokens, and previous
  complete preview retained until atomic publication. *(Engine: the staged cook queue is async
  with monotonic progress, cooperative cancel, and supersede-by-newer-job; the previous manifest
  stays current until the staged cook publishes atomically. Editor: stroke-fired and panel-fired
  cooks publish their job id to `vegetationCookJob`; the panel's Cook section polls
  `vegetation-cook-status` at 1 Hz (state, node progress, published cells), offers Cancel while
  queued/running and "Cook map" (scope all) otherwise; a failed cook raises one toast.)*
- [x] Seed/topology edits show an accepted/removed/moved diff and unresolved override conflicts before
  commit. *(`vegetation-topology-diff {map, from, to?, cells?}` compares two cooked manifests per
  cell — unchanged artifacts skip by hash; changed cells decode both MacroPoints sections and
  report added/removed/moved (counts + capped id samples) plus unresolved authored overrides
  (anchors/pins/transform/state rows whose plant is absent from the newer set). The panel's Cook
  section tracks the two most recent completed cook identities and offers Review changes; the
  authored transaction stays losslessly undoable, so review-then-undo is the keep/revert decision
  and overrides are never discarded — the authored chunks retain them until their plants return.
  E2e proves the anchor-chunk recook re-keys the cell with zero churn and surfaces the
  unconsumed authored anchor as an `anchor` conflict.)*

## Diagnostics are core UX

Provide overlays and inspection for:

- source/material/environment fields and suitability heatmaps;
- candidate, accepted, and rejected points colored by reason;
- map layers, cell hierarchy, halos, owner cells, dependencies, dirty/cached state;
- stable IDs, surface attachments, expanded provenance, parent/colony relations;
- macro/micro density, family/variation/phenotype/lifecycle attributes;
- GPU hierarchy cut, triangle/voxel clusters, bounds/normal cones, HZB reject/retest;
- page requests/residency, memory, indirect counts, overdraw, transition/TAA state;
- collision/nav contribution previews and later interaction/ecology channels; and
- node/cell time, candidate counts, cache hits, transfer bytes, artifact size, and safety-cap errors.

Preview quality (points/bounds/coarse/full) changes visualization work only; it cannot change final
authored/evaluated results.

## UI placement rule

`EnvironmentPanel.tsx` retains shared wind/calendar/environment controls. Species, density, brush,
biome, lifecycle, and plant deformation response never land there. Plant-specific response belongs
in the Plant workspace; world placement belongs in Vegetation mode.

## Acceptance

- [x] The default dock layout is no more crowded than before; Vegetation mode opens on demand and
  restores its last dock location. *(The `vegetation` panel is a closable "editing" panel outside
  `DEFAULT_LEAF` — the default layout is unchanged; `openPanel` resolves through the persisted
  `lastLocation` map, so reopening lands where it was last docked. Visual confirmation rides the
  user's editor pass.)*
- [x] Paint/erase/reapply/anchor/pin/override across cell borders is one atomic undoable transaction.
  *(Paint/erase: one cross-cell `vegetation-map-chunk-commit`; anchor: one `vegetation-mutate`
  batch; pin: the Pin tool toggles the clicked plant's row in the active layer's AnchorOverride
  chunk — one read-modify-write transaction with a toggle-back inverse; reapply: a stroke that
  recooks the touched cells in one cells-scoped cook (deterministic refresh — no authored
  mutation, so nothing to undo); override: dragging the selected macro plant with the Select
  tool streams `transform-override` mutations preserving the plant's orientation/scale
  (`VegetationRuntimePlantDto` gained `orientation`/`scaleBits` for the capture) and records ONE
  "Move plant" edit whose undo restores the captured transform. Every gesture is a single
  transaction or a single mutation batch; none expands to per-plant editor commands.)*
- [x] Canceling generation keeps the prior complete preview and publishes no partial tiles/cells.
  *(The staged cook publishes atomically at completion — the previous manifest stays current until
  then; cooperative cancel leaves the store untouched. E2e cancels a prepared evaluation job and
  asserts the cancelled state with its preflight retained; the cook queue shares the staged
  publication path.)*
- [x] Seed/topology conflicts are previewed and never silently discard overrides.
  *(The Review diff lists every unresolved authored override per changed cell; the authored
  chunks retain the rows regardless of cook outcome — a conflict means "not currently applied",
  never deletion.)*
- [x] Graph and viewport diagnostics agree with `sa` counts/provenance/rejection output.
  *(E2e cross-checks: the rejection overlay's source rows (`vegetation-rejections`) sum exactly
  to the `sa vegetation-asset-summary` cook statistics' per-reason totals for the cooked cell;
  the biome panel's typed pins come from the same `vegetation-node-schema` the CLI serves; the
  plant-selection path resolves provenance through the same `vegetation-runtime-inspect` rows
  earlier legs assert. One source of truth per number — the surfaces read the same commands.)*
- [x] Large brush operations produce compact tile deltas, not per-plant editor commands.
  *(A stroke commits quantized `AuthoredFieldTileDto` grids — one chunk row per touched cell —
  never per-plant commands; the plant population is re-derived by the cell-scoped recook.)*
- [x] Dock/graph/store/client unit tests, editor E2E, standard gate, and authoring docs are green.
  *(`cd editor && bun run check` (protocol regen + typecheck) plus `bun test` over the dock, graph,
  store and client suites and a production `bun run build`; `just e2e` for the control-plane legs;
  `just prepare-for-commit` for fmt + clippy; the docs pages checked by the docs-page skill's
  `hugo --gc` + `check_links.py` + `check_style.py`. On macOS there is no toolbox, so the gate runs as
  its parts rather than through `tools/ci/check.sh`.)*

## NO-LEGACY gate

There is one Vegetation world-building surface. No temporary Environment rows, separate foliage
window, duplicate graph evaluator, or editor-only point store remains.

