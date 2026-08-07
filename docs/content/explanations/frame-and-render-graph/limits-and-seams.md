+++
title = 'Limits'
weight = 6
+++

# Limits

The render graph allocates buffers, assigns queues, derives synchronization, and records passes.
It preserves declaration order and does not own image memory, cull passes, or alias allocations.
Those boundaries keep the frame predictable while leaving clear seams for graph analysis.

```mermaid
flowchart LR
    A[declared passes<br/>+ resource usage] --> B[assign queues<br/>+ derive barriers]
    B --> C[batch contiguous passes<br/>in declaration order]
    C --> D[record graphics<br/>and compute command buffers]
    D --> E[timeline-link<br/>dependent submissions]
```

## Queue assignment is capability-driven

`RgPass::compute` prefers an independent compute queue. The submission compiler assigns it there
only when the device exposes the queue and every resource can enter it with known ownership. The
same pass stays on graphics otherwise. Graphics passes and `GraphicsCommands` passes always use the
graphics queue.

A dependency between queues produces matching release and acquire barriers plus a timeline
semaphore wait. This applies when the queues use different families and when two queues in one
family still need execution ordering. The declared graph therefore has one behavior contract across
dedicated-compute, shared-family, and graphics-only devices.

The scheduler does not search for a globally optimal overlap. It retains declaration order and
groups maximal contiguous runs for the same queue. This gives the driver independent command
buffers without allowing scheduling to change the engine's intended frame sequence.

## Buffers can be graph-owned

`RenderGraph::create_buffer` acquires an allocation from `RenderGraphResources` and registers its
exact size, usage flags, and lifetime. A transient buffer is keyed by frame slot and can recycle
after that slot's fence signals. A persistent buffer retains a stable key and its cross-frame
byte-range state.

Imported buffers use the same resource table. `access_buffer` narrows synchronization to a checked,
half-open byte range, so unrelated regions do not create false hazards. Whole-buffer access remains
available for resources that do not need range precision.

Images are imported. `RenderGraphResources` owns scratch images, view targets own their attachments,
and both enter the graph through `import_image` or `import_image_3d`. The graph does not create or
alias image allocations. Consequently, targets with disjoint lifetimes still occupy separate image
memory.

## Every declared pass records

The graph records every pass from front to back. It performs no reachability analysis to remove a
pass whose output is unused, and it does not reorder passes to shorten dependencies. Pruning happens
during construction: `Renderer::record_scene_graph` adds shadows, deformation, screen-space effects,
and other optional work only when the frame needs them.

Declaration order also defines batching. Adjacent passes assigned to the same queue share a primary
command buffer; a queue change begins another batch. Dependencies name earlier batch indices, making
submission ordering explicit and device-free to test.

## Image state covers the whole image

An imported image has one tracked state. Barriers begin at mip zero and array layer zero and use
`VK_REMAINING_MIP_LEVELS` and `VK_REMAINING_ARRAY_LAYERS`, so one declaration covers every mip and
layer represented by the image.

This model cannot keep different subresources in different layouts at the same time. No pass needs
that exception: the virtual-shadow page passes render disjoint tile regions of one atlas image under
a single tracked state, and multi-scope bodies (a `GraphicsCommands` pass opening its own
dynamic-rendering scopes) declare their complete images so the graph transitions each once. No pass
writes a manual image barrier.

## In the code

| What | File | Symbols |
|---|---|---|
| Queue resolution and batching | `render_graph.rs` | `RgQueueFamilies`, `queue_assignments`, `submission_plan`, `RgPassBatch` |
| Cross-queue synchronization | `render_graph.rs` | `RgPassBarriers`, `barrier_schedule`, `record_submission_plan_profiled` |
| Graph-owned buffers | `render_graph.rs`, `transient.rs` | `create_buffer`, `RgBufferDesc`, `RgBufferLifetime`, `RenderGraphResources` |
| Byte-range tracking | `render_graph.rs` | `RgBufferRange`, `access_buffer`, `RgExternalBufferState` |
| Imported images | `render_graph.rs`, `transient.rs` | `import_image`, `import_image_3d`, `RenderGraphResources::acquire_image` |
| Conditional construction | `renderer.rs` | `Renderer::record_scene_graph` |

## Related

- [Render graph](../render-graph-overview/) — the model these boundaries belong to
- [Barrier derivation](../usage-and-barrier-derivation/) — ranges, ownership, and hazard rules
- [Cross-frame layouts](../cross-frame-layouts/) — state carried between graph instances
- [Adding passes](../who-can-add-passes/) — who constructs the graph each frame
