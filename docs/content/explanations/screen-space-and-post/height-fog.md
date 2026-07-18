+++
title = 'Fog'
weight = 7
math = true
+++

# Fog

Fog adds participating media between the camera and visible surfaces. A scene chooses an analytic
height-and-distance integral or a volumetric froxel grid; both composite into scene-linear HDR before
[bloom](../bloom/) and share the scene's atmospheric transmittance.

## Analytic fog

The analytic mode reconstructs each pixel's world position from scene depth and integrates two
exponential height layers along the view ray. For a layer with density $\sigma$, reference height $h$,
height falloff $F$, and receiver distance $\ell$, the optical depth is:

$$
\tau = \sigma 2^{-F(y_\mathrm{cam}-h)}
       \frac{1-2^{-F\Delta y}}{F\Delta y}
       \max(\ell-\ell_\mathrm{start},0)
$$

Here, $\Delta y=y_\mathrm{recv}-y_\mathrm{cam}$. Near a horizontal ray, the denominator approaches
zero, so `layerOpticalDepth` uses this Taylor approximation for $|F\Delta y|\le10^{-4}$:

$$
\frac{1-2^{-F\Delta y}}{F\Delta y}
\approx \ln 2 - \tfrac12(\ln 2)^2F\Delta y
$$

The broad layer and optional ground layer add into one optical depth. Transmittance is
`max(exp2(-tau), 1 - maxOpacity)`. In-scattered light combines the authored albedo and emission with a
directional sun lobe. When the procedural atmosphere is active, the albedo is tinted from its
[sky-view LUT](../../image-based-lighting/procedural-atmosphere/).

## Volumetric fog

