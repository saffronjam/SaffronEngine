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

Whole spaces react when their mapping moves: a directional level whose snapped window shifted and
the spot transform invalidate their pages, while a point-light transform dirties the resident cube
faces. The point-light key tracks exact position and range changes. A small point-light move keeps
the last coherent face visible during refresh; a larger move withholds dirty faces until the
frame's refresh pass schedules them, so old face contents are not sampled with a visibly different
light transform.

## What a mover dirties

A static page persists until something invalidates it, so the cache is only as good as the
invalidation is tight. A caster that moved, arrived, or left dirties the pages its geometry actually
covered — not its whole footprint.

The persistent scene reports one swept world box per **cooked leaf page** of the moved instance's
prototype: one triangle cluster or one aggregate brick each. The box spans the deformed extent the
cook proved the payload can reach, over both the current and previous transform, so it covers where
the content was as well as where it is.

Interior pages are left out because their cooked bounds enclose their whole subtree. Including them
would hand the consumer the prototype's own root box beside every tight cluster box, and the union
would be the root box. Dropping them stays conservative: a coarse representation's vertices are
convex combinations of the fine ones it simplifies, so it lies inside the union of its children's
boxes.

Each box is projected into the directional clip levels by its eight **corners**, and only the page
rectangle that extent covers is marked. A bounding sphere through the box would be up to √3 wider
on every axis, and each extra page in that margin is re-rasterized for geometry that never enters
it. A caster's aspect decides the cost, so an edge-on plank dirties a strip rather than a disc
(`VsmDirectionalSpace::directional_page_span`).

Spot and point pages publish as coherent projective grids. The spot plane and each point cube face
are completed as page grids, and the sampler sees a grid only when every page in that grid is
resident, rendered, and clean. A moved caster therefore dirties each armed projective grid once per
frame; the directional levels keep the per-leaf page precision.

Point faces also drain as coherent render units. One cube face is 64 pages, matching the default
page budget, so a moving point light refreshes one complete face instead of scattering work across
several partial, non-publishable faces. Receiver-demanded faces drain first, so visible shadow
receivers steer the next refreshed face. A point-light transform change expands the frame's page
budget to cover every receiver-demanded face.

Continuous wind is the one dirty source that is not per-caster: it re-marks the directional levels
fine enough to resolve sway, capped at `VSM_DYNAMIC_MAX_LEVEL`, since levels whose texels span half
a metre or more cannot show it. The per-frame render budget paces the resulting churn, and pages
left un-rasterized re-mark themselves rather than showing stale depth.

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
regions. A receiver demand inside the spot plane completes the spot grid, and a receiver demand
inside a point face completes that one face. Other point faces stay absent until receivers ask for
them.

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

`render-stats` reports the previous frame's residency activity as the `vsm` block. The top-level
counts summarize the atlas: `requested`, `hits`, `allocated`, `rendered`, `dirtied`, `evicted`, and
`overflow` (demands the full atlas could not satisfy). The `directional`, `spot`, and `point`
objects split those counts by page family and add `invalidated`, which separates address-space
drops from LRU eviction.

The same block splits causes: `requestedBootstrap`, `requestedProjective`, and
`requestedReceiver` identify who asked for pages; `dirtiedRestaged`, `dirtiedDynamic`, and
`dirtiedMoved` identify why clean resident pages became dirty; `invalidatedDirectionalWindow` and
`invalidatedLightTransform` identify mapping changes. The `set-shadows` master toggle gates the
whole system; off publishes a disabled table, and every sampler reads unshadowed.

```json
{
  "vsm": {
    "requestedReceiver": 82,
    "dirtiedMoved": 64,
    "point": { "requested": 64, "rendered": 64, "dirtied": 64, "invalidated": 0 }
  }
}
```

## In the code

| What | File | Symbols |
|---|---|---|
| Constants + page keys | `crates/rendering/src/vsm.rs` | `VSM_PAGE_SIZE`, `VSM_ATLAS_SIZE`, `VsmPageKey` |
| CPU residency | `crates/rendering/src/vsm.rs` | `VsmResidency`, `VsmCounters` |
| Atlas + page table | `crates/rendering/src/vsm.rs` | `VsmGpu`, `publish_table`, `vsm_table_entry` |
| GPU demand | `crates/rendering/src/vsm.rs`, `vsm_demand.slang`, `vsm_demand_compact.slang` | `VsmDemand`, `vsm_demand_key` |
| Frame preparation | `crates/rendering/src/renderer.rs` | `prepare_vsm_frame` |
| Mover invalidation | `crates/rendering/src/renderer/vsm_passes.rs`, `vsm.rs`, `persistent_gpu_scene/apply.rs`, `assets/src/gpu_scene_mirror/shared.rs` | `collect_vsm_directional_swept_bounds`, `mark_spot_dirty`, `mark_point_dirty`, `VsmDirectionalSpace::directional_page_span`, `instance_moved_bounds`, `note_instances_moved`, `leaf_page_bounds` |
| Page raster passes | `crates/rendering/src/renderer.rs` | `add_vsm_page_passes`, `vsm_page_crop` |
| Samplers | `assets/shaders/lighting_common.slang` | `vsmSampleDirectional`, `vsmSampleSpot`, `vsmSamplePoint`, `vsmTilePcf` |

## Related

- [Directional shadows](../directional-shadows/) — the clip-level space the sun samples through
- [Spot-light shadows](../spot-light-shadows/) — the projective page space of the shadowed spot
- [Point-light shadows](../point-light-cube-shadows/) — six face spaces behind the cube mapping
- [PCF filtering](../pcf-filtering/) — the in-tile comparison kernel
- [Shadow bias](../shadow-bias/) — the depth bias applied while pages rasterize
