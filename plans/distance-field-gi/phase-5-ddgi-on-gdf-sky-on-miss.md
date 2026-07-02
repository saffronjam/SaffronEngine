# Phase 5 — DDGI on the GDF, denser probes, sky-on-miss, default ON

**Status:** COMPLETED

## Goal / Context

By the end of Phase 4 the engine has one camera-centered Global Distance Field clipmap
(`crates/rendering/src/global_sdf.rs`, `gdf_cull.slang` + `gdf_composite.slang`) that is the
shared distance oracle, plus the per-pixel near/far trace handoff in `sdf.slang`. DDGI, however,
still traces the *old* coarse solid-AABB voxel proxy: `ddgi_voxelize.slang` fills a 32³
`Image3D` from per-draw world AABBs (a box SSBO), and `ddgi_trace.slang` fixed-step marches that
grid. That proxy is the wrong field — it fills a mesh's whole bounding box as solid, so an indoor
probe sees a "wall" where there is an open doorway, and the 8×4×8 whole-scene cage puts probes
~4.6 m apart along X (Sponza spans ~37 m), far coarser than a wall is thick, so the Chebyshev
leak test cannot stop light bleeding through floors.

This phase makes DDGI trace the real geometry. DDGI rays sphere-march the **GDF** (with the
fine per-mesh MDF for the near field, the P4 handoff), the **sky enters radiance only on a ray
miss** (the `lighting.slang` replace-by-coverage stays exactly as is — that is the correct
Lumen-style piece), and the whole-scene 8×4×8 cage becomes a **camera-centered scrolling probe
volume at ~1.5 m spacing** so probe spacing sits below wall thickness and the kept Chebyshev test
actually bounds leaks. Hit radiance comes from a **lite per-cell albedo cache** (a documented
fidelity cap — a flat per-cell base color, *not* a Surface Cache). `use_ddgi` flips **default
ON**.

Symptom this fixes: the Sponza second floor stays lit by the skybox because indirect rays never
reach a real occluder. After this phase, an interior is dark by construction — indirect rays that
stay enclosed never reach the sky, and only rays that escape an opening pick up sky radiance —
while the exterior, where rays escape, stays lit, directionally and without the box-proxy's
all-or-nothing occlusion. The solid-AABB proxy and its box buffer are **deleted (NO LEGACY)**.

This phase is the payoff of P1–P4: it is where interiors finally go dark. The per-pixel DFAO
prepass (`sdf_ao.slang`) still runs alongside DDGI here; reconciling to one indirect path and
removing its 1.25 ms is Phase 6's job, not this one.

## Work (ordered steps)

1. **Delete the solid-AABB voxel proxy.**
   - Remove `engine/assets/shaders/ddgi_voxelize.slang` and drop it from the shader build list
     (`crates/rendering/src/renderer.rs` shader registration / `xtask` shader enumeration is glob
     based, so deleting the file is enough — confirm no explicit reference remains).
   - In `crates/rendering/src/ddgi.rs` delete: the `voxels: Image3D` field and its `Image3D::new`
     allocation; `box_buffer`, `box_capacity`, `frame_box_count`, `DDGI_MAX_BOXES`,
     `DDGI_VOXEL_RES`, `DDGI_VOXEL_FORMAT`; `VoxelizePush` + its size assert + `voxelize_push()`;
     `voxels()`, `set_voxel_layout()`, `voxel_set`, `voxel_layout`, `voxel_set()`,
     `voxel_layout()`, `wants_voxelize()`; the `make_mapped_storage_buffer` helper and the box
     interleave loop in `set_scene`; the `voxel`/`box` bindings from `build_layouts` and the box
     SSBO write in `write_static_descriptors`. Update the doc comments (the module header lists
     "the grow-only per-frame scene-box SSBO" and "five passes" — it becomes four passes:
     trace → blend-irr → blend-dist → border).
   - In `crates/rendering/src/renderer.rs` delete the `ddgi_voxelize: Option<Arc<Pipeline>>`
     `FramePipelines` field, its `request_ddgi_voxelize` resolution (the `wants_voxelize()`
     branch around the pipeline-resolve block), the voxelize dispatch in the DDGI graph-build
     (`build_ddgi_passes`, the `if let Some(voxelize) = …` block + its `DDGI_VOXEL_RES.div_ceil`
     group math), and the voxel-image layout writeback. Update `request_ddgi_voxelize` removal in
     `crates/rendering/src/descriptors.rs`.