Volumetric mode follows the frustum-aligned froxel pipeline described by
[Bartlomiej Wronski](https://advances.realtimerendering.com/s2014/wronski/bwronski_volumetric_fog_siggraph2014.pdf)
and the energy-conserving integration described by
[Sebastien Hillaire](https://advances.realtimerendering.com/s2015/Siggraph_2015_Hillaire_Volumetric-fog.pdf).
It runs three stages:

1. `fog_inject.slang` reconstructs each froxel centre, evaluates the height layers and local fog
   volumes, and accumulates shadowed light into linear in-scatter and extinction.
2. `fog_integrate.slang` scans each XY column front to back and writes accumulated in-scatter plus
   transmittance for every depth slice.
3. `height_fog.slang` samples the integrated volume at the receiver depth and composites it over the
   scene.

The inject pass uses the clustered light lists and shadow maps. Its light scattering follows the
[Henyey-Greenstein phase function](https://ui.adsabs.harvard.edu/abs/1941ApJ....93...70H/abstract),
with `phaseG` controlling the forward or backward bias. Directional, point, and spot lights expose
`volumetricScattering` and `castVolumetricShadow` fields for shaft intensity and shadowing.

For a slice of thickness $d$, extinction $\sigma$, and source radiance $S$, the integrate pass uses:

$$
T_\mathrm{slice}=e^{-\sigma d}, \qquad
S_\mathrm{int}=\frac{S-S T_\mathrm{slice}}{\max(\sigma,10^{-5})}
$$

The running values are `accum += totalT * S_int` and `totalT *= sliceT`. The composite reads the
result as `(inScatter, transmittance)` and evaluates
$c_\mathrm{out}=c_\mathrm{scene}T+L_\mathrm{scatter}$.

## Grid quality and history

The volumetric quality setting selects a fixed grid:

| Quality | Grid |
|---|---:|
| `low` | $128\times72\times64$ |
| `medium` | $160\times90\times64$ |
| `high` | $160\times90\times128$ |

Depth slices follow the same exponential distribution as clustered light culling. A froxel therefore
maps to a cull cluster through `clusterIndexFor` without another light-culling pass.

Two `rgba16f` scatter volumes ping-pong between the current injection target and the previous frame's
history. Injection reprojects the froxel centre through the previous view-projection matrix and blends
linear in-scatter and extinction. `historyBlend` is the fresh-sample weight and defaults to `0.05`.

The XY offset uses the active TAA jitter, while `haltonZ` moves the sample within each depth slice.
History is invalid on the first frame, after a camera cut, and after a quality change. The optional
neighbourhood clamp restricts reprojected history around the fresh value, and `lightClamp` caps a
single light's contribution before accumulation.

## Local fog volumes

A `FogVolume` component adds bounded density to the volumetric grid. Its entity transform positions an
oriented box or sphere. Soft edges, a local height falloff, albedo, emission, and phase anisotropy shape
the medium.

Optional tiling 3D noise erodes the density. `noiseScale`, `noiseIntensity`, and `noiseDetail` control
the two sampled octaves; `wind` and `speed` advect the coordinates over time. A zero noise intensity
produces uniform density and skips the texture sample.

The renderer uploads at most `MAX_FOG_VOLUMES` records per frame. Each record contains the inverse
world transform and packed optical values. The native overlay draws a fog icon and the box or sphere
bounds so meshless volumes remain selectable.

## Composition and inspection

The fog composite also reads the
[aerial-perspective](../aerial-perspective/) volume when enabled. It multiplies fog and atmospheric
transmittance, attenuates the scene once, and places aerial in-scatter behind the near fog:

$$
T=T_\mathrm{fog}T_\mathrm{aerial}, \qquad
L=cT+L_\mathrm{fog}+T_\mathrm{fog}L_\mathrm{aerial}
$$

Forward transparent materials sample the integrated froxel volume in their mesh shader because they
do not contribute receiver depth to the opaque depth buffer. `ViewMode::Fog` replaces the frame with
integrated in-scatter plus opacity, which exposes the volumetric density and light shafts directly.

Fog belongs to `SceneEnvironment` and persists in the project's `environment.fog` block. `set-fog`
partially updates that block, and `get-environment` reads it back. This example enables medium-quality
volumetric fog with the default anisotropy:

```sh
sa set-fog --enabled true --mode volumetric --quality medium --baseDensity 0.02 --phaseG 0.6
sa set-view-mode --mode fog
```

## In the code

| What | File | Symbols |
|---|---|---|
| Analytic and final composite | `height_fog.slang` | `FogParams`, `layerOpticalDepth`, `skyViewTint`, `computeMain` |
| Volumetric injection and integration | `fog_inject.slang`, `fog_integrate.slang` | `froxelSampleWorld`, `froxelUvwFromWorld`, `haltonZ`, `sdBox`, `fogVolumeNoise`, `computeMain` |
| Shared light and fog functions | `lighting.slang` | `clusterIndexFor`, `hgPhase`, `fogPunctualInScatter`, `fogDirectionalInScatter`, `applyFroxelFog` |
| Froxel resources and quality | `froxel_fog.rs` | `FroxelFog`, `FroxelQuality`, `FogGridParams`, `FogVolumeGpu`, `MAX_FOG_VOLUMES` |
| Scene-wide settings | `environment.rs` | `FogSettings`, `FogMode`, `FogQuality` |
| Local volume component | `component.rs` | `FogVolume`, `FogShape` |
| Render-graph passes | `renderer.rs` | `submit_fog_volumes`, `add_froxel_fog_passes`, `add_fog_pass`, `ViewMode::Fog` |
| Project serialization | `serde.rs` | `fog_to_json`, `fog_from_json`, `fog_quality_name` |
| Control-plane update | `dto.rs`, `commands_scene.rs` | `SetFogParams`, `set-fog` |

## Related

- [Aerial perspective](../aerial-perspective/) — contributes planetary scattering to the same composite
- [Bloom](../bloom/) — reads the fogged scene-linear image
- [Procedural atmosphere](../../image-based-lighting/procedural-atmosphere/) — supplies the sky-view tint
- [Compute post-process](../compute-post-process-pattern/) — describes the in-place composite pass
