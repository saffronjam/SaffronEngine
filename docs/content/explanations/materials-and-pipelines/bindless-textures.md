+++
title = 'Bindless textures'
weight = 4
+++

# Bindless textures

Bindless texturing stores sampled images in device-wide descriptor arrays. Materials carry integer slots into those arrays, so changing a texture does not require a new descriptor set or pipeline bind for each material.

The renderer uses [Vulkan descriptor indexing](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_EXT_descriptor_indexing.html), promoted to Vulkan 1.2, for non-uniform array access and descriptor updates after binding.

## Global set 0

Every mesh pipeline begins with one global descriptor set. Its five bindings serve different image families:

| Binding | Array | Slots | Consumer |
|---:|---|---:|---|
| 0 | 2D material and environment textures | 1,024 | fragment and compute shaders |
| 1 | mesh-SDF brick atlases | `MAX_BINDLESS_SDF` | compute shaders |
| 2 | mesh-SDF indirection volumes | `MAX_BINDLESS_SDF` | compute shaders |
| 3 | mesh-SDF coverage volumes | `MAX_BINDLESS_SDF` | compute shaders |
| 4 | displacement min/max pyramids | 1,024 | tessellation-factor compute shader |

Bindings 1 through 3 share one SDF slot. Binding 4 shares the 2D texture slot, so a displacement height index addresses both the source texture and its min/max pyramid.

Each binding uses `PARTIALLY_BOUND` and `UPDATE_AFTER_BIND`, and the layout comes from an `UPDATE_AFTER_BIND_POOL`. Device selection also requires `runtime_descriptor_array`, `shader_sampled_image_array_non_uniform_indexing`, and the two matching descriptor-binding features.

## Material indexing

The frame builder resolves each material texture to a stable slot. It packs those slots into `MaterialParamsData`:

```rust
pub struct MaterialParamsData {
    pub base_color: Vec4,
    pub pbr: Vec4,
    pub emissive: Vec4,
    pub uv: Vec4,
    pub tex0: UVec4, // albedo, ORM/MR, normal, emissive
    pub tex1: UVec4, // height, occlusion, reserved, feature bits
}
```

The table is deduplicated by its 96 raw bytes for the current frame. `InstanceData.texture.w` selects a material-table entry, while the fragment shader samples its texture slots with `NonUniformResourceIndex`:

```hlsl
MaterialParams mat = materialParams[input.materialIndex];
float4 base = albedoTextures[NonUniformResourceIndex(mat.tex0.x)].Sample(uv);
```

The index may vary between shader invocations in one draw. Non-uniform indexing tells Vulkan that the array access is not subgroup-uniform.

## Upload and defaults

`Uploader::upload_texture` creates the image and mip chain, claims a slot, writes binding 0, and returns a `GpuTexture` that owns the slot. The shared sampler uses linear minification, magnification, and mip filtering with repeat addressing. Anisotropy is enabled up to the device limit when supported.

Slot 0 contains a 1-by-1 white texture. Missing maps resolve to this slot, which leaves material factors unchanged. Initialization writes that white view into all 1,024 elements of binding 0. Real uploads overwrite their claimed element.

The renderer applies the same complete-seeding rule to SDF bindings and displacement min/max pyramids. Defaults make every declared array element valid, while descriptor-indexing flags still permit live slot replacement.

## Allocation and lifetime

The 2D texture and SDF families have separate `SlotAllocator` instances. Each allocator reuses a slot from its LIFO free list before increasing its high-water mark. A claim returns `None` at capacity, so the upload path cannot write beyond the descriptor array.

`GpuTexture::drop` returns its slot through `BindlessFreeList`. Draw-list construction keeps every sampled `Arc<GpuTexture>` in `live_textures` for the frame, so a descriptor cannot be reclaimed while recorded draws still reference it. Descriptor writes and slot allocation share a mutex because Vulkan requires host access to a descriptor set to be externally synchronized.

The following command exposes the allocator counters as `bindlessTextures` and `bindlessFree`:

```sh
sa render-stats
```

`bindlessTextures` is the high-water count, including slot 0. `bindlessFree` is the number of returned 2D slots available for reuse.

## Batch consequence

Texture identity lives in material data rather than the draw-bucket key. Instances that share mesh geometry and a compatible PSO can remain in one batch even when their material-table entries select different images. Other material state, such as blend mode, culling, skinning, and shader graph, can still select a different batch or pipeline.

## In the code

| What | File | Symbols |
|---|---|---|
| Global arrays, flags, and allocators | `engine/crates/rendering/src/descriptors.rs` | `Descriptors`, `create_bindless_layout`, `SlotAllocator`, `MAX_BINDLESS_TEXTURES` |
| Descriptor-indexing feature gate | `engine/crates/rendering/src/device.rs` | `evaluate_device`, `create_logical_device`, `runtime_descriptor_array` |
| Texture upload and default seeding | `engine/crates/rendering/src/upload.rs` | `Uploader::upload_texture`, `Uploader::upload_default_white`, `mip_count` |
| Slot-owning resources | `engine/crates/rendering/src/resources.rs` | `BindlessFreeList`, `GpuTexture`, `GpuSdf` |
| Material and instance tables | `engine/crates/rendering/src/gpu_types.rs` · `instancing.rs` | `MaterialParamsData`, `InstanceData`, `build_instance_rows`, `resolve_material` |
| Shader sampling | `engine/assets/shaders/lighting.slang` · `mesh.slang` | `albedoTextures`, `MaterialParams`, `NonUniformResourceIndex` |

## Related

- [Descriptor sets](../descriptor-sets/) - set 0 in the complete mesh pipeline layout
- [Material and PSO selection](../material-and-pso-selection/) - material state that changes batches or pipelines
- [Ubershader and specialization](../ubershader-and-specialization/) - specialization constants and graph-generated shader variants
- [Compute displacement](../../frame-and-render-graph/compute-displacement/) - the height texture and min/max pyramid consumers
