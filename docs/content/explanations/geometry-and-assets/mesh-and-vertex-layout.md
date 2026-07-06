+++
title = 'Vertex layout'
weight = 1
+++

# Vertex layout

A vertex layout is the fixed memory format of a single mesh vertex: which attributes it
carries, in what order, and at what total stride. Anima uses one CPU-side mesh type,
`Mesh`, and one 48-byte vertex struct for every importer.

A single fixed layout lets one mesh pipeline, one `.smesh` on-disk stride, and one upload
path serve glTF and OBJ alike. The format is the same in memory, on disk, and on the GPU.

## The vertex

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

The size is pinned at compile time. `saffron-geometry`'s `lib.rs` carries a
`const _: () = assert!(size_of::<Vertex>() == 48, …)`, so a stray `Vec3A` or a glam bump
that changed a layout fails the build, not at a torn-mesh runtime. The
[`.smesh` format](../smesh-format/) writes the vertex array as one raw `bytemuck::cast_slice`
blob and the loader reads it straight back, so the in-memory stride is the disk stride.
Adding a member without bumping the format version would misalign every baked mesh on disk.

The **tangent** is UV-aligned (Lengyel's method, computed on import — or the glTF `TANGENT`
accessor when present), with the ±1 bitangent handedness in `w` (glTF convention:
`bitangent = w · cross(normal, tangent)`). It is a raw `[f32; 4]` rather than glam's
SIMD-aligned `Vec4` so the struct stays tightly packed at 48 bytes. It lets a UV-space vector
field — a normal map's frame, or tangent-space vector displacement — be transformed by a true
TBN instead of a derivative-reconstructed one, and the skin/morph/displace compute kernels
carry (and skinning rotates) it into the deformed buffer. The `#[repr(C)]` Pod derive (the
`Pod`/`Zeroable` from `bytemuck`) is what lets the byte codec reinterpret the array safely
under the crate's `#![deny(unsafe_code)]`.

## Mesh and submeshes

A `Mesh` is three flat vectors: one shared vertex buffer, one shared index buffer, and a
list of `Submesh` ranges over them.

```rust
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<Submesh>,
}
```

A `Submesh` is one `drawIndexed` call's worth of arguments:

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

`vertex_offset` is signed because that is the type `vkCmdDrawIndexed` takes. The glTF
importer sets it per primitive so each primitive's indices stay zero-based against its own
vertex block; the OBJ importer leaves it at 0 and emits indices already relative to the
shared array. Indices are 32-bit throughout, and the loader rejects any file whose
`index_width` is not 4.

A parallel `VertexSkin` stream (24 bytes — `[u16; 4]` joints plus `[f32; 4]` weights, the
raw array rather than glam's SIMD-aligned `Vec4` so the stride stays fixed) rides alongside
the vertices for a skinned mesh, and is empty for an unskinned one.

## Why submeshes

A submesh is one `drawIndexed` call's worth of arguments over the mesh's shared buffers, so
one logical model can carry several draw ranges. The draw path loops every batch's
submeshes and issues one `drawIndexed` per submesh. A model with three glTF primitives
becomes three draw ranges against one bound buffer pair.

Each submesh selects a material through `material_slot`, an index into the entity's
[`MaterialSet`](../../scene-and-ecs/built-in-components/). A single-material mesh keeps every
submesh at slot 0 (one slot); a multi-material import gets one slot per material, and the
[draw list](../draw-list/) indexes the slots by `material_slot` so each submesh draws with its
own material.

## In the code

| What | File | Symbols |
|---|---|---|
| Vertex + stride assert | `geometry/src/types.rs`; `geometry/src/lib.rs` | `Vertex` |
| Mesh + submesh | `geometry/src/types.rs` | `Mesh`, `Submesh` |
| Skin stream | `geometry/src/types.rs` | `VertexSkin` |
| Normal regeneration | `geometry/src/picking.rs` | `generate_normals` |
| GPU side | `rendering/src/resources.rs` | `GpuMesh` |
| Per-submesh draw loop | `rendering/src/scene_pass.rs` | `record_batch_submeshes` |

## Related

- [Model import](../gltf-and-obj-import/) — what fills these vectors
- [.smesh format](../smesh-format/) — why the stride asserts matter
- [Mesh upload](../gpu-mesh-upload/) — `Mesh` → `GpuMesh`
- [Built-in components](../../scene-and-ecs/built-in-components/) — where material actually lives
