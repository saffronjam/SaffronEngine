+++
title = 'IBL overview'
weight = 1
math = true
+++

# IBL overview

Image-based lighting computes a surface's indirect, or *ambient*, illumination by treating an environment as a light source and integrating the [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) against it.

[Direct lighting](../../lighting-and-brdf/cook-torrance-brdf/) accounts only for the sun and the punctual lights. Everything else a surface sees — the sky, the bounce off nearby geometry, the general fill of a room — is the ambient term. The defining integral is too expensive to evaluate per pixel per frame, so the engine precomputes three small textures from the environment once at startup and the mesh fragment shader samples them.

## Split-sum approximation

Reflected radiance from an environment is the BRDF integrated over the hemisphere:

$$
L_o(v) = \int_\Omega f(l, v)\, L_i(l)\, (n \cdot l)\, dl
$$

There is no closed form, and Monte-Carlo sampling it per fragment is too slow for real time. The split-sum approximation (Karis, *Real Shading in Unreal Engine 4*) factors the specular part into two integrals that each precompute into a lookup:

$$
\int_\Omega f\, L_i\, (n\cdot l)\, dl \;\approx\;
\underbrace{\left(\frac{1}{N}\sum L_i(l_k)\right)}_{\text{prefiltered env}}
\;\cdot\;
\underbrace{\int_\Omega f\,(n\cdot l)\, dl}_{\text{BRDF LUT}}
$$

The first factor is the environment prefiltered by roughness: a cubemap whose mip chain holds progressively blurrier reflections. The second depends only on $n\cdot v$, roughness, and $F_0$, and being environment-independent it bakes into a single 2D table reused across scenes. Diffuse is handled separately by a cosine-weighted irradiance convolution.

## Three baked textures

The engine bakes one environment (a procedural sky by default) into:

| Texture | What it holds | Page |
|---|---|---|
| Irradiance cube | cosine-weighted diffuse over the hemisphere | [Diffuse irradiance](../diffuse-irradiance/) |
| Prefiltered cube | GGX-blurred specular, one mip per roughness | [Specular prefilter](../specular-prefilter/) |
| BRDF LUT | the Fresnel scale/bias split-sum factor | [BRDF LUT](../brdf-lut/) |

All three are baked once by [the bake](../ibl-bake-pass/) and bound as descriptor set 3 in the mesh pipeline.

## How the mesh shader uses them

The ambient block in `lighting.slang` (imported by `mesh.slang`) reads all three and assembles diffuse plus specular. Diffuse samples the irradiance cube along the normal $n$ and scales by the energy-conservation factor $k_d$, computed with `fresnelSchlickRoughness` so rough surfaces do not over-reflect at grazing angles. Specular samples the prefiltered cube along the reflection vector $R$ at a mip chosen by roughness, then applies the LUT, where `F0 * ab.x + ab.y` is the split-sum scale and bias.

```hlsl
float3 irradiance  = irradianceMap.SampleLevel(n, 0.0).rgb;   // along the shading normal
float3 indirectIrr = irradiance;                              // analytic sky (residual; DDGI replaces it)
float3 diffuseIBL  = kd * indirectIrr * albedo;
float3 prefiltered = prefilteredMap.SampleLevel(R, roughness * IblPrefilterMaxMip).rgb;
float2 ab          = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
float3 specularIBL = prefiltered * (F0 * ab.x + ab.y) * specSkyVis;        // reflection-cone occluded
ambient = diffuseIBL * ao + specularIBL;                      // ao = material × contact GTAO
```

## Why a ceiling does not block the sky on its own

The irradiance cube treats the whole environment as visible from every point. A surface deep inside an enclosed room receives the same sky irradiance as one in open air, because the cube knows nothing about the geometry between them. Material AO maps and screen-space [GTAO](../../screen-space-and-post/) only darken the contact scale (creases, the few centimetres around a corner), so neither can say "a ceiling stands between this floor and the sky." Left alone, the analytic sky leaks into interiors and washes them out.

Two mechanisms fix this, and they compose. Both leave open and outdoor areas alone; they only bite where geometry actually encloses a surface.

### DDGI replaces the IBL diffuse where it has coverage

