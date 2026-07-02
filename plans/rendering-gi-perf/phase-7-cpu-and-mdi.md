# Phase 7 — CPU re-measure + GPU-driven submission

**Status:** 7.1 DONE — the CPU-bound number was the confound, as predicted. 7.2 NOT NEEDED.

Measured release build (`target-perf/release`) + `SAFFRON_DISABLE_VALIDATION=1` on the NVIDIA GPU,
`dev` scene, DDGI + sky-occ on: **frame-history p50 = 0.67 ms** (vs the debug+validation 6.35 ms —
a ~10× drop). The p95/p99/max (57 / 100 / 7500 ms, stutterCount 3/91) are one-time startup/load
hitches (SDF bake, project load, first-frame PSO compiles), not steady state. So CPU work is ~0.67 ms
/frame and the frame is **GPU-bound at the ~3–4 ms GPU span** — under the 4.2 ms / 240 Hz budget. The
original "over budget, CPU-bound" reading was the debug validation layer, not a real bottleneck.
**7.2 (draw-list caching / MDI) is not built** — there is no genuine CPU bottleneck to fix.

The reported CPU-bound 6.35 ms is **not trustworthy** until re-measured — the profiled binary is a
debug build with `VK_LAYER_KHRONOS_validation` on (`device.rs` ~610, gated by `cfg!(debug_assertions)`
unless `SAFFRON_DISABLE_VALIDATION`). This phase first establishes the real number, then does GPU-
driven submission **only if** a genuine CPU bottleneck remains.

## 7.1 — Re-measure honestly (gate for the rest of this phase)
- Build **release** in the isolated target (`CARGO_TARGET_DIR=target-perf`, release codegen) and run
  with `SAFFRON_DISABLE_VALIDATION=1`. Also compare profiler-off wall-clock to isolate the profiler's
  own `clock_gettime` + timestamp cost.
- Report the real CPU frame time. If CPU < GPU (likely), the frame is GPU-bound and 7.2 is deferred.

## 7.2 — Draw-list change-detection + GPU-driven MDI (only if CPU-bound persists)
- **Genuine residual CPU cost** (secondary): unconditional per-frame ECS re-gather + SSBO re-upload
  (`render_scene.rs` `gather_static/skinned_draw_list` clones; `instancing.rs` ~419/430/442
  `upload_into` memcpy every frame regardless of change; O(items×buckets) bucketing + per-frame
  HashMap dedup).
- **Change:** cache the draw list across unchanged frames (scene-change detection); then compute-cull
  → `VkDrawIndexedIndirectCount`, bindless via BDA/push constants, bind descriptors once per frame.
  This is the repo's already-noted "GPU-driven culling (MDI)" not-yet item — the correct destination.
- **Ruled out** (do not chase): descriptor sets are allocated once not per-frame; SSBOs grow-only;
  no per-frame `vkQueue/DeviceWaitIdle`; timestamp readback is non-blocking (reads a fence signalled
  2 frames ago).

## Verification
- 7.1: release + validation-off wall-clock CPU frame time, reported before any 7.2 work.
- 7.2 (if done): CPU submission time before/after; scene-size-independent submission. Correct render
  output. `just engine` + lint clean; validation-clean.
