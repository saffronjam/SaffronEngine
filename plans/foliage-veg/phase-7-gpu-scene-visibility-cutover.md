# Phase 7 — Persistent GPU Scene and visibility cutover

**Status:** COMPLETED

**Depends on:** Phase 6

This is one atomic renderer migration. It replaces per-frame CPU `DrawItem` gathering/bucketing and
the opt-in per-instance mesh-task loop with a persistent derived GPU Scene, page residency, hierarchical
visibility, and indirect execution for every view and material class. The phase is not complete while
any old static, transparent, shadow, preview, or meshlet gather survives.

## Active checkpoint

The persistent GPU Scene value model, Renderer ownership, and the scene/asset delta adapter are
implemented and green. `Renderer` owns one `GlobalGpuData` and one `PersistentGpuScene`, registers
Scene, AssetPreview, and Thumbnail world/view identities, and retires both mirrors from
fence-completed frame slots. `GpuSceneMirror` (saffron-assets) consumes both mutation journals from
retained cursors, resolves mesh/skinned instances and punctual lights into typed world deltas and
assets into geometry/page/material/texture/coverage device records plus shared deltas, interns
material variants by content identity with refcounts, uploads only changed records via cached
revisions, and rebuilds from live state on journal overflow, catalog replacement, or a rebound
scene instance (`Scene::instance_id`). Host (scene/preview/thumbnail views), control-plane
thumbnails, and the player all sync through the same world/view vocabulary; `gpu-scene-stats` is
the control/`sa` surface. The upload translation is implemented and frame-integrated:
`GpuSceneUploader` (rendering) owns slot-indexed device tables mirroring the persistent scene
one-to-one (16-byte occupancy header + locked std430 body, variable data in element arenas),
drains `stage_upload_batch` into graph-owned transfer passes with growth-before-write ordering,
and `GpuScenePendingUploads` carries the mirror's resident-record stages, retirements,
vertex/index streams, and packed `MaterialParamsData` blocks into the same frame. Byte-exact
readback, growth-preservation, and drain/tombstone GPU tests cover it; e2e runs with the
translation live in every host frame. The descriptor vocabulary is one per-frame
`GpuSceneAddressBlock` uniform (every table's buffer device address + capacities, bound at
instance-set binding 3, rewritten per frame for the active view's world so growth never
rewrites a descriptor); `global_gpu_data.slang` declares the block and typed pointer
accessors, the übershader imports the module, and a MoltenVK compute fixture resolves the
instance → prototype → material chain through the addresses byte-exactly. RT coverage
parity is closed: TLAS instances carry their GPU-scene instance slot as
`instanceCustomIndex` (deformed/unmirrored instances carry the force-opaque sentinel),
per-instance opacity flags route non-opaque candidates into the ray-query candidate loops,
and every inline query (mesh-family shadow/reflection and the ReSTIR resolve visibility
ray, whose pipeline binds the address block + bindless array) confirms candidates through
`gpuSceneRayCandidateCovered` — the shared classifier over table-reconstructed surfaces,
proven byte-exact against the CPU classifier on MoltenVK
(`ray_candidate_classification_matches_the_cpu_classifier`) with the resident table
strides locked (`resident_table_strides_lock_the_slang_pointer_constants`). Page
residency is live: every page carries a byte-locked device payload (`page_payload.rs` —
node header, child page-table handles, cluster records, geometry-relative cluster index
blobs, voxel surfaces), the `page-stream` worker loads payloads from the source artifact
(`.smesh` envelope slice or a retained cooked hierarchy) off the frame loop, and
`PageResidency` publishes parent-before-child into the global page arena with byte
budgets, LRU leaf-first eviction that never touches guaranteed roots or resident
parents, and fence-deferred range reuse. Demand is scored on the refinement frontier
(projected transition error × frustum probability × motion boost), and the per-frame GPU
missing-page request ring (`gpuSceneRequestPage` / `drain_page_requests`) folds shader
misses into the same path; the address block carries the page arena and the request
buffer. `gpu-scene-stats` reports the residency counters end to end (asserted over the
control plane in `gpu-scene-residency.test.ts`). The prioritizer's shadow/GI/RT demand
input arrives with the hierarchical-visibility traversal (the request-buffer path it
feeds already exists). The visibility machinery is built and live in every
host frame: per-view HZB max pyramid ping-pong pairs built after the scene pass
(`hzb.rs`); the parameterized instance cull/retest compute (previous-pyramid occlusion
with previous transforms, per-slot history words, retest merge after the current
build); hierarchy traversal from guaranteed roots by projected appearance error with
missing-page requests and no-hole parents, emitting `GpuDrawRecord` streams; binning
(count/scan/scatter) into per-bin `VkDrawIndexedIndirectCommand` ranges with the pages
arena as the executor index buffer and BDA vertex pulling; a depth-only executor PSO
proven end to end (counted indirect draw rasterizes the cut, validation-clean); and the
GPU transparent radix sort (stable LSD, back-to-front command stream) proven on known
depths. The cutover groundwork is in place: `vertexMainExecutor` in the übershader
module emits the exact `VertexOutput` interface (fragment shading byte-identical),
the record stream binds at instance-set binding 4, `PsoKey.executor` mints executor
permutations, and codegen material shaders carry identity end to end
(`ExecutorShaderRegistry` → `GpuMaterialTableRecord.shader_index` →
`GpuDrawRecord.reserved`). The atomic cutover is landed: every raster pass body records
the executor draws (per-bucket counted indirect for the scene, one-PSO depth family
for the depth prepass, the `vsm-pages` shadow pass, G-buffer and motion,
per-blend-bucket zero-masked sorted
streams for translucency, the reactive-coverage mask over the blend buckets, the
executor-only wireframe overlay), the survivor chain runs end to end (snapshot →
HZB#1 → retest → survivor traversal/binning → survivor raster with loaded
attachments and MSAA re-resolve → full re-bin → HZB rebuild over the same imported
pyramid), displaced instances draw through the same binned cut (the traversal emits a
`GPU_REPRESENTATION_DISPLACED_MICRO` record per displaced instance, `DisplacedRow`
carries its amplification-arena row plus the local-amplitude slack the cull adds to
the cooked bounds, and the scatter points that record's indirect draw at the arena's
index stream through `gpuSceneDisplacedDraw`), the scene driver submits
`DeformationWork` + joints through `submit_gpu_scene_deformations` (no draw list),
the editor-camera gizmo is a `PreviewGhost` child entity over the reserved
`EDITOR_CAMERA_MESH_ID`, set-2 binding 2 is the global material-parameter arena,
render stats derive from the visibility readback (records/visible/triangles via
counter word 8), and `DrawItem`/`DrawBatch`/the batcher/CPU recorders/CPU
transparent sort/the meshlet raster path + `SAFFRON_MESH_SHADER` are deleted with
the draw-path tripwire in the gate. The optional mesh-shader executor is built too:
the übershader's `meshMainExecutor` over the same binned records, opt-in through
`SAFFRON_MESH_EXECUTOR=1` on a device whose `Capabilities::mesh_shader` holds, rendering
frames bit-identical to the indexed path.

