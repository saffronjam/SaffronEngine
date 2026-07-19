+++
title = 'Descriptor sets'
weight = 6
math = false
+++

# Descriptor sets

The descriptor sets bound by the mesh übershader. The binding declarations live in `lighting.slang` (the shared header `mesh.slang` includes); each is spelled `[[vk::binding(b, set)]]`. Sets 0–4 are core (bindless textures, lighting, instances, IBL, screen-space maps); sets 5–7 are the GI / RT extensions. The push constant is the camera `viewProj` (`float4x4`, `vk::push_constant Camera camera`).

| What | File | Symbols |
|---|---|---|
| The übershader entry points | `engine/assets/shaders/mesh.slang` | `vertexMain`, `fragmentMain` |
| The set/binding declarations | `engine/assets/shaders/lighting.slang` | `albedoTextures`, `globals`, `instances`, `materialParams`, `irradianceMap`, … |

## Set 0 — bindless albedo

| Binding | Slang type | Note |
|---|---|---|
| 0 | `Sampler2D albedoTextures[1024]` | one global array, indexed per-instance; size = `MAX_BINDLESS_TEXTURES` |

## Set 1 — lighting

| Binding | Slang type | Note |
|---|---|---|
| 0 | `ConstantBuffer<LightGlobals> globals` | directional + ambient + eye + shadow transforms + feature flags |
| 1 | `StructuredBuffer<GpuLight> lights` | per-frame punctual light list |
| 2 | `StructuredBuffer<Cluster> clusters` | per-froxel light index lists (cap 64) |
| 3 | `ConstantBuffer<ClusterParams> clusterParams` | view, inverse-projection, grid dims, screen size, z planes |
| 4 | `Sampler2DShadow shadowMap` | directional depth map (PCF compare) |
| 5 | `Sampler2DShadow spotShadowMap` | spot depth map (PCF compare) |
| 6 | `SamplerCube pointShadowMap` | omnidirectional distance cube |

`LightGlobals.counts`: x = punctual count, y = directional shadow, z = IBL, w = SSAO. `screenFlags`: x = contact shadows, y = SSGI, z = DDGI, w = ReSTIR.

## Set 2 — instances and material params

| Binding | Slang type | Note |
|---|---|---|
| 0 | `StructuredBuffer<Instance> instances` | per-instance model + normalMatrix + prevModel + baseColor + texture indices + pbr + emissive; indexed by `SV_VulkanInstanceID` |
| 1 | `StructuredBuffer<float4x4> jointMatrices` | the joint palette (`worldBone * inverseBind`) for skinned draws |
| 2 | `StructuredBuffer<MaterialParams> materialParams` | deduplicated per-frame material params, indexed by `instance.texture.w` |

## Set 3 — IBL and reflection probes

| Binding | Slang type | Note |
|---|---|---|
| 0 | `StructuredBuffer<float4> skyShCoefficients` | nine raw-radiance sky SH coefficients |
| 1 | `SamplerCube prefilteredMap` | global GGX-prefiltered specular (max mip 4) |
| 2 | `Sampler2D brdfLut` | split-sum (scale, bias) LUT |
| 3 | `SamplerCube probeCubes[8]` | per-probe prefiltered specular cubes (`MaxReflectionProbes` = 8) |
| 4 | `SamplerCube probeIrradiance[8]` | per-probe diffuse irradiance cubes |
| 5 | `StructuredBuffer<ProbeMeta> probeMeta` | per-probe origin / radius / box extent / intensity / flags |

The global IBL is sampled for ambient when `globals.counts.z != 0`; the probe arrays are blended in when `ambientColor.w` (the probe count) is non-zero.

## Set 4 — screen-space maps

| Binding | Slang type | Gate | Note |
|---|---|---|---|
| 0 | `Sampler2D aoMap` | `counts.w` | AO factor (1 = open); darkens indirect |
| 1 | `Sampler2D contactMap` | `screenFlags.x` | contact-shadow factor (1 = lit); darkens directional direct |
| 2 | `Sampler2D ssgiMap` | `screenFlags.y` | one-bounce GI radiance (rgba16f) |

All sampled by screen UV.

## Sets 5–7 — GI / RT extensions

| Set | Binding | Slang type | Gate |
|---|---|---|---|
| 5 | 0 | `Sampler2D ddgiIrradiance` | `screenFlags.z` (DDGI octahedral irradiance atlas) |
| 5 | 1 | `Sampler2D ddgiDistance` | `screenFlags.z` (DDGI moment atlas) |
| 6 | 0 | `RaytracingAccelerationStructure rtScene` | RT device support (set omitted from the layout otherwise) |
| 7 | 0 | `Sampler2D restirRadiance` | `screenFlags.w` (replaces the punctual loop) |

## Related

- [Descriptor sets](../../explanations/materials-and-pipelines/descriptor-sets/) — how the sets are laid out and bound
- [Bindless textures](../../explanations/materials-and-pipelines/material-and-pso-selection/) — the set 0 array
- [Clustered forward](../../explanations/lighting-and-brdf/clustered-forward/) — set 1's cluster lists
