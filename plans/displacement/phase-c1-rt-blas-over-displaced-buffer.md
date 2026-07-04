# Phase C1 — RT: build/refit BLAS over the displaced buffer

**Status:** NOT STARTED
**Scope:** `saffron-rendering` (RT acceleration structures)
**Depends on:** phase-b2

## Goal

Ray-traced shadows, reflections, and GI see **true displaced geometry** — the portable analogue of RTX
Mega Geometry — by building/refitting the BLAS over the Phase B displaced-vertex buffer.

## Approach

Because B2 writes displaced geometry to a VkBuffer (unlike a hardware tessellator's transient output),
a BLAS can be built over it. Prefer **refit** where the topology is stable (only positions move) to keep
per-frame cost down; rebuild when tessellation factors change topology. Do **not** adopt DMM (deprecated,
NV-only). Watch `VK_NV_cluster_acceleration_structure` / RTX Mega Geometry as the eventual accelerated
path, but treat as vendor-specific until it goes cross-vendor.

## Touch points

- RT AS management in `saffron-rendering` — a BLAS backed by the transient displaced buffer; refit vs
  rebuild policy; TLAS wiring.
- A `sa` control command to inspect/toggle displaced-RT (per keep-current).

## Verification

- A ray-traced reflection / shadow of a displaced surface matches the rasterized silhouette (no
  peter-panning / self-shadow mismatch between the GBuffer and the RT view).

## Risks

- **Per-frame refit vs rebuild cost** — displaced RT may be viable only for static/low-LOD content until
  cross-vendor cluster AS lands (open questions #4, #8).
- **Projective displacement at intersection time** (arXiv 2502.02011) is a possible alternative that
  avoids per-frame geometry regeneration — evaluate its maturity (open question #7).
