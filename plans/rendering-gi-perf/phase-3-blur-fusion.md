# Phase 3 — Blur fusion

**Status:** NOT STARTED

`ao-blur`, `ssgi-blur`, `dfao-blur`, `specocc-blur` are the **identical** 5×5 bilateral edge-aware
upsample (Gaussian spatial × view-Z similarity from the G-buffer `.a`), dispatched as four full-res
passes reading the same guide ~4× (~1.08 ms total; ~100 guide taps/pixel where 25 would do). Fuse.

## Approach
- One packed kernel: pack the single-channel signals **gtao + dfao + specocc into RGB** of one
  half-res `rgba16f`; **SSGI stays `rgba16f`** (it is RGB and must preserve `.a = centerZ`, the
  contract `ssgi-accum` reads). Compute the 25-tap edge-stopping weights **once** from the shared
  view-Z guide, apply to all packed channels.
- Two dispatches at most (packed AO-likes; SSGI separately because of the `.a` contract), or one if
  SSGI's accum contract can be met — prefer the minimal that keeps behavior identical.
- **NO-COMPAT:** delete the four separate pass sites (`renderer.rs` ~5137/5252/5421/5587), retire
  `ao_blur.spv` if now unused, update `view_target.rs` sets (~382-396) and the `ssao` compute3
  layout. One blur path.

## Expected effect
~1.08 → ~0.35–0.5 ms (**−0.55–0.7 ms**). `gtao` r8→rgba16f packing is a trivial cost.

## Risks / watch
- Must reproduce the bilateral output bit-for-similar; preserve `centerZ` where `ssgi-accum` reads it.
- Downstream consumers unpack the right channel — verify GTAO, DFAO, specocc each read their slot.

## Verification
- Profiler: single (or dual) merged pass ms vs prior 4×0.27.
- Pixel-diff the denoised AO/DFAO/specocc/SSGI outputs before vs after for parity (no visible change
  in the final frame). `just engine` + lint clean; validation-clean.
