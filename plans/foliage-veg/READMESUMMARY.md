# Foliage/vegetation — state of play

The short version. Every design decision, root cause, and per-slice seal lives in
[`READMEFABLE.md`](READMEFABLE.md); this file is the map, not the territory. Per-box evidence lives on
the box itself in each `phase-N-*.md`.

## Phase status

| Phase | State | Boxes |
|---|---|---|
| 1 spatial/numeric | **complete** | 34 / 34 |
| 2 domain assets + mutations | **complete** | 25 / 25 |
| 3 graph determinism | **complete** | 31 / 31 |
| 4 cooker + cell artifacts | **complete** | 23 / 23 |
| 5 runtime cells + persistence | **complete** | 28 / 28 |
| 6 virtual geometry substrate | **complete** | 32 / 32 |
| 7 GPU scene + visibility cutover | **complete** | 26 / 26 |
| 8 vegetation rendering | **complete** | 26 / 26 |
| 9 editor authoring + debug | **complete** | 21 / 21 |
| 10 wind/deformation/phenology | **complete**, 6 carve-outs | 31 / 31 |
| 11 VSM / lighting / RT | **complete** | 24 / 24 |
| 12 interaction/physics/queries/nav | **complete** | 26 / 26 |
| 13 ecology + catch-up | **complete** | 28 / 28 |
| 14 botanical authoring + interchange | **complete**, 1 carve-out | 25 / 25 |
| 15 production/platform closure | **complete** | 20 / 20 |

**All fifteen phases complete; no boxes open.** Each carries its evidence on the box itself.

## What the NVIDIA machine unblocked

The work is now ordinary — hardware is no longer the reason any of it is open.

- **Ray tracing — 8 phase-11 boxes**, one of which is now part-built: AS **memory, compaction saving
  and selected representation** are reported through `render-stats` and covered by e2e
  `rt-telemetry` (4/4). Compaction reclaims **54.8%** on this device (254,848 built → 115,072 kept).
  That box stays open on its time, OMM-hit-class and page-demand clauses.
  The **opacity-micromap capability layer** is also built — `VK_EXT_opacity_micromap` is probed
  behind the RT gate, extension *and* `micromap` feature enabled, dispatch resolved, reported as
  `ommSupported` (measured **true** here, validation-clean). We target EXT because this driver
  advertises only EXT rev 2 and `ash` is pinned `=0.38`; `VK_KHR_opacity_micromap` *does* exist
  (spec-dated 2026-05-08) and an earlier note here wrongly denied it. Derivation stays open, and its
  blocker is plumbing, not data: coverage texels are retained (`texture_pixels_by_uuid`) and the
  cooker already reads them per texel. `Device::new` resolves `rt_supported = true` and acceleration
  structures build and trace validation-clean. Shared compacted BLAS, deformation materialization,
  voxel clusters as AABBs, the KHR any-hit baseline, OMM derivation
  (built against **`VK_EXT_opacity_micromap`** — the KHR extension exists but this driver and our
  pinned `ash` expose only EXT),
  `VK_NV_cluster_acceleration_structure`, BLAS/TLAS tracking, and the any-hit/OMM parity acceptance
  all need building. Also the BLAS half of phase-15's GPU-telemetry box —
  `render-stats` reports `blasCount: 0` while `rtInstances: 2`, so that counter is not wired.
- **Mesh shaders — 2 phase-7 boxes.** `VK_EXT_mesh_shader` is advertised and the design is now
  settled with the numbers checked: a portable cluster is 64 verts / 124 tris, the RTX reports
  256/256/1024 and llvmpipe 256/256/128, so **one cluster fits one mesh workgroup on both tiers**.
  The binner already emits one command per cluster carrying the record index in `firstInstance`, so
  the mesh path reads the *same* command stream as data — the cut is shared, not re-derived. The scatter
  writes the per-command `VkDrawMeshTasksIndirectCommandEXT` itself (`groupCountX =
  ceil(indexCount/3/62)`), so the mesh draw mirrors the indexed one against the same count word and
  a workgroup recovers its place from `DrawIndex` + `SV_GroupID.x` — no prefix sum, and all three
  representations (cluster, micro-blade, aggregate voxel) are covered by construction rather than by
  a bound someone must remember to raise. The plumbing is now BUILT and gated — mesh-args stream, scatter
  writes for all three representations, `scene_executor_depth_mesh.slang`, the mesh-stage PSO, and
  the draw recorder — with `just e2e` **341/341** proving the live-path scatter change disturbed
  nothing. **It now executes**: `executor_draws_the_binned_cut_depth_only` runs both
  executors over one binned cut and compares depth — mesh **144 texels against indexed 144, exact**,
  validation-clean, skipped on devices without the extension. Rendering it caught a bug compiling it
  could not: the mesh shader read `addresses.indices` and rasterized nothing, because `firstIndex`
  offsets the **pages** arena. Still open on scope — depth-only, no mesh counterpart for the shaded
  übershader pass and no runtime selection yet.
