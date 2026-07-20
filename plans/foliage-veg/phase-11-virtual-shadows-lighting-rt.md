# Phase 11 — Virtual shadows, lighting, GI, and ray tracing

**Status:** NOT STARTED

**Depends on:** Phases 7–8 and 10

Dense animated vegetation exposes fixed whole-light shadow maps immediately. This phase atomically
replaces the existing fixed raster shadow-map path with portable physical-atlas virtual shadow maps,
then makes foliage's thin-sheet/aggregate appearance consistent across every lighting, GI, reflection,
and ray-tracing consumer. Structured deformation and tight swept bounds from Phase 10 are hard inputs.

## Physical-atlas virtual shadow maps

- [ ] Add directional clip levels and demand-paged local-light virtual address spaces with one normal
  physical depth atlas, page table, free list, LRU/budget, and generation-tagged mappings. Do not use
  Vulkan sparse binding/residency.
- [ ] Mark receiver demand from camera-visible depth/lighting, compact unique page requests on GPU,
  prioritize by projected contribution, and allocate/evict with fence-safe publication.
- [ ] Invoke the shared GPU Scene culler per requested page/view, selecting the same resident
  triangle/aggregate-voxel hierarchy, coverage rules, deformation output, and semantic cluster IDs.
- [ ] Split cache policy into stable/static and dynamic layers without duplicating shadow content.
  Exact current/previous swept cluster bounds dirty only intersecting pages.
- [ ] Keep a valid coarser/older page while replacement work is pending where conservative reuse is
  correct; otherwise mark missing data explicitly and avoid light leaks through a defined fallback.
- [ ] Support directional, spot, and point-light virtual pages through one page/cache vocabulary.
- [ ] Expose page request/allocation/render/cache-hit/dirty/eviction/overflow counters and overlays.

In the same phase, delete the fixed 2048² directional/spot and 512² point-face ownership, associated
special draw gathers, settings, and docs. There is no `useVirtualShadows` switch or permanent parallel
map path.

## Coverage and deformation agreement

- [ ] Shadow raster uses the same modeled leaf geometry, hashed/A2C-equivalent canonical coverage,
  two-sided policy, triangle/voxel cut, and representation transition as the main view.
- [ ] Wind and interaction never disable at distance to save page invalidation. Reduced far modes and
  tight bounds provide the scalability mechanism.
- [ ] Current/previous deformation generations and page mappings participate in temporal invalidation.
- [ ] No whole-tree shadow impostor or proxy silhouette is introduced.

## GI, IBL, reflection, and SDF integration

- [ ] Add the thin-sheet surface response to direct clustered lighting, IBL, DDGI, SSGI, ReSTIR,
  SSR/reflection paths, thumbnails, and debug view modes from the same shared shader function.
- [ ] Represent foliage in GDF/DDGI as porous aggregate occupancy/transmission/material/normal data;
  do not voxelize a canopy into solid opaque SDF matter.
- [ ] Update aggregate voxel injection/sampling so triangle↔voxel transitions preserve indirect
  irradiance, sky visibility, reflection response, and transmitted energy within error bounds.
- [ ] Parameterize GI/reflection culling through the same hierarchy and residency demand rather than
  rebuilding plant-specific lists.

## Portable KHR ray tracing policy

- [ ] Static nondeforming whole-family/variation representations may share compacted BLAS where their
  transforms/material classification permit.
- [ ] For structured deformation, materialize the selected assembly hierarchy into GPU geometry for
  BLAS build/update through the shared deformation output and cache policy. Do not assume KHR AS
  supports nested micro-instance parts inside one plant BLAS.
- [ ] Represent aggregate voxel clusters through cooked triangle surfaces or procedural AABBs with
  intersection/hit shading that matches raster aggregate moments.
- [ ] Standard KHR any-hit over canonical coverage remains baseline-correct.
- [ ] Derive optional `VK_KHR_opacity_micromap` data from the exact coverage texture/mip/classification
  source and validate conservative/unknown states. OMM removes cost, never correctness.
- [ ] Add optional `VK_NV_cluster_acceleration_structure` and partitioned-AS execution over canonical
  cluster/page data only after KHR correctness; no NVIDIA-specific plant representation.
- [ ] Track BLAS/TLAS build/update/compaction time, memory, selected representation, OMM hit classes,
  and page demand without copying vendor performance thresholds.

## Acceptance

- [ ] VSM covers all current shadow-casting light families and the old fixed-map symbols/resources are
  absent.
- [ ] Rapid wind, interaction, camera travel, page churn, and phenotype transitions create no stale
  shadow, light leak, silhouette pop, or whole-tree invalidation storm.
- [ ] Main/depth/VSM/GI/reflection/RT select compatible hierarchy cuts and coverage.
- [ ] Triangle↔aggregate transitions stay within defined direct/indirect/transmission/normal error.
- [ ] KHR any-hit output is correct without OMM; OMM/vendor tiers match it within tolerance.
- [ ] Physical-atlas VSM and full raster quality pass on MoltenVK without sparse-residency dependency.
- [ ] Standard gate, cross-platform validation, visual/timing captures, and shadow/GI/RT docs are green.

## NO-LEGACY gate

VSM is the raster shadow system when this phase lands. GI/RT consume canonical plant materials,
hierarchies, and deformation; no foliage-only shadow, solid-canopy approximation, or vendor content
fork remains.

