# Phase B3 — vector displacement (overhangs / undercuts)

**Status:** BLOCKED on a prerequisite the engine's vertex stream lacks — a **UV-aligned tangent per
vertex**. B2's `displace` compute pre-pass is one-thread-per-vertex over the 32-byte `saffron_geometry::Vertex`
(position@0, normal@12, uv0@24) — **no tangent**. Correct vector displacement transforms the sampled
tangent-space XYZ by the vertex's TBN; a single vertex thread cannot derive a *UV-aligned* tangent (that
needs face adjacency / the neighbouring vertices' UVs), and any substitute frame (e.g. a branchless
normal-only basis) points the X/Y offset in arbitrary directions — so the overhang would not match the
authored map. Implementing a wrong-direction approximation would violate "always the modern, correct
approach", so this phase is **deferred until a tangent stream exists** — a genuine, separate engine
feature (widen `Vertex` to carry a tangent: the glTF/OBJ importers compute + store it, every shader's
`VertexInput` grows a `[[vk::location]]` tangent, and the skin/morph/**displace** kernels + the deformed
buffer stride widen to match). This is the same class of gate as C2's mesh-shader block: real
infrastructure, out of this plan-set's displacement scope. **When the tangent stream lands**, B3 is small:
add a vector-displacement texture ref on the material (additive to `height_texture`), a feature bit, and
one `displace.slang` branch — `pos += mul(tbn, sampleVector(uv))` instead of `pos += normal * h`.
**Scope:** `saffron-assets` (material/texture-ref set), `saffron-rendering` (compute path, shaders) +
**a mesh tangent stream** (prerequisite)
**Depends on:** phase-b2 (done) + **a per-vertex tangent stream** (not yet in `Vertex`)

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
