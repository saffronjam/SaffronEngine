# Phase 4 — Global SDF clipmap + near/far trace handoff

**Status:** COMPLETED

## Goal / Context

Phases 1–3 fix the per-mesh field: a GPU jump-flood bake into sparse SDST v2 bricks (P2), 3-mip
prefilter + coverage (P3), and a signal-correct denoiser (P1). What is still missing is the *global*
half of UE5's system. Today every per-pixel and per-ray SDF consumer loops over **all** the per-mesh
`SdfInstance`s and `min()`s them (`sdf.slang::sdfDistance`, the `for (uint i = 0; i < count; …)`
loop). That is O(instances) per sample, it has no shared spatial structure, and on modular content it
scales linearly with scene complexity — the opposite of UE's O(1) Global Distance Field tap.

This phase builds the Global Distance Field (GDF): a **camera-centered 3-cascade clipmap** that, each
frame, **culls** the per-mesh MDF bricks into the cascade they touch and **composites** them with a
`min()` into a single device-resident distance volume per cascade. Every distance query beyond the
near field then becomes **one trilinear tap** of the cascade covering the sample, independent of how
many meshes are in the scene. `sdf.slang` traces the fine per-mesh MDF for the **near ~2 m** (full
resolution where it matters — the leak-mitigation contract from the README) and a **single GDF tap
beyond**, so far-field cost decouples from instance count.

Honest scope (per the plan): on Sponza-as-one-giant-mesh the GDF adds **no new spatial detail** — the
blob fix already came from the densified MDF in P2 — and the frame stays **cost-neutral** here until
P6. The GDF's wins land on modular content and at scale: alias-free O(1) tracing and cost decoupling.
It is also the **shared, alias-free oracle** that P5's DDGI rays sphere-march. Build it correctly now
so P5 and P6 have one field to consume.

## Work (ordered steps)

1. **Cascade geometry + state — new `crates/rendering/src/global_sdf.rs`.** Define a `GlobalSdf`
   sub-state mirroring the shape of `Ddgi` (owns `Arc<DeviceResources>`, a `use_gdf`/`ready` pair,
   per-frame state, set layouts + sets, `Drop` frees the sampler + layouts; the images Drop by field
   order). Constants near the top, the way `ddgi.rs` declares `DDGI_*`:
   - `GDF_CASCADES: u32 = 3`; `GDF_RES: u32 = 128` (finest cascade is `128³`); `GDF_EXPONENT: f32 =
     2.0` (each cascade covers `exponent×` the world extent of the previous — finest ~ enough world
     span that its voxel ≈ the near-field handoff scale, ~2 m start as the README specifies via the
     innermost cascade's world extent / `GDF_RES`).
   - The cascade volume format is `R16_SNORM` (`vk::Format::R16_SNORM`), matching the per-mesh brick
     atlas encode so the composite `min()` is in the same normalized space; allocated as an
     [`Image3D`] (`resources.rs`), `STORAGE | SAMPLED` — written by the composite compute as
     `GENERAL`, sampled by `sdf.slang` / the P5 DDGI trace as `SHADER_READ_ONLY_OPTIMAL`. One image
     per cascade (or one `128 × 128 × (128·cascades)` stacked volume addressed by a cascade slice —
     pick stacked so a single bindless 3D handle serves `sdf.slang`, the same pattern the DDGI atlases
     use). Store each cascade's world-space center + half-extent + voxel size for the push/UBO.
   - A linear clamp-to-edge sampler (reuse the helper shape from `ddgi.rs::create_linear_clamp_sampler`
     / `descriptors.rs::create_sdf_sampler`), since the cascade tap is a trilinear read.

2. **Camera-centered placement, toroidal addressing.** Each frame, recenter every cascade on the
   camera eye (snapped to its own voxel grid so the field does not shimmer sub-voxel as the camera
   creeps). Address each cascade **toroidally**: the volume wraps modulo `GDF_RES`, so recentering by
   `k` voxels only invalidates the `k`-voxel slab that scrolled in — not the whole volume. Track each
   cascade's previous snapped center; the per-frame delta gives the dirty slab(s). The eye comes from
   the camera the renderer already has — `Ssao::set_camera` stores `view`/`proj` and exposes
   `inv_view` (`ssao.rs`); the eye is `inv_view.col(3)`. Add a `GlobalSdf::set_camera(eye)` (or fold
   it into a `set_scene`) called from the renderer alongside `set_ssao_camera`/`set_cluster_camera`
   (`renderer.rs`).

3. **Cull pass — new `engine/assets/shaders/gdf_cull.slang`.** A compute pass that bins the per-mesh
   MDF instances per cascade. Input: the existing `SdfInstance` SSBO (set 1 binding 8 today;
   `gpu_types.rs::SdfInstance`, `renderer.rs::set_sdf_scene`) carrying each field's world AABB
   (`worldMin`/`worldMax`) + bindless brick locators (added in P2). For each cascade, test each
   instance's world AABB against the cascade bounds and emit the surviving instances (+ the brick
   ranges that overlap the dirty slab) into a per-cascade compacted list (a small append SSBO with an
   atomic counter, like the DDGI box buffer pattern but GPU-built). Only **dirty** bricks for the
   incrementally-updated region are emitted, so a moving camera pays a slab, not a full rebuild. On a
   **moved static** (its `SdfInstance` transform changed), mark its old + new footprint dirty so the
   composite re-mins those voxels.

