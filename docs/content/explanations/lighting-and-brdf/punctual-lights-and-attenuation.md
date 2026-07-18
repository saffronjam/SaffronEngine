+++
title = 'Punctual lights'
weight = 4
math = true
+++

# Punctual lights

A punctual light emits from one world-space position. A point light radiates in every direction,
while a spot light restricts the same emission to a cone. Both use distance attenuation and a hard
range so their influence remains spatially bounded.

Entities need a `Transform` together with `PointLight` or `SpotLight` to enter the render light list.
The world translation supplies the position. For a spot, the entity's world rotation also rotates
the component direction before upload.

## Packed light data

Every point and spot becomes one 64-byte `GpuLight` record. Point records set `direction_type.w` to
zero; spot records set it to one and store the cone cosines in `spot_cos.xy`.

| Property | Point default | Spot default |
|---|---:|---:|
| Intensity | `5.0` | `5.0` |
| Range | `10.0` | `10.0` |
| Inner half-angle | n/a | `20°` |
| Outer half-angle | n/a | `30°` |
| Volumetric scattering | `1.0` | `1.0` |
| Cast volumetric shadow | `true` | `true` |

The per-frame storage buffer grows when the scene exceeds its capacity. The
[cluster cull](../clustered-forward/) uses each light's position and range as a sphere, and the mesh
fragment loops only the indices assigned to its froxel. Disabling clustered lighting makes the
fragment loop the full punctual-light buffer.

## Distance attenuation

The attenuation follows the windowed inverse-square form described in
[Real Shading in Unreal Engine 4](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf).
It preserves inverse-square behavior near the source and reaches exactly zero at `range`:

$$
A(d,r)=\frac{1}{\max(d^2,10^{-4})}
\left[\operatorname{sat}\left(1-\left(\frac{d}{r}\right)^4\right)\right]^2.
$$

```hlsl
public float distanceAttenuation(float dist, float range)
{
    float invSquare = 1.0 / max(dist * dist, 0.0001);
    float t = saturate(1.0 - pow(dist / range, 4.0));
    return invSquare * t * t;
}
```

The denominator floor keeps the value finite at the source. The smooth range window gives the light
compact support, allowing the cluster cull to reject froxels outside the range sphere. `punctual`
also returns before BRDF work when the surface distance exceeds `range`.

## Spotlight cone

A spot compares its rotated aim with the direction from the light to the surface. The CPU converts
the authored half-angles from degrees to cosines, and the shader interpolates between them:

```hlsl
float3 spotDir = normalize(lt.directionType.xyz);
float cosAngle = dot(spotDir, -l);
float cone = smoothstep(lt.spotCos.y, lt.spotCos.x, cosAngle);
```

`spot_cos.x` is the inner cosine and `spot_cos.y` is the outer cosine. The cone is one inside the
inner angle, zero outside the outer angle, and follows a smooth Hermite ramp through the penumbra. A
point light keeps the cone factor at one.

## Surface contribution

`punctual` multiplies light color and intensity by attenuation, cone, and visibility. The result is
incoming radiance for the shared [Cook-Torrance BRDF](../cook-torrance-brdf/):

```hlsl
float3 radiance = lt.colorIntensity.rgb * lt.colorIntensity.a
                * attenuation * cone * shadow;
return brdf(n, v, l, albedo, metallic, roughness, radiance);
```

The map-based path shadows the first spot light with a 2048×2048 perspective depth map. It shadows
the first point light with 512×512 static and dynamic distance cubes, taking the nearer stored depth.
When ray-query shadows run, every punctual surface contribution traces toward its light instead.

For opaque surfaces, [ReSTIR](../../global-illumination-and-raytracing/restir-passes/) can replace the
punctual loop with one selected, visibility-tested diffuse sample per pixel. Transparent surfaces
continue through `punctual` because the ReSTIR radiance image describes opaque G-buffer pixels.

## Volumetric contribution

`fogPunctualInScatter` reuses the distance, cone, and map-based visibility terms for each fog froxel.
It replaces the surface BRDF with the fog phase response and multiplies by
`volumetric_scattering`. The `cast_volumetric_shadow` field controls only fog visibility; it does not
disable the light's surface shadow.

## In the code

| What | File | Symbols |
|---|---|---|
| Authored components | `scene/src/component.rs` | `PointLight`, `SpotLight` |
| GPU record | `rendering/src/gpu_types.rs` | `GpuLight` |
| Scene packing | `assets/src/render_scene.rs` | `gather_punctual_lights` |
| Surface evaluation | `assets/shaders/lighting.slang` | `punctual`, `distanceAttenuation` |
| Fog evaluation | `assets/shaders/lighting.slang` | `fogPunctualInScatter` |
| Froxel assignment | `assets/shaders/light_cull.slang` | `computeMain`, `MAX_LIGHTS_PER_CLUSTER` |

## Related

- [Light components](../light-components/) describes the scene-facing light types.
- [Cook-Torrance BRDF](../cook-torrance-brdf/) defines the material response.
- [Clustered forward](../clustered-forward/) explains how range bounds fragment work.
- [Spot light shadows](../../shadows-and-culling/spot-light-shadows/) covers the perspective shadow pass.
- [Point light shadows](../../shadows-and-culling/point-light-cube-shadows/) covers the distance cubes.
