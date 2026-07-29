+++
title = 'Render graph API'
weight = 3
math = false
+++

# Render graph API

`saffron-rendering` exposes a declared pass graph, graph-owned buffers, queue compilation, and
profiled batch recording. Passes never record barriers. Vulkan handles use the `ash::vk` bindings.

| What | File | Symbols |
|---|---|---|
| Graph, passes, resources, and scheduling | `render_graph.rs` | `RenderGraph`, `RgPass`, `RgUsage`, `RgResource`, `RgSubmissionPlan` |
| Buffer allocation pool | `transient.rs` | `RenderGraphResources` |

## `RgUsage`

Each non-attachment access uses one synchronization intent.

| Variant | Resource and intent |
|---|---|
| `ColorWrite` | color-attachment image write |
| `DepthWrite` | early/late depth-attachment image write |
| `SampledRead` | fragment-sampled image read |
| `StorageWriteCompute` | compute storage-buffer write |
| `StorageReadCompute` | compute storage-buffer read |
| `StorageReadFragment` | fragment storage-buffer read |
| `StorageReadWriteCompute` | compute storage-buffer read and write |
| `StorageImageRwCompute` | compute storage-image read and write in `GENERAL` |
| `SampledReadCompute` | compute-sampled image read |
| `TransferRead` / `TransferWrite` | transfer buffer source or destination |
| `VertexInputRead` / `IndexInputRead` | vertex or index input buffer read |
| `AccelStructBuildRead` | acceleration-structure build input read |
| `ShaderDeviceAddressRead` | shader device-address buffer read |
| `IndirectCommandRead` / `IndirectCountRead` | indirect argument or count read |

[Barrier derivation](../../explanations/frame-and-render-graph/usage-and-barrier-derivation/)
lists the exact Vulkan stages, access masks, and image layouts.

## Passes

| Type | Purpose |
|---|---|
| `RgPassKind::Graphics` | the graph opens one dynamic-rendering scope around the body |
| `RgPassKind::GraphicsCommands` | the body opens multiple dynamic-rendering scopes |
| `RgPassKind::Compute` | the body records outside a rendering scope |
| `RgQueuePreference::Graphics` | require the graphics queue |
| `RgQueuePreference::AsyncCompute` | prefer independent compute, with graphics fallback |
| `RgAccess` | resource, usage, and optional byte range |
| `RgAttachment` | image, load/store operations, clear value, and optional resolve image |

Constructors and builders:

| Method | Effect |
|---|---|
| `RgPass::graphics(name, extent)` | graphics pass on the graphics queue |
| `RgPass::graphics_commands(name)` | graphics body that owns its rendering scopes |
| `RgPass::compute(name)` | compute pass on the graphics queue |
| `.queue(preference)` | opt a compute pass into independent compute |
| `.access(resource, usage)` | declare a whole-resource access |
| `.access_buffer(resource, range, usage)` | declare a byte-ranged buffer access |
| `.color(attachment)` | append a color attachment in MRT order |
| `.depth_attachment(attachment)` | set the depth attachment |
| `.body(\|cmd, scopes\| { ... })` | install the consumed recording closure |

`RgAttachment::clear_store(resource)` creates the common clear-and-store attachment without a
resolve target.

## Buffers

`RgBufferRange::new(offset, size)` creates a non-empty half-open byte range and rejects end
overflow. `RgBufferResource` records a handle, exact size, Vulkan usage flags, and lifetime.

| `RgBufferLifetime` | Ownership |
|---|---|
| `Imported` | allocation owned outside the graph |
| `Transient` | frame-slot allocation recyclable after its fence |
| `Persistent` | stable-key allocation retained across graph instances |

`RgBufferDesc { size, usage, lifetime }` is the request passed to `create_buffer`. A registered
buffer access asserts that the allocation carries the Vulkan usage flag required by its `RgUsage`.

## Cross-frame state

`RgExternalState::new(layout)` seeds an image layout before first use. Its slot later carries the
resolved layout, queue identity, ownership family, and synchronization scope. An
`RgExternalBufferState` slot carries the equivalent state per byte range.

| Method | Effect |
|---|---|
| `alloc_external_state(initial)` / `external_state(slot)` | allocate and read image state |
| `alloc_external_buffer_state(initial)` / `external_buffer_state(slot)` | allocate and read buffer state |

## `RenderGraph`

| Method | Effect |
|---|---|
| `RenderGraph::new()` | create an empty graph |
| `import_image(image, view, aspect, initial_layout, external)` | import a complete 2D, array, cube, or layered image |
| `import_image_3d(image, view, initial_layout, external)` | import a complete color 3D image |
| `import_buffer(buffer, external)` | import a whole-size buffer, optionally with cross-frame state |
| `register_buffer(resource)` | register a fully described imported or allocated buffer |
| `create_buffer(resources, frame, key, desc)` | allocate and register a graph-owned buffer |
| `add_pass(pass)` | append a pass in declaration order |
| `image(resource)` / `view(resource)` / `buffer(resource)` | resolve the underlying handle |
| `buffer_resource(resource)` | inspect a buffer's complete registration |

An image import represents the complete image. Derived barriers use remaining mip levels and array
layers from zero, so every represented subresource enters one layout together.

## Compilation and recording

`RgQueueFamilies` describes the graphics family and optional independent compute family. The graph
compiles without a device handle:

| Method | Result |
|---|---|
| `queue_assignments(families)` | one resolved `RgQueueAssignment` per pass |
| `barrier_schedule(families)` | per-pass before/after barriers and producer waits |
| `submission_plan(families)` | barriers plus maximal contiguous queue batches |

`RgSubmissionPlan` reports the number of graphics and compute command buffers it needs. Supply
those through `RgBatchCommandBuffers`, then call
`record_submission_plan_profiled(device, plan, commands, recorders)`. The result is one
`RgRecordedBatch` per submitted primary, with its queue, pass range, and earlier batch waits.

`execute` and `execute_profiled` record a graph into a supplied graphics command buffer using a
graphics-only topology. They serve callers whose surrounding submission already owns one graphics
command buffer.

## Related

- [Render graph](../../explanations/frame-and-render-graph/render-graph-overview/) — the model behind these types
- [Barrier derivation](../../explanations/frame-and-render-graph/usage-and-barrier-derivation/) — synchronization and ownership rules
- [Passes](../../explanations/frame-and-render-graph/passes-and-attachments/) — declaring attachments and bodies
- [Limits](../../explanations/frame-and-render-graph/limits-and-seams/) — current scheduling and resource boundaries
