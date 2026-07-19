# Phase 2 — Real-time sky-light capture: SH diffuse, amortized specular prefilter, and GI retint

**Status:** COMPLETED

Part of `plans/sky-and-volume/` (dynamic sky, time-of-day, and volumetric clouds). Phase 1 made the atmosphere *animate* — it split `Ibl::bake` so the sun-independent Transmittance + Multiple-Scattering LUTs freeze behind an atmosphere-dirty gate while the sun-dependent Sky-View LUT and the `atmos_skygen` env-cube fill re-derive per sun-move off the synchronous `device.wait_idle` path (temporally blending the new env cube into the old), widened `should_rebake` to a sun-angle epsilon, and coupled the sun/moon key lights to the Transmittance LUT. What Phase 1 did **not** do is make the *sky-light* — the diffuse ambient, the prefiltered reflections, and the indirect GI bounce — follow that live sky. Today those are all still products of the one-shot `wait_idle` convolution chain: a 32² diffuse irradiance cube (`ibl_irradiance.slang`), a 5-mip GGX prefiltered specular cube (`ibl_prefilter.slang`), and a DDGI/voxel-GI ambient that is a *flat scene color* (`ddgi_sky = environment.ambient_color * ambient_intensity`, `render_scene.rs`) with no atmosphere in it at all. So even with Phase 1's animated sky, a moving sun leaves the ambient tint, the reflections, and the indirect bounce frozen — the tell-tale decoupled look.

This phase closes that gap the modern way (UE5 *SkyLight Real-Time Capture*): project the live env cube into a 9-coefficient spherical-harmonic diffuse basis every frame it is cheap to do so (~0.015 ms), reconverge the expensive specular prefilter **amortized** across frames with a temporal blend so reflections update without a pop, and feed the same live SH sky into the DDGI ray-miss radiance and the GI-resolve fallback so indirect bounce retints with time-of-day. It is NO-LEGACY throughout: the 32² irradiance cube is *removed* in the same change that adds the SH buffer, the flat `ddgi_sky` feed is *deleted* (not left beside the SH feed), and the synchronous convolution steps move off `device.wait_idle` onto the per-frame render graph.

## Goal

Make the sky-light IBL and GI track the dynamic sun without re-baking the world:

- **SH diffuse, per-frame.** Replace `ibl_irradiance.slang`'s 32²×6 cube cosine-convolution with a Ramamoorthi 9-coefficient SH projection of the live env cube, written into a small storage buffer. Rewire mesh set-3 binding 0 and every global-irradiance consumer (mesh forward shade, forward transparent, GI-resolve fallback) to reconstruct diffuse irradiance analytically from the SH buffer. The projection runs every frame the atmosphere is live (~0.015 ms), so the diffuse ambient follows the sun (and, from Phase 1's moon coupling, the moon at night) for free.
- **Amortized specular reconvergence.** Keep the split-sum GGX prefilter (`ibl_prefilter.slang`) and its existing Krivanek–Colbert filtered importance sampling (the firefly control already in-shader), but stop running its whole 5-mip chain synchronously behind `wait_idle`. Instead time-slice the reconvergence across ~9 frames on the per-frame render graph, refresh only past the Phase-1 sun-move threshold, and exponentially blend each newly-prefiltered mip into the previous prefiltered cube so a moving sun's reflections update smoothly, never a hard swap that pops.
- **Cost-asymmetric scheduling.** The diffuse SH (cheap) recomputes per-frame; the specular reconvergence (the ~50× more expensive half) stays gated + time-sliced, cadence controlled by one scene knob.
- **Dynamic-sky GI retint.** Feed the live SH sky into the `ddgi_trace.slang` ray-miss radiance and the `gi_resolve.slang` analytic fallback, deleting the flat `ddgi_sky = ambient_color × ambient_intensity` feed and its CPU plumbing so indirect bounce follows time-of-day.
- **One write path, one representation.** A single SH coefficient buffer (raw radiance $L_{lm}$) serves both diffuse irradiance (cosine-kernel reconstruction) and GI miss radiance (raw-SH reconstruction); a capture-cadence knob rides `set-atmosphere` (extended in Phase 1), scriptable from `sa`, surfaced in the Environment panel, documented under image-based-lighting.

## Design stance (grounded in current engine practice)

