# Phase 3 — MDF 3-mip prefilter + coarse coverage

**Status:** COMPLETED

Implemented on the **host** side of the Phase 2 bake (which signs + compacts the readback
seed volume on the CPU — there is no `sdf_finalize.slang`/`sdf_downsample.slang` GPU finalize,
so the prefilter lives where the dense field already does, in `geometry/src/sdf.rs`): SDST
bumped to v3 with a 112-byte header carrying `mip_count` + `coverage_dims`; `Sdf` gains a
coarse coverage volume (min-|d| per brick block) and derives the 3-level prefiltered atlas via
a conservative min-|d| reduction chain (`atlas_image_data_mip`); `from_dense_field` computes
coverage; round-trip + conservative-lower-bound unit tests (mips + coverage vs the fine field
on sphere/box). Rendering: `Image3D::new` takes a mip count; `GpuSdf`/`GpuSdfParts` carry the
mipped atlas + coverage image + `mip_count`; the SDF sampler is LINEAR mipmap mode; bindless
binding 3 is the coverage `Texture3D` array (claimed/written/seeded with the atlas + indirection
at the same slot); `create_and_upload_sdf_image` uploads a mip chain; `upload_sdf` builds 3
atlas mips + coverage. `sdf.slang` replaces `sdfDistance` with `sdfSample(wp, count, footprint)`
— cone-footprint mip-select (`lod = log2(footprint / voxelSize)`, `SampleLevel(uvw, lod)`),
coverage-aware step (conservative leap across empty blocks) + openness early-out — consumed by
`sdfSkyOcclusion`/`sdfReflectionOcclusion`; `render_scene.rs` sets `params.w = mip_count`. GPU
bake test extended (mip-count assertions, validation-clean with the new image views + binding).

## Goal / Context

After Phase 2 each mesh carries a sparse SDST v2 field — an `R32_UINT` indirection volume and an
`R16_SNORM` brick atlas — sampled by a single trilinear tap in `sdf.slang::sdfDistance`. That field
is now dense enough to resolve real geometry, but a single-resolution distance field still has two
problems the cone trace exposes:

- **Aliasing at range.** A sky-occlusion cone widens with march distance `t`. Far along the cone its
  footprint covers many voxels, but `sdfDistance` keeps point-sampling the finest field, so a single
  voxel of high-frequency surface detail aliases into the occlusion ratio `d / (coneHalfAngle * t)`.
  This is the moiré/zebra failure mode the trace is most prone to on flat far surfaces — the field
  error has to be low-passed *before* it feeds the occlusion ratio, not after (a screen-space denoiser
  cannot recover a frequency the trace already aliased).
- **Wasted steps through empty space.** The sphere-march clamps its step to `clamp(d, kStepFloor, …)`.
  Inside a large empty brick the field still reports a bounded distance, so the march takes many small
  steps where one big leap would do, and it has no cheap "this whole region is open, stop" signal.

UE's distance fields solve both by baking a prefiltered mip chain (`NumMips`, default 3) plus a
coarse coverage/min-distance volume, and selecting the mip by the cone footprint at the march point.
This phase bakes those two extra products (cheap on the GPU now that the JFA result is already on
device from Phase 2) and rewrites the trace to use them. It is anti-aliasing and a per-step cost cut;
it also lays the groundwork the GDF composite in Phase 4 reads (coarse coverage is what lets a cascade
cheaply reject empty MDF regions).

Honest scope, per the design source of truth: this remains **cost-neutral on Sponza** (one giant
mesh, the prepass cost is unchanged until Phase 6) — the wins here are alias-free far occlusion and
fewer march steps per cone, which compound on modular content and feed the GDF. Do not water it down:
the destination is the full UE5-style prefiltered field, three real mips and a real coverage volume,
not a single blurred level.

## Work (ordered steps)

