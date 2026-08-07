+++
title = 'Vertex layout'
weight = 1
+++

# Vertex layout

A vertex layout is the fixed memory format of one mesh vertex: which attributes it carries, in
what order, and at what total stride. Anima has one CPU-side mesh type, `Mesh`, and one 48-byte
vertex struct shared by every importer.

A single fixed layout lets one mesh pipeline, one `.smesh` on-disk stride, and one upload path
serve [glTF and OBJ](../gltf-and-obj-import/) alike. The bytes are the same in memory, on disk,
and in the GPU vertex arena the executor vertex path pulls from.

## One 48-byte vertex

A vertex is position, normal, one UV channel, and a UV-aligned tangent:

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct Vertex {
    pub position: Vec3,     // glam's 12-byte Vec3, never the 16-byte Vec3A
    pub normal: Vec3,
    pub uv0: Vec2,
    pub tangent: [f32; 4],  // xyz object-space tangent, w the ±1 bitangent handedness
}
```

Position and normal are [glam](https://docs.rs/glam)'s 12-byte `Vec3`, never the 16-byte SIMD
`Vec3A`, so the struct packs to exactly 48 bytes. The size is pinned at compile time:
`saffron-geometry`'s `lib.rs` carries `const _: () = assert!(size_of::<Vertex>() == 48, …)`,
with sibling asserts pinning `Submesh` to 16 bytes and `VertexSkin` to 24. A stray `Vec3A` or a
glam bump that changes a layout fails the build, not a torn-mesh runtime.

The `#[repr(C)]` plus the `Pod`/`Zeroable` derives (from
[bytemuck](https://docs.rs/bytemuck)) let the [`.smesh` format](../smesh-format/) write the
vertex array as one raw `bytemuck::cast_slice` blob and read it straight back, all under the
crate's `#![deny(unsafe_code)]`. The `.smesh` header records the stride, and the loader rejects
a file whose `vertex_stride` differs from `size_of::<Vertex>()` with `Error::BadLayout`, so a
layout change makes stale bakes fail loudly instead of decoding garbage.

Normals come from the source asset. An OBJ that ships none gets smooth normals rebuilt from
accumulated triangle face normals (`generate_normals`) before the tangent pass runs.

## The tangent

The tangent is UV-aligned: `xyz` points along +U on the surface, and `w` stores the ±1
bitangent handedness so a shader rebuilds the bitangent as `w · cross(normal, tangent)` — the
convention of the [glTF 2.0 specification](https://github.com/KhronosGroup/glTF/tree/main/specification/2.0).

The importers compute it with [Lengyel's method](https://terathon.com/blog/tangent-space.html)
in `compute_tangents`: accumulate each triangle's UV-gradient tangent into its vertices, then
Gram-Schmidt-orthonormalize against the normal. The glTF importer keeps a provided `TANGENT`
accessor and computes only when the asset omits one. A vertex with no usable UV gradient falls
back to a stable basis built from the normal, so every tangent stays finite and unit-length.

The field is a raw `[f32; 4]` rather than glam's `Vec4`, which is 16-byte SIMD-aligned and
would pad the struct past 48 bytes. Storing the tangent gives every consumer a true UV-aligned
TBN for normal maps and tangent-space vector displacement, instead of a
derivative-reconstructed frame. The skin, morph, and displace compute kernels carry it through
into the deformed buffer; skinning rotates the `xyz` and keeps `w`, since the handedness sign
is invariant under rotation.

## Mesh and submeshes

A `Mesh` is three flat vectors: one shared vertex buffer, one shared index buffer, and a list
of `Submesh` ranges over them.

```rust
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<Submesh>,
}
```

A `Submesh` is one `vkCmdDrawIndexed` call's worth of arguments — 16 bytes, baked directly
into a `.smesh`:

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct Submesh {
    pub first_index: u32,
    pub index_count: u32,
    pub vertex_offset: i32,   // signed, matching vkCmdDrawIndexed
    pub material_slot: u32,   // index into the model's material table
}
```

`vertex_offset` is signed because that is the type `vkCmdDrawIndexed` takes. The glTF importer
sets it to each primitive's base vertex, so a primitive's indices stay zero-based against its
own block of the shared array; the OBJ importer dedups every face corner into one array up
front and leaves it at 0. Indices are 32-bit throughout, and the `.smesh` loader rejects any
file whose `index_width` is not 4.

## Parallel streams

An attribute only some meshes need rides a parallel stream instead of widening `Vertex`:

- **`VertexSkin`** — 24 bytes per vertex: `[u16; 4]` joint indices plus `[f32; 4]` blend
  weights, one entry per vertex, empty for an unskinned mesh. The weights are a raw `[f32; 4]`
  for the same reason the tangent is: glam's `Vec4` would pad the stride.
- **`MorphDelta`** — 28 bytes: a sparse blend-shape record of one vertex index plus the
  position and normal deltas applied at weight 1.0. Only moved vertices are stored; the morph
  kernel copies the base tangent through unchanged.

An unskinned mesh with no blend shapes pays nothing for either stream.

## Submeshes at draw time

A submesh lets one logical model carry several draw ranges over one shared vertex/index pair.
A model with three glTF primitives is three draw ranges, not three meshes; the same
`submeshes` table rides `GpuMesh` after upload.

Each submesh selects its material through `material_slot`, an index into the entity's
[`MaterialSet`](../../scene-and-ecs/built-in-components/) slots. `resolve_entity_materials`
clamps the index to the last slot, so a single-slot set covers every submesh of a
single-material mesh. An emitted [executor draw record](../draw-list/) resolves its material
through the same slot.

## In the code

| What | File | Symbols |
|---|---|---|
| Vertex, mesh, submesh, stream types | `geometry/src/types.rs` | `Vertex`, `Mesh`, `Submesh`, `VertexSkin`, `MorphDelta` |
| Compile-time stride pins | `geometry/src/lib.rs` | the `const _` size asserts |
| Tangent generation | `geometry/src/types.rs` | `compute_tangents` |
| Normal regeneration | `geometry/src/picking.rs` | `generate_normals` |
| Disk round-trip | `geometry/src/smesh.rs` | `save_mesh_to_buffer`, `load_mesh_from_bytes` |
| GPU side | `rendering/src/resources.rs` | `GpuMesh` |
| Executor pass recording | `rendering/src/scene_pass.rs` | `record_executor_buckets`, `record_executor_depth_family` |
| Slot → material resolve | `assets/src/render_material.rs` | `resolve_entity_materials` |

## Related

- [Model import](../gltf-and-obj-import/) — what fills these vectors
- [.smesh format](../smesh-format/) — the byte image that pins these strides
- [Mesh upload](../gpu-mesh-upload/) — `Mesh` → `GpuMesh`
- [Executor draws](../draw-list/) — how submeshes become draws
- [Built-in components](../../scene-and-ecs/built-in-components/) — the `MaterialSet` the slots index
