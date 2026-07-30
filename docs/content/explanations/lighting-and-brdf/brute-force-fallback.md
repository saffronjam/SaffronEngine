+++
title = 'Brute-force fallback'
weight = 7
+++

# Brute-force fallback

The brute-force fallback is a shading path that loops over every light in the scene for each
fragment, rather than only the lights assigned to that fragment's froxel. It is the reference
implementation: simpler, slower, and the ground truth the [clustered](../clustered-forward/)
path is validated against. `sa set-clustered 0` selects it at runtime.

## One flag, two loops

The flag rides in the cluster params (`screen_size.z`, read in the shader as
`clusterParams.screenSize.z`), set from `use_clustered` in `Lighting::set_cluster_camera`. When
it is zero there is no cluster lookup, and the loop runs over the full light count from the
lighting UBO:

```hlsl
else
{
    for (uint i = 0; i < globals.counts.x; i = i + 1)
    {
        lo += punctual(lights[i], i, input.worldPos, n, v, albedo, metallic, roughness);
    }
}
```

The body is the same `punctual(...)` call the clustered loop makes; only the iteration set
differs. When the flag is off, no cull dispatch is armed
(`take_cluster_dispatch_pending` returns false), so the [cull pass](../clustered-forward/) is not
added to the render graph that frame.

## Why the two paths match

The clustered and brute-force paths produce pixel-identical images by construction. Both call
the same `punctual` → `brdf` functions, so a given light shades a fragment identically either
way. A light is added to a froxel only when its `range` sphere overlaps, and punctual
[attenuation](../punctual-lights-and-attenuation/) is windowed to reach exactly zero at
`range`. Any light the clustered loop skips therefore contributes exactly zero.

Summing a set of lights where the omitted terms are all zero equals summing the full set.
Float summation order can differ, but every dropped term is a hard zero and cannot perturb the
result. Toggling `sa set-clustered` between 1 and 0 reproduces the same frame.

## What it is for

Cluster culling has several places to get a sign or a slice boundary wrong: the exponential Z
mapping, the AABB construction, the flat index encoding. Diffing a clustered frame against the
brute-force frame is the cheapest way to catch those errors. It is also the safe default when
the light count is small enough that culling is not worth the compute dispatch.

## In the code

| What | File | Symbols |
|---|---|---|
| The two loops | `engine/assets/shaders/lighting.slang` | `evalLighting` — `clusterParams.screenSize.z` branch |
| Shared per-light body | `engine/assets/shaders/lighting.slang` | `punctual`, `brdf` |
| The flag | `engine/crates/rendering/src/renderer/` | `Renderer::set_clustered`, `Renderer::clustered_enabled` |
| Skipping the cull pass | `engine/crates/rendering/src/lighting/` | `Lighting::use_clustered`, `take_cluster_dispatch_pending` |
| `set-clustered` control command | `engine/crates/control/src/commands_render/` | the `set-clustered` registration |

> [!TIP]
> The brute-force loop reads `globals.counts.x` (the full count); the clustered loop reads a
> per-froxel `count` bounded by [the 64-light cap](../per-cluster-cap/). With more than 64
> lights overlapping a froxel the two paths can diverge — the brute-force loop sees every
> light, the clustered one drops the overflow. Pixel-identity holds below the cap.

## Related

- [Clustered forward](../clustered-forward/) — the optimized path this validates
- [Cluster indexing](../cluster-indexing/) — what the clustered branch does instead
- [Per-cluster cap](../per-cluster-cap/) — the one case where the paths can differ
- [Punctual lights and attenuation](../punctual-lights-and-attenuation/) — the body both loops share
