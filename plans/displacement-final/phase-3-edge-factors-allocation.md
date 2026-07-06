# Phase 3 — Visibility-robust fractional per-edge factors + worst-case allocation + budget & LOD bucketing

**Status:** NOT STARTED

## Goal

Decide **how much** each displaced instance emits, and **reserve worst-case space** for it, before the
GPU has computed any exact count. Two compute passes:

- **(a)** a per-unique-edge kernel that writes **one fractional tessellation factor per base edge** from
  the two shared endpoints only (DiagSplit local-edge invariant → no T-junctions), with a
  **visibility-robust** world-space fallback so a clipped / behind-near / back-facing endpoint never
  changes the factor an incident triangle reads;
- **(b)** a `predict → prefix-sum → indirect-args` pass that turns the per-edge factors into exact
  per-triangle output counts, prefix-sums them to packed write offsets, and emits the
  `cmd_dispatch_indirect` args + per-instance `VkDrawIndexedIndirectCommand` seeds + a GPU primitive
  count.

This is where `TransientResources` (`transient.rs`) gains its **first consumer**: the scratch VB,
generated IB, and count/args buffers are acquired at a **worst-case** upper bound via the Phase-1
**keyed** `acquire_buffer`, and the prefix-sum offsets only pack exact data *inside* that reservation.
A hard per-instance factor cap, a global tessellation budget, a `TransientResources` shrink/reclaim
path, and a per-instance LOD-bucket key all land here. Phase 3 produces the factors, offsets, args, and
allocation and proves them by **CPU readback**; the dice/emit kernel that consumes them is Phase 4.

## Build plan

### Scaffolding: a `Tessellation` subsystem, gathered from the existing displaced-instance path

Stand up a `saffron_rendering::Tessellation` subsystem module (`crates/rendering/src/tessellation.rs`)
**mirroring `Displacement`** (`displacement.rs`) and `Skinning`: it owns its compute set layouts + a
per-frame descriptor pool, and exposes `wire_dispatches` / `record_*`. Phase 3 fills in the **factor**
and **predict/scan/args** passes; Phase 4 adds the dice/emit kernel to the same subsystem.

The set of instances to tessellate is exactly the set the current displace path already gathers.
`displace_info_for` (`instancing.rs:928`) resolves the first `HeightMode::Displacement` submesh material
into a `DisplaceInfo { height_index, height_scale, uv_transform, vector_index }` (`instancing.rs:917`);
the deform loop (`instancing.rs:330-451`) pushes a `DisplaceBucket` (`displacement.rs`) per displaced
instance. Phase 3 gathers a parallel `TessBucket` per displaced instance carrying:

- `mesh: Arc<GpuMesh>` — the base mesh, now with the **Phase-2 conditioning buffers** (unique-edge list,
  per-triangle edge references, welded per-vertex displacement direction + seam-consistent tangent,
  per-edge seam-sampling flag) uploaded alongside the base vertex/index buffers (Phase 2 propagates
  these through `GpuMesh` via `upload.rs`);
- the `DisplaceInfo` fields (unchanged — `height_index`, `height_scale`, `uv_transform`, `vector_index`);
- the per-instance model matrix `bucket.model` and entity uuid (for RT keying in Phase 7);
- the **budget** (factor cap, edge-length target, global micro-tri ceiling) — Phase-9's `sa` tessellation
  control command feeds these; until it exists, a compiled-in default.

Both prep passes slot into the render-graph **deform scope** in `record_scene_graph` (`renderer.rs`,
where `RgPass::compute("displace")` is added ~3862), before the Phase-4 emit pass. During Phase 3 the
1:1 `displace` pass is **left untouched** and remains the rendering path; the tessellation prep passes
have no renderer consumer yet (only the verification readback), so no second *rendering* path exists.
Phase 4 performs the atomic NO-LEGACY cutover (delete the 1:1 kernel + `deformed_cursor +=
vertex_count`) in the same change that lands the emit kernel.

