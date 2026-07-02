+++
title = 'Software ray trace'
weight = 4
math = true
+++

# Software ray trace

A software ray trace gathers a DDGI probe's incoming light by sphere-marching rays through the
engine's distance field in a compute shader, with no ray-tracing hardware involved. Each probe casts
64 rays; the march reuses the shared `sdf` module's `sampleField` — the full-resolution per-mesh MDF
for the near field and the [Global Distance Field](../../) clipmap beyond — so the entire DDGI path
runs on the llvmpipe dev GPU.

The dispatch is one thread per `(probe, ray)` pair: a 64-wide thread group over rays, one group per
probe. Each thread picks its ray direction, sphere-marches the field, and writes radiance and hit
distance into a `rays × probeCount` image that the blend passes read. Because the trace runs the same
three sets the per-pixel cone trace does (the bindless brick atlas, the light set's instance list +
GDF cascades, and the DDGI trace set), it reads exactly the geometry the rest of the lighting sees.

## Fibonacci-sphere ray directions

The 64 directions per probe are spread evenly over the sphere with a spherical Fibonacci sequence.
Direction $i$ of $n$ is

$$
\varphi = 2\pi \,\operatorname{frac}\!\big(i\,\phi^{-1} + r\big), \qquad
\cos\theta = 1 - \frac{2i + 1}{n}, \qquad
\sin\theta = \sqrt{1 - \cos^2\theta}
$$

with $\phi^{-1} = 0.618033\ldots$ the golden-ratio conjugate and $r$ a per-frame rotation offset.
The $\cos\theta$ term steps uniformly in height (equal-area latitude bands) and the golden angle
spirals the azimuth, so the points never clump.

The per-frame rotation $r = \operatorname{frac}(\text{frame} \cdot \phi^{-1})$ turns the fixed
64-ray set every frame, so the temporal blend averages many different directions over time. The
trace casts 64 rays per frame and resolves to effectively far more once converged.

## Sphere-marching the distance field

From a small offset off the probe centre, each step samples the signed distance to the nearest
surface and advances by that distance (clamped to a floor and a max leap). The field distance is the
largest safe step, so the march leaps across empty space and slows only as it nears a surface — the
hallmark of sphere tracing. A step where the distance drops below a surface epsilon (~10 cm) is a
hit; a march that runs past the max distance or the step budget is a miss.

The near ~2 m taps the per-mesh MDF at full resolution (where a thin wall would otherwise leak), and
beyond the handoff radius a single Global-SDF cascade tap gives the far-field distance independent of
how many meshes are in the scene. When the GDF is off, the march falls back to the per-mesh field
everywhere.

## Radiance on a hit, sky on a miss

On a hit the radiance is the surface's flat per-cell base color (read from the lite albedo cache at
the hit point) lit by a crude direct term — sky ambient plus a half-strength sun — plus a
multi-bounce contribution. On a miss the ray escaped and returns the sky color:

$$
L_\text{hit} = \rho \,(\tfrac12 L_\text{sky} + \tfrac12\, L_\text{sun}\, I_\text{sun}) \;+\; \tfrac14\,\rho \, E_\text{prev}
$$

A probe inside a sealed room occludes the sky intrinsically: most of its rays hit the enclosing
geometry and return bounce light rather than escaping to the sky, so the probe's irradiance stays dim
without any explicit visibility factor. Only rays that genuinely escape inject the sky color. This is
why the [IBL diffuse is *replaced*](../ddgi-overview/) by DDGI irradiance where the probe cage covers
a surface — the probe already carries its own occlusion. (A surface *outside* the cage falls back to
the full analytic IBL residual; there is no separate distance-field diffuse occlusion any more — the
SDF's only per-pixel role is [reflection occlusion](../distance-field-reflection-occlusion/).)

The albedo cache is a documented fidelity cap: a flat per-cell base color the GDF composite splats
for the finest cascade, with no normal, no view-dependent shading, and no emissive. It is not a
Surface Cache; where the GDF is off or the hit falls beyond the cache the trace uses a neutral
mid-gray.

## Multi-bounce by reading last frame

The $E_\text{prev}$ term multiplies the bounce light. At a hit, the shader samples last frame's
irradiance atlas in the ray direction and folds a quarter of it (times the hit albedo) back into this
ray's radiance. That atlas was itself fed by the previous frame's bounce, so each frame adds one
indirect bounce and the temporal blend settles to many bounces over a fraction of a second. The
feedback loop carries the bounces; no extra rays are cast.

The probe whose atlas it samples is the same probe doing the trace (`sampleProbeIrradiance`), a cheap
approximation of "gather the bounce at the hit point" that works because the volume is low-frequency.

Each thread writes `float4(radiance, hitDist)` to `rayOut[ray, physIndex]` — indexed by the probe's
**physical** atlas tile, the toroidal fold of its camera-relative cell. The radiance feeds
[the irradiance atlas](../irradiance-and-moment-atlases/) and the hit distance feeds the moment atlas
for Chebyshev visibility.

## In the code

| What | File | Symbols |
|---|---|---|
| The trace | `ddgi_trace.slang` | `computeMain` |
| The near/far field sample | `sdf.slang` | `sampleField` (near MDF → far GDF), `sdfSample`, `gdfDistance` |
| Ray directions | `ddgi_trace.slang` | `sphericalFibonacci` |
| Hit color + multi-bounce | `ddgi_trace.slang` | `sampleAlbedo`, `sampleProbeIrradiance` |
| Probe world position (toroidal) | `ddgi_trace.slang` | `probeWorldPos`, `wrapMod` |
| Trace graph pass | `rendering/src/renderer.rs` | `ddgi-trace` pass (`Renderer::add_ddgi_passes`) |

> [!NOTE]
> The bounce term reads the same probe's *previous* irradiance, not the irradiance at the actual
> hit point's nearest probe. It biases the result slightly but avoids a second field→probe lookup per
> ray. The volume's low spatial frequency hides the error.

## Related

- [DDGI overview](../ddgi-overview/) — where the trace sits in the per-frame pipeline
- [Probe atlases](../irradiance-and-moment-atlases/) — where the radiance and distance go
- [Distance field reflection occlusion](../distance-field-reflection-occlusion/) — the per-pixel cone that taps the same GDF clipmap the trace's far field reads
- [Acceleration structures](../raytracing-foundation/) — the BLAS/TLAS path this trace stands in for