4. **Composite pass — new `engine/assets/shaders/gdf_composite.slang`.** A compute pass that, for each
   dirty voxel of each cascade, samples every culled brick covering that voxel and keeps the minimum
   signed distance, writing the `R16_SNORM` cascade volume. **Reuse the P2 brick machinery**: the
   in-brick indirection lookup + trilinear tap is exactly the `sdfDistance` brick path P2 introduces;
   factor that into a `sdf.slang` helper (`sampleMdfBrick(instance, worldPos)`) the composite calls,
   so there is one brick-sampling implementation, not two. Empty regions (no brick covers the voxel)
   write the coarse coverage distance (a large positive step), so the cascade is conservative and the
   sphere-march skips empty space fast. Dispatch only over the dirty slab(s) from step 2 — toroidal
   write coordinates wrap modulo `GDF_RES`. Near cascade updates every frame; far cascades are
   **staggered** (one cascade's full refresh round-robined across frames, mirroring the DDGI
   `DDGI_PROBE_BUDGET` round-robin), so the per-frame composite cost is bounded.

5. **Render-graph wiring — `renderer.rs`.** Add the two GDF passes ahead of the lighting/DDGI passes,
   gated on a `GlobalSdf::wants_gdf(pipelines_ready)` (mirror `Ddgi::wants_ddgi`). Import each cascade
   volume with `RenderGraph::import_image_3d` (the same import the DDGI voxel proxy uses — see
   `renderer.rs` around the `import_image_3d(vox_image, …)` call) and declare
   `RgUsage::StorageImageRwCompute` on both the cull-output SSBO and the cascade volumes so the graph
   derives the `GENERAL` barriers. Write back the resolved cascade layout after the graph executes
   (the `set_*_layout` write-back pattern from `ddgi.rs`). Build the two compute PSOs through the
   pipeline cache the way `request_sdf_ao` is requested. Bind the cascade volumes into the bindless /
   light descriptor set the consumers read (next step).

