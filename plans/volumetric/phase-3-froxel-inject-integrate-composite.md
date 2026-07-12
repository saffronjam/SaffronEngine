# Phase 3 — Froxel volumetric fog: inject, integrate, composite

**Status:** COMPLETED — §0 foundation (the `lighting.slang` public refactor + `mesh.slang`/`meshlet.slang` migration, verified byte-identical), the froxel volumes/descriptors, `fog_inject.slang`, `fog_integrate.slang`, the `FogMode` volumetric branch + composite, the `FogMode`/`base_density`/`scatter_albedo`/`phase_g` protocol+control+editor surface, and §8 (the fog debug `ViewMode::Fog` — the composite outputs the froxel in-scatter + opacity directly, surfaced over the `set-view-mode` path + the editor View Modes menu; and forward transparent fog — the übershader samples the `froxelIntegration` volume bound at light-set binding 11, gated by `LightUbo::froxel_fog`, so translucents receive the same volumetric fog) are all built and green. Verified: `just engine`/`just lint` clean, protocol + `LightUbo` byte-layout tests pass, `bun run check` clean, and a direct-GPU drive on the RTX 3070 Ti (`set-fog --mode volumetric` round-trips, `set-view-mode fog` echoes `{"viewMode":"fog"}`, a fog-view screenshot renders) ran with **zero** Vulkan validation errors. The fog `e2e` case + the docs "Fog" page are updated. The debug view rides `set-view-mode` (the ViewMode path), not `set-debug-overlays` (that DTO is world-overlay toggles) — the plan's §8 `set-debug-overlays` phrasing was imprecise; `MotionVectors` is the exact template followed.

Part of `plans/volumetric/` (volumetric + height fog: an analytic exponential base unified with a Wronski-2014 / Hillaire-2015 froxel pipeline, composited into scene-linear HDR before bloom and tonemap). This is the marquee phase: it stands up the two compute shaders (`fog_inject.slang`, `fog_integrate.slang`) that fill the froxel scatter/integration volumes built in Phase 2, folds a froxel-sampling branch into the Phase-1 fog composite pass, and wires the three passes into the frame graph after light-cull and before motion/TAA/bloom/tonemap. It depends on Phase 1 (the `FogSettings` scene state, `set-fog` command, and the closed-form composite pass) and Phase 2 (the 3D transient volumes, the 3D `groups_z` dispatch path, `froxel_fog.rs`, and the promoted public `lighting` module with `hgPhase`). It does NOT add temporal reprojection, per-light volumetric flags, local `FogVolume` entities, or aerial perspective — those are Phases 4, 5, and 6 and this phase deliberately leaves their seams clean without building their infra (no history ping-pong, no per-light `GpuLight` bits, no `FogVolume` component, no atmosphere-LUT ray-march).

## Goal

Turn on true participating media: shadowed sun in-scatter yields god-rays and volumetric shadows for free, point/spot lights bloom haloes into the haze, and a single `fog.mode` flag selects between the Phase-1 closed form and the froxel path with no double-counting. Concretely:

- **`fog_inject.slang` (new)** — one thread per froxel: reconstruct the (jittered) froxel-center world position, evaluate `sigma_t` (Phase-1 analytic height density as the base medium + `base_density`) and `sigma_s = albedo * sigma_t`, map the froxel view position to the containing cull cluster via the promoted `clusterIndexFor`, read that cluster's light list, and accumulate `radiance * attenuation * shadow * hgPhase(dot(view,l), g) * sigma_s` over the directional (cascade PCF + optional RT ray-query), spot, and point lights, plus atmosphere/IBL ambient and emissive; write `(L_scat.rgb, sigma_t)` into the `scatterExtinction` volume (`StorageImageRwCompute`).
- **`fog_integrate.slang` (new)** — one thread per XY column marching front-to-back along Z with the energy-conserving analytic slice `S_int = (S - S*exp(-sigma*d)) / max(sigma, 1e-5)`, `accum += totalT * S_int`, `totalT *= exp(-sigma*d)`, storing `(accum.rgb, totalT)` per slice into the `integration` volume — Hillaire's fix for Wronski's high-density banding.
- **Composite folded into the Phase-1 fog pass** — `height_fog.slang` gains a `volumetric` branch that samples `integration` at `(screenUV, w = log(viewZ/near)/log(far/near))` trilinearly, applies `out = scene*T_froxel + inScatter_froxel`, then layers the analytic height fog beyond `fogFar` on the shared transmittance ledger `T_total = T_froxel * T_height`.
- **`FogMode` on `FogSettings` + `SetFogParams`** — `analytic` runs the Phase-1 closed form unchanged; `volumetric` injects the same height density as the base medium into the froxel grid instead of applying the closed form twice (NO-LEGACY: the analytic term is a strict subset of the froxel path, so the two never overlap).
- **A fog debug `ViewMode`** (alongside `MotionVectors`) visualizing froxel transmittance / in-scatter, reachable over the existing debug-view path from `sa` and the editor Render panel.