### (a) Per-unique-edge fractional factor kernel

One compute thread per **unique base edge** (from the Phase-2 edge list). Per edge:

1. Read its two endpoint positions from the mesh's **welded** base stream (Phase 2 guarantees the two
   incident triangles reference the *same* endpoint indices for a shared edge). Transform both to clip
   space with the pushed main-camera `view_proj` (`SubmitInputs.view_proj`, `instancing.rs:52`) composed
   with `bucket.model`.
2. **Angular screen-space arc metric** (stable under camera rotation, unlike post-projection screen
   length): compute the angle the edge subtends at the camera — `atan2` of the endpoints' view-space
   directions, or the great-circle arc of the two normalized camera-to-endpoint vectors — and convert to
   a pixel budget using the pushed viewport extent and vertical FOV. The fractional factor is
   `f = clamp(arc_pixels / edge_length_target, min_factor, factor_cap)`. **Store the full fractional
   value** (an `f32`, or fixed-point at ≥8 fractional bits): the integer part `floor(f)` drives the dice
   grid; the **fractional remainder** `f - floor(f)` is what Phase 5 reads to geomorph across integer
   transitions (pop-free dolly/zoom).
3. **Visibility robustness (the load-bearing correctness point).** Bit-identity between the two incident
   triangles is **structural, not a floating-point hope**: the factor is computed *once* per unique edge
   and both triangles index the *same* buffer slot, so there is nothing to reconcile even under
   non-associative float reordering. The fallback exists only to keep the metric **finite and
   range-clamped** when the screen-space arc is undefined: if either endpoint has `clip.w <= near`
   (clipped / behind the near plane), or lies outside a guard band, substitute a **world-space** metric
   that depends only on the two endpoints — world edge length over camera distance, converted to an
   approximate pixel budget — clamped to `[min_factor, factor_cap]`. Back-facing is *not* a per-edge
   property (an edge is shared by two oppositely-facing triangles in general), so the kernel never
   consults triangle winding; "back-facing endpoint" reduces to the same world-space fallback whenever
   the screen-space arc degenerates. The factor is therefore a pure function of `(endpoint A, endpoint B,
   camera)` and nothing else.
4. Write the factor to a per-edge factor buffer (transient, keyed — see allocation).

**Inner factors** are *not* stored per edge; they are derived in pass (b) / the Phase-4 emit kernel from
a triangle's three edge factors (the clamped-parallelogram interior density is a function of the three
outer factors), so a triangle never needs data beyond its three shared-edge slots.

**Shadow-LOD consequence (documented compromise).** These factors are **main-camera-derived** and, per
Phase 6, shared across all seven geometry passes (scene, depth prepass, directional/spot shadow,
point-shadow cubes, GBuffer, motion) — this is deliberate: one factor per edge, read by every pass, is
exactly what keeps depth vs shade vs shadow watertight and crack-free. The accepted cost: a caster far
from the main camera but near a light is tessellated for the main camera's screen error, so it **may be
under-tessellated in its own shadow map**. Provide an **optional** endpoint-only light-proximity term —
`max` over the scene's lights of a `1/distance` boost added to the per-edge metric, still a pure function
of the two endpoints — behind a budget flag; when off, this compromise stands and is documented here and
in the Phase-9 docs page. It must never become a per-light factor (that would break the single-factor
invariant and re-introduce cracks between passes).

### (b) `predict → prefix-sum → indirect-args`

A second compute pass over **base triangles** (across all gathered displaced instances):

1. **predict.** Each thread reads its triangle's three edge factors via the Phase-2 per-triangle edge
   references, takes their integer parts, and computes the **exact** output vertex and index counts for
   the Phase-4 clamped-parallelogram dice + matched-gap stitch — a closed-form function of `(f0, f1,
   f2)`. (Phase 4 pins the exact dice/stitch topology; Phase 3's count function must match it bit-for-bit
   so packed offsets are exact.)
