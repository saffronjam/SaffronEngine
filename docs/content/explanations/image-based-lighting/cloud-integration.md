+++
title = 'Cloud integration'
weight = 9
math = true
+++

# Cloud integration

Cloud integration makes volumetric clouds, planetary atmosphere, fog, and opaque surfaces share one
lighting and transmittance model. The cloud renderer produces premultiplied radiance, transmittance,
and a mean front depth. The fog composite combines those values with aerial perspective before bloom,
keeping clouds in scene-linear HDR.

## Aerial perspective at cloud depth

`atmos_ap.slang` contains the bounded atmosphere march used by both the aerial-perspective volume and
the cloud fold. It samples the same Transmittance and Multiple-Scattering LUTs for both receivers. The
cloud fold stops the march at the full-resolution cloud front depth instead of reading the 32 km
aerial-perspective volume, which keeps distant clouds continuous with the horizon.

For premultiplied cloud radiance $L_c$, cloud transmittance $T_c$, atmospheric in-scatter $L_a$, and
camera-to-cloud transmittance $T_a$, the atmosphere-wrapped cloud term is

$$
L'_c = L_c T_a + (1-T_c)L_a.
$$

This follows the participating-media composition described in
[Physically Based Sky, Atmosphere and Cloud Rendering](https://media.contentapi.ea.com/content/dam/eacom/frostbite/files/s2016-pbs-frostbite-sky-clouds-new.pdf).
The cloud receives the camera-to-cloud atmosphere once; the background keeps its own aerial term.

## One cloud shadow field

`cloud_shadow.slang` fills a camera-snapped, three-cascade 2D array. Each texel integrates density
through the cloud layer along the active celestial light and stores Beer transmittance with an
exponential-shadow-map encoding. The 8 km, 32 km, and 128 km cascade extents share one R16 image and a
linear clamp sampler. Texel-snapped cascade centres keep the projection stable as the camera moves.

The map has three consumers. Opaque lighting multiplies the sun term by surface shadow visibility,
the cloud march uses the same visibility for self-shadowing, and `fog_inject.slang` applies it only to
directional in-scatter. The unchanged front-to-back fog integration turns variations in that last
sample into crepuscular rays, including rays from an off-screen sun. This arrangement follows the
integrated cloud-shadow and frustum-volume approach in
[Creating the Atmospheric World of Red Dead Redemption 2](https://www.advances.realtimerendering.com/s2019/index.htm).

## One transmittance ledger

`height_fog.slang` is the only cloud color composite. It folds fog, aerial perspective, and clouds as
independent media:

$$
T = T_f T_a T_c,
$$

$$
L = cT + L_f + T_f L_a + T_f L'_c.
$$

Here $c$ is the background scene radiance, while $L_f$ is fog in-scatter. The cloud term is attenuated
by near fog but not by the background aerial transmittance, because $L'_c$ already contains its own
camera-to-cloud atmosphere. `cloud_upscale.slang` writes full-resolution cloud radiance/transmittance
and front depth; it does not composite into the scene target.

## Wind and night lighting

`WindSettings` defines one scene-wide horizontal field with orientation, speed, and gust. Cloud
weather and noise coordinates advect from the time-of-day clock, while the elevation-indexed coverage
and cloud-type curves drive the weather cycle. A divergence-free 2D curl warp bends
the detail flow without compressing the field, following
[Curl-Noise for Procedural Fluid Flow](https://www.cs.ubc.ca/~rbridson/docs/bridson-siggraph2007-curlnoise.pdf).
Fog volumes with no local wind use the same velocity; an authored `FogVolume.wind` remains a local
override.

The cloud march and shadow fill choose the atmosphere-coupled sun while it is above the horizon and
the moon otherwise. Night clouds therefore use the same lunar direction, colour, intensity, and
atmospheric transmittance as the visible moon and night sky.

This example enables the shared shadow field and sets a north-east wind:

```sh
sa set-clouds --enabled true --castCloudShadows true --cloudShadowStrength 1 \
  --cloudShadowOnSurfaceStrength 0.8
sa set-wind --orientation 45 --speed 20 --gust 0.5
```

## In the code

| What | File | Symbols |
|---|---|---|
| Shared atmosphere march | `engine/assets/shaders/atmos_ap.slang` | `ApParams`, `AtmoSample`, `marchAtmosphere` |
| Cascaded shadow fill and sampling | `engine/assets/shaders/cloud_shadow.slang` · `clouds.slang` | `computeMain`, `cloudShadowVisibility` |
| Cloud/fog/aerial ledger | `engine/assets/shaders/height_fog.slang` | `FogParams`, `computeMain` |
| Directional god-rays | `engine/assets/shaders/fog_inject.slang` | `cloudShadowMap`, `fogDirectionalInScatter` |
| GPU cloud state | `engine/crates/rendering/src/clouds.rs` | `Clouds`, `CloudShadowProjection`, `CloudRenderSettings` |
| Scene wind and cloud controls | `engine/crates/scene/src/environment.rs` | `WindSettings`, `CloudSettings` |
| Control-plane updates | `engine/crates/protocol/src/dto.rs` · `engine/crates/control/src/commands_scene.rs` | `SetWindParams`, `SetCloudsParams`, `set-wind` |

## Related

- [Volumetric cloud shape](../volumetric-cloud-shape/) — the advected density field
- [Volumetric cloud lighting](../volumetric-cloud-lighting/) — the reduced-resolution march and upscale
- [Fog](../../screen-space-and-post/height-fog/) — the froxel and analytic media sharing the ledger
- [Aerial perspective](../../screen-space-and-post/aerial-perspective/) — the background atmosphere term
