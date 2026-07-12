+++
title = 'Limits'
weight = 6
+++

# Limits

The limits of a render graph are the optimizations a mature graph can perform but a given
implementation chooses to leave out. Anima's graph derives barriers and layout transitions from
declared usage and does nothing more; several features a fuller graph eventually grows are absent
by design.

Each omission has a seam already in place: the data a future feature would need is declared, so
adding it later is a contained change rather than a rewrite. The sections below name each limit,
its cost, and the seam that anticipates it.

## Single graphics queue

Every pass records into one command buffer on one graphics queue, in declaration order.
`RenderGraph::execute` walks `passes` start to finish into one command buffer. A pass
has no queue selection and there is no second timeline.

This rules out async compute, where a compute pass runs concurrently on a dedicated compute queue
while the graphics queue does other work. The light-cull and screen-space passes are compute, but
they run inline on the graphics queue, serialized by the same barriers as everything else.

The seam is `RgPass::kind` (`Graphics` / `Compute`) and a barrier model that is stage- and
access-based rather than queue-based. Adding a queue field and a cross-queue semaphore where the
timeline splits is the work; the usage declarations would not change.

## No transient resources, no aliasing

The graph allocates nothing. Every resource is imported — an existing renderer-owned handle
registered with `import_image` / `import_buffer` each frame. There are no graph-created images and
no memory aliasing, which reuses one allocation for two resources whose lifetimes do not overlap.

The cost is memory. The G-buffer normal target, the AO maps, the FXAA scratch, and the TAA history
and motion targets each hold their own allocation for the whole frame, though many never overlap in
time. A graph that allocated transients could fold several into one backing allocation.

The seam is the separation of imports from tracked state: `import_image` builds an `RgResourceState`,
and the resource table is a plain `Vec`. A transient would be a resource the graph allocates lazily
and frees at end of frame, slotting into the same table. The right-sized targets that exist today
are the first candidates to alias.

## Volumes are dimension-agnostic

Barrier derivation reasons about a whole image and never about its dimensionality, so a 3D image is
tracked exactly like a 2D one. `import_image_3d` delegates straight to `import_image` with a `COLOR`
aspect, and the `GENERAL` ↔ `SHADER_READ_ONLY` transitions the compute usages derive apply to a
`TYPE_3D` image with no special case. On top of that, the transient pool serves 3D volumes:
`acquire_image_3d` returns a keyed, grow-only `TYPE_3D` image (backed by `Image3D`) the same way
`acquire_image` returns a 2D one, and a compute pass dispatches over it in three dimensions through
the `groups_z` argument on `add_compute_pass`. The froxel-fog grid uses all three — a per-frame
`rgba16f` frustum volume acquired transiently, written and sampled by a 3D compute grid, imported
each frame for the graph to barrier.

This is the pool side of the transient story, not graph-created aliasing (still absent, above): the
allocation is renderer-owned and imported, only now it can be a `depth > 1` volume.

## No pass culling

The graph records every pass it is given; there is no reachability analysis that drops a pass whose
outputs nothing reads. In practice this rarely matters, because the engine builds the graph
conditionally. `record_scene_graph` adds the shadow pass only when a shadow is pending and the
G-buffer only when a screen-space effect is on. The construction is pruned even though the graph
never culls.

The seam is the read and write declaration on every pass, which is exactly the information a
dead-pass cull would need. The analysis is unwritten because conditional construction already covers
the common case.

## No scheduling or reordering

Passes execute in the order they were added. The graph does not reorder them to overlap work or
minimize barriers. This keeps the per-frame state in `apply_access` a simple running summary that
reasons only about the previous touch, never about a reordered schedule.

The trade is that a good order is the author's responsibility, not the graph's. For a single-queue
frame with a handful of passes that is the right call; a large graph with many independent branches
would benefit from a scheduler.

## One subresource per barrier

`apply_access` emits barriers against the full image — a single mip and a single array layer. The
graph tracks one layout per resource, not per mip or per layer. Images with multiple mips or layers
that need different layouts at once cannot be expressed. The omnidirectional point-shadow cube, for
instance, is handled outside the graph rather than as a six-layer attachment.

The seam is the tracked state, which would grow from one layout to a per-subresource set, with
`apply_access` comparing ranges. The single-subresource assumption is baked into the barrier
construction, so this is the most invasive of the listed changes.

```mermaid
flowchart LR
    A["declared usage<br/>(RgUsage)"] --> B[derive barriers<br/>+ layout transitions]
    B --> C[record passes<br/>in order, one queue]
    C -. not done .-> D[async compute]
    C -. not done .-> E[transient alloc + aliasing]
    C -. not done .-> F[pass culling / reordering]
```

The graph is a correctness tool, not a scheduler or an allocator. It removes the error-prone,
repetitive part of Vulkan and leaves the performance-shaping parts for when they are needed, with
the data they would require already declared.

## In the code

| What | File | Symbols |
|---|---|---|
| Single-queue execution | `render_graph.rs` | `RenderGraph::execute`, `execute_profiled` |
| Import-only resources | `render_graph.rs` | `RenderGraph::import_image`, `import_buffer` |
| 3D transient volumes | `transient.rs`, `render_graph.rs` | `acquire_image_3d`, `TransientImage3D`, `import_image_3d` |
| 3D compute dispatch | `renderer.rs`, `froxel_fog.rs` | `add_compute_pass` (`groups_z`), `FROXEL_GRID_X`, `FogGridParams`, `froxel_grid_matches_shader` |
| Full-image subresource | `render_graph.rs` | `apply_access`, `RgResourceState` |
| Pass kind (the async seam) | `render_graph.rs` | `RgPass::kind`, `RgPassKind` |
| Conditional construction | `renderer.rs` | `Renderer::record_scene_graph` (the `do_*` gates) |

## Related

- [Render graph](../render-graph-overview/) — the model and its closing caveat
- [Cross-frame layouts](../cross-frame-layouts/) — why import-only is what cross-frame persistence relies on
- [Barrier derivation](../usage-and-barrier-derivation/) — the single-subresource, in-order barrier model
- [Adding passes](../who-can-add-passes/) — the app seam that already exists