When [DDGI](../../global-illumination-and-raytracing/ddgi-overview/) is enabled, its probe irradiance already carries sky occlusion intrinsically — a probe inside a sealed room sees the sky only through the gaps its rays actually reach (the sky enters a probe's radiance only on a ray that *misses* every surface). So the indirect diffuse is a *replace*, not an add: the DDGI irradiance lerps over the analytic irradiance by the probe cage's coverage. Where the cage covers a surface, its occluded irradiance wins; where it does not, the analytic term remains as the residual. This DDGI ray-miss is the **large-range** indirect occlusion — there is no longer a distance-field ambient-occlusion prepass dimming the diffuse, which would double-count the same enclosure.

```hlsl
float3 indirectIrr = irradiance;                     // analytic sky (residual where DDGI is absent)
if (ddgiEnabled) {
    float4 ddgi = ddgiSampleIrradiance(worldPos, n); // .w = coverage
    indirectIrr = lerp(indirectIrr, ddgi.rgb, ddgi.w);
}
float3 indirect = kd * indirectIrr * albedo * ao;    // ao = material × contact GTAO
```

The diffuse is a single replace rather than two stacked terms, so a mid-room floor never reads brighter than the analytic sky alone would have made it — DDGI carries the sky, it does not add a second copy of it. The only further occlusion is **contact-scale**: a small-radius [GTAO](../../screen-space-and-post/gtao/) fills in the creases and corners the coarse probe grid cannot resolve. [Screen-space GI](../../screen-space-and-post/) stays additive on top, because it is a one-bounce screen-space term, not the sky.

### The distance field occludes the specular reflection

The diffuse no longer reads the distance field at all, but the specular does. A separate reflection-cone factor `specSkyVis` cuts back the reflected skybox where the reflection vector is blocked — see [distance field reflection occlusion](../../global-illumination-and-raytracing/distance-field-reflection-occlusion/). It is gated on the sky-occlusion toggle. Dimming specular by a diffuse AO scalar would wrongly darken a chrome surface facing open sky, so the two are kept distinct.

## When it replaces flat ambient

IBL is the default. It runs whenever `globals.counts.z != 0`, which is `use_ibl && Ibl::ready`. Disabling it (`sa set-ibl 0`) falls back to a flat scalar — `albedo * (1 - metallic) * ambientColor` — that the scene's ambient carries. The flat version has no directionality and no specular reflection, the two qualities IBL adds.

## In the code

| What | File | Symbols |
|---|---|---|
| Ambient assembly + set-3 bindings | `engine/assets/shaders/lighting.slang` | ambient block, `irradianceMap`, `prefilteredMap`, `brdfLut`, `IblPrefilterMaxMip` |
| SDF reflection occlusion | `engine/assets/shaders/lighting.slang` | `globals.sdfOcclusion`, `specSkyVis`, `sdfReflectionOcclusion` |
| DDGI replaces the IBL diffuse | `engine/assets/shaders/lighting.slang` | `ddgiSampleIrradiance` (coverage in `.w`), the `lerp(indirectIrr, ddgi.rgb, ddgi.w)` |
| IBL-on flag | `engine/crates/rendering/src/lighting.rs` | `set_frame_ibl`, `frame_ibl_flag` → `counts` |
| Toggle + default | `engine/crates/rendering/src/ibl.rs` | `Ibl::use_ibl` (default `true`), `Ibl::ready` |
| Sky-occlusion toggle | `engine/crates/rendering/src/renderer.rs` | `Renderer::set_sky_occlusion`, `sky_occlusion_enabled` |
| Control command | `engine/crates/control/src/commands_render.rs` | `set-ibl` |

## Related

- [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) — the BRDF this integrates
- [Diffuse irradiance](../diffuse-irradiance/) — the diffuse cube
- [Specular prefilter](../specular-prefilter/) — the roughness-mipped specular cube
- [BRDF LUT](../brdf-lut/) — the split-sum scale/bias table
- [Distance field reflection occlusion](../../global-illumination-and-raytracing/distance-field-reflection-occlusion/) — the SDF cone that occludes the reflected skybox
- [HDR and exposure](../../lighting-and-brdf/hdr-and-exposure/) — the linear radiance space
