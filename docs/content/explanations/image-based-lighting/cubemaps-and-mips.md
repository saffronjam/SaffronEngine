+++
title = 'Cubemaps and mips'
weight = 2
math = true
+++

# Cubemaps and mips

A cubemap stores six square faces and accepts a 3D sampling direction. Image-based lighting uses one cube for source environment radiance and one for roughness-prefiltered specular radiance. Diffuse sky lighting uses spherical harmonics instead of a cube.

## One image, two view shapes

`IblCube::new` creates a `CUBE_COMPATIBLE` image with six array layers, `N` mip levels, and `R16G16B16A16_SFLOAT` storage. Its persistent `CUBE` view spans every face and mip for sampling.

A compute shader writes the same memory through a `TYPE_2D_ARRAY` storage view. `IblCube::storage_view` selects one mip and exposes all six layers:

```rust
.view_type(vk::ImageViewType::TYPE_2D_ARRAY)
.subresource_range(vk::ImageSubresourceRange {
    base_mip_level: mip,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 6,
    ..color_range
})
```

The convolution shaders address `tid.z` as the face index. Sampling shaders see the same allocation as a cube, so a normalized direction returns a filtered color.

## Source mips

The 256² environment cube has nine levels down to 1². Successive linear blits create the chain after mip zero is filled. SH projection reads mip 3, which represents a pre-averaged 32² source. The GGX prefilter selects source mips from the solid angle of each importance sample, which reduces bright-texel variance without clamping radiance.

## Roughness mips

The 256² prefiltered cube has five levels. Mip $m$ uses roughness $m/(M-1)$, so mip zero is sharp and mip four is fully rough. The mesh shader maps perceptual roughness through `prefilterLod` and samples between levels with trilinear filtering.

| Texture | Base size | Mips |
|---|---:|---:|
| Environment | 256² × 6 | 9 |
| Prefiltered specular | 256² × 6 | 5 |
| BRDF LUT | 256² | 1 |

The BRDF LUT is a two-dimensional `IblImage`, not a cube. The nine diffuse SH coefficients live in a storage buffer.

## In the code

| What | File | Symbols |
|---|---|---|
| Cube image and sampling view | `engine/crates/rendering/src/ibl.rs` | `IblCube`, `IblCube::new` |
| Per-mip storage views | `engine/crates/rendering/src/ibl.rs` | `IblCube::storage_view` |
| Source mip generation | `engine/crates/rendering/src/ibl.rs` | `generate_cube_mips` |
| Sizes and mip count | `engine/crates/rendering/src/ibl.rs` | `IBL_ENV_SIZE`, `IBL_PREFILTER_SIZE`, `IBL_PREFILTER_MIPS`, `IBL_LUT_SIZE` |
| Mip and roughness contract | `engine/assets/shaders/lighting.slang` | `IblPrefilterMaxMip`, `prefilterLod` |

> [!NOTE]
> `IblPrefilterMaxMip = 4.0` and `IBL_PREFILTER_MIPS = 5` describe the same hand-maintained boundary. A mip-count change updates both values.

## Related

- [Real-time sky-light capture](../realtime-skylight-capture/) explains the imported cube resources.
- [Specular prefilter](../specular-prefilter/) explains what fills the roughness levels.
- [Baking](../ibl-bake-pass/) explains source mip generation and startup validity.
