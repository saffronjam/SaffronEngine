# Phase 11 — Virtual shadows, lighting, GI, and ray tracing

**Status:** COMPLETED

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
  casters shadow from their base geometry: the page traversal runs `displaced_records: 0`, so every
  instance walks its base pages and displacement detail is a camera-pass refinement rather than a
  shadow term.)*
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
- [x] Update aggregate voxel injection/sampling so triangle↔voxel transitions preserve indirect
  irradiance, sky visibility, reflection response, and transmitted energy within error bounds.
  (TRANSMITTED ENERGY IS PRESERVED BY CONSTRUCTION. `parity_occupancy` existed and was unit
  tested but **had no production caller** — injection used the separately authored
  `VoxelMaterialMoments.occupancy`, which is the "two authored guesses" its own doc names as the
  thing to avoid. `derive_parity_occupancy` in `assets/src/render_material.rs` now solves occupancy
  from the sheet's own optics instead: the density at which marching its mean thickness transmits its
  mean transmission. The authored occupancy no longer feeds injection at all.
  The extinction coefficient is achromatic, so one density stands for three channels; the channel
  MEAN is used, because it preserves total transmitted energy rather than favouring a perceptual
  weighting the marches do not apply.
  Proven by `aggregate_occupancy_transmits_what_the_triangles_it_replaces_did`: for three
  transmission/thickness pairs, `aggregate_transmittance(derived, thickness)` returns the sheet's
  transmission within 1e-3, and the degenerate cases (opaque matter, zero thickness) resolve solid.
  `occupancy_follows_the_densest_sheet_and_solid_wins` was rewritten around derived densities rather
  than authored bits.
  THE TRANSITION IS NOW MEASURED, not inferred. Doing it needed a lever the engine lacked: the cut
  is chosen inside the traversal by projected appearance error, so the only way to reach the
  aggregate form was to fly the camera out — which shrinks the subject at the same moment it
  coarsens it, and an image difference that conflates "the plant got smaller" with "the plant
  transmits differently" measures nothing. **The obvious test was the wrong test.**
  `SceneTraversalPush::representation_override` (the former `reserved0` padding word, so the
  48-byte layout is unchanged) pins the cut instead: `SCENE_CUT_FORCE_COARSE` never refines —
  where aggregate voxels live — and `SCENE_CUT_FORCE_FINE` always does. It reaches the renderer
  from `SAFFRON_CUT_OVERRIDE` (`coarse`/`fine`), read once at construction, so a test boots two
  hosts that differ in exactly that and nothing else: same camera, same scene, same lighting, wind
  pinned calm.
  `tests/e2e/vegetation-representation-parity.test.ts`: the two cuts are **8.97** mean
  absolute per-channel difference apart — genuinely different pictures, asserted so the comparison
  cannot pass on an override that did nothing — while mean frame brightness differs by only
  **2.93** of a 6 budget. Different silhouette and detail, the same amount of light, which is
  precisely the claim: an aggregate voxel whose occupancy disagreed with the transmission of the
  leaves it replaces would move that second number. Indirect irradiance, sky visibility and the
  reflection cone all march the same `sdfExtinctionStep` with this occupancy and so ride the same
  result.)
- [x] Parameterize GI/reflection culling through the same hierarchy and residency demand rather than
  rebuilding plant-specific lists.
  (SCREEN-SPACE CONSUMERS INHERIT THE CAMERA CUT TRANSITIVELY: SSR, SSGI and `gi_resolve` read only
  the camera G-buffer. The world-space ones — DDGI, the global SDF/GDF, DFAO, specular occlusion, RT
  reflections and the ReSTIR resolve — consume the reach view and the occluder scatter below.
  THE CUT MUST NOT BE THE CAMERA FRUSTUM. Worth stating before anyone reaches for the obvious fix:
  GI occluders shadow visible surfaces from off-screen, so frustum-culling these sets removes
  occluders that legitimately contribute and darkens nothing that should be dark. The correct gate
  is the GDF cascade window — an occluder outside the furthest cascade cannot affect any march —
  plus the traversal cut and residency demand the box names.
  THE FAILURE MODE IF THE GATE IS WRONG IS SILENT: occluders disappear from GI and no validation
  layer or test notices, which is why `sdfInstancesDropped` is on `render-stats` at all.
  `gi_occluder_bounds(eye)` (`global_sdf.rs`) is the window — the coarsest cascade's, dilated by one
  cascade-0 extent. The dilation is not slop: the cascades re-centre later in the frame than the cut
  runs, so the margin covers a frame of camera motion instead of racing that ordering. `render-stats`
  names culling apart from dropping — culling is a claim about reach, dropping is geometry lost to
  capacity, and only the first is sound.
  A NOTE FOR WHOEVER TRIES TO OBSERVE THIS END TO END: builtin presets are uploaded with
  `SdfSource::None`, so a scene of cubes and spheres contributes no occluder and the counter cannot
  move. An e2e assertion needs a project mesh with a baked field or a cooked plant.
  GI CONTRIBUTES PAGE-RESIDENCY DEMAND.
  `PageDemandView` carries the reachable window beside the camera, and the streaming scorer ranks
  in three tiers instead of on-screen or not: visible content wins outright, a page a march or a
  reflection reads outranks one nothing consults, and the two flags do not compound. Before this a
  page feeding a reflection was scored identically to one nothing reads — which is how GI came to
  march against pages that had never been demanded, a hole in the gather rather than a missing
  pixel. `gi_reachable_pages_outrank_pages_nothing_reads` pins the ordering as an inequality rather
  than as three magic constants, so retuning the weights cannot silently invert it.
  BOTH SETS ARE CUT AGAINST ONE WINDOW. The ray instances and the SDF occluders are gated against
  the same coarsest-cascade window — the occluders by the reach visibility view's device cull plus
  the per-field test in `gi_occluder_scatter.slang`, the ray instances by `window_intersects`
  (`gpu_scene_mirror/facts.rs`) — and a resident cell reaches far past what any ray does, so the
  mirror's plants are gated by it too. Reported as `rtInstancesCulled`, named apart from
  a drop for the same reason `sdfInstancesCulled` is: culling is a claim about REACH and is sound,
  while an instance lost to capacity is geometry silently missing from reflections.
  ONE WINDOW, NOT TWO. `gi_occluder_bounds` is the only definition of reach, pushed to the reach
  cull and read by the mirror's cut, so the two cannot drift into disagreeing about what reachable
  means — which would surface as a reflection and a cone trace gathering from different sets of
  occluders.
  `the_ray_cut_keeps_what_a_ray_can_reach_and_drops_what_it_cannot` pins the cases that matter: an
  occluder DIRECTLY BEHIND THE EYE survives (the one a frustum cull gets backwards), one outside
  the coarsest cascade drops, a straddling one is kept because the cut may only drop what it can
  prove unreachable, and a scaled instance is judged on its world extent rather than its local one.
  THE 4096-INSTANCE CLAMP IS NOT INVISIBLE: `sdfInstancesDropped` reports it on `render-stats`, so a
  scene that loses GI occluders to capacity fails a test instead of looking correct. Covered by
  `tests/e2e/rt-telemetry.test.ts`.
  VEGETATION REACHES BOTH SETS. It never enters the ECS — the mirror syncs it straight into the
  persistent GPU scene and micro-field blades are reconstructed GPU-side in `add_micro_field_passes` —
  so it takes its own route. `GpuSceneMirror::ray_instances` derives a plant's ray instances from the
  RETAINED plant state, not the sync delta, which would have published a plant once and then lost it
  on the next unchanged frame; a multi-prototype family expands into one TLAS instance per use over
  per-prototype structures. `tests/e2e/vegetation-rt.test.ts` asserts resident plants move
  `rtInstances` and bring their own bottom-level structures. Grass stays out of ray tracing: blades
  exist only as GPU-reconstructed micro candidates with no instance to publish.
  THE GPU-SIDE REACH VIEW is what the CPU gate could not do.
  `SCENE_VISIBILITY_PASS_REACH` is a third pass kind over the same instance sweep: no projection, no
  pyramid, no retest list — an instance survives iff its world sphere meets the box
  `gi_occluder_bounds(eye)` returns, the same function the CPU cut calls, so the two cannot drift.
  The renderer's `gi_view` runs it while the distance field does and walks the hierarchy in demand-only
  mode (`SceneTraversalPush::demand_only`), so the pages a gather reads are demanded without a draw
  record being emitted for a view that draws nothing. Reported as `giReachVisible` / `giReachCulled`
  and asserted in `tests/e2e/visibility-counters.test.ts`, whose load-bearing half walks the camera
  a hundred kilometres away and requires the count to MOVE — a reach cull that never rejects reads
  exactly like one that is not wired up.
  DEMAND IS PRICED BY WHO MISSED. `gpuSceneRequestPage` carries a `SceneViewClass` ordinal, so
  the CPU can tell a camera miss from a shadow-page miss from a gather miss instead of giving every
  one `u64::MAX / 2`; the bands sit above `PAGE_DEMAND_PREDICTED_CEILING`, so an actual miss
  outranks every predicted score. Eviction is least-recently-demanded first and then by the
  cheapest reader, and a page's priority is REPLACED on a later frame rather than accumulated —
  a running maximum would let one camera glance protect a page over the image forever, and
  `a_page_nobody_asks_for_goes_before_one_a_gather_still_reads` fails on exactly that mutation.
  THE REQUEST BUFFER IS PARTITIONED BY CLASS. Every view in the frame appends missing-page requests —
  the camera, up to twelve shadow-page views, and a reach view sweeping a hundred-metre box. One
  shared FIFO ordered by nothing but the atomic race would let a gather crowd out the camera, whose
  requests are the ones that are holes in the image, so each
  class owns a count word and a region and what a class loses follows from its own volume;
  because the region IS the class, the entry is one word rather than a (slot, class)
  pair. Overflow is not silent either: the count
  word already ran past the ceiling, so the drain reports the difference as `requestsDropped`
  with a per-class `requestOverflowClasses` mask. `page-request-budget` lowers the effective
  per-class ceiling so the path is reachable at all — the `vsm-page-budget` precedent — and
  addressing deliberately uses the ALLOCATED capacity, because a region base that moved with
  the budget would leave the two halves reading different memory.
  `a_flooded_class_loses_only_its_own_requests` is the proof and it dies the moment the drain
  reads one shared region.
  THE CPU PRIORITIZER RANKS PLANTS TOO: `drive_page_streaming`'s frontier scorer walks
  `world_state.plants` alongside `world_state.instances` in one loop. Vegetation never enters the ECS
  and is where most of the paged geometry is, so a scorer that walked instances alone would rank the
  smaller half of the scene and leave the larger half to demand-on-miss.
  THE OCCLUDER SET IS SCATTERED ON THE GPU:
  `GlobalGpuData::sdfs` (a `GpuSdfTableRecord` per baked field: grid bounds, encode clamp, bindless
  slot, dims) plus a `prototype_sdfs` arena, `GpuScenePrototypeGpuRecord::sdf` replaced by
  `sdfRange: GpuArenaRange` — RANGE-SHAPED, one occluder per field, because the tightness is what
  lets small fields cull independently — and `GpuSceneInstanceGpuRecord::sdf` deleted outright
  (occluders are a prototype property). The mirror's `insert_mesh_sdfs` gives `CreateSdf` its first
  callers ever; refresh and removal retire both halves. `gi_occluder_scatter.slang` walks the reach
  view's visible list, re-tests each FIELD's world AABB against the reach window, resolves slot 0's
  material (with instance overrides) for proxy albedo + occupancy — both newly carried on
  `GpuMaterialTableRecord`, stride unchanged at 64 — and appends `SdfInstance`s into per-frame-slot
  regions with the count in meta words, because a GPU-produced count cannot ride a push constant.
  `gdf_cull` and the DDGI near-field march read the count from those words (light-set binding 15 /
  cull-set binding 2). No CPU occluder upload survives beside it — the scatter is the only producer.
  `sdfInstancesCulled` / `sdfInstancesDropped` are sourced from the meta readback, culled being the
  scatter's per-field window reject count.
  ONE HONEST TRADE, recorded rather than hidden: the GDF near cascade's moved-occluder dirty
  regions diffed CPU-side AABBs that no longer exist, so cascade 0 joins the same staggered
  round-robin full refresh the far cascades always used (slab-amortized, reconverging within the
  round-robin period). A moving occluder's near-field AO now lags a few frames instead of
  recompositing same-frame; a static scene composites exactly what it did before. The AABB-diff
  machinery (`set_instances`, `cur_aabbs`/`prev_aabbs`, `aabb_changed`) is deleted with its caller.
  GLOBAL_GPU_DATA_ABI_VERSION bumped to 2; the byte-lock and slang-lock tests cover the new record
  and the widened material record.
  PLANTS BAKE FIELDS. A `DistanceField` compiled section
  (`PLANT_COMPILED_ARTIFACT_VERSION` 3 → 4) derives a family-space SDST field from the coarsest
  aggregate voxel brick's occupancy — the same grid the aggregate raster form draws, so a march
  occludes against what the coarse cut shows, and family space is what makes it correct for
  assemblies whose triangle streams are prototype-local. The transform is an exact integer
  Felzenszwalb–Huttenlocher EDT (bit-identical on every target), packed by the SAME
  `saffron_geometry::Sdf` codec the mesh bake and its sidecar use — one format, no second encoder.
  `upload_mesh`'s bake parameter became `SdfSource { None, Bake, Cooked }`; `plant_render` decodes
  the section and passes `Cooked`, so `GpuMesh::sdfs()` is non-empty for a family with a voxel
  brick, the mirror publishes the records, and the scatter emits plant occluders like any mesh's.
  An empty section (a family that cooked no brick) reads as "no field", not an error.
  TWO CALIBRATION DEFECTS FOUND AND FIXED BY MEASUREMENT, because the parity test caught both.
  First: the root brick's dilated occupancy is a solid family-sized box (8³, 512/512 inside on the
  unit fixture) — a wall, not a plant — so the field now rasterizes the DEEPEST voxel level's
  bricks into a `bake_grid`-sized family grid before the EDT. Second: the scatter's fallback to the
  drawn material's occupancy served the trunk's solid 1.0 for the whole canopy, and DDGI's
  near-field march then treated every plant as a hard occluder, darkening the fine cut's own leaves
  by 23 mean-brightness points while the coarse cut never moved. The fallback is DELETED by design:
  every field states its own occupancy — a triangle bake writes solid 65535 (walls occlude; set in
  `from_dense_field`, `SDF_FORMAT_VERSION` 3 → 4 so stale sidecars rebake), a cooked plant writes
  `derive_parity_occupancy(transmission_mean, thickness_mean)` from the brick moments. Probe
  numbers: fine/coarse energy delta 25.8 with the defects, 2.88 with the fix — the historical
  parity was 2.93. `vegetation-representation-parity` and the `vegetation-churn` trample test are
  the regression guards.
  THERE IS ONE DOOR INTO GI AND EVERY OCCLUDER GOES THROUGH IT: `gdf_composite.slang` composites
  exclusively by walking `SdfInstance`s through `sampleMdfBrick`, so an occluder is a brick-backed
  field or it is nothing — which is why a plant's coarse `VoxelHierarchy` occupancy has to become a
  cooked field rather than a second kind of occluder.)

