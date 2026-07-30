+++
title = 'GTAO'
weight = 2
math = true
+++

# GTAO

Anima's GTAO pass estimates contact-scale ambient visibility from the thin G-buffer. Nearby depth
samples above a surface's tangent plane reduce a scalar visibility factor. Mesh shading uses that
factor for indirect diffuse lighting, screen-space bounce lighting, and roughness-aware specular
occlusion; direct lights keep their own shadow terms.

The name refers to Ground-Truth Ambient Occlusion, introduced by Jimenez and colleagues in
[*Practical Realtime Strategies for Accurate Indirect Occlusion*](https://research.activision.com/publications/archives/atvi-tr-16-01practical-realtime-strategies-for-accurate-indirect-occlusion).
Anima's shader is a compact GTAO-lite variant: it uses a fixed hemisphere sample pattern and spatial
depth-aware filtering rather than the paper's complete horizon and spatio-temporal formulation.

## Trace

The pass runs at half the viewport width and height. Each output texel samples the full-resolution
[thin G-buffer](../thin-gbuffer/) at the corresponding normalized coordinate, reconstructing
view-space position $p$ from view Z and inverse projection. RGB supplies the view-space normal $n$.

The sampling radius is `0.5` view units. The shader converts it to screen space using

$$
r_\text{screen} = \operatorname{clamp}
\left(\frac{0.5}{\max(-z, 0.1)},\ 0.02,\ 0.25\right).
$$

Four azimuth directions each take six steps from the pixel. A coordinate hash rotates all four
directions by a per-pixel fraction $q$:

$$
\phi_k = \frac{k + q}{4}\,2\pi, \qquad k = 0,1,2,3.
$$

For a valid sample position $s$, let $d=s-p$ and $L=\lVert d\rVert$. Its contribution is

$$
o_s = \max\!\left(n\cdot\frac{d}{L}-0.02,\ 0\right)
\operatorname{saturate}\!\left(1-\frac{L-0.5}{0.5}\right).
$$

The `0.02` angular bias suppresses nearly coplanar self-occlusion. Range weight stays one through the
first `0.5` view units, then falls to zero by `1.0`. Off-screen taps and G-buffer background taps add
zero while still occupying their slot in the fixed 24-sample average.

The output visibility is

$$
\operatorname{ao}=\operatorname{saturate}\left(1-3\frac{\sum_s o_s}{24}\right).
$$

Both the radius and strength are fixed renderer settings carried through `GtaoPush`. Background
pixels store one, meaning fully open.

## Bilateral upsample

The trace writes half-resolution `ao_raw` in `r8`. A second compute pass produces the
full-resolution `ao_map` with a 5x5 filter. Its spatial weight for offset $(x,y)$ is
$\exp(-(x^2+y^2)/4)$, multiplied by the depth similarity
$\exp(-4\lvert z_s-z_c\rvert)$.

`ao_raw` uses a linear sampler, so the same pass upsamples the half-resolution signal. The depth term
reduces bleeding across view-Z discontinuities. The filter does not compare normals and has no
temporal history.

## Lighting use

Opaque mesh shading multiplies the sampled map by the material's authored occlusion value. In the IBL
path, that combined factor darkens contact-scale indirect diffuse and SSGI, and enters the
roughness-aware specular-AO expression:

```hlsl
float ao = surf.occlusion;
if (globals.counts.w != 0 && !translucent)
{
    ao *= aoMap.SampleLevel(screenUv, 0.0).r;
}
float specAO = saturate(pow(ndotv + ao, exp2(-16.0 * roughness - 1.0)) - 1.0 + ao);
```

Transparent fragments do not sample the map because their screen coordinate identifies the opaque
G-buffer surface behind them. The `ssao` debug view displays `ao_map` directly as grayscale.

## Example

Enable the screen-space ambient-occlusion stack and inspect its resolved state:

```sh
sa set-render-quality medium
sa get-render-quality
```

`low` disables GTAO. `medium`, `high`, and `ultra` enable the same 4-by-6 trace; the higher tiers
change SSGI and contact-shadow sampling, not GTAO's fixed sample count.

## In the code

| What | File | Symbols |
|---|---|---|
| Half-resolution trace | `engine/assets/shaders/gtao.slang` | `computeMain`, `viewPosFromUv`, `sliceCount`, `stepCount`, `bias` |
| Bilateral upsample | `engine/assets/shaders/ao_blur.slang` | `computeMain`, `aoRaw`, `gbuffer`, `aoOut` |
| Push constants and defaults | `engine/crates/rendering/src/ssao.rs` | `GtaoPush`, `Ssao::gtao_push`, `radius`, `strength` |
| Pass scheduling | `engine/crates/rendering/src/renderer/` | `add_screen_space_passes`, `gtao`, `ao-blur` |
| Targets and descriptors | `engine/crates/rendering/src/view_target/` | `ao_raw`, `ao_map`, `gtao_set`, `ao_blur_set` |
| Lighting integration | `engine/assets/shaders/lighting.slang` | `aoMap`, `counts.w`, `specAO`, `evalLighting` |
| Quality control | `engine/crates/rendering/src/quality.rs`, `engine/crates/control/src/commands_render/` | `gtao_enabled`, `set-render-quality`, `get-render-quality` |

## Related

- [Thin G-buffer](../thin-gbuffer/): provides view-space normal and depth
- [Image-based lighting](../../image-based-lighting/): consumes the ambient-visibility factor
- [Render quality tiers](../render-quality-tiers/): toggle the GTAO pass with the screen-space stack
