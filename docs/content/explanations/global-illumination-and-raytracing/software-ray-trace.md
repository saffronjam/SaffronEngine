+++
title = 'Software ray trace'
weight = 4
math = true
+++

# Software ray trace

A software ray trace gathers incoming light for [DDGI probes](../ddgi-overview/) without
ray-tracing extensions. A compute shader uses
[sphere tracing](https://doi.org/10.1007/s003710050084) over the shared distance field: per-mesh
distance fields (MDFs) cover nearby geometry, and the Global Distance Field (GDF) clipmap covers
the far field. Each trace pass refreshes 512 of the 2,048 probes, with 64 rays per refreshed probe.

## Dispatch and persistent rays

`ddgi-trace` dispatches `(1, 512, 1)` workgroups with `numthreads(64, 1, 1)`. `tid.x` selects a ray,
while `tid.y` selects a probe inside the round-robin window. The window offset advances by 512 each
frame and wraps over the 16×8×16 volume, so every probe receives a new ray set once every four
frames.

Each thread writes `float4(radiance, hitDistance)` to a persistent 64×2,048 ray image. The row uses
the probe's physical atlas index, preserving the toroidal mapping when the camera-centred volume
scrolls. The irradiance and distance blend shaders dispatch across their atlases, but `probeTraced`
leaves tiles outside the current 512-probe window unchanged.

The trace binds three descriptor sets. The bindless set supplies MDF bricks, the light set supplies
the SDF instance list and GDF cascades, and the trace set supplies the GDF albedo cache, previous
irradiance atlas, and ray output image. Render-graph reads on the cascades and albedo cache order the
trace after this frame's GDF composite.

## Fibonacci-sphere ray directions

The 64 directions per probe use
[spherical Fibonacci mapping](https://doi.org/10.1145/2816795.2818131). Direction $i$ of $n$ is

$$
\varphi = 2\pi \,\operatorname{frac}\!\big(i\,\phi^{-1} + r\big), \qquad
\cos\theta = 1 - \frac{2i + 1}{n}, \qquad
\sin\theta = \sqrt{1 - \cos^2\theta}
$$

with $\phi^{-1} = 0.618033\ldots$ the golden-ratio conjugate and $r$ a per-frame rotation offset.
The $\cos\theta$ term steps uniformly in height, and the golden angle advances the azimuth.

The per-frame rotation $r = \operatorname{frac}(\text{frame} \cdot \phi^{-1})$ turns the fixed
ray set. The blend shaders reconstruct this same rotation for probes refreshed in the current
window. Untraced probes keep their last integrated atlas values rather than interpreting stored
rays with a different rotation.

## Sphere-marching the distance field

The main march starts 0.3 metres from the probe and takes at most 64 steps. Each step advances by
the sampled distance clamped to `[0.05, 4.0]` metres. A distance below 0.1 metres records a hit;
reaching 40 metres or exhausting the step count records a miss.

| Parameter | Value |
|---|---:|
| Start distance | 0.3 m |
| Surface epsilon | 0.1 m |
| Step range | 0.05–4.0 m |
| Maximum distance | 40 m |
| Maximum steps | 64 |

`sampleField` reads per-mesh MDFs for the first 2 metres of travel. Beyond that handoff, it uses one
GDF cascade sample when the point lies inside clipmap coverage. A disabled GDF or a point outside
the coarsest cascade falls back to the MDF path.

## Radiance on a hit, sky on a miss

At a hit, forward differences of the distance field estimate the surface normal. A secondary
sphere march toward the sun tests visibility for up to 24 steps and 20 metres. The hit radiance is

$$
L_\text{hit} = \rho\left(L_\text{sun} I_\text{sun} V_\text{sun}
\max(0, n\!\cdot\!l) + 0.5 E_\text{prev}(\omega)\right).
$$

Here $\rho$ comes from the lite per-cell albedo cache. The sun term includes normal incidence and
the secondary march's visibility. No sky ambient is added at a hit. A miss returns
$L_\text{sky}$, so sky energy enters the probe volume only along rays that escape the distance field.

## Porous aggregate matter

Foliage-classed matter never hardens the distance field. Instead the composite splats its density
into per-cascade occupancy volumes, and both marches accumulate
[Beer–Lambert](https://en.wikipedia.org/wiki/Beer%E2%80%93Lambert_law) extinction through it via the
shared `sdfExtinctionStep` (full density transmits about 5% per metre). A primary ray dims what it
sees beyond a canopy, and a ray whose transmittance saturates inside dense matter records an
aggregate hit: the cell's cached colour, an occupancy-gradient normal, and the sun dimmed by the
canopy above.

That density is **derived, not authored**. A plant seen close up is triangles; far away it is one
aggregate voxel. Were the voxel's occupancy an independently authored number, it would transmit a
different amount of light than the leaves it stands in for, and the plant would change brightness at
the switch.

So occupancy is solved from the surface's own optics: the density at which marching the sheet's mean
thickness transmits the sheet's mean transmission. Both representations then describe one optical
depth, and since the same extinction step serves indirect irradiance, sky visibility and the
reflection cone, each inherits that continuity.

The extinction coefficient is achromatic, so one density stands for three colour channels. The
channel mean is the reduction used, because it preserves total transmitted energy rather than
favouring a perceptual weighting the marches do not apply.

Testing that continuity needs care. The cut follows projected appearance error, so reaching the
aggregate form in a normal frame means moving the camera away — which shrinks the subject at the
same moment it coarsens it, and the difference cannot separate the two.

The cut is pinned instead. `SAFFRON_CUT_OVERRIDE` forces the traversal to stop refining or to refine
fully, leaving camera, scene and lighting identical across two runs. The two cuts then render
visibly different pictures carrying the same amount of light.

The sun march multiplies its transmittance into $V_\text{sun}$, so surfaces under foliage receive
dappled rather than absent sunlight. The
[DFAO cones and the reflection cone](../distance-field-reflection-occlusion/) apply the same
extinction step.

Because a hit carries no direct sky term, enclosed probes do not receive analytic sky through
blocked ray directions. Surfaces inside the probe cage use DDGI instead of analytic diffuse IBL;
surfaces outside the cage retain the IBL fallback.

The albedo cache stores a flat base colour per covered cell in the finest GDF cascade. It carries no
normal, emissive term, or view-dependent material response. A disabled GDF, uncovered cell, or hit
outside the cache uses neutral `float3(0.5)`.

## Multi-bounce by reading last frame

`sampleProbeIrradiance` reads the tracing probe's previous irradiance in the current ray direction.
Multiplying that value by `0.5 * albedo` feeds indirect energy back into the next probe update. The
same-probe lookup is an approximation of irradiance at the hit point; it does not select the probe
nearest to that point.

The feedback loop propagates energy over successive updates without another ray set. Radiance feeds
[the irradiance atlas](../irradiance-and-moment-atlases/), while hit distance feeds the first and
second moments used for Chebyshev visibility.

## In the code

| What | File | Symbols |
|---|---|---|
| The trace | `ddgi_trace.slang` | `computeMain` |
| The near/far field sample | `sdf.slang` | `sampleField` (near MDF → far GDF), `sdfSample`, `gdfDistanceOccupancy` |
| Ray directions | `ddgi_trace.slang` | `sphericalFibonacci` |
| Hit color + multi-bounce | `ddgi_trace.slang` | `sampleAlbedo`, `sampleProbeIrradiance` |
| Probe world position (toroidal) | `ddgi_trace.slang` | `probeWorldPos`, `wrapMod` |
| Round-robin constants | `rendering/src/ddgi/` | `DDGI_PROBE_BUDGET`, `DDGI_PROBE_CYCLE`, `Ddgi::trace_push` |
| Trace graph pass | `rendering/src/renderer/gi_passes.rs` | `Renderer::add_ddgi_passes` |
| Occluder list production | `gi_occluder_scatter.slang`, `gi_occluder_micro.slang` | `computeMain`, `GiOccluderScatterPush`, `GiOccluderMicroPush` |
| Occluder-list pressure | `rendering/src/renderer/` | `MAX_SDF_INSTANCES`, `sdf_instances_dropped` |
| Occupancy parity | `material/src/lib.rs`, `assets/src/render_material.rs` | `parity_occupancy`, `aggregate_transmittance`, `AGGREGATE_EXTINCTION_PER_METER`, `derive_parity_occupancy` |
| Updated-tile filter | `ddgi_blend_irradiance.slang`, `ddgi_blend_distance.slang` | `probeTraced` |

## Related

- [DDGI overview](../ddgi-overview/) — where the trace sits in the per-frame pipeline
- [Probe atlases](../irradiance-and-moment-atlases/) — where the radiance and distance go
- [Distance field reflection occlusion](../distance-field-reflection-occlusion/) — the per-pixel cone that taps the same GDF clipmap the trace's far field reads
- [Acceleration structures](../raytracing-foundation/) — the hardware visibility path used by ray-query features
