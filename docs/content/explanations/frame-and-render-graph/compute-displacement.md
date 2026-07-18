+++
title = 'Compute displacement'
weight = 11
+++

# Compute displacement

Compute displacement turns a material height field into generated micro-geometry. The renderer dices conditioned base triangles, displaces the resulting vertices, and writes per-frame vertex and index buffers. Depth, shadow, G-buffer, motion, and scene passes then draw those buffers through the normal mesh pipelines.

This path changes the silhouette and supplies real triangles to ray queries. Bump mapping changes only the shading normal, while parallax mapping offsets texture coordinates on an unchanged surface.

## Frame flow

```mermaid
flowchart LR
  mesh[conditioned base mesh] --> factor[tess-factor]
  pyramid[height min/max pyramid] --> factor
  factor --> scan[tess-scan]
  scan --> finalize[tess-finalize]
  scan --> emit[tess-emit]
  finalize --> args[indirect draw command]
  emit --> raster[depth, shadows, G-buffer, scene]
  emit --> motion[motion vectors]
  factor --> coarse[coarse RT factor, scan, emit]
  coarse --> blas[per-frame tessellated BLAS]
```

`Instancing::submit_draw_list` creates one `TessBucket` for each displacement-enabled instance whose `GpuMesh` carries conditioning data. The renderer accepts at most `TESS_MAX_INSTANCES = 64` buckets per frame. A bucket contains the base mesh, object transform, texture slots, UV transform, height scale, and tessellation quality controls.

The primary chain contains five compute passes:

1. `tess-factor` writes one fractional factor per unique base edge.
2. `tess-scan` calculates each base triangle's exact output counts and atomically assigns packed offsets.
3. `tess-finalize` writes one `VkDrawIndexedIndirectCommand` per displaced instance.
4. `tess-args` converts the global micro-vertex total into a `VkDispatchIndirectCommand`.
5. `tess-emit` dispatches one 64-lane workgroup per base triangle and writes current vertices, previous vertices, and indices.

The renderer submits `tess-emit` with direct dispatches. The command written by `tess-args` has no consumer in this chain.

## Adaptive factors and budget

The factor kernel measures the angular screen extent of an edge from its two welded endpoints. If either endpoint crosses the near-plane guard, it uses a finite length-over-distance fallback. Both forms divide the projected extent by the target pixels per micro-edge.

Displacement detail also affects the factor. Each displacement height texture owns an `R32G32_SFLOAT` min/max pyramid at bindless binding 4. The kernel chooses a mip from the edge's tiled UV span, reads a conservative local height range, projects `height_range * height_scale`, and uses the larger of base-edge and displacement extents.

The calculation uses mesh-local endpoints and a camera position transformed into local space. It is exact for uniform object scale; non-uniform scale distorts the angular metric.

The default quality tuple is:

```text
factor cap:        256
minimum factor:     1
micro-edge target:  4 px
```

`budget_scaled_caps` reduces per-instance caps when their combined worst-case reservation exceeds `TESS_MICRO_VERTEX_BUDGET`, which is 2,097,152 vertices. It scales caps approximately with the square root of the available fraction and never lowers one below its minimum factor.

## Split, dice, and emit

A leaf grid is limited to `TESS_MAX_DICE_FACTOR = 11`, which yields at most 121 triangles and 78 vertices. A driving factor above 11 recursively splits the base triangle into four child patches per level. Each child is then diced on an integer barycentric grid.

For a factor cap of 256, `dice_plan` returns 1,024 subpatches at leaf level 8. `tess_worst_case` uses that same plan to reserve the transient slices, while `tess_scan.slang` reproduces it to compute packed output counts.

