# Phase 9 — Dedicated Vegetation authoring and diagnostics

**Status:** NOT STARTED

**Depends on:** Phases 3, 5, and 8

This phase builds the editor experience over the existing control plane, evaluator, reducer, and GPU
diagnostics. Vegetation is an on-demand world-building mode and asset workspace, not another long
section in the Environment panel. The default scene layout remains uncluttered.

## Scene Vegetation mode

- [ ] Add a closable `vegetation` scene dock panel and viewport tool mode through
  `dockLayout.ts`/`panelRegistry.tsx`. Keep it out of `REQUIRED_PANELS` and the default right-side
  stack; entering Vegetation mode opens/reveals its last location.
- [ ] Provide Select/Lasso, Paint, Erase, Density, Reapply, Single/Anchor, Fill, Spline, Volume,
  Exclude/Block, Pin, and Promote tools in the viewport toolbar with visible shortcuts/brush HUD.
- [ ] Add searchable thumbnail species/community palette, multi-select and weights, active map/layer,
  brush radius/falloff/pressure/spacing, projection direction/surface filters, variation preview,
  predicted accepted count, memory/work estimate, and cancellation.
- [ ] Add ordered named layers with mute/solo/lock, blend/operator, coordinate space, provenance,
  bounds, dirty/cook state, drag reorder, and conflict badges.
- [ ] Editing a generated plant writes an override/pin/anchor mutation. It never writes a transform
  into `.svegcell` or expands a million instance commands.

## Plant and biome asset workspaces

- [ ] Route Plant/Biome/VegetationMap assets through `AssetEditorWorkspace` capabilities and add
  disjoint asset-editor panel IDs for family preview/structure/phenotypes, biome graph, palette/
  parameters, layers/tiles, diagnostics, and cook statistics.
- [ ] Extract reusable typed graph-canvas primitives from the current React Flow material editor,
  while keeping material, biome, and botanical node type systems separate.
- [ ] Biome graphs expose typed pins, modules/interfaces, parameter presets, predicted cardinality,
  influence/halo, authority/taint, CPU/GPU grouping and transfers, execution time, cache hits, and
  node-local errors.
- [ ] Plant preview can scrub variation, life stage, season, wind strength, representation/error,
  coverage, collision/nav proxies, and source/reimport conflicts through the actual asset-preview GPU
  Scene.

## Transactional editing and undo

- [ ] Brush/graph/layer operations call typed control commands backed by the Phase-2 reducer. Add
  editor client helpers for common scene/map setup rather than repeating raw command sequences.
- [ ] One brush gesture is one cross-tile/cell transaction with compressed tile preimages/deltas and
  canonical cell order. Undo/redo invokes inverse envelopes, not a list of generated transforms.
- [ ] Persist quantized tiles/anchors/overrides as truth. Optional stroke history is diagnostic only.
- [ ] Run invalidation/cook asynchronously with progress, cancel, generation tokens, and previous
  complete preview retained until atomic publication.
- [ ] Seed/topology edits show an accepted/removed/moved diff and unresolved override conflicts before
  commit.

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

- [ ] The default dock layout is no more crowded than before; Vegetation mode opens on demand and
  restores its last dock location.
- [ ] Paint/erase/reapply/anchor/pin/override across cell borders is one atomic undoable transaction.
- [ ] Canceling generation keeps the prior complete preview and publishes no partial tiles/cells.
- [ ] Seed/topology conflicts are previewed and never silently discard overrides.
- [ ] Graph and viewport diagnostics agree with `sa` counts/provenance/rejection output.
- [ ] Large brush operations produce compact tile deltas, not per-plant editor commands.
- [ ] Dock/graph/store/client unit tests, editor E2E, standard gate, and authoring docs are green.

## NO-LEGACY gate

There is one Vegetation world-building surface. No temporary Environment rows, separate foliage
window, duplicate graph evaluator, or editor-only point store remains.

