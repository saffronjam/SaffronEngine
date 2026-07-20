+++
title = 'Render graph API'
weight = 3
math = false
+++

# Render graph API

The types and methods of `saffron-rendering`'s render graph: the usage enum, the pass and attachment shapes, and the import / add / execute methods on `RenderGraph`. Vulkan handles are the `ash` `vk::` bindings. Resource state is internal; a pass declares its usage and the graph derives every barrier.

| What | File | Symbols |
|---|---|---|
| The graph, passes, and barrier derivation | `render_graph.rs` | `RenderGraph`, `RgPass`, `RgUsage`, `RgPassKind`, `RgResource`, `RgAccess`, `RgAttachment` |

## `RgUsage`

How a pass uses a non-attachment resource — the single source of truth for barrier derivation.

| Variant | Stage | Access | Layout |
|---|---|---|---|
| `ColorWrite` | ColorAttachmentOutput | ColorAttachmentWrite | ColorAttachmentOptimal |
| `DepthWrite` | Early+LateFragmentTests | DepthStencilAttachmentWrite | DepthAttachmentOptimal |
| `SampledRead` | FragmentShader | ShaderSampledRead | ShaderReadOnlyOptimal |
| `StorageWriteCompute` | ComputeShader | ShaderStorageWrite | (buffer) |
| `StorageReadCompute` | ComputeShader | ShaderStorageRead | (buffer) |
| `StorageReadFragment` | FragmentShader | ShaderStorageRead | (buffer) |
| `StorageImageRwCompute` | ComputeShader | ShaderStorageRead+Write | General |
| `SampledReadCompute` | ComputeShader | ShaderSampledRead | ShaderReadOnlyOptimal |
| `VertexInputRead` | VertexAttributeInput | VertexAttributeRead | (buffer) |
| `AccelStructBuildRead` | AccelerationStructureBuildKhr | ShaderRead | (buffer) |

## `RgPassKind`

| Variant | Effect |
|---|---|
| `Graphics` | the graph opens a `cmd_begin_rendering` / `cmd_end_rendering` scope around the body |
| `Compute` | the body runs bare (no rendering scope) |

## Structs

| Type | Fields |
|---|---|
| `RgResource` | `index: u32` — index into the graph resource table |
| `RgAccess` | `resource: RgResource`; `usage: RgUsage` |
| `RgAttachment` | `resource: RgResource`; `load_op: vk::AttachmentLoadOp`; `store_op: vk::AttachmentStoreOp`; `clear_value: vk::ClearValue`; `resolve: Option<RgResource>` (MSAA resolve target, color only) |
| `RgPass` | `name: String`; `kind: RgPassKind`; `accesses: Vec<RgAccess>`; `colors: Vec<RgAttachment>` (MRT: index 0 = location 0); `depth: Option<RgAttachment>`; `render_area: vk::Extent2D`; `execute: Option<Box<dyn FnOnce(vk::CommandBuffer)>>` |

`RgAttachment::clear_store(resource)` is the common CLEAR+STORE, no-resolve constructor.

## Building a pass

`RgPass` is built with a small builder chain:

| Method | Effect |
|---|---|
| `RgPass::graphics(name, render_area)` | start a graphics pass |
| `RgPass::compute(name)` | start a compute pass |
| `.access(resource, usage)` | declare a non-attachment usage |
| `.color(attachment)` | add a color attachment (MRT in order) |
| `.depth_attachment(attachment)` | set the depth attachment |
| `.body(\|cmd\| { … })` | record the pass body closure (consumed on execute) |

## `RenderGraph`

| Method | Effect |
|---|---|
| `RenderGraph::new()` | an empty graph |
| `import_image(image, view, aspect, initial_layout, external)` | track a complete external 2D image; `external: Option<usize>` is a cross-frame layout slot |
| `import_image_3d(image, view, initial_layout, external)` | track a complete external 3D image (GDF cascade volume, lite albedo cache) |
| `import_buffer(buffer)` | track an external buffer; returns an `RgResource` |
| `alloc_external_layout(initial)` / `external_layout(slot)` | allocate / read a cross-frame layout slot |
| `add_pass(pass)` | append an `RgPass` |
| `image(resource)` / `view(resource)` / `buffer(resource)` | resolve a handle for a pass body |
| `execute(device, cmd)` | derive + emit barriers, record each body, write back cross-frame layouts |
| `execute_profiled(device, cmd, recorders)` | the same, with GPU-timestamp / CPU-span recorders armed |

Image imports track one state for the complete image. Derived image barriers start at mip and layer
zero and use `VK_REMAINING_MIP_LEVELS` plus `VK_REMAINING_ARRAY_LAYERS`, so the declared usage covers
every mip and array layer represented by the imported handle.

## Related

- [Render graph](../../explanations/frame-and-render-graph/render-graph-overview/) — the model behind these types
- [Usage and barrier derivation](../../explanations/frame-and-render-graph/usage-and-barrier-derivation/) — how `RgUsage` becomes a barrier
- [Passes and attachments](../../explanations/frame-and-render-graph/passes-and-attachments/) — declaring a pass
