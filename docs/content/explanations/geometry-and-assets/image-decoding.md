+++
title = 'Image decoding'
weight = 4
+++

# Image decoding

Image decoding turns encoded image bytes (a PNG, a JPG, or an HDR float source) into raw,
tightly packed pixels. A GPU sampler reads a fixed uncompressed layout, so every encoded file
passes through this step before it becomes a texture.

Encoded bytes reach the decoder from a texture file copied into the asset directory, an external
image a model references, or a chunk embedded in a [`.smodel` container](../smodel-container/).
The decode functions in `saffron-geometry` sit on the
[`image` crate](https://github.com/image-rs/image) and normalize all of them into one of two
output shapes.

## Two output contracts

The 8-bit path produces a `DecodedImage`: four channels, 8 bits each, no row padding. The float
path produces a `DecodedImageFloat`: four channels of linear `f32`, for HDR sources whose
radiance can exceed `1.0`.

```rust
pub struct DecodedImage {
    pub rgba: Vec<u8>,    // tightly packed, width * height * 4 bytes
    pub width: u32,
    pub height: u32,
}

pub struct DecodedImageFloat {
    pub rgba: Vec<f32>,   // tightly packed, width * height * 4 floats; linear radiance
    pub width: u32,
    pub height: u32,
}
```

Both paths force four channels whatever the source stores. `to_rgba8` runs
`image::DynamicImage::to_rgba8`, so a 3-channel JPG comes out as RGBA8 with alpha filled to
`255`; `to_rgba32f` does the same for floats, filling alpha to `1.0`. The float conversion
applies no transfer function, and a bright texel keeps its real radiance. One fixed layout per
shape lets the upload path assume a single format.

## Entry points

Each shape has a file entry point and a memory entry point:

| Function | Input | Output |
|---|---|---|
| `decode_image` | file path, via `image::open` | `DecodedImage` |
| `decode_image_from_memory` | byte slice, via `image::load_from_memory` | `DecodedImage` |
| `decode_image_hdr` | file path, decoded by content | `DecodedImageFloat` |
| `decode_image_from_memory_hdr` | byte slice | `DecodedImageFloat` |

`decode_image_hdr` reads the file into memory and decodes the bytes rather than calling
`image::open`, which dispatches on the file extension. Float textures are stored under a `.hdr`
name whatever the real container holds — a
[Radiance RGBE](https://en.wikipedia.org/wiki/RGBE_image_format) payload or an
[OpenEXR](https://openexr.com/) one. Content sniffing picks the right decoder for both.

A failed decode returns `Err(Error::Decode(…))` carrying the decoder's message. The pixels are
owned by the returned `Vec`, so no caller manages a separate buffer lifetime.

## One byte seam for files and chunks

The texture flows decode from memory. The [asset server](../asset-server-and-catalog/) resolves
a texture id to a `ByteSource`, whose `read` returns either a whole standalone file or exactly
the `[offset, offset + length)` slice of a `.smodel` `STEX` chunk. An embedded texture and a
loose file therefore hit the same decoder, with the same bytes the
[importer](../gltf-and-obj-import/) originally carried.

Import registration decodes as well. `register_texture_bytes` and `register_hdr_texture_bytes`
decode incoming bytes to validate them and seed the GPU texture cache, then write the encoded
original under `textures/<uuid>.<ext>`. The project stores the compressed file; a later load
re-runs the decode.

## Colorspace is decided at upload

Decoding is format work only: no transfer function is applied or recorded. The catalog row's
`Colorspace` selects the uploader — an explicit `.smeta` value wins, otherwise the row's
`hdr`/`linear` provenance set at registration:

| `Colorspace` | Uploader | GPU format | Mips |
|---|---|---|---|
| `Hdr` | `upload_texture_float` | `R16G16B16A16_SFLOAT`, f32→f16 narrowed on the CPU | 1 |
| `Linear` | `upload_texture`, `srgb = false` | `R8G8B8A8_UNORM` | full chain, blitted down |
| `Srgb` / `Auto` | `upload_texture`, `srgb = true` | `R8G8B8A8_SRGB` | full chain, blitted down |

An `_SRGB` format makes the sampler apply the sRGB-to-linear transfer in hardware, so the
[BRDF](../../lighting-and-brdf/cook-torrance-brdf/) computes on linear color. The policy that
fills those rows keys on the texture's semantic role: albedo and emissive maps are sRGB-encoded
color, and every data map (normal, metallic-roughness, occlusion, height) is linear.

## In the code

| What | File | Symbols |
|---|---|---|
| Decoded contracts | `geometry/src/types.rs` | `DecodedImage`, `DecodedImageFloat` |
| Decode entry points | `geometry/src/image_decode.rs` | `decode_image`, `decode_image_from_memory`, `decode_image_hdr`, `decode_image_from_memory_hdr` |
| File-or-chunk byte source | `assets/src/model.rs` | `ByteSource::read` |
| Cache-miss decode + upload | `assets/src/load.rs` | `load_texture_asset`, `upload_texture_from_source` |
| Import-time decode + registration | `assets/src/scan.rs` | `register_texture_bytes`, `register_hdr_texture_bytes`, `colorspace_for_role_explicit` |
| Upload seam + implementation | `assets/src/gpu.rs`; `rendering/src/upload.rs` | `GpuUploader`, `Uploader::upload_texture`, `Uploader::upload_texture_float` |
| Colorspace + role | `scene/src/environment.rs` | `Colorspace`, `TextureRole` |

## Related

- [Model import](../gltf-and-obj-import/) — carries the encoded texture bytes out of glTF/OBJ
- [smodel container](../smodel-container/) — the `STEX` chunk the slice decode reads
- [Asset catalog](../asset-server-and-catalog/) — the rows whose colorspace picks the uploader
- [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) — why albedo is sampled sRGB→linear
