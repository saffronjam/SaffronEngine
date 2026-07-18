+++
title = 'IBL ambient term'
weight = 9
math = true
+++

# IBL ambient term

The ambient term combines indirect diffuse light with environment reflections. Anima evaluates it after direct punctual and directional lighting, then adds emissive radiance:

$$
L_\text{out} = L_\text{direct} + L_\text{ambient} + L_\text{emissive}.
$$

`LightUbo.counts.z` selects image-based lighting (IBL) when the environment bake is ready and IBL is enabled. Otherwise, the shader uses the scene's authored ambient color as a diffuse fallback.

## Indirect diffuse

Opaque and transparent surfaces obtain diffuse irradiance through different paths. Opaque geometry samples `giIndirectMap`, a half-resolution result produced by `gi_resolve.slang`. The compute pass starts with the global irradiance cubemap, optionally multiplies it by distance-field sky visibility, and replaces it by [DDGI](../../global-illumination-and-raytracing/ddgi-overview/) irradiance where the probe volume has coverage.

Transparent geometry cannot use the screen-space GI map because that map describes the opaque surface behind it. It samples the global irradiance cubemap directly and applies the same DDGI coverage blend in the fragment shader.

Both paths apply the diffuse energy factor

$$
k_d = (1 - F)(1 - \text{metallic})
$$

and multiply by albedo and material occlusion. The opaque path also receives any distance-field sky visibility already folded into `giIndirectMap`.

## Split-sum specular

Specular IBL follows Brian Karis's [split-sum approximation](https://cdn2.unrealengine.com/Resources/files/2013SiggraphPresentationsNotes-26915738.pdf). The shader samples the prefiltered environment in reflection direction $R$ and reads the BRDF integration LUT by $n \cdot v$ and roughness:

$$
L_\text{specular} = L_\text{prefiltered}(R, \ell)\left(F_0 A + B\right).
$$

The five-level prefiltered cube uses a nonlinear roughness-to-LOD mapping:

$$
\ell = 4r(2-r).
$$

This keeps near-mirror surfaces on sharper mips longer than a linear mapping. The shader then applies multi-scatter energy compensation, a geometric-normal horizon fade, roughness-aware specular occlusion, and distance-field reflection visibility.

```hlsl
float2 ab = brdfLut.SampleLevel(float2(ndotv, roughness), 0.0).rg;
float3 specularIBL = prefiltered * (F0 * ab.x + ab.y);

float Ess = ab.x + ab.y;
float3 energyComp = 1.0 + F0 * (1.0 / max(Ess, 1e-3) - 1.0);
specularIBL *= energyComp;

float horizon = min(1.0 + dot(R, normalize(input.worldNormal)), 1.0);
specularIBL *= horizon * horizon;
```

For opaque geometry, [screen-space reflections](https://jcgt.org/published/0003/04/04/paper.pdf) and [ray-traced reflections](../../global-illumination-and-raytracing/raytracing-foundation/) can replace the prefiltered radiance before the BRDF factors are applied. Both use `saturate(1 - 2r)` as their smoothness gate. Transparent geometry keeps the cubemap value.

## Ambient fallback

When IBL is disabled or its resources are not ready, the shader evaluates

$$
L_\text{fallback} = \text{albedo}(1-\text{metallic})C_\text{ambient}O_\text{material}.
$$

`C_ambient` comes from `SceneLighting.ambient`, uploaded as `LightUbo.ambient_color.rgb`. With `use_sky_for_ambient`, scene gathering uses `ambient_color * ambient_intensity`. Otherwise, it uses the directional light's scalar `ambient` as a grayscale value. The default environment contributes `(0.15, 0.15, 0.15)`.

DDGI remains additive on this fallback path. It contributes irradiance times probe coverage, albedo, and the nonmetal factor.

## Screen-space gates

The shader contains gates for [GTAO](../../screen-space-and-post/gtao/) and [SSGI](../../screen-space-and-post/ssgi/). GTAO would multiply material occlusion for opaque diffuse and specular occlusion. SSGI would add an AO-modulated one-bounce diffuse term.

> [!NOTE]
> `Lighting::set_scene_lighting` writes `LightUbo.counts.w = 0` and `LightUbo.screen_flags.y = 0`. The fragment shader therefore skips the GTAO and SSGI ambient contributions even when their screen-space passes run.

`LightUbo.screen_flags.z` does carry the DDGI enable state. Distance-field sky visibility reaches opaque diffuse through `gi-resolve`, while reflection visibility reaches specular through `speoccMap`.

## In code

| What | File | Symbols |
|---|---|---|
| Ambient composition | `engine/assets/shaders/lighting.slang` | `evalLighting`, `ambient`, `reflectionSpec` |
| Specular IBL | `engine/assets/shaders/lighting.slang` | `prefilterLod`, `prefilteredMap`, `brdfLut`, `fresnelSchlickRoughness` |
| Opaque diffuse resolve | `engine/assets/shaders/gi_resolve.slang` | `GiParams`, `computeMain`, `indirectOut` |
| Light UBO upload | `engine/crates/rendering/src/lighting.rs` | `LightUbo`, `Lighting::set_scene_lighting`, `Lighting::set_frame_ibl`, `Lighting::set_frame_ddgi` |
| Ambient source selection | `engine/crates/assets/src/render_scene.rs` | `render_scene` |
| Environment defaults | `engine/crates/scene/src/environment.rs` | `SceneEnvironment`, `SceneEnvironment::default` |

## Related

- [Image-based lighting](../../image-based-lighting/)
- [Cook-Torrance BRDF](../cook-torrance-brdf/)
- [Distance field reflection occlusion](../../global-illumination-and-raytracing/distance-field-reflection-occlusion/)
- [HDR and exposure](../hdr-and-exposure/)
