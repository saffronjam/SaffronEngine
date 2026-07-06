# Phase D3 — parallax-occlusion-mapping robustness (fix the grazing-angle swim)

**Status:** IMPLEMENTED (shaders compile; visual grazing-angle check deferred to a GPU-with-eyes pass).
`parallaxUv` (`lighting.slang`) was rewritten with the two mitigations that kill the reported swim:
**offset limiting** (march the bounded `v.xy * scale` offset — never `v.xy / v.z`, whose division by the
near-zero grazing `v.z` caused the blow-up) and a **dynamic sample count**
(`lerp(MAX_STEPS, MIN_STEPS, saturate(v.z))`, 8–32 layers — more at grazing). Added a guard on the
interpolation denominator + a `clamp(w, 0, 1)`. **Deferred follow-ups** (documented, not regressions):
distance/mip fade POM→bump and pixel-depth-offset (the larger step that touches depth/shadow ordering) —
each wants its own verification pass.
**Scope:** `saffron-rendering` (`lighting.slang` `parallaxUv`, `mesh.slang`)
**Depends on:** — (standalone; makes D1's **Parallax** mode a first-class, artifact-free technique).
Independent of the geometry engine (A/B/C) and of D2's routing.

## Goal

Make the **Parallax (POM)** mode artifact-free at grazing angles. Even after D2 routes library
*Displacement* maps to real displacement, POM remains the right tool for genuine height/parallax maps
on near-perpendicular surfaces — and today it **swims/melts** at the sphere's silhouette, the exact
artifact seen on Rock 063 before D2 reroutes it.

## The defect (grounded)

`parallaxUv` (`lighting.slang:540`) marches a **fixed 24 steps** (`:542`) with offset
`delta = (vt.xy / max(|vt.z|, 0.1)) * scale / steps` (`:544`). The `max(|vt.z|, 0.1)` clamp still lets
the UV offset grow without bound as the view grazes the surface (`vt.z → 0`), so texels are sampled
from wildly wrong locations → the swimming/melting.

## Approach (standard mitigations, from the survey — Welsh 2003, Tatarchuk SIGGRAPH 2006)

Apply in order; the first two kill the reported artifact:

1. **Offset limiting (Welsh 2003).** Clamp the *total* UV offset magnitude to `≤ height × scale`
   instead of dividing by `vt.z`. This is Unreal's "Bump Offset" / Babylon's `parallaxScaleBias`
   regime; it removes the grazing blow-up at the cost of mild flattening at extreme angles (accepted
   industry tradeoff).
2. **Dynamic sample count (Tatarchuk).** `n = n_min + (N·V)(n_max − n_min)` — more march steps near
   grazing, fewer head-on — replacing the fixed 24. Better quality where it's needed, cheaper where it
   isn't.
3. **Distance / mip fade to Bump.** Blend POM → the D1 height-gradient bump over a mip transition band
   so distant tiles stop shimmering (also a straight perf win).
4. **Pixel depth offset (optional, larger).** Write the displaced depth so sorting, self-shadowing, and
   SSAO composite against the parallaxed surface (Unity "Depth Offset", Unreal "Pixel Depth Offset").
   Scope separately — it touches depth/shadow ordering.

## Verification

- A Parallax-mode material on a sphere and a plane shows **no swimming at grazing angles** (force
  Rock 063 to Parallax and compare before/after side by side).
- Distant tiled surfaces do not shimmer (mip fade engaged).
- Validation-clean; POM cost measured (dynamic sample count should not regress the head-on case).

## Risks

- **Offset limiting flattens the parallax at extreme grazing** — expected and standard; document it.
  Real silhouettes are D1's Displacement mode / the geometry engine, not POM's job.
- **Pixel depth offset** interacts with depth pre-pass, shadow bias, and motion vectors — if pursued,
  gate it behind its own verification, not folded into the offset-limiting fix.
