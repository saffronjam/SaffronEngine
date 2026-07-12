+++
title = 'Aerial perspective'
weight = 8
math = true
+++

# Aerial perspective

A far ridge is not the same color as the same ridge up close. Kilometres of air between the eye and the
mountain scatter sunlight into the view ray and absorb the light coming back, so distance washes the
ridge toward the blue-grey of the sky behind it. The [procedural atmosphere](../../image-based-lighting/procedural-atmosphere/)
already computes exactly this scattering for the sky — but the sky-view LUT is a background lookup with
no distance in it, so it never touches scene geometry. A ridge rendered against a scattering sky reads
as untinted paint over a physically-shaded backdrop.

Aerial perspective closes that gap: it applies the atmosphere's own in-scattering to scene geometry, so
the ridge and the sky behind it agree in hue. It is **Hillaire 2020's aerial-perspective volume** — a
small froxel volume fed by the same atmosphere LUTs, composited on the same transmittance ledger as the
[fog](../height-fog/) — not a screen-space light-scattering post filter (which cannot tint occluded
geometry) and not a second bespoke atmosphere path bolted onto the sky.

## A froxel volume marched from the atmosphere LUTs

Aerial perspective reuses the fog subsystem's 3D-transient infrastructure at a smaller, cheaper size: a
frustum-aligned $32\times32\times32$ `rgba16f` volume on the **same exponential-Z depth mapping** the
fog grid uses, only with a far plane at the atmosphere horizon (`AP_FAR_M` = 32 km) instead of the fog's
~128 m. One mapping function, two instantiations — `ap_slice_view_z` is a direct call into the fog grid's
`froxel_slice_view_z` with the AP far bound.

One compute dispatch fills it. Per froxel, `aerial_perspective.slang` reconstructs the froxel-center
world position from the inverse view-projection and the exponential-Z slice, then marches the atmosphere
from the camera **to that froxel's distance** — the same single-scatter loop as `atmos_skyview.slang`
(Rayleigh + Henyey-Greenstein Mie phase, `sampleTransmittance` toward the sun, isotropic
`sampleMultiScatter`), only bounded at the froxel instead of the atmosphere edge:

$$
L(d) = \sum_{k} T_k \cdot \frac{S_k - S_k\,\sigma_k^{\text{step}}}{\sigma_k},
\qquad
T(d) = \prod_k \sigma_k^{\text{step}}
$$

Each froxel stores `(inScatter, meanTransmittance)` — the exact `(rgb, a)` convention the fog integration
volume uses, which is what lets the composite treat them the same way. Sampling the sky-view LUT directly
would give every far pixel the horizon color regardless of range; only marching the LUTs to the froxel
distance yields the correct range-dependent blue-shift.

## One transmittance ledger, no double darkening

Fog and aerial perspective are two **independent** participating media, composited as two multiplied
terms on a single transmittance ledger so a region inside both is attenuated by each exactly once:

$$
T = T_\text{fog}\cdot T_\text{aerial},
\qquad
L = \text{scene}\cdot T + \text{inScatter}_\text{fog} + T_\text{fog}\cdot\text{inScatter}_\text{aerial}
$$

Aerial perspective sits *behind* the near/mid fog along the view ray, so its in-scatter is pre-attenuated
by $T_\text{fog}$ — adding it raw would let far planetary scattering punch through dense near fog. Keeping
the two media separate rather than injecting the atmosphere into the fog grid is the correctness
mechanism, not a shortcut: fog carries near/mid density out to `fogFar`, aerial perspective carries the
long-range Rayleigh/Mie out to the horizon, and neither double-counts the other. The AP term degrades to
identity ($T=1$, in-scatter $=0$) whenever the atmosphere is off, so the ledger collapses cleanly to
fog-only; when fog is off but AP is on, the fog term goes neutral and the composite runs for AP alone.

The composite pass reads one extra volume and folds in one extra ledger term — there is no second
composite pass and no second apply operator.

## Driving it

Aerial perspective is fog-ledger state, so it rides the existing `set-fog` merge — there is no
`set-aerial-perspective` command. Two fields join `FogSettings`: `aerialPerspective` (a bool) and
`aerialIntensity` (a coherence multiplier on the AP in-scatter, not an exposure control). The active
atmosphere is the gate: with no baked LUTs there is nothing to march, so the fill is skipped and the
composite branch is a no-op.

```sh
sa set-atmosphere --enabled true
sa set-fog --enabled true --aerialPerspective true --aerialIntensity 1.5
```

`sa set-atmosphere --enabled false` collapses aerial perspective to fog-only; the two fields round-trip
through the project `environment` block and the `EnvironmentDto` echo, and are edited from the
Environment panel's Fog section.

## In the code

| What | File | Symbols |
|---|---|---|
| AP fill shader | `engine/assets/shaders/aerial_perspective.slang` | `computeMain`, `froxelCenterWorld`, `sampleTransmittance`, `sampleMultiScatter`, `hgPhase` |
| Shared-ledger composite | `engine/assets/shaders/height_fog.slang` | `computeMain` — `T = fogT*apT`, `aerialVolume` (binding 5) |
| AP volume + params + fill set | `engine/crates/rendering/src/froxel_fog.rs` | `AerialPerspective`, `AerialParamsUbo`, `AP_GRID`, `AP_FAR_M`, `ap_slice_view_z` |
| Atmosphere LUT accessors | `engine/crates/rendering/src/ibl.rs` | `transmittance_view`, `multi_scatter_view`, `baked_atmosphere`, `baked_sun` |
| Fill pass + composite fold | `engine/crates/rendering/src/renderer.rs` | `add_aerial_perspective_pass`, `add_fog_pass`, `FogParams` (`aerial`, `fog_enabled`) |
| Per-view AP binding | `engine/crates/rendering/src/view_target.rs` | `write_fog_aerial` (fog-set binding 5) |
| Scene state + serde | `engine/crates/scene/src/environment.rs`, `serde.rs` | `FogSettings` (`aerial_perspective`, `aerial_intensity`), `fog_to_json`/`fog_from_json` |
| Wire DTO + command | `engine/crates/protocol/src/dto.rs`, `engine/crates/control/src/commands_scene.rs` | `SetFogParams` (`aerialPerspective`/`aerialIntensity`), the `set-fog` merge (`aerialIntensity >= 0`) |

## Related

- [Fog](../height-fog/) — the shared authoring surface, transmittance ledger, and froxel infrastructure
- [Procedural atmosphere](../../image-based-lighting/procedural-atmosphere/) — the transmittance + multiscatter LUTs the AP march samples
- [Compute post-process](../compute-post-process-pattern/) — the 3D storage-image dispatch shape

> [!NOTE]
> The AP volume carries **only** the atmosphere medium — no shadowed local lights — so it is the fog
> volume with a cheaper injection. Because the storage convention and the apply operator are identical to
> the fog integration volume's, folding the two is a single extra ledger term, not a parallel pipeline.
