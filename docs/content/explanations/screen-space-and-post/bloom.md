+++
title = 'Bloom'
weight = 7
math = true
+++

# Bloom

Bloom spreads bright scene radiance over nearby pixels. An emissive surface or a sun reflection can
carry far more energy than the display can show, yet remain a single sharp pixel without this filter.
The resulting glow makes that intensity visible over a larger area before the view transform maps it
to the display.

Anima follows the mip-pyramid veil/bloom method presented in Jorge Jimenez's
[*Next Generation Post Processing in Call of Duty: Advanced Warfare*](https://www.advances.realtimerendering.com/s2014/index.html#_NEXT_GENERATION_POST_PROCESSING_IN_CALL_OF_DUTY_ADVANCED_WARFARE).
Compute passes build and collapse the pyramid in scene-linear `rgba16f`, then composite it into the
display-extent color image before [tonemapping](../tonemap-and-exposure/).

## The pyramid

One compute pipeline serves four branches selected by `BloomPush.pass`:

1. **Downsample:** a 13-sample bilinear kernel halves the image at every level. The first output is
   half resolution.
2. **Upsample:** a 9-sample tent filter reads a coarse level and adds it into the next finer level.
   `filterRadius` controls the UV spacing of the tent.
3. **Streak:** two optional horizontal Gaussian passes blur `mip0` through ping-pong images.
4. **Composite:** the shader combines the pyramid, dirt mask, and streak with the full-resolution
   scene color.

For a display extent with minimum dimension $d$, the renderer chooses

$$
N = \operatorname{clamp}(\lfloor \log_2 d \rfloor - 3,\ 1,\ 7).
$$

A 1280x720 view therefore uses six levels; a 1920x1080 view reaches the seven-level cap. Each level
has its own render-graph resource identity and barriers. The backing images come from keyed,
per-frame-slot transient storage, which reuses compatible allocations after that frame slot becomes
available again.

The final branch evaluates the following base mix after applying the optional art-direction terms:

$$
c_\text{out} = \operatorname{lerp}
\left(c_\text{hdr},\ c_\text{bloom}\,t_\text{bloom},\ w_\text{bloom}\right).
$$

`intensity` supplies $w_\text{bloom}$ and defaults to `0.05`; `tint` supplies
$t_\text{bloom}$. This is a relative `lerp` composite rather than an additive
`c_hdr + bloom` operation.

## First-level filtering

Bloom is thresholdless when `threshold` is zero. In that mode, every scene-linear pixel contributes
according to its value. The first downsample also applies a Karis luminance-weighted average to five
overlapping 2x2 groups. Its $1/(1 + \operatorname{luma})$ weighting reduces the influence of an
isolated high-energy sample. Deeper levels omit this step because their inputs have already passed
through the first filter.

A positive `threshold` enables the shader's soft-knee prefilter on that first downsample. Values
below the knee contribute less, while brighter values pass through according to the prefilter curve.
The renderer default is `0.0`.

## Art-direction terms

The composite always samples a lens-dirt texture. Without an authored texture, the binding points to
a 1x1 white fallback. With a texture, the shader clamps its RGB values to at most one, multiplies by
`dirtTint`, and interpolates from identity using `dirtIntensity`. The dirt term therefore modulates
the pyramid before the bloom tint and mix.

When anamorphic bloom is enabled, two half-resolution horizontal blur passes run from `mip0`. Their
sample spacing is `scatter * max(ratio, 1)`. The composite adds the final streak image to the radial
pyramid, multiplied by `anamorphicTint` and `anamorphicIntensity`. Disabled streaks bind the white
fallback and set their composite intensity to zero.

`perMipTint` supplies one RGB tint for each progressive upsample step. Missing entries use white, so
an empty list leaves the pyramid unchanged. This supports different colors at different glow scales
without introducing another composite path.

## Frame order

Bloom reads the scene color after SSGI history and atmospheric compositing. Fog therefore attenuates
a distant bright source before that source spreads through the bloom pyramid. Tonemapping and
exposure run after bloom, so the view transform receives the combined scene-linear result.

Every dispatch declares sampled reads and storage-image read/write access through the
[render graph](../../frame-and-render-graph/render-graph-overview/). The graph derives the transitions
between `GENERAL` and `SHADER_READ_ONLY_OPTIMAL`; bloom code does not record image barriers directly.

## Example

Enable thresholdless bloom with the renderer defaults for mix and scatter:

```sh
sa set-bloom \
  --enabled true \
  --intensity 0.05 \
  --scatter 0.005 \
  --tint '[1,1,1]' \
  --threshold 0
```

The same `set-bloom` request can patch `dirtTexture`, `dirtIntensity`, `dirtTint`, `anamorphic`, and
`perMipTint`. `render-stats` returns the applied settings. Project save/load stores them under
`renderSettings`, and the editor exposes them in the Post panel.

## In the code

| What | File | Symbols |
|---|---|---|
| Compute shader | `engine/assets/shaders/bloom.slang` | `computeMain`, `downsample13`, `upsampleTent`, `streakBlur`, `Push` |
| Pyramid and composite scheduling | `engine/crates/rendering/src/renderer/` | `acquire_bloom_mips`, `acquire_bloom_streak`, `add_bloom_pass`, `set_bloom` |
| Push constants | `engine/crates/rendering/src/overlay.rs` | `BloomPush` |
| Keyed transient images | `engine/crates/rendering/src/transient.rs` | `BLOOM_MIP_KEYS`, `BLOOM_STREAK_KEYS` |
| Descriptors and pipeline | `engine/crates/rendering/src/descriptors/`, `engine/crates/rendering/src/pipelines/` | `MAX_BLOOM_MIPS`, `BLOOM_PASSES_PER_FRAME`, `create_bloom_layout`, `request_bloom` |
| Per-view bindings | `engine/crates/rendering/src/view_target/` | `write_bloom_sets`, `BloomCompositeBindings` |
| Control contract | `engine/crates/protocol/src/dto/`, `engine/crates/control/src/commands_render/` | `SetBloomParams`, `AnamorphicParams`, `set-bloom` |
| Project persistence | `engine/crates/rendering/src/render_settings.rs` | `RenderSettings`, `settings_to_json`, `parse_render_settings` |
| Editor controls | `editor/src/panels/PostProcessPanel.tsx` | `PostProcessPanel`, `bloomFrom`, `applyBloom` |

## Related

- [Tonemapping](../tonemap-and-exposure/): maps the bloomed HDR result to display values
- [Compute post-process](../compute-post-process-pattern/): the shared compute-pass structure
- [Render graph](../../frame-and-render-graph/render-graph-overview/): derives bloom's resource transitions
