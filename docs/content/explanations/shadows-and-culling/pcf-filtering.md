+++
title = 'PCF filtering'
weight = 4
math = true
+++

# PCF filtering

Percentage-closer filtering averages several depth-comparison samples around a lookup point so a
shadow edge fades over a few texels instead of snapping from lit to dark. The result is a
visibility factor in $[0, 1]$ rather than a single lit-or-shadowed bit. The technique comes from
Reeves, Salesin, and Cook (SIGGRAPH 1987); GPU Gems'
[Shadow Map Antialiasing](https://developer.nvidia.com/gpugems/gpugems/part-ii-lighting-and-shadows/chapter-11-shadow-map-antialiasing)
is the practical treatment.

The 2D shadow maps (directional and spot) share one `pcfShadow` function: a 3×3 grid of hardware
comparison taps.

## How it works

The maps are bound as `Sampler2DShadow`, a comparison sampler: each tap returns the result of a
depth test, not the depth itself. `pcfShadow` projects the world position into the light's clip
space, takes `ndc.z` as the reference depth, and averages nine `SampleCmp` taps stepped one texel
apart (`texel = 1/2048`, matching `SHADOW_MAP_SIZE`):

```hlsl
sum += map.SampleCmp(uv + float2(x, y) * texel, ndc.z);
```

The sampler compares with `LESS_OR_EQUAL` and filters with `LINEAR`, so the hardware blends the
comparison results of the four texels under each tap. Each tap is a small percentage-closer
filter on its own. Nine of them give a smooth $[0, 1]$ gradient across the penumbra.

## Off-map and beyond-far cases

A fragment can project outside the map, past its far plane, or behind the light. None of these
positions carry valid shadow information, so an early-out guard handles each case:

| Condition | Meaning | Result |
|---|---|---|
| `clip.w <= 0` | behind the light | lit |
| `uv` outside $[0,1]^2$ | outside the light frustum | lit |
| `ndc.z > 1` | past the far plane | lit |

Treating absent information as lit is the safe default. Shadowing these fragments instead would
draw a hard black band at the frustum edge and put everything past the far plane in shadow. The
sampler applies the same policy in hardware: `CLAMP_TO_BORDER` with an opaque-white border makes
a stray off-map tap compare against depth 1.0 and pass as lit.

The cost is that geometry genuinely outside the frustum is never shadowed. That is why the
directional frustum is [fit to the whole scene](../directional-shadows/).

## Trade-offs

A fixed 3×3 kernel is the cheapest filter that visibly helps. It hides the texel grid, but the
penumbra width is constant: the softening does not grow with occluder distance, so shadows do not
contact-harden the way
[percentage-closer soft shadows](https://developer.download.nvidia.com/shaderlibrary/docs/shadow_PCSS.pdf)
do. A wider or jittered kernel smooths more and costs more taps per fragment.

The point light does not use this path: its cube map stores distance and does a hard comparison,
described in [point shadows](../point-light-cube-shadows/).

## In the code

| What | File | Symbols |
|---|---|---|
| The 3×3 comparison filter | `assets/shaders/lighting.slang` | `pcfShadow` |
| Comparison samplers | `assets/shaders/lighting.slang` | `shadowMap`, `spotShadowMap` (`Sampler2DShadow`) |
| Where it's called | `assets/shaders/lighting.slang` | `evalLighting`, `punctual` |
| Map size (texel step) | `crates/rendering/src/lighting.rs` | `SHADOW_MAP_SIZE` |
| The compare sampler object | `crates/rendering/src/descriptors.rs` | `shadow_sampler`, `create_shadow_sampler` |

## Related

- [Directional shadows](../directional-shadows/) — the map this filters
- [Spot-light shadows](../spot-light-shadows/) — the other map on the same path
- [Shadow bias](../shadow-bias/) — the bias the reference depth carries into the compare
- [Point shadows](../point-light-cube-shadows/) — the hard-comparison alternative for points
