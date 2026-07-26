# Foliage/vegetation — state of play

The short version. Every design decision, root cause, and per-slice seal lives in
[`READMEFABLE.md`](READMEFABLE.md); this file is the map, not the territory. Per-box evidence lives on
the box itself in each `phase-N-*.md`.

## Phase status

| Phase | State | Boxes |
|---|---|---|
| 1 spatial/numeric | in progress | 33 / 34 |
| 2 domain assets + mutations | **complete** | 25 / 25 |
| 3 graph determinism | in progress | 30 / 31 |
| 4 cooker + cell artifacts | **complete** | 23 / 23 |
| 5 runtime cells + persistence | **complete** | 28 / 28 |
| 6 virtual geometry substrate | **complete** | 32 / 32 |
| 7 GPU scene + visibility cutover | complete, 3 carve-outs | 23 / 26 |
| 8 vegetation rendering | **complete** | 26 / 26 |
| 9 editor authoring + debug | **complete** | 21 / 21 |
| 10 wind/deformation/phenology | complete, 7 carve-outs | 24 / 31 |
| 11 VSM / lighting / RT | in progress | 15 / 29 |
| 12 interaction/physics/queries/nav | **complete** | 26 / 26 |
| 13 ecology + catch-up | **complete** | 28 / 28 |
| 14 botanical authoring + interchange | in progress | 20 / 25 |
| 15 production/platform closure | in progress | 8 / 20 |

Nine phases complete; **43 boxes open**, of which **23 cannot be closed on the current Mac**.

## What the NVIDIA machine unblocks

Everything below is blocked purely by hardware today. On the Linux toolbox with a discrete RTX these
become ordinary work — no code change is needed first.

- **Ray tracing — 8 phase-11 boxes.** This Mac reports zero `VK_KHR_ray_query` /
  `VK_KHR_acceleration_structure`, so `Device::new` resolves `rt_supported = false`. Unblocks shared
  compacted BLAS, deformation materialization, voxel clusters as AABBs, the KHR any-hit baseline, OMM
  derivation, `VK_NV_cluster_acceleration_structure`, BLAS/TLAS tracking, and the any-hit/OMM parity
  acceptance. Also the BLAS half of phase-15's GPU-telemetry box.
- **Mesh shaders — 3 phase-7 boxes.** `VK_EXT_mesh_shader` execution, indexed-vs-mesh executor
  semantic-cut parity, and the validation sweep.
- **A second platform — phase-15 image comparison** (NVIDIA + MoltenVK is two), and **the NVIDIA
  validation box** itself.
- **A software rasterizer — phase-15 headless/software box.** `just run-software` forces llvmpipe,
  which has no macOS equivalent. Headless alone is already validated.
- **Visual/timing captures** in phases 10 and 11.
- **Probably the player-boot box** — see the live bug below.

## What stays blocked after the move

- **Anything naming AMD.** Phase-15's AMD box; the Rust/Slang goldens in phases 1 and 3 and the
  validation sweep in phase 7 all say *NVIDIA, AMD, and MoltenVK* — NVIDIA gets two of three.

## Buildable anywhere (no hardware gate)

- **Phase 14 (5):** Plant-workspace render surfaces (3D preview, wind preview, lifecycle timeline,
  materials/atlas, collision/nav, hierarchy/voxel/error); bounded cancellable preview;
  presets/subgraphs via `.splant` internal modules (**no new asset format**); the generator outputs
  (atlases, coverage-preserving textures, aggregate-voxel appearance error); USD skeleton/plant
  metadata.
- **Phase 15:** distributed work-item manifests; the non-BLAS GPU telemetry (bins, overdraw,
  deformation counts); Perfetto/capture integration.
- **Phase 11 (2):** aggregate-voxel injection/sampling parity — the `parity_occupancy` /
  `aggregate_transmittance` helpers exist and are tested, wiring them through injection is the work —
  and the GI/reflection culling parameterization audit.
- **Phase 10:** deform assembly parts without expanding authored structure; the event-emission box;
  the wind debug overlays.

## Live bug, not a gap

`saffron-player` creates its renderer and then hangs on frame 1 under MoltenVK in a non-interactive
context. The frame watchdog reports `GPU submission 'frame 1' has been in flight 119s`. It reproduces
with no project and no vegetation, so it is the windowed present path, not the export:

```sh
SAFFRON_EXIT_AFTER_FRAMES=5 ./engine/target/debug/saffron-player
```

Retest this first on Linux — it may simply not reproduce, which closes the player-smoke arm of two
phase-15 boxes.

## Verifying on the new machine

The `just` recipes auto-enter the `saffron-build` toolbox and set the NVIDIA ICD via the `gpu_driver`
macro. Do **not** hand-roll the driver path; a wrong one silently drops to llvmpipe.

```sh
just engine && just prepare-for-commit    # build + fmt + clippy -D warnings
just schema                               # 249 manifest-driven control checks
just test                                 # cargo test --workspace
just e2e                                  # 328 tests / 51 files
just check                                # the whole reproducible gate
```

Two differences from the Mac runs: the e2e harness starts a headless weston itself on Linux (no
`env -u VK_LAYER_PATH …` prefix needed), and `vulkaninfo --summary` should name the RTX rather than
`Apple M4`. If a `.splant` schema identity changes, regenerate the e2e fixture with
`cargo run -p xtask -- gen-vegetation-e2e-fixture`.

Last full verification on this Mac (2026-07-26): every command above green except the platform arms,
with `just e2e` at 328/328.

## State of the tree

All work is **unstaged and uncommitted** — the standing rule is that staging and committing are the
user's call. The last checkpoint commit is the user's own.
