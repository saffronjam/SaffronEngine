# Phase C1 — RT: build/refit BLAS over the displaced buffer

**Status:** IMPLEMENTED — falls out of B2's shared-buffer design for free (builds + green with B2; a
live RT-shadows-on displaced scene wants a GPU-with-eyes run). B2 writes displaced vertices into the
**same deformed buffer** the skinned-BLAS refit already reads, and `Instancing` appends each displaced
instance to `SceneDrawList::deformed_rt_instances` (with its `deformed_offset`, `world_transform` = the
node model matrix since the displaced vertices are mesh-local). `plan_skinned_blas_refits` /
`SkinnedBlas` is agnostic to *how* a vertex slice was deformed — it reads a device-address slice of the
shared buffer — so a displaced instance (keyed by its entity uuid, RT-armed) gets a BUILD-then-in-place
`MODE_UPDATE` refit exactly like a skinned one. The `tlas-build` pass's `AccelStructBuildRead` on the
shared `deformed` resource orders it after the `displace` compute pass automatically (the graph derives
the compute-write → AS-build barrier). The deformed buffer already carries
`ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR` (via `make_deformed_buffer` when RT is supported). No
displacement-specific RT code was needed — the shared-buffer architecture is what makes RT free.
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
