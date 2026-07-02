# Phase 2 — GPU jump-flood MDF bake + sparse bricks + resolution scale

**Status:** COMPLETED

Implemented + tested (engine side): SDST v2 sparse format (indirection volume + brick
atlas) in `geometry/src/sdf.rs`; the GPU jump-flood bake (`sdf_voxelize`/`sdf_jfa` compute
shaders → seed readback → host sign + CPU brick-compaction) on `Uploader`; the sidecar
`assets/cache/<hash>.sdf`; the densified grid (voxel-density × `resolution_scale`, longest
axis ≤ 128); the two-image `GpuSdf` + second bindless `Texture3D<uint>` indirection array;
the brick-tap `sdfDistance` in `sdf.slang`; the v2 `SdfInstance`; and the removal of the CPU
import bake + the `SignedDistance` chunk + dense v1 (NO LEGACY). Unit tests: SDST v2
round-trip / bad-magic / truncation / sphere+box analytic / sign / cross-brick continuity
(geometry); GPU bake validation-clean + whole-grid sign-vs-`MeshBvh`-oracle on a convex box
**and a concave L-prism** + slot reclaim in both arrays + sidecar round-trip (rendering, on
llvmpipe).

The GPU owns the nearest-surface search — `sdf_voxelize` (seed scatter) → `sdf_jfa`
(jump-flood) — and reads back the final seed volume (closest surface point + packed
nearest-triangle normal). Brick compaction *and the sign* run on the host after that
readback: the sign is an outside-flood (grid-border voxels are outside; "outside" floods
6-connected through every voxel the surface band does not block) cross-checked by the seed
normal in the band, per the spec's settled decision. This is the connected-component fill
the spec mandates — exact and cheap on the CPU, and (unlike a single nearest-face normal) it
will not read a concave-corner interior voxel as outside, the sky-leak the flood was chosen
to prevent. `bake_sdf` first drops degenerate (zero-area/sliver) triangles, matching the
`MeshBvh` oracle, so a poisoned (NaN) seed normal can never reach the sign pass. There is no
third GPU pass: the dense signed `sdf_finalize` shader (single-normal sign) is removed (NO
LEGACY) — it implemented a sign rule that diverged from its own oracle at edges/corners.

Remaining: the `saffron-control` bake-stats / `resolution_scale` command (item 7) — deferred
because the control command registry is mid-flux from concurrent work (the
`asset_commands_register_in_manifest_order` frozen test is already red from `export-app`/
`get-stores`/`set-stores` additions that are not part of this change), and the per-mesh
`resolution_scale` default stays `1.0`; and the visual NVIDIA-path check (needs a display).

## Goal / Context

The per-mesh signed distance field is too coarse for Sponza-as-one-mesh. Sponza imports as a single
262k-tri mesh spanning ~37 m, and the current bake caps the longest grid axis at 64
(`SDF_MAX_AXIS` in `engine/crates/geometry/src/sdf.rs`), which is ~58 cm voxels. Trilinear
reconstruction of a 58 cm field on flat walls is exactly the smeary low-frequency "stain" blobs the
study identified. The CPU bake also re-freezes import: a 192-axis dense bake measured 125 s/core.

This phase fixes the blobs at the root by replacing the bake entirely:

- The bake becomes a **GPU jump-flood** run at mesh-upload time (ms-scale), not a CPU import-time
  pass. Import stays GPU-free.
- The field is densified — longest axis cap raised to ~128 (~19–29 cm voxels) via a voxel-density
  term and a per-asset `resolution_scale`.
- Storage becomes **SDST v2 sparse bricks** (indirection volume + brick atlas) so a 128–192-axis
  Sponza field stays in the low tens of MB instead of the ~14 MB/mesh a dense 192³ would cost.
- The CPU `compute_sdf_for_mesh` import bake and the dense SDST v1 format are removed (NO LEGACY).
  `MeshBvh::nearest_signed_distance` stays only as the format/sign unit-test oracle.

This phase is **cost-neutral on the per-pixel side** — it changes how the field is built and stored,
not how `sdf_ao.slang` consumes it. The visible win is that the densified MDF resolves wall blobs
into real geometry. The central new risk is **GPU sign correctness on non-watertight Sponza**: the
sign derived from the jump-flood must be validated against the CPU oracle.

## Settled decisions (do not re-litigate)

