+++
title = 'Cluster indexing'
weight = 6
math = true
+++

# Cluster indexing

Cluster indexing maps a fragment's screen position and view-space depth to the flat index of its
froxel. [Clustered shading](https://diglib.eg.org/items/6342d4d6-5220-4376-a5c6-a153058f4a3c)
partitions the view frustum into a 3D grid and assigns each cell a light list. A shading fragment must
locate its cell to read the right list.

The mapping must invert exactly the slicing the [cull pass](../clustered-forward/) used. A fragment
that resolves to a different froxel than the cull assigned reads a light list built for another
region of the frustum. Anima implements the mapping as `clusterIndexFor` in `lighting.slang`.

## The three coordinates

A fragment's cluster is a triple $(x, y, z)$: a screen tile in X and Y, and a depth slice in Z. The
renderer uses a fixed $16 \times 9 \times 24$ grid, for 3,456 clusters. The flat index packs them as

$$
\text{index} = x + y \cdot G_x + z \cdot G_x G_y
$$

the same encoding the cull pass unpacks. The X and Y tiles are a division of the pixel position
(`SV_Position.xy`) by the tile size, with a `min` that clamps the edge fragment so a pixel exactly
on the right or bottom border does not index one tile past the grid.

## The exponential Z slice

The depth slice must match the cull pass's exponential planes. Define view-space depth as
$d = \max(-z_\text{view}, n)$, the negated view-Z floored at the near plane. The slice is the
logarithmic inverse of $z_i = -n(f/n)^{i/N}$:

$$
\text{slice} = \left\lfloor \frac{\ln(d / n)}{\ln(f / n)} \cdot N \right\rfloor
$$

```hlsl
float depth = max(-viewZ, near);
uint zSlice = uint(log(depth / near) / log(far / near) * float(clusterParams.gridSize.z));
zSlice = min(zSlice, clusterParams.gridSize.z - 1);
```

The $\log$ undoes the $\text{pow}$ the cull used; the two formulas are mirror images. View-space
depth is required here: slicing on screen-space (NDC) depth places a fragment in a different slice
than the one the cull assigned its light to, so lights near slice boundaries pop.

The fragment first needs `viewZ`, obtained by transforming its world position by the cached view
matrix, then loops over its cluster's lights:

```hlsl
float viewZ = mul(clusterParams.view, float4(input.worldPos, 1.0)).z;
uint clusterIndex = clusterIndexFor(clusterParams, input.position.xy, viewZ);
uint count = clusters[clusterIndex].count;
for (uint i = 0; i < count; i = i + 1)
    lo += punctual(lights[clusters[clusterIndex].indices[i]], ..., albedo, metallic, roughness);
```

The mesh forward path and volumetric-fog injection call this shared shader function. ReSTIR initial
sampling carries the same calculation in `restir_initial.slang`, while `froxel_to_cluster` provides
the CPU mirror used by fog-grid tests.

## In the code

| What | File | Symbols |
|---|---|---|
| Pixel + view-Z → flat index | `engine/assets/shaders/lighting.slang` | `clusterIndexFor` |
| View-Z transform + the loop | `engine/assets/shaders/lighting.slang` | `evalLighting` — `clusterParams.view`, `clusters[...]` |
| Matching forward slicing | `engine/assets/shaders/light_cull.slang` | `computeMain` — `tileNear`/`tileFar` `pow` |
| Grid dims + z planes upload | `engine/crates/rendering/src/lighting.rs` | `Lighting::set_cluster_camera` — `ClusterParams::grid_size`, `ClusterParams::z_planes` |
| Fog CPU mirror | `engine/crates/rendering/src/froxel_fog.rs` | `froxel_to_cluster` |
| ReSTIR mirror | `engine/assets/shaders/restir_initial.slang` | `clusterIndexFor` |

> [!TIP]
> The Z slice uses view-space depth, not the rasterizer's NDC depth. The $\log$/$\text{pow}$
> pair across the index and cull shaders must use the same `near`/`far`, or fragments and their
> lights land in different slices.

## Related

- [Clustered forward](../clustered-forward/) — the cull pass this inverts
- [Per-cluster cap](../per-cluster-cap/) — what bounds the `count` this loop reads
- [Punctual lights and attenuation](../punctual-lights-and-attenuation/) — what the loop calls per light
