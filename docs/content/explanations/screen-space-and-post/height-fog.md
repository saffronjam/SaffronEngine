+++
title = 'Fog'
weight = 7
math = true
+++

# Fog

Between the camera and the geometry there is air, and air scatters light. Without a participating
medium a scene reads as a perfect vacuum: distant surfaces keep full contrast and the space in front
of the sky stays empty. Height fog fills that gap with a depth-correct atmospheric haze that thickens
with distance and settles into the low ground, authored per-scene like the sky and on for free with no
raymarching.

It is the **analytic exponential height & distance fog** — the Unreal `HeightFogCommon.ush` /
iquilezles "Better Fog" closed-form line integral, not a per-vertex fog factor or a screen-tint on a
linear distance ramp. One fullscreen compute pass reconstructs each pixel's world position from the
scene depth, integrates the exponential-in-height density along the view ray in closed form, and blends
the result into the scene-linear HDR offscreen **after** the sky and scene resolve and **before**
[bloom](../bloom/).

## The closed-form optical depth

The density of the medium falls off exponentially with world-up height (`+Y`): a broad layer at
reference height $h$ has density $\sigma \cdot 2^{-F(y - h)}$ for falloff $F$. Integrated along a
straight view ray from the camera to the receiver, that path integral collapses to a closed form —
there is no march, no 3D volume, no temporal history:

$$
\tau = \sigma\, 2^{-F(y_\text{cam} - h)} \cdot \text{lineInt} \cdot \max(\ell - \ell_\text{start},\, 0),
\qquad
\text{lineInt} = \frac{1 - 2^{-F\,\Delta y}}{F\,\Delta y}
$$

where $\Delta y = y_\text{recv} - y_\text{cam}$ and $\ell$ is the ray length. As the ray approaches
horizontal, $F\,\Delta y \to 0$ and that ratio is $0/0$; the one numerically critical detail is the
**Taylor branch** that keeps it finite instead of banding:

$$
\text{lineInt} \approx \ln 2 - \tfrac{1}{2}\ln^2 2 \cdot F\,\Delta y \qquad (|F\,\Delta y| \le 10^{-4})
$$

A broad layer plus an optional summed **ground layer** (a thinner haze under the broader one) add into
one optical depth $\tau$ — never a second composite. The transmittance is `exp2(-τ)`, clamped by a
`maxOpacity` floor, and a `startDistance` holds the fog off the near field. The whole thing is base-2
throughout, matching the reference so authored densities read the same.

## In-scatter that agrees with the sky

The in-scatter colour is a flat `albedo`, tinted by sampling the Hillaire
[sky-view LUT](../../image-based-lighting/procedural-atmosphere/) in the view direction — so fog reddens
toward the sun at sunset and matches the horizon hue with no extra authoring. A directional
sun-through-haze lobe adds believable shafts on top:

$$
c_\text{out} = c_\text{scene}\, T + \big(\text{albedo}\cdot\text{tint} + \text{emissive} + c_\text{sun}\big)(1 - T),
\qquad
c_\text{sun} = c_\text{dir}\,\big(\tfrac{1}{2}(\hat v \cdot \hat s) + \tfrac{1}{2}\big)^{k}
$$

