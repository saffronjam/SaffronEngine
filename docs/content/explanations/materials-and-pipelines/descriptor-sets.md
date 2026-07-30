+++
title = 'Descriptor sets'
weight = 3
+++

# Descriptor sets

A Vulkan [`VkDescriptorSet`](https://registry.khronos.org/vulkan/specs/latest/man/html/VkDescriptorSet.html) groups shader-visible buffers, images, samplers, and acceleration structures under one pipeline-layout slot. A shader declaration and its Rust descriptor-set layout must agree on binding number, descriptor type, array count, and shader stages.

Anima groups the mesh resources by ownership. The resulting layout remains compatible across every material PSO, so scene recording can bind the shared sets before replaying the frame's GPU-binned counted-indirect draw buckets.

## Mesh pipeline contract

The mesh shader uses sets 0 through 5 on every device. Ray-tracing devices add sets 6 and 7. `Pipelines::new` assembles the Rust layout in the same order that Slang's `vk::binding(binding, set)` attributes declare.

| Set | Owner | Bindings |
|---:|---|---|
| 0 | Global bindless images | `0` material 2D textures; `1-3` mesh-SDF atlas, indirection, and coverage; `4` height min/max pyramids |
| 1 | Frame lighting | `0` globals; `1` punctual lights; `2` cluster lists; `3` cluster params; `8-10` SDF/GDF data; `11` froxel integration; `12` cloud shadows; `13` virtual-shadow atlas |
| 2 | Frame geometry data | `0` tess-seam instance rows; `1` current joint palette; `2` the global material-parameter arena; `3` the GPU-scene address block; `4` the frame's record stream |
| 3 | IBL and reflection probes | `0-2` sky-radiance SH buffer, prefiltered environment, and BRDF LUT; `3-4` probe cube arrays; `5` probe metadata |
| 4 | Screen-space lighting | `0-7` sampled images for AO, contact shadows, SSGI, SSR, previous color, DFAO, specular occlusion, and resolved indirect diffuse; `8` shared immutable linear sampler |
| 5 | DDGI | `0` irradiance atlas; `1` distance atlas |
| 6 | Ray tracing | `0` TLAS |
| 7 | ReSTIR | `0` resolved direct-light radiance |

Set 0 is device-global. Sets 1 through 3 select frame-in-flight resources: set 3 shares persistent
IBL images but pairs each descriptor set with that slot's reflection-probe metadata buffer. Sets 4
and 5 select the active renderer view. Set 6 selects the current frame's TLAS, while set 7 selects
the active view's ReSTIR result.

## Shader declarations

The shared `lighting` module supplies most of the descriptor interface imported by `mesh.slang`; the GPU-scene address block declares in `global_gpu_data.slang` and the record stream in `mesh.slang` itself:

```hlsl
[[vk::binding(0, 0)]] Sampler2D albedoTextures[1024];
[[vk::binding(0, 1)]] ConstantBuffer<LightGlobals> globals;
[[vk::binding(1, 1)]] StructuredBuffer<GpuLight> lights;
[[vk::binding(0, 2)]] StructuredBuffer<Instance> instances;
[[vk::binding(2, 2)]] StructuredBuffer<MaterialParams> materialParams;
[[vk::binding(3, 2)]] ConstantBuffer<GpuSceneAddressBlock> gpuSceneAddresses;
[[vk::binding(4, 2)]] StructuredBuffer<GpuDrawRecord> executorRecords;
[[vk::binding(0, 3)]] StructuredBuffer<float4> skyShCoefficients;
[[vk::binding(7, 4)]] Texture2D<float4> giIndirectMap;
[[vk::binding(8, 4)]] SamplerState screenLinearSampler;
[[vk::binding(0, 5)]] Sampler2D ddgiIrradiance;
```

The Rust layout builders mirror those declarations. For example, `create_instance_layout` exposes four storage-buffer bindings plus the address-block uniform with vertex or fragment visibility matching their consumers. `create_ibl_layout` gives each reflection-probe array eight descriptors, matching `MAX_REFLECTION_PROBES` and the shader array declarations. Set 4 separates its images from one immutable sampler, keeping the full mesh interface within portability devices' per-stage sampler limit. The two comparison samplers in set 1 are also immutable because portability devices may not support mutable comparison samplers.

## Ray-tracing variant

Sets 6 and 7 require ray-tracing descriptor types and resources. When the device supports the path, `Pipelines` appends both layouts, the shared descriptor pool gains acceleration-structure capacity, and the renderer loads the normal mesh shader variant.

The RT-off shader is compiled with `SAFFRON_NO_RT`. Preprocessor guards remove `rtScene`, `restirRadiance`, and the ray-query helpers from its interface. Its pipeline layout therefore ends at set 5, which is required for strict shader-layout matching on MoltenVK as well as Vulkan validation.

## Recording cost

`bind_mesh_descriptor_sets` binds the shared descriptor state once for an opaque or translucent scope:

```text
call 1: set 0
call 2: sets 1 and 2 together
call 3: set 3
call 4: set 4
call 5: set 5
call 6: set 6, on an RT device
call 7: set 7, on an RT device
```

Bucket replay changes pipelines but does not rebind these sets: every pass replays counted-indirect commands over the global pages arena, bound once as the index buffer, while the ubershader's `vertexMainExecutor` pulls vertices through buffer device address. `scene_pass_bind_count` reports five descriptor-binding calls for a non-RT scope and adds one for each RT set. This count is independent of the number of draw buckets.

Depth-family pipelines use compatible prefixes of the same layout. `record_executor_depth_family` records the depth prepass, the virtual-shadow pages, the G-buffer, motion, the wireframe overlay, and the reactive-coverage mask with one pipeline per pass, binding set 0 for the albedo alpha and set 2 for the record stream and material params, plus that pass's push-constant bytes.

## Feature flags and valid fallbacks

Optional lighting effects use flags in `LightGlobals` to decide whether to sample their resources. The layouts do not change when AO, SSR, DDGI, or another runtime effect is toggled.

Resource owners initialize neutral images and valid descriptor sets before drawing. Disabled screen-space effects therefore keep set 4 bound to neutral maps, and the shader branch skips or harmlessly samples them. This avoids pipeline churn while satisfying Vulkan's requirement that statically used descriptors be bound.

## In the code

| What | File | Symbols |
|---|---|---|
| Device-global layout builders | `engine/crates/rendering/src/descriptors/` | `create_bindless_layout`, `create_light_layout`, `create_instance_layout`, `create_ibl_layout`, `create_ssao_mesh_layout` |
| Pipeline layout assembly | `engine/crates/rendering/src/pipelines/` | `Pipelines::new`, `set_layouts`, `rt_enabled` |
| Mesh descriptor declarations | `engine/assets/shaders/lighting.slang`, `global_gpu_data.slang` | `vk::binding`, `LightGlobals`, `MaterialParams`, `GpuSceneAddressBlock`, `SAFFRON_NO_RT` |
| Scope-level descriptor binding | `engine/crates/rendering/src/scene_pass.rs` | `bind_mesh_descriptor_sets`, `scene_pass_bind_count`, `record_executor_buckets`, `record_executor_depth_family` |
| Frame sets 1 through 3 | `engine/crates/rendering/src/lighting/`, `instancing.rs`, `ibl.rs` | `Lighting::light_set`, `Instancing::instance_set`, `Ibl::set`, `ReflectionProbes::prepare_frame` |

## Related

- [Bindless textures](../bindless-textures/) - set 0 arrays, slot allocation, and non-uniform indexing
- [Material and PSO selection](../material-and-pso-selection/) - the compatible pipeline variants that share this layout
- [Clustered forward+](../../lighting-and-brdf/clustered-forward/) - data supplied by the light and cluster bindings
- [ReSTIR overview](../../global-illumination-and-raytracing/restir-overview/) - the RT-only set 7 consumer
