# Phase 6 — Retire the per-pixel DFAO prepass; reconcile to one system

**Status:** COMPLETED

## Goal / Context

By the end of Phase 5 there are two systems doing the same job. The screen-space DFAO prepass
(`sdf_ao.slang` trace + the Phase 1 `sdf_ao_accum` denoiser) writes a temporally accumulated
open-sky factor + bent normal that `lighting.slang` reads as `sky.ao` to dim the analytic IBL
diffuse — and, in parallel, DDGI now carries the indirect diffuse and lets the sky in only on a
ray-miss (the `ddgiSampleIrradiance` replace-by-coverage path). Running both double-counts the
indirect occlusion and keeps paying the measured **1.25 ms `sdf-ao`** prepass for a term DDGI
already produces. This is exactly the configuration UE collapses under Lumen: distance-field AO is
*gone* as a diffuse term, the skylight is occluded because miss-rays reach the sky, and the only
distance-field per-pixel consumer left is specular/reflection occlusion.

This phase deletes the prepass and reconciles to one indirect path:

- **Indirect diffuse occlusion** = DDGI ray-miss (medium/long range, already in `lighting.slang`)
  + **small-radius GTAO on the indirect term only** (contact-scale detail the probe grid is too
  coarse to resolve). No `sky.ao` × `aoMap` stacking; no double count.
- **One per-pixel SDF consumer remains**: `sdfReflectionOcclusion` for specular, now reading the
  **GDF** (built in Phase 4) instead of the per-pixel per-instance `sdfDistance` loop.

This is the only phase that removes the 1.25 ms. The visible symptom it fixes: the diffuse "stain"
blobs and any residual prepass cost disappear entirely, while interiors stay dark via DDGI.

## Work (ordered steps)

1. **Delete the DFAO prepass shaders.** Remove `engine/assets/shaders/sdf_ao.slang` and the Phase 1
   `engine/assets/shaders/sdf_ao_accum.slang` denoiser. Drop both from the shader build list in
   `engine/xtask` (the `shaders` task) so `cargo run -p xtask -- shaders` no longer compiles
   `sdf_ao.spv` / `sdf_ao_accum.spv`. NO LEGACY: there is no fallback prepass left.

