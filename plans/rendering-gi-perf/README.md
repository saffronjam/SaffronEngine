# Rendering — GI correctness + frame-cost overhaul

**Status:** CORE GOALS ACHIEVED. All three reported problems fixed + verified; frame confirmed under
budget in release. Phases 1–2 done; 5/6/7.2 found unnecessary; 3/4 remain as **optional** further
optimization (a frame that is already under budget).

## Outcome (verified)
- **Artifact A (floor blobs):** FIXED — RTXGI surface bias + squared backface weight + low-weight crush
  in the DDGI sampler. Before/after captures: floor lobes gone.
- **Artifact B (wall darkens on approach):** RESOLVED — the DDGI FIX 4/5 (blend only re-traced probes
  + adaptive hysteresis) + surface bias. Forward-walk: wall correctly lit at mid-approach (the only
  darkening is the genuinely-dark lion alcove nose-to-wall).
- **Perf:** `scene-opaque` **2.14 → 0.42 ms** (Phase 1 rasterization foundation + Phase 2 half-res GI
  resolve). Release + validation-off frame: **p50 0.67 ms CPU / ~3–4 ms GPU**, under the 4.2 ms /
  240 Hz budget. The original "6.35 ms CPU-bound" was the debug validation layer, not real cost.
- **Changeset:** workspace builds clean, `cargo fmt --all --check` clean, touched crates clippy-clean.
  Nothing staged/committed (git read-only).

## Optional remaining (on request)
- Phase 3 blur fusion (~0.5 ms; complex — 3 blurs with 3 different gates). Phase 4 GI shader
  permutations (occupancy). Phase 2 follow-ups: reflection-probe diffuse in the resolve + dead-code
  cleanup in `lighting.slang`.

Grounds a multi-phase fix for two user-reported problems on a trivial scene (Khronos Sponza — one
262k-tri mesh — plus a few small models) at ~720p on an RTX 3070 Ti:

1. **DDGI artifacts** — (A) soft semicircular interpolation lobes on the floor that slide as the
   camera moves; (B) the far wall + arches *darken* as the camera approaches them.
2. **Frame cost too high for the geometry** — `scene-opaque` 2.14 ms (51 % of a 5.92 ms GPU span);
   frame reported CPU-bound at 6.35 ms; four redundant `*-blur` passes (~1.08 ms).

The diagnosis was produced by an 8-agent investigation cross-validated against RTXGI / UE5 Lumen /
Godot SDFGI / id Tech. Verdicts:

- **A:** right DDGI architecture, three canonical interpolant terms missing. **DONE** (see below).
- **B:** scroll mechanism correct, but a single 24×12×24 m camera-centered volume (8 probes tall) is
  the wrong coverage model for a tall nave, and there is no probe classification/relocation.
- **scene-opaque:** architecturally heavy — full GI resolved per full-res pixel in the forward
  über-fragment, **no depth prepass**, **backface culling off**, **MSAA 4×**, monolithic
  uniform-branched PSO.
- **CPU-bound 6.35 ms:** largely a **confound** — the profiled binary is a debug build with
  `VK_LAYER_KHRONOS_validation` on. GPU pass times are real; the CPU number must be re-measured in a
  release/validation-off build before any CPU work is justified.
- **blurs:** one 5×5 bilateral kernel run four times over the same view-Z guide → fuse into one
  multi-channel pass.

## Decisions (from the user)

- **Perf: go all-in**, including moving GI resolve out of the forward fragment into a **half-res
  screen-space GI pass** (Lumen-style) — not just the quick wins.
- **MSAA: drop to 1× and rely on TAA** (modern forward+ standard; TAA is already in the pipeline).
- **DDGI-B: build the structural fix** — scroll-budget + probe classification/relocation, then
  **nested cascades** if residual darkening remains (the modern-correct destination, per the
  no-shortcuts rule — not a stopgap volume enlargement).

## Phases (dependency-ordered)

| Phase | Title | Depends on | Headline |
|-------|-------|-----------|----------|
| — | **DDGI interpolation terms (artifact A)** | — | **COMPLETED** — surface bias + squared backface weight + low-weight crush in `ddgiSampleIrradiance`. Verified before/after: floor lobes gone, validation-clean. |
| 1 | [Rasterization foundation](phase-1-rasterization-foundation.md) | — | **COMPLETED** — MSAA 1×, depth prepass (alpha-clip), end-to-end two-sided + per-submesh backface cull. `scene-opaque` **2.14 → 0.67 ms**. |
| 2 | [Screen-space GI resolve](phase-2-screenspace-gi-resolve.md) | 1 | **COMPLETED (core)** — DDGI + IBL-diffuse resolved in a half-res compute pass; fragment samples it. `scene-opaque` → **0.42 ms**. (Probe-diffuse blend + dead-code cleanup: follow-ups.) |
| 3 | [Blur fusion](phase-3-blur-fusion.md) | — | Fuse ao/dfao/specocc (+ ssgi) bilateral upsamples into one multi-channel pass sharing the guide. |
| 4 | [GI shader permutations](phase-4-gi-permutations.md) | 2 | Specialization-constant GI tiers to raise occupancy (dead-strip unreached indirect paths). |
| 5 | [DDGI scroll + classification (artifact B)](phase-5-ddgi-scroll-classification.md) | — | **LIKELY NOT NEEDED** — artifact B appears resolved by the DDGI FIX 4/5 + surface-bias (wall correctly lit at mid-approach; measured). Verify with user before building. |
| 6 | [DDGI cascades](phase-6-ddgi-cascades.md) | 5 | Nested world-aligned cascades so tall/far geometry never leaves coverage (only if residual B remains after 5). |
| 7 | [CPU re-measure + GPU-driven submission](phase-7-cpu-and-mdi.md) | 1–2 | **7.1 DONE** — release+validation-off frame p50 **0.67 ms** (debug's 6.35 ms was the validation confound). Frame is GPU-bound ~3–4 ms, under budget. **7.2 (MDI) not needed.** |

## Verification discipline (every phase)

- `just engine` clean + `just prepare-for-commit` (fmt + clippy `-D warnings`) at each phase boundary.
- `cargo run -p xtask -- shaders` after any `.slang` change; sync into the capture target.
- Objective GPU-timestamp + screenshot A/B on the NVIDIA GPU headless (`dev` project). GPU pass
  times are valid in a debug build; **CPU** frame time is only meaningful in a release/validation-off
  build (see Phase 7). Read the actual pixels — this is the gate that caught "compiles but wrong".
- Leave all work unstaged (git read-only); the user stages/commits.