**SH diffuse replaces the irradiance cube — same math, analytic, and per-frame affordable.** A Lambertian environment is captured to ~1% RMS by just 9 SH coefficients because the cosine convolution kernel has only three non-zero bands ($A_0=\pi$, $A_1=\tfrac{2\pi}{3}$, $A_2=\tfrac{\pi}{4}$; higher even bands fall as $\sim 1/l^2$, odd bands vanish) — Ramamoorthi & Hanrahan 2001. The reconstruction $E(\mathbf n)=\sum_{l\le 2,m} A_l\,L_{lm}\,Y_{lm}(\mathbf n)$ is a few multiply-adds per fragment, and the GPU projection is a single-workgroup reduction over a coarse env mip (King, GPU Gems 2 Ch.10), so it costs microseconds and legitimately runs every frame. The 32² irradiance cube — its `IblCube`, its `IBL_IRRADIANCE_SIZE`, its `ibl_irradiance.slang` cube-convolution, and its set-3 binding-0 sampler — is retired in the same change; there is no cube path left beside the SH path.

**Specular is the expensive half, so it is the amortized half.** The diffuse SH (~0.015 ms) and the GGX prefilter (~0.8 ms for the full chain) are ~50× apart in cost, so they are scheduled asymmetrically (UE *SkyLight Real-Time Capture* TimeSlice): SH per-frame, specular time-sliced across ~9 frames and temporally blended into the previous prefiltered cube. The prefilter's filtered importance sampling (Karis 2013 / Zero Radiance) already kills low-roughness fireflies in-shader — that stays; the phase adds an exponential-moving-average blend and a per-frame mip-slice schedule, never a hard swap. A stale-by-a-few-frames prefiltered cube is invisible because the sky changes far slower than 60 Hz.

**Single graphics queue, so amortization is time-slicing, not a second queue.** The engine runs one graphics queue (`device.graphics_queue_family`); there is no async-compute queue today. The modern-correct win here is the *time-slicing* — capping the per-frame cost to ≤ one mip-slice — recorded as ordinary per-frame render-graph compute passes on the graphics queue, where the graph derives barriers and overlaps them with unrelated raster through normal pass scheduling. A dedicated async-compute queue is a noted future optimization, **not** a prerequisite, and this phase does not invent one. The load-bearing change is moving the convolutions off `device.wait_idle` onto the graph and spreading the specular across frames.

**GI reads the same live sky, on the GPU, not a flat CPU color.** The DDGI ray-miss radiance is currently a flat `pc.skyColor.rgb` pushed from the CPU (`Ddgi::set_scene`), derived from `environment.ambient_color * ambient_intensity` — atmosphere-blind. Binding the SH buffer into the DDGI trace and evaluating band-limited sky *radiance* at the miss direction makes a ray escaping toward the bright horizon pick up the horizon tint, not an average — strictly better than a flat color, and it tracks the sun with no CPU readback (Majercik et al. DDGI 2019: the sky is the ray-miss radiance). The flat feed and its `ddgi_sky` plumbing are deleted.

> **live env cube (Phase 1) → SH projection (per-frame, 9 coeffs into a buffer) + specular reconvergence (time-sliced mip-slice, EMA-blended into the persistent prefiltered cube) → mesh/transparent/GI-resolve reconstruct diffuse from SH · specular from the reconverged cube → DDGI trace evaluates SH at ray-miss.** The BRDF LUT is environment-independent and stays a one-time bake; the prefiltered cube stays persistent (never re-allocated); only the *contents* refresh, incrementally.

## NO-LEGACY checklist for this phase

