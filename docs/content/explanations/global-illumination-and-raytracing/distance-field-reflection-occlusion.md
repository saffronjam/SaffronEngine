+++
title = 'Distance field reflection occlusion'
weight = 11
math = true
+++

# Distance field reflection occlusion

The prefiltered [IBL](../../image-based-lighting/ibl-overview/) environment treats the sky as
visible from every point, so a polished floor under an overhang would mirror open sky it cannot
see. Distance-field reflection occlusion corrects this: a cone sphere-marched along the reflection
vector through the Global Distance Field yields a $[0,1]$ visibility factor that dims the specular
IBL where the reflected direction is blocked. The technique is the reflection half of Wright's
[Dynamic Occlusion with Signed Distance Fields](https://advances.realtimerendering.com/s2015/)
(SIGGRAPH 2015).

## Why the cone follows the reflection vector

Dimming reflections by a diffuse AO scalar gives the wrong answer for a chrome surface facing open
sky: its hemisphere may be half-enclosed while the one direction it mirrors is unobstructed.
Reflection occlusion asks whether that specific ray escapes. `sdfReflectionOcclusion` marches a
single cone along $R$ and keeps the tightest penumbra ratio it meets:

$$
\text{occ} = \min_{t}\ \operatorname{saturate}\!\left(\frac{d(t)}{\theta\, t}\right)
$$

where $d(t)$ is the field distance at march distance $t$ and $\theta\,t$ is the cone footprint
there. The half-angle widens with roughness, from $\tan\theta = 0.05$ at mirror-smooth to $0.6$ at
full roughness: a mirror samples a thin pencil of the environment, a rough surface a broad lobe.
The march takes up to 12 sphere-trace steps over at most 12 m, stepping by the field distance
(floored at 0.05 m), and exits early once the cone is fully open and far from any surface.

## Marching the Global Distance Field

The cone reads the Global Distance Field clipmap: three camera-centered `R16_SNORM` volumes of
128³ voxels, the finest spanning 32 m (a 0.25 m voxel) and each coarser cascade doubling the
extent. `gdfDistanceOccupancy` selects the finest cascade containing the sample, converts world position to
that cascade's toroidal UVW, and takes one trilinear tap; near a cascade's outer face it blends
into the next coarser one so the handoff shows no shell. One tap costs the same regardless of
scene size, because the cone never iterates an instance list.

When a sample leaves the coarsest cascade the field can say nothing more, so the march stops and
keeps the openness accumulated so far. The reflection then reaches the open sky, which is exactly
what the analytic environment assumes. The per-mesh distance-field bricks feed only the GDF
composite and the [DDGI trace's](../software-ray-trace/) near field; this cone taps the composited
clipmap alone (light-set bindings 9 and 10).

## Half-resolution prepass

The march runs once per half-resolution pixel in the `specocc` compute pass, not per shaded
fragment. The pass reconstructs world position and normal from the
[thin G-buffer](../../screen-space-and-post/thin-gbuffer/), reads per-pixel roughness from the
G-buffer roughness target, reflects the camera ray, and writes the cone result into an `rgba16f`
image with view-Z in alpha. Background pixels write 1.0: no geometry, fully open sky.

A bilateral upsample (`specocc-blur`, the [SSGI](../../screen-space-and-post/ssgi/) blur kernel
bound with this pass's set) lifts the half-res result to full resolution with depth-aware weights.
There is no temporal accumulation: the term is view-dependent ($R$ moves with the camera), and
reprojecting it through surface motion would smear it. Spatially denoised, the low-frequency
scalar holds steady on its own.

## Application in the mesh

The lighting übershader samples the resolved map by screen UV and multiplies it into the specular
IBL after the energy-compensation and horizon terms:

```hlsl
float specSkyVis = (globals.sdfOcclusion.y != 0 && !translucent)
    ? speoccMap.SampleLevel(screenUv, 0.0).r : 1.0;
// ... energy compensation + horizon fade ...
float specAO = saturate(pow(ndotv + ao, exp2(-16.0 * roughness - 1.0)) - 1.0 + ao);
specularIBL *= specAO * specSkyVis;
```

The two factors occlude different things. `specAO` is the roughness-aware specular occlusion from
[Moving Frostbite to PBR](https://seblagarde.wordpress.com/2015/07/14/siggraph-2014-moving-frostbite-to-physically-based-rendering/)
(Lagarde and de Rousiers), driven by the contact-scale `ao` (the material occlusion texture ×
[GTAO](../../screen-space-and-post/gtao/)); it darkens reflections in creases and tends to 1 on
smooth metals. `specSkyVis` is the directional GDF cone, darkening the reflected skybox under
overhangs. Translucent surfaces read 1.0 because the map is keyed to the opaque G-buffer behind
them.

## The diffuse twin

`sdfSkyVisibility` applies the same idea to indirect diffuse: nine cosine-weighted cones (about a
30° half-angle, 8 m range) sample the hemisphere of the shading normal against the same clipmap.
It runs in the `dfao` half-res prepass with an eight-position azimuth jitter per frame, and a
bilateral upsample plus a temporal accumulator average the rotated cone rings into a denser
hemisphere. Sky visibility is view-independent, so temporal reuse is safe here.

The resolved sky visibility multiplies the analytic sky irradiance inside the half-res
`gi-resolve` pass. Where [DDGI](../ddgi-overview/) has coverage, its probe irradiance overrides
that analytic term by coverage weight, so the distance-field occlusion shapes only the residual
sky. Contact-scale diffuse occlusion stays with GTAO and the material occlusion texture.

## One gate

`want_sky_occlusion` arms the whole apparatus per frame: IBL must be on and ready, the
`sky_occlusion` toggle set, and the GDF clipmap composited. The same predicate resolves the two
prepass PSOs and writes the `sdfOcclusion.y` bit into the light UBO, so the fragment never samples
a map no pass produced. The toggle is scriptable:

```sh
sa set-sky-occlusion 0   # reflections keep the full sky everywhere
sa set-sky-occlusion 1   # overhangs occlude the reflected skybox again
```

Disabling the Global Distance Field itself (`sa set-gdf 0`) drops the gate too, since the clipmap
the cones march never composites.

## In the code

| What | File | Symbols |
|---|---|---|
| Reflection cone march | `assets/shaders/sdf.slang` | `sdfReflectionOcclusion`, `gdfDistanceOccupancy`, `gdfEnabled` |
| Diffuse sky-visibility cones | `assets/shaders/sdf.slang` | `sdfSkyVisibility` |
| Specular prepass | `assets/shaders/specocc.slang` | `computeMain` |
| Diffuse (DFAO) prepass | `assets/shaders/dfao.slang` | `computeMain` |
| Mesh application | `assets/shaders/lighting.slang` | `speoccMap`, `globals.sdfOcclusion` |
| Diffuse application | `assets/shaders/gi_resolve.slang` | `computeMain`, `dfaoMap` |
| Frame gate + pass wiring | `crates/rendering/src/renderer.rs` | `want_sky_occlusion`, `set_sky_occlusion` |
| Lighting UBO bit | `crates/rendering/src/lighting.rs` | `set_frame_sdf_occlusion` |
| GDF cascade constants | `crates/rendering/src/global_sdf.rs` | `GDF_CASCADES`, `GDF_RES`, `GDF_CASCADE0_EXTENT` |
| Control command | `crates/control/src/commands_render.rs` | `set-sky-occlusion` |

## Porous matter

Porous aggregate matter (foliage-classed materials) contributes no distance to the field; the cones
instead accumulate Beer–Lambert extinction through its per-cascade occupancy volumes using the
shared `sdfExtinctionStep`. A canopy therefore dims a reflection or the sky by its density rather
than sealing the cone the way a wall does. The [software ray trace](../software-ray-trace/) applies
the same step along its probe rays.

## Related

- [Software ray trace](../software-ray-trace/) — the DDGI trace that sphere-marches the same near/far distance field
- [DDGI overview](../ddgi-overview/) — the probe irradiance that overrides the occluded analytic sky by coverage
- [IBL overview](../../image-based-lighting/ibl-overview/) — the analytic environment the cone occludes
- [GTAO](../../screen-space-and-post/gtao/) — the contact-scale occlusion behind the specular AO term
- [SSGI](../../screen-space-and-post/ssgi/) — the denoise kernels the prepasses reuse
