# Phase 6 — Cloud integration: aerial perspective, cloud shadows, god-rays, and one transmittance ledger

**Status:** COMPLETED

Part of `plans/sky-and-volume/` (dynamic sky, time-of-day, and volumetric clouds). This is the closing phase of the cloud track: it makes the Phase-5 lit cloud raymarch *join the atmosphere the engine already renders* instead of floating in front of it. The aerial-perspective froxel (`AerialPerspective`, `AP_GRID = 32³` out to `AP_FAR_M = 32 000 m`) and the one `(inscatter.rgb, transmittance.a)` transmittance ledger in `height_fog.slang` are **shipped** — this phase reconciles clouds with them and must not rebuild either. Clouds sit *beyond* the 32 km AP froxel (which is written against opaque scene depth), so they receive aerial perspective by sampling the shared Transmittance + sky-view LUTs at the cloud hit distance — the camera→cloud in-scatter + transmittance applied exactly once, explicitly **not** a second cloud-specific AP volume that would drift from scene AP and seam at the horizon. It renders one top-down density-integrated Beer/ESM cloud shadow map from the sun view (cascaded), drives cloud self-shadow + scene-mesh shadowing + atmosphere darkening from it, and samples that map per froxel in `fog_inject.slang` so crepuscular rays through cloud gaps emerge from the *existing* froxel fog rather than a screen-space radial blur. `T_cloud` folds onto the one `height_fog.slang` ledger (`T_total = T_fog · T_aerial · T_cloud`, premultiplied, front-to-back) with no double-darkening. Finally it adds a shared global wind field + divergence-free curl advection driving the weather-map/noise scroll (animate detail, not structure), with weather cycles keyed to the Phase-3 time-of-day, and lets the Phase-1 moon light light night clouds.

It depends on **Phase 3** (the time-of-day driver that keys weather cycles and positions the sun/moon) and **Phase 5** (the lit cloud raymarch, its reduced-res `(L_cloud, T_cloud)` buffer, the transmittance-weighted mean cloud front depth, and the `submit_clouds` seam). It does **not** re-plan the sky LUT chain, the aerial-perspective froxel, the froxel/3D-transient/TAA infra, or the cloud shape/lighting — those are done. It is *reconciliation and coupling*: one new shadow map, one shared atmosphere-march module, one ledger fold, one wind field. NO-LEGACY: the Phase-5 standalone "clouds into `color`" composite is retired here and its work moves onto the shared ledger in the same change — there is never a cloud composite beside the fog/AP composite.

## Goal

Close the loop so sky, fog, aerial perspective, and clouds read as one atmosphere on one transmittance ledger:

- **Clouds receive aerial perspective from the shared LUTs, not a second volume.** During the ledger fold, camera→cloud transmittance + in-scatter are read from the same `Ibl::transmittance_view` / `multi_scatter_view` the AP froxel marches, bounded at the cloud front depth, and applied to the cloud result exactly once — so distant clouds redden and haze in lockstep with scene geometry and the sky, with no horizon seam.
- **One cascaded cloud shadow map, three consumers.** A top-down density-integrated Beer/ESM map from the sun view drives (a) the sun `DirectionalLight`'s shadow onto opaque meshes, (b) cloud self-shadow in the Phase-5 raymarch, and (c) atmosphere/froxel-fog darkening — one occluder shared by everything the sun lights.
- **God-rays from the existing froxel fog.** `fog_inject.slang` samples the cloud shadow map per froxel when accumulating the sun's in-scatter, so shafts through cloud gaps emerge from the froxel integration already built — no screen-space light-shaft pass, and shafts work toward an off-screen sun.
- **One premultiplied ledger.** `height_fog.slang` extends `T_total = T_fog · T_aerial` to `T_total = T_fog · T_aerial · T_cloud`, front-to-back, with the cloud front depth for correct depth ordering and no double-darkening.
- **A shared wind field animates detail, not structure.** One global `WindSettings` {orientation, speed, gust} advects the weather map + noise (scrolled + curl-warped), weather cycles keyed to the Phase-3 time-of-day; the Phase-1 moon `DirectionalLight` (atmosphere-light role index 1) lights clouds at night.

## Design stance (grounded in current engine practice)