- **A second platform — phase-15 image comparison** (NVIDIA + MoltenVK is two), and **the NVIDIA
  validation box** itself.
- **Visual/timing captures** in phases 10 and 11. Phase 10's is now built and its box closed:
  `tests/e2e/vegetation-wind-visual.test.ts` measures motion over time (still **0.0001** vs gale
  **0.408**, a ~3,500x ratio) rather than calm-versus-gale, so the still pair is its own control.

## AMD, descoped

The project owner descoped AMD on 2026-07-26: no such adapter exists for this project and none can be
obtained, so a box demanding it stated an unmeetable requirement rather than a gap. The four boxes
that named it are now closed against the platforms that exist — **NVIDIA** (`RTX 3070 Ti`,
validated), **Apple/MoltenVK** (validated), and **Mesa llvmpipe** as the software tier (validated).

**No AMD verification was performed and none is claimed.** Each box says so on its own line, and
phase-15's AMD-specific box is struck through as out-of-scope rather than done. If an AMD adapter
ever appears, reopen them rather than trust them.

## Buildable anywhere (no hardware gate)

- **Phase 14 (5):** Plant-workspace render surfaces (3D preview, wind preview, lifecycle timeline,
  materials/atlas, collision/nav, hierarchy/voxel/error); bounded cancellable preview;
  presets/subgraphs via `.splant` internal modules (**no new asset format**); the generator outputs
  (atlases, coverage-preserving textures, aggregate-voxel appearance error); USD skeleton/plant
  metadata.
- **Phase 15:** distributed work-item manifests; the non-BLAS GPU telemetry (bins, overdraw,
  deformation counts); Perfetto/capture integration.
- **Phase 11 (2):** aggregate-voxel injection/sampling parity — **the wiring is now done**:
  `parity_occupancy` had no production caller, so injection used a separately authored occupancy;
  `derive_parity_occupancy` now solves it from the sheet's own transmission and thickness, proven by
  a round-trip test to 1e-3. **The box is now closed**: `SAFFRON_CUT_OVERRIDE` pins the hierarchy cut so
  two hosts differ only in representation, and the two cuts render **8.97** apart while their mean
  brightness differs by **2.93** — different pictures, same light. Remaining here: the GI/reflection
  culling parameterization audit.
- **Phase 10:** deform assembly parts without expanding authored structure; the event-emission box;
  the wind debug overlays.

## The player frame-1 hang: FIXED

It was never a GPU hang. `begin_offscreen_frame` waits the slot's in-flight fence and then **resets**
it; a frame that a layer begins and never submits leaves that fence reset-but-unsignalled, so the
next frame's wait can never return. The watchdog wraps that wait, so it reported
`GPU submission 'frame 1' has been in flight` — which is why it read as a GPU hang, and why the
recorded cause ("a MoltenVK windowed-present hang") was wrong on all three counts: not MoltenVK, not
windowed, not present.

The player hits it because a missing `project.json` makes `on_attach` return early, leaving
`started == false`, so both per-frame hooks return on their first line and nothing ever submits.

`Renderer::finish_unsubmitted_frame` now closes such a frame with an empty fence-signalling submit,
called from the loop's `end_frame` — the invariant no longer depends on what a layer chose to draw.
Verified: the plan's own reproducer exits 0 windowed and offscreen, measured as the process exit code.

## The player teardown segfault: FIXED

A second, pre-existing defect sat behind the hang: the player rendered its frames and then
**segfaulted during teardown** (exit 139), with `VkDevice has not been destroyed` at
`vkDestroyInstance` plus leaked buffers, images and memory.

