+++
title = 'Color grading'
weight = 6
math = true
+++

# Color grading

Color grading controls the scene's white balance, contrast, saturation, and colour relationships. The
engine applies the grade to scene-linear HDR radiance after exposure and before the selected
[view transform](../tonemap-and-exposure/), then applies an optional creative 3D LUT in display space.

## Scene-linear grade

The tonemap compute pass evaluates the image pipeline in this order:

1. Multiply scene radiance by the exposure value.
2. Apply white balance, global contrast, saturation, and the global
   [ASC CDL](https://docs.acescentral.com/specifications/asc_cdl/) slope/offset/power operation.
3. Blend separate shadow, midtone, and highlight corrections by scene-linear luma.
4. Apply the channel mixer and split tone.
5. Run the view transform and display encoding.
6. Blend the display-space creative LUT by its intensity.

Global contrast is a gain around the configurable middle-grey pivot $p$. The default pivot is
$0.18$:

$$
c' = 2^{(\log_2(\max(c, 10^{-5})) - \log_2 p)k + \log_2 p}
$$

The global CDL operation follows it:

$$
c' = \max(S c + O, 0)^P
$$

The default grade is an identity. It uses a 6500 K white point, contrast and saturation of 1, CDL
slope and power of 1, zero offset, and an identity channel mixer.

## Tonal ranges

Shadows, midtones, and highlights each carry their own CDL values, saturation, and contrast. Smooth
luma masks blend their corrected result with the global grade:

```text
shadow   = 1 - smoothstep(0, shadowsMax, luma)
highlight = smoothstep(highlightsMin, 1, luma)
midtone   = saturate(1 - shadow - highlight)
```

`shadowsMax` defaults to `0.09`, and `highlightsMin` defaults to `0.5`. The masks use the graded
scene-linear luma, before the view transform compresses highlights.

The channel mixer is a row-major $3\times3$ matrix. Each row defines one output channel as a weighted
sum of the input channels. Split toning multiplies the result by a luma blend between shadow and
highlight tints; `[0.5, 0.5, 0.5]` is neutral because the shader doubles the blended tint.

## Creative LUTs

The asset server imports `.cube` files with sizes 17, 33, or 65 as `AssetType::Lut` assets backed by
`R16G16B16A16_SFLOAT` 3D images. The tonemap shader samples the table with tetrahedral interpolation
after display encoding. An identity size-2 LUT remains bound when no creative look is selected, so the
descriptor layout and shader path stay fixed.

The Post panel can also turn its master and per-channel tone curves into a size-17 `.cube` table. It
imports that table through `import-lut` and assigns the resulting asset through `set-color-grading`.

For example, this command warms the white balance and raises contrast while leaving the other grade
fields unchanged:

```sh
sa set-color-grading --temperature 5000 --contrast 1.2
```

`render-stats` reports the complete grade and the resolved creative LUT's asset id, intensity, and
size.

## Baked looks

`bake-look` evaluates the scene-linear grade, view transform, and creative LUT into a $33^3$ GPU-baked
table. A log2 shaper maps scene-linear radiance into the bounded cube domain over $[-14,+11]$ EV,
anchored at 18% grey:

$$
t = \operatorname{saturate}\left(\frac{\log_2(c / 0.18) + 14}{25}\right)
$$

The resulting `.slut` stores a small header followed by red-fastest RGB16F samples. The player selects
the frozen-look branch in `tonemap.slang`, encodes exposed radiance with the same shaper, and performs
one tetrahedral LUT lookup in place of the live grade and view-transform operations.

## Editor and GPU state

The Post panel's Color tab exposes global Lift/Gamma/Gain wheels, the three tonal ranges, channel
mixing, split toning, creative-LUT intensity, and tone curves. A wheel maps its disc to a zero-sum
chroma offset and its vertical bar to luma. The panel sends edits through `set-color-grading` and
reads the applied values from `RenderStatsDto.colorGrading`.

Each view owns a persistently mapped `GradeUniform` buffer with one aligned slice per frame in flight.
The renderer writes the active frame's slice and binds it through a dynamic uniform-buffer offset. The
tonemap descriptor set therefore does not need a per-frame rewrite.

## In the code

| What | File | Symbols |
|---|---|---|
| Grade and LUT operations | `tonemap_ops.slang` | `grade`, `rangeWeights`, `splitTone`, `sampleCubeTetrahedral`, `lutShaperEncode` |
| Tonemap entry point | `tonemap.slang` | `computeMain`, `GradeUniform` |
| CPU grade state and uniform | `overlay.rs` | `ColorGrade`, `GradeRange`, `GradeUniform`, `LUT_BAKE_SIZE` |
| Per-frame uniform upload | `view_target.rs` | `write_grade`, `grade_ubo_offset` |
| Renderer controls and look bake | `renderer.rs` | `set_color_grading`, `color_grading`, `bake_look_lut`, `add_tonemap_pass` |
| LUT parsing and persistence | `cube.rs` | `parse_cube`, `CubeLut`, `BakedLut`, `import_cube_lut`, `import_baked_lut` |
| Wire types and commands | `dto.rs`, `commands_render.rs`, `commands_asset.rs` | `SetColorGradingParams`, `CreativeLutStat`, `set-color-grading`, `bake-look`, `import-lut` |
| Post Color tab and grading controls | `PostProcessPanel.tsx`, `GradingWheel.tsx`, `ToneCurve.tsx` | `PostProcessPanel`, `GradingWheel`, `ToneCurve`, `curveToCube` |

## Related

- [Tonemapping and exposure](../tonemap-and-exposure/) — maps the graded HDR image to display values
- [Bloom](../bloom/) — contributes scene-linear energy before the grade
- [Compute post-process](../compute-post-process-pattern/) — describes the in-place compute-pass shape
