+++
title = '.smesh format'
weight = 3
+++

# .smesh format

`.smesh` is the baked binary mesh image: a 64-byte header, three required sections
(vertices, indices, submeshes) laid out contiguously, and two optional sections (skin,
morph) selected by header flags. A model is baked once by the
[import pipeline](../import-pipeline/); every later load reads the image directly.

Importing a source model ([glTF](https://github.com/KhronosGroup/glTF) or OBJ) means
parsing JSON or text, de-duplicating vertices, and possibly regenerating normals and
tangents. The bake moves that cost off the runtime path: a load casts the byte spans
straight into typed slices with no per-element decode.

## Layout

The fixed header comes first; the required sections follow at the offsets it records:

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
struct SMeshHeader {
    magic: [u8; 4],        // b"SMSH"
    version: u32,          // MESH_FORMAT_VERSION (4)
    flags: u32,            // MESH_FLAG_SKIN | MESH_FLAG_MORPH bits
    vertex_stride: u32,    // == size_of::<Vertex>() (48)
    vertex_count: u32,
    index_count: u32,
    index_width: u32,      // bytes per index (4)
    submesh_count: u32,
    vertices_offset: u64,  // == size_of::<SMeshHeader>() (64)
    indices_offset: u64,
    submeshes_offset: u64,
    morph_offset: u64,     // 0 when MESH_FLAG_MORPH is clear
}
const _: () = assert!(size_of::<SMeshHeader>() == 64, "SMeshHeader must be exactly 64 bytes");
```

The sections are written with [bytemuck](https://docs.rs/bytemuck)'s `cast_slice` over
`#[repr(C)]` Pod structs, no per-element serialization, so the in-memory layout is the
on-disk layout. `Vertex` and `Submesh` have
[compile-time-pinned sizes](../mesh-and-vertex-layout/) of 48 and 16 bytes. The header
records `vertex_stride` and `index_width` so the loader can reject a file written by an
incompatible build; one version exists, `MESH_FORMAT_VERSION` = 4.

## Optional sections

Two flag bits select the optional payloads, and the encoder sets each bit from whether
the corresponding stream is non-empty. One write path,
`save_mesh_to_buffer(mesh, skin, morph)`, covers every combination.

- `MESH_FLAG_SKIN` — a `VertexSkin` array (24 bytes each, parallel to the vertices
  one-for-one) follows the submeshes. A length mismatch at encode time is
  `Error::SkinLengthMismatch`.
- `MESH_FLAG_MORPH` — a morph section sits at `morph_offset`, after the skin section
  when both are present: a `MorphSectionHeader` with the target and delta totals, one
  16-byte `MorphTargetDesc` range per target, then the flat 28-byte `MorphDelta` array.

Morph target names are not in the binary; they ride the
[`.smodel`](../smodel-container/) container's META, so the image stays pure fixed-stride
Pod arrays. Worked numbers, pinned by the `golden_bytes_header_is_frozen` test: a
3-vertex, 3-index, 1-submesh triangle bakes to 64 + 3·48 + 3·4 + 16 = 236 bytes, and
the skinned bake of the same mesh adds 3·24 = 72 more.

## Why a custom binary format

The format trades import cost for load cost, and it is versioned so a layout change is
detected rather than silently misread. It is also self-describing enough to validate
without trusting the producer. That matters because the payload is read back as raw
memory through safe `bytemuck` casts under the crate's `#![deny(unsafe_code)]`.

The image is a triple contract: the disk bytes equal the in-memory payload equal the GPU
vertex buffer, and the section offsets are self-relative. So a `.smesh` embedded as a
[`.smodel`](../smodel-container/) `MESH` chunk slice reads identically to a standalone
file; both go through the same `load_mesh_from_bytes`.

## Loading defensively

`load_mesh_from_bytes` does not trust the header. Before slicing it recomputes the
expected layout from the counts and requires the stored offsets to match and the span to
be at least that long:

```rust
let vertices_end = size_of::<SMeshHeader>() as u64
    + u64::from(header.vertex_count) * size_of::<Vertex>() as u64;
let indices_end = vertices_end + u64::from(header.index_count) * size_of::<u32>() as u64;
let submeshes_end = indices_end + u64::from(header.submesh_count) * size_of::<Submesh>() as u64;
if header.vertices_offset != size_of::<SMeshHeader>() as u64
    || header.indices_offset != vertices_end
    || header.submeshes_offset != indices_end
    || (bytes.len() as u64) < submeshes_end
{
    return Err(Error::BadLayout);
}
```

The checks run in order: span at least header-sized (`Error::Truncated`), magic `SMSH`
(`Error::BadMagic`), version 4 (`Error::UnsupportedVersion`), stride and index-width
match, then the layout-consistency block above. A malformed huge `vertex_count` would
otherwise drive a giant allocation; rejecting the file as inconsistent first keeps a
corrupt file from exhausting memory. The span length is the chunk length, not a file
size, so an embedded `.smodel` chunk validates the same way.

The optional-section readers apply the same discipline. `load_mesh_skin_from_bytes`
returns an empty stream (not an error) when the flag is clear and `Error::Truncated`
when the flag is set but the skin bytes fall short. `load_mesh_morph_from_bytes`
bounds-checks the section header, the descriptor table, and the delta pool, and rejects
a target whose delta range overruns the pool as `Error::BadLayout`.

## Round-trip coverage

The codec is covered by unit tests in `smesh.rs`. Every skin/morph combination baked
through `save_mesh_to_buffer` reads back equal to the source mesh, skin stream, and
morph deltas. `golden_bytes_header_is_frozen` asserts the exact header field values,
flag bits, and section offsets of a known bake, and the strides stay pinned by the
compile-time `size_of` asserts in `geometry/src/lib.rs` and `geometry/src/types.rs`.
Malformed inputs (bad magic, unknown version, truncated header, truncated body or skin)
each return their expected `Err`.

## In the code

| What | File | Symbols |
|---|---|---|
| Header layout | `geometry/src/smesh.rs` | `SMeshHeader` |
| Version + flags | `geometry/src/smesh.rs` | `MESH_FORMAT_VERSION`, `MESH_FLAG_SKIN`, `MESH_FLAG_MORPH` |
| Morph section | `geometry/src/smesh.rs` | `MorphSectionHeader`, `MorphTargetDesc` |
| Write path | `geometry/src/smesh.rs` | `save_mesh`, `save_mesh_to_buffer`, `encode_mesh_image` |
| Defensive load | `geometry/src/smesh.rs` | `load_mesh`, `load_mesh_from_bytes`, `load_mesh_skin_from_bytes`, `load_mesh_morph_from_bytes` |
| Header-only counts | `geometry/src/smesh.rs` | `mesh_counts_from_bytes`, `mesh_file_counts` |
| Pinned strides | `geometry/src/lib.rs`, `geometry/src/types.rs` | the `Vertex`/`Submesh`/`VertexSkin`/`MorphDelta` size asserts |

> [!WARNING]
> The on-disk vertex/submesh stride is the in-memory `size_of`. Adding a field to `Vertex`
> or `Submesh` without bumping the version would make every existing `.smesh` misread.
> The compile-time size asserts and the version check are the guards.

## Related

- [Vertex layout](../mesh-and-vertex-layout/) — the structs whose size the format pins
- [Import pipeline](../import-pipeline/) — where the bake happens
- [The .smodel container](../smodel-container/) — the file that embeds a `.smesh` as a chunk
- [Mesh upload](../gpu-mesh-upload/) — what consumes a loaded `Mesh`
