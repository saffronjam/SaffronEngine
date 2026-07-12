+++
title = 'Directional light'
weight = 3
math = true
+++

# Directional light

A directional light is a parallel light source with a direction but no position and no falloff,
modelling the sun. It is the first term the fragment shader accumulates, evaluated through the
same [BRDF](../cook-torrance-brdf/) as every punctual light, then attenuated by its shadow.

It is an ordinary scene entity — a `DirectionalLight` component you add, edit, and delete like
any other. The scene shades through the **first** one; a scene with no directional light has **no
direct sun** at all (only the sky and [IBL ambient](../ibl-ambient-term/) light it, so it can go
genuinely dark). A freshly created scene seeds a real "Sun" entity so there is something to light
it by default — delete it and the direct sun is gone; there is no hidden fallback sun behind it.

## A single direction, no attenuation

A directional light has no distance, so it has no attenuation and no cone. The incoming radiance
is the light's color times its intensity, identical for every fragment. The only per-fragment
work is the BRDF and the shadow. The shader stores the light's travel direction in
`globals.directionAmbient.xyz` and negates it to get the direction toward the light, which is
what the BRDF expects:

```hlsl
float3 lDir = -normalize(globals.directionAmbient.xyz);
float3 lo = brdf(n, v, lDir, albedo, metallic, roughness,
                 globals.colorIntensity.rgb * globals.colorIntensity.a) * shadow;
```

The `radiance` argument is `color * intensity` directly. A
[punctual light](../punctual-lights-and-attenuation/) passes the same slot as
`color * intensity * attenuation * cone`: the same function with extra factors.

## Shadow: map, contact, or ray

The `shadow` scalar multiplies the whole direct term. The directional light is the one light
with a full shadowing stack, selected by flags in the global constants:

- **Shadow map.** When `counts.y` is set, `pcfShadow` projects the fragment into the sun's
  light-space `shadowViewProj`, does a 3×3 PCF comparison against a 2048² depth map, and
  returns a `[0, 1]` visibility. Off-map and beyond-far-plane samples count as lit.
- **Contact shadows.** When `screenFlags.x` is set, a screen-space contact-shadow factor
  multiplies on top, adding fine detail the coarse map misses.
- **Ray-traced.** When RT shadows are enabled (`pointShadowMeta.z`), the map path is skipped
  and a long ray-query traces toward the sun instead (`rayQueryShadow`, `1e4` max distance).

```hlsl
float shadow = 1.0;
if (globals.pointShadowMeta.z != 0)
    shadow = rayQueryShadow(input.worldPos, lDir, 1e4);   // RT shadow
else if (globals.counts.y != 0)
    shadow = pcfShadow(shadowMap, globals.shadowViewProj, input.worldPos);
if (globals.screenFlags.x != 0)
    shadow *= contactMap.SampleLevel(screenUv, 0.0).r;    // fine contact detail
```

Contact shadows are directional-only in v1; the map and ray paths are mutually exclusive.

## The ambient companion

The directional component is the only one carrying an `ambient` scalar
(`directionAmbient.w`). When [IBL](../ibl-ambient-term/) is off, that scalar is the flat
indirect fallback: a constant fill so unlit surfaces are not pure black. It is not part of the
direct term above; it is added later with the rest of the ambient. With no directional light in
the scene, there is no scalar fallback either — the ambient comes only from the sky/IBL.

## In the code

| What | File | Symbols |
|---|---|---|
| Direct term | `engine/assets/shaders/lighting.slang` | `evalLighting` — `lDir`, the `brdf` call |
| Shadow map PCF | `engine/assets/shaders/lighting.slang` | `pcfShadow`, `globals.shadowViewProj` |
| Contact + RT shadow | `engine/assets/shaders/lighting.slang` | `contactMap`, `rayQueryShadow` |
| Direction + ambient upload | `engine/crates/rendering/src/lighting.rs` | `Lighting::set_scene_lighting` — `LightUbo::direction_ambient`, `LightUbo::color_intensity` |
| Gather the sun from the scene (`None` = no sun) | `engine/crates/assets/src/render_scene.rs` | `gather_directional_light`, `DirectionalResolved` |
| Seed the starter Sun on a fresh scene | `engine/crates/scene/src/starter.rs` | `seed_starter_scene` |

> [!TIP]
> `globals.directionAmbient.xyz` is the direction the light travels, not the direction toward
> the sun. The shader negates it. A sun pointing straight down is `direction = (0, -1, 0)`.

## Related

- [Cook-Torrance BRDF](../cook-torrance-brdf/) — the shared shading model
- [Light components](../light-components/) — where the direction and intensity come from
- [Punctual lights and attenuation](../punctual-lights-and-attenuation/) — the same BRDF with falloff and a cone
- [IBL ambient term](../ibl-ambient-term/) — what replaces the flat `ambient` scalar
