+++
title = 'Per-cluster cap'
weight = 10
+++

# Per-cluster cap

A per-cluster cap is the fixed maximum number of lights a single cluster can record. Clustered
lighting stores each froxel's overlapping lights in a fixed-size array, and that array length is
the cap. In Anima a cluster holds at most 64 punctual lights.

A fixed array keeps the cluster buffer a flat, predictable allocation that the GPU indexes
directly. The trade is a hard ceiling: when more than 64 lights overlap one froxel, the surplus
cannot be stored.

## A fixed array per cluster

The cap is `MAX_LIGHTS_PER_CLUSTER = 64`, declared in `light_cull.slang`, `lighting.slang`, and
`lighting.rs` (as `MAX_LIGHTS_PER_CLUSTER`). The cluster struct is a `count` plus a fixed
array of that many indices:

```hlsl
struct Cluster
{
    uint count;
    uint indices[MAX_LIGHTS_PER_CLUSTER];   // 64
};
```

The whole cluster buffer is sized `CLUSTER_COUNT * CLUSTER_STRIDE`, where
`CLUSTER_STRIDE = (1 + MAX_LIGHTS_PER_CLUSTER) * size_of::<u32>()` — one count word plus 64 index
words per froxel. The fixed stride lets a froxel's data be reached by a single multiply, with no
per-cluster offset table.

## The silent drop

The [cull pass](../clustered-forward/) appends a light only while there is room. Past 64 it
stops writing but keeps scanning, so further overlapping lights are never recorded:

```hlsl
if (dot(delta, delta) <= radius * radius)
{
    if (count < MAX_LIGHTS_PER_CLUSTER)   // the cap
    {
        clusters[clusterIndex].indices[count] = i;
        count = count + 1;
    }
}
```

There is no warning and no spill list. The fragment shader reads `count` and loops exactly that
many. The surviving lights are whichever ones the cull reached first — lowest scene index, not
nearest or brightest. A 65th overlapping light produces no lighting in that froxel.

## When the cap matters

For typical scenes the cap is invisible, since 64 lights rarely touch one
$16\times9\times24$ froxel. It matters when many large-`range` lights fall into the same view
region — a dense cluster of point lights, or a few oversized ranges that each land in most
froxels.

The cap is also the one case where the clustered path and the
[brute-force fallback](../brute-force-fallback/) diverge. Below the cap the two are
pixel-identical, because every light the clustered loop skips contributes exactly zero. Above
the cap, the clustered loop drops lights the brute-force loop still sees. `sa set-clustered 0`
forces the brute-force path and reveals the difference.

## In the code

| What | File | Symbols |
|---|---|---|
| Cap + array | `engine/assets/shaders/light_cull.slang` | `MAX_LIGHTS_PER_CLUSTER`, `Cluster`, the `count <` guard |
| Matching shader-side cap | `engine/assets/shaders/lighting.slang` | `MAX_LIGHTS_PER_CLUSTER`, `Cluster` |
| Buffer sizing | `engine/crates/rendering/src/lighting/` | `MAX_LIGHTS_PER_CLUSTER`, `CLUSTER_STRIDE`, `CLUSTER_COUNT` |

> [!TIP]
> Raising the cap means changing `MAX_LIGHTS_PER_CLUSTER` in both shaders and in `lighting.rs`
> together. The cluster buffer is `CLUSTER_STRIDE`-sized off the Rust constant, so a mismatch
> corrupts the per-froxel stride; `cluster_grid_matches_shader` pins the constants in a test.

## Related

- [Clustered forward](../clustered-forward/) — the cull pass that fills the list
- [Cluster indexing](../cluster-indexing/) — how the fragment reads the bounded `count`
- [Brute-force fallback](../brute-force-fallback/) — the path that ignores the cap entirely
