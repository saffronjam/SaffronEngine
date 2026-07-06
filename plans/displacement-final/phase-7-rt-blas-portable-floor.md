# Phase 7 — RT BLAS portable floor: indirect / worst-case build, scratch pool, instance bucketing, distinct skinned vs tessellated policies

**Status:** NOT STARTED

Make the ray-traced surface **identical to the rasterized one**, cross-vendor, with nothing NV-only
load-bearing. Phase 4 writes the diced, welded, displaced geometry into the per-frame
`TransientResources` vertex + **generated index** buffers; this phase builds the BLAS over *those exact
buffers* so shadows, reflections, AO, GI, and ReSTIR trace the geometry that was rasterized — the whole
point of the compute→buffer spine. Three RT gaps that `plans/displacement/`'s C1 never faced (because its
displacement was 1:1 fixed-topology and refit for free) are resolved head-on: a **GPU-decided primitive
count** feeding a **CPU-sized build**, **worst-case sizing without per-frame thrash**, and the
**instancing blow-up** that screen-space factors force. The BLAS cache splits into two distinct types with
opposite refit policies.

**Scope:** `saffron-rendering` (RT acceleration structures — `rt.rs`), consuming the Phase-1
`accelerationStructureIndirectBuild` probe and the Phase-3 worst-case bound / LOD buckets.
**Depends on:** [`phase-4-dice-displace-weld-emit.md`](phase-4-dice-displace-weld-emit.md) (the transient
tessellated VB/IB + `DeformedRtInstance` carrying the generated index stream, variable counts, a policy
tag, and a bucket key). Reuses Phase 1 (probe) and Phase 3 (`maxPrimitiveCount`, LOD buckets, the
per-instance budget).

## Goal

Ray-traced shadows/reflections/GI over **true displaced geometry** on AMD / Intel / NVIDIA / llvmpipe —
the portable analogue of RTX Mega Geometry — with a per-instance refit-vs-rebuild policy that never
silently pays a full BUILD on a pure-skinned mesh and never thrashes AS/scratch allocation on per-frame
factor wobble.

## Why the current path cannot carry a tessellated instance

