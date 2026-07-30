+++
title = 'HDR & exposure'
weight = 8
math = true
+++

# HDR & exposure

High dynamic range (HDR) rendering carries light intensities with no upper bound, keeping bright
highlights distinct rather than clamping them to white. Exposure is the single multiplier that
selects how much of that unbounded range maps into the displayable band.

A physically based renderer accumulates radiance from many sources, and a bright light or a tight
specular highlight can push a pixel far past 1.0. That signal must survive untruncated through
shading and live in a floating-point buffer until a final step compresses it for the display.

## How it works

Shading runs in linear HDR: light contributions are summed as unbounded linear radiance and
written to a floating-point color target. Exposure scales that radiance by one multiplier, and a
final tonemap operator maps the result into the $[0, 1]$ range a display can show. Each step is
deferred to the end of the frame so the full range is preserved until the last possible moment.

## The floating-point format

The scene pass writes into an offscreen color target whose format is `rgba16f`
(`R16G16B16A16_SFLOAT`). Half-float gives 16 bits per channel and a floating exponent, so values
above 1.0 survive without clamping. An `rgba8` target would saturate every overbright pixel to
white the moment it was written, discarding the range tonemapping exists to compress. The HDR
format defers that decision to the end of the frame.

## Linear radiance

Every light contribution the fragment shader accumulates is linear radiance:

$$
L_o = \sum_{\text{lights}} \big(f_\text{diffuse} + f_\text{specular}\big)\,L_i\,(n\cdot l) \;+\; \text{ambient} \;+\; \text{emissive}
$$

The shading half in `lighting.slang` applies no gamma, no clamp, and no tonemap. Summing light contributions is plain
addition, which is physically meaningful only in linear space. Encoding (gamma) and range
compression (tonemap) both belong to the display step, not the shading step.

## Exposure in EV stops

Before tonemapping, the HDR value is scaled by a single linear exposure multiplier. The editor
and control plane work in EV stops, and the engine raises 2 to that stop (`exposure_ev.exp2()`):

$$
\text{exposure} = 2^{EV}
$$

So $+1$ EV doubles brightness and $-1$ EV halves it, the photographic convention. The multiplier
is a push constant on the tonemap shader. It is the one knob that decides how much of the
unbounded HDR range lands in the visible $[0, 1]$ band.

## The tonemap step

A compute pass always runs on the offscreen before the present blit samples it. It reads each HDR texel,
applies exposure, a Reinhard curve, and gamma, then writes the result back in place:

```hlsl
float3 hdr    = color.rgb * push.exposure;     // exposure
float3 mapped = hdr / (hdr + 1.0);             // Reinhard
mapped = pow(mapped, float3(1.0 / 2.2));       // gamma 2.2
```

The full operator — why it is a compute pass, how the render graph moves the offscreen through
its layouts — is covered in
[tonemapping and exposure](../../screen-space-and-post/tonemap-and-exposure/). The lighting side
produces unbounded linear radiance and relies on that step to land it on screen.

## In the code

| What | File | Symbols |
|---|---|---|
| HDR offscreen format | `engine/crates/rendering/src/pipelines/` | `OFFSCREEN_COLOR_FORMAT` (`R16G16B16A16_SFLOAT`) |
| Linear radiance accumulation | `engine/assets/shaders/lighting.slang` | `evalLighting` — `lo + ambient + emissive` |
| Exposure + tone curve | `engine/assets/shaders/tonemap.slang` | `computeMain`, `Push.exposure` |
| EV → $2^{EV}$ | `engine/crates/rendering/src/overlay.rs` | `TonemapPush::from_ev` (`exposure_ev.exp2()`) |
| Exposure setter + state | `engine/crates/rendering/src/renderer/` | `Renderer::set_exposure`, `Renderer::exposure_ev` |
| `set-exposure` control command | `engine/crates/control/src/commands_render/` | the `set-exposure` registration |

> [!TIP]
> Exposure is a plain multiply applied before the Reinhard curve, so it shifts which part of
> the HDR range the curve compresses. Cranking exposure does not clip — Reinhard maps
> $[0, \infty)$ into $[0, 1)$ — it rolls more of the highlight band into the visible portion.

## Related

- [Cook-Torrance BRDF](../cook-torrance-brdf/) — where the linear radiance is produced
- [Tonemapping and exposure](../../screen-space-and-post/tonemap-and-exposure/) — the operator and its render-graph layout moves
- [IBL ambient term](../ibl-ambient-term/) — the other HDR contributor on the offscreen
