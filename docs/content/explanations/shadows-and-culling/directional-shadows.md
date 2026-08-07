+++
title = 'Directional shadows'
weight = 1
math = true
+++

# Directional shadows

A directional shadow treats the light as parallel rays from infinity, like the sun. Anima shadows
it through eight camera-snapped clip levels of [virtual shadow pages](../virtual-shadow-maps/):
concentric square windows on the light plane, each twice the extent of the previous, so texel
density concentrates near the viewer without a cascade-split heuristic.

## The clip-level space

`VsmDirectionalSpace::build` derives an orthonormal basis from the sun direction and projects the
camera position onto the light plane. Level $k$ is a window of extent $32 \cdot 2^k$ metres, snapped
to its own page grid:

$$
s = \left\lfloor \frac{c - e/2}{e/32} \right\rfloor, \qquad e = 32 \cdot 2^k,
$$

where $c$ is the camera in light-plane coordinates and $e/32$ is one page in metres — level-0 pages
are 1 m, level-7 pages 128 m. Snapping to whole pages keeps a page's world content stable while the
camera moves inside it; a level whose snap changes invalidates its resident pages.

Depth is shared across levels: a fixed ±512 m span along the light direction, centred on the camera.
`window_view_proj` builds each page's ortho transform with exactly the `depth01` mapping the sampler
inverts, so rasterized depth and compared depth agree by construction.

## Sampling

`vsmSampleDirectional` rotates the world position into the light basis and walks the levels fine to
coarse. The first level whose window contains the receiver and holds a resident page wins; the
sampler takes a [3×3 comparison filter](../pcf-filtering/) inside that page's atlas tile. A missing
fine page falls back to a coarser level's valid content — never to a leak or a stale texel.

```text
for level in 0..8:
    local = (lightPos.xy - window.origin) / window.extent
    if local outside [0,1): continue
    entry = pageTable[level*1024 + page.y*32 + page.x]
    if resident(entry): return vsmTilePcf(entry, frac(local*32), depth01)
return 1.0
```

Receivers demand the level whose texel density matches their screen footprint, so the walk almost
always hits its first candidate. `counts.y` arms sun shadowing, and the fog in-scatter helper
samples the same pages through `fogDirectionalInScatter`.

## In the code

| What | File | Symbols |
|---|---|---|
| Build the snapped space | `crates/rendering/src/vsm.rs` | `VsmDirectionalSpace::build`, `page_view_proj`, `depth01` |
| Arm sun shadowing | `crates/rendering/src/lighting.rs` | `Lighting::set_directional_shadow` |
| Publish windows to the UBO | `crates/rendering/src/renderer.rs` | `prepare_vsm_frame`, `set_frame_vsm` |
| Sample the clip levels | `assets/shaders/lighting_common.slang` | `vsmSampleDirectional`, `fogDirectionalInScatter` |
| Demand by footprint | `assets/shaders/vsm_demand.slang` | `computeMain` (level pick) |

## Related

- [Virtual shadow maps](../virtual-shadow-maps/) — the atlas, residency, and demand behind the levels
- [PCF filtering](../pcf-filtering/) — the in-tile comparison kernel
- [Shadow bias](../shadow-bias/) — the depth bias applied while pages rasterize
- [Spot-light shadows](../spot-light-shadows/) — the projective analogue for one spot