2. **DDGI rays sphere-march the GDF (near MDF → far GDF), sky on miss.** Rewrite
   `engine/assets/shaders/ddgi_trace.slang`:
   - Drop the `voxels` `RWTexture3D` binding and the DDA voxel march. Bind, in set 0: the GDF
     clipmap atlas + indirection + the per-cell **albedo cache** (the P4 `global_sdf.rs`
     resources, step 4), the prev-irradiance atlas sampler (kept), and the ray-out storage
     (kept). Match the binding order to the new `trace_layout` in `ddgi.rs`.
   - Replace the per-step occupancy test with a sphere-march that reuses the P4 GDF sampling
     helper in `sdf.slang` (the same `gdfDistance` / scene-distance function the per-pixel path
     calls) for the far field and the per-mesh MDF (`sdfDistance`) for the **near ~2 m** — the
     leak-mitigation handoff already specified for P4. A hit is `d < surfaceEpsilon`; advance by
     `max(d, minStep)`; cap by march length and a step budget.
   - **Sky only on miss.** On a hit, radiance = the hit cell's albedo (sampled from the albedo
     cache at the hit point) × a crude sun+sky direct term + last-frame probe irradiance
     re-sampled at the hit (free multi-bounce, the existing `sampleProbeIrradiance` reused). On a
     ray that escapes (no hit within range), radiance = `skyColor`. This is the existing
     hit/miss structure; the change is that "hit" now means a true distance-field surface, not a
     box voxel, so enclosed rays no longer miss into the sky.
   - Keep the Fibonacci ray set, per-frame golden rotation, and the round-robin probe-budget
     indexing (`skyColor.w` offset) unchanged.