**Cloud aerial perspective samples the built LUTs at the cloud distance — never a second AP volume.** The `AerialPerspective` froxel is `AP_GRID³ = 32³` out to `AP_FAR_M = 32 000 m` and its fill (`aerial_perspective.slang::computeMain`) marches against froxel-center distances reconstructed from opaque scene depth. Clouds live beyond that shell and are their own participating medium, so they have no valid data in that froxel — reading it for clouds is the double-darkening / horizon-seam trap the digest flags. The correct reconciliation reuses the *building blocks*, not the volume: sample the Transmittance LUT for `T_atm(camera→cloud)` and evaluate the same bounded in-scatter march the AP froxel runs, stopped at the cloud front depth, and apply that in-scatter + transmittance to the premultiplied cloud result exactly once (Hillaire 2020 §aerial perspective; UE `r.VolumetricCloud.HighQualityAerialPerspective` + per-sample atmospheric light transmittance). This is done once *after* the cloud raymarch, not per march step — the standard cost/quality trade.

**One atmosphere-march module, two callers — the `lighting.slang` promotion pattern.** `aerial_perspective.slang` already carries the 16-step LUT march (`sampleTransmittance` / `sampleMultiScatter` + the analytic slice `sInt = (inScatter − inScatter·stepT)/ext`). Rather than copy that loop into the cloud composite, extract it into a public resource-parameterized `atmos_ap` slang module (the same NO-LEGACY extraction Phase-3 did with `lighting.slang`): `marchAtmosphere(worldOrigin, viewDir, sunDir, distKm, transmittanceLut, multiScatterLut, ApParams) → (inScatter, transmittance)`. `aerial_perspective.slang::computeMain` calls it bounded at the froxel distance; the ledger fold calls it bounded at the cloud front distance. Extract, rewire the AP fill, delete the inline copy — in one change.

**Clouds reconcile onto the one shipped ledger; the Phase-5 standalone composite is retired.** `height_fog.slang::computeMain` is the single place independent media fold (`T = fogT·apT; L = c.rgb·T + fogInScatter + fogT·apInScatter`). Phase 5 shipped a temporary standalone "premultiply clouds into `color`" pass explicitly deferring reconciliation to here. This phase deletes that pass and folds `(L_cloud, T_cloud, d_cloud)` into the same composite: `T = fogT·apT·cloudT`, the cloud radiance carrying its own camera→cloud atmosphere and attenuated only by the nearer fog. There is exactly one composite; clouds never get a second one.

**God-rays are a consequence of the density field, not a post filter.** RDR2's crepuscular rays fall out of the frustum voxel grid: each fog froxel weights the sun in-scatter by the cloud shadow map's visibility, so gaps admit light and occluded froxels stay dark, and the front-to-back integration produces the shafts (RDR2 integrated solution). This reuses `fog_inject.slang` + `fog_integrate.slang` + the composite verbatim — the screen-space radial-blur light shaft (view-dependent, blind to off-screen suns) is not offered.

**Modern-correct, NO-LEGACY.** The cloud shadow map is a real density-integrated ESM cascade, not a baked or scrolling cloud-shadow texture; the wind field is one global source consumed by clouds and the fog inject, not a per-system drift; the AP-on-cloud is the shared-LUT sample, not a hand-tuned per-cloud fog fade. Each superseded path (the Phase-5 cloud composite) is retired in the change that supersedes it.

## NO-LEGACY checklist for this phase

- The camera→cloud atmosphere is applied **exactly once** — inside the ledger fold, from the shared LUTs at the cloud front depth. The AP froxel is never sampled for cloud pixels, and there is no second cloud-specific AP volume, resource, shader, or `ap_planes` instantiation.
- There is **one** cloud composite: the `height_fog.slang` ledger fold. The Phase-5 standalone cloud-into-`color` pass and its render-graph node are deleted in this change, and `submit_clouds` hands the cloud buffer to the composite instead of compositing it itself.
- The atmosphere LUT march lives in **one** `atmos_ap` module; `aerial_perspective.slang` imports it and its inline copy is removed. No duplicated `sampleTransmittance`/`marchAtmosphere`.
- The cloud shadow map is **one** cascaded resource with three read sites (mesh surface shadow, `fog_inject` god-rays, cloud self-shadow) — not a per-consumer copy. Beer/ESM only; no binary depth map.
- The global wind is **one** `WindSettings` block + **one** `set-wind` command; it is not smuggled onto `set-clouds` or `set-atmosphere`, and the per-`FogVolume` `wind` field (already shipped) is unaffected. Weather cycles read the Phase-3 time-of-day scalar, not a second clock.
- Every new/extended wire type (`SetWindParams`, the `WindSettings` env block, the cloud-shadow fields on `SetCloudsParams`) is registered across **all** protocol tripwires in one change — the build + schema-fragment tests refuse to compile otherwise.