- The 32² diffuse irradiance cube is **removed**, not left beside the SH path: `Ibl::irradiance_cube` (the `IblCube` field), `irradiance_cube_view()`, `IBL_IRRADIANCE_SIZE`, the `ibl_irradiance.slang` file, and its bake dispatch all go; the SH buffer + `sh_project.slang` replace them in the same change. Set-3 binding 0 changes from a `COMBINED_IMAGE_SAMPLER` (cube) to a `STORAGE_BUFFER` (coefficients), and every consumer is rewired together.
- The flat DDGI sky feed is **deleted**: the `ddgi_sky = ambient_color * ambient_intensity` block in `render_scene.rs`, the `sky_color: Vec3` param on `SceneRenderer::set_ddgi_scene` (trait + `RendererScene` + the `RecordingRenderer` mock), the `sky_color` param + field on `Ddgi::set_scene`, and the `pc.skyColor.rgb` read in `ddgi_trace.slang` are removed; the trace evaluates the SH buffer at miss. The `skyColor.w` round-robin probe-budget offset is preserved as a dedicated `budgetOffset` field (it was never a color).
- The synchronous `device.wait_idle` convolution is **replaced**, not kept beside the live path: the SH projection + the prefilter reconvergence become per-frame render-graph compute passes reading the live env cube (Phase 1) and writing the persistent SH buffer + prefiltered cube. The one-time `Ibl::bake(first_bake=true)` still produces the initial cube/LUTs/SH so a cold start is valid frame 0; there is no second "static IBL" path.
- The prefilter keeps its existing filtered-importance-sampling firefly control — **no** re-introduced hard luminance clamp, **no** second prefilter shader. The reconvergence is an EMA blend added to the one `ibl_prefilter.slang`.
- The capture-cadence knob is **one** field on `AtmosphereSettings`, carried by the existing `set-atmosphere` command — never a second sky command, never a knob on `set-environment`/`set-fog`. It is registered across every protocol tripwire in one change (field-add pattern: `dto.rs` `SetAtmosphereParams`, the hand-authored `AtmosphereSettingsDto` schema + its `required`, the `set-atmosphere` merge handler, the committed generated artifacts).

## 0 — Foundation: the live convolution seam and the SH buffer resource

Before the SH projection can run, three things move: the persistent cubes/buffer must be reachable from the per-frame render graph (they are baked directly today, never imported), the SH coefficient buffer must exist as a device resource with a set-3 binding, and set-3 binding 0 must change type.

| What | File | Symbols |
|---|---|---|
| IBL orchestrator: cubes, LUTs, the re-bake gate, set-3 write | `engine/crates/rendering/src/ibl.rs` | `Ibl`, `Ibl::bake`, `Ibl::write_mesh_set`, `Ibl::fire_rebake`, `Ibl::should_rebake`, `env_cube_view`, `irradiance_cube_view` (removed), `prefiltered_cube_view`, `IBL_IRRADIANCE_SIZE` (removed), `IBL_PREFILTER_MIPS` |
| Set-3 layout (mesh IBL set) | `engine/crates/rendering/src/descriptors.rs` | `create_ibl_layout` (binding 0: `COMBINED_IMAGE_SAMPLER` → `STORAGE_BUFFER`), `ibl_set_layout`, `MAX_REFLECTION_PROBES` |
| Render-graph import seam (persistent images/buffers into the graph) | `engine/crates/rendering/src/render_graph.rs` | `import_image`, `import_buffer`, `alloc_external_layout`, `external_layout`, `RgUsage::{StorageWriteCompute, SampledReadCompute, StorageImageRwCompute}`, `RgPass::compute` |
| Per-frame compute-pass helper + fire site | `engine/crates/rendering/src/renderer.rs` | `add_compute_pass`, the `fire_rebake` block (~L4916), the initial `ibl.bake(true)` (~L1101), `scene_ibl`/`scene_ibl_mut`, `preview_ibl` |

Add a persistent `sh_coeffs: Buffer` to `Ibl` — nine `Vec4` (RGB coefficient in `.xyz`, `.w` padding) = 144 bytes std430, `STORAGE_BUFFER` usage, device-local, written by the SH projection pass and read by the mesh/GI consumers. Drop `irradiance_cube: IblCube`, `irradiance_cube_view()`, and `IBL_IRRADIANCE_SIZE`. In `create_ibl_layout`, binding 0 becomes a `STORAGE_BUFFER` (fragment stage for the mesh; the GI-resolve pass binds the same buffer in its own compute set). `Ibl::write_mesh_set` writes binding 0 as a buffer descriptor pointing at `sh_coeffs`; bindings 1–5 (prefiltered cube, BRDF LUT, probe cube arrays, probe meta) are unchanged.

The persistent `env_cube` and `prefiltered_cube` (and the new `sh_coeffs`) are imported into the per-frame graph via `import_image` / `import_buffer` with `alloc_external_layout` cross-frame layout slots — the exact pattern `add_aerial_perspective_pass` uses for its 3D volume (import → fill pass → barrier-only read pass → write back the exit layout). The one-time `Ibl::bake(first_bake=true)` still runs the full chain once at startup so set 3 is valid before frame 0; from then on the live passes (§1, §2) refresh the contents.