2. **Strip `sdf.slang` to the GDF reflection consumer.** In `engine/assets/shaders/sdf.slang`:
   - Delete `sdfSkyOcclusion` (the diffuse few-cone trace) and the `SkyOcclusion` struct — nothing
     samples them once the prepass is gone.
   - Delete the per-pixel per-instance `sdfDistance(wp, count)` loop over `sdfInstances`, and the
     `SdfInstance` declaration + `[[vk::binding(8, 1)]] sdfInstances` and
     `[[vk::binding(1, 0)]] sdfTextures[256]` bindings from this fragment-side module. The per-mesh
     MDF bricks still exist, but after Phase 4 they are consumed by the GDF composite (compute) and
     the DDGI near-field trace — not by the fragment lighting set.
   - Rewrite `sdfReflectionOcclusion` to sphere-march the **GDF clipmap** (the resource set + tap
     helper introduced in Phase 4's `global_sdf.rs` / `sdf.slang` GDF section) along the reflection
     vector `R`, keeping the roughness-widened cone half-angle and the `occ = min(occ, saturate(d /
     (coneHalfAngle * t)))` penumbra. Keep `octEncode`/`octDecode` only if a remaining consumer
     needs them; otherwise delete them too (they existed for the AO map bent normal).

3. **Remove the `sky.ao` diffuse path in `lighting.slang`.** In `engine/assets/shaders/lighting.slang`:
   - Delete the `[[vk::binding(5, 4)]] Sampler2D sdfAoMap` binding and its doc comment.
   - In `evalLighting`, delete the `SkyOcclusion sky; … if (globals.sdfOcclusion.y != 0) { … }`
     block and the `irradianceMap.SampleLevel(sky.bentNormal, …)` bent-normal lookup. Sample the
     analytic IBL irradiance along the geometric/shading normal `n` again (it is only the residual
     where DDGI coverage is absent).
   - Change `float3 indirectIrr = irradiance * sky.ao;` to `irradiance` unmodified — the DDGI
     `lerp(indirectIrr, ddgi.rgb, ddgi.w)` directly below is now the sole indirect-occlusion source
     (the sky enters only where coverage is low, i.e. on ray-miss).
   - Keep `sdfReflectionOcclusion` for `specSkyVis`, but call it through the GDF (no per-instance
     count). Repurpose or remove the `globals.sdfOcclusion` packing accordingly: `.x` (instance
     count) and `.z` (coarsest voxel size) are dead per-pixel inputs; keep `.y` as the
     enable bit gating the GDF reflection-occlusion term (or fold it under the reflections/DDGI
     flags and drop the field). Update `LightGlobals.sdfOcclusion`'s comment to match.

4. **Retune GTAO to small-radius contact AO on indirect only.** GTAO already modulates only the
   indirect/ambient term (`globals.counts.w` gates `aoMap` into `ao`, which multiplies `indirect`
   and feeds `specAO` — it never touches direct lighting). Tighten its radius so it contributes
   contact-scale detail rather than the large-range occlusion DDGI now owns: reduce
   `Ssao::radius` (`crates/rendering/src/ssao.rs`, default `1.0`, consumed by `gtao_push` →
   `gtao.slang` `params.x`) to a small contact radius, and confirm the blur radius
   (`self.radius * 2.0`) and SSGI/contact derived radii still read sensibly. Document the new
   radius as "contact AO; large-range occlusion is DDGI ray-miss."

5. **Delete the prepass plumbing in `crates/rendering`.**
   - `crates/rendering/src/ssao.rs`: remove `SdfAoPush`, its `size_of` assert, the `sdf_ao_frame`
     field + its init/advance, and `next_sdf_ao_push`. Adjust the `radius` default per step 4.
   - `crates/rendering/src/lib.rs`: drop `SdfAoPush` from the re-export list.
   - `crates/rendering/src/pipelines.rs`: remove the `sdf_ao: Option<Arc<Pipeline>>` field, its
     `None` init, and `request_sdf_ao` (the `shaders/sdf_ao.spv` build). Update the
     `build_compute_multi` doc comment that names the `sdf_ao` trace.
   - `crates/rendering/src/view_target.rs`: remove `sdf_ao_raw`, `sdf_ao_resolved`,
     `sdf_ao_history`, `sdf_ao_set`, `sdf_ao_accum_sets` (and their `None`/null inits, the image
     creation + initial-layout transitions, the descriptor-set allocations in the alloc path, the
     bind plan entries, and `sdf_ao_history_view`). The mesh set's binding 5 (`sdfAoMap`,
     `Binding::sampled(self.mesh_set, 5, …, sdf_ao_resolved)`) goes with the `lighting.slang`
     binding.
   - `crates/rendering/src/renderer.rs`: remove the `sdf_ao` pipeline field, `sdf_ao_push`,
     `sdf_ao_history_slots`, `sdf_ao_resolved_slot`; the `next_sdf_ao_push` call + `request_sdf_ao`
     resolve in frame setup; the entire `if let Some(sdf_ao) = &pipelines.sdf_ao { … }` graph block
     (the `sdf-ao` trace pass + the `sdf-ao-accum` accumulation pass); the
     `writeback_sdf_ao_history_layout` function + its two call sites; and the
     `sdf_ao_history_slots`/`sdf_ao_resolved_slot` reads in the temporal-writeback section. Drop the
     `|| screen.sdf_ao_history_slots.is_some()` from `temporal_ran`.
   - `crates/rendering/src/descriptors.rs`: the bindless SDF array (set 0, binding 1) and the
     SDF-occluder instance list (set 1, binding 8) carry a `COMPUTE` stage flag "for the `sdf_ao`
     prepass". With the prepass gone and per-pixel per-mesh sampling replaced by the GDF tap, drop
     binding 8 from the **lighting** set and the per-mesh array from the **fragment** path if Phase
     4 moved their consumers onto GDF resources; otherwise narrow the stage flags to exactly the
     stages that still read them (the GDF composite compute + DDGI trace own the per-mesh bricks).
     Reconcile the comments so they describe the GDF-era layout, not the prepass.

