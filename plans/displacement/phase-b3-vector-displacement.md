# Phase B3 — vector displacement (overhangs / undercuts)

**Status:** NOT STARTED
**Scope:** `saffron-assets` (material/texture-ref set), `saffron-rendering` (compute path, shaders)
**Depends on:** phase-b2

## Goal

Extend the displacement path from scalar-along-normal to **tangent-space vector (XYZ)** displacement,
enabling overhangs, undercuts, and mushroom-cap / ear shapes that scalar height cannot express.

## Approach

Scalar height displaces along the interpolated normal (one channel, no undercuts). Vector displacement
stores full tangent-space XYZ per texel (3 channels, orientation-sensitive). Add it as an **additive
channel** to the material/texture-ref set — not a rewrite of scalar — so a `.smat` opts into vector
displacement with a distinct texture ref, and the compute prepass (b2) branches on it.

## Touch points

- **`saffron-assets` material model** — a vector-displacement texture ref alongside `height_texture`
  (or a mode flag on the displacement input); exposed-parameter schema entry.
- **compute prepass (b2)** — sample the XYZ field, transform by the TBN, offset the vertex; careful
  filtering (do not normalize away the offset direction).

## Verification

- A vector-displacement map produces overhangs on a subdivided base; scalar `.smat` materials are
  unaffected.

## Risks

- **Filtering** a tangent-space XYZ field without collapsing the offset direction; aliasing vs scalar
  height (open question #5).