## Portable KHR ray tracing policy

- [x] Static nondeforming whole-family/variation representations may share compacted BLAS where their
  transforms/material classification permit. *(SHARING: a BLAS is built once per mesh at upload and
  every static instance of that mesh references it, so instancing shows up as `rtInstances` and
  `blasCount` diverging — `blasCount` is now counted from the distinct AS device addresses in the
  frame's TLAS rather than a dead "built ever" counter that never had a caller. e2e `rt-blas`: four
  added cube instances move `rtInstances` by exactly 4 and `blasCount` by at most 1, and the frame is
  validation-clean. TRANSFORMS AND MATERIAL CLASSIFICATION PERMIT IT BY CONSTRUCTION: the transform is
  per-`VkAccelerationStructureInstanceKHR`, and a geometry's opacity comes from the cooked material
  class of its material-homogeneous submesh (`cooked_submesh_opacity`), so an instance whose runtime
  material contradicts that class expresses the disagreement through `instance_opacity_flags` —
  `FORCE_OPAQUE`/`FORCE_NO_OPAQUE` plus `DISABLE_OPACITY_MICROMAPS_EXT` — rather than through a second
  structure. COMPACTED: `mesh_blas_build_flags` adds
  `ALLOW_COMPACTION`, and `Uploader::compact_mesh_blas` reads the
  `ACCELERATION_STRUCTURE_COMPACTED_SIZE_KHR` query and copies through
  `record_blas_compaction` (`CopyAccelerationStructureModeKHR::COMPACT`) into an exactly-sized
  structure; a driver reporting no saving keeps the built one, since compaction is a memory win and
  never a correctness precondition.)*
- [x] For structured deformation, materialize the selected assembly hierarchy into GPU geometry for
  BLAS build/update through the shared deformation output and cache policy. Do not assume KHR AS
  supports nested micro-instance parts inside one plant BLAS.
  (THE WARNING IN THE BOX IS THE DESIGN. KHR structures cannot nest micro-instance parts, so a
  family is **one structure per prototype plus one TLAS instance per placed use** — never a merged
  plant BLAS. `MeshAssembly::prototype_slices` records each prototype's `AssemblyPrototypeSlice` of the
  flattened index stream (derived from the uploaded submesh table, the authoritative layout),
  `record_mesh_blas_build` takes a `MeshBlasGeometry` carrying that range, and `GpuMesh` holds the
  per-prototype set in `assembly_blas`. `Rt::prepare_tlas_build` expands an assembly input into one
  instance per use its combination leaves active, composing the use's family-local matrix with the
  instance's world transform.
  A PARTIAL SET IS REFUSED: if any prototype's structure fails to build, the whole set is dropped
  rather than placed. Half a canopy casting is worse than none casting, because it looks correct.
  PLANTS REACH THE TLAS THROUGH RETAINED MIRROR STATE. Vegetation never enters the ECS, so nothing
  that walks entities can see it. `GpuSceneMirror::ray_instances()` derives a plant's inputs from
  RETAINED plant state rather than from a sync delta: accumulating during the
  delta walk publishes an empty set on any steady frame, which silently removes every plant from the
  TLAS. It also places single-prototype families, which are cooked as plain meshes — requiring the
  assembly form drops them, and the fixture is exactly that shape.
  Measured by `tests/e2e/vegetation-rt.test.ts`: a resident cell moves `rtInstances` 1 → **5**
  and `blasCount` 1 → **2**, with `blasBytes` rising and validation clean.)
  (HARDWARE NOW PRESENT — this box needs code, not a machine. The RTX 3070 Ti advertises
  `VK_KHR_acceleration_structure`, `VK_KHR_ray_query`, `VK_KHR_deferred_host_operations`,
  `VK_EXT_opacity_micromap` and `VK_NV_cluster_acceleration_structure`, and `Device::new` resolves
  `rt_supported = true`. Acceleration structures build and are traced validation-clean here after two
  fixes: the build scratch now honours `minAccelerationStructureScratchOffsetAlignment`, and AS
  storage takes a dedicated VMA allocation — suballocating it beside ordinary buffers wedged the GPU
  on a later unrelated submission. Still unbuilt for this box.)