> Decision to record when building: **the SH buffer is a single 9-coefficient raw-radiance ($L_{lm}$) store, not two separate diffuse/GI buffers.** Diffuse consumers apply the cosine kernel $A_l$ at reconstruction (→ irradiance $E(\mathbf n)$); the DDGI miss applies no kernel (→ band-limited radiance $L(\mathbf d)$). One projection, one buffer, two reconstruction helpers — no duplicate representation.

## 1 — SH diffuse projection

**File `engine/assets/shaders/sh_project.slang` (new).** Replaces `ibl_irradiance.slang` (deleted). A single-workgroup compute reduction over a coarse env-cube mip (the env cube already carries a full mip chain for the prefilter's filtered importance sampling; project from ~32²×6 texels, cheap and pre-averaged) accumulating nine RGB SH coefficients weighted by the SH basis and the per-texel solid angle, then writing them to `sh_coeffs`. Set 0: binding 0 = env cube (sampler), binding 1 = `sh_coeffs` (RW storage buffer).

$$L_{lm} = \sum_{\text{texels}} L(\mathbf d)\,Y_{lm}(\mathbf d)\,\Delta\omega(\mathbf d), \qquad \Delta\omega = \frac{4}{(u^2+v^2+1)^{3/2}}\cdot\frac{1}{\text{faceRes}^2}$$

```hlsl
// sh_project.slang — one workgroup, coarse env mip → 9 RGB coefficients into sh_coeffs
[[vk::binding(0, 0)]] SamplerCube envCube;
[[vk::binding(1, 0)]] RWStructuredBuffer<float4> shCoeffs;   // [9] rgb in .xyz

groupshared float3 partial[9][THREADS];

// order-2 real SH basis (Y_00 .. Y_2-2..Y_22), the 9 non-zero-through-l=2 terms.
void shBasis(float3 d, out float y[9]) {
    y[0] = 0.282095;
    y[1] = 0.488603 * d.y; y[2] = 0.488603 * d.z; y[3] = 0.488603 * d.x;
    y[4] = 1.092548 * d.x * d.y; y[5] = 1.092548 * d.y * d.z;
    y[6] = 0.315392 * (3.0 * d.z * d.z - 1.0);
    y[7] = 1.092548 * d.x * d.z; y[8] = 0.546274 * (d.x * d.x - d.y * d.y);
}
// each thread strides the coarse cube's texels, reduces in shared memory, thread 0 writes [9].
```

Reconstruction lives in the shared light path so mesh, transparent, and GI-resolve all use one copy. In `lighting.slang`, replace the set-3 binding-0 `SamplerCube irradianceMap` with `StructuredBuffer<float4> shCoeffs` and the sample site `float3 irradiance = irradianceMap.SampleLevel(n, 0.0).rgb;` (~L810) with an analytic reconstruction applying the Ramamoorthi cosine coefficients:

$$E(\mathbf n)=A_0 L_{00}Y_{00} + A_1\!\!\sum_{m}\! L_{1m}Y_{1m}(\mathbf n) + A_2\!\!\sum_{m}\! L_{2m}Y_{2m}(\mathbf n),\quad A_0=\pi,\ A_1=\tfrac{2\pi}{3},\ A_2=\tfrac{\pi}{4}$$

| What | File | Symbols |
|---|---|---|
| SH projection pass | `engine/assets/shaders/sh_project.slang` (new) | `computeMain`, `shBasis`, `texelSolidAngle`; replaces `ibl_irradiance.slang:computeMain` (deleted) |
| Diffuse reconstruction (mesh + transparent) | `engine/assets/shaders/lighting.slang` | binding 0 `shCoeffs`, `shIrradiance(n)`, the ambient block (~L800–841, the `irradiance` term) |
| GI-resolve fallback reconstruction | `engine/assets/shaders/gi_resolve.slang` | binding 3 `irradianceMap` → `shCoeffs`, the `analyticIrr` term (~L73) |
| Buffer + write + projection dispatch | `engine/crates/rendering/src/ibl.rs` | `Ibl::sh_coeffs`, `Ibl::write_mesh_set`, the SH projection pass build |

> Decision to record when building: **the SH basis constants and both reconstruction helpers (`shIrradiance` cosine-kernel + `shRadiance` raw) live in one shared shader surface**, imported by `lighting.slang`, `gi_resolve.slang`, and `ddgi_trace.slang` — no per-shader copy of the 9-term polynomial (the drift the atmosphere `AtmosParams` copy-paste already shows is a warning, not a template).

## 2 — Amortized specular prefilter reconvergence

**File `engine/assets/shaders/ibl_prefilter.slang`.** Keep the shader's GGX importance sampling and its filtered-importance-sampling firefly control verbatim (the `saTexel`/`saSample`/`mip` select at ~L107–113 — that is the Krivanek–Colbert variance control the skeleton asks for, already present). Add one push field, a blend alpha, and read the previous prefiltered value at the output texel to write an exponential-moving-average result:

```hlsl
// ibl_prefilter.slang — reconverge with an EMA blend, per texel (RMW is safe: thread writes its own texel)
struct Push { float roughness; float blendAlpha; };   // blendAlpha = 1 → hard set (first bake), < 1 → blend
[[vk::binding(1, 0)]] RWTexture2DArray<float4> outCube;    // the persistent prefiltered mip (read + write)
...
float3 prev = outCube[tid].rgb;
outCube[tid] = float4(lerp(prev, prefiltered, push.blendAlpha), 1.0);   // EMA toward the new sky
```

Because each thread reads and writes only its own texel, the in-place read-modify-write on the storage image is hazard-free (no cross-texel dependency). On the first bake `blendAlpha = 1.0` (a straight set); on a live reconvergence `blendAlpha ≈ 0.15–0.3` per full pass, so ~9 frames of partial refreshes converge without a pop.

**Time-slicing (Rust side, `ibl.rs` / `renderer.rs`).** The 5-mip chain (`IBL_PREFILTER_MIPS = 5`) is spread across ~9 frames on a rotating schedule: the coarse mips (128²…16², few texels) are refreshed one-per-frame; the expensive mip 0 (256²) is split across several frames by processing a face/row subset per frame (the dispatch already goes per-face via `groups × groups × 6`). A `prefilter_schedule` cursor on `Ibl` advances each frame the reconvergence is armed, so the per-frame cost is bounded to one slice. The prefiltered cube is never re-allocated — only its contents refresh — so `prefiltered_cube_view()`, the mesh set-3 binding 1, the reflection-probe fallback, and `IblPrefilterMaxMip` all stay valid throughout.

| What | File | Symbols |
|---|---|---|
| Prefilter EMA blend | `engine/assets/shaders/ibl_prefilter.slang` | `computeMain`, `Push { roughness, blendAlpha }`, the existing FIS `mip` select (kept), the RMW blend |
| Time-slice schedule + per-frame dispatch | `engine/crates/rendering/src/ibl.rs`, `renderer.rs` | `Ibl::prefilter_schedule` (new cursor), the per-mip dispatch loop (moved out of `bake` into a per-frame graph pass), `IBL_PREFILTER_MIPS`, `prefiltered_cube_view` |

> Decision to record when building: **reconvergence blends into the *persistent* prefiltered cube; there is no scratch/swap cube.** The EMA-into-place is what makes a moving sun's reflections crossfade rather than pop, and it needs no second allocation — the read-modify-write on each texel is the temporal history. A hard per-mip swap (the `wait_idle` bake's behavior) is exactly the pop this replaces.

