+++
title = '.sanim format'
weight = 4
+++

# .sanim format

`.sanim` is the baked binary image of one animation clip: a 32-byte header, the clip name, then a
self-describing record per track — bone, node-TRS, or morph-weight. Each animation in a
[glTF](https://github.com/KhronosGroup/glTF/tree/main/specification/2.0) source is decoded once at
import and serialized to its own image; the runtime reads it back with a bounded byte walk. The
loader accepts a single format version, `ANIM_FORMAT_VERSION` = 2, and rejects any other.

A clip is a separate image from its mesh. The [`.smesh`](../smesh-format/) format carries geometry
and skinning streams and has no animation section; a model with no clips simply writes no clip
images. The two formats share a discipline (a fixed `#[repr(C)]` Pod header, raw little-endian
arrays, a version field, a bounded defensive loader) under different magic tags, so neither can be
mistaken for the other.

## Layout

A fixed 32-byte header, then the clip name bytes, then per track a 24-byte record followed by that
track's target name, times, and values:

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct SANimHeader {
    magic: [u8; 4],     // b"SANM"
    version: u32,       // only ANIM_FORMAT_VERSION (2) is accepted
    track_count: u32,   // records that follow the clip name
    duration: f32,      // clip length, seconds
    name_len: u32,      // clip-name bytes that follow the header
    reserved: [u32; 3], // always 0
}
const _: () = assert!(size_of::<SANimHeader>() == 32);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
struct SANimTrackRecord {   // the target name, times, then values follow it
    index: i32,             // bone index for a Bone track; -1 for Node/Weights
    target: u8,             // AnimTarget (Bone 0, Node 1)
    path: u8,               // AnimPath (Translation 0, Rotation 1, Scale 2, Weights 3)
    interp: u8,             // AnimInterp (Step 0, Linear 1, CubicSpline 2)
    pad: u8,                // explicit pad to a 4-byte boundary, always 0
    morph_count: u32,       // weights per keyframe for a Weights track, else 0
    name_len: u32,          // target node-name bytes (the durable binding key)
    time_count: u32,        // number of f32 keyframe times
    value_count: u32,       // number of flat f32 values (see below)
}
const _: () = assert!(size_of::<SANimTrackRecord>() == 24);
```

The `times` and `values` arrays are raw little-endian `f32` blobs in exactly the flat shape the
sampler consumes: a `Vec3` per key for translation and scale, a quaternion `xyzw` per key for
rotation, `morph_count` floats per key for a `Weights` track, and a 3× (in-tangent, value,
out-tangent) stride for cubic-spline tracks. The
[animation data model](../../animation/animation-data-model/) explains what those arrays mean.

Sections are tightly packed with no padding between them, so nothing after the header is
guaranteed aligned: the float sections start wherever the variable-length names end, and a `SANM`
chunk sliced out of a container can begin at any byte offset. Every struct read goes through
[bytemuck](https://docs.rs/bytemuck)'s `pod_read_unaligned` and every float through an explicit
little-endian decode, so no field read assumes alignment.

The unit tests bake a two-track "Walk" clip (a two-key `Rotation` on "Hip", a two-key
`Translation` on "Foot") and assert its total is
`32 + 4 + (24 + 3 + 8 + 32) + (24 + 4 + 8 + 24) = 163` bytes: header, clip name, then each
record plus its name, times, and values, with nothing in between.

## Binding a track to its target

`target` selects what a track drives. A `Bone` track stores the bone's position in the skinned
mesh's bone list, resolved at import, together with the source node's name; a `Node` track binds
by name alone and stores `index = -1`. A morph-weights track is a `Node` target whose `path` is
`Weights`: its name is the glTF node that owns the morph targets, and `morph_count` says how many
weights each keyframe carries.

Both bindings are written so neither is lost. The index is the fast lookup; the name is the
durable key that survives a joint reorder or a reimport, so an evaluator can re-resolve a stale
index by name.

## From import to catalog

`decode_clips` walks every animation channel in the source: a TRS channel whose node is a skin
joint becomes a `Bone` track keyed by joint position, any other TRS channel a `Node` track, and a
morph-weights channel a `Weights` track. `bake_model` serializes each clip with
`save_animation_to_buffer` and embeds it as a `SANM` chunk of the model's
[`.smodel`](../smodel-container/); a chunk is a standalone `.sanim` image, byte for byte.
`catalog_rows_for_container` registers each clip as an `AssetType::Animation` catalog row carrying
its duration and track count.

`load_anim_clip` is the runtime reader. A chunk-backed clip reads its slice out of the container;
an extracted clip is a standalone `models/<id>.sanim` file read whole. Both paths feed the same
`load_animation_from_bytes`.

## Loading defensively

`load_animation_from_bytes` validates the magic and version, then walks the rest with a bounded
`Cursor`: its `take(n)` returns the next `n` bytes or `Error::Truncated` if fewer remain. Every
length in the image (the clip name, each track record, each track's name, times, and values) is
checked against the real buffer before it drives an allocation, so a header claiming a
four-gigabyte name over a 32-byte file rejects without allocating.

The `target`/`path`/`interp` bytes map back to their enums through the explicit `match` in each
`from_u8`, never a transmute; an out-of-range byte returns `Error::BadLayout`. The crate denies
`unsafe_code` throughout, the same discipline [`.smesh` loading](../smesh-format/) applies.

## Round-trip coverage

Unit tests in `sanim.rs` freeze the format. A synthetic clip round-trips through
`save_animation_to_buffer` / `load_animation_from_bytes` field for field, and
`golden_bytes_header_and_record_are_frozen` pins the byte offsets, section lengths, and
discriminant values of a baked image. Bad magic, wrong versions, truncation, and out-of-range
discriminant bytes each return their expected `Err`. The discriminant values themselves are
asserted by `anim_byte_discriminants_are_pinned` in `geometry/src/lib.rs`.

## In the code

| What | File | Symbols |
|---|---|---|
| Header + track record | `geometry/src/sanim.rs` | `SANimHeader`, `SANimTrackRecord` |
| Version constant | `geometry/src/sanim.rs` | `ANIM_FORMAT_VERSION` |
| Write path | `geometry/src/sanim.rs` | `save_animation`, `save_animation_to_buffer` |
| Defensive load | `geometry/src/sanim.rs` | `load_animation`, `load_animation_from_bytes` |
| Pinned discriminants | `geometry/src/types.rs` | `AnimTarget`, `AnimPath`, `AnimInterp`, `from_u8` |
| Clip decode at import | `geometry/src/gltf_import.rs` | `decode_clips` |
| Chunk embed + catalog rows | `assets/src/import.rs` | `bake_model`, `catalog_rows_for_container` |
| Runtime clip load | `assets/src/load.rs` | `load_anim_clip` |

## Related

- [Animation data model](../../animation/animation-data-model/) — the clip/track types this serializes
- [.smesh format](../smesh-format/) — the sibling mesh image
- [The .smodel container](../smodel-container/) — the container that embeds a clip as a `SANM` chunk
- [Model import](../gltf-and-obj-import/) — where glTF animations are decoded