- [x] Represent aggregate voxel clusters through cooked triangle surfaces or procedural AABBs with
  intersection/hit shading that matches raster aggregate moments.
  (REBUILT AND LANDED 2026-07-28, after the wedge that forced the revert was root-caused and fixed
  (see the box below — uninitialized table slots, not the structure build). The landed shape is the
  recorded design: `Uploader::build_aggregate_blas` builds one family-space structure over the root
  cut's voxel-brick surfaces when every root is a voxel brick — cooked triangle surfaces, no
  procedural AABB, no intersection shader — and TLAS packing (`aggregate_stands_in`, `RtCutView`)
  runs the traversal's own refine test on the root's `appearance_error.total` with the camera's
  eye/projScale/threshold/override, so a distant family packs ONE coarse instance instead of one
  per use and both representations swap on the same boundary. Hits resolve through the instance's
  scene slot; the structure builds opaque (merged coverage). `render-stats` reports
  `rtAggregateInstances`; e2e `rt-telemetry` pins the cut coarse and asserts every instance swaps,
  then fine and asserts none do, validation-clean.
  THE ORIGINAL FAILURE HISTORY, kept because its negatives were most of the work:
  BUILT, TICKED, THEN REVERTED AND UN-TICKED 2026-07-27, because the full gate found it hangs the
  GPU. That sequence is the note: the vegetation suites it was proven against do not exercise the
  ASSET-PREVIEW path, and that is where it fails.
  THE DESIGN WAS RIGHT AND IS WORTH REBUILDING. `PortableVoxelBrick` already carries an indexed
  surface beside its occupancy, so the aggregate needs no procedural AABB and no intersection
  shader — it is a structure over triangles the cooker already emitted. Shading matches by
  construction rather than by agreement: an aggregate raster record resolves material slot 0 and the
  ray hit resolves through the instance's scene slot, so there is no second shading path to keep in
  step. Selection ran the traversal's own projected-error test on the prototype's root page with the
  same `projScale` and the same one-pixel threshold, and honoured `SAFFRON_CUT_OVERRIDE` so a pinned
  cut pinned both representations.
  THE DEFECT, BISECTED PRECISELY. `tests/e2e/vegetation-graph` fails deterministically —
  `timeout calling exit-asset-preview`, the host wedged. Four probes, each a single variable:
  aggregate built AND traced → hang; built but NEVER traced → hang; buffers uploaded but the
  structure build SKIPPED → pass; the build's `ALLOW_DISABLE_OPACITY_MICROMAPS_EXT` flag removed →
  still hang. So it is the BUILD ITSELF — one additional acceleration-structure build during
  `upload_mesh` — and neither the geometry nor the tracing nor the build flags.
  THE GEOMETRY IS NOT MALFORMED, which is what makes the build the interesting suspect rather than
  the obvious one. A probe over a cooked cube's hierarchy returns 1536 vertices, 2304 indices, a
  maximum index of 1535, and bounds of exactly [-0.5, 0.5]. The build range is right too:
  `primitive_count = index_count / 3`, `primitive_offset = first_index * 4 = 0`, and
  `max_vertex = count - 1`.
  WHERE TO LOOK, AND FOUR PLACES IT IS NOT. The obvious explanation was that an assembly upload
  already runs one `build_mesh_blas` per prototype plus the micromap builds, each its own one-off
  submit and wait, and that adding one more tips it. FOUR MINIMAL PROBES SAY OTHERWISE, each a
  single variable against `vegetation-graph` — the deterministic oracle — and each one PASSING:
  an extra structure built over the mesh's own geometry; an extra structure over FRESH SMALL BUFFERS
  staged and copied exactly as the aggregate's were; the same with a deliberately DEGENERATE
  zero-area triangle appended, which a voxel brick's surface can legitimately contain; and the same
  probe moved to the exact point in `upload_mesh` the aggregate build occupied, after the prototype
  structures and the micromaps. Not the count of builds, not fresh buffers, not degenerate
  topology, not ordering.
  THE BRICK PAYLOAD IS NOT IT EITHER, measured device-free on the exact family `vegetation-graph`
  cooks. A native family cooks seven nodes, two of them voxel, and BOTH bricks are 1536 vertices /
  2304 indices with a maximum index of 1535 and Q15.16 bounds of roughly [-0.33, -0.95, -0.49] to
  [0.59, 4.01, 0.54] — finite, small, well-formed, and the same size as the cube's. No extreme
  magnitude, no non-finite coordinate, and nothing like a surface large enough to outrun a driver
  watchdog.
  A FAITHFUL REPLICA OF THE AGGREGATE BUILD DOES NOT HANG, which is the finding that matters most
  and the one that says where to look next. The final probe reproduced every property of it at once —
  fresh small buffers staged and copied the same way, vertices carrying POSITIONS ONLY with every
  other field zeroed exactly as a brick's are, the structure and its buffers RETAINED for the
  process rather than dropped after the build, at the same point in `upload_mesh` the aggregate
  occupied — and `vegetation-graph` passes.
  SO SIX EXPLANATIONS ARE ELIMINATED AND NONE OF THEM WAS IT: not the number of builds, not fresh
  buffers, not degenerate topology, not ordering, not the retained structure's lifetime, and not the
  brick geometry of the family under test.
  WHAT THAT LEAVES is the one thing the replica did not copy: it built from the MESH's geometry,
  while the aggregate built from each mesh's own brick — for EVERY mesh the preview scene uploads,
  not just the plant. The floor, the studio sphere and the preset primitives all cook hierarchies
  too, and none of their bricks has been inspected. The next step is therefore to put the feature
  back with a log of every surface's vertex count, triangle count and bounds at upload, and find the
  one that is not like the others; failing that, GPU-assisted validation armed long enough to reach
  the failure, or a device-fault capture, is the tool that names the faulting instruction.
  These negatives are recorded because they are most of the work: six bisection runs, and they
  eliminate every explanation a reader would reach for first.
  Reverted whole rather than left behind a flag: a feature that hangs the GPU half the time is worse
  than an open box, and a disable switch would have been a second code path this repo does not
  keep.
  THE ASSET-PREVIEW PATH WEDGES ON THE CURRENT TREE TOO, with the aggregate build absent — found
  2026-07-28 while making the schema check headless. That is the fact worth carrying: this box's
  bisection assumed the extra structure build was the cause, and the same wedge reproduces without
  it. They may be one defect the build merely made likelier, or two; nothing here proves either.
  A DETERMINISTIC TRIGGER NOW EXISTS, which is what the capture was waiting on. Adding
  `SAFFRON_EDITOR_NATIVE_VIEWPORT: "1"` to the host spawn in `tools/check-control-schema/check.ts`
  — the offscreen mode `tests/e2e` already uses — makes `just schema` wedge **5 runs out of 5**.
  Committed (windowed), the same check wedged **1 run in 8** on the same afternoon. The signature is
  identical either way: `GPU submission 'frame N' has been in flight 3s — a hang, not a slow frame`,
  then `preview thumbnail render: ... ERROR_DEVICE_LOST`; the main loop stops, so the control socket
  stops answering and the contract test reports `timeout calling list-assets` — the first call after
  the device died, not the cause.
  HEADLESS ALONE IS NOT THE TRIGGER: `just e2e` boots an offscreen host per test file and never
  hits it. Three minimal offscreen probes also pass — a scratch project plus `import-model`, the
  same plus `play`, and the same plus `get-thumbnail` — so it needs more than any of those.
  THE TRIGGERING CALL IS NAMED, by tracing every command the contract test issues: `save-project`,
  `load-project`, two `project-status` polls, `get-stores`, `set-stores`, `list-assets`, then
  **`get-thumbnail`** — the last call through; the next `list-assets` times out. A MESH THUMBNAIL
  RENDERED SHORTLY AFTER A PROJECT RELOAD, which is why the `import-model` + `get-thumbnail` probe
  above misses it: no reload.
  FOUR EXPLANATIONS ELIMINATED, each a single variable against the deterministic repro.
  It is NOT A STATIC MEMORY ERROR: with `VK_KHRONOS_VALIDATION_VALIDATE_SYNC=true` the same run
  PASSES and reports no `SYNC-` hazard, so it is timing-sensitive — which also explains why the
  window masks it and why this went years as an intermittent.
  It is NOT A RACE WITH THE MAIN LOOP'S IN-FLIGHT FRAME: `device.wait_idle()` at the top of
  `render_preview_scene_to_png` still wedges 3/3. The hang is INSIDE the preview render, starting
  from an idle GPU.
  It is NOT `frame_begun` LATCH STEALING: `drive_preview_render_queue` runs in `on_update`, before
  `begin_frame`, so each preview `render_scene_offscreen` does its own `begin_offscreen_frame`.
  It is NOT HEADLESS ITSELF, per the e2e evidence above.
  WHERE IT LIVES, THEN: `render_preview_scene_to_png` (`host/src/layer.rs`) loops up to 256 times
  over `mirror.sync_renderer_world(ViewId::Thumbnail, preview scene)` + `render_scene` +
  `render_scene_offscreen`, driving the SHARED `GpuSceneMirror` with a second scene. That loop, on
  an idle GPU, immediately after a reload, is the whole remaining surface.
  CAPTURED AND FIXED 2026-07-28. The capture that named it: `VK_NV_device_diagnostic_checkpoints`
  markers on every graph pass plus a `VK_EXT_device_fault` query on the loss paths (both now
  permanent, `rendering/src/checkpoints.rs`). The wedge frame stopped at the `wind-deform` marker
  with `READ_INVALID at 0xf744246000` — an address a full buffer-lifetime trace proved NO buffer
  ever occupied, so not a use-after-free but a wild pointer computed FROM live data.
  THE DEFECT: `GpuSceneAddressBlock.instance_capacity` advertises the table's PHYSICAL capacity
  (`slot_capacity()` = `storage.capacity()`), and capacity-wide dispatches — wind-deform first in
  the frame — read every slot header. But `GlobalGpuArena` never cleared its buffer at creation,
  and growth copied only the live prefix, so slots between the written high-water and physical
  capacity read whatever recycled device memory last held. A garbage `occupied` word admits a
  garbage record whose `prototype.index` walks off into unmapped VA. That is why the repro needed
  a long command prefix (VMA had to recycle dirtied blocks under the preview world's fresh
  instance table), why one reactive-idle tick (`viewport-native-info`) shifted it, why sync
  validation's timing masked it, and why the 11:263 bisection's extra AS build "caused" it —
  every allocation-pattern shift re-rolled which junk landed under unwritten slots.
  THE FIX: every arena byte reads as zero until a staged write covers it. `GlobalGpuArena::new`
  fills the fresh buffer synchronously before any address escapes, and every growth op
  zero-fills the tail beyond the preserved prefix (`GpuArenaGrowth::enqueue` records the prefix
  copy + a disjoint `cmd_fill_buffer`). Verified: the 5/5-wedging headless schema check passes
  3/3 with the fix; the full gate, workspace tests, and e2e stay green.
  THE HEADLESS SWITCH LANDED with it: `check.ts` now spawns the host with
  `SAFFRON_EDITOR_NATIVE_VIEWPORT=1`, so the contract test qualifies the same no-surface device
  as the rest of the gate.)
- [x] Standard KHR any-hit over canonical coverage remains baseline-correct. *(THE PATH: a masked
  thin-sheet blocker (albedo-alpha coverage source, `masked` classification) packs FORCE_NO_OPAQUE,
  so its triangles surface as ray candidates instead of auto-committing, and
  `gpuSceneRayCandidateCovered` decides each one through the shared `classifyCanonicalCoverage` —
  standard KHR any-hit, no OMM and no vendor extension. Two fixes were needed to reach it:
  `point_shadow_meta.z`, the gate `lighting.slang` tests before tracing a shadow ray, was hard-coded
  `0` behind a comment claiming the RT phase folded it in, so the path was *unreachable* — it now
  comes from `Rt::shadows_enabled`; and acceleration structures no longer wedge the GPU.
  THE EVIDENCE, e2e `rt-anyhit`: flipping the blocker's coverage between covered and cut-out changes
  the shadow it casts on the receiver — mean 202.0 shadowed against 216.5 lit over the patch the ray
  shadow lands on, a 14.5/255 margin asserted against a threshold of 5, validation-clean with a TLAS
  built and `rtInstances`/`blasCount` non-zero. The patch is not guessed: it was located by diffing a
  normal frame against a build that rejects every candidate, so the pixels sampled are exactly the
  candidate-driven ray shadow. Whole-frame comparison cannot show this — the change is ~2620 px in
  1.44M and sits under that metric's noise floor, so a frame mean reads as no change at all.
  CHAIN PARITY separately: `ray_candidate_classification_matches_the_cpu_classifier`
  (`gpu_scene_upload.rs`) drives the same resolve-and-classify chain on the GPU and compares every
  resolved word against `classify_canonical_coverage` byte-exactly, green on this hardware.)*