2. **scan.** Prefix-sum the per-triangle counts to per-triangle **write offsets** into the packed VB and
   the packed IB. Use a subgroup scan (`WavePrefixSum`) within a workgroup + one atomic add per workgroup
   into a global running total (`InterlockedAdd`) for the cross-workgroup carry — the standard
   scan-with-atomic-carry the Filmic-Worlds pipeline uses. Offsets are computed per instance so a draw's
   `firstIndex` / `vertexOffset` land at the instance's slice base.
3. **args.** Emit:
   - a `cmd_dispatch_indirect` args buffer (`VkDispatchIndirectCommand`) sizing the Phase-4 emit dispatch
     to the actual total micro-triangle count;
   - a per-instance `VkDrawIndexedIndirectCommand` seed buffer (`indexCount` = the instance's packed
     index total, `firstIndex` = its IB slice base, `vertexOffset` = its VB slice base,
     `instanceCount` = 1, `firstInstance` = the instance row) for the Phase-6 `cmd_draw_indexed_indirect`
     raster;
   - a **GPU primitive-count** buffer per instance (triangles = packed index total / 3) for the Phase-7
     BLAS build (indirect build, or the range struct on the worst-case path).

The args/count buffers are written with `StorageWriteCompute`; the Phase-4 emit pass reads the args via
the Phase-1 `IndirectCommandRead` usage on a `cmd_dispatch_indirect`; the Phase-6 draw reads the draw
seeds via `IndirectCommandRead`; the Phase-7 build reads the count. All of these graph usages and the
`INDIRECT_BUFFER` buffer-usage flag are the Phase-1 deliverables (`RgUsage::IndexInputRead`,
`RgUsage::IndirectCommandRead`, `add_indirect_compute_pass`) — Phase 3 is their first user.

### Allocation — worst-case reserve via the Phase-1 keyed acquire

`acquire_buffer` needs a **CPU size before the GPU computes counts**, so the reservation is a
**worst-case upper bound** and the prefix-sum only *packs* exact data inside it (it never shrinks the
reservation). Sizes are computed CPU-side from the gathered base primitive counts and the factor cap:

- A clamped-parallelogram dice of one base triangle at edge factors up to `factor_cap` yields at most
  ≈ `factor_cap²` micro-triangles and ≈ `(factor_cap + 1)²` micro-vertices (the matched-gap stitch adds
  a bounded region within the same cap). So over all displaced instances:
  `worst_verts ≈ Σ base_prims × (factor_cap + 1)²`, `worst_indices ≈ Σ base_prims × factor_cap² × 3`.
- Acquire, **at a fixed keyed label each frame** (so grow-only reuse hits — never a positional cursor,
  which a conditionally-present tessellation pass would desync; this is exactly why Phase 1 adds the
  keyed variant): the scratch VB, the generated IB, the count buffer, and the args buffer. The **usage
  union** on the VB+IB is
  `STORAGE | VERTEX | INDEX | INDIRECT | SHADER_DEVICE_ADDRESS | ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR`
  — the Phase-C1 `make_deformed_buffer` RT flags plus `VERTEX | INDEX | INDIRECT`. Pass the **same
  union at the same key every frame** so `acquire_buffer`'s `usage.contains(usage)` fit test always
  passes and the pool never reallocates on a usage change. Import each into the graph
  (`RenderGraph::import_buffer`) and declare the usages; the graph derives every barrier.

This makes the RT worst-case sizing (Phase 7) share the *same* `factor_cap × base_prims` bound the
transient reservation uses — AS + build scratch are sized from the same number, so the raster reserve and
the BLAS reserve never disagree.

### Budget — hard cap, global ceiling, and a `TransientResources` shrink/reclaim path

Two enforcement points, both fed by the Phase-9 control command:

- **Per-instance factor cap** — `factor_cap` clamps the metric in pass (a); it also bounds the worst-case
  reservation above.
