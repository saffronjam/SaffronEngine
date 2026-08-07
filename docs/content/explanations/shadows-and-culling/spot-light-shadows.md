+++
title = 'Spot shadows'
weight = 2
+++

# Spot shadows

Anima shadows one spot light through a projective space of
[virtual shadow pages](../virtual-shadow-maps/): a 16×16 page grid over the light's perspective
frustum. Only the pages that shaded fragments actually project into get rendered; the rest of the
frustum costs nothing.

The scene selects one shadowed spot. `gather_punctual_lights` records the first spot light it
gathers and its index in the punctual-light buffer. Other spot lights still illuminate the scene
unshadowed unless ray-query shadows are active.

## Light transform

The light position comes from the entity's world translation; its authored direction is rotated by
the entity's world rotation. `look_at_up_for_dir` uses world Y as the view's up vector, switching to
world Z when the direction is nearly vertical. The square projection has aspect 1, near plane
`0.05`, far plane `max(range, 0.1)`, and a field of view two degrees beyond the full outer cone,
capped below 180°:

```rust
let fov = (2.0 * light.outer_angle + 2.0).min(179.0).to_radians();
let light_proj = perspective(fov, 1.0, 0.05, light.range.max(0.1));
```

`Lighting::set_spot_shadow` stores the transform, the light index, and the arming gate (under the
master shadow toggle). A changed transform invalidates every resident spot page — the projective
pages are meaningless under a new view.

## Pages under the frustum

The demand pass projects each camera-depth pixel by `spotShadowViewProj`; a receiver inside the
frustum marks the page at `uv = ndc.xy * 0.5 + 0.5` scaled to the 16×16 grid. Dirty spot pages
rasterize in their own page group: the cull chain uses the spot transform as its frustum, and each
page draws with `vsm_page_crop(16, x, y) * spotShadowViewProj` — a crop matrix that maps the page's
NDC sub-rectangle onto the full clip cube, so the page's content fills its 128² atlas tile.

## Sampling

`vsmSampleSpot` projects the fragment by the same transform, guards `w > 0`, the NDC bounds, and
`z` in $(0,1)$, then looks the page up in the table's spot region and takes the shared
[3×3 tile filter](../pcf-filtering/) at depth `ndc.z`. A missing page reads unshadowed — the spot
has no coarser level to fall back to. The fog in-scatter helper `fogPunctualInScatter` samples the
same pages.

## In the code

| What | File | Symbols |
|---|---|---|
| Select the light + build its projection | `crates/assets/src/render_scene.rs` | `gather_punctual_lights`, `look_at_up_for_dir`, `perspective` |
| Store the transform, index, gate | `crates/rendering/src/lighting.rs` | `Lighting::set_spot_shadow`, `spot_shadow_view_proj` |
| Invalidate on movement | `crates/rendering/src/renderer.rs` | `prepare_vsm_frame`, `VsmResidency::invalidate_spot` |
| The per-page crop | `crates/rendering/src/vsm.rs` | `vsm_page_crop`, `VSM_SPOT_PAGES`, `VSM_SPOT_TABLE_BASE` |
| Mark receiver pages | `assets/shaders/vsm_demand.slang` | `computeMain` (spot block) |
| Sample the pages | `assets/shaders/lighting_common.slang` | `vsmSampleSpot`, `fogPunctualInScatter` |

## Related

- [Virtual shadow maps](../virtual-shadow-maps/) — the atlas and residency behind the pages
- [Directional shadows](../directional-shadows/) — the clip-level analogue for the sun
- [Point-light shadows](../point-light-cube-shadows/) — six of these spaces behind a cube mapping
- [Punctual lights](../../lighting-and-brdf/punctual-lights-and-attenuation/) — spot cone and distance attenuation