The emit kernel evaluates a [Phong-tessellated](https://perso.telecom-paristech.fr/boubek/papers/PhongTessellation/) base surface, samples the height map at a density-matched mip, and derives normal and tangent from finite differences on the displaced surface. Scalar displacement moves along the interpolated normal. A vector displacement map applies tangent-space XYZ offsets.

## Watertight edges and geomorphing

Mesh import welds vertices and writes unique edge IDs plus per-triangle edge references into the `.smesh` conditioning data. Adjacent base triangles read the same factor slot. Boundary micro-vertices snap to a segment grid derived only from that shared edge and factor.

The fractional factor also drives a geomorph. Boundary points blend between floor and ceiling segment placements. Interior points blend from their containing coarse-grid triangle to the fine displaced position with `smoothstep01`. Successive integer barycentric grids are distinct, so interior continuity is bounded by the coarse facet approximation rather than bit-exact nesting.

## Raster and motion consumers

`tess-finalize` seeds an indirect indexed draw whose `firstInstance` points at the instance's material rows. `record_batch_submeshes` takes the tessellated branch and issues one `cmd_draw_indexed_indirect`. `bind_batch_vertices` binds the amplified vertex and index buffers for every raster consumer.

The emit kernel also writes `tess.vb.prev`. It evaluates the current grid with the previous frame's per-edge factors, stored in a persistent two-slot ping-pong. A changed edge-count layout falls back to current factors so previous and current positions match. `record_motion` binds current and previous tessellated streams separately, which captures camera-driven geomorph motion as well as object motion.

## Ray-tracing geometry

An RT-consumed displaced instance runs a second factor, scan, and emit chain. `TESS_RT_COARSEN = 2` doubles the micro-edge target and divides the factor cap by two. The result remains displaced and edge-welded, but it contains fewer triangles than the raster surface.

`Rt::plan_tessellated_blas_builds` performs a full `MODE_BUILD` each frame because the generated topology can change. The acceleration structure is sized for the coarse chain's worst case and reused while that bound stays unchanged. The index reservation is cleared before emission, so unused tail triangles are degenerate and can remain in the worst-case build range without a GPU count readback.

## Synchronization

The primary emit pass declares storage writes for its transient buffers. Raster passes bind those buffers without importing them as graph resources, so `record_tess_prep` emits a manual compute-to-vertex/index/indirect memory barrier at the end of `tess-emit`.

The coarse RT vertex and index buffers do enter the graph. `tess-emit-rt` declares `StorageWriteCompute`, and `tlas-build` declares `AccelStructBuildRead`; the graph derives that compute-to-acceleration-structure dependency.

## Control

`sa set-displacement {0|1}` toggles the compute path. `sa set-tessellation-quality` accepts optional factor cap, minimum factor, and edge-length target values. The renderer rounds and clamps the cap to `1..=2048`, clamps the minimum to the cap, and floors the edge target at one pixel.

```sh
sa set-tessellation-quality 128 1 6
```

This selects a 128 factor cap, a minimum factor of 1, and a six-pixel micro-edge target.

## In code

| What | File | Symbols |
|---|---|---|
| Tessellation model and budgets | `engine/crates/rendering/src/tessellation.rs` | `TessBucket`, `dice_plan`, `tess_worst_case`, `budget_scaled_caps`, `TESS_MICRO_VERTEX_BUDGET` |
| Factor kernel | `engine/assets/shaders/tess_factor.slang` | `localHeightRange`, `computeMain` |
| Count and offset pass | `engine/assets/shaders/tess_scan.slang` | `dicePlan`, `computeMain` |
| Draw command | `engine/assets/shaders/tess_finalize.slang` | `computeMain` |
| Amplifying kernel | `engine/assets/shaders/tessellate.slang` | `emitLeafVertex`, `emitLeafTriangle`, `computeMain` |
| Pass recording | `engine/crates/rendering/src/renderer.rs` | `Renderer::record_tess_prep` |
| Raster and motion draws | `engine/crates/rendering/src/scene_pass.rs`, `engine/crates/rendering/src/aa.rs` | `record_batch_submeshes`, `bind_batch_vertices`, `record_motion` |
| Ray-tracing build | `engine/crates/rendering/src/rt.rs` | `TessellatedBlas`, `Rt::plan_tessellated_blas_builds` |
| Material feature | `engine/crates/rendering/src/instancing.rs` | `FEATURE_DISPLACE`, `resolve_material` |
| Control commands | `engine/crates/control/src/commands_render.rs` | `set-displacement`, `set-tessellation-quality` |

## Related

- [Compute skinning](../compute-skinning/)
- [Barrier derivation](../usage-and-barrier-derivation/)
- [The `.smesh` format](../../geometry-and-assets/smesh-format/)
- [Native materials](../../materials-and-pipelines/native-materials/)
