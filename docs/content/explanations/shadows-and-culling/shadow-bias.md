+++
title = 'Shadow bias'
weight = 5
+++

# Shadow bias

Shadow bias is a small offset added to a shadow map's depth comparison so a surface does not shadow
itself.

A shadow page stores depth at finite resolution. A surface compared against its own quantized depth
tends to half-shadow itself, producing the dark speckles of shadow acne. Bias shifts the comparison
enough to stop that. The offset has a working range: too little and acne returns; too much and
shadows detach from their casters, an artifact called peter-panning.

## How it works

The bias is applied in the rasterizer while [virtual-shadow pages](../virtual-shadow-maps/) render.
The page pass sets the dynamic depth bias once, covering every bucket draw the recorder issues for
every page in the pass:

```rust
raw.cmd_set_depth_bias(cmd, SHADOW_DEPTH_BIAS_CONSTANT, 0.0, SHADOW_DEPTH_BIAS_SLOPE);
```

with constant `1.25` and slope `2.0`. The constant term shifts every depth value by a fixed amount.
The slope term scales with the polygon's gradient relative to the light, which is what acne needs: a
surface seen edge-on by the light spans more depth per texel and needs proportionally more bias.
Because the bias is baked into the stored depth, the comparison in `vsmTilePcf` is a plain
`SampleCmp` with no extra offset.

One pair of constants covers every light kind because every page rasterizes through the same
depth-family pipeline: directional pages under an ortho window, spot and point pages under cropped
perspective transforms.

## The acne–peter-panning trade

The two failure modes pull in opposite directions:

| Too little bias | Too much bias |
|---|---|
| surface shadows itself | shadow lifts off the contact point |
| dark speckle / moiré on lit faces | gap of light under the caster |

No single value is correct; bias lives in a tuning band. Slope bias does most of the work, since
acne is worst exactly where surfaces graze the light, and the constant handles the residual
flat-surface case.

> [!TIP]
> If you see acne, raise `SHADOW_DEPTH_BIAS_SLOPE` before the constant — acne is slope-driven. If
> shadows look detached, the constant is usually the culprit.

## In the code

| What | File | Symbols |
|---|---|---|
| Bias values | `crates/rendering/src/lighting.rs` | `SHADOW_DEPTH_BIAS_CONSTANT`, `SHADOW_DEPTH_BIAS_SLOPE` |
| Where the bias is set | `crates/rendering/src/renderer.rs` | `add_vsm_page_passes` (`cmd_set_depth_bias`) |
| The comparison it feeds | `assets/shaders/lighting_common.slang` | `vsmTilePcf` |

## Related

- [PCF filtering](../pcf-filtering/) — the comparison the bias feeds into
- [Virtual shadow maps](../virtual-shadow-maps/) — the page passes that set it
- [Directional shadows](../directional-shadows/) — the depth mapping the bias offsets
