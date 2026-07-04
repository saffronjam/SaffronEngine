# Phase B1 — transient / scratch graph resources

**Status:** NOT STARTED
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
