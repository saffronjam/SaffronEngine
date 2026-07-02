# Phase 2 — Screen-space GI resolve

**Status:** COMPLETED (core). The indirect-diffuse resolve is fully moved to the half-res `gi-resolve`
compute pass; the fragment samples it (set 4 binding 7) instead of resolving DDGI+IBL diffuse per pixel.
- **`scene-opaque` 2.14 → 0.42 ms** total (Phase 1 → 0.67; the GI-resolve cutover → 0.42).
- Cutover validated: pre- vs post-cutover pixel-diff ~1 % / <1 % of pixels (the intended half-res +
  bilinear-upsample delta for low-frequency diffuse GI) and visually identical; validation-clean.
- gi-resolve **hoisted** to run whenever the screen chain runs (not only when sky-occ is on); verified
  validation-clean + correct with sky-occlusion both ON and OFF.

**Follow-ups (not blocking; noted for correctness):**
- **Probe-diffuse blend** — `gi_resolve.slang` does the global IBL diffuse + DDGI but not the
  reflection-probe diffuse blend, so scenes that place reflection probes lose probe-diffuse GI (the
  `dev` scene has none, so no regression there). Add `probeIrradiance[8]` + `probeMeta` to the resolve
  set + the nearest-probe blend, then it is fully general.
- **Dead-code cleanup** — the fragment's now-unused inline `irradiance` cube diffuse + probe-diffuse
  blend + `dfaoMap` sample are dead-stripped by Slang but should be removed from `lighting.slang` for
  clean source (do together with the probe-diffuse move).
- Step 1 (shared `giprobe` module) DONE + verified (render-identical, pixel-diff ~0, validation-clean).
- Step 2 (`gi_resolve.slang` compute shader) DONE — first cut (global IBL + dfao + shared `giprobe`
  DDGI; probe-diffuse blend deferred to pre-cutover). Compiles + links the module; additive (unwired).
- Architecture refinement (verified render-identical): `giprobe` now declares **no bindings** — the DDGI
  atlases are passed to `ddgiSampleIrradiance` as `Sampler2D` params (Slang resource params), so
  `lighting` keeps its atlases at set 5 and `gi_resolve` is a clean **single-set** pass (no shared set
  number, no empty-set-layout fiddliness). `lighting` re-declares + passes its atlases; parity confirmed.
- **Binding contract (final):** `gi_resolve` set 0 = {b0 G-buffer, b1 half-res out, b2 GiParams UBO,
  b3 IBL irradiance cube, b4 resolved dfao, b5 DDGI irradiance atlas, b6 DDGI distance atlas}.
- Step 3 IN PROGRESS.
  - Backend DONE: `ssao::GiParams` UBO struct (224 B, size-asserted), `create_gi_resolve_layout` (the
    7-binding set), `Pipelines::request_gi_resolve` + slot.
  - `view_target` side DONE + compiles + clippy-clean: `gi_indirect` half-res target, per-frame-slot
    `gi_params_ubos` (mapped) + `gi_resolve_sets`, view-local writes (b0/b1/b4) + b2 UBO writes in
    `write_screen_space_sets`, and `write_gi_resolve_shared` (b3 IBL cube, b5/b6 DDGI atlases).
  - Renderer pass DONE: `FramePipelines.gi_resolve` (requested when the screen chain runs); per-frame
    `GiParams` fill + memcpy to `gi_params_ubos[frame]` + per-frame-slot `write_gi_resolve_shared` in
    `render_scene_offscreen`; the `gi-resolve` `add_compute_pass` after dfao-blur (g_normal + dfao_denoised
    → gi_indirect). Hazard caught + fixed: the shared write must touch ONLY the current fenced slot (not
    all slots) — writing an in-flight slot tripped VUID-vkUpdateDescriptorSets-None-03047. Now clean.
- **Step 4 (next): additive parity.** Add a debug view / readback of `gi_indirect` and pixel-diff it
  against the fragment's in-line `indirectIrr` on a static `dev` capture (they should match on the
  no-probe scene). Confirms the resolve is correct BEFORE the cutover.