- Bake = GPU jump-flood (voxelize → JFA → sign → sparse 8³ bricks) at `GpuMesh`-build time, cached to
  a sidecar `assets/cache/<meshHash>.sdf`. Import is GPU-free; the container `SignedDistance` chunk
  goes away.
- SDST v2 = header + indirection volume (`R32_UINT`, one texel per brick → atlas brick base or
  `EMPTY`) + brick atlas (`Image3D R16_SNORM`, bricks of 8³ = 7 unique voxels + 1 shared border for
  seam-continuous trilinear).
- `sdfDistance`: world → local → voxel coord → brick coord → nearest indirection Load → in-brick UVW
  (with the border offset) → one trilinear tap. Empty bricks return a conservative coarse coverage
  distance (a big march step); the real coverage volume is Phase 3.
- Sign comes from a flooded outside-seed (grid-border voxels are outside), cross-checked by the
  nearest-triangle normal carried in the JFA seed.

## Work (ordered)

### 1. SDST v2 format in `engine/crates/geometry/src/sdf.rs`

- Bump `SDF_FORMAT_VERSION` to `2`. Replace `SdfHeader`/`SdfVolume` (the dense v1 grid) with the v2
  layout. The header carries: magic `SDST`, version, voxel `dims` (the logical fine grid), padded
  `bounds_min`/`bounds_max` (local space), `max_dist` (the `R16_SNORM` encode clamp), `brick_size`
  (8) and `brick_useful` (7), `indirection_dims` (= `ceil((dims - 1) / 7)` per axis), `atlas_dims`
  (the atlas extent in voxels), and `occupied_bricks`. Keep it `Pod`/`Zeroable` with an exact
  `size_of` static assert as v1 had.
- The in-memory `Sdf` (rename from `SdfVolume`) holds the header, the indirection volume
  (`Vec<u32>`, one entry per brick, `EMPTY = u32::MAX` or atlas brick base index), and the brick
  atlas (`Vec<i16>`, `occupied_bricks * 512` cells, X-fastest within each 8³ brick). `to_bytes` =
  header + indirection (`cast_slice::<u32>`) + atlas (`cast_slice::<i16>`); `from_bytes` validates
  magic/version and the two section lengths against the header (`Error::Truncated` / `BadMagic` /
  `UnsupportedVersion` / `BadLayout`, the existing `crate::error::Error` variants).
- Replace `distance_at` with a v2 `sample_voxel(x, y, z)` that resolves brick → indirection → atlas
  (returning the coverage clamp for an `EMPTY` brick) so the unit tests can read the field back.
- Delete `compute_sdf_for_mesh`, `axis_cells`, `SDF_MAX_AXIS`, and the v1 dense grid entirely.
  Keep a `#[cfg(test)]` CPU reference bake `bake_sparse_reference(positions, indices, dims)` that
  builds a v2 `Sdf` from `MeshBvh::nearest_signed_distance` (the oracle) — used only by the format,
  analytic, and sign tests; it is **not** on any production path.

### 2. GPU bake shaders (new in `engine/assets/shaders/`)

Three compute shaders, dispatched in sequence on a one-off command buffer at upload time. Resolution
(grid `dims`) is derived from a **voxel-density** constant (voxels per world metre) times the asset's
`resolution_scale`, with the longest axis capped at 128 to start (raisable). Grids are padded by the
existing `SDF_PAD_FRACTION` shell.

- `sdf_voxelize.slang`: seed pass. One thread per triangle (read the just-uploaded vertex + index
  buffers). For each triangle compute its voxel AABB and, for every covered voxel, the nearest point
  on the triangle; atomically keep the seed (closest surface position + triangle normal) with the
  smallest distance to that voxel centre. Output is a seed `Image3D` (closest-position +
  packed-normal, e.g. `R32G32B32A32_SFLOAT` storage). Voxels with no triangle stay `unseeded`.
- `sdf_jfa.slang`: jump-flood propagation, ping-pong over two seed volumes. Dispatched
  `ceil(log2(maxAxis))` times with step `k = N/2, N/4, … 1`; each voxel samples the 27 neighbours at
  offset `k`, keeping the seed whose stored surface position is nearest to this voxel centre.