It first looked specific to the `export-app` package, because the bare binary exited 0. It is not —
**the trigger is a loaded project.** Pointing the dev binary at a project (`SAFFRON_PROJECT=…`)
reproduces it exactly; the packaged layout, the staged `libc++.so.1`, and the capture seam were all
ruled out by measurement. `PlayerLayer::on_detach` released the uploader and the asset caches but
never the GPU-scene mirror, which retains `Arc<GpuMesh>`/`Arc<GpuTexture>` clones for its mirrored
prototypes and interned textures. With a project loaded those handles kept the device alive past its
own destruction and the NVIDIA driver faulted inside `vkDestroyInstance`; with no project the mirror
holds nothing, which is the whole reason the bare binary looked clean.

`on_detach` now resets the mirror, matching the host's `teardown_recording` — whose comment already
named this exact hazard, one crate away.

## Editor, host and player render the same picture

`tests/e2e/player-parity.test.ts` exports a scene, runs the packaged `saffron-player`, and compares
its frame to the host's: **mean absolute per-channel difference 0.00018** — 121 differing bytes in
691,200, one-step rounding along the cube silhouette.

The comparison must be against the host **in play mode**, and that detail is load-bearing. The player
renders the scene's primary camera; the host in edit mode renders the *editor* camera, so an
edit-mode comparison measures the distance between two camera poses and says nothing about render
semantics. It scores 11.7, and the test asserts that control exceeds the budget so the substitution
cannot be made silently later.

One trap worth recording: `export-app` copies `project.json` **off disk**, so a test that builds its
scene over the control plane must `save-project` before exporting. Without it the player boots the
starter scene and the comparison is between two pictures of nothing — which is exactly what a first
run of this test produced, at a plausible-looking 4.39.

## Verifying on the new machine

The `just` recipes auto-enter the `saffron-build` toolbox and set the NVIDIA ICD via the `gpu_driver`
macro. Do **not** hand-roll the driver path; a wrong one silently drops to llvmpipe.

```sh
just engine && just prepare-for-commit    # build + fmt + clippy -D warnings
just schema                               # 249 manifest-driven control checks
just test                                 # cargo test --workspace
just e2e                                  # 350 tests / 60 files
just check                                # the whole reproducible gate
```

Both harnesses boot the host offscreen on every platform, so no compositor is involved and device
selection takes the discrete GPU; `vulkaninfo --summary` should name the RTX rather than `Apple M4`.
If a `.splant` schema identity changes, regenerate the e2e fixture with
`cargo run -p xtask -- gen-vegetation-e2e-fixture`.

Last full verification on this Mac (2026-07-26): every command above green except the platform arms,
with `just e2e` at 328/328 — the suite has since grown to 341 on the NVIDIA machine.

## First gate on the NVIDIA machine (2026-07-26)

`vulkaninfo --summary` names `NVIDIA GeForce RTX 3070 Ti` (discrete, driver 610.43.03, api 1.4.341)
as GPU0 with llvmpipe as GPU1. The device advertises `VK_KHR_acceleration_structure`,
`VK_KHR_ray_query`, `VK_KHR_deferred_host_operations`, `VK_EXT_mesh_shader`,
`VK_EXT_opacity_micromap` (`micromap = true`, subdivision level 12), and
`VK_NV_cluster_acceleration_structure`. Ray tracing and mesh shaders are both reachable here for the
first time.

| Gate step | Result |
|---|---|
| `just engine` | EXIT=0 |
| `just prepare-for-commit` | EXIT=0 |
| `just schema` | EXIT=0 — all 249 manifest-driven checks |
| `just test` | EXIT=0 |
| `just e2e` | EXIT=0 — **350 / 350** across 60 files |

Five defects the move surfaced, all fixed: the AS build scratch ignored
`minAccelerationStructureScratchOffsetAlignment` (128 here) and lost the device on every build;
first-use images seeded a `FRAGMENT_SHADER` source stage that is illegal to name from a compute-only
queue; `RgPass::compute` routed *every* compute pass to the async-compute queue, which no device the
project had ever run on exposed; a clear-colour readback asserted an exact byte where `0.5` quantizes
to a spec-permitted tie (127 here, 128 on llvmpipe/MoltenVK); and the validation-gate meta-test slept
a fixed 600 ms, too short for a cold boot here, so the probe that proves validation works was itself
red.

