# Phase B1 — transient / scratch graph resources

**Status:** IMPLEMENTED (builds + clippy-clean + unit-tested; a live write→read validation run is
pending a GPU). `saffron_rendering::TransientResources` (new `transient.rs`) is a **per-frame-in-flight,
grow-only scratch pool** for the render graph, constructed on the `Renderer` and rewound each frame in
`begin_offscreen_frame` — **after** the slot's in-flight fence wait, so a transient safely outlives the
GPU work that reads it (the use-after-free a graph-owned, teardown-freed allocation would cause). A pass
calls `acquire_buffer(frame, size, usage)` / `acquire_image(frame, desc)`; the pool reuses the
allocation parked at that acquire position when it already fits + covers the usage, else grows a new one
(same discipline as the skinning deformed ring). The returned raw handle is fed to the graph's existing
`import_buffer` / `import_image`, so **barrier derivation is unchanged** — a transient is just an
imported resource with fresh per-frame state. Design note vs. the plan's "graph-*owned*" wording: because
the graph is `#[derive(Default)]`-rebuilt every frame it *cannot* own allocations safely, so the pool
lives on the renderer (the frame-lifetime authority) and the graph imports from it — the modern-correct
fit for this architecture. **Aliasing** distinct lifetimes onto one allocation is the documented future
optimization; this is the correct portable baseline B2 builds on.
**Scope:** `saffron-rendering` (render graph)
**Depends on:** — (an engine capability gap, prerequisite for B2)

## Goal

A graph-managed **transient resource** facility: passes can declare a scratch buffer/image the graph
allocates, derives barriers for, and (optionally) aliases across non-overlapping lifetimes. This is
listed in AGENTS.md under "not yet" and is the prerequisite the compute-tessellation prepass (B2) needs
for its per-frame displaced-vertex buffer.

## Approach

Extend the render graph's resource model (which today derives barriers for declared usages on
persistent resources) to graph-created transient buffers/images: declare → allocate from a transient
pool → derive barriers → free/alias at end of lifetime. Fits the existing "each pass declares its usage,
the graph derives every barrier" model.

## Touch points

- `saffron-rendering` render-graph resource declaration + barrier derivation; a transient allocator
  (VMA-backed) with lifetime/aliasing.

## Verification

- A test pass can declare a transient buffer, write it in one pass, read it in the next, with
  graph-derived barriers and validation-clean.

## Notes

- This is a general engine feature (also enables async compute and aliasing later, per AGENTS.md). B2
  is its first consumer but it deserves its own scope. Co-develop with B2.