## 3 — Cost-asymmetric per-frame scheduling

The two halves are driven differently each frame, from the renderer's frame-graph assembly (mirroring where the Phase-1 env-cube re-derive is scheduled):

- **SH diffuse — every frame `atmosphere_live()`.** The projection pass (§1) runs each frame the baked source is the atmosphere (`Ibl::atmosphere_live()`), so the diffuse ambient tracks the live sun continuously. It is cheap enough that no gating is needed beyond the atmosphere-live check. When the source is procedural/equirect (a fixed environment), the projection runs once on the bake and then rests — the coefficients are already correct and unchanging.
- **Specular reconvergence — gated + amortized.** Armed only when the sun (or atmosphere params) has moved past the Phase-1 `should_rebake` sun-angle epsilon; once armed, the time-slice cursor (§2) advances for the cadence window and then rests. The cadence — how many frames the reconvergence spreads over — is the capture-cadence knob (§6).

| What | File | Symbols |
|---|---|---|
| Atmosphere-live gate + sun-move threshold | `engine/crates/rendering/src/ibl.rs` | `Ibl::atmosphere_live`, `should_rebake` (Phase-1 sun-angle epsilon), `baked_sun`, `rebake_pending` |
| Per-frame scheduling of the SH + prefilter passes | `engine/crates/rendering/src/renderer.rs` | the `fire_rebake` region (~L4916, now schedules the live convolution passes instead of a `wait_idle` bake), `scene_ibl`/`scene_ibl_mut`, `add_compute_pass` |

