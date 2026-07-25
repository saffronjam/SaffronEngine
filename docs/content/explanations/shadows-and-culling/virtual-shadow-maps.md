+++
title = 'Virtual shadow maps'
weight = 0
math = true
+++

# Virtual shadow maps

Every shadow in Anima renders into one physical depth atlas, addressed through per-light virtual
page spaces. A light's shadow map never exists as a whole image: only the 128×128-texel pages that
receivers actually look up get a physical tile, get rasterized, and stay cached until their content
goes stale. The approach follows the virtual-texturing treatment of shadow maps in
[UE5's virtual shadow maps](https://dev.epicgames.com/documentation/en-us/unreal-engine/virtual-shadow-maps-in-unreal-engine).

## The vocabulary

The atlas is a single 4,096² `D32` image: 32×32 physical tiles of 128² texels. Three kinds of
virtual space map logical pages onto it:

| Space | Shape | Pages |
|---|---|---|
| Directional | 8 camera-snapped clip levels, level $k$ spanning $32 \cdot 2^k$ m | 32×32 per level |
| Spot | one perspective plane per shadowed spot | 16×16 |
| Point | 6 cube faces, each a 90° perspective plane | 8×8 per face |

Each frame publishes a page table — one `u32` per logical page, packing a resident bit and the
physical tile index — at a device address in the light UBO. Samplers resolve a world position to a
page, read its entry, and take a filtered depth comparison inside the tile, or fall back when the
page is absent.

## Residency

`VsmResidency` is the CPU authority over the atlas. A demand for a page returns its tile when
resident, allocates a free tile otherwise, and evicts the least-recently-demanded page when the
atlas is full. An evicted tile cools for two frames before reuse, so an in-flight frame never
samples a tile that changed owners. Fresh allocations are dirty; dirty pages drain into the frame's
render list under a per-frame budget, and pages the graph could not rasterize re-mark themselves.

Whole spaces invalidate when their mapping moves: a directional level whose snapped window shifted,
the spot when its transform changes, all six point faces when the light moves or its range changes.

## Receiver demand

Pages are requested by the receivers that need them. A compute pass walks the camera depth buffer:
each pixel reconstructs its world position, projects into every armed light space, and sets that
page's bit in a demand bitmap — for the directional light it picks the finest clip level whose
texel density matches the pixel's world-space footprint. A second pass compacts set bits into a
mapped request ring the CPU drains at the frame fence:

```
mark:    depth pixel → world → light space → InterlockedOr(bitmap[page])
compact: firstbitlow over the bitmap → append raw table indices, clear
drain:   dedup, decode via vsm_demand_key, demand into the residency
```

A coarse bootstrap ring on the outermost directional level covers the first frames and un-marked
regions; everything else is demand-driven.

## Page rendering

Dirty pages group by space: each directional level, the spot, and each point face runs its own
GPU cull → traversal → binning chain against that space's frustum, then one graphics pass rasterizes
each page with viewport and scissor clamped to its atlas tile. The per-page transform is the space's
sub-window: an ortho window for a directional page, a crop matrix times the light transform for a
spot or point page (`vsm_page_crop`). The draws replay the binned executor stream through
`record_executor_depth_family` — the same recorder every depth-family pass uses.

The scene pass declares the atlas `SampledRead`, so the [render graph](../../frame-and-render-graph/render-graph-overview/)
derives the `DepthWrite → ShaderReadOnly` transition; no barrier is hand-written.

Page draws share the depth family's fragment, so alpha-clipped and thin-sheet surfaces shadow
through the same canonical-coverage test the camera uses, and the executor vertex path applies the
same wind deformation. Displacement-tessellated surfaces shadow from their base geometry; the
amplified detail is a camera-pass refinement.

## Observability

`render-stats` reports the previous frame's residency activity as the `vsm` block: `requested`,
`hits`, `allocated`, `rendered`, `dirtied`, `evicted`, and `overflow` (demands the full atlas could
not satisfy). The `set-shadows` master toggle gates the whole system; off publishes a disabled
table, and every sampler reads unshadowed.

## In the code

| What | File | Symbols |
|---|---|---|
| Constants + page keys | `crates/rendering/src/vsm.rs` | `VSM_PAGE_SIZE`, `VSM_ATLAS_SIZE`, `VsmPageKey` |
| CPU residency | `crates/rendering/src/vsm.rs` | `VsmResidency`, `VsmCounters` |
| Atlas + page table | `crates/rendering/src/vsm.rs` | `VsmGpu`, `publish_table`, `vsm_table_entry` |
| GPU demand | `crates/rendering/src/vsm.rs`, `vsm_demand.slang`, `vsm_demand_compact.slang` | `VsmDemand`, `vsm_demand_key` |
| Frame preparation | `crates/rendering/src/renderer.rs` | `prepare_vsm_frame` |
| Page raster passes | `crates/rendering/src/renderer.rs` | `add_vsm_page_passes`, `vsm_page_crop` |
| Samplers | `assets/shaders/lighting_common.slang` | `vsmSampleDirectional`, `vsmSampleSpot`, `vsmSamplePoint`, `vsmTilePcf` |

## Related

- [Directional shadows](../directional-shadows/) — the clip-level space the sun samples through
- [Spot-light shadows](../spot-light-shadows/) — the projective page space of the shadowed spot
- [Point-light shadows](../point-light-cube-shadows/) — six face spaces behind the cube mapping
- [PCF filtering](../pcf-filtering/) — the in-tile comparison kernel
- [Shadow bias](../shadow-bias/) — the depth bias applied while pages rasterize