6. **Reconcile the sky-occlusion control surface.** `set-sky-occlusion` (registered in
   `crates/control/src/commands_render.rs`, DTO `SetSkyOcclusionResult` in
   `crates/protocol/src/dto.rs`, listed in `crates/protocol/src/command.rs`, bridged via
   `Renderer::set_sky_occlusion` / `sky_occlusion_enabled` in `renderer.rs` and
   `crates/host/src/control_renderer.rs`) currently means "occlude the analytic IBL skylight with
   per-mesh SDF distance-field AO" — the prepass this phase deletes. Repoint it to gate the GDF
   reflection-occlusion term (the single remaining per-pixel SDF consumer) and update its help text
   + the DTO doc comment + the `RenderConfig`/status field (`commands_render.rs` `sky_occlusion:`)
   accordingly. The renderer's `want_sky_occlusion` gate (`gbuf_ready && ibl_enabled &&
   sky_occlusion_enabled && sdf_instance_count > 0`) loses its prepass meaning — repurpose it to
   gate the GDF specular-occlusion path (drop the `sdf_instance_count` dependency; the GDF readiness
   from Phase 4 replaces it). Then regenerate the protocol (`cargo run -p xtask -- gen-protocol`)
   and run `cd editor && bun run check`.

7. **Re-confirm the indirect-energy budget.** With `sky.ao` removed, walk `evalLighting` once more:
   the IBL fallback branch (no DDGI coverage) must not be brighter indoors than before, and the
   `specAO * specSkyVis` specular split must still read from GTAO (`ao`) for the contact term and
   the GDF cone for the directional reflection term — no term applied twice.

## Files

- `engine/assets/shaders/sdf_ao.slang` — **delete**.
- `engine/assets/shaders/sdf_ao_accum.slang` — **delete** (the Phase 1 denoiser).
- `engine/assets/shaders/sdf.slang` — delete `sdfSkyOcclusion`/`SkyOcclusion`, the per-instance
  `sdfDistance` loop + `SdfInstance`/`sdfInstances`/`sdfTextures` fragment bindings; rewrite
  `sdfReflectionOcclusion` onto the GDF tap.
- `engine/assets/shaders/lighting.slang` — drop `sdfAoMap` (binding 5, set 4) + the `sky.ao` diffuse
  block; `indirectIrr = irradiance`; keep specular `sdfReflectionOcclusion` via GDF; fix the
  `LightGlobals.sdfOcclusion` packing/comment.
- `engine/xtask` (shaders task) — remove `sdf_ao` / `sdf_ao_accum` from the compile list.
- `crates/rendering/src/ssao.rs` — remove `SdfAoPush`, `sdf_ao_frame`, `next_sdf_ao_push`; retune
  `radius`.
- `crates/rendering/src/lib.rs` — drop the `SdfAoPush` re-export.
- `crates/rendering/src/pipelines.rs` — remove the `sdf_ao` field + `request_sdf_ao`.
- `crates/rendering/src/view_target.rs` — remove all `sdf_ao_*` images, sets, accum sets, binds,
  and `sdf_ao_history_view`.
- `crates/rendering/src/renderer.rs` — remove the `sdf_ao` pipeline/push/slot fields, the `sdf-ao`
  + `sdf-ao-accum` graph passes, `writeback_sdf_ao_history_layout`, the writeback call sites, and
  the `temporal_ran` term; repurpose `want_sky_occlusion` onto the GDF specular path.
- `crates/rendering/src/descriptors.rs` — narrow/drop the per-mesh SDF bindless array (set 0 /1) +
  instance-list (set 1 /8) bindings + `COMPUTE` stage flags to the GDF-era consumers; fix comments.
- `crates/rendering/src/ddgi.rs` — confirm the ray-miss replace-by-coverage path is now the sole
  diffuse indirect-occlusion source (no remaining coupling to the deleted prepass); adjust any
  comment that points at the per-mesh prepass.
- `crates/control/src/commands_render.rs`, `crates/protocol/src/{dto.rs,command.rs}`,
  `crates/host/src/control_renderer.rs` — repoint `set-sky-occlusion` to the GDF reflection-occlusion
  term; regen protocol.
- `docs/content/` — update the distance-field / GI reference page(s) so the documented indirect path
  is "DDGI ray-miss + contact GTAO + GDF reflection occlusion," not the per-pixel DFAO prepass.

## Reuse

- **Extend, don't rewrite, `sdfReflectionOcclusion`**: keep its cone-march structure and just swap
  the field source from the per-instance `sdfDistance` loop to the Phase 4 GDF tap.
- **Reuse the existing GTAO pass wholesale** (`gtao.slang`, `Ssao::gtao_push`, the `aoMap`
  consumption at `counts.w` in `lighting.slang`); this phase only retunes its radius and confirms it
  stays on the indirect term. No new AO pass.
- **Reuse the DDGI replace-by-coverage** in `ddgiSampleIrradiance` + the `lerp(indirectIrr, ddgi.rgb,
  ddgi.w)` already present — it becomes the sole diffuse occlusion, unchanged.
- **Reuse the `set-sky-occlusion` command + DTO** by repointing its meaning, rather than adding a new
  command (NO LEGACY: one toggle for the one remaining SDF per-pixel term).

## Risks

- **Indirect goes too dark or too bright after removing `sky.ao`.** DDGI coverage must blanket the
  interiors it was masking; any uncovered region falls back to full analytic IBL. Verify the Phase 5
  camera-centered probe clipmap covers the Sponza interior before deleting the prepass, or the
  blobs become bright leaks. If a gap shows, it is a Phase 5 coverage bug, not a reason to keep the
  prepass.
- **Reflection occlusion regresses on the GDF.** The per-mesh field was finer than the GDF near a
  surface; a chrome surface's reflected-skybox occlusion may soften. Acceptable per the design
  (specular detail is lower frequency), but spot-check a polished floor reflection.
- **Stale bindings / descriptor-layout mismatch.** Removing set-4 binding 5 and the set-0/1 SDF
  bindings must stay in lockstep across `lighting.slang`, `sdf.slang`, `descriptors.rs`, and
  `view_target.rs`'s bind plans — a leftover binding throws a validation error at pipeline create.
  Run the validation-clean smoke after the cut.
- **Double-count if a term is missed.** Easy to leave `specAO` reading a now-large-radius GTAO while
  also applying the GDF cone — re-confirm the specular split in step 7.

## Verification

1. `cargo run -p xtask -- shaders` compiles with `sdf_ao`/`sdf_ao_accum` gone (no missing-`.spv`
   error from the renderer pipeline requests).
2. `just engine` builds clean; `just prepare-for-commit` (cargo fmt + `clippy -D warnings` + oxfmt +
   oxlint) passes — including no dead-code warnings from the removed `SdfAoPush`/fields.
3. `cargo run -p xtask -- gen-protocol` then `cd editor && bun run check` pass (the repointed
   `set-sky-occlusion` DTO/help).
4. Headless validation-clean smoke (`just run-engine-headless`) shows no Vulkan validation errors
   from removed bindings or layouts.
5. **Perf (the point of this phase):** profile on the NVIDIA path (`just run`, Sponza, DDGI on) and
   confirm the `sdf-ao` and `sdf-ao-accum` passes are gone from the timeline and the frame is
   **≤ the pre-Phase-1 baseline** (the 1.25 ms `sdf-ao` is reclaimed).
6. **Visual:** Sponza interior stays correctly dark via DDGI with the prepass removed; exterior
   walls lit; no diffuse stain blobs; a polished floor still occludes the reflected skybox under
   overhangs (GDF reflection occlusion alive). Toggle `set-sky-occlusion 0/1` and confirm it now
   only changes the specular reflection-occlusion term, not the diffuse. Re-check the three original
   symptoms (blobs, zebra, prepass cost) are all gone.
