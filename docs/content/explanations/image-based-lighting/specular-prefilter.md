+++
title = 'Specular prefilter'
weight = 5
math = true
+++

# Specular prefilter

The specular prefilter is a cubemap whose mip chain stores environment radiance convolved by GGX at increasing roughness. Mip zero holds a sharp reflection, while coarser mips store progressively wider lobes. This makes the environment-dependent half of the [split-sum approximation](../ibl-overview/) one texture sample at shading time.

## GGX importance sampling

For a fixed roughness, the prefiltered value is an importance-sampled environment integral:

$$
\operatorname{prefilter}(r)\approx
\frac{\sum_k L_i(l_k)(n\cdot l_k)}{\sum_k(n\cdot l_k)}.
$$

The prefilter assumes view, normal, and reflection are aligned. It draws GGX half-vectors from a low-discrepancy Hammersley sequence, reflects the view around each half-vector, and accumulates samples above the horizon.

```hlsl
float2 xi = hammersley(i, sampleCount);
float3 h = importanceSampleGGX(xi, n, push.roughness);
float3 l = normalize(2.0 * dot(v, h) * h - v);
prefiltered += envCube.SampleLevel(l, sourceMip).rgb * max(dot(n, l), 0.0);
```

The source mip comes from the sample solid angle divided by an environment texel's solid angle, following the filtered importance-sampling treatment in [Karis's real-time shading notes](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf). A bright source texel is pre-averaged before it enters the GGX sum, preserving its energy without a luminance clamp.

## Incremental reconvergence

The persistent cube has five mips over a 256² base. A nine-slice schedule splits mip zero into five row bands and treats mips one through four as one slice each. `skyCaptureCadence` maps those slices over the requested number of frames.

Each invocation reads and writes only its own output texel. This permits an in-place exponential moving average:

```hlsl
float3 previous = outCube[output].rgb;
outCube[output] = float4(lerp(previous, prefiltered, push.blendAlpha), 1.0);
```

Startup uses `blendAlpha = 1.0`. An armed live capture uses `0.2`, so the result moves toward the refreshed sky without swapping image allocations or descriptor bindings.

## Sampling contract

The fragment samples along the reflection vector and derives a mip from perceptual roughness. The BRDF LUT then supplies the Fresnel scale and bias:

```hlsl
float3 prefiltered = prefilteredMap.SampleLevel(R, prefilterLod(roughness)).rgb;
float2 ab = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
float3 specularIBL = prefiltered * (F0 * ab.x + ab.y);
```

`IblPrefilterMaxMip = 4.0` matches `IBL_PREFILTER_MIPS - 1`.

## In the code

| What | File | Symbols |
|---|---|---|
| GGX and filtered importance sampling | `engine/assets/shaders/ibl_prefilter.slang` | `importanceSampleGGX`, `distributionGGX`, `computeMain` |
| EMA and row slicing | `engine/assets/shaders/ibl_prefilter.slang` | `Push`, `rowOffset`, `rowCount`, `blendAlpha` |
| Capture schedule | `engine/crates/rendering/src/ibl.rs` | `Ibl::scheduled_prefilter_slices`, `prefilter_slice` |
| Mip count and size | `engine/crates/rendering/src/ibl.rs` | `IBL_PREFILTER_MIPS`, `IBL_PREFILTER_SIZE` |
| Specular consumer | `engine/assets/shaders/lighting.slang` | `prefilterLod`, `prefilteredMap` |

## Related

- [Real-time sky-light capture](../realtime-skylight-capture/) explains the SH and specular scheduling split.
- [BRDF LUT](../brdf-lut/) explains the second split-sum factor.
- [Cubemaps and mips](../cubemaps-and-mips/) explains the persistent image and mip contract.
