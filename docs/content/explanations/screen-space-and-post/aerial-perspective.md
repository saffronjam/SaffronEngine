+++
title = 'Aerial perspective'
weight = 8
math = true
+++

# Aerial perspective

A far ridge has a different colour from the same ridge up close. Air between the eye and the mountain
scatters sunlight into the view ray and attenuates surface radiance. The
[procedural atmosphere](../../image-based-lighting/procedural-atmosphere/) computes this scattering for
the sky, but its sky-view LUT contains direction-dependent background radiance without receiver
distance. Aerial perspective supplies that distance dependence for scene geometry.

Anima follows Sébastien Hillaire's
[scalable atmosphere model](https://sebh.github.io/publications/egsr2020.pdf). A froxel volume samples
the atmosphere transmittance and multiple-scattering LUTs, then the [fog](../height-fog/) composite
applies its in-scatter and transmittance to scene colour.

## A froxel volume marched from the atmosphere LUTs

Aerial perspective owns a persistent, viewport-independent $32\times32\times32$ `rgba16f` volume.
It uses the fog grid's exponential-Z mapping with `FROXEL_NEAR = 0.1` metres and
`AP_FAR_M = 32,000` metres. `ap_slice_view_z` calls `froxel_slice_view_z` with those AP bounds.

One `(8, 8, 8)` workgroup dispatch fills the volume with `numthreads(4, 4, 4)`. Each thread
reconstructs its froxel centre from the inverse projection, inverse view, and slice depth. It takes
16 midpoint samples between the camera and that centre. Each sample evaluates Rayleigh and
Henyey-Greenstein Mie phase terms, solar transmittance, and the multiple-scattering LUT.

$$
T_{k+1}=T_k e^{-\sigma_{t,k}\Delta s}, \qquad
L_{k+1}=L_k+T_k S_k\frac{1-e^{-\sigma_{t,k}\Delta s}}
{\max(\sigma_{t,k},\epsilon)}.
$$

Each froxel stores integrated radiance in RGB and the mean of spectral transmittance in alpha. The
`aerialIntensity` setting multiplies RGB only. The fog integration volume uses the same
`(inScatter, transmittance)` storage convention, so the composite can sample both volumes with one
apply operation.

The AP volume contains atmosphere scattering only. Shadowed local-light scattering remains in the
volumetric-fog injection and integration chain.

## One transmittance ledger, no double darkening

Fog and aerial perspective are independent participating-media terms. The height-fog dispatch
combines them on one transmittance ledger:

$$
T = T_\text{fog}\cdot T_\text{aerial},
\qquad
L = \text{scene}\cdot T + \text{inScatter}_\text{fog} + T_\text{fog}\cdot\text{inScatter}_\text{aerial}
$$

Aerial in-scatter is multiplied by $T_\text{fog}$ because it lies behind the authored near and mid
fog along the view ray. Scene radiance is multiplied once by the product of both transmittances. When
fog is disabled, its term is $(T=1,L=0)$ and AP still composites. When the atmosphere is not live,
the AP pass and sample are disabled.

The `height-fog` compute pass reads the AP volume at binding 5 and performs the complete composite.

## Controls

Aerial perspective is part of `FogSettings` and the `set-fog` merge. `aerialPerspective` enables the
term, and non-negative `aerialIntensity` scales AP in-scatter. The pass runs only when the selected
environment source has a completed atmosphere bake.

```sh
sa set-atmosphere --enabled true
sa set-fog --enabled false --aerialPerspective true --aerialIntensity 1.5
```

This example isolates aerial perspective by leaving authored fog disabled. The two AP fields persist
in the project `environment.fog` object, appear in `EnvironmentDto`, and are edited in the
Environment panel's Fog section.

## In the code

| What | File | Symbols |
|---|---|---|
| AP fill shader | `engine/assets/shaders/aerial_perspective.slang` | `computeMain`, `froxelCenterWorld`, `sampleTransmittance`, `sampleMultiScatter`, `hgPhase` |
| Shared-ledger composite | `engine/assets/shaders/height_fog.slang` | `computeMain`; `T = fogT*apT`, `aerialVolume` (binding 5) |
| AP volume + params + fill set | `engine/crates/rendering/src/froxel_fog.rs` | `AerialPerspective`, `AerialParamsUbo`, `AP_GRID`, `AP_FAR_M`, `ap_slice_view_z` |
| Atmosphere LUT accessors | `engine/crates/rendering/src/ibl.rs` | `transmittance_view`, `multi_scatter_view`, `baked_atmosphere`, `baked_sun` |
| Fill pass + composite fold | `engine/crates/rendering/src/renderer.rs` | `add_aerial_perspective_pass`, `add_fog_pass`, `FogParams` (`aerial`, `fog_enabled`) |
| Per-view AP binding | `engine/crates/rendering/src/view_target.rs` | `write_fog_aerial` (fog-set binding 5) |
| Scene state + serde | `engine/crates/scene/src/environment.rs`, `serde.rs` | `FogSettings` (`aerial_perspective`, `aerial_intensity`), `fog_to_json`/`fog_from_json` |
| Wire DTO + command | `engine/crates/protocol/src/dto.rs`, `engine/crates/control/src/commands_scene.rs` | `SetFogParams` (`aerialPerspective`/`aerialIntensity`), the `set-fog` merge (`aerialIntensity >= 0`) |

## Related

- [Fog](../height-fog/): the shared authoring surface, transmittance ledger, and froxel infrastructure
- [Procedural atmosphere](../../image-based-lighting/procedural-atmosphere/): the transmittance and multiscatter LUTs the AP march samples
- [Compute post-process](../compute-post-process-pattern/): the 3D storage-image dispatch shape
