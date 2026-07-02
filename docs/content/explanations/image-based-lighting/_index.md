+++
title = 'Image-based lighting'
weight = 9
bookCollapseSection = true
+++

# Image-based lighting

Image-based lighting is the ambient, indirect term of the lighting model, where the light arriving at a surface comes from an environment image instead of a flat scalar. An environment is convolved into a diffuse irradiance cube and a roughness-prefiltered specular cube. The split-sum BRDF lookup table is baked once at startup, and the mesh shader samples all three to produce energy-conserving ambient light.

## Pages

| Page | Covers | Code |
|---|---|---|
| `ibl-overview` | the split-sum approximation, diffuse + specular, DDGI replacing the IBL diffuse by coverage, [SDF reflection occlusion](../global-illumination-and-raytracing/distance-field-reflection-occlusion/) on the specular | `lighting.slang` · ambient block, `specSkyVis` |
| `cubemaps-and-mips` | `CUBE_COMPATIBLE` images, 6 layers, mip chains, dual views | `ibl.rs` · `IblCube` |
| `procedural-sky` | the analytic sky baked once (zenith/horizon gradient + sun disk) | `ibl_skygen.slang` |
| `procedural-atmosphere` | a Hillaire-2020 LUT chain that fills the env cube as a richer source than the gradient | `atmos_*.slang` · `Ibl::record_atmosphere` |
| `diffuse-irradiance` | cosine-weighted hemisphere convolution into a small cube | `ibl_irradiance.slang` |
| `specular-prefilter` | GGX importance-sampled prefilter, one mip per roughness | `ibl_prefilter.slang` |
| `brdf-lut` | the 2D Fresnel scale/bias lookup table | `ibl_brdf.slang` |
| `ibl-bake-pass` | the one-time startup compute dispatch + on-demand re-bake | `ibl.rs` · `Ibl::bake` |
| `reflection-probes` | per-entity local environment + nearest-probe blend over the global fallback | `ibl.rs` · `ReflectionProbes` |
