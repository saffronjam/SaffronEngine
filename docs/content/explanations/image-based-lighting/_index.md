+++
title = 'Image-based lighting'
weight = 9
bookCollapseSection = true
+++

# Image-based lighting

Image-based lighting is the ambient, indirect term of the lighting model, where light arrives from an environment instead of a flat scalar. The environment projects into nine spherical-harmonic coefficients for diffuse irradiance and a roughness-prefiltered cube for specular reflections. A split-sum BRDF lookup table supplies the material-dependent specular factor.

## Pages

| Page | Covers | Code |
|---|---|---|
| `ibl-overview` | the split-sum approximation, diffuse + specular, DDGI replacing the IBL diffuse by coverage, [SDF reflection occlusion](../global-illumination-and-raytracing/distance-field-reflection-occlusion/) on the specular | `lighting.slang` · ambient block, `specSkyVis` |
| `cubemaps-and-mips` | `CUBE_COMPATIBLE` images, 6 layers, mip chains, dual views | `ibl.rs` · `IblCube` |
| `procedural-sky` | the analytic zenith, horizon, ground, and sun environment | `ibl_skygen.slang` |
| `procedural-atmosphere` | the dynamic Hillaire LUT refresh, atmosphere-coupled sun and moon lights, and physical discs | `atmos_*.slang` · `Ibl::update_refresh` · `sun_transmittance` |
| `realtime-skylight-capture` | per-frame SH diffuse, amortized specular reconvergence, and GI retint | `sh_project.slang` · `Ibl::add_live_capture_passes` |
| `time-of-day` | ephemeris, scene clock, elevation curves, and manual celestial control | `time_of_day.rs` · `drive_time_of_day` |
| `night-sky` | catalog stars, Milky Way, lunar phase, and scotopic adaptation | `stars.slang` · `StarCatalog` |
| `volumetric-cloud-shape` | weather-map authoring, dimensional profiles, value erosion, and the unlit density view | `clouds.slang` · `Clouds` · `ViewMode::CloudDensity` |
| `volumetric-cloud-lighting` | adaptive lighting, temporal reconstruction, and full-resolution cloud outputs | `cloud_raymarch.slang` · `cloud_reconstruct.slang` · `cloud_upscale.slang` |
| `cloud-integration` | shared atmosphere sampling, cascaded cloud shadows, fog god-rays, the transmittance ledger, and global wind | `atmos_ap.slang` · `cloud_shadow.slang` · `height_fog.slang` |
| `specular-prefilter` | GGX importance-sampled prefilter, one mip per roughness | `ibl_prefilter.slang` |
| `brdf-lut` | the 2D Fresnel scale/bias lookup table | `ibl_brdf.slang` |
| `ibl-bake-pass` | the one-time startup compute dispatch + on-demand re-bake | `ibl.rs` · `Ibl::bake` |
| `reflection-probes` | per-entity local environment + nearest-probe blend over the global fallback | `ibl.rs` · `ReflectionProbes` |