1. **Bake the 3-mip prefiltered distance, on the GPU, after the Phase 2 JFA finalize.** Extend the
   Phase 2 bake (`sdf_finalize.slang` + the bake driver in the rendering crate that runs
   `sdf_voxelize → sdf_jfa → sdf_finalize`) with a downsample pass that produces `NumMips = 3` of the
   signed field. Mip 0 is the Phase 2 fine field; each coarser mip halves the voxel resolution. The
   correct reduction for a *distance* field is **not** a box average of the encoded snorm — averaging
   signed distances across a sign change smears the surface. Filter the field as a band-limited
   distance: take the min of |d| with the sign reconciled from the contributing fine voxels (a
   conservative "nearest surface within this coarse voxel" estimate), so a coarse voxel reports a
   distance that is a valid lower bound for everything it covers. Write a new
   `sdf_downsample.slang` compute pass (one dispatch per coarser mip, reading the previous mip,
   writing into the next brick-atlas mip level). Keep the bake GPU-only and in the ms range.

2. **Bake a coarse coverage / conservative-distance volume per MDF.** Add one low-resolution volume
   per mesh (one texel per N³ block of the fine grid, e.g. one per brick or one per 2 bricks) storing
   the *conservative distance to the nearest surface anywhere in that block* — the minimum |d| over
   the block, as `R16_SNORM` in the same encode clamp as the field. This is the empty-space oracle:
   where coverage reports a large distance the whole block is open and the march can leap across it or
   early-out the cone. Produce it in the same downsample dispatch chain (it is just the coarsest
   reduction taken further), writing a separate small `Image3D`.

3. **Carry the new locators on `SdfInstance`.** Phase 2 already restructures `SdfInstance`
   (`gpu_types.rs` / the `struct SdfInstance` in `sdf.slang`) to carry the indirection-volume and
   brick-atlas bindless slots. Extend it with: the mip count (3), the coverage-volume bindless slot,
   and the coverage volume's local→texel mapping (it can reuse `local_min`/`local_max` since it spans
   the same grid, so only the slot + a mip-base-distance scalar are new). Pack these into the reserved
   `w` lanes of the existing `vec4`-class fields rather than growing the struct where possible; if a
   lane is genuinely needed, grow the struct by one 16-byte block and **update the
   `size_of::<SdfInstance>()` static assertion** in `gpu_types.rs` and the matching layout comment in
   `sdf.slang` together (the two must stay in lockstep — std430).

4. **Select the mip by cone footprint in `sdf.slang::sdfDistance`.** Give `sdfDistance` (and its
   callers `sdfSkyOcclusion`, `sdfReflectionOcclusion`) the cone footprint radius at the sample —
   `coneHalfAngle * t` in world units. Convert that to a continuous LOD: `lod =
   log2(footprint / voxelSize)`, clamped to `[0, NumMips-1]`, and pass it to `SampleLevel` instead of
   the fixed `0.0`. A widening cone now reads a coarser, pre-low-passed mip, so the field detail is
   band-limited *before* the `saturate(d / (coneHalfAngle * t))` occlusion ratio — this is the core
   anti-alias fix. Sample with the existing linear sampler so the fractional LOD trilerps between mips
   (trilinear-within-mip already; this makes it quadrilinear across the chosen pair). Keep the
   brick-border continuity from Phase 2 valid per mip (the downsample must preserve the shared-border
   convention at every level, or seams reappear at range).

5. **Use coverage for empty-space skip and early-out.** Before (or alongside) the per-step brick tap,
   sample the coverage volume. Where coverage reports a distance far larger than the current cone
   footprint, take a big step (advance by the coverage distance, not the clamped fine `d`) — empty
   space is crossed in one leap. When a cone's accumulated occlusion is already ~1 (fully open) and
   coverage says the remaining march is all open, **break** the cone loop early. This replaces the
   blunt `clamp(d, kStepFloor, kMaxDist * 0.25)` step heuristic with a coverage-aware step: fine steps
   near surfaces, coarse leaps through voids. Net effect is fewer taps per cone at equal quality.