- [x] Derive optional `VK_KHR_opacity_micromap` data from the exact coverage texture/mip/classification
  source and validate conservative/unknown states. OMM removes cost, never correctness.
  (**THE BOX NAMES THE KHR EXTENSION; THE TREE TARGETS EXT.** `VK_KHR_opacity_micromap` exists (last
  modified 2026-05-08) and differs
  architecturally from EXT: KHR creates micromaps *as* `VkAccelerationStructureKHR` and builds, copies
  and queries them with the acceleration-structure commands, rather than owning a separate
  `VkMicromapEXT` object and `vkCmdBuildMicromapsEXT`.
  **EXT is what is callable here, for two measured reasons**: driver 610.43.03 on the RTX 3070 Ti
  advertises `VK_EXT_opacity_micromap` rev 2 and no KHR variant, and `ash` is pinned `=0.38`
  (Vulkan 1.3.281), whose only micromap module is `ext::opacity_micromap`. Revisit if either changes;
  everything below uses the EXT name.
  THE CAPABILITY LAYER IS BUILT: `device.rs` probes the extension behind the RT gate — a micromap is
  only meaningful attached to an AS build — queries
  `PhysicalDeviceOpacityMicromapFeaturesEXT::micromap`, enables extension *and* feature at device
  creation, and resolves the `ext::opacity_micromap::Device` dispatch. Surfaced as
  `Capabilities::opacity_micromap` / `Device::omm_supported()` / `Device::omm_dispatch()` and as
  `ommSupported` on `render-stats`. Measured `ommSupported = true` on the RTX 3070 Ti with the device
  coming up validation-clean, asserted by `tests/e2e/rt-telemetry.test.ts` — which does not
  assert the capability's *value* (it is device-dependent) but does assert it is reported, that it
  never claims true without ray tracing, and that enabling it raised no validation message.
  THE DERIVATION IS BUILT AND PROVEN.
  `geometry/src/opacity_micromap.rs` derives conservatively: a micro-triangle is called opaque or
  transparent only where a min/max alpha pyramid proves every point of its UV footprint classifies
  that way, and anything else is UNKNOWN — which traversal treats as non-opaque, so the classifier
  still runs. The space-filling curve is transliterated from the published reference, round-trip
  tested at levels 0..=4. Subdivision follows texel density (4 texels per micro-triangle, the
  bilinear support), which bounds total work by the *texture* rather than the mesh. Uniform
  triangles emit special indices and no block.
  THE KEYSTONE, and it is mutation-checked:
  `settled_micro_triangles_agree_with_the_classifier_under_every_hash`
  samples inside every settled micro-triangle and runs the real
  `classify_canonical_coverage` across many salts, anchors and temporal phases, asserting agreement.
  The first version of that test used a soft-edged card and **passed even with a deliberately
  non-conservative rule** — the ramp was too narrow for the error to land in a settled block. A
  full-range gradient fixture fixed it: the mutation now fails with a named micro-triangle.
  GPU SIDE: `Micromap` (dedicated allocation, as acceleration structures learned to use) plus
  `record_micromap_build`, proven by `a_derived_micromap_builds_validation_clean`. That test caught
  a real defect — micromap build inputs must be **256-byte aligned**, and unaligned ones are invalid
  rather than merely slow.
  OPACITY LIVES ON THE GEOMETRY, WHICH IS WHAT LETS A MICROMAP MEAN ANYTHING. Per spec an
  instance-level `FORCE_OPAQUE`/`FORCE_NO_OPAQUE` overrides a micromap outright, so an engine that
  forced opacity per instance would render every micromap inert. `MeshBlasGeometry` carries `opaque`
  and `micromap: Option<&Micromap>` per geometry, one geometry per material-homogeneous submesh
  (`cooked_submesh_opacity` resolving the cooked `material_class`, non-opaque where a submesh has no
  cluster to answer for it), and `VkAccelerationStructureTrianglesOpacityMicromapEXT` is pushed onto
  the triangles geometry. `instance_opacity_flags` forces opacity only for an instance whose runtime
  material contradicts the cooked class, and that case also sets `DISABLE_OPACITY_MICROMAPS_EXT`,
  because a micromap derived for the cooked material describes coverage this instance does not have.
  `tests/e2e/rt-anyhit.test.ts` is exactly that case: it assigns a thin-sheet material at runtime to a
  built-in cube whose cooked class is Opaque. The *policy* half exists end to end too:
  `OpacityMicromapDerivation` (enable, subdivision cap, transparent/opaque thresholds) is packed into
  `omm_policy`/`omm_thresholds` in `global_gpu_data.rs`.
  THE CLAIM IS MEASURED RATHER THAN ARGUED.
  `vegetation-atlas-micromap` builds a thin-sheet family whose coverage is a diagonal cutout, then
  boots a SECOND host differing in exactly `SAFFRON_OMM=off` and compares settled frames.
  `meanAbsoluteDifference` is **0** — exactly, not within a tolerance, because a micromap that
  shifted a pixel would mean the conservative proof was wrong. And the comparison cannot pass
  vacuously: the same file asserts `ommMicromaps > 0` on the first host and `== 0` on the second,
  so two hosts that both attached micromaps (or neither) fail before the frames are compared.
  ATTACHMENT IS PROVEN ON DEVICE, not merely compiled: micromaps reach the GPU (`ommMicromaps = 2`
  for a two-slot family), settle BOTH opaque and transparent micro-triangles — an all-unknown
  derivation removes no work and would read as healthy — and raise no validation message, which is
  the real result for a build whose usage rows or subdivision could each be a VU violation.
  MUTATION-CHECKED: suppressing attachment in the uploader fails it at `ommMicromaps > 0`.
  THE FIXTURE HAD TO GO IN THROUGH A NATIVE FAMILY, and finding that out was most of the work. The
  other vegetation suites import a `.splant` sourced from an OBJ, and only the glTF importer ever
  sets `AlphaMode::Mask` — so an OBJ-sourced family cannot express a masked material at all, and no
  existing fixture could ever have exercised this. `plant-create` binds catalog materials directly,
  which is the one path where a test can author the coverage it wants to see.
  THE COOK-TO-DEVICE CHAIN IS WHOLE. `derive_family_micromaps` runs as a cook
  stage over the atlas alpha plane and emits into the `RayTracing` section (domain `/v2`, hierarchy
  format 4); `build_cooked_micromaps` rebuilds them at upload and feeds
  `MeshBlasGeometry.micromap`, retained on `GpuMesh` so they outlive every structure referencing
  them; the BLAS is one geometry per submesh (slice 4a); and
  `maxOpacity4StateSubdivisionLevel` is read into `Capabilities::omm_max_subdivision`, with a row
  above it DROPPED rather than clamped — clamping would mean re-deriving states at a coarser level,
  and a block decoded at the wrong level is not conservative, it is wrong.
  ONE BUG THIS FOUND, worth keeping: `VirtualHierarchyMaterial::from_surface` hardcoded
  `opacity_micromap: false` for every `MaterialSurface::Standard`, so masked standard materials —
  half the scope this box names — derived nothing, silently, with the whole suite green. It now
  permits OMM exactly when the alpha classification is `Masked`.)
- [x] Add optional `VK_NV_cluster_acceleration_structure` and partitioned-AS execution over canonical
  cluster/page data only after KHR correctness; no NVIDIA-specific plant representation.
  (BOTH HALVES ARE BUILT AND PROVEN ON THE RTX 3070 Ti. The partitioned half is at the end of this
  note; the cluster half follows immediately.
  No ash release ships the binding (0.38.0+1.3.281 is still the newest, checked against crates.io),
  so the extension enters through hand-transcribed bindings in ONE private module,
  `rendering/src/vk_nv_cluster.rs`, imported by exactly `device.rs` and `rt_cluster.rs` — a third
  importer in that grep is the review tripwire. Two belts keep the transcription honest:
  `the_pinned_ash_release_still_lacks_this_extension` fails on every ash bump with delete-or-repin
  instructions attached, and the device probe refuses any spec revision other than the transcribed
  4, because a revision bump can move struct layouts with no generator following them here. Struct
  sizes are pinned byte-exact against the header, and the bitfield packers are unit-tested.
  THE EXECUTION IS OVER THE CANONICAL CLUSTERS, exactly as the box demands: an assembly
  prototype's bottom level composes from its cooked `PortableTriangleCluster`s — one CLAS per
  cluster (8-bit local indices verbatim, per-cluster f32 positions pulled from the flat vertex
  stream, `source_submesh` as the geometry index so material resolution stays
  representation-blind, cooked opacity as the geometry flag), then one
  BUILD_CLUSTERS_BOTTOM_LEVEL over the written CLAS references, both through the indirect two-op
  batch in implicit-destinations mode on the uploader's one-off submit, with the address/size
  words landed in staging by an explicit transfer. The produced structure enters TLAS packing as
  a device address behind the `RtBlas` enum, so every consumer — instance packing, byte
  telemetry, retention — is representation-blind. Disqualifiers fall back to the KHR triangle
  build per prototype: no extension, clusters exceeding device limits, clusters not exactly
  covering the prototype's fine index range, and spans carrying cooked opacity micromaps (only
  the KHR geometry chain attaches those; OMM-in-CLAS via `opacityMicromapArray` is the recorded
  follow-up).
  VALIDATION NAMED TWO TRANSCRIPTION-ADJACENT MISTAKES AND THEN CAME UP CLEAN — the SDK layers
  understand the extension, as predicted: every destination address/size array must live in an
  AS-STORAGE-usage buffer (VUID-…-12307 — including the readback words, which are now
  device-local with a staged copy out), and SHADER_READ/WRITE are not valid access masks on the
  AS-build stage barrier between the two ops.
  PROOF: `render-stats` carries `clusterAsSupported` / `clusterBlasCount` / `clasCount`;
  e2e `rt-telemetry` asserts the capability's coherence and that plain single-prototype meshes
  never take the cluster path; the canopy suite's third test enables RT over the assembly family
  and asserts composed structures with CLAS on a supported device (verified live:
  `clusterAsSupported = true`, structures composed, validation-clean).
  THE PARTITIONED HALF IS NOW BUILT TOO, on the same terms: a second hand-transcribed module
  (`vk_nv_ptlas.rs`, spec revision 1, sTypes 1000570000…), byte-pinned against the header's real
  layouts (measured with a C probe rather than derived by hand — `WriteInstanceDataNV` is 104 bytes
  with the structure address at +96), quarantined to `device.rs` / `descriptors.rs` /
  `rt_ptlas.rs`, and covered by the same ash tripwire, which now names both modules.
  THE DESIGN IS INCREMENTAL, WHICH IS THE ONLY VERSION WORTH BUILDING. A partitioned structure that
  rewrote every instance each frame would be valid usage and would save nothing. Instead: instance
  slots are STABLE across frames (keyed by GPU-scene slot plus assembly use, entity for a deforming
  one), so a frame diffs the placements against what the structure already holds and emits a write
  only for what appeared or moved, an update for a structure address that changed under an unmoved
  transform, and an inert write for what left. Partitions are the world's own base cells
  (`BASE_CELL_EDGE_METERS`) hashed into a 256-entry table — the granularity content actually changes
  at — with deforming instances in the global partition since their bottom level is refit anyway.
  BOTH TOP-LEVEL FORMS DERIVE FROM ONE PLACEMENT LIST (`Placement`), so they cannot disagree about
  the same scene; the KHR path packs it into its instance array, the partitioned path diffs it.
  The structure alternates per-frame buffers, reading the slot the previous frame wrote and writing
  the slot whose fence is already waited — incremental and safe to overwrite at once. The seed is
  one inert-instance build per slot, because set 6 is statically bound and an allocated-but-unwritten
  structure is undefined rather than empty.
  PROVEN BY PICTURE, WHICH IS THE ASSERTION THAT MATTERS: e2e `rt-ptlas` runs two hosts over one
  ray-traced scene, one partitioned and one not, and the frames are BYTE-IDENTICAL (mean absolute
  difference exactly 0). A settled frame then emits ZERO ops, and adding a cube writes fewer
  instances than the table holds — sampled over a window, because these are per-frame counters and a
  reading taken after the fact reports a clean zero that is true and useless (the same trap the VSM
  page-budget arm records). On the wire as `ptlasSupported` / `ptlasPartitions` / `ptlasWrites` /
  `ptlasUpdates`.
  IT IS OPT-IN (`SAFFRON_PTLAS=1`), AND THE REASON IS TOOLING RATHER THAN THE ENGINE. The SDK's
  validation layers ship the extension's header but do not model it, and emit two messages no
  engine-side change can avoid: `VUID-VkGraphicsPipelineCreateInfo-layout-07990`, a descriptor-type
  mismatch for a shader variable that HAS no partitioned SPIR-V form to declare — confirmed against
  `vk.xml`, which registers no `spirvcapability` or `spirvextension` for this extension, so the
  shader correctly declares an ordinary acceleration structure — and
  `VUID-vkCmdDrawIndexedIndirectCount-None-08114`, which cannot resolve the structure's address to
  an acceleration-structure object because a partitioned structure is memory, not an object. A
  default-on path that cannot be validated is worse than an opt-in one that can, so the flag stays
  until the layers catch up; `rt-ptlas` whitelists exactly those two VUIDs and fails on every other
  validation message, so the whitelist cannot hide a real one.)