where $\hat s$ points toward the sun (the directional light's travel direction, negated) and $k$ is the
lobe exponent. When the atmosphere is off, the sky-view sample is gated out (`useSkyLut = 0`) and the
flat `albedo` carries the in-scatter through the **same** code path — not a second shader variant. Sky
pixels at the far plane reconstruct a long ray, so the fog saturates toward `1 - maxOpacity` and the
already-painted sky fades naturally into the fog colour.

## Why before bloom

The composite is a single `StorageImageRwCompute` read-modify-write into `ViewTarget::offscreen`, in the
seam after the scene resolve and before the bloom pyramid — the same in-place
[compute post-process](../compute-post-process-pattern/) shape TAA and the tonemap use. Distant bright
emitters must be attenuated **first** so the energy-conserving bloom pyramid reads already-fogged HDR and
blooms them less; running bloom first would let far highlights bloom at full strength and then get
covered, punching physically wrong halos through the fog. Because it composites in scene-linear HDR
before the tonemap, authored densities are exposure-independent.

Opaque geometry is fogged via the depth read. Forward-blended transparents are not in the depth prepass,
so the depth-based composite would fog them at the *opaque* depth behind them, not their own; instead the
translucent permutation self-applies fog in its forward shader. In volumetric mode it samples the same
froxel integration volume — bound into the mesh light set (binding 11) — at its fragment's
$(\text{screenUV}, w)$, the identical mapping the composite uses, and applies the same
$c\,T + L_\text{scat}$ operator. No second code path: the shared $(\text{inscatter}, \text{transmittance})$
convention means the transparent read and the composite read are one operator against one volume.

## Volumetric fog: the froxel path

`fog.mode = volumetric` swaps the closed form for a **frustum-aligned froxel volume** — the
Wronski-2014 inject/integrate pipeline with Hillaire-2015 energy-conserving integration, so a
shadow-casting occluder throws real god-rays and volumetric shadows instead of a flat haze. It is
`analytic | volumetric` on one authoring surface, selected by the `mode` flag; there is no second fog
command and no double-count.

A fixed $160\times90\times128$ `rgba16f` grid partitions the frustum on the **same exponential-Z
distribution the clustered light cull uses**, so a fog froxel lands inside exactly one $16\times9\times24$
cull cluster and reads that cluster's already-built light list through the promoted public
`clusterIndexFor` — the same reuse [ReSTIR](../../lighting/restir/) relies on, no dedicated fog cull. Three
compute passes fill and consume it, inserted after the light-cull and before bloom:

- **`fog_inject.slang`** — one thread per froxel. Reconstruct the froxel-center world position from the
  exponential-Z view distance + the inverse view-proj, evaluate the extinction $\sigma_t$ (the **analytic
  height density injected as the base medium** — never applied again at composite — plus a constant
  `baseDensity`), and accumulate in-scatter over the cluster's directional sun (cascade PCF), point/spot
  lights (map shadows), and an ambient term, each weighted by the Henyey-Greenstein phase
  $p(\cos\theta, g)$. It writes $(L_\text{scat}, \sigma_t)$ with $L_\text{scat} = \sigma_s\sum
  L_i\,p_i + \text{emissive}$ and scattering coefficient $\sigma_s = \text{albedo}\cdot\sigma_t$. The
  light + shadow math is the **shared `lighting` module** (`fogPunctualInScatter` /
  `fogDirectionalInScatter`), one copy with the forward mesh path — the phase lobe substituting for the
  Cook-Torrance BRDF.
- **`fog_integrate.slang`** — one thread per XY column, marching front-to-back along Z with the
  energy-conserving analytic slice $S_\text{int} = (S - S\,e^{-\sigma d})/\max(\sigma, \varepsilon)$,
  accumulating $\text{accum} \mathrel{+}= T\,S_\text{int}$ and $T \mathrel{*}= e^{-\sigma d}$, storing
  $(\text{accum}, T)$ per slice. This is Hillaire's fix for the density-dependent banding of summing
  $S\cdot T$ at slice centres.
- **the composite** — `height_fog.slang`'s volumetric branch samples the integration volume trilinearly
  at $(\text{screenUV}, w)$ with $w = \log(z_\text{view}/n)/\log(f/n)$ (the exact inverse of the inject
  distance mapping) and applies the shared operator $c_\text{out} = c_\text{scene}\,T_\text{froxel} +
  L_\text{scat}$ — the same $(\text{inscatter}, \text{transmittance})$ convention the analytic branch
  uses, one transmittance ledger.

The scatter/extinction volume is kept separate from the integrated result precisely so the *linear*
scatter can be temporally reprojected (below); the sun stays on cascade PCF (the inline ray-query is a
later per-light opt-in).

To inspect the volume directly there is a **`fog` view mode** (`ViewMode::Fog`, alongside motion
vectors): the composite replaces the frame with the froxel in-scatter plus the fog opacity
$1 - T$ as a grey floor, so the god-ray in-scatter and the density structure read on their own. It
rides the existing debug-view path — `sa set-view-mode --mode fog` or the editor's View Modes menu —
and needs `mode = volumetric` (the grid is only populated there).

## Temporal reprojection: a coarse grid that reads smooth

A 64–128-slice grid is deliberately low-resolution, so a raw shaft *swims* as the camera moves. The fix
is [TAA](../../anti-aliasing/temporal-aa/)'s technique one level down, in the 3D grid: **reproject last
frame's linear scatter and blend a sliver of the fresh sample in.** The scatter volume is a persistent
**ping-pong pair** (`scatter[write]` this frame, `scatter[write ^ 1]` last frame's history) — the one
cross-frame-surviving 3D resource the froxel path owns, tracked across the frame boundary the way the
Global-SDF cascades are. Each inject:

- reconstructs the froxel-centre world position at a **jittered** sub-froxel offset — the *same* Halton
  NDC jitter TAA advances (`Renderer::active_view_jitter`), so the two temporal filters stay phase-locked
  and successive frames supersample distinct points inside each coarse froxel;
- reprojects that world position through the previous frame's view-proj to the previous froxel `uvw`,
  samples the history volume trilinearly, and blends $\text{out} = \text{lerp}(\text{hist},
  \text{fresh}, \text{historyBlend})$ with `historyBlend` $= 0.05$ — 95 % carried, 5 % fresh.

The blend is on the **linear** $(L_\text{scat}, \sigma_t)$, **never** the integrated transmittance: $T =
e^{-\int\sigma}$ is non-linear in $\sigma_t$, so lerping the integrated result accumulates the wrong energy
at a density edge (Hillaire's energy note). The front-to-back integrate still runs fresh each frame off
the reprojected volume. History resets to the fresh sample whole on a **camera cut** (the renderer's
`prev_view_proj_valid`, reused — no second cut detector), on the **first frame**, and on a **grid resize**
(a quality switch changes the froxel `uvw`, so the differently-shaped history can't be reprojected). When
TAA is off the jitter is zero and the fog degenerates to un-jittered froxel centres — correct, matching
the TAA-off path. Two opt-in firefly knobs ride the same UBO: `neighborhoodClamp` bounds the reprojected
history to a band of the fresh sample (ghost suppression for fast-moving lights), and `lightClamp` caps a
single light's per-froxel in-scatter before accumulation.

## Quality tiers

`quality` selects the froxel grid dimensions: `low` $128\times72\times64$, `medium` $160\times90\times64$,
`high` $160\times90\times128$. Z is the expensive axis (per-slice light evaluation), so `low`/`medium`
share $Z=64$ and only `high` doubles it; XY steps up for tile density (matching UE's
`r.VolumetricFog.GridSizeZ` being the primary cost lever). A tier switch reallocates the ping-pong history +
integration volumes, rebinds the composite's integration sample, and clears the history — the
`froxel_grid_matches_shader` CPU-mirror test locks the exponential-Z mapping for all three tiers.

## Per-light volumetric control

Every `PointLight`, `SpotLight`, and `DirectionalLight` carries two fog fields: `volumetricScattering` (a
per-light in-scatter multiplier, default `1.0` — a light's shaft brightness) and `castVolumetricShadow`
(default `true` — whether the light's shadow map gates its in-scatter, i.e. whether it throws a *shadowed*
god-ray). They pack into slots that already exist — no GPU record grows: the punctual pair rides
`GpuLight.spot_cos.zw`, the directional pair the reserved `LightUbo.extra_flags.zw`. They are plain
component fields edited by the **generic** `set-component-field` / `inspect` and rendered from two
`FIELD_HINTS` rows per light in the Inspector — no per-light command, no per-component handler.

## Local fog volumes

Scene-wide fog fills the whole frustum evenly; a **`FogVolume`** component adds a *bounded* patch of
extra density — a box of steam in a doorway, a sphere of haze around a fire — placed and moved with the
ordinary translate gizmo. It is a hecs component shaped like a [reflection probe](../../lighting/reflection-probes/):
positioned by the entity's `Transform`, spawned by `add-entity --preset fog-volume`, and edited entirely
through the generic `set-component-field` / `inspect` — there is no per-component command and no second
"local fog" primitive.

A volume injects into the **same froxel grid** during the Phase-3 density evaluation, so it is lit,
shadowed, temporally reprojected, and integrated by the identical stages — one medium, one integrate, no
bespoke local pass. Each frame `gather_fog_volumes` snapshots every `(Transform, FogVolume)` into a small
`FogVolumeGpu[]` SSBO (world transform baked, capped at `MAX_FOG_VOLUMES`); `fog_inject.slang` loops it
per froxel and, for each volume whose bounds contain the froxel-centre world position, adds a density
term to $\sigma_t$ alongside the analytic base:

$$
\sigma = \text{density}\cdot\underbrace{\text{smoothstep}(e, 0, d)}_{\text{soft edge}}\cdot
\underbrace{2^{-F_h\max(y_\text{local},\,0)}}_{\text{height slab}}\cdot\underbrace{n(\mathbf{p})}_{\text{erosion}}
$$

where $d$ is the signed distance to the box (`sdBox` in the volume's local frame) or sphere boundary and
$e = \text{edgeFalloff}$ — so density fades to zero over the soft margin instead of hard-clipping. An
optional per-volume exponential **height slab** ($F_h = \text{heightFalloff}$) lets a box double as a
local ground haze. The volume's own `albedo` folds into the shared $\sigma_s$, its `emissive` into the
in-scatter, and its `phaseG` blends into the froxel's Henyey-Greenstein anisotropy weighted by each
medium's share of $\sigma_t$ — a single phase evaluation stays correct where volumes overlap.

The **erosion** term $n$ is a two-octave sample of a precomputed tiling Perlin-Worley 3D noise texture
(baked once at renderer init, read through a linear-**repeat** sampler so a large volume tiles without
seams) at $\mathbf{p} = \text{worldPos}\cdot\text{noiseScale} - \text{wind}\cdot\text{speed}\cdot t$: a
low-frequency base minus a higher-frequency detail octave (`noiseDetail`), the whole modulated by
`noiseIntensity` (`0` skips the fetch — a uniform volume). `wind` + `speed` drift the pattern for animated
haze. Because both the bounds test and the noise sample run in the volume's local frame, a rotated box
erodes and softens along its own axes, matching the wireframe.

A meshless volume needs to be visible: it draws a **cloud glyph** billboard (always on top, so it stays
pickable even behind a wall) and a passive **box/sphere bounds wireframe** in the depth-tested overlay
range. Placement is the existing translate/rotate/scale native gizmo unchanged — extents are numeric
`vec3` Inspector fields with no box-resize handle, exactly like `Collider.halfExtents`. A `FogVolume` in
a scene with fog disabled contributes nothing; it is a strict addition to the froxel medium, which only
runs in `volumetric` mode.

## Driving it

`set-fog` is the one control command for scene-wide fog — a partial merge onto `environment.fog`. It
carries the analytic fields (`enabled`, `density`, `albedo`, `height`, `heightFalloff`, `startDistance`,
`maxOpacity`, `emissive`, `directionalColor`, `directionalExponent`, and the `layer2*` ground layer) and
the froxel fields (`mode` = `analytic | volumetric`, `baseDensity`, `scatterAlbedo`, `phaseG`, `quality` =
`low | medium | high`, `historyBlend`, `neighborhoodClamp`, `lightClamp`). Per-light shafts are the
lights' own `volumetricScattering` / `castVolumetricShadow` fields, not `set-fog`. Fog is
**scene state**, so it lives on `SceneEnvironment` and round-trips in the project `environment` block next
to the atmosphere — it is *not* a `renderSettings` field. It is scriptable from the `sa` CLI
(`sa set-fog --mode volumetric --baseDensity 0.05 --phaseG 0.6`), read back through `get-environment`, and
authored in the **Fog** section of the editor's Environment panel.

## In the code

| What | File | Symbols |
|---|---|---|
| Composite shader | `height_fog.slang` | `computeMain`, `layerOpticalDepth`, `skyViewTint`, `FogParams`, `FOG_MODE_VOLUMETRIC` |
| Froxel inject / integrate | `fog_inject.slang`, `fog_integrate.slang` | `injectFroxel`, `froxelCenterWorld`, energy-conserving slice scan |
| Reprojection + jitter + per-light + clamps | `fog_inject.slang` | `froxelSampleWorld`, `froxelUvwFromWorld`, `haltonZ`, temporal blend; `fogPunctualInScatter` reads `spotCos.zw`, `fogDirectionalInScatter` reads `extraFlags.zw` |
| Shared light/shadow module | `lighting.slang` | `clusterIndexFor`, `pcfShadow`, `pointShadow`, `hgPhase`, `fogPunctualInScatter`, `fogDirectionalInScatter`, `applyFroxelFog` (transparent path) |
| Froxel resource + ping-pong history + tiers | `froxel_fog.rs` | `FroxelFog` (`scatter[2]`, `write`, `history_ready`), `FroxelQuality`, `set_quality`, `advance_frame`, `FogGridParams`, `FROXEL_NEAR/FAR` |
| Per-light packing | `gpu_types.rs`, `lighting.rs` | `GpuLight.spot_cos` (`z`/`w`), `SceneLighting::directional_volumetric`, `LightUbo.extra_flags` (`z`/`w`) |
| Scene state | `environment.rs`, `component.rs` | `FogMode`, `FogQuality`, `FogSettings` (`quality`, `history_blend`, `neighborhood_clamp`, `light_clamp`); `PointLight`/`SpotLight`/`DirectionalLight` `volumetric_scattering` / `cast_volumetric_shadow` |
| Local fog volumes | `component.rs`, `render_scene.rs`, `froxel_fog.rs`, `fog_inject.slang`, `overlay.rs` | `FogVolume`, `FogShape`; `gather_fog_volumes`, `submit_fog_volumes`; `FogVolumeUpload`, `FogVolumeGpu`, `MAX_FOG_VOLUMES`; `sdBox`, `fogVolumeNoise` (inject loop); `BillboardKind::FogVolume`, `add_fog_icon` |
| Project round-trip | `serde.rs` | `fog_to_json`, `fog_from_json`, `fog_quality_name`, light + `FogVolume` `SceneSerialize` |
| Renderer seam + passes | `renderer.rs` | `FogRenderSettings`, `FogParams`, `add_fog_pass`, `add_froxel_fog_passes` (reprojection + jitter push), `set_fog` (quality realloc), `ViewMode::Fog` |
| Transparent-fog light-set binding | `lighting.rs`, `descriptors.rs` | `Lighting::bind_froxel_integration`, `set_frame_froxel_fog`, `LightUbo::froxel_fog`, light-set binding 11 |
| Frame push | `render_scene.rs` | `SceneRenderer::submit_fog` |
| PSOs + descriptor sets | `pipelines.rs`, `descriptors.rs` | `request_fog`, `request_fog_inject`, `request_fog_integrate`, `create_fog_layout` |
| Per-view set + UBO | `view_target.rs` | `ViewTarget::fog_set`, `write_fog`, `write_fog_integration` |
| Wire DTO + command | `dto.rs`, `commands_scene.rs` | `FogMode`, `FogQuality`, `SetFogParams`, the `set-fog` command (`historyBlend`/`lightClamp` range validation) |

## Related

- [Aerial perspective](../aerial-perspective/) — the atmosphere's long-range scattering on distant geometry, folded into this composite on the same transmittance ledger
- [Bloom](../bloom/) — the pyramid fog composites in front of, on already-fogged HDR
- [Procedural atmosphere](../../image-based-lighting/procedural-atmosphere/) — the sky-view LUT the in-scatter is tinted by
- [Compute post-process](../compute-post-process-pattern/) — the shared read-modify-write shape

> [!NOTE]
> In `volumetric` mode the analytic height term is the froxel **base medium**: the same height density is
> injected into the grid rather than applied a second time, so the closed form is a strict subset of the
> marched path, never a sibling of it — one transmittance ledger, no double-count.