6. **Allocate the mip chain + coverage volume on the GPU side.** In `resources.rs`, the Phase 2 brick
   atlas `Image3D` must be created with `mip_levels = 3` (today `Image3D::new` hardcodes
   `mip_levels(1)` and a `level_count: 1` view — generalize it to take a mip count, threading it
   through `GpuSdfParts` / `GpuSdf`). Add a second small `Image3D` for the coverage volume. In
   `descriptors.rs`, the SDF sampler currently uses `mipmap_mode(NEAREST)` and is built for a
   single-mip field (`create_sdf_sampler`) — switch it to `LINEAR` mipmap mode with no LOD clamp so
   fractional `SampleLevel` LODs blend, and register the coverage volume as a bindless 3D resource
   (reuse the existing bindless 3D table + free-list machinery; it needs no new descriptor set, just a
   claimed slot like the atlas/indirection volumes).

7. **Plumb the coverage slot through upload.** The Phase 2 upload path (`upload.rs`) that writes the
   brick atlas + indirection volume into bindless slots and assembles `GpuSdf` must also claim a slot
   for, and write, the coverage volume, and the bake driver must hand the coverage volume + the 3-mip
   atlas back through `GpuSdfParts`. `render_scene.rs` (which builds the `SdfInstance` list each frame)
   sets the new locator/mip fields on each instance.

## Files

- `engine/assets/shaders/sdf.slang` — `sdfDistance` (mip-select by cone footprint, coverage-aware
  step), `sdfSkyOcclusion` + `sdfReflectionOcclusion` (pass footprint, coverage early-out), the
  `SdfInstance` struct + its layout comment (mip count + coverage slot).
- `engine/assets/shaders/sdf_downsample.slang` — **new**: GPU compute downsample producing mips 1..2
  and the coverage volume from the Phase 2 fine field (conservative min-|d| reduction, border-aware).
- `engine/assets/shaders/sdf_finalize.slang` — **extend (from Phase 2)**: hook the downsample chain
  after sign/brick-compress so the bake emits all three mips + coverage in one pass sequence.
- `engine/crates/geometry/src/sdf.rs` — `SDF_FORMAT_VERSION` bump; `SdfHeader` gains a mip count +
  coverage-dims field (kept 16-byte aligned); `Sdf` round-trip + the analytic unit-tests extend
  to assert the coarse mips are conservative lower bounds of the fine field (`MeshBvh` oracle).
- `engine/crates/rendering/src/resources.rs` — `Image3D::new` takes a mip count (drop the hardcoded
  `mip_levels(1)` / `level_count: 1`); `GpuSdf` / `GpuSdfParts` carry the 3-mip atlas + the coverage
  `Image3D` + its bindless slot.
- `engine/crates/rendering/src/descriptors.rs` — `create_sdf_sampler` to `LINEAR` mipmap mode;
  claim/write a bindless slot for the coverage volume (reuse the SDF bindless table + free-list).
- `engine/crates/rendering/src/gpu_types.rs` — `SdfInstance` mip-count + coverage-slot fields; keep
  the `size_of::<SdfInstance>()` static assertion in sync with `sdf.slang`.
- `engine/crates/rendering/src/upload.rs` — bake driver emits 3 mips + coverage; claim/write the
  coverage bindless slot; assemble `GpuSdf` from the extended `GpuSdfParts`.
- `engine/crates/assets/src/render_scene.rs` — set the new `SdfInstance` mip/coverage fields per
  frame.

## Reuse

- The Phase 2 GPU bake chain (`sdf_voxelize`/`sdf_jfa`/`sdf_finalize` + its rendering-crate driver) —
  extend it with the downsample pass; do not stand up a second bake path.
- The bindless 3D atlas + free-list + slot allocator in `descriptors.rs` (`claim_sdf_slot`,
  `write_sdf_texture`, `seed_all_sdf_textures`, `sdf_free_list`) — the coverage volume is just another
  claimed slot in the same table, not a new descriptor set.