- **Global tessellation budget** — an edge-length target (pixels) plus a max-micro-tris-per-frame ceiling.
  The scan's global running total is compared against the ceiling; when it would overflow, apply a global
  factor down-scale (a multiplier reducing every edge factor uniformly, preserving watertightness because
  it scales the *shared* per-edge value) and/or drop the lowest-priority instances (by projected
  coverage). This replaces the current blunt `DISPLACE_MAX_SETS_PER_FRAME = 64` +
  `clamp_to_set_budget` truncation (`instancing.rs:631`) for the tessellated path with a real
  micro-triangle budget.

Because the pool is **grow-only** (`grow_bytes`, `transient.rs:30`) and now holds the session peak across
VB + IB + scratch + AS × `MAX_FRAMES_IN_FLIGHT`, one close-up high-factor frame pins peak VRAM for the
session. Add an explicit **shrink/reclaim path to `TransientResources`** (an intended change to the reused
pool):

- Track, per keyed slot, a rolling **high-water** over a window; when a slot's `capacity` exceeds its
  recent high-water by a margin for `M` consecutive frames, **reallocate it smaller** (grow-only becomes
  grow-mostly, with bounded reclaim).
- Reclaim must obey the same fence discipline as `begin_frame` (`transient.rs:79`): a slot is only
  reallocated after its in-flight fence has signalled, so no reclaim frees an allocation the GPU is still
  reading. The keyed-acquire identity (Phase 1) is what makes reclaim safe — a keyed slot has a stable
  label, so its high-water history is well-defined frame to frame even when some frames skip the pass.

### LOD bucketing key (Phase-7 setup)

Quantize a per-instance representative factor into **LOD buckets** and store the bucket id on the
tessellated instance (carried into the Phase-7 `TessellatedBlas` path via a field added to
`DeformedRtInstance`, `draw_list.rs:273`). The representative factor is derived **CPU-side, no readback**,
from the instance's projected bounding-sphere size — the same bounding-sphere-vs-`view_proj` estimate the
meshlet task-shader cull already uses (`meshlet.slang`) and that `bucket.mesh` + `bucket.model` +
`SubmitInputs.view_proj` make available — quantized by `log2` into a small number of buckets. Phase 7
uses the bucket id so same-bucket instances can share **one coarse tessellated BLAS** referenced many
times in the TLAS, without which a populated scene of displaced instances is infeasible.

## Scope

`saffron-rendering` — a new `Tessellation` subsystem (factor + predict/scan/args compute passes, their
Slang kernels under `assets/shaders/`), the deform-scope wiring in `renderer.rs`, the keyed
`TransientResources` acquisitions + the shrink/reclaim path in `transient.rs`, and the per-instance gather
extension in `instancing.rs`. Consumes Phase-2 conditioning buffers (via `GpuMesh`) and Phase-1 graph
usages / keyed acquire / capability probes. Produces the per-edge factor buffer, packed offsets, indirect
args, GPU primitive counts, worst-case-sized transient VB/IB, and the per-instance LOD-bucket key. No
renderer output yet (the emit kernel is Phase 4); the 1:1 `displace` path is untouched until Phase 4.

## Depends on

- **[`phase-1-graph-indirect-foundations.md`](phase-1-graph-indirect-foundations.md)** — the keyed
  `TransientResources::acquire_buffer(frame, key, bytes, usage)`, `RgUsage::IndexInputRead` /
  `IndirectCommandRead` + `INDIRECT_BUFFER` on `Buffer::new`, `add_indirect_compute_pass` /
  `cmd_dispatch_indirect`, and the capability probes.
- **[`phase-2-import-watertight-conditioning.md`](phase-2-import-watertight-conditioning.md)** — the
  unique-edge list + per-triangle edge references (so a factor is indexable from both incident triangles),
  the welded per-vertex data, and the seam flags, all propagated through `GpuMesh`.

## Verification

All Phase-3 verification is **CPU-readback** and needs no GPU-with-eyes (the visual crack test is Phase
4/6/7). Run a headless host (`SAFFRON_EXIT_AFTER_FRAMES`), map the transient buffers back, and assert:

- **Worst-case allocation sizing** — the acquired VB/IB byte sizes equal `factor_cap × base_prims`
  worst-case (not the packed counts); acquiring the same keyed label two frames running with the same
  scene does **not** reallocate (grow-only reuse hits under the fixed usage union).
- **Exact packed counts** — the prefix-summed total vertex/index counts equal the closed-form sum of the
  per-triangle count function over the actual (integer-part) factors, and per-instance
  `VkDrawIndexedIndirectCommand.firstIndex` / `vertexOffset` land exactly at each instance's slice base
  with no overlap and no gap beyond the reserved worst case.
- **Shared-edge bit-identity, including off-screen** — construct a two-triangle mesh sharing one edge;
  read the per-edge factor buffer and assert both triangles' edge references resolve to the **same slot
  with the same fractional value**. Repeat with the camera posed so **one endpoint of the shared edge is
  behind the near plane** (world-space fallback active) and assert the factor is still a single finite
  value in `[min_factor, factor_cap]` that both triangles read — the shared surface cannot crack because
  the value is stored once.
- **Geomorph remainder present** — assert the stored factor carries a nonzero fractional remainder at a
  non-integer camera distance (the Phase-5 geomorph input exists).
- **Budget enforcement** — with a low global micro-tri ceiling, assert the running total is held at or
  below the ceiling (global down-scale applied) and that watertightness holds (the down-scale is uniform
  over shared per-edge values).
- **Shrink/reclaim** — force one high-factor frame, then `M+` low-factor frames, and assert the keyed
  slot's capacity is reclaimed below the session peak (and never reclaimed before the slot's fence).

Add these as `#[cfg(test)]` / e2e-driven readback assertions in `saffron-rendering` (mirroring the
`transient.rs` `grow_bytes` unit test and the render-graph golden-table tests). Gate each phase boundary
with `just engine` then `just prepare-for-commit`.

## Risks

- **The count function must match Phase-4's dice/stitch exactly.** If pass (b)'s per-triangle count
  diverges from the Phase-4 clamped-parallelogram + matched-gap emit by even one micro-triangle, packed
  offsets overlap or leave gaps and the generated IB is corrupt. Land the count function and the emit
  topology as one contract (Phase 3 defines it, Phase 4 must not deviate); the exact-packed-counts test is
  the guard.
- **Worst-case reservation pins VRAM.** `factor_cap² × base_prims × MAX_FRAMES_IN_FLIGHT` for VB + IB
  (plus AS + scratch in Phase 7) is large at a high cap; the factor cap, global ceiling, and the new
  shrink/reclaim path bound it, but peak-hold pressure is real and the reclaim path is a change to a
  shared pool that must respect fence safety exactly or it frees live memory.
- **Angular-metric determinism.** The arc metric must be computed identically regardless of endpoint
  order in the edge record (swap-invariant), or an edge could get order-dependent values across rebuilds;
  because the value is stored once per unique edge this cannot crack a *single* frame, but an
  order-sensitive metric would jitter frame to frame — pin a symmetric formulation (e.g. sort the two
  endpoints by index before combining, or use only order-independent operations).
- **Shadow-LOD compromise.** With the optional light-proximity term off, shadow maps of far casters near
  lights are under-tessellated; this is accepted and documented, but if it proves visible the term must be
  enabled without ever becoming per-light (which would break the single-shared-factor invariant).
- **Bucketing coarseness vs BLAS sharing.** Too few LOD buckets makes distinct instances share a BLAS
  whose factor is wrong for them (visible LOD mismatch); too many defeats the sharing Phase 7 needs for a
  populated scene. The `log2` quantization granularity is a tuning knob the Phase-9 control command should
  expose.
- **Budget down-scale vs geomorph.** A global factor down-scale changes integer factors and thus the
  geomorph remainder mid-motion; ensure the down-scale is temporally smoothed (hysteresis) so budget
  pressure does not itself become a motion-vector spike Phase 5 must then absorb.