- **Step 5: fragment cutover** (eyeball) — replace `evalLighting`'s `indirectIrr` block with a bilinear
  sample of `gi_indirect`; delete the in-fragment DDGI call + IBL-diffuse there; add the probe-diffuse
  blend to `gi_resolve.slang` first (for scenes with reflection probes).
  - **Remaining 3 — full design (accessors + lifecycle all resolved, ready to implement in one pass):**
    - Target: `gi_indirect` half-res `Image::new(half_extent, G_NORMAL_FORMAT rgba16f, storage_sampled)`
      in `view_target` (mirror `dfao_raw`). (Half-res; the cutover bilinear-samples it — a later blur/
      upsample pass can sharpen if needed.)
    - **Lifecycle (the key correctness point):** `GiParams` is 224 B > 128 B push, so it's a UBO, and
      the engine's hazard-free per-frame-UBO pattern is **per-frame-slot** (`lighting.rs` `frames:
      Vec<Frame>` of `MAX_FRAMES_IN_FLIGHT`, each a `make_mapped_uniform_buffer`). So make
      `gi_params_ubos: [Buffer; MAX_FRAMES_IN_FLIGHT]` + `gi_resolve_sets: [DescriptorSet; N]`
      (`descriptors.allocate_set(ssao.gi_resolve_layout())`). Per frame: memcpy `GiParams` into slot
      `frame`'s mapped UBO; dispatch with `gi_resolve_sets[frame]`. Image bindings are stable → written
      once; only UBO *contents* change per frame (no per-frame descriptor rewrite → no in-flight hazard).
    - **Split population** (`build_screen_space` lacks `ibl`/`ddgi`): write the view-local bindings there
      into each slot's set — b0 `g_normal` view + `ssao.nearest_sampler()`, b1 `gi_indirect` (storage),
      b2 `gi_params_ubos[i]` (`descriptors.write_uniform_buffer`), b4 `dfao_resolved` view + sampler;
      write the shared bindings from the renderer (has `ibl`+`ddgi`) — b3 `ibl.irradiance_cube_view()` +
      `ibl.sampler()`, b5 `ddgi.irradiance()` view + `ddgi.sampler()`, b6 `ddgi.distance()` view +
      `ddgi.sampler()` — once when ibl+ddgi ready, re-written on IBL rebake / DDGI rebuild / resize
      (same triggers the mesh sets use).
    - Pass: renderer fills `GiParams` (inv_proj/inv_view from `ssao`, `DdgiVolume` from `ddgi`, flags from
      the DDGI/sky-occ gates), memcpy to slot UBO, then `add_compute_pass("gi-resolve", gi_resolve_pso,
      gi_resolve_sets[frame], &[(g_normal, SampledReadCompute), (gi_indirect, StorageImageRwCompute),
      (dfao_resolved, SampledReadCompute)], None, groups(half.w), groups(half.h))` — after the dfao chain,
      before scene-opaque. Additive: nothing samples `gi_indirect` yet.
- Step 4 additive parity vs the fragment; Step 5 cutover (eyeball).

The forward über-fragment resolves every indirect term per full-res pixel: DDGI 8-corner cage (16
atlas taps + Chebyshev), two IBL cubes, an 8-slot reflection-probe loop with data-dependent cube
indexing, SSR, RT reflections. This is the dominant `scene-opaque` cost and the modern standard
(Lumen final-gather, id Tech) is to resolve indirect **once, at reduced resolution, in a compute
pass off the G-buffer**, then have the fragment add a single upsampled indirect texture.

## Concrete design (derived from the code — implementation-ready)

**The exact term to move** is `indirectIrr` in `lighting.slang` `evalLighting` (~875-882):
```
skyVis      = dfaoMap.r                                  // set 4 b5, already a screen map
analyticIrr = irradiance * skyVis                        // IBL/probe diffuse cube × dfao
indirectIrr = screenFlags.z ? lerp(analyticIrr, ddgiSampleIrradiance(worldPos,n).rgb, .w)
                            : analyticIrr                // DDGI replaces analytic by coverage