- `Image3D` (`resources.rs`) — generalize its mip count rather than introducing a new image wrapper;
  the brick atlas becomes a 3-mip `Image3D`.
- `MeshBvh::nearest_signed_distance` (`geometry/src/picking.rs`) — the SDST format unit-test oracle;
  reuse it to assert each coarser mip and the coverage volume are conservative bounds.
- The cone-trace structure in `sdfSkyOcclusion` — keep the cone set + accumulation from Phase 1/2;
  this phase only changes the *sampling* (which mip, how big a step), not the cone geometry.

## Risks (phase-specific)

- **Distance-field downsample is not a box filter.** Naively averaging snorm across a sign boundary
  smears the surface and makes coarse mips lie about where geometry is. Use a conservative min-|d|
  reduction with reconciled sign; validate against the `MeshBvh` oracle that mip *k* is a lower bound
  for the fine field over each coarse voxel. Getting this wrong reintroduces blobs at range instead of
  removing aliasing.
- **Brick-border continuity per mip.** Phase 2's seam-free trilinear depends on the shared 1-voxel
  border in each 8³ brick. The downsample must reproduce that border at every mip or seams reappear
  exactly where the mip-select kicks in (at range), which would look like a new banding artifact.
- **LOD selection over a sparse brick atlas.** `SampleLevel` with a fractional LOD must stay within
  the brick's valid mapped region per mip; an off-by-one in the per-mip indirection or UVW remap reads
  a neighbour brick's data. Empty bricks must resolve to the coverage path at every LOD, not to a
  garbage atlas tap.
- **Coverage step over-stepping.** A coverage-driven leap that overshoots a thin occluder skips the
  surface entirely (the leak failure). Bound the leap by the coverage *distance* (a conservative lower
  bound on nearest surface), never by a guess, and fall back to fine stepping as the footprint
  approaches a surface.
- **Memory.** Three mips add ~1/8 + 1/64 over the fine atlas (≈ +14%) plus a small coverage volume —
  negligible against the sparse-brick budget, but confirm the atlas allocation accounts for all mip
  levels (a mip chain on a 3D image is non-trivial in bytes).

## Verification

- **Build / lint:** `just engine` clean; `just prepare-for-commit` (cargo fmt + clippy `-D warnings`)
  with zero new warnings; `cargo run -p xtask -- shaders` compiles `sdf.slang` + the new
  `sdf_downsample.slang` to SPIR-V.
- **Unit tests (geometry):** the `sdf.rs` round-trip test covers the new header fields and the 3-mip
  + coverage payload; a new test asserts, against `MeshBvh::nearest_signed_distance` on the unit
  sphere/box, that each coarser mip and the coverage volume are conservative lower bounds of the fine
  field (no coarse voxel claims more open space than truly exists). Run isolated
  (`cargo test -p saffron-geometry sdf`) — the e2e sandbox is flaky.
- **Visual on the NVIDIA path** (`just run`, Sponza): the far-range moiré/zebra on flat surfaces is
  gone (the mip select low-passes it) while near-field contact occlusion stays as sharp as Phase 2 —
  i.e. anti-aliasing without softening contact. No new seams at the distance where the trace switches
  mips. Sweep the camera: the field must not shimmer between mip levels (fractional LOD should blend).
- **Perf:** profiler `sdf-ao` step count/time at equal visual quality — fewer taps per cone from the
  coverage-aware stepping; the prepass time is ≤ the Phase 2 number (cost-neutral-or-better is the
  bar here, since the absolute prepass cost only goes away in Phase 6).
- **Headless smoke:** `just run-engine-headless` exits clean with a validation-clean log (the new
  bindless coverage slot + 3-mip image views are correctly described — no Vulkan validation errors on
  the SDF descriptor writes or the mipped sampler).
