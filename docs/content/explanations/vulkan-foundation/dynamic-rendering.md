+++
title = 'Dynamic rendering'
weight = 5
+++

# Dynamic rendering

[Dynamic rendering](https://docs.vulkan.org/samples/latest/samples/extensions/dynamic_rendering/README.html)
lets a command buffer begin a graphics scope with the image views, layouts, load operations, store
operations, and resolve targets needed for that pass. Anima requires the Vulkan 1.3
`dynamicRendering` feature and constructs these scopes from render-graph declarations.

This keeps attachment binding local to each graph pass. A pass declares its color and depth outputs,
the graph prepares their layouts, and `record_graphics` turns that declaration into
`vk::RenderingInfo` immediately before recording draw commands.

## Pass declaration

`RgPass::graphics` carries a render extent, an ordered list of color attachments, an optional depth
attachment, and a recording closure. A typical color pass has this shape:

```rust
let pass = RgPass::graphics("composite", extent)
    .color(RgAttachment::clear_store(output))
    .access(input, RgUsage::SampledRead)
    .body(move |cmd, _scopes| {
        // Bind the compatible pipeline and descriptors, then draw.
    });
graph.add_pass(pass);
```

Color attachment order maps to fragment output locations. Multiple calls to `color` form an MRT pass,
as used by the normal and roughness targets in the thin G-buffer. `depth_attachment` adds the pass's
single depth binding.

`RgPass::compute` carries resource accesses and a body but no render area or attachments. During graph
execution its body records directly after the derived barriers.

## Recording sequence

`RenderGraph::execute_profiled` processes passes in graph order. For each pass it derives and emits
`pipeline_barrier2` dependencies, opens profiling scopes, then dispatches according to `RgPassKind`.
Graphics work enters `record_graphics`; compute work invokes the body directly.

For a graphics pass, `record_graphics` performs this sequence:

1. Build one `vk::RenderingAttachmentInfo` for each color attachment.
2. Build the optional depth attachment info.
3. Create `vk::RenderingInfo` for the pass extent and one layer.
4. Call `cmd_begin_rendering`.
5. Set the full-area viewport and scissor.
6. Invoke the pass body.
7. Call `cmd_end_rendering`.

The rendering scope brackets only graphics commands for that pass. Barriers remain outside the scope
and are derived from the same attachment and access declarations.

## Attachment state

`RgAttachment` holds the graph resource, load operation, store operation, clear value, and optional
resolve target. `clear_store` represents the common zero-clear and store policy; depth helpers supply a
depth clear of `1.0` or a load-only policy for overlays.

Color attachments enter the scope as `COLOR_ATTACHMENT_OPTIMAL`. Depth attachments use
`DEPTH_ATTACHMENT_OPTIMAL`. Dynamic rendering consumes those declared layouts but does not perform the
transitions, so `derive_pass_barriers` must move every image into the required state first.

The attachment's load operation determines whether its previous contents are preserved, cleared, or
ignored. Its store operation determines whether contents remain defined after the scope. These choices
also communicate whether an intermediate multisampled image needs to survive the pass.

## Multisample resolve

An attachment may name a second graph resource as its resolve target. `record_graphics` adds that view
and layout to the same `RenderingAttachmentInfo`, using `AVERAGE` for color and `SAMPLE_ZERO` for depth.
Both the multisampled attachment and resolve target are tracked as writes by barrier derivation.

The main scene pass renders into multisampled color and depth images when MSAA is active. Their store
operations become `DONT_CARE`, while the single-sample color and depth targets receive the resolve.
Later post-processing and overlay passes consume those single-sample resources.

## Pipeline compatibility

Dynamic rendering still requires a graphics pipeline to describe its attachment interface.
`vk::PipelineRenderingCreateInfo` supplies the ordered color formats and optional depth format when the
pipeline is created. The bound attachments must match those formats, and pipeline multisample state must
match their sample count.

Pipelines declare viewport and scissor as dynamic state. Every graph graphics pass sets both from its
`render_area`, allowing the same compatible pipeline to record at different viewport dimensions.
Individual pipelines may add dynamic cull mode or depth bias where their draw path needs it.

## Feature boundary

Physical-device evaluation rejects devices without `dynamicRendering`, and logical-device creation
enables it through `PhysicalDeviceVulkan13Features`. The render graph can therefore use
`cmd_begin_rendering` for every graphics pass without a secondary feature branch.

```mermaid
flowchart LR
    A[RgPass attachments and accesses] --> B[derive_pass_barriers]
    B --> C[RenderingAttachmentInfo list]
    C --> D[cmd_begin_rendering]
    D --> E[viewport + scissor + pass body]
    E --> F[cmd_end_rendering]
```

## In the code

| What | File | Symbols |
|---|---|---|
| Pass and attachment declarations | `render_graph.rs` | `RgPass`, `RgPassKind`, `RgAttachment`, `RgUsage` |
| Graph execution | `render_graph.rs` | `execute`, `execute_profiled`, `record_graphics` |
| Barrier derivation | `render_graph.rs` | `derive_pass_barriers`, `usage_info`, `apply_access` |
| Scene attachment policies | `renderer.rs` | `depth_clear_store`, `depth_load_readonly` |
| Pipeline attachment formats | `pipelines.rs` | `PipelineRenderingCreateInfo`, `PipelineDynamicStateCreateInfo` |
| Device feature gate | `device.rs` | `evaluate_device`, `create_logical_device` |

## Related

- [Render graph overview](../../frame-and-render-graph/render-graph-overview/) - covers pass ordering and resource tracking
- [Passes and attachments](../../frame-and-render-graph/passes-and-attachments/) - lists declaration patterns
- [Synchronization2 and barriers](../synchronization2-and-barriers/) - explains the transitions before each scope
- [MSAA](../../anti-aliasing/msaa/) - covers multisampled targets and resolves