- `sdf_finalize.slang`: sign + brick-compress. Per voxel: unsigned distance = `|centre − seed.pos|`;
  sign it by an outside-flood (border voxels seeded outside, propagated by the same JFA) **cross-
  checked** against `sign(dot(centre − seed.pos, seed.normal))`, encode `R16_SNORM` to `max_dist`.
  Then classify each 8³ brick over the 7-voxel stride: a brick whose voxels all saturate to `+max`
  is marked `EMPTY` in the indirection volume; otherwise allocate an atlas brick via an atomic
  counter, write its 7 unique voxels plus the 1-voxel border copied from the neighbouring brick's
  first plane (so trilinear is seam-continuous), and store the atlas base in the indirection texel.

These pipelines are compute PSOs the bake path owns (see step 4); their descriptor layouts bind the
storage seed/work `Image3D`s, the mesh vertex/index `StorageBuffer`s, the output indirection +
atlas storage images, and a small push-constant block (grid dims, bounds, `max_dist`, JFA step). Add
them to `cargo run -p xtask -- shaders`.

### 3. GPU bake orchestration in `engine/crates/rendering/src/upload.rs`

- Rewrite `upload_sdf` into a `bake_sdf` that, given the mesh's device buffers + grid dims +
  `resolution_scale`, allocates the transient seed/work `Image3D`s (reuse `crate::Image3D` with
  `STORAGE | SAMPLED`), records voxelize → JFA loop → finalize through the existing
  `with_one_off_commands` / `submit_and_wait` machinery (the same one-shot path `upload_sdf` uses
  today), and reads back the indirection + atlas to assemble an `Sdf` (v2) → device-local `GpuSdf`
  v2.
- The three bake compute pipelines + their descriptor pools/layouts live on `Uploader` (built in
  `Uploader::new`, alongside the existing `accel` dispatch), because the bake runs on the upload
  path — including the thumbnail worker's own `Uploader` — not the renderer's frame pipeline cache.
- Wrap the bake in a `tracing` span timing it (the "completes in ms, no import freeze" claim must be
  observable).
- Sidecar cache: add a `bake_or_load_sdf` step keyed by a content hash of the mesh
  (positions + indices). On a hit, read `assets/cache/<hash>.sdf`, `Sdf::from_bytes`, and upload
  directly (skip the GPU bake). On a miss, bake then write the sidecar bytes. Resolve the cache dir
  from the project/asset root used by `engine_asset_path` (`engine/crates/assets/src/load.rs`),
  creating `assets/cache/` on first write.
- `upload_default_sdf` + `seed_all_sdf_textures`: keep the "empty space" default, now a 1-brick v2
  field (a single `EMPTY`/far-positive brick) seeded into every unbound slot of **both** bindless
  arrays (atlas + indirection).

### 4. SDST v2 GPU resources

- `engine/crates/rendering/src/resources.rs`: extend `GpuSdf` / `GpuSdfParts` to hold two images +
  views — the indirection volume (`R32_UINT` 3D) and the brick atlas (`R16_SNORM` 3D) — plus one
  shared bindless slot index and the v2 metadata (bounds, `max_dist`, voxel `dims`,
  `indirection_dims`, `atlas_dims`). `Drop` frees both images/views and returns the one slot to the
  free-list. `create_sdf_image` gains an indirection variant (or a format parameter).
- `engine/crates/rendering/src/descriptors.rs`: add a second bindless 3D array alongside the existing
  atlas array (binding 1) — the indirection array (a new binding, `R32_UINT`, read by integer
  `Load`, no sampler). `claim_sdf_slot` returns one index used to write **both** arrays;
  `write_sdf_texture` writes the atlas view, a new `write_sdf_indirection` writes the indirection
  view at the same slot; `seed_all_sdf_textures` seeds both. Reuse the linear clamp `sdf_sampler` for
  the atlas; the indirection needs no sampler (texel `Load`).

### 5. Shader sampling — `engine/assets/shaders/sdf.slang`

- Add the indirection bindless array binding (mirroring `sdfTextures[256]` at set 0) and an `EMPTY`
  sentinel constant. Extend `SdfInstance` `params`/add a field for the brick locators
  (`indirection_dims`, `atlas_dims`, `voxel size`, `max_dist`, world scale).
