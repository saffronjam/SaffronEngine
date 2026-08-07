# Foliage/vegetation — state of play

The short version. Every design decision, root cause, and per-slice seal lives in
[`READMEFABLE.md`](READMEFABLE.md); this file is the map, not the territory. Per-box evidence lives on
the box itself in each `phase-N-*.md`, and [`AUDIT.md`](AUDIT.md) is an independent read of those boxes
against the tree.

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
| 10 wind/deformation/phenology | **complete**, carve-outs below | 31 / 31 |
| 11 VSM / lighting / RT | **complete** | 24 / 24 |
| 12 interaction/physics/queries/nav | **complete** | 26 / 26 |
| 13 ecology + catch-up | **complete** | 28 / 28 |
| 14 botanical authoring + interchange | **complete**, 1 carve-out | 25 / 25 |
| 15 production/platform closure | **complete** | 20 / 20 |

No box is open. Each carve-out is annotated on its own box and named in its phase's `Status` line
rather than counted as done: phase 10's cluster-tight swept bounds (they ride the Phase 11 VSM/RT
consumers that need the per-cluster form), its deeper debug surfaces (turbulence spectra as a
per-octave view, a whole-field interaction capture) and its authored per-part stiffness; phase 14's
plant-proxy overlay appearance, which is a human-at-the-screen check.

## Platform coverage

| Adapter | What it covers |
|---|---|
| `NVIDIA GeForce RTX 3070 Ti` (driver 610.43.03, api 1.4.341) | the required tier plus every optional one: `VK_KHR_acceleration_structure`, `VK_KHR_ray_query`, `VK_KHR_deferred_host_operations`, `VK_EXT_mesh_shader`, `VK_EXT_opacity_micromap` (`micromap = true`, subdivision level 12), `VK_NV_cluster_acceleration_structure`, `VK_NV_partitioned_acceleration_structure` |
| `Apple M4` through MoltenVK (api 1.4.334) | the required indexed-MDI path, portable aggregate voxels, physical-atlas VSM, KHR features the driver exposes; no mesh stage, and nothing degrades for it. Its Rust/Slang conformance record is not current — `just compute-conformance` on an Apple device writes one |
| Mesa llvmpipe | the software tier — correctness and validation, never performance. `vegetation-graph` is the one suite a CPU device cannot pass, because `VulkanGraphComputeExecutor::new` refuses GPU graph qualification there by contract |

AMD is out of scope by the project owner's decision (2026-07-26): no such adapter exists for this
project and none can be obtained. **No AMD verification was performed and none is claimed.** Phase
15's AMD box is struck through as out-of-scope rather than done; if an adapter ever appears, reopen the
boxes that named it rather than trusting them.

Behaviour keys on feature bits and limits, never on vendor identity or extension names — `vendor_id`,
`device_id`, `driver_id`, both UUIDs and the `molten_vk` flag are recorded into `VulkanProfileEvidence`
and read by nothing that chooses behaviour.

## The opt-in switches

These environment switches select a capability path or pin a decision a test needs held still. Every
harness asserts the switch took effect rather than trusting the flag — a readback from `render-stats`,
a counter the switch moves, or a measured frame difference — so a switch that silently did nothing
fails rather than passes. Five of the six compare two hosts differing in exactly that variable; the
micro-field harness runs one host and asserts `microCandidates` is zero.

| Switch | What it does | Harness |
|---|---|---|
| `SAFFRON_MESH_EXECUTOR=1` | runs the shaded scene pass through the übershader's `VK_EXT_mesh_shader` entry where `Capabilities::mesh_shader` holds | `tests/e2e/mesh-executor-parity.test.ts` |
| `SAFFRON_PTLAS=1` | replaces the whole-table TLAS rebuild with `VK_NV_partitioned_acceleration_structure` | `tests/e2e/rt-ptlas.test.ts` |
| `SAFFRON_OMM=off` | suppresses opacity-micromap attachment while the cook still derives them | `tests/e2e/vegetation-atlas-micromap.test.ts` |
| `SAFFRON_CUT_OVERRIDE` | pins the traversal's refinement (`coarse`/`fine`) so a representation comparison changes one variable | `tests/e2e/vegetation-representation-parity.test.ts` |
| `SAFFRON_NODE_CULL` | pins the hierarchy node cull on or off | `tests/e2e/node-cull-parity.test.ts` |
| `SAFFRON_MICRO_FIELD=off` | stops the reconstructed micro-blade passes so a test can measure the macro plants alone | `tests/e2e/vegetation-distant-wind.test.ts` |

The cut is also pinnable over the control plane through `set-hierarchy-cut {auto|coarse|fine}` per
view.

## Verifying

The `just` recipes auto-enter the `saffron-build` toolbox and set the NVIDIA ICD via the `gpu_driver`
macro. Do **not** hand-roll the driver path; a wrong one silently drops to llvmpipe.

```sh
just engine && just prepare-for-commit    # build + fmt + clippy -D warnings
just schema                               # the manifest-driven control contract against a live host
just test                                 # cargo test --workspace
just e2e                                  # the tests/e2e bun suite
just check                                # the whole reproducible gate (tools/ci/check.sh)
```

Both harnesses boot the host offscreen on every platform, so no compositor is involved and device
selection takes the discrete GPU; `vulkaninfo --summary` should name the RTX. A headless compositor
denies present support to the discrete adapter, so a *windowed* boot there silently lands on llvmpipe —
which is how an "NVIDIA validation" claim can be false without anyone lying.

If a `.splant` schema identity changes, regenerate the e2e fixture with
`cargo run -p xtask -- gen-vegetation-e2e-fixture`.

## The one open defect

`just schema` intermittently wedges the NVIDIA GPU with `ERROR_DEVICE_LOST` in the thumbnail render
path: `GPU submission 'frame 167' has been in flight`, then the loss surfacing on
`preview thumbnail render` and the next `begin_frame`, with the validation layers silent. Every contract
check itself passes before the host dies. Ruled out: the node cull (`SAFFRON_NODE_CULL=off` hangs too),
the aggregate ray work (the rate does not follow the feature across a full bisection), the
reactive-coverage wind read, the windowed present path, a cross-thread queue race (`GpuQueue` holds its
handle behind a mutex and every operation takes it), and the GPU itself. It wants a capture with
GPU-assisted validation armed long enough to reach frame 167. Phase 15's terminal box carries the full
record.