## 4 — Dynamic-sky GI retint

Delete the flat sky feed and route the live SH into the two GI consumers.

**Remove the CPU flat feed.** In `render_scene.rs`, delete the `ddgi_sky = environment.ambient_color * ambient_intensity` block (~L685–695) and the `sky_color` argument. Drop the `sky_color: Vec3` param from `SceneRenderer::set_ddgi_scene` (trait, `RendererScene` forward ~L267, and the `RecordingRenderer` mock ~L1636), from `Renderer::set_ddgi_scene` (~L1917), and from `Ddgi::set_scene` (param + the `Ddgi::sky_color` field). The DDGI push's `skyColor` vec4 (`ddgi.rs` ~L553, `.rgb` = sky, `.w` = round-robin offset) becomes a dedicated `budgetOffset` field carrying only the offset — the `.rgb` is gone.

**Bind the SH into the trace.** Add the `sh_coeffs` buffer as a new binding in the DDGI trace descriptor set (set 2, next free binding after `b2 = rayOut`), and in `ddgi_trace.slang` replace the ray-miss `radiance = pc.skyColor.rgb;` (~L205) with `radiance = shRadiance(dir, shCoeffs);` — the raw-SH (no cosine kernel) reconstruction of band-limited sky radiance along the escaping ray direction. This tints the miss with the actual horizon/zenith gradient of the live sky, so probes retint with time-of-day and the multi-bounce that re-gathers missed rays (the `bounce` term) propagates the atmosphere inward.

**GI-resolve fallback.** `gi_resolve.slang` binding 3 (`irradianceMap`, the analytic IBL diffuse fallback where the DDGI cage does not cover) becomes the `shCoeffs` buffer, reconstructed with `shIrradiance(n)` — the same analytic diffuse the mesh uses. `write_gi_resolve_shared` (~L5996) binds the SH buffer instead of `scene_ibl().irradiance_cube_view()`.

| What | File | Symbols |
|---|---|---|
| Delete flat feed + retire `sky_color` plumbing | `engine/crates/assets/src/render_scene.rs`, `engine/crates/rendering/src/renderer.rs`, `ddgi.rs` | `ddgi_sky` (removed), `SceneRenderer::set_ddgi_scene` (param dropped, incl. mock), `Renderer::set_ddgi_scene`, `Ddgi::set_scene`, `Ddgi::sky_color` (removed), push `budgetOffset` |
| DDGI miss reads SH | `engine/assets/shaders/ddgi_trace.slang` | `pc.skyColor` (removed), `shCoeffs` (new binding), the ray-miss `radiance` (~L205), `shRadiance` |
| GI-resolve fallback reads SH | `engine/assets/shaders/gi_resolve.slang`, `engine/crates/rendering/src/renderer.rs` | binding 3 `irradianceMap` → `shCoeffs`, `analyticIrr`; `write_gi_resolve_shared` bind |
| Shared cage sampler (unchanged) | `engine/assets/shaders/giprobe.slang` | `ddgiSampleIrradiance` (reads the *retinted* atlas downstream — no change here) |

> Decision to record when building: **the DDGI miss reads band-limited SH *radiance*, not the diffuse irradiance.** A miss ray sees sky radiance $L(\mathbf d)$, so it reconstructs the raw $L_{lm}$ (no $A_l$ cosine kernel); applying the diffuse kernel here would double-cosine the bounce. Order-2 SH loses the sun disc, which is correct for a soft ambient miss term — the sun's *direct* contribution to probes is the separate shadowed sun term already in `ddgi_trace.slang`, unchanged.

## 5 — Consumer parity and the prefilter contract

Confirm every specular/diffuse consumer reads the reconverged resources and that the mip contract holds — this is a verification section, not new code, but it is part of "done":

