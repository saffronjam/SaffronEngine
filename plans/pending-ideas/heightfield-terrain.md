# Heightfield terrain

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (a 16-bit height asset
> type, a quadtree/clipmap LOD selector feeding instanced patch draws, and the Jolt heightfield shape).

The second major **enabler** (alongside the GPU particle system). It unlocks a whole cluster: terrain
collision, splat materials, sculpting, foliage scatter, water shorelines, and "vehicles on landscape."
Jolt's `HeightFieldShape` makes the collision nearly free; the LOD-morphing mesh is the real cost.

## What it is

A large, editable ground surface sampled from a heightmap, with multi-layer materials, sculpt/paint
tools, holes, and physics collision.

- **UE5:** Landscape (heightmap + layer weightmaps + LOD) and Virtual Heightfield Mesh for extreme scale.
- **Unity:** the Terrain system (heightmap + splatmaps + detail/tree layers).

## Core technique

The mesh is a uniform grid that samples a **16-bit height texture in the vertex shader**; a
**quadtree or geo-clipmap** selects per-patch resolution by distance, with **continuous LOD morphing**
between mip levels to hide popping and **seam-stitching** between neighboring LODs. Normals are generated
on the GPU from the height texture. Materials blend N layers via a **splat/weight map** (per-texel layer
weights) — naturally an extension of the material node graph. **Holes** are a per-texel mask that discards
fragments and removes collision. **Sculpting** is compute brush kernels writing the height texture;
runtime deformation (craters, footprints) writes the same texture, with optional collision write-back.

## Build size

- **L** terrain core — the LOD morph + seam-stitching is where the cost lives.
- **S–M** Jolt heightfield collision (nearly free via `HeightFieldShape`) — land it alongside the core so
  terrain is immediately walkable.
- **M** layer/splat materials (an N-layer blend node in the graph).
- **M** GPU sculpt brushes (compute kernels + control-plane stroke commands).
- **S** holes; **M** runtime deformation (visual) / **L** (with collision write-back).
- **L** edit layers + spline brushes (roads/rivers carving).
- **XL** Virtual Heightfield Mesh — defer until GPU-driven culling + transient resources land.

## Dependencies (do these first)

- A **quadtree/clipmap LOD selector** + a **16-bit height asset type** — the genuinely new pieces.
- *Scales better with* GPU-driven culling / MDI (a known gap) — works via plain instancing to start.
- *Sculpting* reuses the vertex-paint tooling also wanted by cloth and foliage.

## What we reuse / what's missing

**Reuse:** bindless + instancing (patch draws), the übershader/PSO cache, the material node-graph (splat
blend), the JSON project format, Jolt (`HeightFieldShape` collision), and the control plane (sculpt
strokes as commands → scriptable from `sa`).

**Missing:** the quadtree/clipmap LOD selector, a 16-bit height asset type, and a splat/weight-map editor
brush.

## Unlocks (downstream pending ideas)

[`foliage-veg`](../foliage-veg/README.md) (as a shared `SurfaceField` provider, never a separate
terrain-grass path), `water-and-ocean` (shorelines, rivers
carving), and "vehicles on terrain" (vehicles themselves need no terrain). Terrain and vegetation
share the spatial foundation; neither is an implementation prerequisite for the other.

## Notes & references

- Losasso & Hoppe, "Geometry Clipmaps" — the clipmap LOD approach.
- CDLOD (Continuous Distance-Dependent LOD) — the morph-between-mips technique.
- Jolt `HeightFieldShape` docs — for the cheap collision.
- UE5 Landscape + Virtual Heightfield Mesh docs; Unity Terrain manual — for the layer/splat model.