`rt.rs::plan_skinned_blas_refits` (line 404) builds each `DeformedRtInstance`'s BLAS *once* (`SkinnedBlas
{ accel, built: false }`, line 42) then does an in-place `MODE_UPDATE` refit every frame after
(`record_tlas_build_plan`, line 790, picks `UPDATE` when `op.update`). Vulkan `MODE_UPDATE` requires
**byte-identical topology**: the same index buffer, the same `triangle_count`, the same `max_vertex`. The
skinned path satisfies this because the deformed slice is a 1:1 remap of the base mesh and
`index_data = inst.mesh.index_buffer()` (line 430) — the base mesh's own static index stream. A
tessellated instance mints a **different vertex/triangle count and a different index buffer every frame**,
so `UPDATE` is illegal for it; it must full-`BUILD` each frame over the generated index stream. That is
the load-bearing change here.

## Build plan (grounded in `rt.rs`)

### 1. Split the BLAS cache into `SkinnedBlas` (UPDATE) and `TessellatedBlas` (BUILD)

- Keep `SkinnedBlas { accel, built }` and `FrameRt.skinned_blas: HashMap<u64, SkinnedBlas>` (lines 42,
  59) **unchanged** — pure skinning/morph (fixed topology) stays on the create-once-then-`UPDATE` fast
  path. `plan_skinned_blas_refits` keeps its `PREFER_FAST_TRACE | ALLOW_UPDATE`, first-sight `BUILD`, then
  in-place `UPDATE` gate (lines 437–499).
- Add a sibling `TessellatedBlas { accel: Arc<AccelerationStructure>, worst_case_prims: u32, as_size:
  vk::DeviceSize }` and `FrameRt.tessellated_blas: HashMap<TessKey, TessellatedBlas>`. There is **no
  `built` flag** — a tessellated BLAS is *always* a full `MODE_BUILD` (`ALLOW_UPDATE` dropped), so
  `record_tlas_build_plan` never selects `UPDATE`/`src_accel = dst` for it.
- `TessKey` is `(mesh_id, lod_bucket)` (not the entity uuid) so **bucketing** (§5) collapses same-bucket
  instances onto one shared BLAS. `clear_skinned_blas` (line 282) grows to clear both maps on scene reset.
- **Policy is chosen by whether the instance is tessellated, not by whether it is skinned.** A
  skinned **and** displaced instance reads the skinned deformed ring as its dice base (Phase 4's input
  binding) and therefore emits variable topology into the transient buffer — it goes through the
  `TessellatedBlas` `BUILD` path and **necessarily loses skinning's cheap `UPDATE`**. State this in the
  policy selector (§7) so it is a deliberate, visible cost, never a silent regression on a pure-skinned
  mesh. The selector reads the Phase-4 `DeformedRtInstance` policy tag: `Skinned → SkinnedBlas`;
  `Tessellated` (incl. skinned+displaced) `→ TessellatedBlas`.

### 2. GPU count → a CPU-sized build (fix #1)

`get_acceleration_structure_build_sizes` (line 445) needs `maxPrimitiveCount` **at record time** and the
`VkAccelerationStructureBuildRangeInfoKHR` needs a `primitiveCount`, but the real count is decided on the
GPU by Phase 3's prefix-sum. Two explicit paths, selected on the Phase-1 `Capabilities` probe (add
`accel_indirect_build: bool` beside `mesh_shader_supported`, line 85, filled in `probe_optional_features`
from `PhysicalDeviceAccelerationStructureFeaturesKHR::acceleration_structure_indirect_build`, line 1068):

- **Indirect (probe set).** Record via `accel::Device::cmd_build_acceleration_structures_indirect`
  (`vkCmdBuildAccelerationStructuresIndirectKHR`): pass the worst-case `maxPrimitiveCount` to the *size*
  query (sizing is still CPU/worst-case, see §3) but drive the *build* from a GPU-resident
  `VkAccelerationStructureBuildRangeInfoKHR` — its `primitiveCount` is the exact GPU-emitted triangle
  count. Phase 3 emits that range struct into a small transient buffer alongside its indirect-draw args
  (one per `(mesh, bucket)`); this phase passes its device address + stride as `indirect_device_addresses`
  / `indirect_strides`. The build then processes **only the real triangles**.
- **Fallback (probe clear — lavapipe / software RT).** Record via the existing
  `cmd_build_acceleration_structures` (line 834) with the range `primitiveCount` set to the CPU
  **worst-case** `maxPrimitiveCount`. Phase 4 packs the real triangles at the front of the worst-case-sized
  index buffer and **pads the unused tail to degenerate (zero-area) triangles** (all three indices equal,
  e.g. `(0,0,0)`), which the AS builder discards. This wastes build work on the padded tail — that is the
  documented cost of the portable floor, not a hand-wave.

Phase 4 pads the tail unconditionally (one kernel, NO-LEGACY); this phase's only branch is which of the
two record calls runs and, for the indirect path, whether the GPU range buffer or the CPU worst-case count
supplies `primitiveCount`.

### 3. Worst-case AS + scratch sizing, reactive only on a bound change (fix #2-RT)

- Size both the AS backing store **and** the build scratch to the per-instance **worst-case** bound —
  `get_acceleration_structure_build_sizes` called with `maxPrimitiveCount = factor_cap² × base_prims` for
  that `(mesh, bucket)` (Phase 3's worst-case reserve), never the per-frame emitted count. Both build
  paths in §2 use the same worst-case size query, so the AS is large enough regardless of the frame's
  actual count.
- `TessellatedBlas` caches `worst_case_prims` + `as_size`. Recreate the AS (via
  `AccelerationStructure::create`, line 457) **only when the worst-case bound itself changes** — the mesh's
  factor cap or the bucket quantum moved — never on per-frame count wobble. A close-up high-factor frame
  and a distant low-factor frame of the *same bucket* reuse one AS. This is the direct fix for
  alloc/free thrash and heap fragmentation: no reactive resize keyed to the fluctuating GPU count.

### 4. Per-build scratch pool so BUILDs overlap (fix #3)

`ensure_blas_scratch` (line 648) today owns **one** shared `blas_scratch` sized to the `max` over all ops
(line 486), and `record_tlas_build_plan` serializes consecutive builds with `accel_scratch_barrier()`
between them (line 792–799) because they all write the same scratch (WAR/WAW). For many per-frame
tessellated `BUILD`s that serialization is a throughput cliff.

- Replace the single shared scratch with a **per-build scratch pool**: one contiguous scratch buffer
  partitioned into per-op slices at distinct offsets aligned to
  `minAccelerationStructureScratchOffsetAlignment` (query from
  `PhysicalDeviceAccelerationStructurePropertiesKHR`). Each `BlasBuildOp` carries its own scratch offset;
  because the slices do not alias, the inter-op `accel_scratch_barrier` is **dropped** for pooled ops and
  the driver may overlap them. Keep the single-region serialized scratch for the small `SkinnedBlas`
  `UPDATE` ops (they are cheap and few).
- Alternatively batch multiple `(build_info, range)` pairs into **one**
  `cmd_build_acceleration_structures` call (Vulkan permits an array of build infos), each pointing at its
  own scratch slice — the modern equivalent, and the natural shape for the indirect path.
- **Budget the builds/frame.** Cap the number of distinct `(mesh, bucket)` tessellated `BUILD`s per frame
  (the Phase-3 displaced-instance budget); the pool is sized to that cap × worst-case scratch. Instances
  over budget fall to a coarse proxy (§6).

### 5. Instance bucketing + a hard displaced-instance budget (fix #4)

Screen-space edge factors make hardware instancing impossible: N displaced instances would want N
tessellated buffers + N BLAS. Bucketing recovers it.

- Phase 3 quantizes each instance's per-instance factor into a **LOD bucket**; Phase 4 tessellates **one
  representative** per `(mesh, bucket)` into a shared transient VB/IB. This phase builds **one**
  `TessellatedBlas` per `(mesh, bucket)` (keyed by `TessKey`) and references it from **many** TLAS
  instances — one `VkAccelerationStructureInstanceKHR` per real entity, each with its own
  `world_transform`. The per-frame TLAS ring already packs one instance per referenced draw and
  full-rebuilds every frame (`prepare_tlas`, line 514), so it absorbs the many→one BLAS sharing with no
  change — reuse it.
- The `DeformedRtInstance` (draw_list.rs:273, extended in Phase 4) carries the `TessKey` so the planner
  builds each bucket's BLAS once and emits a TLAS instance per entity. The **hard displaced-instance
  budget** (Phase 3) caps distinct buckets built; without it a forest of thousands is infeasible.

### 6. Source geometry from the generated index buffer; wire the build ops

- In the new `plan_tessellated_blas_builds` (the `TessellatedBlas` sibling of `plan_skinned_blas_refits`),
  source `vertex_data` from the transient tessellated **vertex** buffer slice and `index_data` +
  `triangle_count` from the Phase-4 **generated index** buffer — **not** `inst.mesh.index_buffer()`. Both
  are device addresses into `TransientResources` buffers that already carry
  `SHADER_DEVICE_ADDRESS | ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR` (Phase 3's usage union).
  `triangle_geometry` (line 972) is reused verbatim (`R32G32B32_SFLOAT` positions at offset 0, `UINT32`
  indices, `OPAQUE`).
- Add a `BlasBuildOp` beside `BlasRefitOp` (line 748): `dst`, `vertex_data`, `vertex_stride`, `max_vertex`
  (= worst-case vertex count − 1), `index_data`, `max_prim_count` (worst-case, for the fallback range and
  the size query), `indirect_range_addr: Option<vk::DeviceAddress>` (Some on the indirect path), and a
  `scratch_offset`. `TlasBuildPlan` (line 772) grows a `tess_ops: Vec<BlasBuildOp>` recorded before the
  TLAS build. `record_tlas_build_plan` records the tessellated `BUILD`s (indirect or worst-case per §2)
  with pooled scratch (§4), then the existing `accel_build_to_build_read_barrier` (line 839) hands them to
  the TLAS build unchanged.
- The `tlas-build` pass's `AccelStructBuildRead` on the transient resource already orders the whole build
  after the Phase-4 emit pass (the graph derives the compute-write→AS-build barrier) — reuse it; no new
  barrier code.
- **Keep-current:** extend the `skinned_blas_count` inspect counter (line 111) with a `tessellated_blas_count`
  so the `sa`/control surface can show how many tessellated BUILDs ran this frame (the full tessellation
  quality command is Phase 9).

### 7. Cost mitigations (documented optimizations over the correctness floor)

- **Static-cache where factors are temporally stable.** When Phase 3 flags an instance's factors +
  transform unchanged, skip re-dicing and reuse the previous BLAS. This requires the tessellated output to
  live in a **retained** (non-transient) slice rather than the per-frame `TransientResources` buffer that
  Phase 4 overwrites — layer it on the Phase-3 shrink/reclaim path, and treat it as an optimization, not
  part of the floor.
- **Hysteresis/clamp on edge factors** (Phase 3) cuts rebuild churn across the LOD-bucket boundary.
- **Coarse-proxy BLAS for far LOD.** Below a screen-size threshold — or when over the per-frame build
  budget (§4) — reference the static base-mesh BLAS already built at upload (`GpuMesh.blas`,
  `record_mesh_blas_build`, line ~901) instead of a tessellated one. The silhouette error is sub-pixel at
  that distance.

## Watertightness / RT correctness handling

The BLAS is built from the **exact welded VB/IB the raster passes read** (the same transient resource), so
rays trace the rasterized surface — watertightness is a *construction property inherited* from Phases 2/3/4
(identical fractional edge factor, UV-seam value agreement, welded direction + seam-consistent tangent, one
pinned edge-continuous base), not re-derived here. This phase's obligation is only to **not re-introduce
leaks**:

- Degenerate pad triangles must be truly zero-area (three identical indices) so the AS discards them; they
  must never yield a spurious hit that would darken a shadow or occlude a reflection.
- Build over the generated index buffer verbatim — never a separate/coarser topology — so the BLAS
  triangles are exactly the rasterized ones (no divergence, no self-shadow acne).
- Preserve the `OPAQUE` geometry flag (`triangle_geometry`) so no any-hit is needed.
- Place each TLAS instance with the **same** `world_transform` convention Phase 4 chose for its emitted
  space (mesh-local displaced verts → node world matrix; skinned+displaced already world-space →
  identity), so the traced position matches the rasterized one. Normals/tangents are a shading concern
  (Phase 4 emits the welded frame into the VB); the BLAS reads positions only, and hit-shader
  position-fetch consumers read the same VB, so they stay consistent.

## Verification

- **CPU unit tests (the gate — no GPU needed, fix #21):**
  - Build-path selection: `accel_indirect_build == true` ⇒ indirect record + GPU range `primitiveCount`;
    `false` ⇒ worst-case record + degenerate-padded tail.
  - Worst-case sizing math: the size query is called with `maxPrimitiveCount = factor_cap² × base_prims`,
    and the AS is recreated **only** when `worst_case_prims` changes, not on a per-frame count change.
  - Bucketing: K instances sharing one `(mesh, bucket)` produce **one** `BlasBuildOp` and **K** TLAS
    instances; the displaced-instance budget caps distinct builds and routes the overflow to the coarse
    proxy.
  - Policy per instance type: pure-skinned/morph ⇒ `SkinnedBlas` `UPDATE`; tessellated **and**
    skinned+displaced ⇒ `TessellatedBlas` `BUILD` (asserting the skinned+displaced case pays BUILD, not a
    silent UPDATE).
  - Scratch pool: per-op offsets are `minAccelerationStructureScratchOffsetAlignment`-aligned and
    non-overlapping; no inter-op scratch barrier is emitted between pooled ops.
- **GPU-with-eyes (available GPU only):** the RT shadow **and** reflection silhouette of a displaced rock
  matches its raster silhouette — no self-shadow acne, no flat mirror, no light leaking through a seam —
  under camera motion, with a validation-clean log. Confirm on the NVIDIA card via `just run` /
  `just run-engine-headless`.
- **Deferred, not proven:** real-GPU cross-vendor validation on AMD/Intel — no hardware GPU in the toolbox
  and limited/slow lavapipe RT. The floor is architected portable and nothing NV-only is wired here (CLAS
  is Phase 8, behind a seam); this is **not** a claim that the floor is proven on those vendors.

## Risks

- **The GPU-count→CPU-build constraint is the single hardest seam.** `accelerationStructureIndirectBuild`
  is not universal (lavapipe/software RT likely lacks it), so the true cross-vendor floor is
  build-at-worst-case + degenerate-pad, which spends BVH build work on padded triangles. The indirect path
  is a probed fast lane, not a guarantee.
- **Every tessellated instance full-`BUILD`s every frame** (variable topology forbids `UPDATE`), including
  skinned+displaced which loses skinning's cheap refit. Without §5 bucketing + the §4 build budget a
  populated scene is infeasible; even with them, per-frame build cost, scratch throughput, and VRAM are
  real and must be budgeted.
- **Worst-case AS+scratch sizing under grow-only `TransientResources`** holds the session peak across
  VB+IB+scratch+AS × `MAX_FRAMES_IN_FLIGHT`. The factor cap + global budget + the Phase-3 shrink/reclaim
  path bound it, but peak-hold pressure remains, and the scratch pool is a new allocation to size against
  the build budget.
- **Static-cache vs the per-frame transient buffer** — reusing a BLAS across frames needs a *retained*
  tessellated slice, which fights the per-frame `TransientResources` model; it is an optimization layered
  on the floor, and getting its invalidation wrong (reusing a stale BLAS after factors moved) reintroduces
  silhouette mismatch.
- **Cross-vendor validation is environment-limited** — only CPU unit tests (sizing/policy/selection) can
  gate here; AMD/Intel parity is deferred.
