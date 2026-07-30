+++
title = 'Real-time sky-light capture'
weight = 4
math = true
+++

# Real-time sky-light capture

Real-time sky-light capture keeps diffuse ambient light, glossy reflections, and indirect-light misses aligned with the environment cube. It projects diffuse lighting into a compact spherical-harmonic buffer every atmosphere frame and spreads the more expensive specular convolution across a configurable number of frames.

## One radiance representation

The capture stores nine RGB coefficients for a second-order real spherical-harmonic expansion. `sh_project.slang` samples a coarse environment mip, weights every cube texel by solid angle, and reduces the result in one compute workgroup. This follows the GPU projection described by [King](https://developer.nvidia.com/gpugems/gpugems2/part-ii-shading-lighting-and-shadows/chapter-10-real-time-computation-dynamic).

The buffer contains raw radiance coefficients $L_{lm}$. A sky ray reconstructs radiance directly:

$$
L(d)=\sum_{l=0}^{2}\sum_m L_{lm}Y_{lm}(d).
$$

A diffuse surface applies the cosine kernel from [Ramamoorthi and Hanrahan](https://cseweb.ucsd.edu/~ravir/papers/envmap/envmap.pdf):

$$
E(n)=A_0L_{00}Y_{00}+A_1\sum_mL_{1m}Y_{1m}(n)+A_2\sum_mL_{2m}Y_{2m}(n),
$$

where $A_0=\pi$, $A_1=2\pi/3$, and $A_2=\pi/4$. The shared `sky_sh.slang` module provides both reconstructions, which prevents the mesh, GI resolve, and DDGI trace from drifting to different basis conventions.

```hlsl
float3 diffuseIrradiance = skyShIrradiance(worldNormal, skyShCoefficients);
float3 escapedRayRadiance = skyShRadiance(rayDirection, skyShCoefficients);
```

## Amortized specular capture

The persistent prefiltered cube stores the split-sum environment term at five roughness levels. The [GGX prefilter](../specular-prefilter/) uses filtered importance sampling from [Karis](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf), including source-mip selection from each sample's solid angle.

A nine-slice base schedule divides mip 0 into five row bands and assigns one whole slice to each coarser mip. `skyCaptureCadence` maps those slices onto the requested frame window. Each dispatch blends its result into the existing texel, so reflections converge smoothly while the image view and descriptor remain stable.

```hlsl
float3 previous = outCube[output].rgb;
outCube[output] = float4(lerp(previous, prefiltered, push.blendAlpha), 1.0);
```

The startup capture uses a blend alpha of one. Live reconvergence uses an exponential moving average. The render graph imports the environment, SH buffer, and prefiltered cube, then derives the compute-to-compute and compute-to-fragment barriers from their declared uses.

## GI retint

`gi_resolve.slang` evaluates SH irradiance for the analytic sky fallback outside DDGI coverage. `ddgi_trace.slang` evaluates raw SH radiance only when a probe ray escapes the distance field. A miss therefore receives the sky color in its own direction, while a surface receives the cosine-filtered form exactly once.

## Capture cadence

The atmosphere's `skyCaptureCadence` value controls specular cost and latency. A value of `1` completes every armed reconvergence in one frame. The default `9` distributes the base schedule across nine frames, while larger values insert idle frames between slices. SH projection remains per-frame whenever the atmosphere is live.

## In the code

| What | File | Symbols |
|---|---|---|
| SH projection and reconstruction | `engine/assets/shaders/sh_project.slang`, `sky_sh.slang` | `computeMain`, `skyShRadiance`, `skyShIrradiance` |
| Persistent resources and schedule | `engine/crates/rendering/src/ibl/` | `LiveCapture`, `Ibl::add_live_capture_passes`, `prefilter_slice` |
| Specular reconvergence | `engine/assets/shaders/ibl_prefilter.slang` | `Push`, `blendAlpha`, `rowOffset`, `rowCount` |
| GI consumers | `engine/assets/shaders/gi_resolve.slang`, `ddgi_trace.slang` | `skyShCoefficients`, `skyShIrradiance`, `skyShRadiance` |
| Authoring control | `engine/crates/scene/src/environment.rs` | `AtmosphereSettings::sky_capture_cadence` |

## Related

- [IBL overview](../ibl-overview/) explains the full ambient-lighting composition.
- [Specular prefilter](../specular-prefilter/) explains the GGX convolution.
- [Procedural atmosphere](../procedural-atmosphere/) supplies the dynamic environment.
