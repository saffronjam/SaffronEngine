+++
title = 'Ray-query shadows'
weight = 8
+++

# Ray-query shadows

A ray-query shadow tests whether a shaded point sees a light by tracing one ray toward it through
the scene's acceleration structure, instead of comparing against a depth map. Any triangle along
the ray occludes the point.

The trace is an inline ray query, the mechanism
[`VK_KHR_ray_query`](https://www.khronos.org/blog/ray-tracing-in-vulkan) adds to ordinary shader
stages: no ray-tracing pipeline, no shader binding table, no hit or miss shaders. The shadow test
is a few instructions in the mesh fragment shader, next to the rest of shading.

> [!NOTE]
> The path requires a device with the `VK_KHR_acceleration_structure` and `VK_KHR_ray_query`
> extensions (see [RT device gating](../raytracing-device-gating/)). Without them,
> `sa set-rt-shadows` fails with "ray tracing not supported on this device" and the toggle
> clamps off.

## How it works

`rayQueryShadow` builds a ray from the shaded point toward the light, traces it against the
[TLAS](../raytracing-foundation/) bound at set 6, and returns the visibility: 1 on a miss (lit),
0 on a committed triangle hit (shadowed).

```hlsl
RayDesc ray;
ray.Origin = worldPos + toLight * 0.02;  // bias off the surface to avoid self-hit
ray.Direction = toLight;
ray.TMin = 0.0;
ray.TMax = maxDist;
RayQuery<RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES> q;
q.TraceRayInline(rtScene, RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH | RAY_FLAG_SKIP_PROCEDURAL_PRIMITIVES, 0xFF, ray);
q.Proceed();
return q.CommittedStatus() == COMMITTED_TRIANGLE_HIT ? 0.0 : 1.0;
```

`RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH` is the standard shadow-ray flag (defined in the
[DirectX Raytracing spec](https://microsoft.github.io/DirectX-Specs/d3d/Raytracing.html), which
Slang's `RayQuery` API follows): any hit shadows the point, so traversal commits the first
triangle it meets instead of searching for the closest. The `0.02` origin offset along the light
direction lifts the ray off the surface so it does not immediately strike the triangle it
started on.

## Integration with shading

The light UBO's `pointShadowMeta.z` flag selects the ray path in the shader. When it is nonzero,
`evalLighting` traces one long ray toward the sun (`maxDist = 1e4`) in place of the directional
PCF lookup, and `punctual` traces one ray per punctual light with the light distance as the ray
length. Both paths produce the same `shadow` scalar the BRDF multiplies.

`sa set-rt-shadows` drives the runtime toggle, `Renderer::set_rt_shadows`. Enabling it arms the
per-frame TLAS build over the frame's instances; `rt_shadows_enabled` reports the path active
only when the toggle is on, the device supports ray query, and the frame built a TLAS. The
command's response and `render-stats` both expose that value as `rtShadows`.

```sh
sa set-rt-shadows 1   # {"rtShadows": true} once frames are building a TLAS
sa render-stats       # rtSupported, rtShadows, blasCount, ...
```

## One ray per light

The shadow-map paths shadow the directional light plus one spot and one point light; every other
punctual light casts no shadow. The ray path shadows all punctual lights at one ray each, with no
per-light shadow-map budget. The ray length equals the light distance, so an occluder past the
light can never shadow the point.

A single ray also returns a binary answer. Shadow edges are hard, with none of the penumbra
gradient [PCF filtering](../../shadows-and-culling/pcf-filtering/) gives the map paths.

## Shader interface on non-RT devices

The `rtScene` binding and the ray-query helpers compile only into the RT übershader variant.
`xtask shaders` builds `mesh.spv` plus a `mesh_nort.spv` sibling with `SAFFRON_NO_RT` defined,
which strips the set 6/7 declarations. On a device without ray tracing the layouts for those sets
are never created, the mesh pipeline layout omits them, and the PSO cache loads the `_nort`
variant, so the shader's declared interface matches the layout.

## In the code

| What | File | Symbols |
|---|---|---|
| The inline shadow ray | `assets/shaders/lighting.slang` | `rayQueryShadow` |
| TLAS binding (set 6) | `assets/shaders/lighting.slang` | `rtScene` |
| The shader gate | `assets/shaders/lighting.slang` | `evalLighting`, `punctual` (the `pointShadowMeta.z` branches) |
| The runtime toggle | `crates/rendering/src/renderer.rs` | `Renderer::set_rt_shadows`, `rt_shadows_enabled` |
| Toggle + readiness state | `crates/rendering/src/rt.rs` | `Rt::set_rt_shadows`, `Rt::shadows_enabled`, `Rt::tlas_ready` |
| TLAS supply | `crates/rendering/src/rt.rs` | `Rt::set_rt_scene`, `Rt::prepare_tlas_build` |
| The control command | `crates/control/src/commands_render.rs` | the `set-rt-shadows` registration |
| The RT-off variant | `xtask/src/shaders.rs`, `crates/rendering/src/pipelines.rs` | `NO_RT_DEFINE`, `nort_variant_path` |

## Related

- [Acceleration structures](../raytracing-foundation/) — the TLAS the ray traverses
- [RT device gating](../raytracing-device-gating/) — how `rt_supported` is detected and enforced
- [Directional shadows](../../shadows-and-culling/directional-shadows/) — the map path the ray replaces for the sun
- [PCF filtering](../../shadows-and-culling/pcf-filtering/) — the filtered map lookup and its penumbra
- [Cook-Torrance BRDF](../../lighting-and-brdf/cook-torrance-brdf/) — what the `shadow` scalar multiplies