```
`indirectIrr` is **view-independent** (only worldPos + normal + the dfao map) → computable at half-res
from the G-buffer. The fragment keeps the cheap per-pixel tail: `indirect = kd * indirectIrr * albedo;
ambient = indirect*ao + specularIBL; (+ SSGI)`. **Specular stays full-res** in the fragment (R-dependent).

**Bindings the resolve pass needs** (mirror the mesh's indirect-diffuse inputs, bind the SAME set
objects): globals `LightGlobals` (set 1 b0 — DDGI volume/scroll/probeCount + eyePosition + sdfOcclusion),
IBL `irradianceMap` + reflection `probeIrradiance[8]`/`probeMeta` (set 3), `dfaoMap` (set 4 b5), DDGI
`ddgiIrradiance`/`ddgiDistance` (set 5), plus a pass set (b0 = G-buffer sampler, b1 = half-res out).
Template: `dfao.slang` (worldPos/worldN reconstruction via `invProjection`/`invView`; `viewPosFromUv`).

**Steps (incremental, each compiles + verifiable):**
1. **Extract the shared sampler into a Slang module** (NO-LEGACY: one copy). Move `ddgiSampleIrradiance`
   + the octahedral helpers (and the IBL/probe diffuse-irradiance selection) into a `giprobe` module
   (pattern = the existing `sdf` module, which declares GDF bindings imported by both `lighting` +
   `dfao`). `lighting.slang` imports it → **verify identical render** (pure refactor, no behavior change).
2. **`gi_resolve.slang`** compute: reconstruct worldPos/n from the G-buffer, compute `indirectIrr`
   (analyticIrr via IBL/probe diffuse × dfao skyVis, replaced by DDGI by coverage). Write `rgba16f`
   (rgb = indirectIrr, `.a` = viewZ for the denoiser). Half-res.
3. **Wire the pass** in `renderer.rs`/`pipelines.rs`/`view_target.rs`: a `gi-resolve` graph pass off the
   G-buffer (before scene-opaque, or async-compute), its descriptor set + the shared globals/IBL/dfao/
   DDGI sets. Reuse the bilateral upsample (Phase 3's fused kernel) to full-res.
4. **Additive validation first:** compute the map but have the fragment still resolve in-line; A/B the
   resolve map vs the fragment's `indirectIrr` (pixel-diff) to confirm parity **before** cutover.
5. **Fragment cutover (NO-COMPAT):** replace `evalLighting`'s `indirectIrr` block (875-882) with a
   single sample of the upsampled indirect-diffuse map; delete the in-fragment DDGI call + IBL-diffuse
   path there; retire the now-unused mesh-side DDGI/probe-diffuse bindings if nothing else reads them.
   Keep the non-IBL fallback path (897-914) consistent (it also calls `ddgiSampleIrradiance`).

> **Highest-visual-risk step is the cutover (5)** — GI parity must be eyeballed, not just headless-diffed.
> Do the refactor (1) + pass (2-3) + additive parity (4) freely; land the cutover (5) where a human can
> confirm diffuse-GI looks identical.

## Expected effect
Removes ~40–70 % of indirect cost from the fragment → `scene-opaque` ~0.6–1.0 ms combined with
Phase 1; adds ~0.2–0.4 ms half-res GI pass (async-hideable).

## Risks / watch
- Needs a reliable depth/G-buffer (Phase 1 prepass helps). Half-res GI on thin geometry (curtains,
  foliage) needs bilateral edge-stopping to avoid halos — verify on the banners.
- Disocclusion under motion: the resolve is per-frame from the G-buffer (no temporal reuse required
  first); if noisy, add the SSGI-style accum. Keep DDGI's own temporal (probe atlas) intact.
- Reflections/specular must look identical (they stay full-res) — regression-check the metal props.

## Verification
- Profiler: fragment / `scene-opaque` ms down; new `gi-resolve` ms accounted; net frame drop.
- Screenshot A/B: diffuse GI visually matches the pre-cutover fragment resolve (no banding/halos);
  reflections unchanged. Validation-clean. `just engine` + lint clean.