- **Reflection-probe fallback** (`lighting.slang` ~L835–840, the nearest-covering-probe blend over the global IBL) reads `prefilteredMap` (set-3 binding 1) for its global specular fallback and the SH buffer for its global diffuse fallback — both now the reconverged/live resources. The per-probe irradiance/specular cubes (`probeIrradiance`/`probeCubes`, set-3 bindings 3–4, captured by `ReflectionProbes`) are a *separate* local-capture system and are **out of scope** here (they follow the reflection-probe recapture path, not the sky-light capture) — note the boundary so a builder does not conflate them.
- **ReSTIR is not an environment consumer.** Verified by grep: no `restir_*.slang` binds `prefilteredMap`/`irradianceMap`/`envCube` — ReSTIR resolves punctual cluster lights only. The skeleton's "ReSTIR fallback reads the reconverged specular" describes a consumer that does not exist; record this so the phase does *not* invent a binding. The reconverged specular reaches ReSTIR-lit surfaces through the *mesh* ambient block (set-3 binding 1), which is the real path.
- **`IblPrefilterMaxMip` parity.** `lighting.slang`'s `IblPrefilterMaxMip = 4.0` must remain `IBL_PREFILTER_MIPS − 1 = 4`; the reconvergence keeps the persistent 5-mip cube, so parity is unchanged — assert it still holds after the refactor (the `prefilterLod` curve depends on it).

## 6 — Capture-cadence knob (scene state + protocol + control + panel)

One scalar field on `AtmosphereSettings` controls how many frames the specular reconvergence spreads over (1 = every frame, higher = cheaper/laggier; default ≈ 9). It rides the existing `set-atmosphere` command (extended in Phase 1) — the field-add pattern, not a new DTO/command.

| What | File | Symbols |
|---|---|---|
| Scene state field + Default | `engine/crates/scene/src/environment.rs` | `AtmosphereSettings` (add `sky_capture_cadence: f32`), its `Default` |
| Serde round-trip (both halves) | `engine/crates/scene/src/serde.rs` | `atmosphere_to_json`, `atmosphere_from_json` (default when absent) |
| Renderer mirror | `engine/crates/rendering/src/ibl.rs` | `AtmosphereParams` (add the cadence), `AtmosphereParams::default`, threaded into the §2/§3 schedule |
| Wire DTO field + merge | `engine/crates/protocol/src/dto.rs`, `engine/crates/control/src/commands_scene.rs` | `SetAtmosphereParams` (`sky_capture_cadence: Option<f32>`), the `set-atmosphere` handler merge branch |
| Hand-authored schema | `engine/crates/protocol/src/schema.rs` | `AtmosphereSettingsDto` block + its `required` array |
| Committed generated artifacts | `schemas/control/openrpc.generated.json`, `schemas/control/command-manifest.generated.json` | regenerated via `cargo run -p xtask -- gen-protocol` |
| Editor panel row | `editor/src/panels/EnvironmentPanel.tsx`, `editor/src/control/client.ts` | `patchAtmos("skyCaptureCadence", …)`, `recordAtmosEdit`, `setAtmosphere` (generic; no new wrapper) |

Because the field adds to `AtmosphereSettings` (no new wire enum), the DTO tripwire set is the narrow field-add one: `dto.rs` + the hand-authored `AtmosphereSettingsDto` schema + the `set-atmosphere` merge + regenerate. `DTO_TYPE_NAMES`/`codegen`/`inventory`/`schema_fragments` are unchanged (no new type). `sa set-atmosphere --skyCaptureCadence N` flows through the generated manifest with no per-command CLI code.

> Decision to record when building: **the cadence is a scalar frame-count on `AtmosphereSettings`, carried by `set-atmosphere`, not a new enum or a new command.** SH diffuse is unconditionally per-frame (no knob); the knob governs only the expensive specular reconvergence, where a real cost/latency trade exists. Keeping it on the atmosphere block (one write path) mirrors how the atmosphere is already authored per-scene.

## 7 — Docs

Add an image-based-lighting explanation page for the real-time capture and a hub row.

| What | File | Symbols |
|---|---|---|
| New page: real-time sky-light capture | `docs/content/explanations/image-based-lighting/realtime-skylight-capture.md` (new) | `sh_project.slang`, `ibl_prefilter.slang` (reconvergence), `Ibl::sh_coeffs`, `ddgi_trace.slang` SH miss |
| Hub row | `docs/content/explanations/image-based-lighting/_index.md` | the `## Pages` table (a new row; retire/repoint the `diffuse-irradiance` row → SH) |

The page (TOML front matter, `math = true`) explains: the Ramamoorthi 9-coefficient SH diffuse ($A_0,A_1,A_2$) replacing the irradiance cube, the amortized specular reconvergence (time-slice + EMA blend, single graphics queue), the cost asymmetry (SH per-frame vs specular gated), and the GI retint — with the slim `What | File | Symbols` table. Because `ibl_irradiance.slang` is deleted, retire the `diffuse-irradiance` hub row (repoint it to the SH page in the same change — NO-LEGACY in the docs too).