3. **Camera-centered scrolling probe volume at ~1.5 m spacing.** In `crates/rendering/src/ddgi.rs`:
   - Replace the `DDGI_PROBES_X/Y/Z = 8/4/8` whole-scene constants with a denser camera-centered
     grid (e.g. `16 × 8 × 16`) at a fixed `DDGI_PROBE_SPACING ≈ 1.5 m`, so the volume extent is
     `spacing · count` (a fixed-size box that follows the camera), not a scene-AABB fit. Probe
     count rising ~8× grows the octahedral atlases (`irradiance_atlas_width/height`,
     `distance_atlas_width/height`) and the ray image — verify the new sizes against device
     limits; they remain small (irradiance ≈ 1280×160 for 16×8×16). Keep `DDGI_PROBE_BUDGET` as a
     fraction of the new total so the per-frame trace cost stays bounded.
   - `set_scene` takes the **camera position** and snaps the volume origin to the probe grid
     (`floor(camPos / spacing) · spacing - extent/2`) so the cage centers on the camera without
     swimming. Store the integer grid-snap offset; expose `volume()` (already read by
     `set_scene_lighting` into the light UBO) returning the camera-centered min/extent.
   - **Probe relocation on scroll.** When the snapped origin moves by N probe cells between
     frames, the probes that scroll in are stale. Add toroidal probe addressing (a scroll offset
     folded into the probe index in `trace_push` + `blend_*_push` + the mesh-side
     `ddgiSampleIrradiance` lookup) and **clear/reset the newly-exposed probe planes' history**
     (a per-probe first-frame mask, generalizing today's whole-volume `history_reset`). Probes
     that stay in view keep converging; only the in-scrolled slab re-rays. This is the standard
     "infinite scrolling volume" DDGI mechanism — implement it, do not fall back to a static
     cage.
   - Flip `use_ddgi` **default ON** in `Ddgi::new` and update the doc comments that say "off by
     default" / "adds five compute passes" (now four). Update the `set_enabled`/`reset_history`
     arming so the default-on first frame still arms a full reset.

4. **Lite per-cell albedo cache (documented cap).** The GDF carries distance only, so a hit has
   no color. Add a coarse albedo clipmap volume aligned to the GDF finest cascade:
   - In `crates/rendering/src/global_sdf.rs` (the P4 module) add an `Image3D` albedo cache
     (rgba8 or rgba16f) the same resolution/placement as the GDF finest cascade, and have
     `gdf_composite.slang` splat each composited instance's **base color** into the cells its
     bricks cover (the cull pass already bins instances per cascade — reuse it). Document, in the
     module header and the shader, that this is a **flat per-cell base color** — no normal, no
     view-dependent shading, no emissive, no multi-material resolution within a cell — a coarse
     radiance approximation, explicitly **not** a Surface Cache; it is the known fidelity cap of
     this phase.
   - Bind the albedo cache into the DDGI trace set (step 2) so the hit radiance can read it.

5. **Drop the box-proxy CPU path; feed the camera-centered volume.** In
   `crates/assets/src/render_scene.rs`:
   - Delete the `box_mins` / `box_maxs` / `box_albedos` build (the `RenderSceneBuild` fields and
     the per-item AABB+albedo collection that fed the voxelize SSBO) and the whole-scene
     DDGI volume fit (`let pad = Vec3::ONE; let vol_min = scene_min - pad; …`). The scene AABB is
     still computed for the directional-shadow fit — keep that; only the DDGI box/volume use of
     it goes.
   - Change `set_ddgi_scene` (the `RenderSceneTarget` trait method here + its renderer impl in
     `crates/rendering/src/renderer.rs` + the test stub at `render_scene.rs` ~line 1454) to drop
     the three box-array params and take the **camera position** instead, passing through to
     `Ddgi::set_scene`. The sun/sky params stay (the trace still needs them).
   - `set_scene_lighting` already folds `ddgi.volume()` + `probe_count_ubo()` into the light UBO
     via `set_frame_ddgi`; with the camera-centered volume this now publishes a moving cage —
     confirm the call order (DDGI scene set before lighting) is preserved.

6. **Keep `lighting.slang` replace-by-coverage; verify against the camera-centered cage.** No
   structural change to `ddgiSampleIrradiance` or the indirect-diffuse REPLACE in
   `computeMesh` — the sky-on-miss is in the trace, and the per-pixel coverage lerp + Chebyshev
   leak test are correct. The only thing that changes underneath is that `ddgiVolumeMin/Extent`
   now describe a camera-centered box; confirm coverage still reads ~1 deep inside the cage (the
   trilinear-mass `covsum`, unchanged) so indirect fully replaces the analytic sky there, and a
   surface outside the cage falls back to analytic IBL (acceptable — the cage follows the
   camera). Keep the Chebyshev moment test exactly as is.

7. **Control + tests.** `set-gi` (`crates/control/src/commands_render.rs`) needs no new command —
   default ON is renderer state — but update the status it reports and any e2e/unit assertion
   that expects DDGI off at boot: the `ddgi.rs` `ddgi_resource_bringup_is_validation_clean` test
   asserts `!ddgi.use_ddgi` (flip it), the `wants_ddgi`/`history_reset` state tests, the push-size
   tests (drop `VoxelizePush`, keep the rest), and `atlas_dimensions_match_octahedral_tiling`
   (update to the new probe counts). If the `set-gi` DTO or status surface changes at all, run
   `cargo run -p xtask -- gen-protocol` + `cd editor && bun run check`.

## Files

- `engine/assets/shaders/ddgi_trace.slang` — rewrite: GDF sphere-march (near MDF → far GDF), sky
  on miss, albedo-cache hit color; new set-0 bindings (GDF atlas + indirection + albedo cache +
  prev-irradiance + ray-out).
- `engine/assets/shaders/ddgi_voxelize.slang` — **delete**.
- `engine/assets/shaders/lighting.slang` — no structural change; verify `ddgiSampleIrradiance`
  (`covsum` coverage, Chebyshev moments) against the camera-centered volume.
- `engine/assets/shaders/sdf.slang` — reuse the P4 GDF sampling helper + near-field MDF
  `sdfDistance` from the DDGI trace (no new symbol expected; confirm it is shareable from the
  trace shader's include).
- `engine/assets/shaders/gdf_composite.slang` — extend (P4 shader) to splat per-instance base
  color into the albedo cache.
- `engine/crates/rendering/src/ddgi.rs` — delete voxel proxy + box SSBO + `VoxelizePush` +
  `voxelize_push`/`wants_voxelize`/`voxel_*`; new `DDGI_PROBES_*`/`DDGI_PROBE_SPACING`;
  camera-centered snapped `set_scene` + toroidal scroll offset + per-probe relocation reset;
  rebuilt `trace_layout`/`write_static_descriptors`; `use_ddgi` default ON.
- `engine/crates/rendering/src/global_sdf.rs` — add the albedo cache `Image3D` (P4 module) +
  its write in the composite pass.
- `engine/crates/rendering/src/renderer.rs` — drop `ddgi_voxelize` pipeline field + request +
  dispatch + layout writeback; wire the DDGI trace set to the GDF + albedo cache; `set_ddgi_scene`
  signature (camera pos, no box arrays); confirm `set_scene_lighting` publishes the camera-centered
  volume.
- `engine/crates/rendering/src/descriptors.rs` — remove `request_ddgi_voxelize`; adjust the DDGI
  trace set layout writes if the binding count changes.
- `engine/crates/rendering/src/gpu_types.rs`, `upload.rs` — only if the albedo-cache splat needs a
  per-instance base-color in the composite input (reuse the existing instance/material data;
  prefer no new buffer).
- `engine/crates/assets/src/render_scene.rs` — delete the box-proxy build + whole-scene volume
  fit; `set_ddgi_scene` trait + impl + test stub take the camera position.
- `engine/crates/control/src/commands_render.rs` — `set-gi` status reflects default-ON; no new
  command.

## Reuse

- **Keep the DDGI probe machinery** — the two octahedral atlases, the blend-irradiance /
  blend-distance / border passes, the round-robin probe budget, `sampleProbeIrradiance`, the
  octahedral encode/decode, and the linear-clamp atlas sampler all carry over unchanged. This
  phase swaps the *geometry the trace reads* (voxel proxy → GDF) and the *volume placement*
  (scene-fit → camera-centered), not the probe update pipeline.
- **Keep `lighting.slang`'s replace-by-coverage + Chebyshev** — it is already the correct
  Lumen-style piece; do not touch its math.
- **Reuse the P4 GDF + cull/composite machinery** for the albedo cache: the cull pass already
  bins MDF instances per cascade, so the albedo splat rides the same dispatch rather than a new
  scene-traversal.
- **Reuse the scene AABB** still computed in `render_scene.rs` for the shadow fit — only its DDGI
  consumer changes.

## Risks

- **GDF/MDF leak through thin walls.** Even at ~1.5 m probe spacing, a wall thinner than a probe
  cell can leak. Mitigations are already in the design and must all stay: near-field MDF trace
  (full per-mesh resolution for the first ~2 m), probe spacing below wall thickness, and the
  Chebyshev moment test. Validate on the Sponza interior specifically; if a known thin wall still
  leaks, tighten spacing before adding hacks.
- **Probe swimming / popping on camera move.** The snap-to-grid origin plus toroidal addressing
  must be exact, or probes appear to crawl or flash as the camera moves. The per-probe relocation
  reset must clear *only* the in-scrolled slab — clearing the whole volume every move would make
  DDGI never converge while walking. Test by dragging the camera and watching for indirect-light
  flicker (the README visual gate calls this out).
- **Albedo-cache fidelity cap is visible.** A flat per-cell base color means bounce light is the
  wrong hue near multi-material cells and has no directional shading. This is accepted and
  documented; the risk is scope creep into a Surface Cache. Hold the cap for this phase.
- **Atlas growth.** 8× more probes enlarges the irradiance/distance atlases and the per-frame ray
  image; confirm the new dimensions are within `maxImageDimension*` and the ray image height
  (probe total) is legal. Adjust the probe count down one notch if a software/llvmpipe device
  rejects it, but keep ~1.5 m spacing as the invariant.
- **Default ON cost.** DDGI's four passes now always run; with the P5 trace reading the GDF (one
  tap beyond the near field) it should stay within the round-robin budget. This phase is
  cost-neutral-ish; the 1.25 ms `sdf-ao` prepass is still present until P6.

## Verification

- `just engine` builds clean (shaders compile, including the deleted `ddgi_voxelize` no longer
  referenced); `just prepare-for-commit` (cargo fmt + clippy `-D warnings`, oxfmt/oxlint if the
  control surface changed) passes for this phase's changes only.
- Unit tests in `crates/rendering/src/ddgi.rs` updated and green: bring-up validation-clean with
  the new four-pass set + camera-centered volume, `use_ddgi` default-ON assertion, the
  scroll/relocation history-reset behavior, push sizes (no `VoxelizePush`), and atlas dimensions
  for the new probe counts. `ddgi_resource_bringup_is_validation_clean` stays validation-clean on
  the software device.
- If `set-gi` status or any DTO changed: `cargo run -p xtask -- gen-protocol` + `cd editor && bun
  run check` clean.
- **Visual on the NVIDIA path** (`just run`, Sponza) — the gate cannot judge look, so this is the
  human-checked boundary:
  - The interior (second floor) is **dark** with DDGI on; the exterior/courtyard stays lit.
  - Occlusion is **directional** — a surface facing an opening is brighter than one facing a
    wall — not the box-proxy's flat all-or-nothing.
  - **No flicker** while dragging the camera (probe scroll + relocation reset are stable).
  - Toggling `set-gi off` falls back to analytic IBL and still renders reasonably (no crash, no
    black scene).
  - Re-confirm the three original symptoms remain gone (no zebra from P1, no wall blobs from P2).
- **Perf:** capture the profiler with DDGI default-on; confirm the four DDGI passes fit the
  round-robin budget and the frame is not worse than the P4 baseline (the `sdf-ao` 1.25 ms is
  expected to still be present — its removal is P6). Verify single test files in isolation (the
  sandbox e2e suite is environmentally flaky).