## 0 — Foundation: what Phases 3 & 5 leave, and the shared atmosphere-march module

This phase assembles on top of shipped infra. The reconciliation targets, verified in-tree:

| What | File | Symbols |
|---|---|---|
| The shipped AP froxel + its LUT march (reuse, do not rebuild) | `engine/crates/rendering/src/froxel_fog.rs`, `engine/assets/shaders/aerial_perspective.slang` | `AerialPerspective`, `AerialParamsUbo`, `AP_GRID`, `AP_FAR_M`, `AerialPerspective::bind_luts`; `computeMain`, `sampleTransmittance`, `sampleMultiScatter`, `froxelCenterWorld`, `ApParams` |
| The one transmittance ledger + composite (extend, do not duplicate) | `engine/assets/shaders/height_fog.slang`, `engine/crates/rendering/src/renderer.rs` | `computeMain` (`T = fogT·apT`), `skyViewTint`, `FogParams`, `layerOpticalDepth`; `add_fog_pass`, `add_aerial_perspective_pass`, `ap_active` |
| The froxel fog inject/integrate (god-rays ride these) | `engine/assets/shaders/fog_inject.slang`, `engine/crates/rendering/src/renderer.rs` | `computeMain`, `fogDirectionalInScatter`, `clusterIndexFor`; `add_froxel_fog_passes`, `FogGridParams` |
| The atmosphere LUT views + baked sun/atmosphere | `engine/crates/rendering/src/ibl.rs` | `transmittance_view`, `multi_scatter_view`, `sky_view_lut_view`, `baked_atmosphere`, `baked_sun`, `atmosphere_live` |
| The sun (and Phase-1 moon) key light | `engine/crates/scene/src/component.rs`, `engine/crates/rendering/src/renderer.rs` | `DirectionalLight` (`direction`, `volumetric_scattering`, `cast_volumetric_shadow`, `DEFAULT_DIRECTION`); `sun_direction`, `baked_sun` |
| Cloud state + settings (Phase 4/5, extend) | `engine/crates/scene/src/environment.rs`, `engine/crates/scene/src/serde.rs` | `CloudSettings`, `cloud_to_json`/`cloud_from_json` (Phase-4 siblings of `FogSettings`, `fog_to_json`/`fog_from_json`); `SceneEnvironment` |
| Cloud raymarch buffer + front depth + submit seam (Phase 5) | `engine/crates/assets/src/render_scene.rs`, `engine/crates/rendering/src/renderer.rs` | `submit_clouds`, the reduced-res `(L_cloud, T_cloud)` view target + transmittance-weighted mean front-depth output |

**The shared atmosphere-march module (build first — everything in §1 and §4 consumes it).** Promote `aerial_perspective.slang`'s LUT march into a public, resource-parameterized `engine/assets/shaders/atmos_ap.slang` (the `giprobe`/`lighting` module shape: no fixed bindings; LUT samplers + `ApParams` passed as parameters):

```hlsl
// atmos_ap.slang — the one bounded atmosphere march, shared by the AP froxel fill and the cloud fold.
struct AtmoSample { float3 inScatter; float3 transmittance; };

// March from `origin` toward `viewDir` for `distKm`, sampling the transmittance + multiscatter LUTs
// exactly as aerial_perspective.slang does today (single-scatter phase + isotropic multiscatter +
// the analytic slice sInt = (S - S*stepT)/ext). 16 steps resolves any segment <= AP far.
AtmoSample marchAtmosphere(float3 pos, float3 viewDir, float3 sunDir, float distKm,
                           Sampler2D<float4> transmittanceLut, Sampler2D<float4> multiScatterLut,
                           ApParams p);
```

`aerial_perspective.slang::computeMain` becomes: reconstruct the froxel-center distance (unchanged), then `AtmoSample s = marchAtmosphere(pos, viewDir, sunDir, distKm, …); apVolume[id] = float4(s.inScatter * p.params1.w, dot(s.transmittance, 1/3))`. Delete the inline march. The `height_fog` fold (§4) calls the identical function bounded at the cloud front distance.

## 1 — Cloud aerial perspective on the shared LUTs (no second AP volume)

