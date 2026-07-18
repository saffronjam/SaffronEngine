+++
title = 'Contact shadows'
weight = 3
math = true
+++

# Contact shadows

Contact shadows recover short-range occlusion that a directional shadow map or long ray query can
miss. For each opaque pixel, a compute shader marches a short screen-space ray toward the sun and
compares it with the thin G-buffer depth. The resulting factor darkens fine contacts while the main
directional-shadow method supplies large-scale visibility.

The pass affects only the directional direct-light term. It does not shadow punctual lights,
indirect lighting, or transparent fragments. Transparent shading omits screen-space terms because its
screen coordinate addresses the opaque surface behind the fragment.

## Ray march

The thin G-buffer stores a view-space normal in RGB and view-space Z in alpha. The shader reconstructs
the starting position $p$, normalizes the view-space direction to the sun $l$, and offsets the origin
by `0.02` along the normal. With per-pixel interleaved gradient noise $j$, sample $i$ is

$$
s_i = p + 0.02n + 0.2l\frac{i-j}{N}, \qquad i = 1, \ldots, N.
$$

`N` comes from the active [render-quality tier](../render-quality-tiers/): `high` uses 12 steps and
`ultra` uses 16. The noise offsets the march within its first step, replacing a uniform band pattern
with a stable screen-space dither.

Each $s_i$ is projected through the camera projection matrix. The march stops when the sample passes
behind the camera or leaves the viewport. A G-buffer value with view Z greater than `-1e-4` represents
background and cannot occlude the ray.

## Depth test

Let $z_s$ be the sampled G-buffer depth and $z_r$ the marched ray depth. Both values are negative in
front of the camera. The shader computes

$$
\Delta z = z_s - z_r.
$$

A hit requires $0.01 < \Delta z < 0.1$. The lower bound rejects near-equal self-intersections. The
upper bound treats a stored depth sample as a finite-thickness surface instead of an infinitely deep
column. This matters because one screen-space depth value cannot reveal what lies behind its visible
surface, a general limitation of screen-space ray tests described by
[McGuire and Mara](https://jcgt.org/published/0003/04/04/paper.pdf).

The first hit ends the loop. The `r8` output stores `0` for occluded and `1` for lit. Pixels with no
hit, including background pixels, store `1`.

## Lighting integration

Opaque mesh shading first evaluates directional visibility through a ray query when enabled,
otherwise through the directional shadow map when available. It then multiplies the contact factor:

```hlsl
if (globals.screenFlags.x != 0 && !translucent)
{
    shadow *= contactMap.SampleLevel(screenUv, 0.0).r;
}
```

Multiplication preserves existing occlusion: a zero from the main shadow remains zero, while a lit
pixel can receive short-range contact occlusion. `screenFlags.x` is set only when the contact pipeline
and target are ready and the active quality tier enables the effect.

The renderer transforms the incoming world-space sun direction once per frame. `Ssao::set_camera`
negates it to obtain the direction toward the light, transforms that vector into view space, and
normalizes it. `ContactPush` carries that direction with the projection matrices and march parameters.

## Example

Select the standard contact-shadow march and inspect the resolved settings:

```sh
sa set-render-quality high
sa get-render-quality
```

`high` enables contact shadows with 12 steps. `ultra` raises the march to 16 steps. `low` and
`medium` disable the pass, so no contact-shadow dispatch or lighting sample runs.

## In the code

| What | File | Symbols |
|---|---|---|
| Ray march | `engine/assets/shaders/contact.slang` | `computeMain`, `ign`, `viewPosFromUv`, `Push` |
| Lighting application | `engine/assets/shaders/lighting.slang` | `contactMap`, `screenFlags.x`, `evalLighting` |
| Push constants and camera transform | `engine/crates/rendering/src/ssao.rs` | `ContactPush`, `Ssao::contact_push`, `Ssao::set_camera` |
| Pass scheduling | `engine/crates/rendering/src/renderer.rs` | `add_screen_space_passes`, `contact-shadows`, `contact_map` |
| Per-view target and descriptors | `engine/crates/rendering/src/view_target.rs` | `ViewTarget::contact_map`, `contact_set`, `mesh_set` |
| Tier settings | `engine/crates/rendering/src/quality.rs` | `QualityTier`, `RenderQuality::contact_enabled`, `contact_steps` |
| Control commands | `engine/crates/control/src/commands_render.rs` | `set-render-quality`, `get-render-quality`, `render_quality_result` |

## Related

- [Thin G-buffer](../thin-gbuffer/): provides view-space normal and depth
- [Directional shadows](../../shadows-and-culling/directional-shadows/): supplies the main visibility term
- [Render quality tiers](../render-quality-tiers/): select the contact-shadow enable flag and step count
