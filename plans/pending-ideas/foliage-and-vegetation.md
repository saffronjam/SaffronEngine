# Foliage & vegetation

**Status:** PENDING IDEA

> Inspiration backlog — not yet implementable as written. Needs a codebase pass (the instance-buffer
> management + per-cell cull, a brush tool over `sa.raycast` snapping, and Slang wind material nodes).

Grass, plants, and trees scattered across surfaces, with wind animation and interactive bending. The
painting/instancing core works on any mesh today; the canonical "scatter across a landscape" use wants
[heightfield-terrain](heightfield-terrain.md), and dense Nanite-style foliage wants GPU-driven culling.

## What it is

- **UE5:** Foliage Mode + Hierarchical Instanced Static Meshes (HISM); PCG for procedural placement;
  Nanite foliage for extreme density.
- **Unity:** Terrain Detail/Tree painting + the grass system.

## Core technique

A **brush raycasts the surface**, **Poisson-disk samples** positions inside the brush, snaps each to the
hit point and orients to the surface normal (with random yaw/scale jitter), and appends per-instance
transforms. Rendering is **one indexed-instanced draw per (mesh, LOD)** with per-cell frustum culling.
**Wind** is procedural vertex animation in the material (a Slang node) sampling the shared
[wind](wind.md) field, using baked per-vertex stiffness weights so trunks stay rigid and leaves flutter —
motion vectors keep TAA clean. **Interactive bending**
(grass trampled by characters) keeps a camera-following "trample" render-target updated by a decay/splat
compute pass, sampled by the grass material — the *Ghost of Tsushima* model. **Procedural placement**
(PCG) is a node graph of scatter/filter/transform rules.

## Build size

- **M** foliage painting + instancing (the brush + instance buffers).
- **M** procedural vertex wind (Slang node + baked weights, sampling the [wind](wind.md) field).
- **M** interactive grass bending (trample RT + decay compute + Jolt pushers).
- **L** SpeedTree-style LOD/impostor trees — introduces the engine's first **LOD-group** + octahedral
  **impostor** bake (reuses the offscreen thumbnail-capture path).
- **L** PCG scatter graph (editor-time) / **XL** runtime biome generation.
- **XL** Nanite-style dense foliage — gated on GPU-driven culling.

## Dependencies (do these first)

- **Instance-buffer management + per-cell cull** — the core new code; CPU-cell cull to start, GPU culling
  (a known gap) for scale.
- *Canonical use* wants **heightfield-terrain** ("scatter on landscape"); works on any mesh now.
- *Interactive bending* reuses the **trample-RT pattern** (a small persistent render-target).
- *Impostors* introduce an **LOD-group** concept the engine doesn't have yet.

## What we reuse / what's missing

**Reuse:** instancing + bindless, `sa.raycast` for brush snapping, the asset catalog, the material
node-graph (wind nodes), the offscreen thumbnail-capture (impostor bake), and the control plane (brush
strokes as commands).

**Missing:** the brush tool + instance-buffer management, an LOD-group concept, the octahedral impostor
baker, and GPU-driven culling for large counts.

## Notes & references

- *Ghost of Tsushima* GDC talk — the wind + interactive-grass-bending trample-RT model.
- UE5 Foliage Mode / HISM + PCG framework docs; Unity Terrain Details manual.
- Octahedral impostor references (e.g. Brian Karis / UE impostor baker) for tree LOD.
