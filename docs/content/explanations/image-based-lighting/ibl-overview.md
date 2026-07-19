+++
title = 'IBL overview'
weight = 1
math = true
+++

# IBL overview

Image-based lighting turns an environment into indirect diffuse light and view-dependent reflections. It integrates incoming radiance with the [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) without evaluating the full lighting integral for every fragment.

## Split-sum specular

Reflected environment radiance is the BRDF integrated over the hemisphere:

$$
L_o(v)=\int_\Omega f(l,v)L_i(l)(n\cdot l)\,dl.
$$

The [split-sum approximation](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf) separates this into an environment-dependent prefilter and a material-dependent lookup. The engine stores the first term in a five-mip cubemap and the second in a BRDF lookup table.

```hlsl
float3 prefiltered = prefilteredMap.SampleLevel(R, prefilterLod(roughness)).rgb;
float2 ab = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
float3 specularIBL = prefiltered * (F0 * ab.x + ab.y);
```

The fragment also applies GGX multi-scatter energy compensation, horizon occlusion, material occlusion, GTAO, and reflection-cone sky visibility. Local [reflection probes](../reflection-probes/), screen-space reflections, and ray-traced reflections can replace the global prefiltered radiance before those terms apply.

## Spherical-harmonic diffuse

Diffuse IBL reconstructs the cosine-weighted environment from nine raw-radiance spherical-harmonic coefficients. The shared reconstruction multiplies bands zero through two by the analytic Lambert cosine-kernel coefficients. This produces irradiance from a normal without a cubemap fetch.

Opaque surfaces receive the result from `gi_resolve.slang` at half resolution. That pass applies distance-field sky visibility and blends in [DDGI](../../global-illumination-and-raytracing/ddgi-overview/) according to probe-cage coverage. Transparent surfaces evaluate the same SH and DDGI terms directly because the opaque screen-space result describes the surface behind them.

## Persistent resources

The environment source is a procedural sky, an equirectangular panorama, or the [procedural atmosphere](../procedural-atmosphere/). Its products have stable allocations and descriptor bindings:

| Resource | Extent | Format | Purpose |
|---|---:|---|---|
| Environment cube | 256×256 per face, 9 mips | RGBA16F | Source radiance and visible sky |
| Sky SH buffer | 9 × RGB coefficients | `vec4` std430 | Diffuse irradiance and GI miss radiance |
| Prefiltered cube | 256×256 per face, 5 mips | RGBA16F | Specular radiance by roughness |
| BRDF LUT | 256×256 | RGBA16F | Fresnel scale and bias |

Each frame slot has a mesh descriptor set 3. Bindings 0 through 2 share the persistent SH buffer,
prefiltered cube, and BRDF LUT; bindings 3 through 5 carry the probe arrays and that slot's metadata
buffer.

## Capture lifetime

Renderer construction performs a complete startup capture before the first frame. A source, panorama, atmosphere, or celestial-direction change arms an asynchronous environment refresh. [Real-time sky-light capture](../realtime-skylight-capture/) projects SH on the render graph and reconverges the persistent specular cube across the authored cadence.

The master IBL switch is enabled by default. `sa set-ibl 0` selects the authored flat ambient fallback, and `sa set-ibl 1` restores the environment-derived diffuse and specular paths.

## In the code

| What | File | Symbols |
|---|---|---|
| Resources and capture lifetime | `engine/crates/rendering/src/ibl.rs` | `Ibl`, `LiveCapture`, `Ibl::request_env_bake` |
| Capture dimensions | `engine/crates/rendering/src/ibl.rs` | `IBL_ENV_SIZE`, `SKY_SH_COEFFICIENTS`, `IBL_PREFILTER_SIZE`, `IBL_PREFILTER_MIPS`, `IBL_LUT_SIZE` |
| IBL descriptor layout | `engine/crates/rendering/src/descriptors.rs` | `create_ibl_layout` |
| Opaque diffuse resolve | `engine/assets/shaders/gi_resolve.slang` | `computeMain`, `skyShIrradiance` |
| Specular and transparent diffuse | `engine/assets/shaders/lighting.slang` | `evalLighting`, `prefilterLod`, `skyShIrradiance` |
| Runtime toggle | `engine/crates/control/src/commands_render.rs` | `set-ibl` |

## Related

- [Real-time sky-light capture](../realtime-skylight-capture/) covers SH projection and specular scheduling.
- [Specular prefilter](../specular-prefilter/) covers roughness-filtered environment radiance.
- [BRDF LUT](../brdf-lut/) covers the scale-and-bias integration.
- [Baking](../ibl-bake-pass/) covers startup validity and environment refreshes.
- [Distance field reflection occlusion](../../global-illumination-and-raytracing/distance-field-reflection-occlusion/) covers diffuse and specular sky visibility.
