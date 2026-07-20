+++
title = 'Directional light'
weight = 3
math = true
+++

# Directional light

A directional light represents a source whose rays are parallel across the scene, such as the sun.
It has a direction but no position, range, or distance attenuation. Every surface therefore receives
the same incoming radiance before the BRDF and visibility terms are applied.

The light is an ordinary scene entity with a `DirectionalLight` component. A fresh scene contains an
editable `Sun` entity, but the renderer does not create a hidden fallback. Removing the component
leaves the scene without direct sunlight.

## Resolving the scene sun

The renderer uses the first `DirectionalLight` found in the scene. If its entity has a `Transform`,
the entity's world rotation rotates the component's authored direction. The result is normalized
before upload; a zero-length direction falls back to `DirectionalLight::DEFAULT_DIRECTION` so shader
normalization remains finite.

| Field | Default | Effect |
|---|---:|---|
| `direction` | `(-0.5, -1.0, -0.3)` | World-space direction the light travels |
| `color` | `(1, 1, 1)` | Direct-light color |
| `intensity` | `1.0` | Direct radiance multiplier |
| `ambient` | `0.15` | Grayscale flat-ambient fallback |
| `volumetric_scattering` | `1.0` | Sun in-scatter multiplier in volumetric fog |
| `cast_volumetric_shadow` | `true` | Applies the directional shadow map to fog in-scatter |

The frame upload stores the travel direction in `direction_ambient.xyz` and color plus intensity in
`color_intensity`. Zero intensity represents the no-sun case while retaining a valid direction for
the sky and lighting math.

## Surface lighting

The fragment shader negates the stored travel direction to obtain the direction from the surface
toward the light. It then evaluates the same
[Cook-Torrance BRDF](../cook-torrance-brdf/) used by punctual lights:

```hlsl
float3 lDir = -normalize(globals.directionAmbient.xyz);
float3 lo = brdf(
    n,
    v,
    lDir,
    albedo,
    metallic,
    roughness,
    globals.colorIntensity.rgb * globals.colorIntensity.a
) * shadow;
```

Unlike a [punctual light](../punctual-lights-and-attenuation/), the directional radiance has no range
window or spotlight cone. Surface orientation, material response, and visibility provide all
per-fragment variation.

## Visibility

The directional shadow pass renders a 2048×2048 depth map from an orthographic light view fitted to
the scene bounds. `pcfShadow` projects the surface into that map and averages a 3×3 comparison kernel.
Samples outside the map or beyond its far plane are lit.

When inline ray-query shadows run, `evalLighting` traces toward the sun with a maximum distance of
`1e4` instead of sampling the map. Opaque surfaces can also multiply the selected visibility by the
screen-space contact-shadow map. The contact term supplies short-range detail while the map or ray
query handles larger occluders.

## Fog and ambient

Volumetric fog calls `fogDirectionalInScatter` with the same color, intensity, and direction. The
light's `volumetric_scattering` scales this contribution. When `cast_volumetric_shadow` is true and a
directional shadow map is available, the fog sample uses that map to form shadowed shafts.

The `ambient` field does not change the direct term. When IBL is disabled and the environment does
not supply sky ambient, the renderer expands the scalar into a grayscale flat-ambient value. If the
environment enables `use_sky_for_ambient`, its `ambient_color * ambient_intensity` replaces the
component scalar. [IBL](../ibl-ambient-term/) ignores both flat fallbacks while it is enabled.

## In the code

| What | File | Symbols |
|---|---|---|
| Authored component | `scene/src/component.rs` | `DirectionalLight`, `DirectionalLight::DEFAULT_DIRECTION` |
| Scene resolution | `assets/src/render_scene.rs` | `gather_directional_light`, `DirectionalResolved` |
| Frame upload | `rendering/src/lighting.rs` | `SceneLighting`, `Lighting::set_scene_lighting`, `LightUbo` |
| Surface and fog shading | `assets/shaders/lighting.slang` | `evalLighting`, `fogDirectionalInScatter`, `pcfShadow` |
| Starter sun | `scene/src/starter.rs` | `seed_starter_scene` |

## Related

- [Light components](../light-components/) compares directional, point, and spot components.
- [Cook-Torrance BRDF](../cook-torrance-brdf/) defines the shared direct-light response.
- [Directional shadows](../../shadows-and-culling/directional-shadows/) covers the orthographic shadow pass.
- [IBL ambient term](../ibl-ambient-term/) explains the baked ambient path and flat fallback.