- Rewrite `sdfDistance` to the brick tap: world → local → voxel coord (from `localMin`/`localMax` +
  `dims`) → brick coord = `floor(voxel / 7)`; `Load` the indirection at the brick; if `EMPTY`, return
  the conservative coverage distance (a large positive — a big march step); otherwise compute the
  in-brick UVW as `(atlasBase * 8 + fracWithinBrick * 7 + 0.5) / atlasDims` and take **one**
  `SampleLevel` trilinear tap on the atlas, denormalized by `max_dist * worldScale`. Keep the global
  `min()` over instances and the world-AABB cull.
- `sdfSkyOcclusion` / `sdfReflectionOcclusion` keep their Phase-1 trace; only the field-sampling
  primitive underneath changes.

### 6. Instance + CPU plumbing

- `engine/crates/rendering/src/gpu_types.rs`: extend `SdfInstance` to carry the v2 locators
  (single bindless slot, `indirection_dims`, `atlas_dims`, voxel `dims`, `max_dist`, world scale).
  Update the `size_of` static assert and `Default` to the new block count.
- `engine/crates/assets/src/render_scene.rs`: `build_sdf_instance` (around the
  `mesh_ref.sdf()` branch) writes the v2 locators from the `GpuSdf` instead of the v1 grid fields.
- `engine/crates/assets/src/load.rs`: drop `read_sdf_chunk` and the v1 `load_mesh_sdf`/SDST-chunk
  read; `upload_mesh` no longer takes a pre-baked `Sdf` — it bakes-or-loads from the sidecar cache
  (resolution_scale from asset metadata, default `1.0`). Thread the same change through the
  `RenderScene` upload trait in `render_scene.rs` (`upload_mesh` signature) and the test double.
- `engine/crates/assets/src/import.rs`: delete the `compute_sdf_for_mesh` call + the
  `ChunkKind::SignedDistance` pending chunk (and remove the now-dead `SignedDistance` chunk kind if
  nothing else uses it — NO LEGACY). Import stays GPU-free.

### 7. Per-asset resolution scale + control surface

- Carry `resolution_scale: f32` (default `1.0`) on the mesh sub-asset / bake request so a heavy asset
  can be densified or a trivial one coarsened, capped so the longest axis stays ≤ 128 to start.
- Per the `sa`-CLI convention, add a `saffron-control` command reporting per-mesh SDF bake stats
  (grid dims, occupied vs total bricks, atlas MB, last bake ms) and setting `resolution_scale`. Run
  `cargo run -p xtask -- gen-protocol` and `cd editor && bun run check` for the new wire surface.

## Files

Change:
- `engine/crates/geometry/src/sdf.rs` — SDST v2 format only (`Sdf`, `SdfHeader`, `to_bytes`/
  `from_bytes`, `sample_voxel`); delete `compute_sdf_for_mesh`/`axis_cells`/`SDF_MAX_AXIS`; add the
  `#[cfg(test)]` `bake_sparse_reference` oracle helper.
- `engine/crates/rendering/src/upload.rs` — `bake_sdf` / `bake_or_load_sdf` (GPU dispatch via
  `with_one_off_commands`), bake compute pipelines on `Uploader`, sidecar cache, v2
  `upload_default_sdf`.
- `engine/crates/rendering/src/resources.rs` — `GpuSdf`/`GpuSdfParts` two-image v2 (atlas +
  indirection), v2 metadata, `Drop`.
- `engine/crates/rendering/src/descriptors.rs` — second bindless 3D array (indirection),
  `claim_sdf_slot` writing both arrays, `write_sdf_indirection`, `seed_all_sdf_textures` both.
- `engine/crates/rendering/src/gpu_types.rs` — `SdfInstance` v2 locators + static assert + `Default`.
- `engine/crates/rendering/src/renderer.rs` — `set_sdf_scene` / `sdf_max_world_voxel` derive from the
  v2 instance fields; default-SDF wiring.
- `engine/crates/assets/src/render_scene.rs` — v2 `SdfInstance` build; `upload_mesh` signature.
- `engine/crates/assets/src/load.rs` — bake-or-load on upload; drop `read_sdf_chunk`/`load_mesh_sdf`.
- `engine/crates/assets/src/import.rs` — drop the CPU bake + `SignedDistance` chunk.
- `engine/assets/shaders/sdf.slang` — indirection binding + brick-tap `sdfDistance`.