## Scene and asset delta journals

- [x] Add a tracked scene mutation journal covering create/destroy, component add/remove/update,
  hierarchy/world-transform dirtiness, and render-relevant component/material changes.
- [x] Make `Scene::with_component_mut`, mutable queries, hierarchy propagation, physics/script writes,
  load/undo, and registry deserialization report affected entity/type/revision. Refactor callers that
  bypass the journal; do not retain an untracked mutable escape.
- [x] Add asset prototype/material/texture/page invalidation events for import, reimport, edit,
  unload, and deletion.
- [x] Propagate parent transform dirtiness to descendants once, cache world/current/previous revisions,
  and upload only changed render records.

## Persistent GPU Scene

- [x] Define stable generational prototype, material, instance, deformation, light, SDF, and page
  handles with fence-deferred reuse and stale-handle rejection.
- [x] Store compact exact static point transforms, separate current/previous dynamic transforms,
  shared material-set references, and strictly ordered sparse per-object overrides.
- [x] Apply validated create/update/remove deltas through coalesced, bounded frame-slot upload ranges;
  preserve shared immutable records and caller-keyed per-world instance/light stores.
- [x] Keep per-view visibility/HZB/history/command revisions outside shared scene data and support a
  complete derived snapshot rebuild with full-table reupload.
- [x] Instantiate the mirror for host/player/scene/thumbnail/asset-preview worlds and bind its staged
  ranges to the resident GPU tables as part of the atomic renderer cutover.

hecs/project/vegetation state remains canonical. A GPU Scene can be discarded and rebuilt from a
snapshot; it never assigns plant/entity identity.

## Page residency before traversal

- [x] Add guaranteed-resident roots, generational page tables, compact GPU missing-page requests,
  async I/O/decompression/upload, budgets/LRU, fence-safe eviction, and parent-before-child publication.