**The GPU hang is fixed.** It was never the build command: creating an acceleration structure and
then submitting anything was enough to wedge the device on a later unrelated frame, while either half
alone was harmless. VMA suballocates a small AS-storage buffer into a memory block shared with
ordinary buffers; acceleration-structure storage now takes a **dedicated** allocation. (A 64 KiB AS
passed only because it cleared VMA's dedicated-allocation threshold — that accident is what made the
size look relevant.) `just schema` went from a deterministic hang to 249/249.

The **ray-query shadow path is now reachable**: `point_shadow_meta.z` is the gate the mesh fragment
tests and `lighting.rs` hard-coded it to `0` with a comment claiming the RT phase folded it in.
Nothing did. It is now fed from `Rt::shadows_enabled`, and a cube + directional light with
`set-rt-shadows true` reports `rtShadows: true` / `rtInstances: 2` and renders validation-clean.

**The whole gate is green on this machine**, `just e2e` included. Four e2e failures were fixed this
session, none of them a vegetation defect: `set-viewport-size` was a third GPU hang (the viewport shm
capture ring freed an image and staging buffer under in-flight work during frame recording);
`material-preview-render` was the Linux harness still booting windowed; `vsm` sampled `allocated` and
`rendered` from one frame although they are per-frame counters that peak on different frames — and a
sibling test asserts `allocated` is 0 once warm, so the two contradicted each other; and
`vegetation-export` looked for the payload under `<root>/resources`, which is the macOS bundle layout
— every other platform stages it at the export root itself.

**The player hang is not MoltenVK-specific.** `SAFFRON_EXIT_AFTER_FRAMES=5 ./engine/target/debug/saffron-player`
reproduces the frame-1 hang here (`GPU submission 'frame 1' has been in flight 119s`), so the recorded
cause — the MoltenVK windowed present path — is wrong. Both the hang and the teardown segfault behind
it are fixed (see the two sections above), and the player-smoke arm of the two phase-15 boxes is
closed.

**A platform-parity trap worth keeping:** the Rust e2e harness booted the host *windowed* under a
headless weston on Linux. A headless compositor denies present support to the discrete adapter, so
`select_physical_device` rejected the RTX and the suite ran on llvmpipe — an "NVIDIA validation" claim
made that way would have been false. Both harnesses now boot offscreen on every platform, which needs
no compositor and selects the discrete GPU.

## Where the planset stands (2026-07-27)

Twelve of fifteen phases are `COMPLETED`. Three carry the remaining **20 open boxes**:

| Phase | Open | What is left |
|---|---|---|
| 11 — shadows/lighting/RT | 9 | GI/reflection consumers still bypass the hierarchy; OMM attachment; NV cluster-AS; the acceptance boxes that depend on those |
| 14 — botanical authoring | 3 | the plant workspace's render surfaces; a cook stage calling the four generators; USD skeletons as recipe *inputs* |
| 15 — production closure | 6 | distributed manifests; Perfetto/alarms; overdraw + BLAS build time; the terminal COMPLETED box |
| 10 — wind (COMPLETED, carve-outs) | 2 | interaction-field scroll invalidation; a measurable distant-motion test |

**The single largest blocker is one piece of work, not several.** Phase 11's GI/reflection culling
box gates three of its own acceptance boxes: the world-space consumers (`sdf_instances`,
`rt_instances`) are CPU-gathered by a full-ECS scan and do not participate in the hierarchy at all.
Until they consume the traversal cut, "compatible cuts across consumers" and "page demand
attribution" cannot be assessed, let alone closed.

**A second pattern worth naming:** four of the remaining boxes are blocked on *wiring* rather than
algorithms. The atlas packer, the coverage-preserving family-texture generator, the measured
aggregate-voxel calibrator and the OMM alpha-plane all exist and are tested; no cook stage calls any
of them. That is a smaller, better-defined job than it looks from the box text.