## Out of scope (later phases / unscheduled)

- **Time-of-day driver + night sky (Phase 3).** This phase makes the SH/specular/GI *follow* whatever sun the atmosphere holds; the ephemeris/curve driver that *animates* the sun over a day, and the star/moon night content, are Phase 3. The moon coupling from Phase 1 already flows into the SH for free (the env cube it lit is what gets projected).
- **RT sky GI for the sun's moving indirect bounce (unscheduled).** SH diffuse retints *sky-color* GI, but does not capture the sun's moving *bounced* indirect; the DDGI probe field already accumulates that from its shadowed sun term. A ray-traced sky-GI probe reconvergence (RTXGI) is the end state when the ray budget allows — noted, not built.
- **Per-probe reflection irradiance → SH.** The per-entity reflection-probe local irradiance stays a captured cube here; converting probe capture to SH is a reflection-probe change, not a sky-light change.
- **Async-compute queue.** Time-slicing on the single graphics queue is the shipped win; a dedicated async queue overlapping the convolution with raster is a future optimization, deliberately not built.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Then, on the NVIDIA GPU:

- **Validation-clean headless boot** (`just run-engine-headless`) with the atmosphere enabled, logging zero Vulkan validation errors — the SH projection, the reconvergence RMW blend, and the retired irradiance cube must all pass sync/layout validation.
- **An `sa` probe that changes the frame.** With `sa set-atmosphere --enabled true`, move the sun (`sa set-light --direction {…}`) and confirm across a short capture that (a) the diffuse ambient retints *immediately* (SH per-frame), (b) the reflections retint *smoothly over ~9 frames* (amortized specular, no pop), and (c) the DDGI indirect retints with the sky — a screenshot diff against the pre-move frame must differ in all three. Then `sa set-atmosphere --skyCaptureCadence 1` vs a larger value must visibly change the specular reconvergence speed.
- **`bun run check` in `editor/`** — a wire type changed (`AtmosphereSettings`/`SetAtmosphereParams` gains `skyCaptureCadence`); regenerate `@saffron/protocol` (never hand-edit `sa-types.ts`) and typecheck the panel edit.
- **`just e2e`** — add a `tests/e2e` case driving `set-atmosphere` with `skyCaptureCadence` over the control plane, asserting the `EnvironmentDto` echo carries it and the log is validation-clean.
- **Docs** — the new `realtime-skylight-capture.md` page + the repointed hub `_index.md` row, in the same change.

## References

Sky-light IBL — SH diffuse:

- Ramamoorthi & Hanrahan — *An Efficient Representation for Irradiance Environment Maps* (9-coefficient SH, $A_0=\pi$, $A_1=\tfrac{2\pi}{3}$, $A_2=\tfrac{\pi}{4}$): https://cseweb.ucsd.edu/~ravir/papers/envmap/envmap.pdf
- King — *Real-Time Computation of Dynamic Irradiance Environment Maps* (GPU Gems 2 Ch.10, GPU SH projection per frame): https://developer.nvidia.com/gpugems/gpugems2/part-ii-shading-lighting-and-shadows/chapter-10-real-time-computation-dynamic

Specular prefilter reconvergence + real-time capture:

- Karis — *Real Shading in Unreal Engine 4* (split-sum prefilter + filtered importance sampling + BRDF LUT): https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf
- Zero Radiance — *Alternative Take on the Split Sum Approximation for Cubemap Pre-filtering* (filtered importance sampling detail): https://zero-radiance.github.io/post/split-sum/
- Unreal Engine — *Sky Lights* (Real Time Capture, TimeSlicing over ~9 frames, SH + specular cost breakdown): https://dev.epicgames.com/documentation/unreal-engine/sky-lights-in-unreal-engine?lang=en-US

Dynamic-sky GI:

- Majercik et al. — *Dynamic Diffuse Global Illumination with Ray-Traced Irradiance Fields* (DDGI; the sky as ray-miss radiance, hysteresis blend): https://www.jcgt.org/published/0008/02/01/paper-lowres.pdf

Atmosphere context (Phase 1 substrate):

- Hillaire 2020 — *A Scalable and Production Ready Sky and Atmosphere Rendering Technique*: https://sebh.github.io/publications/egsr2020.pdf
