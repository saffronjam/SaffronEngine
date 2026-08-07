+++
title = 'Global illumination & ray tracing'
weight = 12
bookCollapseSection = true
+++

# Global illumination & ray tracing

Dynamic global illumination computes indirect light that tracks moving geometry, and ray tracing
resolves visibility and direct lighting stochastically. [Image-based lighting](../image-based-lighting/)
supplies a static analytic ambient that DDGI replaces where its probes have coverage, and
[screen-space](../screen-space-and-post/) effects approximate indirect light from what is on screen.

The dynamic tier combines DDGI irradiance probes with a software distance-field trace: per-mesh
distance fields cover the near field, the Global Distance Field covers the far field, and misses
sample the sky. An optional hardware path builds BLAS and TLAS acceleration structures for inline
ray queries. ReSTIR uses that visibility path for many-light direct lighting.

> [!NOTE]
> Hardware ray tracing and ReSTIR require `VK_KHR_acceleration_structure`, `VK_KHR_ray_query`, and
> their corresponding device features. Capability probing keeps both paths disabled when that
> contract is unavailable. DDGI uses the software distance-field path and does not require it.

## Pages

| Page | Covers | Code |
|---|---|---|
| `ddgi-overview` | what DDGI is, the four-pass probe pipeline, sky-on-miss, the camera-centered clipmap, replacing the IBL diffuse by coverage | `lighting.slang` · `ddgiSampleIrradiance`; `rendering/src/renderer.rs` · `add_ddgi_passes` |
| `distance-field-reflection-occlusion` | a roughness-widened cone marching the Global Distance Field along the reflection vector to occlude the reflected skybox, and the occluder set the field is composited from | `sdf.slang` · `sdfReflectionOcclusion`, `gdfDistanceOccupancy`; `gi_occluder_micro.slang` · `computeMain` |
| `probe-volume-and-sampling` | the 16×8×16 camera-centered cage, the toroidal tile fold, octahedral encoding, trilinear + backface + Chebyshev weights | `lighting.slang` · `ddgiSampleIrradiance`, `ddgiOctEncode`; `rendering/src/ddgi.rs` · `Ddgi` |
| `software-ray-trace` | Fibonacci-sphere rays, sphere-marching the MDF→GDF field, sky-on-miss + albedo-cache hit color, free multi-bounce via probe reuse | `ddgi_trace.slang` · `computeMain`, `sphericalFibonacci`, `sampleAlbedo` |
| `irradiance-and-moment-atlases` | temporal irradiance blend, Chebyshev moment atlas, octahedral border wrap | `ddgi_blend_irradiance.slang`, `ddgi_blend_distance.slang`, `ddgi_border.slang` |
| `raytracing-foundation` | per-mesh BLAS, per-frame TLAS + instance buffer, buffer device address, deforming refits, and materialized wind and micro-blade geometry | `rendering/src/resources/` · `AccelerationStructure`; `rt/` · `record_mesh_blas_build`, `record_tlas_build_plan`; `rt_deform.rs` · `plan_wind_deformation`; `rt_micro.rs` · `plan_micro_rt_tiles` |
| `raytracing-device-gating` | optional RT extensions, `rt_supported`, the `ash::khr::acceleration_structure` dispatch | `rendering/src/device.rs` · `probe_optional_features`, `Device::accel_dispatch` |
| `ray-query-shadows` | inline `RayQuery` shadow rays in the mesh fragment, replacing shadow maps | `lighting.slang` · `rayQueryShadow`; `rendering/src/renderer.rs` · `set_rt_shadows` |
| `restir-overview` | reservoirs, RIS, the three-pass spatiotemporal resampling pipeline | `restir_initial.slang` · `Reservoir`; `rendering/src/restir.rs` · `Restir` |
| `restir-passes` | initial candidate sampling, temporal+spatial reuse, resolve + shading, M-clamping | `restir_initial.slang`, `restir_reuse.slang`, `restir_resolve.slang` |
