+++
title = 'Executor draws'
weight = 8
math = true
+++

# Executor draws

Every geometry pass draws from the [persistent GPU scene](../../frame-and-render-graph/persistent-gpu-scene/):
the visibility traversal emits a per-view stream of semantic draw records on the GPU, binning
kernels scatter them into indirect-command slices, and each pass replays those commands with
counted indirect draws. The CPU never builds a per-frame list of things to draw — render
preparation scales with scene *changes*, not with visible-instance count.

A frame draws the same geometry several times: shaded color, depth, shadows, G-buffer, motion
vectors. All of them consume the one record stream the traversal produced for the view.

## Records: the semantic draw vocabulary

The [visibility chain](../../frame-and-render-graph/hierarchical-visibility/) culls instances
against the previous frame's HZB, then traverses each survivor's resident page hierarchy by
projected appearance error. Every emitted `GpuDrawRecord` names one drawable cut:

```text
(geometry, instance, part, representation, material, deformation, psoBin, shaderIndex)
```

The record carries everything a draw needs to reconstruct itself on the GPU: the instance's
transform (via the instance table), the material's parameter block and textures (via the
material table), and the cluster or voxel-surface index range (via the resident page).

## Buckets: records into indirect commands

The CPU enumerates the frame's *draw buckets* — the live `(shaderIndex, psoBin)` combos the
[GPU-scene mirror](../../frame-and-render-graph/persistent-gpu-scene/) reports — and
partitions the command buffer into equal per-bucket slices (`build_executor_buckets`,
`SceneBucketTable`). The binning kernels count each record's bucket, seed per-bucket cursors,
and scatter one `VkDrawIndexedIndirectCommand` per record into its bucket's slice. A record's
command indexes the global pages arena (the executor's index buffer) with `firstInstance`
carrying the record index.

Each pass then issues one `vkCmdDrawIndexedIndirectCount` per bucket over its slice, with the
per-bucket count buffer supplying the draw count. The scene pass binds one mesh PSO per
bucket (the bucket's material class + registered shader decode to the PSO request). The depth
family (depth prepass, the virtual-shadow pages, G-buffer, motion) uses one
vertex-only executor PSO per pass for every opaque/masked bucket.

## Vertex pulling

The übershader's `vertexMainExecutor` entry runs with no vertex input state: `SV_VertexID` is
the pulled index value from the pages arena, `SV_VulkanInstanceID` is the record index. The
vertex loads its record, resolves the instance transform and material through the
[address block](../../frame-and-render-graph/persistent-gpu-scene/), and pulls positions
through buffer device addresses — the static vertex arena for a rigid instance, the
per-frame deformed buffer for a [skinned or morphing](../../frame-and-render-graph/compute-skinning/)
one. The emitted interface matches the instanced vertex path exactly, so every fragment
shades identically.

```mermaid
flowchart TD
    A[cull — previous-HZB occlusion] --> B[traversal — GpuDrawRecord stream]
    B --> C[binning — per-bucket indirect commands]
    C --> D[scene — per-bucket mesh PSOs]
    C --> E[depth family — one PSO per pass]
    B --> F[transparent sort — keys, radix, reorder]
    F --> G[translucent scope — per-blend-bucket slices]
```

## Transparency

Alpha-blended records sort on the GPU: a keys kernel collects `(flipped view-depth, record)`
pairs, a stable 4-pass LSD radix sort orders them, and a reorder kernel writes one
full-length back-to-front command slice per live blend bucket — a pair belonging to another
bucket masks to a zero draw, so each blend PSO replays the whole global order. The scene's
translucent scope draws each slice with its bucket's blend PSO (depth-test on, depth-write
off), counted by the transparent counter word.

## Deformation and the tessellation seam

The scene driver walks the ECS once per frame for *frame facts* only: the world AABB (the
shadow-frustum fit), the SDF occluder list, the RT instance inputs, and one
`DeformationWork` item per skinned, morphing, or displaced instance. It submits the work and
the concatenated joint palette through `Renderer::submit_gpu_scene_deformations`, which wires
the skin/morph compute dispatches; the deformed outputs are pulled by the executor vertex
path.

A displaced (`HeightMode::Displacement`) instance draws through the *tessellation seam*: its
material carries `GPU_MATERIAL_TABLE_FLAG_TESSELLATED`, the traversal skips its records, and
every pass replays a per-instance `TessSceneDraw` — an indirect draw over the frame's
amplified transient geometry, the renderer's only vertex-input path (see
[displacement](../../frame-and-render-graph/compute-displacement/)).

## Stats

The frame's counters derive from the visibility readback: `drawCalls` is the emitted record
count (plus tess-seam draws), `instances` the cull survivors, `triangles` the traversal's
per-record index counts over three, `batches` the live bucket count. The control plane
exposes them, and `instanceUploadBytes` reports the GPU-scene table bytes staged this frame —
(near-)zero on a steady scene:

```sh
sa render-stats
# { "drawCalls": 12, "batches": 2, "instances": 2, "triangles": 24, "instanceUploadBytes": 0, ... }
```

## In the code

| What | File | Symbols |
|---|---|---|
| Frame facts + deformation work | `assets/src/render_scene.rs` | `render_scene`, `gather_static_frame_facts`, `gather_skinned_frame_facts` |
| Buckets + visibility lists | `rendering/src/visibility.rs` | `build_executor_buckets`, `ExecutorBucket`, `SceneVisibilityView`, `ExecutorDrawInputs` |
| Pass recorders | `rendering/src/scene_pass.rs` | `record_executor_buckets`, `record_executor_depth_family`, `record_executor_transparent_stream` |
| Tess-seam draws | `rendering/src/scene_pass.rs`; `rendering/src/draw_list.rs` | `record_tess_scene_draws`, `record_tess_depth_draws`, `TessSceneDraw` |
| Deformation driver | `rendering/src/renderer.rs`; `rendering/src/instancing.rs` | `submit_gpu_scene_deformations`, `DeformationWork`, `gather_instance_deformation` |
| Executor vertex path | `assets/shaders/mesh.slang` | `vertexMainExecutor` |
| Binning + sort kernels | `assets/shaders/` | `scene_bin_count/seed/scatter.slang`, `scene_transparent_keys.slang`, `scene_transparent_reorder.slang` |

> [!NOTE]
> The shader and the unlit flag follow `MaterialSet` slot 0 for the whole mesh, so one mesh
> cannot mix lit and unlit submeshes. Blend mode, textures, and every PBR factor vary freely
> per submesh through the material table.

## Related

- [Persistent GPU scene](../../frame-and-render-graph/persistent-gpu-scene/) — the tables the records resolve through
- [Hierarchical visibility](../../frame-and-render-graph/hierarchical-visibility/) — the cull + traversal that emits the records
- [Bindless textures](../../materials-and-pipelines/bindless-textures/) — why textures never split a bucket
- [Materials & PSOs](../../materials-and-pipelines/material-and-pso-selection/) — the bucket → PSO resolution
- [Compute skinning](../../frame-and-render-graph/compute-skinning/) — how a deforming instance is drawn
- [Render graph](../../frame-and-render-graph/render-graph-overview/) — the passes that replay the commands
- [Render commands](../../tooling-and-control/render-commands/) — reading the draw stats live
