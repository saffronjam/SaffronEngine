# Phase 6 — DDGI cascades

**Status:** NOT STARTED (do only if residual artifact B remains after Phase 5)

A single 24×12×24 m camera-centered volume (16×8×16 probes @ 1.5 m) is 8 probes tall — marginal for
Sponza's ~15 m nave. When the camera approaches a tall wall, far/high geometry leaves the cage and
hands back to dimmer analytic IBL. The modern-correct fix (Lumen world radiance cache, Godot SDFGI,
DDGI-2021 multivolume) is **nested world-aligned cascades**, not a bigger single volume.

## Approach
- Add a coarser outer cascade at 2× spacing (same probe count, 4× the coverage volume) surrounding
  the fine cascade. Per-cascade scroll base + reset + atlases.
- `ddgiSampleIrradiance`: select the finest cascade that contains the sample; fall back to the next
  coarser when the sample is outside the fine cascade (blend across the boundary to avoid a seam).
- Trace/blend budgets per cascade (the coarse cascade refreshes slower — it is lower frequency).

## Expected effect
Definitive structural fix for B — far wall + arches never leave coverage. Bright-far / dark-near is
eliminated on every wall.

## Risks / watch
- Largest DDGI effort: extra atlases + per-cascade scroll/reset/relocation, boundary blend, memory.
  Watch total atlas memory with 2+ cascades.
- Do **after** Phase 5 — measure residual B first; classification/relocation + scroll-clear may
  already remove enough that cascades are only a polish.

## Verification
- Walk the full nave on `dev`: no bright-far/dark-near on any wall at any distance; smooth cascade
  boundary (no visible ring). Validation-clean; `just engine` + lint clean.