Create:
- `engine/assets/shaders/sdf_voxelize.slang`, `engine/assets/shaders/sdf_jfa.slang`
  (registered in `xtask shaders`). The sign is resolved on the host (`sign_field` in
  `upload.rs`), so there is no `sdf_finalize` shader — the dense single-normal finalize was
  dropped because its sign rule diverged from the `MeshBvh` oracle at edges/corners.
- `assets/cache/<meshHash>.sdf` sidecars (written at runtime, not checked in).

## Reuse

- `MeshBvh` (`engine/crates/geometry/src/picking.rs`) — keep as the sign/distance test oracle; do
  not delete with the CPU bake.
- The bindless 3D atlas + sampler + slot allocator in `descriptors.rs`/`resources.rs` — extend to a
  two-array (atlas + indirection) scheme, do not rewrite. `GpuSdf` already returns its slot to a
  shared free-list on `Drop`; keep that discipline for the second image.
- `crate::Image3D` (`resources.rs`) — already a storage 3D image (the DDGI voxel proxy); reuse for
  the transient bake seed/work volumes.
- `Uploader::with_one_off_commands` / `submit_and_wait` / `GpuQueue` (`upload.rs`) — the existing
  one-shot submit-and-wait path the bake dispatches through; reuse rather than a new queue.
- The `Sdf::to_bytes`/`from_bytes` round-trip + analytic + sign unit-test scaffolding in `sdf.rs`
  (sphere / box / inside-outside) — keep, retargeted to v2 via `bake_sparse_reference`.

## Risks (phase-specific)

- **GPU sign correctness (central).** Signing from the jump-flood on non-watertight Sponza is the
  new risk. The outside-flood and the nearest-triangle-normal cross-check can disagree on thin or
  open geometry. Validate the GPU output against `MeshBvh::nearest_signed_distance` on the unit
  sphere/box and a Sponza spot-check before trusting it; if they disagree, prefer the flood result
  but log/clamp the conflict band.
- **Voxelize coverage.** A scatter-by-triangle seed must not miss thin features; conservative
  triangle-voxel overlap (test the voxel AABB against the triangle, not just centroid) is required or
  the JFA seeds from a hole.
- **Brick border seams.** The 1-voxel shared border must duplicate the correct neighbour plane or
  trilinear shows brick-grid creases. The round-trip test should sample across a brick boundary.
- **Bake latency on the worker.** The bake runs on the upload path including the thumbnail worker;
  confirm the per-`Uploader` bake pipelines + one-off submit do not serialize the frame queue
  pathologically (it shares `GpuQueue`). Time it.
- **Memory.** A 128–192-axis sparse Sponza field should land in the low tens of MB; assert the atlas
  size in the bake-stats control command so a regression to dense is visible.

## Verification

- `just engine` clean; `just prepare-for-commit` (cargo fmt + clippy `-D warnings`, oxfmt/oxlint for
  the editor) with every warning this change raises fixed.
- `cargo run -p xtask -- shaders` compiles the three new bake shaders + the rewritten `sdf.slang`.
- Geometry unit tests on the v2 format: `Sdf` byte round-trip; bad-magic / truncation rejection;
  `bake_sparse_reference` sphere + box distances match the analytic field away from the surface band;
  sign negative inside / positive outside — all against the `MeshBvh` oracle. Add a cross-brick
  sampling test for the border continuity.
- Extend the existing `upload_sdf_is_validation_clean_and_reclaims_its_slot` test (`upload.rs`) to
  the GPU bake: a small mesh bakes through voxelize/JFA/finalize validation-clean, the `GpuSdf` v2
  claims + reclaims its slot in both bindless arrays, and the GPU-baked field agrees in sign with the
  CPU oracle at sampled points (the central-risk gate). Skips when no Vulkan device is present.
- Sidecar cache: a second load of the same mesh reads `assets/cache/<hash>.sdf` and skips the bake
  (assert via the bake `tracing` span / a hit counter).
- `gen-protocol` + `cd editor && bun run check` for the new control command.
- **Perf**: profiler confirms the GPU bake is ms-scale with no import freeze (the whole point) and
  the per-pixel `sdf-ao` cost is unchanged from baseline (this phase is cost-neutral per-pixel).
- **Visual on the NVIDIA path** (`just run`, Sponza): the smeary wall blobs resolve into real
  geometry at the densified resolution; no brick-grid seams on flat walls; the open roof stays clean
  (Phase 1's zebra fix holds). The interior-dark / sky-on-miss outcome is still later phases.