## Design stance (grounded in current engine practice)

The engine already ships every input this pass consumes: the clustered light-cull produces per-cluster light lists into `cluster_buffer` and the punctual light SSBO into `light_list` (`engine/crates/rendering/src/lighting.rs`, `Lighting::cluster_buffer_with_size`, `light_list_buffer`), ReSTIR already binds those two SSBOs into a standalone compute set and reconstructs the cluster with its own `clusterIndexFor` (`engine/crates/rendering/src/restir.rs` ~l.648, `engine/assets/shaders/restir_initial.slang`), and all four shadow families are bind-ready in the light set (b4 directional PCF, b5 spot PCF, b6/b7 point cubes, set6 b0 TLAS via `rayQueryShadow`) written by `write_shadow_samplers`. The froxel volumes, the exponential-Z depth mapping (`FROXEL_GRID_{X,Y,Z}` + `FogGridParams`, matching `light_cull.slang`'s `tileNear = -near*pow(far/near, gz/gridZ)`), the 3D transient acquire path, the `groups_z` dispatch, and the promoted public `lighting` module with `hgPhase` were all built in Phase 2. So the injection shader is `lighting.slang` minus the Cook-Torrance BRDF plus a Henyey-Greenstein phase, and this phase is *assembly* of existing seams, not novel plumbing — with one genuinely new capability exercised for the first time: a 3D compute dispatch writing an `RWTexture3D` transient volume through the render graph.

**Injection — the modern-correct Wronski/Hillaire froxel density-and-scatter pass, not a screen-space godray post.** The industry-correct destination for volumetric light shafts is a frustum-aligned froxel grid that evaluates participating-media in-scatter per froxel from the *same* clustered light list the opaque pass uses (Wronski 2014; Hillaire, Frostbite 2015), so shadowed sun in-scatter produces god-rays and volumetric shadows as a natural consequence of the density field rather than a radial screen-space blur keyed to a bright pixel (Mitchell, GPU Gems 3 — explicitly NOT offered here as a parallel path). Reusing `cluster_buffer` / `light_list` / the shadow maps means shafts are physically consistent with scene lighting and shadowing for free; the only new per-froxel term is the `hgPhase` anisotropy lobe promoted in Phase 2.

**Integration — Hillaire's energy-conserving analytic slice, not Wronski's naive Beer-Lambert accumulation.** Front-to-back Z marching stores in-scatter *scaled by the analytically-integrated transmittance across each slice* (`S_int = (S - S*exp(-sigma*d))/max(sigma,1e-5)`), which removes the density-dependent banding of summing `S*T` at slice centers (Hillaire 2015; Godot `volumetric_fog_process.glsl` MODE_FOG). The cheaper midpoint sum is deliberately not offered.

**Composite — one operator, one transmittance ledger, strictly before bloom.** The froxel branch and the analytic branch share the single `color*T + inScatter` operator and the single `(inScatter.rgb, transmittance.a)` storage convention Phase 1 established, so `volumetric` mode simply swaps the near/mid term's *source* (sampled volume vs. closed form) while the far field is always the analytic layer on the shared ledger `T_total = T_froxel * T_height`. Fog stays before bloom so distant bright emitters are attenuated first and the energy-conserving bloom pyramid reads the already-fogged HDR (Chetan Jags — pre-tonemap bloom ordering); putting bloom first would punch physically-wrong halos through the fog.

> **light-cull → fog_inject (density + shadowed in-scatter per froxel) → fog_integrate (front-to-back energy-conserving Z march) → fog composite (trilinear sample + `scene*T_froxel + inScatter`, then analytic far field on the shared ledger) → motion/TAA/bloom/tonemap.** The inject/integrate volumes are per-frame transient scratch; the composite reads them same-frame and everything stays scene-linear `rgba16f` before display space is ever touched.

## NO-LEGACY checklist for this phase

- `fog.mode` is **the** selector between analytic and volumetric — one `FogMode` enum on `FogSettings`, one `mode` field on `SetFogParams`, one branch in `height_fog.slang`. The volumetric path injects the height density as the base medium; it never re-runs the closed form on top (no `T_height` applied twice). There is no second "volumetric enable" boolean and no parallel fog command.
- The `lighting.slang` helpers are extracted to the public module **once** (§0), and `mesh.slang` + `meshlet.slang` migrate to it in the same change — no file-private copy left behind, no shader reading fixed bindings the fog pass can't reuse. The Phase 2 §6 `froxel_debug*.slang` fixture is deleted here (its purpose was proving the transient-3D path; the real passes supersede it).
- The three passes and the composite branch live in exactly one place per layer: the two shaders under `engine/assets/shaders/`, the pass-build + binding code in `engine/crates/rendering/src/froxel_fog.rs`, the graph wiring in `renderer.rs`, and the scene-state selector in `engine/crates/scene/src/environment.rs::FogSettings` (round-tripped by `fog_to_json`/`fog_from_json` in `serde.rs`). Fog is SceneEnvironment state, not `RenderSettings`, so persistence is automatic through the environment block — no `RenderSettings` field, no `ControlRenderer` setter.
- `SceneRenderer::submit_fog` (added in Phase 1) is extended with the froxel path; the mock `SceneRenderer` in the `render_scene.rs` test harness gains the same behaviour in the same change — no stub left rendering only the analytic form.
- The new `FogMode` enum is registered in *all* protocol tripwire lists (`dto.rs`, `command.rs` `DTO_TYPE_NAMES`, `codegen.rs` `ts_decls`/`struct_fragments`, `tests/inventory.rs`, `tests/schema_fragments.rs`, the hand-authored `schema.rs` `FogSettingsDto` + `xtask` `component_block.ts`/`environment_dto.ts`) in one change — the build and the schema-fragment byte-equivalence test refuse to compile otherwise, by design. `set-fog` already exists (Phase 1); this phase only extends its merged fields, it does not add a command.

## 0 — Foundation: froxel volumes + the shared `lighting` module (relocated from Phase 2)

Build these two before the injection shader — they are its prerequisites, relocated here from Phase 2 so a NO-LEGACY refactor and its unexercised scaffolding land atomically with their first consumer. Follow the authoritative specs in `phase-2-froxel-infra-3d-transient.md` §4 and §5; the summary:

- **The persistent volumes + descriptor sets (Phase 2 §4).** In `engine/crates/rendering/src/froxel_fog.rs`, add the `FroxelFog` resource: the persistent `scatterExtinction` ping-pong (`scatter: [Image3D; 2]`, `STORAGE | SAMPLED`, `GENERAL`-written / linear-sampled — the GDF albedo pattern) swapped each frame, the host-mapped `FogGridParams` UBO, and the write/sample descriptor sets. The `integration` volume is the same-frame `acquire_image_3d` transient from Phase 2 §1. Add the froxel set layouts in `descriptors.rs` (STORAGE_IMAGE write binding + COMBINED_IMAGE_SAMPLER read binding + the UBO) and bump the `STORAGE_IMAGE` / `COMBINED_IMAGE_SAMPLER` pool budgets by the froxel set count. Build the PSOs via `Pipelines::build_compute_multi` (bindless set + froxel set) / `build_compute`.

- **The `lighting.slang` shared-module refactor (Phase 2 §5).** Promote `lighting.slang`'s file-private light/shadow helpers (`clusterIndexFor`, `distanceAttenuation`, `punctual`, `pcfShadow`, `pointShadow`, `rayQueryShadow`) into a **public, resource-parameterized** surface (the `giprobe.slang` shape: no bindings in the module, resources passed as parameters), and add a public `hgPhase(cosTheta, g)` lifted from `atmos_skyview.slang`. Migrate `mesh.slang` and `meshlet.slang` to the public surface **in the same change** — they pass their existing bindings as arguments and the shade result is byte-identical. This is the atomic cutover: extract, add the fog caller (§1), and prove mesh/meshlet render identically, together. The debug froxel-fill (`froxel_debug*.slang`) from Phase 2 §6 is removed in this same change once the real inject/integrate passes write the volumes (NO-LEGACY).

## 1 — Froxel injection shader

**File `engine/assets/shaders/fog_inject.slang` (new).**

Model the consumption of the cluster + light SSBOs on `restir_initial.slang` (a compute shader that binds `light_list` at b1 and `cluster_buffer` at b2 and reconstructs the cluster with its own `clusterIndexFor`), and the light/shadow evaluation on `lighting.slang` — but call the *promoted public* helpers (`clusterIndexFor`, `distanceAttenuation`, `punctual`, `pcfShadow`, `pointShadow`, `rayQueryShadow`, `hgPhase`) that §0 extracts into the shared `lighting` module, so this shader `import lighting` and binds the same descriptor sets (light globals/lights/clusters/`ClusterParams` at set 1, the shadow images at b4/b5/b6/b7, the TLAS at set6 b0). The dispatch is one thread per froxel over `FROXEL_GRID_{X,Y,Z}` (3D `groups_z` path from Phase 2). Substitute `radiance * hgPhase(cosTheta, g)` for `punctual`'s `brdf() * ndotl`: fog scatters, it does not shade a surface.

Reconstruct the froxel-center world position from the froxel integer coordinate using `FogGridParams` (the exponential-Z view distance `viewZ = -near*pow(far/near, (z+0.5)/gridZ)` mirrored from `light_cull.slang`, then screen-tile → view ray → world via the inverse view-proj), evaluate density, and accumulate:

```hlsl
// fog_inject.slang — one thread per froxel (SV_DispatchThreadID.xyz)
// bindings mirror the mesh light set (set 1) + TLAS (set 6); volume at set 2
[[vk::binding(0, 2)]] RWTexture3D<float4> scatterExtinction; // rgb = L_scat, a = sigma_t

float4 injectFroxel(uint3 froxel) {
    float3 worldPos = froxelCenterWorld(froxel, gridParams);   // exp-Z + inv view-proj
    float3 viewDir  = normalize(gridParams.eyeWorld - worldPos);

    // participating medium: Phase-1 analytic height density is the BASE medium here,
    // NOT applied again at composite (fog.mode == volumetric)
    float sigma_t = analyticHeightDensity(worldPos, fog) + fog.baseDensity;
    float sigma_s = fog.albedo * sigma_t;                      // albedo ~0.9

    // read the containing cull cluster's light list (coarse 16x9x24 grid)
    uint  cluster = clusterIndexFor(worldPos, clusterParams);
    float3 Lscat  = 0.0;
    for (uint i = 0; i < clusters[cluster].count; ++i) {
        GpuLight li = lights[clusters[cluster].indices[i]];
        float3 toL; float atten; /* distanceAttenuation + spot cone from the shared module */
        float  sh   = shadowFor(li, worldPos);                 // pcf / pointShadow / rayQuery
        float  ph   = hgPhase(dot(viewDir, toL), fog.phaseG);  // lifted from atmos_skyview
        Lscat += radiance(li) * atten * sh * ph * sigma_s;
    }
    // directional sun: cascade PCF is the default; optional RT ray-query for crisp shafts
    Lscat += sunInScatter(worldPos, viewDir, sigma_s, fog.phaseG);
    // ambient: atmosphere/IBL * sigma_s, plus emissive medium
    Lscat += (iblAmbient(viewDir) * sigma_s) + fog.emissive;

    return float4(Lscat, sigma_t);
}
```

> Decision to record when building: **the coarse 16×9×24 cull cluster feeds the fine fog froxel's light list via `clusterIndexFor`, rather than running a dedicated fog-froxel cull.** The cull grid is already exponential-Z and its list is conservative per cluster; a finer fog grid mapping each froxel to the containing cluster reuses that culling exactly (the ReSTIR pattern), which is correct and free. A dedicated fog cull is a Future refinement only if coarse per-cluster culling proves inadequate — it is not offered as a parallel path now.

> Decision to record when building: **the sun's default shadow is cascade PCF (`pcfShadow` + `shadow_view_proj`), with the inline TLAS ray-query (`rayQueryShadow`) gated off by default.** One ray per froxel per light is expensive; the PCF path gives correct volumetric shadows, and RT crisp shafts become an opt-in in Phase 4 (per-light `castVolumetricShadow`). This phase wires the ray-query call site but leaves it behind a compile/quality gate so the default headless boot stays on PCF.

## 2 — Energy-conserving integration shader

**File `engine/assets/shaders/fog_integrate.slang` (new).**

One thread per XY column (`FROXEL_GRID_X * FROXEL_GRID_Y` invocations, `groups_z = 1`), marching front-to-back along Z and writing the running accumulation into the `integration` volume. This is the Hillaire slice, not a midpoint sum:

```hlsl
// fog_integrate.slang — one thread per (x,y) column, serial over z
[[vk::binding(0, 2)]] Texture3D<float4>   scatterExtinction; // read (L_scat, sigma_t)
[[vk::binding(1, 2)]] RWTexture3D<float4> integration;        // write (accum, transmittance)

void integrateColumn(uint2 xy) {
    float3 accum = 0.0; float totalT = 1.0;
    for (uint z = 0; z < FROXEL_GRID_Z; ++z) {
        float4 se    = scatterExtinction[uint3(xy, z)];
        float  sigma = max(se.a, 0.0);
        float  d     = sliceThicknessWorld(z, gridParams);     // exp-Z slice length
        float  sliceT = exp(-sigma * d);
        // energy-conserving analytic in-scatter across the slice (Hillaire)
        float3 S_int = (se.rgb - se.rgb * sliceT) / max(sigma, 1e-5);
        accum  += totalT * S_int;
        totalT *= sliceT;
        integration[uint3(xy, z)] = float4(accum, totalT);
    }
}
```

> Decision to record when building: **integration writes `(accum.rgb, totalT)` per slice into a separate `integration` volume, not in-place into `scatterExtinction`.** Keeping the scatter/extinction inputs live is what lets Phase 4 reproject the *linear* scatter+extinction (never the non-linear integrated transmittance — Hillaire's energy note). A single in-place volume would foreclose the correct temporal blend; the two-volume split is the modern-correct shape and both are transient this phase (history is Phase 4).

## 3 — Froxel pass build and bindings

**File `engine/crates/rendering/src/froxel_fog.rs`.**

Phase 2 stood up the module (grid constants, `FogGridParams` UBO, the exponential-Z CPU mirror + `froxel_grid_matches_shader` test, the descriptor layouts, and the transient `scatterExtinction` + `integration` volume acquisition). This phase adds the binding and PSO glue: build the inject/integrate compute PSOs (`Pipelines::build_compute_multi` for the inject shader — bindless set0 + the fog volume set + the reused light set; `build_compute` for integrate), and bind `Lighting::cluster_buffer_with_size` + `light_list_buffer` + the shadow samplers (`write_shadow_samplers`) + the TLAS into the inject descriptor set, mirroring how `restir.rs` binds `light_list` (b1) and `cluster_buffer` (b2). Mirror `global_sdf.rs`'s `Image3D` write/read convention: the `scatterExtinction` and `integration` volumes are written as `STORAGE_IMAGE` (GENERAL) and the composite samples `integration` as `COMBINED_IMAGE_SAMPLER` through a linear sampler (`create_linear_repeat_sampler`).

```rust
// froxel_fog.rs — bind the reused cull/light/shadow rig + the fog volumes
pub struct FroxelFog {
    inject_pso: ComputePipeline,     // build_compute_multi: bindless + fog + light sets
    integrate_pso: ComputePipeline,  // build_compute: fog set only
    grid_params: Buffer,             // FogGridParams UBO (Phase 2)
    // scatter/integration transient volume handles acquired per frame (Phase 2)
}

impl FroxelFog {
    /// Bind cluster_buffer + light_list + shadow maps (b4/b5/b6/b7) + TLAS into the
    /// inject set, exactly as restir.rs binds the froxel candidate lists.
    fn write_inject_set(&self, lighting: &Lighting, /* shadow images, tlas */) { /* ... */ }
}
```

> Decision to record when building: **the inject shader binds the same descriptor sets the mesh path uses (via the promoted `lighting` module), not a bespoke copy of the cluster/shadow math.** Phase 2's public resource-parameterized module is the single source of `clusterIndexFor`/`punctual`/`pcfShadow`/`pointShadow`/`rayQueryShadow`/`hgPhase`; duplicating them into the fog shader (as `restir_initial.slang` does today for its self-contained copy) would re-introduce the drift the Phase-2 extraction removed. One shared module, imported by mesh and fog alike.

## 4 — Graph wiring and pass ordering

**File `engine/crates/rendering/src/renderer.rs`.**

Insert three `RgPass::compute` passes immediately after the existing light-cull pass (the template is the `light-cull` pass at ~l.5249: import the buffers, declare usages, let the graph derive barriers) and before motion/TAA/bloom/tonemap. Use the 3D `groups_z` dispatch path Phase 2 added to `Renderer::add_compute_pass` (or the bespoke `RgPass::compute` body like the GDF composite at ~l.6667 dispatches `gx,gy,gz`). Import the two 3D volumes with `import_image_3d` (`render_graph.rs:494`, already tracks 3D like 2D for barriers), declaring `StorageImageRwCompute` for the writes and `SampledReadCompute` for the reads.

- **fog_inject** — access: `scatterExtinction` `StorageImageRwCompute`; `cluster_buffer`/`light_list` `StorageReadCompute`; shadow images + TLAS as read; dispatch `(ceil(X/8), ceil(Y/8), ceil(Z/4))`.
- **fog_integrate** — access: `scatterExtinction` `SampledReadCompute`, `integration` `StorageImageRwCompute`; dispatch `(ceil(X/8), ceil(Y/8), 1)` (one thread per column).
- **fog composite** — the Phase-1 fog pass, extended: `ViewTarget.depth` `SampledReadCompute` (DEPTH aspect, already set on import — there is no `DepthRead` usage variant), `integration` `SampledReadCompute`, `ViewTarget.offscreen` `StorageImageRwCompute` blend; dispatch 2D over the framebuffer.

```rust
// renderer.rs — after light-cull, before motion/TAA/bloom/tonemap
let scatter = graph.import_image_3d(self.froxel.scatter(), /* GENERAL */);
let integ   = graph.import_image_3d(self.froxel.integration(), /* GENERAL */);
graph.add_pass(RgPass::compute("fog-inject")
    .access(scatter, RgUsage::StorageImageRwCompute)
    .access(cluster_buffer, RgUsage::StorageReadCompute)
    .access(light_list, RgUsage::StorageReadCompute)
    .body(move |cmd| { /* bind inject set; cmd_dispatch(gx, gy, gz) */ }));
graph.add_pass(RgPass::compute("fog-integrate")
    .access(scatter, RgUsage::SampledReadCompute)
    .access(integ, RgUsage::StorageImageRwCompute)
    .body(move |cmd| { /* cmd_dispatch(gx, gy, 1) */ }));
// fog composite: the Phase-1 pass, now sampling `integ` in volumetric mode
```

> Decision to record when building: **fog runs strictly before the bloom pyramid and tonemap, all in scene-linear `rgba16f`.** This is the load-bearing ordering: distant bright emitters must be attenuated by fog before the energy-conserving bloom reads the HDR, or far highlights bloom at full strength and then get covered, punching physically-wrong halos through the haze (Chetan Jags). Compositing fog after bloom is not offered.

## 5 — Composite branch and the shared transmittance ledger

**File `engine/assets/shaders/height_fog.slang`.**

Phase 1's fog composite pass reads `ViewTarget.depth` and blends `scene*T + inScatter` into `ViewTarget.offscreen`. Add a `volumetric` branch gated on `fog.mode`: reconstruct the froxel W from the scene depth (`w = log(viewZ/near)/log(far/near)`, the CPU/GPU-locked inverse of the injection distance), sample `integration` trilinearly at `(screenUV, w)`, and combine on the one shared ledger:

```hlsl
// height_fog.slang composite — volumetric branch
if (fog.mode == FOG_MODE_VOLUMETRIC) {
    float  w   = log(viewZ / fog.near) / log(fog.far / fog.near);
    float4 vol = integration.SampleLevel(linearClamp, float3(screenUV, w), 0);
    float  T_froxel = vol.a;
    float3 inScatterFroxel = vol.rgb;

    // far field beyond fogFar carried by the analytic height fog on the shared ledger
    float  T_height = analyticTransmittanceBeyond(worldPos, fog);      // 1.0 within fogFar
    float3 inScatterHeight = analyticInScatterBeyond(worldPos, fog);   // 0 within fogFar

    float  T_total = T_froxel * T_height;
    float3 inScatter = inScatterFroxel + T_froxel * inScatterHeight;
    outColor = scene * T_total + inScatter;
} else {
    outColor = analyticClosedForm(scene, worldPos, fog);              // Phase-1 path
}
```

> Decision to record when building: **volumetric mode injects the height density as the froxel base medium and lets the froxel grid integrate it; the closed form is applied only *beyond* `fogFar` as the far-field fallback.** The analytic term is a strict subset of the froxel path within the grid, so `T_total = T_froxel * T_height` never double-counts the near/mid medium (`T_height == 1` inside `fogFar`). Applying the closed form over the whole ray in volumetric mode would darken the near field twice — that is the double-count NO-LEGACY forbids.

## 6 — `fog.mode` protocol DTO and command table

**File `engine/crates/scene/src/environment.rs`.** Add a `FogMode { Analytic, Volumetric }` enum (`Default::Analytic`) and a `pub mode: FogMode` field on `FogSettings`.
**File `engine/crates/scene/src/serde.rs`.** Serialize `mode` inside the existing `fog_to_json`/`fog_from_json` (a string round-trip beside the Phase-1 fields) — `document.rs` save/load picks it up through `environment_to_json`/`environment_from_json` with no extra code.
**File `engine/crates/protocol/src/dto.rs`.** Add the `FogMode` wire enum and a `mode: Option<FogMode>` field on the existing `SetFogParams`, with the full `#[derive(Serialize, Deserialize, JsonSchema)]` + `#[ts(export)]` stack, mirroring how `AtmosphereSettings`' enum-like fields are shaped. Add `FogMode` to the `inventory.rs` existence list.
**File `engine/crates/protocol/src/command.rs`.** `set-fog` already exists (Phase 1) — add `"FogMode"` to `DTO_TYPE_NAMES`; no new `COMMANDS`/`COMMAND_FIXTURES`/`scene_domain` row, but bump the `fog` fixture if its snapshot must exercise `mode`.
**File `engine/crates/protocol/src/codegen.rs`.** `decl_entry!(FogMode)` + `frag_entry!(FogMode)`; add to `tests/inventory.rs` and `tests/schema_fragments.rs`.
**File `engine/crates/protocol/src/schema.rs`.** Extend the hand-authored `FogSettingsDto` schema with the `mode` property + a `FogMode` enum schema.
**File `engine/xtask/src/protocol/component_block.ts` + `environment_dto.ts`.** Add `FogMode` + the `mode` field to the hand-authored `FogSettingsDto` TS interface.

## 7 — Control seam and renderer push

**File `engine/crates/control/src/commands_scene.rs`.** The `set-fog` handler (Phase 1) merges a partial `SetFogParams` onto `environment.fog`; extend the merge to carry `mode`, validate it, and bump `scene_version` as before. The `sa` CLI needs **no** per-command code — `sa call set-fog --mode volumetric` flows through the generated manifest.
**File `engine/crates/assets/src/render_scene.rs`.** `SceneRenderer::submit_fog` (Phase 1) already receives the fog block each frame from `scene.environment.fog`; extend the `FogRenderSettings` it forwards with `mode` (and the froxel params the renderer needs), and drive the three-pass path when `mode == Volumetric`. The mock `SceneRenderer` in the test harness gains the same field so the render-scene unit tests compile and exercise both modes.

> Decision to record when building: **fog is SceneEnvironment state driven by `submit_fog`, not `RenderSettings`/`ControlRenderer`.** Scene-wide fog round-trips in the project's `environment` block like the sky and atmosphere; there is no `ControlRenderer` setter and no `RenderSettings` field for it. The render-panel-quality path is deliberately not used.

## 8 — Transparents and the fog debug ViewMode

**File `engine/assets/shaders/` (the forward transparent shader) + `renderer.rs`.** Transparent surfaces are not in the depth prepass, so they sample the same `integration` volume in their own forward pass at their fragment's `(screenUV, w)` and apply the identical `color*T + inScatter` operator — the shared storage convention means no second code path.

**File `engine/crates/rendering/src/renderer.rs` (`ViewMode`) + `engine/assets/shaders/lighting.slang` (`evalViewMode`).** Add a `Fog` variant to the `ViewMode` enum alongside `MotionVectors`, visualizing froxel transmittance / in-scatter (sample `integration`, output `transmittance` or `inScatter` directly). Surface it through the existing debug-view path (the Render panel `DEBUG_OVERLAYS` / `set-debug-overlays` non-undoable toggle), so it is reachable from `sa` and the editor without a new command.

> Decision to record when building: **the fog debug view rides the existing `ViewMode`/debug-overlay path, not a bespoke fog visualizer.** `MotionVectors` is the template; a fog `ViewMode` variant plus one `DEBUG_OVERLAYS` row is the whole surface.

## Out of scope (later phases)

- **Temporal reprojection + Halton jitter + neighborhood clamp (Phase 4).** This phase allocates `scatterExtinction` and `integration` as per-frame transient volumes and does not add the history ping-pong. The two-volume split (§2) keeps the linear scatter+extinction available so Phase 4 can reproject them, but no reprojection, no `taa.slang` jitter reuse, and no light-clamp knobs land here.
- **Per-light volumetric flags (Phase 4).** `volumetricScattering` / `castVolumetricShadow` on `GpuLight` are Phase 4. This phase reads every culled light with a uniform contribution and keeps the sun on cascade PCF (the RT ray-query call site is wired but gated).
- **Local `FogVolume` entities (Phase 5).** `sigma_t` this phase is `analyticHeightDensity + baseDensity` only; the per-froxel loop over a global `FogVolume` upload list, soft edges, and animated 3D noise are Phase 5, injected into the same density evaluation with no special path.
- **Aerial perspective (Phase 6).** No atmosphere-LUT ray-march and no separate 32³ AP volume here; the far field beyond `fogFar` is the analytic height fog, and the shared-ledger composite (§5) is written so Phase 6 slots `T_aerial` in without restructuring.

## Known interactions (note, don't over-engineer)

- **Coarse cull grid vs. fine fog grid.** Each fine froxel reads the containing coarse 16×9×24 cluster's list via `clusterIndexFor`; note that a very large light close to a cluster boundary is conservatively included, which is fine for in-scatter. Do not add a dedicated fog cull now — revisit only if coarse culling visibly under-lights (an open question flagged in the README).
- **Sky-view LUT tint on the analytic far field.** Phase 1 stubbed tinting the analytic inscatter from the Hillaire sky-view LUT; the volumetric far field (§5) reuses that same analytic term, so the tint carries through unchanged. Finalizing cross-coherence with the sky is Phase 6 — do not re-derive it here.
- **Grid resolution is fixed this phase.** The quality-tier selection (`Z = 64/64/128`) is Phase 4. Build against the Phase-2 default grid; do not thread a tier switch through the passes yet.
- **RT-shadowed sun cost.** The ray-query path is one ray per froxel per light and is expensive; keep it gated behind the compile/quality flag and default the sun to PCF, so the headless gate stays cheap and validation-clean.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot a lit fixture scene with a shadow-casting occluder headless (`just run-engine-headless`, on the NVIDIA GPU) with a validation-clean log and confirm visible light shafts / volumetric shadows appear when fog is on: `sa call set-fog --mode volumetric --base-density 0.03 --phase-g 0.6` must change the frame relative to `--mode analytic`, and the `set-debug-overlays` fog `ViewMode` must render the transmittance / in-scatter volume. Because a wire type changed (`FogMode` + the `mode` field), run `bun run check` in `editor/` (regenerates `@saffron/protocol` — the `SetFogParams` TS type gains `mode`; never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-fog` with `mode: "volumetric"` over the control plane and asserts the `EnvironmentDto` echo carries the mode plus a validation-clean log. Expand the `docs/content/` fog concept page into "Volumetric fog" (the froxel grid, inject/integrate/composite, god-rays from shadowed in-scatter, the reuse of the cluster cull and shadow families, and the `analytic|volumetric` mode with no double-counting) and update its hub `_index.md` row, using the slim `What | File | Symbols` table: `fog_inject.slang`/`fog_integrate.slang`, `engine/crates/rendering/src/froxel_fog.rs` (`FroxelFog`), `engine/crates/rendering/src/renderer.rs` (the three fog passes + `ViewMode::Fog`), `engine/crates/scene/src/environment.rs` (`FogMode`, `FogSettings`), and `SetFogParams` — in the same change.

## References

Froxel injection + energy-conserving integration:

- Bart Wronski — *Volumetric Fog: Unified Compute Shader Based Solution to Atmospheric Scattering* (SIGGRAPH 2014): https://bartwronski.com/wp-content/uploads/2014/08/bwronski_volumetric_fog_siggraph2014.pdf
- Sebastien Hillaire — *Physically Based and Unified Volumetric Rendering in Frostbite* (SIGGRAPH 2015): https://www.ea.com/frostbite/news/physically-based-unified-volumetric-rendering-in-frostbite
- Godot — *volumetric_fog_process.glsl* (MODE_DENSITY/FOG, HG phase, Beer-Lambert integration): https://github.com/godotengine/godot/blob/4.2/servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl
- Flax Engine — *Flax Facts #14: Volumetric Fog* (RGBA16F scatter/extinction, concrete froxel impl): https://flaxengine.com/blog/flax-facts-14-volumetric-fog/

Phase function + participating media:

- Lagarde & de Rousiers — *Moving Frostbite to Physically Based Rendering 3.0* (analytic height fog + phase function): https://seblagarde.files.wordpress.com/2015/07/course_notes_moving_frostbite_to_pbr_v32.pdf
- Zero Radiance — *Sampling Analytic Participating Media*: https://zero-radiance.github.io/post/analytic-media/

God-rays + composite ordering:

- Kenny Mitchell — *Volumetric Light Scattering as a Post-Process* (GPU Gems 3; the screen-space approach we do NOT take): https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-13-volumetric-light-scattering-post-process
- Chetan Jags — *HDR Rendering: Tonemapping and Bloom* (pre-tonemap fog/bloom ordering): https://chetanjags.wordpress.com/2015/06/28/hdr-rendering-tonemapping-and-bloom/
- Volumetric Fog in Unreal Engine (UE5 docs — froxel volumetric fog, per-light scattering): https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-fog-in-unreal-engine
