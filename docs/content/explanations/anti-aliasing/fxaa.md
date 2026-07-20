+++
title = 'FXAA'
weight = 2
+++

# FXAA

FXAA (Fast Approximate Anti-Aliasing) is a post-process: it finds high-contrast edges in the
finished image and blends along them. The technique is
[Timothy Lottes' FXAA (NVIDIA)](https://developer.download.nvidia.com/assets/gamedev/files/sdk/11/FXAA_WhitePaper.pdf);
`fxaa.slang` implements the console variant of FXAA 3 as a single compute dispatch. Because it
reads only the rendered image, it costs a few texture samples per pixel and touches nothing about
how the scene is rasterized.

Working from the image alone cuts both ways. FXAA smooths edges that rasterization-time
anti-aliasing cannot reach (alpha-tested cutouts, specular highlights, shader aliasing), but it
cannot tell a real edge from sharp texture detail, so it softens both wherever it fires.

## How it works

The shader works in luma, perceived brightness:
`dot(c, float3(0.299, 0.587, 0.114))`, the [Rec. 601](https://www.itu.int/rec/R-REC-BT.601)
weights. Each invocation handles one output pixel in four steps.

1. **Sample a cross.** Read the center pixel and its four diagonal neighbours, take each luma,
   and find the local min and max. Their difference is the contrast range.

2. **Skip flat regions.** If the range is below
   `max(EDGE_THRESHOLD_MIN, lumaMax * EDGE_THRESHOLD_MAX)` — 0.0312 absolute, 0.125 relative to
   the brightest sample — this is not an edge: write the original pixel and return. Most of the
   screen takes this branch, which is why FXAA is cheap.

3. **Find the edge direction.** The diagonal luma differences give a 2D direction along the
   edge, normalized by its smaller component and clamped so the blur never reaches past a few
   texels.

4. **Blend along it.** Average a near pair of samples at ±1/6 of the direction (`rgbA`), then
   fold in a far pair at ±1/2 for a wider four-tap average (`rgbB`). If the wide average's luma
   escapes the min/max range from step 1, it overshot the edge and the shader keeps the near
   pair.

The direction math is the whole trick, so here it is with its guards:

```hlsl
dir.x = -((lumaNW + lumaNE) - (lumaSW + lumaSE));
dir.y = ((lumaNW + lumaSW) - (lumaNE + lumaSE));

float dirReduce = max((lumaNW + lumaNE + lumaSW + lumaSE) * 0.25 * REDUCE_MUL, REDUCE_MIN);
float rcpDirMin = 1.0 / (min(abs(dir.x), abs(dir.y)) + dirReduce);
dir = clamp(dir * rcpDirMin, -SPAN_MAX, SPAN_MAX) * texel;
```

`dirReduce` keeps a near-flat gradient from exploding under the reciprocal, and the clamp caps
the blur reach at `SPAN_MAX` (8) texels along either axis.

## The resolve slot in the frame

Every frame lands the scene's color in an input-extent, single-sample scratch image (rgba16f,
the offscreen color format); MSAA resolves its multisampled target into it, the other modes
attach it directly. A resolve stage then writes the display-extent offscreen, and the
[AA mode](../aa-modes/) picks which resolve that is: the [TAA](../../screen-space-and-post/taa/)
resolve, this FXAA pass, or a plain upscale copy when neither is on. `Aa::set` keeps the modes
mutually exclusive, and the control plane fronts it:

```sh
sa set-aa fxaa
{
  "aa": "fxaa"
}
```

While FXAA is the active mode and the scratch exists, `Pipelines::request_fxaa` resolves the
compute PSO — built once from `shaders/fxaa.spv` over the two-binding fxaa set layout, no push
constants — and `add_fxaa_pass` appends the pass. It declares `SampledReadCompute` on the scratch
and `StorageImageRwCompute` on the offscreen, and the graph derives the transitions: scratch to
shader-read-only, offscreen to `GENERAL` for the storage write, then onward for the present blit.
This is the read-from-A, write-to-B shape of the
[compute post-process pattern](../../screen-space-and-post/compute-post-process-pattern/).

The dispatch covers the display grid in 8×8 groups and samples the scratch at normalized UVs. At
a render scale below 1 the scratch is smaller than the display, so the same pass folds a bilinear
upscale into the edge blend; at 1:1 the UVs land on texel centers and it reduces to a per-pixel
edge blur.

## In the code

| What | File | Symbols |
|---|---|---|
| Mode switch | `aa.rs` | `Aa::set`, `Aa::fxaa` |
| Scratch target (shared by every resolve) | `view_target.rs` | `ViewTarget::build_aa_targets`, `scratch` |
| Descriptor set (scratch → offscreen) | `view_target.rs`, `descriptors.rs` | `write_aa_sets`, `fxaa_set`, `Descriptors::fxaa_set_layout` |
| Pass + dispatch in the graph | `renderer.rs` | `add_fxaa_pass` |
| PSO | `pipelines.rs` | `Pipelines::request_fxaa` |
| The shader | `fxaa.slang` | `computeMain`, `luma`, `EDGE_THRESHOLD_MIN`, `EDGE_THRESHOLD_MAX`, `SPAN_MAX` |
| CLI front | `commands_render.rs` | `set-aa` |

> [!NOTE]
> FXAA treats luma contrast as "an edge" and cannot distinguish a geometry edge from a sharp
> texture or a noisy specular highlight, so it softens all three. That is the trade against
> MSAA, which fixes geometry edges exactly but sees nothing inside a triangle.

## Related

- [AA modes](../aa-modes/) — the full mode table and how the three are switched
- [MSAA](../msaa/) — the rasterization-time alternative
- [TAA](../../screen-space-and-post/taa/) — the temporal alternative on the same scratch
- [Compute post-process pattern](../../screen-space-and-post/compute-post-process-pattern/) — the shared dispatch shape
