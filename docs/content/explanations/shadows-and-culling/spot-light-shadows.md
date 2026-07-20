+++
title = 'Spot shadows'
weight = 2
+++

# Spot shadows

Anima renders a spot light with [shadow mapping](https://doi.org/10.1145/800248.807402): the scene is drawn into a depth image from the light's position, and shaded fragments compare their projected depth with that image. A perspective projection follows the light's cone, unlike the orthographic projection used for the directional light.

The scene owns one 2D spot-shadow map. `gather_punctual_lights` selects the first spot light it gathers and records that light's index in the punctual-light buffer. Other spot lights still illuminate the scene, but they do not sample this map.

## Light transform

The light position comes from the entity's world translation. Its authored direction is rotated by the entity's world rotation and normalized. `look_at_up_for_dir` uses world Y as the view's up vector, switching to world Z when the direction is nearly vertical so the view basis remains defined.

The square projection has aspect ratio 1, near plane `0.05`, and a far plane of `max(range, 0.1)`. Its field of view includes two degrees beyond the full outer cone and is capped below 180 degrees:

```rust
let fov = (2.0 * light.outer_angle + 2.0).min(179.0).to_radians();
let light_proj = perspective(fov, 1.0, 0.05, light.range.max(0.1));
```

For the default spot light, `outer_angle = 30` degrees and `range = 10`, so the shadow projection uses a 62-degree field of view with near and far planes at `0.05` and `10`. The two-degree margin keeps the cone boundary inside the depth image.

## Depth pass

`Lighting::set_spot_shadow` stores the perspective view-projection, selected light index, and pass gate. The master shadow setting controls that gate. When it is active, `Renderer::record_scene_graph` imports the spot map through an external-layout slot and adds the `spot-shadow` pass.

The target is a single-mip, 2,048 by 2,048 `D32_SFLOAT` image. `add_shadow_pass` clears it and calls `record_shadow_depth`, the same vertex-only draw used by the directional map. It binds the per-frame instance data, pushes the spot view-projection, draws the scene batches, and applies the shared constant and slope-scaled [depth bias](../shadow-bias/).

```mermaid
flowchart LR
    A[First spot light] --> B[Perspective view-projection]
    B --> C[spot-shadow depth pass]
    C -->|DepthWrite| D[2D depth map]
    D -->|SampledRead| E[Punctual-light shading]
```

The scene pass declares the map as `SampledRead`. The [render graph](../../frame-and-render-graph/render-graph-overview/) therefore orders the depth write before shading and transitions the image to its sampled layout. The external slot carries the resulting layout between frames when no spot-shadow pass runs.

## Selecting and sampling the light

The light UBO stores the selected buffer index in `spot_shadow.x` and the map gate in `spot_shadow.y`. `punctual` checks both values inside the spot-light branch:

```hlsl
if (globals.spotShadow.y != 0 && lightIndex == globals.spotShadow.x)
{
    shadow = pcfShadow(spotShadowMap, globals.spotShadowViewProj, worldPos);
}
```

`pcfShadow` projects the surface position into the spot map and averages a 3 by 3 grid of comparison samples. Positions behind the light, outside the map, or beyond the far plane return fully lit. The resulting visibility multiplies the light's distance attenuation and cone falloff before the BRDF. [PCF filtering](../pcf-filtering/) describes the comparison sampler and kernel.

The `SpotLight::cast_volumetric_shadow` field controls only the map's effect on volumetric-fog in-scatter. It does not disable the selected spot's surface shadow. In the fog path, `fogPunctualInScatter` samples the same map and transform only when that field is enabled.

## In the code

| What | File | Symbols |
|---|---|---|
| Select the light and build its projection | `engine/crates/assets/src/render_scene.rs` | `gather_punctual_lights`, `SpotShadow`, `look_at_up_for_dir`, `perspective` |
| Spot-light defaults and fog control | `engine/crates/scene/src/component.rs` | `SpotLight`, `SpotLight::default` |
| Store the transform, index, and gate | `engine/crates/rendering/src/lighting.rs` | `LightUbo`, `Lighting::set_spot_shadow`, `Lighting::spot_shadow_pending`, `Lighting::spot_shadow_view_proj` |
| Allocate the depth image | `engine/crates/rendering/src/targets.rs` | `Targets::new`, `shadow_depth_map` |
| Add and record the pass | `engine/crates/rendering/src/renderer.rs` | `Renderer::record_scene_graph`, `Renderer::add_shadow_pass`, `"spot-shadow"` |
| Draw the depth batches | `engine/crates/rendering/src/scene_pass.rs` | `record_shadow_depth` |
| Surface and fog sampling | `engine/assets/shaders/lighting.slang` | `punctual`, `pcfShadow`, `fogPunctualInScatter` |

## Related

- [Directional shadows](../directional-shadows/) — the orthographic variant of the shared depth path
- [PCF filtering](../pcf-filtering/) — the comparison kernel used at the surface
- [Shadow bias](../shadow-bias/) — the rasterizer offset applied while filling the map
- [Punctual lights](../../lighting-and-brdf/punctual-lights-and-attenuation/) — spot cone and distance attenuation
- [Volumetric fog](../../screen-space-and-post/height-fog/) — fog in-scatter that can sample the same map
