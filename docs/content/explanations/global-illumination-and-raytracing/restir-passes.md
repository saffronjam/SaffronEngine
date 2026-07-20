+++
title = 'ReSTIR passes'
weight = 10
math = true
+++

# ReSTIR passes

[ReSTIR DI](https://research.nvidia.com/publication/2020-07_spatiotemporal-reservoir-resampling-real-time-ray-tracing-dynamic-direct)
reduces many-light direct illumination to one selected punctual light per opaque pixel. Three
compute passes sample candidates, reuse reservoirs across space and time, and test the selected
light for visibility. Each pass dispatches one thread per pixel in 8×8 groups.

The chain runs when ReSTIR is enabled and the active view has ray-query support, a TLAS, a G-buffer,
and clustered-light data. Otherwise, opaque meshes use the
[clustered-forward](../../lighting-and-brdf/clustered-forward/) punctual-light loop.

## Per-view state

Each view owns three storage buffers with one 32-byte `Reservoir` per pixel. A reservoir records the
selected light index, normalized weight $W$, accumulated weight, sample count $M$, and selected target
value. The view also owns an RGBA16F image containing the resolved direct radiance.

| Buffer | Written by | Read by |
|---|---|---|
| `initial` | Initial sampling | Reuse |
| `combined` | Reuse | Resolve |
| `previous` | Resolve | Reuse in the next frame |

Enabling ReSTIR or rebuilding a resized view invalidates temporal history. The first reuse after a
reset therefore uses the current frame and spatial neighbours only.

## Initial sampling

`restir_initial.slang` reconstructs the world-space surface from the view normal and view depth. It
draws $K=16$ candidates uniformly from the pixel's froxel light list. An empty froxel falls back to
the full punctual-light list, while a background pixel or a scene without punctual lights produces
an empty reservoir.

The target value $\hat p$ is the luminance of the light's unshadowed diffuse contribution. It includes
intensity, $n\cdot l$, inverse-square attenuation with the range window, and the spotlight cone. Each
candidate receives importance weight $w=\hat p/p_{source}$ and competes through weighted reservoir
sampling.

After $K$ candidates, the pass stores

$$
W = \frac{\sum_i w_i}{K\,\hat p_{selected}}.
$$

No visibility ray is traced in this pass. Deferring visibility keeps candidate evaluation cheap even
when a froxel contains many lights.

## Temporal and spatial reuse

`restir_reuse.slang` starts with the current pixel's `initial` reservoir. If history is valid, the
motion vector maps the pixel to a `previous` reservoir. Four random offsets also select candidate
reservoirs from the current `initial` buffer within a 16-pixel radius.

A spatial candidate passes only when its view-depth difference is at most `0.5` and its view-normal
dot product is at least `0.9`. Each accepted reservoir is evaluated at the destination surface and
contributes

$$
w_{merge}=\hat p_{destination}\,W_{source}\,M_{source}.
$$

The pass clamps each reused source count to $M=20$ before merging. This bounds the influence of
accumulated history and keeps new samples relevant when lights or geometry move. The combined
reservoir receives a normalized weight from the merged sum:

$$
W = \frac{\sum w_{merge}}{M_{combined}\,\hat p_{selected}}.
$$

## Visibility and material response

`restir_resolve.slang` reads the selected light from `combined` and traces one inline ray query toward
it. The ray starts `0.02` world units along the light direction, stops at the light distance, accepts
the first triangle hit, and skips procedural primitives. A hit writes zero direct radiance.

For a visible light, the pass writes light color and intensity multiplied by attenuation, spotlight
cone, $n\cdot l$, and reservoir weight $W$. It does not apply the surface albedo. The mesh fragment
shader samples the radiance image and multiplies it by `albedo / PI`, replacing its ordinary punctual
light loop for opaque surfaces.

Resolve also copies `combined` into `previous`. This copy supplies the temporal candidate for the
next frame.

## Graph ordering

The render graph imports `combined` as a sentinel resource for the reservoir chain. Initial sampling
declares `StorageWriteCompute` on the sentinel, and reuse and resolve declare
`StorageReadCompute`. The motion-vector image and resolved-radiance image are tracked as their own
graph resources.

The shader descriptor sets bind the actual `initial`, `combined`, and `previous` buffers. The
sentinel declarations order the pass bodies; the resolve shader performs the cross-frame copy to
`previous` directly.

## In the code

| What | File | Symbols |
|---|---|---|
| Reservoir layout and limits | `rendering/src/restir.rs` | `Reservoir`, `RESTIR_CANDIDATE_COUNT`, `RESTIR_SPATIAL_RADIUS`, `RESTIR_MAX_M` |
| Candidate sampling | `assets/shaders/restir_initial.slang` | `computeMain`, `targetContribution`, `clusterIndexFor` |
| Temporal and spatial reuse | `assets/shaders/restir_reuse.slang` | `computeMain`, `combineInto` |
| Visibility resolve | `assets/shaders/restir_resolve.slang` | `computeMain`, `rayShadow` |
| Graph integration | `rendering/src/renderer.rs` | `Renderer::add_restir_passes` |
| Material integration | `assets/shaders/lighting.slang` | `evalLighting` |

## Related

- [ReSTIR](../restir-overview/) explains reservoir resampling in the lighting pipeline.
- [Clustered forward](../../lighting-and-brdf/clustered-forward/) supplies the initial candidate lists.
- [Ray-query shadows](../ray-query-shadows/) covers inline visibility rays and the TLAS.
- [Motion vectors](../../screen-space-and-post/) provide temporal reprojection.
