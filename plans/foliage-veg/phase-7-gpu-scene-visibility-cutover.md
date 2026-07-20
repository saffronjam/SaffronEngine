# Phase 7 — Persistent GPU Scene and visibility cutover

**Status:** NOT STARTED

**Depends on:** Phase 6

This is one atomic renderer migration. It replaces per-frame CPU `DrawItem` gathering/bucketing and
the opt-in per-instance mesh-task loop with a persistent derived GPU Scene, page residency, hierarchical
visibility, and indirect execution for every view and material class. The phase is not complete while
any old static, transparent, shadow, preview, or meshlet gather survives.

## Scene and asset delta journals

- [ ] Add a tracked scene mutation journal covering create/destroy, component add/remove/update,
  hierarchy/world-transform dirtiness, and render-relevant component/material changes.
- [ ] Make `Scene::with_component_mut`, mutable queries, hierarchy propagation, physics/script writes,
  load/undo, and registry deserialization report affected entity/type/revision. Refactor callers that
  bypass the journal; do not retain an untracked mutable escape.
- [ ] Add asset prototype/material/texture/page invalidation events for import, reimport, edit,
  unload, and deletion.
- [ ] Propagate parent transform dirtiness to descendants once, cache world/current/previous revisions,
  and upload only changed render records.

## Persistent GPU Scene

Build one render mirror shared by host/player/scene/thumbnail/asset-preview worlds as appropriate:

- stable generational prototype, material, instance, deformation, light, SDF, and page handles;
- compact static point transforms and separate current/previous dynamic transform payloads;
- material-set and sparse per-object override references, not repeated 256-byte material blobs;
- create/update/remove delta application through frame-safe upload rings and deferred reuse;
- immutable shared geometry/material/page tables plus per-world instance stores; and
- independent per-view visibility/HZB/history/command state over shared scene data.

hecs/project/vegetation state remains canonical. A GPU Scene can be discarded and rebuilt from a
snapshot; it never assigns plant/entity identity.

## Page residency before traversal

- [ ] Add guaranteed-resident roots, generational page tables, compact GPU missing-page requests,
  async I/O/decompression/upload, budgets/LRU, fence-safe eviction, and parent-before-child publication.
- [ ] Prioritize projected error, visibility probability, motion/prefetch, shadow/GI/RT demand, and
  source priority rather than distance alone.
- [ ] Retain a drawable resident ancestor until all requested children are resident. Eviction reverses
  dependency order and cannot create holes.
- [ ] Use ordinary buffers/images and page tables; do not depend on Vulkan sparse residency.

## Hierarchical visibility

One parameterized compute pipeline handles camera, shadow, reflection, and probe views:

1. Cull new/current candidates conservatively; new/streamed/history-invalid instances bypass stale HZB.
2. Test established candidates against the previous HZB using previous transforms and swept bounds.
3. Traverse resident hierarchy nodes by projected appearance error, frustum, normal cone, and
   conservative occlusion; append missing-page requests and keep resident parents.
4. Rasterize the provisional visible cut and build current HZB using Anima's depth convention.
5. Retest previous-pass occluded work against current HZB/current transforms.
6. Rasterize survivors and publish the final history HZB.

Camera cuts, resize, projection changes, teleport/origin shifts, page generation changes, and temporal
representation resets invalidate the required history explicitly. Masked/voxel occluders contribute
only conservative depth so they cannot create false occlusion.

Visibility emits canonical semantic records:

```text
(cluster, instance, part, representation, material, deformation, transition, source generation)
```

It also emits material/depth bins, transparent sort keys, page/shadow/GI/RT demand, and counters.
Indexed-MDI and mesh-task executors derive their own command layouts from the same records.

## Required portable and optional executors

- [ ] Implement compute binning/compaction plus `vkCmdDrawIndexedIndirectCount` over global arenas as
  the required executor.
- [ ] GPU radix-sort alpha-blended records back-to-front per view; no CPU transparent exception.
- [ ] Add `VK_EXT_mesh_shader` execution when individual feature bits/limits qualify. It consumes the
  same clusters/materials/representations and cannot unlock unique content.
- [ ] Schedule count/scan/scatter so capacities are proven; expose every pressure/overflow flag.
- [ ] Drive depth, main, motion, current fixed directional/spot/point shadows, G-buffer, transparent,
  wire/debug, selection ID, and thumbnail/preview passes from this data.

## Rehome every current responsibility

Before deletion, migrate:

- current/previous transforms and normal transforms;
- all `MaterialSet` and per-object overrides;
- opaque, masked, thin-sheet, and transparent routing/sorting;
- skinning, morphs, displacement/tessellation, deformed vertex/index data, and joint palettes through
  a common deformation-provider output;
- depth, main, motion, all raster shadow views, overlay/debug selection, asset preview, thumbnails,
  host, and player;
- static/deformed BLAS/TLAS inputs and material/coverage hit data;
- GDF/SDF instance updates and scene bounds; and
- render stats/profiler/capture labels and validation fixtures.

Then delete `DrawItem`, `DrawBatch`, `SceneDrawList`, CPU `Instancing` bucketing/uploads,
`gather_static_draw_list`, CPU transparent sorting, shadow-only gathers, the per-instance descriptor
meshlet loop, `SAFFRON_MESH_SHADER`, and their tests/docs. Any remaining dynamic CPU simulation data
may update GPU records, but may not rebuild draw lists.

## Acceptance

- [ ] Static-scene CPU render preparation scales with changes/residency, not total visible instances
  or draw count.
- [ ] Existing scenes, morph/skinning/displacement, every material mode, all passes, previews,
  screenshots, RT/GDF/GI, host, and player match or improve their Phase-1 quality fixtures.
- [ ] Rapid camera motion, camera cuts, teleports, resize, wind-bound stress, and page churn produce
  no HZB disappearance, hole, or stale-handle alias.
- [ ] Indexed and mesh executors select identical semantic cluster cuts and render within image
  tolerance; MoltenVK receives full quality through indexed MDI.
- [ ] Transparent sorting is GPU-driven and stable under hierarchy/stream changes.
- [ ] Vulkan validation is clean on NVIDIA, AMD, and MoltenVK; all capability decisions are reported.
- [ ] No old gather/batcher/env toggle symbol remains (`rg` tripwire in the gate).
- [ ] Standard gate and GPU Scene/visibility docs are green.

## NO-LEGACY gate

There is exactly one production scene-render path after this phase. Capability executors vary command
mechanics over one semantic visibility result; they are not alternate renderers or content tiers.

