+++
title = 'Tonemapping'
weight = 5
math = true
+++

# Tonemapping

Tonemapping maps an unbounded linear HDR signal down to the bounded range a display can show. A scene
is rendered in linear high dynamic range, so a bright light can push a pixel well past 1.0, beyond
anything a display reproduces. The tonemap stage compresses that range, encodes the result for the
display, and produces the final viewable image.

It runs on the `rgba16f` offscreen — where the [BRDF](../../lighting-and-brdf/cook-torrance-brdf/)
accumulates radiance with no ceiling — before the present blit samples it. It is a single compute pass, one
invocation per pixel, that applies exposure, tonemaps, gamma-corrects, and writes the result back in
place.

## What each pixel does

```hlsl
float3 hdr    = color.rgb * push.exposure;     // 1. exposure
float3 mapped = hdr / (hdr + 1.0);             // 2. Reinhard
mapped = pow(mapped, float3(1.0 / 2.2));       // 3. gamma 2.2
```

**Exposure** is a single linear multiplier in the push constant. It is $2^{EV}$: the UI works in EV
stops and the engine raises 2 to that stop, so +1 EV doubles brightness. This knob decides how much of
the HDR range lands in the visible band.

**Reinhard** is the tone curve, applied per channel:

$$
c_\text{mapped} = \frac{c}{c + 1}
$$

It maps $[0, \infty)$ into $[0, 1)$, compressing highlights smoothly so nothing clips hard to white. It
is the simplest operator that does the job, and the pass leaves a clean seam to swap in ACES later.

**Gamma** raises the result to $1/2.2$ to encode it for an sRGB-ish display, since everything upstream
lives in linear light. Alpha is passed through untouched.

Between exposure and the transform the pass folds a scene-linear [color grade](../color-grading/) —
white balance, contrast, saturation, and ASC-CDL — so the operator is the **view/display transform**,
not the place look is injected. A neutral grade is a mathematical identity, leaving the frame unchanged.

The Post panel's Tone tab owns both exposure and the view transform. Keeping these controls together
matches their order in the display pipeline; renderer quality and diagnostic controls remain in the
Render panel.

## A compute pass, in place

The offscreen is an `rgba16f` storage image. The tonemap shader binds it as an `RWTexture2D` (set 0,
binding 0), reads each texel, and writes the mapped value back to the same texel — no second target. An
8×8 thread group covers the image, with a bounds check so edge groups do not write past the extent.

Writing in place means the [render graph](../../frame-and-render-graph/render-graph-overview/) moves
the offscreen through two layouts around the pass, both derived from the declared usage: **Color →
General** before (a storage image is written in `GENERAL`, declared `StorageImageRwCompute`), then
**General → ShaderReadOnly** after, so the present blit can sample it as the viewport image. Neither transition
is written by hand. This pass is the template every other [compute post-process](../compute-post-process-pattern/)
follows; FXAA uses the same read-modify-write shape.

## In the code

| What | File | Symbols |
|---|---|---|
| The shader | `tonemap.slang` | `computeMain`, `Push.exposure`, the `target` RWTexture2D |
| Push + EV→multiplier | `overlay.rs` | `TonemapPush`, `TonemapPush::from_ev` |
| Pass + exposure state | `renderer.rs` | the `tonemap` pass, `Renderer::set_exposure`, `exposure_ev` |
| HDR offscreen format | `pipelines.rs` | `OFFSCREEN_COLOR_FORMAT` (`R16G16B16A16_SFLOAT`) |
| Layout transitions | `render_graph.rs` | `RgUsage::StorageImageRwCompute`, `RgUsage::SampledRead` |
| Exposure control command | `commands_render.rs` | the `set-exposure` command (EV stops → $2^{EV}$) |
| Editor controls | `editor/src/panels/PostProcessPanel.tsx` | `PostProcessPanel`, `onTonemap` |

## Related

- [HDR and exposure](../../lighting-and-brdf/hdr-and-exposure/) — where the linear radiance comes from
- [Compute post-process](../compute-post-process-pattern/) — the shared RMW shape
- [Render graph](../../frame-and-render-graph/render-graph-overview/) — how the layout moves are derived