6. **Near/far handoff in `sdf.slang`.** Add a `gdfDistance(worldPos)` that selects the finest cascade
   whose bounds contain `worldPos` (else the next coarser, else a large positive "open" distance),
   converts world → that cascade's `[0,1]³` toroidal UVW, and does **one trilinear tap**, denormalized
   from `R16_SNORM` by the cascade's voxel/encode scale. Then change the consumers:
   - `sdfSkyOcclusion` / `sdfReflectionOcclusion`: for march distance `t < ~2 m` keep the per-mesh
     `sdfDistance` (P2 brick path) for full near-field resolution; for `t ≥ ~2 m` take the `gdfDistance`
     tap. This is the README's leak mitigation: near occluders register at full MDF resolution, far at
     GDF cost. The handoff radius is a named constant (≈ the finest cascade's voxel-scaled near field).
   - Keep the existing bindings (`sdfTextures[256]`, `sdfInstances`) for the near path; add the cascade
     `Sampler3D` + a small cascade-params UBO (centers/extents/voxel sizes, one per cascade) as new
     bindings on the light set, declared in `descriptors.rs` and written from `global_sdf.rs` (the
     `write_combined_sampler` pattern in `ddgi.rs`).

7. **`gen-protocol` / control surface.** If a `gdf`-enable toggle or cascade-debug readback is exposed
   (a `sa` command, per the AGENTS.md keep-current rule — DDGI has `set_enabled`), add the one
   `saffron-control` registration + run `cargo run -p xtask -- gen-protocol` then `cd editor && bun run
   check`. A debug visualizer (sample the chosen cascade distance to a color) is worth a toggle for the
   visual gate.

## Files

Create:
- `engine/crates/rendering/src/global_sdf.rs` — `GlobalSdf` sub-state: cascade `Image3D`s, cull-output
  SSBO, sampler, set layouts + sets, push structs (`GdfCullPush`, `GdfCompositePush` with
  `const _: () = assert!(size_of::<…>() == …)` pins like `ddgi.rs`), `set_camera`/`set_scene`,
  `wants_gdf`, `advance_frame`, `cascade_params_ubo`, the `import_image_3d` accessors + layout
  write-backs. Register the module in `crates/rendering/src/lib.rs` and re-export `GlobalSdf`.
- `engine/assets/shaders/gdf_cull.slang` — per-cascade AABB cull of `SdfInstance`s → compacted
  per-cascade brick lists (atomic-append), dirty-slab restricted.
- `engine/assets/shaders/gdf_composite.slang` — per-dirty-voxel `min()` over culled bricks → the
  `R16_SNORM` cascade volume, toroidal write, coverage fill for empty voxels.

Change:
- `engine/assets/shaders/sdf.slang` — add `gdfDistance`, factor `sampleMdfBrick` out of the P2
  `sdfDistance`, switch `sdfSkyOcclusion` + `sdfReflectionOcclusion` to the near-MDF / far-GDF handoff;
  new cascade `Sampler3D` + cascade-params bindings.
- `engine/crates/rendering/src/renderer.rs` — own a `GlobalSdf`; add the cull + composite passes to the
  graph (import cascades via `import_image_3d`, `StorageImageRwCompute` accesses, layout write-back),
  feed the camera eye from the SSAO camera, request the two PSOs, bind cascades into the consumer set.
- `engine/crates/rendering/src/descriptors.rs` — the cascade `Sampler3D` + cascade-params bindings on
  the light/bindless set the consumers read (extend, do not duplicate, the existing SDF binding setup).
- `engine/crates/rendering/src/gpu_types.rs` — if cull/composite need a packed cascade-instance or the
  cascade-params struct, add it here with the std430 size assert (the `SdfInstance` 144-byte pin is the
  model).

Read for grounding (no change, or only as P2 lands): `crates/rendering/src/ddgi.rs` (the sub-state +
round-robin + import-image-3d + layout-writeback patterns to mirror), `crates/rendering/src/ssao.rs`
(`set_camera` / `inv_view` for the eye), `crates/rendering/src/resources.rs` (`Image3D`),
`crates/geometry/src/sdf.rs` (SDST v2 brick format the composite consumes), `crates/assets/src/render_scene.rs`
(`set_sdf_scene`, how `SdfInstance`s are gathered).

## Reuse

- **The DDGI sub-state shape** (`ddgi.rs`): `GlobalSdf` copies its structure — `Arc<DeviceResources>`
  ownership, partial-failure unwind in `new`, `import_image_3d` + `set_*_layout` write-back for 3D
  storage images, the round-robin budget (`DDGI_PROBE_BUDGET`) → the staggered far-cascade refresh,
  push-size `assert!`s, and the linear-clamp sampler helper. Do not invent a new pattern.
- **The P2 brick sample** (`sdf.slang::sdfDistance` brick path): the composite mins the same bricks the
  near-field tap reads — extract one `sampleMdfBrick` helper both call (one implementation).
- **The `SdfInstance` SSBO + cull inputs** (`gpu_types.rs::SdfInstance`, `set_sdf_scene` in
  `renderer.rs` / `render_scene.rs`): the cull reads the world AABB + brick locators already on each
  instance; do not add a parallel instance list.
- **`RgUsage::StorageImageRwCompute` + `import_image_3d`** (`renderer.rs`): the existing 3D-storage
  graph plumbing the DDGI voxelize/trace/blend passes use — the GDF passes declare the same usages.

## Risks (phase-specific)

- **Toroidal addressing correctness.** Off-by-one in the wrap-modulo or the snapped-center delta gives
  a smeared field that ghosts as the camera moves. Validate the wrap math with a unit test on the
  device-free index helper (center delta → dirty-slab voxel ranges, mod `GDF_RES`), the way `ddgi.rs`
  unit-tests its pure state machine without a device.
- **Cascade-boundary seam.** A discontinuity where one cascade hands to the next shows as a visible
  shell. Mitigate by blending across an overlap band near the boundary (sample both cascades, lerp by
  distance to the inner cascade's edge) and by the near-MDF handoff covering the innermost transition.
- **Depends on P2/P3.** The composite consumes SDST v2 bricks (P2) and the coverage distance (P3); do
  not start the composite shader before the P2 brick atlas + indirection bindings exist. The cull +
  cascade-state scaffolding (`global_sdf.rs`, placement, toroidal math) can land first against the
  existing `SdfInstance` world AABBs.
- **Cost-neutral on Sponza.** One giant mesh fills one cascade with one instance — the GDF buys nothing
  visible here. Do not chase a perf win on Sponza this phase; verify correctness + decoupling on
  modular content (multiple imported meshes).
- **Empty-voxel coverage value.** If empty voxels write `0` instead of a large positive distance the
  sphere-march stalls (zero step) — write the conservative coverage distance, asserted by the empty-
  region test below.

## Verification

- `just engine` clean, then `just prepare-for-commit` (fmt + clippy `-D warnings`); fix every warning
  this change raises. `cargo run -p xtask -- shaders` compiles `gdf_cull.slang` + `gdf_composite.slang`
  + the edited `sdf.slang` to SPIR-V.
- **Validation-clean resource bring-up test** in `global_sdf.rs` (mirror
  `ddgi_resource_bringup_is_validation_clean`): build `GlobalSdf` on the software device, assert
  `ready`, the cascade volumes rest in the expected layout after init, the sets are non-null, and the
  bring-up raises no validation issues. Skips cleanly when no Vulkan device is obtainable.
- **Push-size + pure-state unit tests** (device-free, like `ddgi.rs`): `assert!(size_of::<GdfCullPush>()
  == …)` / `GdfCompositePush`; the toroidal recenter helper maps a camera delta to the correct dirty-
  slab ranges; `wants_gdf` gates on on+ready+pipelines.
- **Composite-vs-oracle correctness:** a small unit test feeds two overlapping analytic brick fields
  (or reuses the sphere/box bake from `sdf.rs`) and asserts the composited cascade distance equals the
  per-instance `min()` within the encode tolerance, and that an empty voxel returns the coverage
  distance (large positive), not zero. This makes the GDF the *same* oracle the per-mesh path is.
- **Visual on the NVIDIA path** (`just run`): the per-pixel reflection-occlusion / sky-occlusion look
  is **unchanged on Sponza** (cost-neutral, no new artifacts, no cascade-boundary shell as the camera
  moves) — the gate cannot judge look, so this is a human visual check at the phase boundary. On a
  modular multi-mesh scene, the GDF debug visualizer shows a continuous field that recenters smoothly on
  the camera with no ghosting on drag.
- **Perf:** profiler each frame — the cull + composite cost is bounded (near cascade per frame, far
  staggered) and the far-field SDF query cost no longer scales with instance count. Frame ≤ baseline on
  Sponza. The e2e suite is environmentally flaky — verify single files in isolation.