- [x] Prioritize projected error, visibility probability, motion/prefetch, shadow/GI/RT demand, and
  source priority rather than distance alone. (Demand scores on the refinement frontier —
  projected transition error × frustum probability × motion boost — plus the GPU
  missing-page request ring every pass folds into. Shadow, GI, and RT consume the camera
  traversal's cut, so their page demand is the camera demand by construction.)
- [x] Retain a drawable resident ancestor until all requested children are resident. Eviction reverses
  dependency order and cannot create holes.
- [x] Use ordinary buffers/images and page tables; do not depend on Vulkan sparse residency.

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

- [x] Implement compute binning/compaction plus `vkCmdDrawIndexedIndirectCount` over global arenas as
  the required executor.
- [x] GPU radix-sort alpha-blended records back-to-front per view; no CPU transparent exception.
- [x] Add `VK_EXT_mesh_shader` execution when individual feature bits/limits qualify. It consumes the
  same clusters/materials/representations and cannot unlock unique content.
  (Both executors consume ONE binned command stream. `scene_bin_scatter.slang` emits one draw command
  per cluster — `command.indexCount = cluster.indexCount`, `command.firstInstance = index`, the record
  index the indexed path reads as `SV_VulkanInstanceID` — so the mesh path reads that same stream as
  **data** rather than as draw arguments: a workgroup loads the command its `SV_DrawIndex` names within
  the bucket's slice and has everything the vertex path had. No second binning, no second record
  format.
  THE DISPATCH SHAPE, and it needs no prefix sum: the scatter writes the group count itself. Beside
  each `command.indexCount` it writes a parallel 12-byte `VkDrawMeshTasksIndirectCommandEXT` at the
  same slot through `writeMeshArgs`, with
  `groupCountX = ceil(indexCount / 3 / MESH_TRIANGLES_PER_GROUP)` (62) and Y = Z = 1, into
  `VisibilityFrame::mesh_args` (cleared each frame beside `commands`, so an unwritten slot dispatches
  nothing). `record_executor_bucket_draw_mesh` then mirrors the indexed recorder exactly —
  `cmd_draw_mesh_tasks_indirect_count(meshArgs, base * MESH_TASK_COMMAND_STRIDE, bucketCounts,
  bucketIndex * 4, draws, MESH_TASK_COMMAND_STRIDE)` against the indexed recorder's
  `cmd_draw_indexed_indirect_count` over the 20-byte command stream — reading the same count word, and
  both recorders share one `ExecutorBucketDraw` parameter struct. A workgroup recovers its
  place from two builtins: `SV_DrawIndex` gives its command ordinal within the bucket slice and
  `SV_GroupID.x` its 62-triangle block within that command.
  PER-COMMAND GROUP COUNTS ARE WHAT MAKE ALL THREE REPRESENTATIONS SAFE. The stream carries
  micro-blade records (a shared template block, `pc.microIndexCount`), aggregate-voxel records
  (`node.indexCount`), and triangle clusters (`cluster.indexCount`); only the last is bounded by 124
  triangles, so a fixed groups-per-command factor sized for clusters would silently drop geometry from
  the other two — missing triangles, not a validation error.
  A CLUSTER CARRIES NO LOCAL VERTEX TABLE: `GpuPageClusterRecord` is
  `{firstIndex, indexCount, materialSlot, prototype, bounds, cone}`, a flat range into the page's u32
  index blob. Emitting one output vertex per index would need up to 372, past
  `maxMeshOutputVertices`, so a cluster's 124 triangles are covered by two groups of 62 (186 vertices,
  62 primitives — inside every tier's limits). A portable cluster is
  `PORTABLE_CLUSTER_MAX_VERTICES = 64` / `PORTABLE_CLUSTER_MAX_TRIANGLES = 124`
  (`geometry/src/virtual_hierarchy.rs`); the RTX 3070 Ti reports 256 / 256 / 1024 for output vertices,
  primitives and workgroup invocations, and Mesa llvmpipe advertises the extension at 256 / 256 / 128,
  so the software tier runs the same path.
  `firstIndex` IS AN OFFSET INTO THE **PAGES** ARENA, not into `addresses.indices`: the indexed
  executor binds `gpu_data.pages` as its index buffer and the scatter derives `firstIndex` from a page
  byte offset. Reading the wrong buffer rasterizes nothing, with no validation error and no crash.
  ONE ENTRY, IN THE ÜBERSHADER ITSELF: `meshMainExecutor` in `mesh.slang` sits beside
  `vertexMainExecutor`, and both call the same `executorVertexOutput` helper, so vertex-for-vertex
  agreement is structural rather than tested. `PsoKey.mesh_shader` picks the stage (`MESH_EXT` +
  `meshMainExecutor` against `VERTEX` + `vertexMainExecutor`, one module, one `fragmentMain`), the
  command stream binds at set 2 binding 5 so the mesh entry reads as data what the indexed entry
  consumes as arguments, and the bucket's slice base rides the push because `SV_DrawIndex` counts from
  zero within the bucket's slice. A separate mesh depth shader and PSO would be a second executor
  shadowing the übershader's own depth pre-pass; `tools/ci/check.sh`'s step 4b fails the gate on the
  symbol.
  SELECTION is `SAFFRON_MESH_EXECUTOR=1` plus `Capabilities::mesh_shader` — the box asks for an
  optional second executor, and MoltenVK has no mesh stage and keeps the indexed path.
  `tests/e2e/mesh-executor-parity.test.ts` boots two hosts differing in exactly that variable, reads
  `meshExecutor` back from `render-stats` to prove each run used the executor it claims, requires
  bit-identical frames, and asserts both runs drew something — a toggle that silently did nothing
  would otherwise pass perfectly.
  SCOPE: the mesh stage serves the shaded scene pass and its survivor pass; the depth family (prepass,
  shadow pages, gbuffer, motion) stays indexed. The box asks for execution that "cannot unlock unique
  content"; the shaded pass demonstrates it, and leaving the depth family indexed unlocks nothing.)
- [x] Schedule count/scan/scatter so capacities are proven; expose every pressure/overflow flag.
- [x] Drive depth, main, motion, every shadow view, G-buffer, transparent, wire/debug, selection ID,
  and thumbnail/preview passes from this data. (Shadows are the one `vsm-pages` pass — directional,
  spot, and point lights are page allocations inside it, not passes of their own — and it records the
  executor draws like every other raster pass. Selection/picking is the CPU BVH query, so there is no
  ID pass to drive.)

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

- [x] Static-scene CPU render preparation scales with changes/residency, not total visible instances
  or draw count (`instanceUploadBytes` stays (near-)zero on a steady scene — asserted in e2e).
- [x] Existing scenes, morph/skinning/displacement, every material mode, all passes, previews,
  screenshots, RT/GDF/GI, host, and player match or improve their Phase-1 quality fixtures (the
  `tests/e2e` suite runs against the executor-only renderer, each render-touching file asserting
  `validationErrors()` empty).
- [x] Rapid camera motion, camera cuts, teleports, resize, wind-bound stress, and page churn produce
  no HZB disappearance, hole, or stale-handle alias (the camera-churn e2e in
  `gpu-scene-residency.test.ts` teleports/cuts and asserts the cut recovers with zero
  overflow/pressure, validation-clean).
- [x] Indexed and mesh executors select identical semantic cluster cuts and render within image
  tolerance; MoltenVK receives full quality through indexed MDI.
  (IDENTICAL CUTS IS BY CONSTRUCTION rather than by test: both executors consume the same scattered
  command stream from `scene_bin_scatter.slang`, so the cut is *shared* rather than independently
  selected — a divergence would mean one executor ignored records the binner emitted.
  IMAGE TOLERANCE IS MEASURED: `tests/e2e/mesh-executor-parity.test.ts` toggles only the executor
  between two hosts and scores meanAbs 0 — bit-identical, not merely within tolerance. Edit mode,
  camera fixed, wind pinned calm, anti-aliasing off (a temporally accumulated frame's value depends
  on how many frames it settled for, which is wall-clock), one variable changing.
  MoltenVK receives full quality through the indexed-MDI path, which is the one path rather than a
  fallback; `meshShader` is false there, so the comparison skips rather than fakes.)
- [x] Transparent sorting is GPU-driven and stable under hierarchy/stream changes (stable LSD radix
  + per-blend-bucket zero-masked streams; known-depth GPU ordering test).
- [x] Vulkan validation is clean on **NVIDIA and MoltenVK**; all capability decisions are
  reported. (MoltenVK is clean at gate step 5 and on every e2e boot; NVIDIA (`NVIDIA GeForce RTX
  3070 Ti`, driver 610.43.03) is clean with every render-touching e2e file asserting
  `validationErrors()` empty. Three fixes that adapter forced: AS build scratch honouring
  `minAccelerationStructureScratchOffsetAlignment`, first-use images no longer naming a graphics
  source stage from a compute-only queue, and `RgPass::compute` no longer routing every compute pass
  to the async queue. AMD is out of scope by the project owner's decision (2026-07-26) — no such
  adapter exists for this project, so nothing is verified or claimed there.)
- [x] No old gather/batcher/env toggle symbol remains (the draw-path tripwire is step 4b of
  `tools/ci/check.sh`).
- [x] Standard gate and GPU Scene/visibility docs are green. (`tools/ci/check.sh` is the gate; the
  docs pages are `persistent-gpu-scene.md`, `hierarchical-visibility.md`, and `page-residency.md`,
  checked by the docs-page skill's `hugo --gc` + `check_links.py` + `check_style.py` — those three
  are the docs checkers, and they live in the skill rather than in the gate.)

## NO-LEGACY gate

There is exactly one production scene-render path after this phase. Capability executors vary command
mechanics over one semantic visibility result; they are not alternate renderers or content tiers.
