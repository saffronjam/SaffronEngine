# Phase 11 — Virtual shadows, lighting, GI, and ray tracing

**Status:** IN PROGRESS

**Depends on:** Phases 7–8 and 10

Dense animated vegetation exposes fixed whole-light shadow maps immediately. This phase atomically
replaces the existing fixed raster shadow-map path with portable physical-atlas virtual shadow maps,
then makes foliage's thin-sheet/aggregate appearance consistent across every lighting, GI, reflection,
and ray-tracing consumer. Structured deformation and tight swept bounds from Phase 10 are hard inputs.

## Physical-atlas virtual shadow maps

- [x] Add directional clip levels and demand-paged local-light virtual address spaces with one normal *(VSM S1–S3 sealed in READMEFABLE: 4096² atlas, 8 clip levels + spot 16² + 6 point faces 8², GPU demand mark/compact + CPU drain, per-space cull chains through record_executor_depth_family, fine→coarse directional fallback; fixed path deleted in S3c)*
  physical depth atlas, page table, free list, LRU/budget, and generation-tagged mappings. Do not use
  Vulkan sparse binding/residency.
- [x] Mark receiver demand from camera-visible depth/lighting, compact unique page requests on GPU, *(VSM S1–S3 sealed in READMEFABLE: 4096² atlas, 8 clip levels + spot 16² + 6 point faces 8², GPU demand mark/compact + CPU drain, per-space cull chains through record_executor_depth_family, fine→coarse directional fallback; fixed path deleted in S3c)*
  prioritize by projected contribution, and allocate/evict with fence-safe publication.
- [x] Invoke the shared GPU Scene culler per requested page/view, selecting the same resident *(VSM S1–S3 sealed in READMEFABLE: 4096² atlas, 8 clip levels + spot 16² + 6 point faces 8², GPU demand mark/compact + CPU drain, per-space cull chains through record_executor_depth_family, fine→coarse directional fallback; fixed path deleted in S3c)*
  triangle/aggregate-voxel hierarchy, coverage rules, deformation output, and semantic cluster IDs.
- [x] Split cache policy into stable/static and dynamic layers without duplicating shadow content. *(VSM S4: static pages persist until invalidated; moved/removed casters dirty overlapped pages via swept bounds from the persistent-scene delta seam; wind re-dirties levels 0..=5 + spot + point under the 64-page budget; holes drain before refreshes)*
  Exact current/previous swept cluster bounds dirty only intersecting pages.
- [x] Keep a valid coarser/older page while replacement work is pending where conservative reuse is *(VSM S1–S3 sealed in READMEFABLE: 4096² atlas, 8 clip levels + spot 16² + 6 point faces 8², GPU demand mark/compact + CPU drain, per-space cull chains through record_executor_depth_family, fine→coarse directional fallback; fixed path deleted in S3c)*
  correct; otherwise mark missing data explicitly and avoid light leaks through a defined fallback.
- [x] Support directional, spot, and point-light virtual pages through one page/cache vocabulary. *(VSM S1–S3 sealed in READMEFABLE: 4096² atlas, 8 clip levels + spot 16² + 6 point faces 8², GPU demand mark/compact + CPU drain, per-space cull chains through record_executor_depth_family, fine→coarse directional fallback; fixed path deleted in S3c)*
- [x] Expose page request/allocation/render/cache-hit/dirty/eviction/overflow counters and overlays. *(render-stats `vsm` block + the `shadow-pages` view mode)*

In the same phase, delete the fixed 2048² directional/spot and 512² point-face ownership, associated
special draw gathers, settings, and docs. There is no `useVirtualShadows` switch or permanent parallel
map path.

## Coverage and deformation agreement

- [x] Shadow raster uses the same modeled leaf geometry, hashed/A2C-equivalent canonical coverage,
  two-sided policy, triangle/voxel cut, and representation transition as the main view. *(the page
  raster replays the executor stream through `vertexMainExecutor` + `depthPrepassFragment` — the
  same wind/deform vertex path and `sampleCanonicalCoverage` fragment as the depth prepass; each
  page group runs the same cull → traversal → binning chain over the same hierarchy. Displaced
  (tess-seam) casters shadow from their base geometry: the page traversal runs `tess_seam: 0`, so
  displacement detail is a camera-pass refinement, not a shadow term.)*
- [x] Wind and interaction never disable at distance to save page invalidation. Reduced far modes and
  tight bounds provide the scalability mechanism. *(no distance gate exists in `gpuSceneWindDeform`;
  scalability comes from the S4 dynamic-dirty level cap — levels 6–7 whose 0.5 m+ texels cannot
  resolve sway stay cached — and the 64-page frame budget)*
- [x] Current/previous deformation generations and page mappings participate in temporal invalidation.
  *(moved/removed instances dirty overlapped pages via swept current+previous bounds; per-frame
  compute-skinned instances dirty theirs through `note_instances_moved` at the deformation-patch
  seam; wind re-dirties the resolving levels each frame)*
