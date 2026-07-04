# Phase B2 — in-scene compute adaptive tessellation → buffer

**Status:** NOT STARTED
**Scope:** `saffron-rendering` (compute prepass, render graph, mesh übershader)
**Depends on:** phase-b1 (transient buffer), **`primitive-meshes/`** (clean-UV base meshes for tests)

## Goal

The modern-correct, portable in-scene displacement path: a render-graph **compute prepass** that, per
frame, computes screen-space-adaptive **watertight** edge factors, tessellates a scene mesh into a
scratch vertex/index buffer, displaces (scalar height), welds seams, and recomputes normals/tangents.
Rasterizer, shadow passes, and GBuffer all consume the same buffer. Retire in-scene POM for
displacement-enabled `.smat` materials.

## Why this over hardware tessellation

The stored-buffer model enables **edge welding** (averaging positions/normals across shared edges/UV
seams before rasterizing) — impossible with hardware tessellation's immediate-consumption model — and,
crucially, the buffer can back a **BLAS** (Phase C1). Reported ~0.5 ms adaptive vs ~15 ms fixed
tessellation-127 (Filmic Worlds). One tessellation shared by all passes, no sub-pixel micro-triangle
rasterizer inefficiency.

## Touch points

- **Compute prepass** — per-edge screen-space factor derivation (identical from both patches sharing an
  edge → no cracks); tessellate into the transient buffer (Phong or similar barycentric scheme);
  displace along the interpolated normal by the height field.
- **Welding pass** — average seam vertices; recompute normals/tangents (analytic finite-difference).
- **Render graph** — declare the transient buffer (phase-b1), derive barriers; depth/shadow/GBuffer read
  it.
- **Übershader / material** — a displacement feature bit gating displacement-enabled `.smat` materials;
  the in-scene POM path is retired for those.

## Verification

- A displaced plane/terrain shows true silhouettes in the main viewport and in shadow maps
  (self-shadowing matches the displaced surface); validation-clean; frame-cost measured.
- No cracks at UV seams / mesh boundaries under camera motion (LOD transitions watertight).

## Risks (the hard part)

- **Watertight adaptive factors across UV seams + mesh boundaries** — budget welding; possibly authored
  seam-height normalization (open question #1).
- **Per-frame cost vs animation-static reuse** — caching displaced buffers, interaction with instancing
  and the PSO/übershader cache (open questions #3, #6).
- **`height_scale` world-space units** — consistent with the Phase A preview (open question #2).