Clouds get aerial perspective in the ledger fold (§4), not a new pass. Given the Phase-5 cloud result `(L_cloud, T_cloud)` (premultiplied, i.e. `L_cloud` already scaled by the cloud's internal `1 − T_cloud`) and the transmittance-weighted mean cloud front depth `d_cloud`, apply the camera→cloud atmosphere once:

```hlsl
// height_fog.slang fold — camera->cloud atmosphere applied exactly once, from the shared LUTs.
float3 toCloud   = viewDir * d_cloud;                 // cloud front along the view ray
float  distKm    = length(toCloud) * 1.0e-3;
AtmoSample atm   = marchAtmosphere(cameraPos, viewDir, sunDir, distKm,
                                   transmittanceLut, multiScatterLut, apParams);
float3 atmT      = atm.transmittance;                 // T(camera -> cloud)
float3 cloudL    = L_cloud * atmT + (1.0 - T_cloud) * atm.inScatter;   // AP on the cloud, once
```

`transmittanceLut` / `multiScatterLut` are the existing `Ibl::transmittance_view` / `multi_scatter_view` bound onto the per-view fog set (new bindings 6/7), and `apParams` reuses the atmosphere physical block already assembled for the AP froxel in `add_aerial_perspective_pass` (`baked_atmosphere`, `baked_sun`). The bound at `d_cloud` — not the froxel far — is what makes this range-correct beyond the 32 km AP shell.

> Decision to record when building: **the camera→cloud atmosphere is a single `marchAtmosphere` at the cloud front depth, applied to `cloudL` once — the AP froxel term (`apT`, `apInScatter`) is *not* also applied to the cloud radiance.** `apT`/`apInScatter` are the camera→background segment (the opaque receiver behind the cloud); the cloud is nearer, so its own segment is `atmT`. Multiplying the cloud by both double-darkens. The scene/background keeps `apT` (its receiver is the background); the cloud keeps `atmT`; the two segments never overlap. This is the "apply the atmosphere between camera and cloud exactly once" invariant.

> Decision to record when building: **there is no cloud AP toggle — the fold applies the shared-LUT atmosphere whenever `atmosphere_live()`, the same gate the AP froxel uses.** Aerial perspective on clouds is a correctness requirement (or the cloud reads as a pasted sprite at the horizon), not an option; UE exposes `HighQualityAerialPerspective` only as a LUT-vs-raymarch quality switch, and here the shared-LUT march *is* the quality path, so no knob is warranted.

## 2 — The top-down density-integrated cloud shadow map (Beer/ESM, cascaded)

**New shader `engine/assets/shaders/cloud_shadow.slang` + new resource `CloudShadow` in `engine/crates/rendering/src/froxel_fog.rs`** (or a sibling `cloud.rs` module Phase 4/5 introduces). One compute dispatch marches the Phase-4 cloud density field from the sun's view, top-down orthographic, integrating optical depth into a cascaded 2D texture (concentric extents around the camera, UE "Cloud Shadow Extent"):

```hlsl
// cloud_shadow.slang — one thread per shadow texel: integrate optical depth down the sun ray through
// the cloud layer, store the exponential shadow term (ESM, RDR2) so it filters softly under a linear
// sampler. Beer transmittance is exp(-sigma_t * integratedDensity).
float tau = 0.0;
for (int s = 0; s < CLOUD_SHADOW_STEPS; ++s) {
    float3 p = layerTop - sunDir * (s + 0.5) * stepLen;
    tau += cloudDensity(p) * stepLen;                 // the Phase-4 density sampler
}
float beer = exp(-tau);                               // Beer-Lambert transmittance to the cloud base
cloudShadow[texel] = exp(ESM_C * beer);              // ESM: mip-mappable, softenable
```

The resource mirrors the directional shadow map + the `AerialPerspective` persistent-`Image3D` pattern: a `VK_IMAGE_TYPE_2D` array (one layer per cascade), a host-mapped sun-view-matrix UBO (`world → cloud-shadow UV` per cascade + the ESM constant), a linear-clamp sampler, and a descriptor set written once. The fill dispatch reads the Phase-4 noise volumes + weather map + `CloudSettings` and writes the cascade array.

**Wired into the render graph before the shadow-consuming passes** — alongside the existing directional/spot/point shadow passes (renderer step 5, before light-cull → fog inject → the scene pass). `add_cloud_shadow_pass` imports the cascade array (`import_image` with an external-layout slot, resting `SHADER_READ_ONLY_OPTIMAL`), dispatches the march, and rests the map for its three readers.

**Three read sites, one map:**

| Consumer | File | How |
|---|---|---|
| Opaque mesh surface shadow | `engine/assets/shaders/lighting.slang` (the public shade helper) | multiply the sun's direct term by `cloudShadowVisibility(worldPos)` — the ESM sample at `worldPos` projected into the cloud-shadow cascade; bound at a new light-set binding past the froxel-integration binding 11 |
| Cloud self-shadow | the Phase-5 cloud raymarch | replace/augment the secondary cone march with the map sample (the cheaper Beer-shadow-map path, UE "Ray March Volume Shadow" off) |
| Froxel-fog god-rays | `engine/assets/shaders/fog_inject.slang` | §3 |

Strength knobs (`cloud_shadow_strength`, `cloud_shadow_on_surface_strength`) lerp the visibility toward 1, matching UE's Cast Cloud Shadows controls. Sources: UE Beer Shadow Map / Cast Cloud Shadows (`Volumetric Cloud Component Properties`); RDR2 mip-mapped ESM cloud shadows.

## 3 — God-rays through the froxel fog (cloud shadow per froxel in `fog_inject`)

**File `engine/assets/shaders/fog_inject.slang`.** The directional sun in-scatter is `fogDirectionalInScatter(worldPos, viewDir, phaseG, globals, shadowMap)` today, gated by the cascade PCF `shadowMap`. Add the cloud shadow map as a second occluder on the sun term: sample `cloudShadowVisibility(worldPos)` (the §2 ESM map, bound onto the fog inject set 0 at the new light-set binding) and multiply it into the directional in-scatter *before* it accumulates. Gaps in the cloud deck admit sun in-scatter into those froxels; occluded froxels stay dark; the unchanged `fog_integrate.slang` front-to-back march turns the density gradient into visible shafts.

```hlsl
// fog_inject.slang — the sun in-scatter, now gated by the cloud deck as well as the scene shadow.
float3 inScatter = fogDirectionalInScatter(worldPos, viewDir, phaseG, globals, shadowMap);
inScatter *= cloudShadowVisibility(worldPos, cloudShadow, cloudShadowMatrices);   // crepuscular gaps
```

No new pass, no screen-space blur: the shafts are a consequence of the existing froxel grid weighting the sun by cloud visibility (RDR2 frustum voxel grid). Because the froxel grid spans only `FROXEL_FAR = 128 m`, this lights near-field shafts (a beam through a gap onto the scene); the far cloud-to-atmosphere darkening rides §2's mesh-surface + atmosphere read instead.

> Decision to record when building: **the cloud shadow only gates the *directional* (sun/moon) in-scatter in the fog inject, not the punctual lights.** Point/spot lights are below the cloud deck and are already shadowed by their own maps; multiplying them by the cloud shadow would wrongly dim a lamp under an overcast. Only `fogDirectionalInScatter` takes the cloud-visibility factor.

## 4 — One transmittance ledger: fold `T_cloud` into `height_fog.slang`

**File `engine/assets/shaders/height_fog.slang` + `engine/crates/rendering/src/renderer.rs::add_fog_pass`.** The composite already folds fog and AP as independent multiplied media (`T = fogT·apT; L = c.rgb·T + fogInScatter + fogT·apInScatter`). Extend it with the cloud as a third medium, front-to-back, using the §1 atmosphere-wrapped `cloudL` and the Phase-5 `T_cloud` (sampled from the reduced-res cloud buffer at this pixel):

```hlsl
// height_fog.slang computeMain — the shared ledger, now T_fog * T_aerial * T_cloud (premultiplied).
float4 cloud   = cloudBuffer.SampleLevel(uv, 0.0);        // rgb = L_cloud (premult), a = T_cloud
float  cloudT  = cloud.a;
float  dCloud  = cloudDepth.SampleLevel(uv, 0.0).r;       // transmittance-weighted mean front depth
float3 cloudL  = applyCloudAerialPerspective(cloud.rgb, cloudT, viewDir, dCloud);   // §1, once

float  T = fogT * apT * cloudT;                            // T_total = T_fog * T_aerial * T_cloud
float3 L = c.rgb * T                                       // background: dimmed by fog, aerial, cloud
         + fogInScatter                                    // fog in-scatter (nearest)
         + fogT * apInScatter                              // scene aerial in-scatter (behind fog)
         + fogT * cloudL;                                  // cloud radiance (behind fog, own atmosphere)
target[px] = float4(L, c.a);
```

New bindings on the per-view fog set (extending the current 0–5): binding 6 transmittance LUT, binding 7 multiscatter LUT (both the IBL clamp sampler, from `Ibl::transmittance_view`/`multi_scatter_view`), binding 8 the cloud color/transmittance buffer, binding 9 the cloud front-depth buffer. `add_fog_pass` binds them, extends `FogParams` with a `cloud.x = clouds live` flag + the `ApParams` block for the march, and widens the composite gate so the pass runs when `fog.enabled || ap_active || clouds_present` (it already runs for AP-only; clouds-only is the new case).

> Decision to record when building: **the Phase-5 standalone cloud composite is removed in this change; `submit_clouds` now hands `(L_cloud, T_cloud, d_cloud)` to `add_fog_pass` for the fold, and the cloud raymarch/reconstruct passes stay but stop writing `color` directly.** One composite, one ledger — the NO-LEGACY cutover. The cloud raymarch already clips against opaque depth (Phase 5), so `T_cloud = 1` where scene geometry is nearer; the fold needs no extra depth test beyond `d_cloud` for the AP sample.

> Decision to record when building: **`cloudL` is attenuated by `fogT` only, never `apT`.** The cloud sits between camera and the far background; `apT`/`apInScatter` describe the camera→background segment, and `cloudL` already carries its own camera→cloud atmosphere (`atmT`) from §1. Attenuating it by `apT` too would double the near-segment extinction. The background keeps the full `T = fogT·apT·cloudT`; the cloud radiance rides `fogT` and its own atmosphere.

## 5 — The shared wind field, curl advection, weather cycles, and night clouds

**Global wind is one setting.** Add a `WindSettings { orientation: f32, speed: f32, gust: f32 }` block to `SceneEnvironment` (mirroring `AtmosphereSettings`/`FogSettings` end-to-end: struct + `Default` in `engine/crates/scene/src/environment.rs`, `wind_to_json`/`wind_from_json` in `serde.rs` folded into `environment_to_json`/`from_json`), authored by a new `set-wind` command. Clouds and the froxel fog inject both read it; the per-`FogVolume` `wind` field already shipped stays a local override.

**Curl advection (Bridson 2007).** The weather map + noise sample positions scroll by `p − windDir·speed·t` (using the Phase-3 time-of-day scalar for `t`), warped by a divergence-free curl field so motion reads as turbulent flow, not a sliding texture. A curl-noise warp is the curl of a potential (`v = ∇ × ψ`, so `div(v) = 0`); the 2D form used for the weather scroll is `v = (∂ψ/∂y, −∂ψ/∂x)` sampled from the Phase-4 curl volume, added to the linear wind advection with a gust-scaled amplitude. Structure (coverage / cloud-map) stays fixed while the detail noise animates — the "animate detail, not structure" rule (Nubis; Unity HDRP global wind + shape/erosion speed multipliers).

**Weather cycles keyed to time-of-day.** Coverage / cloud-type are driven by the Phase-3 elevation-indexed appearance curves (the monotone-cubic curve widget), so a storm front rolls in on the day cycle rather than a second clock — the wind field advects the map the cycle produces.

**Night clouds.** The Phase-1 moon `DirectionalLight` (atmosphere-light role index 1) feeds the Phase-5 cloud lighting loop and the §2 cloud shadow map when the sun is below the horizon: the cloud raymarch's sun-transmittance march runs for the moon direction/intensity, and the cloud shadow map is rendered from whichever atmosphere light is above the horizon. No new light — the moon is the second `DirectionalLight` Phase 1 added.

## 6 — Protocol, control, and editor surface

Two protocol changes, both across all tripwires in one change:

**Extend `SetCloudsParams` (Phase-4 DTO) with the cloud-shadow fields** — `cast_cloud_shadows: Option<bool>`, `cloud_shadow_strength: Option<f32>`, `cloud_shadow_on_surface_strength: Option<f32>` — plus the matching `CloudSettings` fields (struct + `Default` in `environment.rs`, `cloud_to_json`/`cloud_from_json` round-trip, the hand-authored `CloudSettingsDto` schema block + its `required` array, and the `set-clouds` handler merge branches in `commands_scene.rs`). No new type — narrower tripwires (no `DTO_TYPE_NAMES`/codegen/inventory change).

**Add `SetWindParams` + the `set-wind` command** (new type + command — the full tripwire list, per the digest checklist):

| Tripwire | File | Entry |
|---|---|---|
| DTO struct | `engine/crates/protocol/src/dto.rs` | `SetWindParams` (Option fields + `json` escape hatch), returns `EnvironmentDto` |
| Command row + fixture + type name + domain order | `engine/crates/protocol/src/command.rs` | `COMMANDS` `set-wind` row; one of `COMMAND_FIXTURES`/`COMMAND_SKIPS`; `DTO_TYPE_NAMES` += `SetWindParams`; `scene_domain()` order |
| Codegen | `engine/crates/protocol/src/codegen.rs` | `decl_entry!(SetWindParams)` + `frag_entry!(SetWindParams)` |
| Schema | `engine/crates/protocol/src/schema.rs` | hand-authored `Wind` block on the `Environment` shape + its `required`; `SetWindParams` fragment |
| Existence + fragment tests | `engine/crates/protocol/tests/inventory.rs`, `.../schema_fragments.rs` | add `SetWindParams` |
| Handler | `engine/crates/control/src/commands_scene.rs` | `reg.register::<SetWindParams, EnvironmentDto>("set-wind", …)` — the `set-fog` merge idiom (serialize env → merge `Some` fields onto `wind` → `environment_from_json` → bump `scene_version` → `environment_dto(ctx)`) |
| Fixture params | `tools/check-control-schema/check.ts` | `paramsForFixture` case for the `set-wind` fixture |
| Regenerate artifacts | `schemas/control/{openrpc,command-manifest}.generated.json` | `cargo run -p xtask -- gen-protocol` |

**Editor.** `editor/src/control/client.ts` gains `setWind` (mirroring `setFog`/`setAtmosphere`) once `bun run check` regenerates `@saffron/protocol`; `editor/src/panels/EnvironmentPanel.tsx` gains a Wind section (a `patchWind`/`recordWindEdit` pair like `patchFog`/`recordFogEdit`, driven by `<Row>` widgets) and the cloud-shadow toggles + strength rows in the existing Clouds section (`patchClouds`). The `sa` CLI needs no per-command code — `sa set-wind --speed 20 --orientation 45` flows through the generated manifest.

> Decision to record when building: **wind is its own `set-wind` command, not a knob on `set-clouds` or `set-atmosphere`.** It is a *shared* global field (clouds + froxel fog inject read it, and it is the future default for foliage/particles), so it has one write path of its own — smuggling it onto a cloud command would tie a cross-cutting setting to one consumer and break "one write path per setting" the moment a second consumer reads it.

## Out of scope (later / unscheduled)

- **Cloud-to-atmosphere GI / sky darkening under a deck.** §2 darkens opaque surfaces + fog and §1 tints clouds by AP, but the diffuse sky-light (Phase-2 SH) is not re-derived under an overcast this phase; feeding the cloud shadow coverage back into the SH capture is a Phase-2-adjacent refinement, not built here.
- **A dedicated cloud-frustum voxel grid for shafts.** God-rays ride the shipped `FROXEL_FAR = 128 m` fog grid; extending shafts to full cloud-layer range would want a coarse far grid (RDR2's `160×88×64`), which is the fog planset's own unscheduled item.
- **Nubis-3 terrain-cast cloud shadows / lightning-lit interiors.** The shadow map is the density-integrated ESM cascade; terrain-shadowed bespoke clouds and storm inner-glow belong to the Phase-4 voxel-authoring future direction.
- **Global wind consumed by foliage / particles / rain.** `WindSettings` is read by clouds + fog this phase; wiring it into gameplay VFX is a follow-on once those systems exist.

## Known interactions (note, don't over-engineer)

- **AP froxel vs. cloud AP boundary.** The AP froxel covers opaque geometry ≤ `AP_FAR_M`; the cloud AP march covers camera→cloud for cloud pixels. They address disjoint receivers (background vs. cloud) so they never overlap on one pixel — but confirm the composite does not add `apInScatter` to a cloud that fully occludes the background (`cloudT → 0` already zeroes `c.rgb·T`, and `apInScatter` is the background's own in-scatter, correctly independent). Do not special-case this; the independent-media ledger handles it.
- **ESM light bleed.** The exponential shadow map's `ESM_C` trades softness for bleed near thick-cloud edges; start at the RDR2-class constant and expose it only if bleed shows, not preemptively.
- **Cloud shadow cascade extent vs. camera.** The cascades follow the camera (UE Cloud Shadow Extent); snap the sun-view origin to texel granularity so the map does not shimmer as the camera moves — the same stability trick the directional cascades already use.
- **Reduced-res cloud buffer against the full-res ledger.** The composite samples the Phase-5 reduced-res `(L_cloud, T_cloud)` + front depth with the depth-aware bilateral upscale already built in Phase 5; the fold reads the upscaled full-res result, so no new upscale here.
- **Moon single-scatter only.** Per Phase 1, the atmosphere computes multiple scattering only for light index 0; night clouds lit by the moon get single-scatter atmosphere transmittance, which is correct for the balance Phase 1 set — do not add a second multiscatter LUT for the moon.

## Milestone gate

Run `just engine` then `just prepare-for-commit` (format + clippy `-D warnings`) and fix every warning this change raises. Boot a fixture scene with an atmosphere + a cloud deck + a shadow-casting sun headless (`just run-engine-headless`, on the NVIDIA GPU) with a **validation-clean** log, and confirm the three couplings are visible: cloud shadows fall on the ground, crepuscular rays appear through cloud gaps, and distant clouds pick up aerial perspective (no horizon seam). Probes that must change the frame relative to their off state: `sa set-clouds --castCloudShadows true --cloudShadowStrength 1.0` (shadows + god-rays appear), `sa set-wind --speed 20 --orientation 45` (the deck drifts), and — with the atmosphere live — the cloud AP tint at the horizon. Because wire types changed (`SetWindParams` + the `WindSettings` env block + the cloud-shadow fields on `SetCloudsParams`), run `bun run check` in `editor/` (regenerates `@saffron/protocol` — never hand-edit `sa-types.ts`) and `just e2e`, adding a `tests/e2e` case that drives `set-wind` and `set-clouds --castCloudShadows` over the control plane and asserts the `EnvironmentDto` echo carries the new state plus a validation-clean log. Add the docs integration page — a new `docs/content/explanations/image-based-lighting/cloud-integration.md` (the cloud AP on the shared LUTs, the density-integrated cloud shadow map + its three consumers, god-rays through the froxel fog, the `T_fog · T_aerial · T_cloud` ledger, and the shared wind field) with the slim `What | File | Symbols` table (`atmos_ap.slang`/`cloud_shadow.slang`, `height_fog.slang`, `fog_inject.slang`, `CloudShadow` + `WindSettings`, `SetWindParams`) — and update the image-based-lighting hub `_index.md` `## Pages` row plus the `screen-space-and-post/height-fog.md` ledger section to note `T_cloud`, in the same change.

## References

Cloud aerial perspective + per-sample transmittance:

- Sébastien Hillaire — *A Scalable and Production Ready Sky and Atmosphere Rendering Technique* (EGSR 2020; the LUT chain the cloud AP samples): https://sebh.github.io/publications/egsr2020.pdf
- Unreal Engine 4.27 — *Volumetric Clouds* (`HighQualityAerialPerspective`, Beer Shadow Map, per-sample atmospheric light transmittance): https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-clouds?application_version=4.27

Cloud shadow map + god-rays + one ledger:

- Unreal Engine — *Volumetric Cloud Component Properties* (Cast Cloud Shadows, Ray March Volume Shadow, two directional lights): https://dev.epicgames.com/documentation/unreal-engine/volumetric-cloud-component-properties-in-unreal-engine
- Bauer — *Creating the Atmospheric World of Red Dead Redemption 2* (SIGGRAPH 2019; frustum voxel grid, ESM cloud shadows, integrated solution): https://www.advances.realtimerendering.com/s2019/index.htm
- RDR2 atmospheric system — technical summary (frustum voxel grid, ESM cloud shadows, single transmittance ledger): https://roeas.github.io/2023/12/19/SigRedDeadRedemption2Atmospheric/
- Sébastien Hillaire (Frostbite) — *Physically Based Sky, Atmosphere and Cloud Rendering* (premultiplied scattering + transmittance, single-apply atmosphere): https://media.contentapi.ea.com/content/dam/eacom/frostbite/files/s2016-pbs-frostbite-sky-clouds-new.pdf
- Andrew Schneider — *Nubis, Evolved* (cloud self-shadow, multiscatter): https://www.guerrilla-games.com/read/nubis-evolved

Wind field + curl advection:

- Bridson, Hourihan, Nordenstam — *Curl-Noise for Procedural Fluid Flow* (SIGGRAPH 2007; divergence-free advection warp): https://www.cs.ubc.ca/~rbridson/docs/bridson-siggraph2007-curlnoise.pdf
- Unity HDRP — *Volumetric Clouds Volume Override reference* (global horizontal wind, shape/erosion speed multipliers, animate detail not structure): https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.3/manual/volumetric-clouds-volume-override-reference.html