**That pattern turned out to be the dominant one.** A re-audit of the whole record against the tree
(slice 1) found four notes claiming absent what is in fact built: the opacity-micromap producer and
its `VkAccelerationStructureTrianglesOpacityMicromapEXT` chain, per-pass overdraw, GPU timestamp
coverage for the BLAS build, and the plant 3D preview. In each case the remaining work is wiring at
named seams rather than the subsystem the note described. Two further claims in the audit brief —
that `CookDependencyAddress::SourceAsset` is missing and that the cook has no cache-sharing lock —
were wrong in the brief and right in the plan files, and were left alone. The corrected count is
four, not six.

**Opacity micromaps are the clearest case.** The derivation exists and is mutation-checked, the
Vulkan chain is built and pushed, and `MeshBlasGeometry` carries both an `opaque` field and a
`micromap` slot. Missing: any production caller for the derivation, `micromap: None` at both upload
sites, no cook stage emitting into the `RayTracing` section, and one geometry per BLAS so opacity
cannot vary per submesh. The last of those has to land first and alone — deleting the per-instance
opacity override is what `tests/e2e/rt-anyhit.test.ts` guards.

**One acceptance box now has a counterexample rather than silence:** triangle↔aggregate transitions
do *not* always stay within the declared error. A comb of thin blades cooks to a hierarchy whose
measured error exceeds its declared one, because the analytic estimate cannot see that a brick fills
the gaps. `calibrate_voxel_appearance_error` fixes it and is idempotent; nothing calls it yet. The
failing case is thin features, and vegetation is entirely thin features.

**One defect is open and not attributable to this work:** `just schema` intermittently wedges the
NVIDIA GPU with `ERROR_DEVICE_LOST` in the thumbnail render path. Ruled out: the node cull
(`SAFFRON_NODE_CULL=off` hangs too), the code (llvmpipe green at 251/251, and the identical binary
passes on NVIDIA between failures), and the engine broadly (`just e2e` 370/370 on NVIDIA).

Gate at the time of writing: `just engine`, `just prepare-for-commit`, `just test` EXIT=0;
`just schema` 251/251; `just e2e` **370/370 across 64 files**; docs three-check clean.

## Where the planset stands (2026-07-29) — closed

The planset is finished. The last pass closed the remaining boxes:

- **11:352's final mystery is explained and fixed.** The senescent flip dropped draw records while
  the pixels never changed because a multi-part source cooked ONE prototype placed by coincident
  whole-mesh uses — the healthy tree drew itself twice and the mask removed the duplicate. The cook
  now partitions a source by its Part-destination submesh semantic targets into per-part prototypes
  (several parts on an un-partitioned row is a compile error), and the canopy flip test asserts the
  pixels beside the records: ~0.28 mean absolute difference on flip against a ~0.007 noise floor,
  exact record restoration on flip-back. That closed the churn box (11:663) — every arm covered.
- **The cluster half of the NV box is built.** `VK_NV_cluster_acceleration_structure` executes over
  the canonical cooked clusters through hand-transcribed bindings (no ash release ships them; a
  pinned-version test forces their deletion on any ash bump), proven live on the RTX 3070 Ti,
  validation-clean, with `clusterAsSupported`/`clusterBlasCount`/`clasCount` on `render-stats` and
  e2e coverage in `rt-telemetry` + the canopy suite.
- **The partitioned top level closed the last box.** `VK_NV_partitioned_acceleration_structure`
  replaces the whole-table TLAS rebuild with a structure whose instances live in world-base-cell
  partitions, advanced by an op stream naming only what changed — stable instance slots keyed by
  scene identity, writes for what appeared or moved, the cheaper update for a structure address
  that moved under an unchanged transform, inert writes for what left. Both top-level forms derive
  from one placement list, so they cannot disagree. Proven by picture: two hosts over one
  ray-traced scene, partitioned and not, render **byte-identical** frames; a settled frame then
  emits **zero** ops and adding geometry writes fewer instances than the table holds. It is opt-in
  (`SAFFRON_PTLAS=1`) because the SDK validation layers ship the extension's header without
  modelling it — there is no SPIR-V form for a shader to declare a partitioned structure, and it is
  memory rather than an object — so two VUIDs are unavoidable from engine code. e2e `rt-ptlas`
  whitelists exactly those two and fails on any other, so the exemption cannot hide a real one.

## State of the tree

All work is **unstaged and uncommitted** — the standing rule is that staging and committing are the
user's call. The last checkpoint commit is the user's own.