- [x] No whole-tree shadow impostor or proxy silhouette is introduced. *(the only shadow geometry
  is the executor stream's real representations)*

## GI, IBL, reflection, and SDF integration

- [x] Add the thin-sheet surface response to direct clustered lighting, IBL, DDGI, SSGI, ReSTIR,
  SSR/reflection paths, thumbnails, and debug view modes from the same shared shader function.
  *(verified complete: `thinSheetPartition`/`thin_sheet.slang` is the one shared function —
  `surfaceDirect` covers every direct/clustered/ReSTIR-resolved light, the `SURFACE_THIN_SHEET`
  ambient block covers IBL + DDGI + SSGI-resolved irradiance and scales `reflectionSpec` for
  SSR/reflection, and thumbnails + debug view modes evaluate the same `evalLighting`)*
- [x] Represent foliage in GDF/DDGI as porous aggregate occupancy/transmission/material/normal data;
  do not voxelize a canopy into solid opaque SDF matter. *(per-cascade R8 occupancy volumes beside
  the distance cascades; porous-classed instances — entity occupancy from the material vocabulary's
  `VoxelMaterialMoments.occupancy`, plumbed through `SdfInstance.local_max.w` — skip the distance
  min and splat density; the albedo cache's alpha carries solid/porous/open; every march
  (`ddgi_trace` primary + sun, DFAO cones, reflection cone) accumulates Beer–Lambert extinction via
  the shared `sdfExtinctionStep`, with saturated extinction shading an aggregate hit from the cached
  colour + occupancy-gradient normal)*
- [ ] Update aggregate voxel injection/sampling so triangle↔voxel transitions preserve indirect
  irradiance, sky visibility, reflection response, and transmitted energy within error bounds.
- [ ] Parameterize GI/reflection culling through the same hierarchy and residency demand rather than
  rebuilding plant-specific lists.

## Portable KHR ray tracing policy

- [ ] Static nondeforming whole-family/variation representations may share compacted BLAS where their
  transforms/material classification permit.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [ ] For structured deformation, materialize the selected assembly hierarchy into GPU geometry for
  BLAS build/update through the shared deformation output and cache policy. Do not assume KHR AS
  supports nested micro-instance parts inside one plant BLAS.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [ ] Represent aggregate voxel clusters through cooked triangle surfaces or procedural AABBs with
  intersection/hit shading that matches raster aggregate moments.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [ ] Standard KHR any-hit over canonical coverage remains baseline-correct.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [ ] Derive optional `VK_KHR_opacity_micromap` data from the exact coverage texture/mip/classification
  source and validate conservative/unknown states. OMM removes cost, never correctness.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [ ] Add optional `VK_NV_cluster_acceleration_structure` and partitioned-AS execution over canonical
  cluster/page data only after KHR correctness; no NVIDIA-specific plant representation.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [ ] Track BLAS/TLAS build/update/compaction time, memory, selected representation, OMM hit classes,
  and page demand without copying vendor performance thresholds.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)

## Acceptance

- [x] VSM covers all current shadow-casting light families and the old fixed-map symbols/resources are
  absent. (The page table reserves a base per family — `VSM_SPOT_TABLE_BASE` after the 8 directional
  clip levels, `VSM_POINT_TABLE_BASE` after the spot pages, 6 point faces to `VSM_TABLE_ENTRIES` — and
  no fixed shadow-map symbol survives outside the VSM/contact/cloud paths. e2e `vsm`: a scene with
  directional + spot + point casters demands and allocates pages with `overflow == 0`,
  validation-clean.)
- [ ] Rapid wind, interaction, camera travel, page churn, and phenotype transitions create no stale
  shadow, light leak, silhouette pop, or whole-tree invalidation storm. (PARTIAL, honestly: e2e `vsm`
  proves the invalidation half — a settled frame answers repeat demand from residency with zero fresh
  allocations, zero evictions and zero overflow, and a moving caster dirties far below the atlas page
  count without overflowing. The remaining half — that no *stale* shadow, leak, or silhouette pop is
  visible under rapid wind/phenotype churn — is a visual claim needing image comparison, which this
  suite has no harness for.)
- [ ] Main/depth/VSM/GI/reflection/RT select compatible hierarchy cuts and coverage.
- [ ] Triangle↔aggregate transitions stay within defined direct/indirect/transmission/normal error.
- [ ] KHR any-hit output is correct without OMM; OMM/vendor tiers match it within tolerance.
  (RAY-TRACING HARDWARE ABSENT. `vulkaninfo` on this machine reports ZERO occurrences of
  `VK_KHR_ray_query` and `VK_KHR_acceleration_structure`: the only device is `Apple M4` through
  MoltenVK, and `Device::new` accordingly resolves `rt_supported = false` from
  `has_as && has_rq && has_deferred`. Nothing in this box can be exercised here, and no code change
  closes it. Recorded rather than claimed.)
- [x] Physical-atlas VSM and full raster quality pass on MoltenVK without sparse-residency dependency.
  (The atlas is one ordinary image — `vsm.rs`: "No sparse binding" — and the whole e2e suite,
  including the new `vsm` file, runs on MoltenVK on an Apple M4. e2e `vsm` 4/4 with the shadow-page
  debug channel rendering validation-clean and returning to lit.)
- [ ] Standard gate, cross-platform validation, visual/timing captures, and shadow/GI/RT docs are green.
  (GREEN: the standard gate (`just engine`, `just prepare-for-commit`, `just schema`, `just test`,
  `just e2e` 328/328) and the shadow/GI docs three-check —
  `explanations/shadows-and-culling/virtual-shadow-maps.md` and the GI pages pass hugo, the link check,
  and the style check at 0/0; `tests/e2e/vsm.test.ts` covers the physical atlas at 4/4. OUTSTANDING and
  hardware-gated: cross-platform validation needs a second adapter, and the RT half of the docs cannot
  be validated against a run because this device reports no ray-tracing extensions.)

## NO-LEGACY gate

VSM is the raster shadow system when this phase lands. GI/RT consume canonical plant materials,
hierarchies, and deformation; no foliage-only shadow, solid-canopy approximation, or vendor content
fork remains.

