+++
title = 'Mesh upload'
weight = 5
+++

# Mesh upload

Mesh upload moves a CPU-side [`Mesh`](../mesh-and-vertex-layout/) into device-local Vulkan
buffers and derives, in the same call, everything the renderer will later ask of that
geometry: draw metadata, a local-space bounding box, CPU copies for picking, and the
optional skin, morph, meshlet, BLAS, and distance-field sidecars. `Uploader::upload_mesh`
is the single entry point; the result is a shared `Arc<GpuMesh>`.

## Stage, then copy

Device-local memory is the fastest for the GPU to read, and the CPU generally cannot write
it directly. The upload follows the standard Vulkan
[staging-buffer pattern](https://vulkan-tutorial.com/Vertex_buffers/Staging_buffer): the
vertex, index, and optional skin streams are packed back-to-back into one host-visible
mapped buffer through `bytemuck::cast_slice`, and `cmd_copy_buffer` fans that buffer out
into the device-local targets.

```rust
let bytes = staging.mapped_slice();
bytes[..vb].copy_from_slice(bytemuck::cast_slice(&mesh.vertices));
bytes[vb..vb + ib].copy_from_slice(bytemuck::cast_slice(&mesh.indices));
if !skin.is_empty() {
    bytes[vb + ib..].copy_from_slice(bytemuck::cast_slice(skin));
}
staging.flush();
```

The copies record on a one-off command buffer (`with_one_off_commands`): allocated from
the uploader's transient pool, submitted once, and blocked on a fresh per-submit fence
before the staging buffer drops. The wait is on that fence alone, never a device
wait-idle, which would stall the in-flight scene frame. Upload is an import-time
operation, so the synchronous wait keeps the staging lifetime trivially correct.

A Vulkan queue is externally synchronized, so the graphics queue lives behind `GpuQueue`,
an `Arc<Mutex<vk::Queue>>`; a one-off submit holds the lock for `queue_submit2` only and
waits the fence outside it. Command pools are not thread-safe, so each `Uploader` owns its
own pool, and a second thread uploads through its own `Uploader` over a clone of the same
`GpuQueue`. The `vk::` calls are the [ash seam](../../vulkan-foundation/vulkan-hpp-no-exceptions/);
allocation goes through the [VMA allocator](../../vulkan-foundation/vma-allocator/).

An empty mesh returns `Error::EmptyMesh`, and a skin stream that does not pair one
`VertexSkin` per vertex returns `Error::SkinMismatch`. On any mid-upload failure, the
already-created buffers are freed before the error returns, so a `GpuMesh` never owns a
partial set.

## Usage flags name the consumers

Every device buffer carries `TRANSFER_DST` (it is a copy target). The remaining usage bits
map one-to-one onto downstream readers:

| Buffer | Usage | Reader |
|---|---|---|
| Vertex | `VERTEX_BUFFER \| STORAGE_BUFFER` + RT bits | vertex input; the compute deform prepasses |
| Index | `INDEX_BUFFER` + RT bits | indexed draws; the BLAS build |
| Skin | `VERTEX_BUFFER \| STORAGE_BUFFER` | the [compute-skinning](../../frame-and-render-graph/compute-skinning/) prepass |

The vertex buffer carries `STORAGE_BUFFER` on every mesh, skinned or not: the
[displacement prepass](../../frame-and-render-graph/compute-displacement/) reads the base
vertices of any mesh whose material enables displacement, and displacement is a material
choice, not a mesh property. The RT bits (`SHADER_DEVICE_ADDRESS` plus
`ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR`) appear on a ray-tracing-capable device
so the vertex and index buffers can feed the BLAS build; on other hardware they are absent
and the buffers are otherwise identical.

## What rides along

`upload_mesh` builds the optional sidecars inside the same call, so a `GpuMesh` is
complete the moment it exists:

| Sidecar | Built when | Consumer | On failure |
|---|---|---|---|
| Skin buffer | the mesh has a skin stream | compute skinning | fails the upload |
| Morph buffers | the mesh has blend shapes | the morph compute pass | fails the upload |
| BLAS | RT-capable device, ≥ 1 triangle | the per-frame TLAS build | logged; mesh renders without RT |
| Per-mesh SDFs | an `SdfBake` request | the distance-field traces | logged; mesh carries no field |
| Meshlet buffers | a `VK_EXT_mesh_shader` device | the mesh-shader raster front end | logged; mesh uses the index-draw path |

The morph buffers concatenate every target's deltas into one flat `MorphDelta` array plus
a per-target `[first_delta, delta_count]` range table, both storage buffers for the deform
dispatch (see [morph targets](../../animation/morph-targets/)). `build_mesh_blas` records
the [BLAS](../../global-illumination-and-raytracing/raytracing-foundation/) build as one
more one-off submit and drops the scratch after the fence wait.

An `SdfBake` request bakes a signed distance field per primitive (chunked when a primitive
is oversized) on the GPU at upload time, or loads it from the `assets/cache/<meshHash>.sdf`
sidecar keyed by a content hash of the mesh. Each field claims a slot in the bindless SDF
arrays, and the [software ray trace](../../global-illumination-and-raytracing/software-ray-trace/)
and cone-trace paths index it by that slot. Gizmo and preview meshes pass no request and
bake nothing.

The meshlet path clusters the mesh with `build_meshlets` and uploads three storage buffers:
the meshlet descriptors, the flat vertex indices, and the packed triangle bytes (padded to
a 4-byte multiple for byte-address reads). The draw issues one `cmd_draw_mesh_tasks` range
per submesh when
[`VK_EXT_mesh_shader`](https://www.khronos.org/blog/mesh-shading-for-vulkan) is enabled.

## What a GpuMesh holds

```rust
pub struct GpuMesh {
    // device-local vertex + index buffers, optional skin/morph/meshlet buffers
    pub index_count: u32,
    pub vertex_count: u32,
    pub submeshes: Vec<Submesh>,
    pub bounds_min: Vec3,                          // local-space AABB
    pub bounds_max: Vec3,
    pub cpu_positions: Vec<Vec3>,                  // retained for picking
    pub cpu_indices: Vec<u32>,
    pub cpu_skin: Vec<VertexSkin>,                 // empty when unskinned
    pub blas: Option<Arc<AccelerationStructure>>,  // None without RT
    pub sdfs: Vec<Arc<GpuSdf>>,                    // per-primitive distance fields
}
```

Submesh ranges are copied straight off the source `Mesh`, so the
[draw list](../draw-list/) reads them from the GPU mesh directly. The whole thing is
shared as an `Arc<GpuMesh>`: many entities reuse one upload, and `Drop` frees the VMA
allocations while the BLAS and SDF `Arc`s free themselves (each `GpuSdf` returns its
bindless slot to the shared free list). The asset server clears its caches only after
`wait_gpu_idle`, so no in-flight frame references a freed buffer.

## Bounds and CPU copies

Upload sweeps every vertex position once to find the local-space AABB and keeps the
positions, indices, and skin stream resident on the CPU. Both consumers then work without
re-reading GPU memory:

- [Picking](../../scene-and-ecs/picking/) slab-tests the world-space box as a broad phase,
  then descends a cached per-mesh BVH built from the CPU copies (`mesh_pick_bvh`). A
  skinned mesh is CPU-skinned through the joint palette first, so the pick agrees with the
  deformed surface on screen.
- `render_scene` unions each entity's world-space box into a scene AABB and fits the
  [directional shadow](../../shadows-and-culling/directional-shadows/) frustum to its
  bounding sphere. A skinned entity contributes its bind-pose box swept through every
  joint, a conservative fit.

## In the code

| What | File | Symbols |
|---|---|---|
| The upload | `rendering/src/upload.rs` | `Uploader::upload_mesh`, `with_one_off_commands` |
| Staging + device buffers | `rendering/src/upload.rs` | `StagingBuffer`, `make_device_buffer` |
| Shared queue | `rendering/src/upload.rs` | `GpuQueue` |
| Sidecar builds | `rendering/src/upload.rs` | `build_mesh_blas`, `upload_morph_buffers`, `upload_meshlet_buffers`, `upload_sdf` |
| GPU mesh type | `rendering/src/resources.rs` | `GpuMesh`, `GpuMeshParts`, `MorphBuffers`, `MeshletBuffers` |
| Upload seam | `assets/src/gpu.rs` | `GpuUploader`, `RendererUploader` |
| Bounds consumers | `assets/src/render_scene.rs` | `render_scene`, `pick_scene_surface`, `scene_render_aabb` |
| Pick BVH cache | `assets/src/load.rs` | `AssetServer::mesh_pick_bvh` |

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the `Mesh` this consumes
- [Draw list](../draw-list/) — how a `GpuMesh` and its submeshes are drawn
- [Picking](../../scene-and-ecs/picking/) — the AABB and CPU copies at work
- [Directional shadows](../../shadows-and-culling/directional-shadows/) — fits to the scene AABB
- [Compute skinning](../../frame-and-render-graph/compute-skinning/) — reads the vertex and skin streams as storage
- [Raytracing foundation](../../global-illumination-and-raytracing/raytracing-foundation/) — where the BLAS goes
- [Software ray trace](../../global-illumination-and-raytracing/software-ray-trace/) — marches the per-mesh distance fields
