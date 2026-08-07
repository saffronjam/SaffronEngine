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

Every [virtual-shadow](../virtual-shadow-maps/) sampler (directional, spot, and point) funnels
into one function, `vsmTilePcf`: a 3×3 grid of hardware comparison taps inside a single resident
atlas page.

## How it works

The atlas is bound as `Sampler2DShadow` behind the layout's immutable comparison sampler: each tap
returns the result of a depth test, not the depth itself. The caller resolves the receiver to a
page and a fractional position within it; `vsmTilePcf` maps that to the page's physical tile and
averages nine `SampleCmpLevelZero` taps stepped one atlas texel apart:

```hlsl
float2 texel = clamp(pageFrac * VSM_PAGE_TEXELS, 1.5, VSM_PAGE_TEXELS - 1.5);
float2 baseUv = (tileBase + texel) / VSM_ATLAS_TEXELS;
sum += atlas.SampleCmpLevelZero(baseUv + float2(dx, dy) / VSM_ATLAS_TEXELS, depth);
```

The sampler compares with `LESS_OR_EQUAL` and filters with `LINEAR`, so the hardware blends the
comparison results of the four texels under each tap. Nine taps give a smooth $[0, 1]$ gradient
across the penumbra.

## The tile gutter

Adjacent atlas tiles belong to unrelated pages, often of different lights, so a filter kernel must
never cross a tile edge. The clamp above keeps the tap centre 1.5 texels inside the tile, which
bounds all nine one-texel-offset taps (plus their linear-filter footprint) within the page. The
cost is that the outermost 1.5 texels of each 128² page filter slightly flatter than the interior.

## Absent information

A receiver can project outside a light's space, behind it, or past its far plane; a page can simply
not be resident. Every such case reads fully lit — the safe default for missing shadow information.
The directional walk has a gentler middle ground first: a missing fine page falls back to a coarser
clip level's valid content before giving up.

## In the code

| What | File | Symbols |
|---|---|---|
| The shared tile filter | `assets/shaders/lighting_common.slang` | `vsmTilePcf` |
| Its callers | `assets/shaders/lighting_common.slang` | `vsmSampleDirectional`, `vsmSampleSpot`, `vsmSamplePoint` |
| The compare sampler object | `crates/rendering/src/descriptors.rs` | `shadow_sampler`, `create_light_layout` (binding 13) |
| Page + atlas texel sizes | `crates/rendering/src/vsm.rs` | `VSM_PAGE_SIZE`, `VSM_ATLAS_SIZE` |

## Related

- [Virtual shadow maps](../virtual-shadow-maps/) — the pages the filter stays inside
- [Directional shadows](../directional-shadows/) — the fine-to-coarse walk that calls it
- [Shadow bias](../shadow-bias/) — the bias the reference depth carries into the compare