- [x] Track BLAS/TLAS build/update/compaction time, memory, selected representation, OMM hit classes,
  and page demand without copying vendor performance thresholds.
  (BUILD/UPDATE TIME rides the render graph's per-pass timestamps. `record_tlas_build_plan`
  splits into `record_blas_refits` and `record_tlas_build` under two named child scopes, so
  `blas-refit` and `tlas-build` time separately. That split is the point: refits scale with
  deforming instances and the TLAS with total instance count, and one combined number could not
  say which moved.
  OMM HIT CLASSES: `Micromap` carries its derivation classes, and
  `distinct_micromap_classes` sums them across the frame's instances — deduplicated by handle, for
  the same reason the BLAS bytes are, since instances of one mesh share its micromaps and charging
  per instance would report sharing as work. On the wire as `ommMicromaps`, `ommOpaque`,
  `ommTransparent`, `ommUnknown`. THE UNKNOWN COUNT IS THE OPERATIONAL ONE: micromaps present with
  everything unknown removed no classifier work at all, and reads as healthy unless it is reported.
  COMPACTION AND INITIAL-BUILD TIME HAVE THEIR OWN POOL. Those two run outside the graph on the
  uploader's private one-off submits, so no pass scope reaches them; a two-timestamp query pool on
  the uploader brackets each and accumulates into the shared `DeviceResources`, which is the one
  thing the uploader and the renderer both hold. On the wire as `accelBuildUs`.
  IT IS A SESSION TOTAL, NOT A PER-FRAME FIGURE, because that is what the work is: structures are
  built when content arrives, not every frame. Reporting it per frame would read as zero on every
  frame that loaded nothing, which is the number a reader would take as "builds are free".
  ONLY THE STRUCTURE SUBMITS ARE TIMED. Timing every one-off would charge staging copies and SDF
  bakes to a figure named for structure builds, and a wrong attribution survives every sanity check
  a right one would fail. A device with no timestamp support measures nothing rather than reporting
  a fabricated zero, and a failed query read is dropped for the same reason — zero reads as a fast
  build rather than as an unmeasured one.
  MEMORY: `AccelerationStructure` carries its own `size`, and `Rt` sums the distinct structures each
  frame into `blasBytes` / `tlasBytes` / `rtScratchBytes` on `render-stats`. Bottom-level bytes are
  deduplicated by device address for the same reason `distinct_blas_count` is — a structure shared by
  N instances charged N times would report instancing as memory growth.
  COMPACTION: a compacted structure records what its build reserved (`note_compacted_from`), so
  `blasBuiltBytes - blasBytes` is the realized saving. Measured on the RTX 3070 Ti: 254,848 built →
  115,072 kept, a **54.8% saving**, with TLAS 12,672 and scratch 6,912.
  SELECTED REPRESENTATION: `skinnedBlasCount` (refit) and `tessellatedBlasCount` (full rebuild) are
  reported beside the static `blasCount`, so a deforming scene is distinguishable from a static one.
  Covered by `tests/e2e/rt-telemetry.test.ts`, whose load-bearing case adds six instances of an
  existing mesh and asserts the byte total does **not** move.
  ATTACHMENT IS AT THE UPLOAD SEAM, which is what makes the class counts nonzero: `build_cooked_micromaps`
  builds a cooked family's micromaps on their own submit before the structure build reads them by device
  address, `GpuMesh` retains them so they outlive every structure referencing them, and
  `MeshBlasGeometry::micromap` carries the one belonging to each submesh — one geometry per submesh,
  because a single-geometry structure holds one class and one masked submesh would otherwise force the
  coverage classifier onto every other.
  PAGE DEMAND: the GI reach traversal runs as `SceneViewClass::Gi`, so its missing-page requests reach
  the same ring the camera and shadow-page views feed, tagged with the view class that asked.)

## Acceptance

- [x] VSM covers all current shadow-casting light families and the old fixed-map symbols/resources are
  absent. (The page table reserves a base per family — `VSM_SPOT_TABLE_BASE` after the 8 directional
  clip levels, `VSM_POINT_TABLE_BASE` after the spot pages, 6 point faces to `VSM_TABLE_ENTRIES` — and
  no fixed shadow-map symbol survives outside the VSM/contact/cloud paths. e2e `vsm`: a scene with
  directional + spot + point casters demands and allocates pages with `overflow == 0`,
  validation-clean.)
