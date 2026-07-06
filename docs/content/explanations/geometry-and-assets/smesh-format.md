+++
title = '.smesh format'
weight = 3
+++

# .smesh format

`.smesh` is a baked binary mesh format: a 64-byte header followed by raw arrays of
vertices, indices, and submeshes, with an optional skin section appended. A model is baked
once on import, and every later load reads the `.smesh` image directly.

Importing a glTF or OBJ means parsing JSON or text, de-duplicating vertices, and possibly
regenerating normals. The baked format moves that cost off the runtime path: a load casts
the arrays straight into pre-sized slices with no per-element decode.

## Layout

A fixed 64-byte header, then vertices, indices, and submeshes contiguously at the offsets
the header records:

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
struct SMeshHeader {
    magic: [u8; 4],        // b"SMSH"
    version: u32,          // 4
    flags: u32,            // MESH_FLAG_SKIN | MESH_FLAG_MORPH bits
    vertex_stride: u32,    // == size_of::<Vertex>() (48)
    vertex_count: u32,
    index_count: u32,
    index_width: u32,      // bytes per index (4)
    submesh_count: u32,
    vertices_offset: u64,
    indices_offset: u64,
    submeshes_offset: u64,
    morph_offset: u64,     // start of the morph section (0 when no MORPH flag)
}
const _: () = assert!(size_of::<SMeshHeader>() == 64, "SMeshHeader must be exactly 64 bytes");
```

The arrays are written with `bytemuck::cast_slice`, no per-element serialization. `Vertex`
and `Submesh` have [compile-time-pinned sizes](../mesh-and-vertex-layout/) (48 and 16 bytes — the
`Vertex` carries a UV-aligned tangent for normal maps + vector displacement),
so the in-memory layout is the on-disk layout. The header records `vertex_stride` and
`index_width` so the loader can reject a file written by an incompatible build. There is **one**
version — `MESH_FORMAT_VERSION` = 4 — and two `flags` bits select the optional sections: `MESH_FLAG_SKIN`
appends a `VertexSkin` section, `MESH_FLAG_MORPH` appends a morph section (a `MorphSectionHeader`, then
per-target `MorphTargetDesc` ranges, then the flat `MorphDelta` array) at `morph_offset`. The encoder
sets each bit from whether the skin / morph stream is non-empty; the loader accepts version 4 and rejects
any other. One write path — `save_mesh_to_buffer(mesh, skin, morph)` — covers every combination; there is
no separate skinned encoder.

## Why a custom binary format

The format trades import cost for load cost. The runtime path never re-parses glTF or OBJ;
it casts three contiguous spans into slices and copies them into `Vec`s. The format is
versioned so a layout change is detected rather than silently misread, and self-describing
enough to validate without trusting the producer — which matters because the payload is read
back as raw memory through `bytemuck` under the crate's `#![deny(unsafe_code)]`.

The image is the canonical triple contract: the disk bytes equal the in-memory payload equal
the GPU vertex buffer, and the section offsets are self-relative. So a `.smesh` embedded as a
[`.smodel`](../smodel-container/) `MESH` chunk slice reads identically to a standalone file —
both go through the same `load_mesh_from_bytes`.

## Loading defensively

`load_mesh_from_bytes` does not trust the header. Before slicing it recomputes the expected
layout from the counts and requires the header's offsets to match and the span to be that
long:

```rust
let vertices_end = size_of::<SMeshHeader>() as u64 + u64::from(header.vertex_count) * size_of::<Vertex>() as u64;
let indices_end  = vertices_end + u64::from(header.index_count) * size_of::<u32>() as u64;
let submeshes_end = indices_end + u64::from(header.submesh_count) * size_of::<Submesh>() as u64;
if header.vertices_offset != size_of::<SMeshHeader>() as u64
    || header.indices_offset != vertices_end
    || header.submeshes_offset != indices_end
    || (bytes.len() as u64) < submeshes_end { return Err(Error::BadLayout); }
```

The checks run in order: span at least header-sized (`Error::Truncated`), magic `SMSH`
(`Error::BadMagic`), version 4, stride and index-width match, then the
layout-consistency block. A malformed huge `vertex_count` would otherwise drive a giant
allocation; rejecting the file as inconsistent or truncated first keeps a corrupt file from
exhausting memory. The span length is the chunk length, not a file size, so an embedded
`.smodel` chunk validates the same way.

## Round-trip coverage

The codec is covered by unit tests in `smesh.rs`: a baked mesh reads back with the same
counts and the first vertex position byte-for-byte (with skin and morph round-tripping through
`save_mesh_to_buffer` / `load_mesh_morph_from_bytes`), a bad magic / bad version / truncated
span each return the expected `Err`, and the format strides stay pinned by the compile-time
`size_of` asserts in `geometry/src/lib.rs`.

## In the code

| What | File | Symbols |
|---|---|---|
| Header layout | `geometry/src/smesh.rs` | `SMeshHeader` |
| Version + flags | `geometry/src/smesh.rs` | `MESH_FORMAT_VERSION`, `MESH_FLAG_SKIN`, `MESH_FLAG_MORPH` |
| Morph section | `geometry/src/smesh.rs` | `MorphSectionHeader`, `MorphTargetDesc` |
| Write path | `geometry/src/smesh.rs` | `save_mesh`, `save_mesh_to_buffer` |
| Defensive load | `geometry/src/smesh.rs` | `load_mesh`, `load_mesh_from_bytes`, `load_mesh_skin_from_bytes`, `load_mesh_morph_from_bytes` |
| Header-only counts | `geometry/src/smesh.rs` | `mesh_counts_from_bytes`, `mesh_file_counts` |

> [!WARNING]
> The on-disk vertex/submesh stride is the in-memory `size_of`. Adding a field to `Vertex`
> or `Submesh` without bumping the version would make every existing `.smesh` misread.
> The compile-time size asserts and the version check are the guards.

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the structs whose size the format pins
- [Import pipeline](../import-pipeline/) — where the bake happens
- [The .smodel container](../smodel-container/) — the file that embeds a `.smesh` as a chunk
- [Mesh upload](../gpu-mesh-upload/) — what consumes a loaded `Mesh`
