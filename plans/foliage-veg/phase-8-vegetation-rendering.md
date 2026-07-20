# Phase 8 — Macro and micro vegetation rendering

**Status:** NOT STARTED

**Depends on:** Phases 5 and 7

This phase connects authoritative vegetation snapshots to the final GPU Scene. Macro plants become
compact render instances with stable IDs and virtual plant hierarchy roots. Micro vegetation is
reconstructed near spatial sources from authoritative field tiles. Both use the same material,
coverage, visibility, page, picking, and diagnostic infrastructure as ordinary geometry.

## Macro snapshot adapter

- [ ] Translate resident render-facet macro columns into GPU Scene create/update/remove deltas by
  `PlantId` and cell generation; never expose `PlantSlot` or store renderer state in `VegetationWorld`.
- [ ] Resolve `.splant` family/variation/phenotype to its `.splantc` prototype and guaranteed root.
- [ ] Upload compact point transforms, state/phenotype parameters, stable IDs, material overrides,
  surface attachment, interaction policy, and conservative current/previous bounds.
- [ ] Maintain a `PlantId ↔ RenderHandle` adapter map that is disposable, generation-tagged, and
  rebuilt from snapshots after GPU Scene loss.
- [ ] Apply cell generation changes atomically so one plant never has two visible representations.

## Micro vegetation fields

- [ ] Stream authoritative quantized density/species/height/orientation/phenotype/disturbance tiles
  around render sources.
- [ ] Generate spatially stable grass/blade/ground-cover candidates on GPU from cell/tile coordinates,
  source-independent keys, and fields. Camera travel changes residency, not established phase/placement.
- [ ] Emit the same semantic visible-cluster records as macro plants; procedural blade/curve emission
  must have a portable compute/indexed representation as well as any mesh-shader acceleration.
- [ ] Never expose individual micro blades to saves, collision, nav, ecology, or gameplay queries.
  Persistent crushed/cleared areas modify disturbance/field tiles through mutations.
- [ ] Use count/scan/scatter and predicted tile budgets; no silent density reduction on overflow.

## Representation and transitions

- [ ] Traverse plant assembly, triangle, and aggregate-voxel nodes by appearance error; cells do not
  select LOD.
- [ ] Parent stays visible until children and every referenced part/material page are resident.
- [ ] Use stable cluster/plant IDs and temporally stable stochastic coverage for triangle↔voxel/
  parent↔child transition; output transition/reactive state to TAA.
- [ ] Track current and previous representation IDs as well as transforms. Motion/history rejection
  handles representation change without ghost trails or holes.
- [ ] Keep geometry-first leaf/blade silhouettes; imported alpha-card sources are contoured/meshed
  where the cooker can preserve semantic accuracy, with remaining micro coverage using Phase-6 rules.

## Picking, bounds, and streaming feedback

- [ ] Add a GPU selection-ID path returning tagged `Vegetation(PlantId)` for macro plants and an
  explicit nonpersistent micro hit for paint feedback. Editor selection resolves through the CPU cell
  snapshot/provenance, not GPU slot indices.
- [ ] Merge selection with the shared `SurfaceField`/entity picking vocabulary without a second
  viewport ray implementation.
- [ ] Feed visible/error/missing-page, camera prediction, and view demand back to residency.
- [ ] Expose per-family/cell/view counts, selected representation, hierarchy depth, cull reasons,
  page faults, residency bytes, overdraw, quad utilization, and transition/TAA rejection.

## Stress fixtures

Add checked-in project/scene definitions for meadow micro-density, mixed shrub/tree woodland,
geometry-first broad leaves, remaining masked serrations, dense conifer needles, seasonal variations,
extreme scale, negative cells, rapid camera traversal, and asset-preview views. The fixtures specify
content and camera paths, not vendor-derived performance numbers.

## Acceptance

- [ ] Millions of macro plants and dense micro fields add no per-instance CPU draw/gather work.
- [ ] Render-cell load/unload, page miss/eviction, and hierarchy transitions never create a hole,
  billboard flattening, silhouette collapse, transmission jump, or TAA trail.
- [ ] Macro selection returns stable string `PlantId` and provenance after compaction/reload.
- [ ] Micro blades reconstruct stably after unload/reload and cannot enter authoritative APIs.
- [ ] Depth/main/current shadow/selection coverage agrees for modeled and residual-masked foliage.
- [ ] Every representation and fixture renders at full quality on MoltenVK's indexed executor.
- [ ] Standard gate, validation runs, visual comparisons, and vegetation-rendering docs are green.

## NO-LEGACY gate

No `VegetationRenderer`, terrain detail renderer, grass particle system, or actor-foliage path exists
beside GPU Scene. Macro and micro differ in authority/storage, not in render architecture.