- [x] Rapid wind, interaction, camera travel, page churn, and phenotype transitions create no stale
  shadow, light leak, silhouette pop, or whole-tree invalidation storm. (Every arm is covered; the
  phenotype arm, the last to close, is at the end of this note. e2e `vsm`
  proves the invalidation half — a settled frame answers repeat demand from residency with zero fresh
  allocations, zero evictions and zero overflow, and a moving caster dirties far below the atlas page
  count without overflowing. THE IMAGE HARNESS NOW EXISTS (`tests/e2e/image.ts`: an RGB8 PNG decoder
  plus `regionMean` / `meanAbsoluteDifference`), and e2e `vsm-churn` uses it for the WIND AND CAMERA
  half: a settled reference frame, then four passes swinging the wind between calm and gale while
  flying the camera out and back without settling, then a return to the reference pose. The frame
  reconverges to within a mean absolute per-channel difference of 2.0/255, with `vsm.overflow == 0`
  and validation clean — a shadow left at a stale caster position does not fit in that budget. The
  test proves its own metric discriminates: a frame from one of the churn poses must score above
  5× the tolerance, so the assertion cannot pass on a blind metric.
  PAGE CHURN UNDER PRESSURE IS COVERED. A compile-time
  render budget of 64 against an atlas of 1024 tiles cannot be starved by any scene a test can
  build. `vsm-page-budget` makes the throttle settable: at ONE page a frame the dirty set
  outruns the refresh, which is the backlog the reconvergence path exists to survive.
  `a starved page budget still reconverges to the reference image` asserts the pressure was REAL
  before trusting the recovery — peak dirtied above zero and peak rendered at most one, sampled
  DURING the churn because those counters are per-frame and a sample taken afterwards reports a
  clean zero that is true and useless. Then the budget is restored and the frame must return to its
  reference within the same tolerance, because a page dirtied under starvation and never
  re-rasterized shows the old shadow while every counter reads clean.
  INTERACTION CHURN IS NOW COVERED, on the WOODLAND fixture. `vegetation-churn` captures a settled
  frame, hammers directional impulses across the cell without letting it recover, asserts the push
  REALLY moved the picture, then waits out the oscillator and requires the frame back within budget
  — a page dirtied by the trampled silhouette and never re-rasterized shows the old shadow while
  every counter reads clean.
  THE FIXTURE CHOICE IS THE WHOLE FIX. On the canonical cell — four metres across, sparse — a full
  lean moves the whole-frame metric by about 0.6 against a post-churn floor near 0.2, and three
  times separation is not enough to assert on. The woodland's taller trunks over a wider cell give
  0.97 against 0.21, which is four and a half times and holds with headroom at both ends.
  The first attempt and what it cost is kept below, because both of its findings still bite:
  THE FIELD REALLY DOES REACH THE PLANTS. `emit-interaction-impulse` moves a plant's recorded
  displacement to the full one-metre clamp, read back through `vegetation-wind-record`. The
  interaction path is not the problem.
  A DIRECTIONLESS IMPULSE PUSHES RADIALLY FROM ITS OWN CENTRE, so a ring of them AROUND the subject
  sums to nothing AT the subject. The first attempt trampled with a symmetric ring and measured
  exactly zero displacement — indistinguishable from an interaction path that does not work, and it
  cost most of the effort. Any future attempt must pass an explicit `direction`.
  THE METRIC IS THE REAL BLOCKER. A cooked cell is four metres across and its plants cover a small
  part of the frame, so a full lean moves the whole-frame mean absolute difference by about 0.6
  while the run-to-run floor after a churn sits near 0.2. Three times separation is not enough to
  assert on, where `vsm-churn` gets five times its budget from a plane-and-cube scene that fills the
  view. Closing this arm needs a screen REGION over the canopy rather than the whole frame, or a
  fixture whose vegetation fills it. Close framings were tried and render the cell while showing no
  interaction response at all, so the responding plants are not the ones they frame.
  ONE ENGINE OBSERVATION FELL OUT OF IT, and it is not what the docs say. Saturating impulses
  (strength 50, several per frame) leave the field ringing at about seven centimetres FIVE SECONDS
  later, against the roughly one second the interaction field is documented to spring back in. A
  gentler push (strength 12) settles to four millimetres in the same time. The displacement clamp
  rescales displacement without rescaling velocity, which is the mechanism to look at first.
  THE LAST ARM TO CLOSE WAS PHENOTYPE TRANSITIONS, and the road there is kept in full below
  because each dead end still bites. The closing state: the canopy suite's flip test proves the
  transition end to end — the senescent flip moves the image (~0.28 mean absolute difference,
  floor 0.1), the flip back reconverges to the reference within 0.05 against a measured ~0.007
  noise floor with the draw-record count restored EXACTLY, validation-clean — no stale
  silhouette, no invalidation storm. The history, starting from when the authoring half was
  solved and the fixture half was not:
  IT USED TO BE THAT NO FIXTURE COULD TRANSITION: `plant-create` scaffolds a family with exactly ONE
  phenotype, no control command added another, and the e2e vegetation fixture is a binary `.splant`
  blob generated by `xtask gen-vegetation-e2e-fixture` rather than JSON a test can edit.
  `plant-phenotypes {plant, phenotypes?}` closes that: it reads a family's authored appearances and
  replaces the whole list as ONE semantic operation. Replacing whole rather than patching is what
  lets the family validator judge a set that has to hold together — a phenotype must name a declared
  variation, two on one variation may not share a role, a remap must move between real slots, and a
  family needs a healthy appearance — instead of a field that looks fine alone. The validator is the
  authority; the command adds no second rule to keep in sync.
  Covered by `plant_phenotypes_reads_replaces_and_refuses_an_invalid_set`, which asserts the read,
  the replace, that it PERSISTED rather than being echoed, and that a refused set leaves the stored
  one unchanged — the last being the half that separates a transaction from a validation message.
  THE FLIP ITSELF WORKS AND WAS MEASURED: `vegetation-mutate` with `state-override` applies a
  phenotype to every resident plant and is accepted. What does not work is SEEING it. On the seasonal
  woodland family the transition moves the frame by 0.0097 — indistinguishable from nothing — and the
  reason is in the generator: the family carries ONE material slot and ONE part, and the senescent
  phenotype's variation 1 declares the same sources, an empty material remap and an empty active-part
  set. Variation 1 is variation 0. The fixture proves a seasonal phenotype EXISTS; it was never built
  to look different.
  SO THE REMAINING WORK IS FIXTURE CONTENT, not test code, and the cheap-looking options were checked
  and do not work. Giving variation 1 a SUBSET OF SOURCES does nothing: source 10 is the geometry and
  source 11 is the material, and the material reaches slot 0 through a family-level semantic target
  rather than through the variation, so dropping it from the variation changes no binding. Narrowing
  ACTIVE PARTS does nothing either: the family derives exactly one part, and an empty active-part set
  already means all of them, so there is no subset to take.
  THE SECOND-PART ROUTE WAS BUILT AND BACKED OUT, and it got far enough to be worth handing on. The
  generator grew a canopy box in `trunk_obj` behind the `seasonal` flag, a second `PlantPart` under
  it, a semantic target naming it, and `active_parts: vec![1]` on the senescent phenotype so the
  family sheds its canopy in autumn.
  SCOPING TO THE SEASONAL RECIPE WORKS, which was the main risk and is now measured: with the change
  gated on `recipe.seasonal`, regeneration left the other four fixtures byte-identical and exactly
  TWO tests failed, both woodland's. A first attempt that also changed part one's selector from
  `Whole` to an element broke every fixture — so leave part one alone; with one object in the file,
  `Whole` IS the trunk.
  IT ALSO VALIDATES AT THE ASSET LEVEL: `validate_plant_family` accepts the two-part family, and the
  generator wrote it. What fails is the COOK — `plant family 7320001 failed validation` — which means
  the compiler could not resolve the canopy's element selector.
  THE UNKNOWN IS NOW ANSWERED, AND IT IS NOT WHAT THE QUESTION ASSUMED. There is no element identity
  for a second `o` block because there is no second element: `import_obj_model` merges every object
  in the file into ONE `ImportedNode` carrying ONE mesh, named for the file stem. So the element id
  is `sub_id_for("e2e-birch", "mesh", "e2e-birch", 0)` and there is exactly one of them, whatever the
  file contains. Looking up "the id the importer assigned to the canopy" would have searched forever
  for something the importer never mints.
  WHAT SURVIVES THE MERGE IS THE MATERIAL RUN. Faces are grouped into submeshes in first-seen
  `usemtl` order, so a second object addressed as `PlantSourceSelector::Submesh { element, index: 1 }`
  is the mechanism that works — and it needs the OBJ to carry a real `.mtl` with two materials,
  because with no material file every face normalizes to slot `-1` and collapses into one submesh.
  THREE MORE COOK RULES CAME OUT OF IT, each of which fails the whole family rather than its own
  binding: every imported material needs exactly ONE material-slot target (a second material run
  with nowhere to land is an error); a family's `material_slots` must be pairwise DISTINCT; and two
  sources reading one file must agree on that file's content hash.
  AND ONE REAL ENGINE DEFECT, which is why the route looked impossible. Adding a `Leaf` part turns
  on `geometry_first_semantic`, so `apply_geometry_first_contours` rewrites the GEOMETRY source's
  snapshot — and the snapshot was hashed AFTER that rewrite. The material source reading the same
  file was not rewritten, so the two disagreed and the cook rejected one `SourceFile` address
  carrying two contents. The hash is now taken BEFORE the derivation: it identifies what the source
  holds, and the derivation is a pure function of the family asset and the file, both already in the
  key. `insert_dependency` also names the offending address now, instead of reporting that some
  unspecified dependency conflicted.
  Covered by `obj_submesh_selectors_bind_each_material_run_to_its_own_part`, which cooks a
  two-object two-material OBJ into two parts and asserts both material runs survived as submeshes of
  the one merged element — the premise the canopy selector rests on.
  THE ENGINE HALF IS LANDED AND GREEN. The hash fix, the named dependency address, and the test
  above are in the tree; 551 `saffron-assets` + `saffron-vegetation` tests pass with them.
  THE FIXTURE HALF WAS BUILT AND BISECTED, AND THE BLOCKER IS NOT FIXTURE CONTENT AT ALL. The
  generator grew the canopy behind `recipe.seasonal`, and everything upstream of the renderer
  worked: the other four fixtures regenerated byte-identical, the family validated, and the woodland
  family COOKED and published offline. What broke is that the plants stop responding to the
  interaction field — the trample metric collapses from 0.97 to 0.0078, deterministically, to the
  same digits on every run.
  THEY ARE STILL BEING DRAWN. `render-stats` under the failing fixture reports 6 instances, 35 draw
  calls, 54 shadow draws and 7160 triangles against 6 resident plants, so this is NOT a residency,
  culling, or visibility failure. The geometry reaches the GPU and holds still.
  FOUR CANDIDATES WERE ELIMINATED, one regenerate-and-run cycle each, and they are worth recording
  so nobody pays for them twice:
  AN UNRESOLVABLE MATERIAL IS NOT IT. Pointing the family's ONLY slot at a uuid with no catalog row
  still renders and still passes — the resolve path falls back to the built-in default exactly as
  documented, so a bare uuid in a slot is not what breaks a plant.
  THE WIDENED `local_bounds_max` IS NOT IT. Extending the seasonal family's volume to clear the
  crown changes nothing.
  THE CANOPY GEOMETRY IS NOT IT. A second box in the OBJ sharing the trunk's `usemtl` — one
  material run, one submesh, one part, a `Whole` selector — renders and passes.
  `active_parts` IS NOT IT. The full structure with an EMPTY senescent active-part set fails
  identically, so shedding the canopy is not what breaks it.
  WHAT REMAINS IS THE TWO-MATERIAL STRUCTURE ITSELF: a second `usemtl` run, which forces a second
  family material slot, which the cook accepts and publishes. Reduced to its minimum — two material
  runs, two slots, two material-slot targets, and still just ONE part on a `Whole` selector, no
  submesh selectors and no second part — the plants draw and stop moving. That is the repro.
  THE WIND SUBSYSTEM IS NOT THE CAUSE, and that is measured rather than argued. Applying the
  two-material structure to the CANONICAL fixture and running e2e `vegetation-mechanics` — the suite
  built to isolate exactly this — passes all ten: the authored response reaches the GPU, wind moves
  the plant and the prepass records it, a still field leaves the sway at rest, the prepass applies
  the response rather than merely carrying it, the height scale is the plant's own, the deformed
  counter counts it, and a cascade-edge camera jump marks it reactive. A multi-slot family deforms
  correctly. `GPU_SCENE_INSTANCE_FLAG_WIND` is also set unconditionally on every vegetation point,
  so the prepass never takes its zeroing branch.
  THE COOK DOES NOT MOVE THEM EITHER, which was the next guess and is now closed off by
  `a_second_material_run_does_not_move_the_cooked_geometry`: the same two-box OBJ cooked with its
  faces in one `usemtl` run and in two produces bit-identical vertex positions. The only differences
  the artifact carries are the ones you would expect — one submesh becomes two, one material slot
  becomes two, one material document becomes two — and no atlas in either case.
  SO THE PLANTS ARE MISSING FROM THE PICTURE AND NOTHING UPSTREAM EXPLAINS IT. Both frames were
  captured: the good fixture shows three tall trunks filling much of the view, the two-material
  fixture shows the same sky and ground with nothing on it. Geometry, placement, the wind path, and
  the cook's material output are all verified identical or correct.
  THEY ARE SUBMITTED, DRAWN AND DEFORMED — measured by diffing the counters between a one-material
  and a two-material run of the SAME scene, which is the comparison that settles it (reading one
  run's `render-stats` alone does not; an earlier reading of 6 instances / 7160 triangles was
  over-read that way, when six plants of two boxes are only 144 triangles and the rest of the scene
  supplies the remainder). Same scene, one material against two:

  | counter | one material | two materials |
  |---|---|---|
  | resident plants | 6 | 6 |
  | draw calls | 31 | 35 |
  | triangles | 1064 | 1112 |
  | visible / deformed instances | 5 / 6 | 5 / 6 |
  | **max cut depth** | **1** | **2** |
  | visited nodes | 10 | 18 |
  | resident pages | 4 | 6 |
  | materials | 3 | 4 |

  So the plants draw MORE geometry, not less, and the deform counter still counts every one. The
  real difference is the HIERARCHY: a second material makes the family cook a two-level cut where a
  single material cooks a flat one, and the cut descends — eighteen visited nodes against ten, six
  resident pages against four.
  AND PINNING THE CUT PROVES IT IS THE REFINED LEVELS. Driving `set-hierarchy-cut` over the same
  two-material scene, capturing a frame, trampling it and scoring the response:

  | cut | cut depth | triangles | trample response |
  |---|---|---|---|
  | auto | 2 | 1112 | 0.0254 |
  | **coarse** | **0** | **3200** | **1.7859** |
  | fine | 2 | 1112 | 0.0259 |

  Pinned COARSE the plants are back and respond harder than the one-material family ever did (1.79
  against 0.97). At depth 2 the family draws 1112 triangles where its own root carries 3200, and the
  response dies. `fine` matches `auto` exactly, so this is not the selector choosing badly — every
  refined level is wrong.
  SO THE DEFECT IS IN THE COOK'S HIERARCHY BUILDER FOR A MULTI-SUBMESH FAMILY: the refined nodes drop
  most of the family's geometry, and because a single-submesh family cooks a FLAT hierarchy (depth 1,
  no refinement to descend into) nothing in the tree ever exercised the path. That is why every
  vegetation suite stayed green through all of this — the fixtures are all one-material.
  THE EXTRA LEVEL IS `aggregate_node`, and `cook_portable_virtual_hierarchy` names it plainly:

      mesh_roots.push(if submesh_roots.len() == 1 {
          submesh_roots[0]                                   // one submesh: NO extra level
      } else {
          aggregate_node(&mut cooked, &submesh_roots, padding)?   // two: a voxel aggregate
      });

  A one-submesh family's mesh root IS its submesh root, so the depth-1 flat hierarchy the fixtures
  all cook never builds an aggregate at all. A second submesh introduces one, and it is a coarse
  voxel brick standing over the triangle subtrees.
  ONE REAL INCONSISTENCY LIVES THERE, and it is NOT the cause — this was built, measured and backed
  out, so nobody should re-derive it. `aggregate_node` takes `let error = brick.appearance_error`,
  its own brick's error alone, while the family root three hundred lines below folds in every
  child's (`mesh_roots.iter().try_fold(root_brick.appearance_error, ..max(child))`). An aggregate
  stands in for its whole subtree, so claiming only the brick's error understates what selecting it
  costs. Folding the children in compiles clean and keeps all 686 geometry/assets/vegetation tests
  green — AND DOES NOT RESTORE THE PICTURE. It is worth landing on its own merits, with e2e-wide
  validation, because it changes cut selection for every multi-submesh mesh in the engine; it was
  left out here rather than shipped unvalidated at the end of a session.
  THE BRICK'S CONTENTS ARE NOT IT EITHER — also built, measured and backed out. `aggregate_node`
  calls `build_coarse_root_brick(bounds, material)` with `occupancy = u16::MAX`, deriving the brick
  from BOUNDS ALONE, where `build_voxel_brick` beside it voxelizes real indices. Passing the mesh
  down and voxelizing it (`build_voxel_brick(brick_id, bounds, mesh, &mesh.indices, ..)`) compiles
  clean, keeps the workspace suites green, and STILL does not restore the picture — with or without
  the error fold above.
  THE COOKED TREES, WALKED, ARE THESE — and they are the clearest statement of the difference:

      one material    node 1  root      voxel  err 4294967295 (saturated)  children [0]
                      node 0            tri    err 0                        page 1
      two materials   node 3  root      voxel  err 4294967295 (saturated)  children [2]
                      node 2  AGGREGATE voxel  err 262143                   children [0, 1]
                      node 0, node 1    tri    err 0                        pages 2, 3

  Both roots saturate, so refinement is forced in both. With one submesh that lands directly on
  triangles. With two it lands on the aggregate, whose error is a FINITE 262143 — about 4.0 units in
  Q15.16, derived from its small union bounds — where its triangle children report exactly 0.
  THIS ALSO EXPLAINS WHY THE FIRST TWO FIXES DID NOTHING, which is worth stating so the reasoning is
  not repeated: folding children's errors into the aggregate cannot raise it when every child
  reports 0, and the brick's CONTENTS never enter the selector's decision at all.
  A THIRD FIX WAS TRIED AND ALSO BACKED OUT: making the aggregate's error saturate outright, the way
  the family root's does. The cooked tree then shows node 2 at 4294967295 — verified by walking it —
  the workspace suites stay green, and the picture is STILL unchanged.
  SO THE COOKED APPEARANCE ERROR IS NOT WHAT DECIDES THIS AT RUNTIME. That is the assumption all
  three fixes shared and it is now falsified: an aggregate that saturates is still not refined past.
  AND `scene_traversal.slang` SAYS WHY. Refinement is gated on three things, not one:

      bool refine = node.childCount != 0u && wantRefine
          && depth + node.childCount <= SCENE_TRAVERSAL_STACK_DEPTH;
      if (refine) for each child:
          if (!pageResident(addresses, childHandle, childRecord)) {
              gpuSceneRequestPage(addresses, childHandle.index);
              refine = false;                       // draw THIS node this frame
          }

  The error only feeds `wantRefine`. THE MEASUREMENT ALREADY RULES THAT TERM OUT: `cut=fine` sets
  `wantRefine = true` unconditionally (`SCENE_CUT_FORCE_FINE`) and rendered 1112 triangles with a
  0.0259 response — identical to `auto`. If the error were the blocker, forcing fine would have
  fixed it. It did not, which is also why saturating the cooked error could not.
  RESIDENCY IS NOT OBVIOUSLY IT EITHER, and the counters that say so were already captured in the
  one-vs-two comparison above: the two-material run reports `pageResidency` of registered 46,
  resident 6, **requested 0, loading 0**, faults 6 — against 44/4/0/0/4 for one material. Nothing is
  pending, and the two extra pages the deeper tree needs did resolve. `maxCutDepth` also reads 2,
  and the cooked tree puts the TRIANGLE children at depth 2 — consistent with the traversal reaching
  them rather than stopping at the aggregate on page 1.
  AND THE FRAMES WERE CAPTURED, WHICH CORRECTS THE `coarse` READING ABOVE. Under `cut=coarse` the
  two-material scene draws three FAT SOLID BOXES — the voxel proxies — where the one-material scene
  at its own cut draws three NARROW TALL TRUNKS. So coarse was never "the plants coming back": the
  1.79 response is a large proxy blob swinging, not the family rendering correctly. Any reading of
  that number as a restored picture (including an earlier one in this note) is wrong.
  THE REAL SYMPTOM, STATED CLEANLY: at `auto` and at `fine` the two-material family reaches the
  TRIANGLE level — `maxCutDepth` 2, and the cooked tree puts its triangle nodes at depth 2 — draws
  1112 triangles, and shows NOTHING on screen. The one-material family reaches its triangle node at
  depth 1, draws 1064, and shows trunks. Two comparable triangle counts, one visible and one not.
  AND THAT LED TO A REAL BUG, NOW FIXED — in the OBJ IMPORTER, not in vegetation at all. Cooking a
  two-TRIANGLE OBJ (six distinct positions: one triangle, and a second above it) produced:

      verts=3  indices=6  submeshes=[(0, 3, 0, slot 0), (3, 3, 0, slot 1)]

  SIX INDICES INTO A THREE-VERTEX BUFFER — index count and submesh split right, vertex array
  truncated to the first triangle's worth. `import_obj_model` returned exactly the same, so the
  plant cook was faithful and the fault was upstream: tobj re-indexes each object and each `usemtl`
  run against its OWN arrays, and the importer shared one dedup `BTreeMap<[i32; 3], u32>` across
  every model. Two models both start at the triple `(0, 0, 0)` meaning different vertices, so every
  object after the first folded onto the first one's geometry.
  THE FIX IS THE MAP PER MODEL, and it is landed with
  `each_object_in_a_multi_object_obj_keeps_its_own_vertices` pinning it. Determinism is unaffected
  (the ordered map is what the module exists for). Validated across the geometry, assets and
  vegetation crate suites and the `vegetation` e2e files.
  THE BLAST RADIUS IS WIDER THAN THIS BOX: every multi-object OBJ the engine has ever imported was
  silently collapsing its later objects onto the first — correct index counts, correct submeshes,
  and geometry drawn on top of itself. Only single-object OBJs were ever right, which is why nothing
  in the tree caught it (`cube.obj` is one object, and every vegetation fixture is one box).
  IT IS NOT THE WHOLE OF 11:352, but the FIXTURE SIDE IS NOW COMPLETE AND KNOWN-GOOD. With the
  importer fixed the canopy family stopped cooking, and the offline diagnostics named why in one
  run: `BoundsMismatch` at `dimensions.localBounds` — "normalized geometry lies outside the authored
  conservative bounds", because the crown rises above `local_bounds_max[1] = height`. Widening the
  seasonal recipe's bounds to `height + 2` publishes it. So the complete recipe is: the canopy box
  in `trunk_obj` behind `seasonal`, a `.mtl` with `material_0`/`material_1`, the material source's
  selector as `Whole`, a second material-slot target, two distinct `material_slots`, the second
  `PlantPart` on submesh 1, `active_parts: vec![1]` on the senescent phenotype, AND the widened
  bounds. That cooks and publishes.
  WHAT REMAINS IS PURELY THE RENDER, and it was confirmed with BOTH fixes in place: the importer
  correct, the bounds widened, the family cooking and publishing — and the captured frame is still
  sky and ground with no plants on it. So a multi-object plant family does not render, full stop;
  nothing about the authoring, the cook, or the two defects fixed this session explains it.
  Everything already ruled out is above — placement, residency, refinement, the cooked error, the
  wind path, three hierarchy fixes built and backed out — and note the `coarse` baseline was a
  misreading: it draws voxel proxies, not trunks.
  THE DISCRIMINATOR IS CONCLUSIVE: ORDINARY MULTI-OBJECT OBJ IMPORT AND RENDER IS FINE. Built on a
  scene construction copied from `material-render` (the scratch project's Sun, IBL on,
  `instantiate-model`), framed so ONE box substantially fills the view, and with the premise
  asserted before the conclusion:

      one-object vs empty frame   7.6992      (the subject really is on screen)
      one-object vs two-object    3.1084      (the second object really does render)

  A second box stacked above the first contributes about 40% of what the first contributes, which
  is what a partly-shared screen area and a different lighting angle give. So the second object is
  NOT lost outside vegetation.
  AND THE COOKED ARTIFACT IS VERIFIABLY PERFECT, so the loss is purely at RUNTIME. Walking the
  cooked triangle clusters of a two-submesh family against a one-submesh cook of the same geometry:

      one material    cluster 0  submesh 0  slot 0  srcVerts [0,1,2,3,4,5]  y [0,0,1,1,1,2]
      two materials   cluster 0  submesh 0  slot 0  srcVerts [0,1,2]        y [0,0,1]
                      cluster 1  submesh 1  slot 1  srcVerts [3,4,5]        y [1,1,2]

  Both clusters carry correct, DISTINCT source vertices at the right heights, with the right submesh
  and the right material slot. Nothing about the artifact is wrong.
  THEREFORE THE LOSS IS VEGETATION-SPECIFIC AND PURELY AT RUNTIME, and everything the box originally
  scoped was in the right subsystem after all.
  THE MIRROR'S MATERIAL PATH READS CORRECT TOO, recorded so it is not re-read: the vegetation
  instance builder takes `slot_count` from `shared.meshes[&family].slot_count`, which `insert_mesh`
  derives as `max(submesh.material_slot + 1)` — the SAME derivation the ordinary mesh path uses, and
  2 for a two-submesh family. The per-slot override loop then walks `0..slot_count` and pushes one
  override per non-zero `render.materials[slot]`, which for this family is two.
  THE RUNTIME HIERARCHY MATCHES THE COOK — checked, and the apparent mismatch was a REPORTING BUG,
  now fixed. `plant-hierarchy` reported the two triangle children of an aggregate at depth 1, the
  same depth as their own parent. The cause was its depth pass: a single sweep in array order,
  reading each node's parent depth before that parent had one. The cooker emits LEAVES FIRST and
  roots last, so a child is nearly always visited before its parent, and every node came out one
  level below its root — a two-level family read as flat. The pass now walks DOWN from
  `hierarchy.roots` through `children`, which is order-independent and still O(n). Measured on a
  two-level family: [1,1,1,0] before, [2,2,1,0] after, matching the cooked parents (0->2, 1->2,
  2->3, 3->root) exactly.
  SO THE RUNTIME TREE IS THE COOKED TREE, and that layer is cleared too: the cook, the clusters, the
  traversal gate, the mirror's material derivation, and now the runtime hierarchy have each been
  cleared in turn. Combined with the rest of this note that is a tight remaining target: a
  multi-object family cooks and publishes correctly, its geometry positions are bit-identical, its
  wind path is correct, its pages are resident, its cut refines — and only through the VEGETATION
  render path does the second object vanish. Compare what a two-submesh family produces at
  `plant_render`/`gpu_scene_mirror` against what the same mesh produces through the ordinary mesh
  path, which the probe above shows is correct.
  RETESTED 2026-07-28 AFTER THE UNINITIALIZED-TABLE FIX (the box below): the loss still
  reproduces, so that defect was not this one. NEW THIS ROUND, all landed:
  THE FIXTURE SIDE IS IN THE TREE — `vegetation-canopy.json`, a `multi_object` recipe in the
  fixture generator (canopy box as a second `usemtl` run + an `.mtl` sidecar the fixture installs;
  two parts, two slots, senescent `active_parts: [1]`, bounds `height + 2`), with a unit probe
  (`seasonal_fixture_family_publishes_against_the_current_compiler`) proving it cooks: 1
  prototype, 2 uses, combinations (0,0) mask 0b11 / (1,1) mask 0b01, identity use transforms.
  The woodland stays single-object so the churn/stress/parity suites stay green while the defect
  stands.
  THE REPRO, deterministic and visual: cook the canopy map, query the accepted plant (10 m
  bounds), frame it (this map is 64 m cells: 4096 ticks per metre), capture — sky and ground,
  no plant, in EVERY configuration tried: fine cut, forced coarse (voxelRecords stays 0),
  senescent override (one active use), and `SAFFRON_NODE_CULL=off`. Meanwhile
  `vegetation-render-stats` reports the 6 instances resident and `gpu-scene-stats` shows their
  nodes visited and records emitted for other content — so the loss is inside record emission
  or the executor draw for use-carrying nodes, not in placement, residency, or the cull.
  FOUR MORE LAYERS CLEARED THIS ROUND, each verified: the cooked combination masks (correct,
  above); the cooked use transforms (exact identity); the mirror's combination resolution
  (`(variation, rendered phenotype)` → index 0) and its journey into the GPU instance record
  (`gpu_scene_upload.rs` writes it into the record's reserved word the traversal reads); and
  the executor's vertex addressing (1 prototype → `vertexBase` 0, cluster indices
  geometry-relative, so the double-offset hypothesis is dead for this family).
  A SECOND ROUND OF SHADER-LEVEL BISECTION NARROWED IT FURTHER, and overturned one premise.
  **THE FORK PATH HAS NEVER BEEN EXERCISED BY ANY GREEN TEST**: `assembly_from_hierarchy`
  returns `None` for a single-prototype single-identity-use hierarchy, so the phase-3 family —
  and every other fixture — draws as a PLAIN mesh. The canopy family is the first content ever
  to reach `GpuAssemblyUseRecord` on the GPU. The loss is a virgin code path, not a regression.
  WHAT THE BISECTION ESTABLISHED, one temporary shader hack per fact, all reverted:
  records emit (10 voxel records under forced coarse — the earlier "voxelRecords 0" reading was
  taken in auto cut and was wrong); draws are right-sized (384 raster triangles, sane for 6
  plants); no bucket misses (pressureFlags 0). With wind on, the plant rasterizes as a
  SPECKLE of scattered fragments; with wind zeroed it becomes a STABLE GIANT WALL filling most
  of the frame — garbage vertex positions either way, wind only adds per-frame scatter.
  Skipping fork 1 (use 0 only) keeps the wall; skipping every fork removes it — so ONE
  use-carrying record already draws the wall. Re-pointing part 2 at submesh 0 (same geometry
  and material for both uses) keeps the wall — not a submesh-1/material-1 defect. Neutralizing
  the ENTIRE assembly branch in the color pass's vertex fetch (plain
  `vertices.first + vertexIndex * stride`, no use transform, vertexBase 0) STILL walls — so
  the corruption is NOT the use-record read or the use transform: the pulled INDEX VALUES (or
  the record's geometry/page identity) are wrong for use-carrying records.
  LAYOUTS VERIFIED EQUAL BY READING, so they are not it: the parts-arena packing
  (header 16 + prototypes 16 each + uses 64 each + masks) against
  `gpuSceneAssemblyUse`/`gpuSceneAssemblyPrototype`; the page payload (96-byte node record +
  child handles + pad-16 + clusters 48 each + voxel vertices 16 each + index blob) against
  `gpuScenePagePayloadBase` and the scatter's `firstIndex` composition; the parts arena is
  byte-strided so `geometry.parts.first` is a byte offset as the shaders assume.
  **ROOT-CAUSED AND FIXED, SAME DAY.** A counter-word readback of one voxel record named the
  wall: the family's own aggregate voxel brick (1536 vertices / 2304 indices — the exact
  cooked brick) drawn every frame under a representation crossfade whose `transition` word
  never cleared. The defect: `transitionKey(slot, page)` keyed the flip state table by
  (instance slot, page) ONLY, while an assembly visits the same node once PER USE — the two
  visits read each other's mode as a cut flip and restarted the crossfade at phase 1 forever,
  so the aggregate box drew over (and around) the fine geometry every frame. Wind scattered
  the box's vertices into the speckle. THE FIX is one line of key material: fold the use into
  the hash (`(use + 1u) * 0xc2b2ae35u`; `GPU_ASSEMBLY_NO_USE + 1` is zero, so a plain mesh's
  key is bit-identical to before). Verified visually — the canopy family renders its trunk
  AND its crown with distinct slot materials — and structurally: at the fine cut,
  `voxelRecords` settles to 0 where it was pinned at 4. The earlier "framed capture is blank"
  readings were the box occluding plus a camera facing +Z; the multi-angle recapture shows
  the tree.
  LANDED WITH IT: `tests/e2e/vegetation-canopy.test.ts` — the only end-to-end coverage of the
  GPU assembly-use path (fork, per-use records, per-use crossfades), asserting the family
  draws and its crossfades settle, validation-clean.
  THE MUTATION→MIRROR INVALIDATION GAP FOUND WHILE PROVING THE MASKS IS ALSO FIXED:
  `apply_confirmed_mutations` now bumps every changed cell's bulk revision — the same signal
  promote/demote raise — so adapters caching beside the generation id re-derive the cell.
  Unit-pinned (`a_confirmed_mutation_bumps_the_cell_bulk_revision`) and proven end to end
  structurally: on the fine cut a senescent flip drops the crown uses' draw records
  (33 → 25 in the repro scene) and a flip back restores the exact count — the second canopy
  test asserts both, validation-clean.
  THE PIXELS-VS-RECORDS DISAGREEMENT IS EXPLAINED AND FIXED, and it was neither of the
  suspects (screenshot frame identity, second draw path). The masked records were REDUNDANT:
  `add_micro_instances` pushed one identity use of the WHOLE source-mesh prototype per part
  referencing the source, so the canopy cooked to 1 prototype × 2 coincident uses — the
  healthy tree drew trunk+crown TWICE on top of itself, and the senescent mask removed only
  the duplicate copy. Records dropped (one full cluster set gone) while the picture could
  not change (the surviving trunk use still drew the crown's clusters; `emitNodeRecords`
  emits every cluster of the node under a use, with no part filter — correctly, because a
  use is a placement, not a subset). THE FIX IS AT THE COOK, where the meaning was wrong:
  Part-destination submesh semantic targets now PARTITION a geometry source — the compile
  splits it into one normalized row per submesh (`partition_selected_by_parts`, riding the
  existing `SelectedMesh { submesh }` extraction and the `Submesh` selector identity), each
  row cooks to its own prototype, and `add_micro_instances` binds each row's use to the part
  its exact target names. Several parts sharing an un-partitioned row is now a compile error
  (`several_parts_on_an_unpartitioned_source_are_refused`) instead of a silent double-draw,
  and the crown row now takes its own part's Leaf semantics for aggregation
  (`semantic_for_mesh`). Un-partitioned single-part sources cook byte-identical, so every
  other fixture is untouched; the two-prototype upload/traversal/RT path was already proven
  (`two_prototype_family`, per-prototype BLAS with mask-gated TLAS instances), so no runtime
  vocabulary changed. Pinned by `part_submesh_targets_partition_the_source_into_per_part_rows`
  and the canopy probe (prototypes=2, uses=2, one per part). THE VISUAL DELTA NOW SITS BESIDE
  THE STRUCTURAL ONE: the canopy flip test captures healthy/bare/restored frames — the flip
  moves the image by ~0.28 mean absolute difference (floor 0.1) where it moved ~0.006 before
  the fix, and the flip back returns within 0.05 (measured noise ~0.007) with the record
  count restored exactly.
  TWO MEASUREMENT LESSONS FROM GETTING HERE, both of which cost a round each. A hand-assembled
  entity (`create-entity` + a `Mesh` component) renders NOTHING — one-object vs empty was 0.3719 —
  so its "second object missing" reading was noise; build image probes on a scene already proven to
  render. And a badly framed subject makes every difference small: the same probe framed loosely
  gave 1.09/0.42, which discriminates nothing. Assert the one-object case against an empty frame,
  and require the subject to fill the view, before trusting any delta.

## NO-LEGACY gate

VSM is the raster shadow system when this phase lands. GI/RT consume canonical plant materials,
hierarchies, and deformation; no foliage-only shadow, solid-canopy approximation, or vendor content
fork remains.

