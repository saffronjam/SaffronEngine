+++
title = 'Distance field reflection occlusion'
weight = 11
math = true
+++

# Distance field reflection occlusion

The prefiltered [IBL](../../image-based-lighting/ibl-overview/) environment treats the sky as
visible from every point, so a polished floor under an overhang would mirror open sky it cannot
see. Distance-field reflection occlusion corrects this: a cone sphere-marched along the reflection
vector through the Global Distance Field yields a $[0,1]$ visibility factor that dims the specular
IBL where the reflected direction is blocked. The technique is the reflection half of Wright's
[Dynamic Occlusion with Signed Distance Fields](https://advances.realtimerendering.com/s2015/)
(SIGGRAPH 2015).

## Why the cone follows the reflection vector

Dimming reflections by a diffuse AO scalar gives the wrong answer for a chrome surface facing open
sky: its hemisphere may be half-enclosed while the one direction it mirrors is unobstructed.
Reflection occlusion asks whether that specific ray escapes. `sdfReflectionOcclusion` marches a
single cone along $R$ and keeps the tightest penumbra ratio it meets:

$$
\text{occ} = \min_{t}\ \operatorname{saturate}\!\left(\frac{d(t)}{\theta\, t}\right)
$$

where $d(t)$ is the field distance at march distance $t$ and $\theta\,t$ is the cone footprint
there. The half-angle widens with roughness, from $\tan\theta = 0.05$ at mirror-smooth to $0.6$ at
full roughness: a mirror samples a thin pencil of the environment, a rough surface a broad lobe.
The march takes up to 12 sphere-trace steps over at most 12 m, stepping by the field distance
(floored at 0.05 m), and exits early once the cone is fully open and far from any surface.

## Marching the Global Distance Field

The cone reads the Global Distance Field clipmap: three camera-centered `R16_SNORM` volumes of
128³ voxels, the finest spanning 32 m (a 0.25 m voxel) and each coarser cascade doubling the
extent. `gdfDistanceOccupancy` selects the finest cascade containing the sample, converts world position to
that cascade's toroidal UVW, and takes one trilinear tap; near a cascade's outer face it blends
into the next coarser one so the handoff shows no shell. One tap costs the same regardless of
scene size, because the cone never iterates an instance list.

When a sample leaves the coarsest cascade the field can say nothing more, so the march stops and
keeps the openness accumulated so far. The reflection then reaches the open sky, which is exactly
what the analytic environment assumes. The per-mesh distance-field bricks feed only the GDF
composite and the [DDGI trace's](../software-ray-trace/) near field; this cone taps the composited
clipmap alone (light-set bindings 9 and 10).

## Half-resolution prepass

The march runs once per half-resolution pixel in the `specocc` compute pass, not per shaded
fragment. The pass reconstructs world position and normal from the
[thin G-buffer](../../screen-space-and-post/thin-gbuffer/), reads per-pixel roughness from the
G-buffer roughness target, reflects the camera ray, and writes the cone result into an `rgba16f`
image with view-Z in alpha. Background pixels write 1.0: no geometry, fully open sky.

A bilateral upsample (`specocc-blur`, the [SSGI](../../screen-space-and-post/ssgi/) blur kernel
bound with this pass's set) lifts the half-res result to full resolution with depth-aware weights.
There is no temporal accumulation: the term is view-dependent ($R$ moves with the camera), and
reprojecting it through surface motion would smear it. Spatially denoised, the low-frequency
scalar holds steady on its own.

## Application in the mesh

The lighting übershader samples the resolved map by screen UV and multiplies it into the specular
IBL after the energy-compensation and horizon terms:

```hlsl
float specSkyVis = (globals.sdfOcclusion.y != 0 && !translucent)
    ? speoccMap.SampleLevel(screenUv, 0.0).r : 1.0;
// ... energy compensation + horizon fade ...
float specAO = saturate(pow(ndotv + ao, exp2(-16.0 * roughness - 1.0)) - 1.0 + ao);
specularIBL *= specAO * specSkyVis;
```

The two factors occlude different things. `specAO` is the roughness-aware specular occlusion from
[Moving Frostbite to PBR](https://seblagarde.wordpress.com/2015/07/14/siggraph-2014-moving-frostbite-to-physically-based-rendering/)
(Lagarde and de Rousiers), driven by the contact-scale `ao` (the material occlusion texture ×
[GTAO](../../screen-space-and-post/gtao/)); it darkens reflections in creases and tends to 1 on
smooth metals. `specSkyVis` is the directional GDF cone, darkening the reflected skybox under
overhangs. Translucent surfaces read 1.0 because the map is keyed to the opaque G-buffer behind
them.

## The diffuse twin

`sdfSkyVisibility` applies the same idea to indirect diffuse: nine cosine-weighted cones (about a
30° half-angle, 8 m range) sample the hemisphere of the shading normal against the same clipmap.
It runs in the `dfao` half-res prepass with an eight-position azimuth jitter per frame, and a
bilateral upsample plus a temporal accumulator average the rotated cone rings into a denser
hemisphere. Sky visibility is view-independent, so temporal reuse is safe here.

The accumulator is the chain's answer, not a polish stage on it. Each frame's cones sit at a
different azimuth, so the blurred per-frame estimate carries that rotation as variance forever,
while the clamp-free EMA in `dfao-accum` converges to the rotation-invariant mean a still surface
should hold. Every consumer therefore reads `dfao_resolved`, and the three passes resolve as a unit
— a chain that cannot accumulate does not trace either.

The accumulated sky visibility multiplies the analytic sky irradiance inside the half-res
`gi-resolve` pass, which runs after `dfao-accum` in the same frame. Where
[DDGI](../ddgi-overview/) has coverage, its probe irradiance overrides that analytic term by
coverage weight, so the distance-field occlusion shapes only the residual sky — and where DDGI is
off, the analytic term carries the map at full weight, which is why the cone ring's variance has to
be gone before this pass reads it. Contact-scale diffuse occlusion stays with GTAO and the material
occlusion texture.

## One gate

`want_sky_occlusion` arms the whole apparatus per frame: IBL must be on and ready, the
`sky_occlusion` toggle set, and the GDF clipmap composited. The same predicate resolves both
prepasses and writes the `sdfOcclusion.y` bit into the light UBO, so the fragment never samples
a map no pass produced. The diffuse half adds one condition of its own — its accumulator
reprojects through the motion target, so the DFAO chain also needs motion. The toggle is
scriptable:

```sh
sa set-sky-occlusion 0   # reflections keep the full sky everywhere
sa set-sky-occlusion 1   # overhangs occlude the reflected skybox again
```

Disabling the Global Distance Field itself (`sa set-gdf 0`) drops the gate too, since the clipmap
the cones march never composites.

## In the code

| What | File | Symbols |
|---|---|---|
| Reflection cone march | `assets/shaders/sdf.slang` | `sdfReflectionOcclusion`, `gdfDistanceOccupancy`, `gdfEnabled` |
| Diffuse sky-visibility cones | `assets/shaders/sdf.slang` | `sdfSkyVisibility` |
| Specular prepass | `assets/shaders/specocc.slang` | `computeMain` |
| Diffuse (DFAO) prepass | `assets/shaders/dfao.slang` | `computeMain` |
| Diffuse temporal accumulator | `assets/shaders/dfao_accum.slang` | `computeMain` |
| Mesh application | `assets/shaders/lighting.slang` | `speoccMap`, `globals.sdfOcclusion` |
| Diffuse application | `assets/shaders/gi_resolve.slang` | `computeMain`, `dfaoMap` |
| Frame gate + pass wiring | `crates/rendering/src/renderer.rs` | `want_sky_occlusion`, `set_sky_occlusion`, `DfaoPipelines` |
| Lighting UBO bit | `crates/rendering/src/lighting.rs` | `set_frame_sdf_occlusion` |
| GDF cascade constants | `crates/rendering/src/global_sdf.rs` | `GDF_CASCADES`, `GDF_RES`, `GDF_CASCADE0_EXTENT` |
| The reach predicate | `crates/rendering/src/global_sdf.rs`, `crates/assets/src/render_scene.rs` | `gi_occluder_bounds`, `gi_reachable` |
| The occluder scatter | `assets/shaders/gi_occluder_scatter.slang`, `renderer.rs`, `global_sdf.rs` | `GiOccluderScatterPush`, `GlobalSdf::write_scatter_inputs`, `SDF_META_SLOT_BYTES` |
| Micro-field slab occluders | `assets/shaders/gi_occluder_micro.slang`, `assets/shaders/scene_micro_common.slang` | `GiOccluderMicroPush`, `microTileSlab`, `MICRO_FIELD_FULL_LEAF_AREA`, `AGGREGATE_MAX_OCCUPANCY` |
| The unit-box brick a slab is backed by | `crates/rendering/src/upload/sdf.rs` | `Uploader::upload_unit_box_sdf` |
| The resident SDF table | `global_gpu_data.rs`, `gpu_scene_mirror.rs` | `GpuSdfTableRecord`, `GpuScenePrototypeGpuRecord::sdf_range`, `insert_mesh_sdfs` |
| Control command | `crates/control/src/commands_render.rs` | `set-sky-occlusion` |

## What the field is composited from

The clipmap the cones tap is composited from an occluder set the GPU produces: the
[reach view](../../frame-and-render-graph/hierarchical-visibility/) culls instance slots
against `gi_occluder_bounds(eye)` — the coarsest cascade's window dilated by one
cascade-0 extent, the last position from which anything can affect a march — and the
`gi-occluder-scatter` pass turns its visible list into one `SdfInstance` per baked field
of each surviving instance. The fields are many and tight on purpose (one per primitive
and per spatial chunk), and the scatter re-tests each field's own world AABB against the
window, so an instance straddling the window edge keeps only the fields inside it. The
CPU never sees the occluder list; the count reaches the GDF cull and the DDGI near-field
march through the scatter's meta words, because a GPU-produced count cannot ride a push
constant.

Reach is the predicate everywhere: the ray list still cuts against the same function
(`rtInstancesCulled`), and the scatter reports `sdfInstancesCulled` for its per-field
window rejects. *Culled* is a claim about reach and nothing else — geometry lost to
capacity is counted as *dropped*, separately, because the two say opposite things about
whether the picture is right.

One class of matter has no instance for that walk to classify: a micro vegetation field's
grass blades exist only as a device-side reconstruction of the field tile's density
samples, so no visible slot names one and no cooked field describes one. The
`gi-occluder-micro` pass appends those from the other end — one aggregate slab occluder
per resident tile of the same directory the reconstruction dispatches over, into the same
region and through the same meta counter. The slab spans the volume that tile's blades
occupy: its owner cell's footprint, and vertically the tile's rooting grid plus the blade
height envelope. Its occupancy comes from the tile's own authored density — the
authoritative field, not the card count the reconstruction happens to draw at this cut —
spread over the slab's height as optical depth, so a shorter grass field is denser matter
for the same leaf area. It is always porous, never solid: a field is matter the marches go
through.

A slab still arrives as a real brick-backed field, because the consumers know exactly one
way to read an occluder. Its `worldToLocal` maps the world box onto a unit-box brick
synthesized once at renderer bring-up, whose every voxel reads negative, which is what
makes the composite splat the tile's occupancy across the whole slab.

The near cascade reconverges on occluder motion through the same staggered round-robin
full refresh the far cascades always used: the occluder set is GPU-produced, so no
CPU-side AABB diff can dirty the near field ahead of it.

## Porous matter

Porous aggregate matter (foliage-classed materials) contributes no distance to the field; the cones
instead accumulate Beer–Lambert extinction through its per-cascade occupancy volumes using the
shared `sdfExtinctionStep`. A canopy therefore dims a reflection or the sky by its density rather
than sealing the cone the way a wall does. The [software ray trace](../software-ray-trace/) applies
the same step along its probe rays.

## Related

- [Software ray trace](../software-ray-trace/) — the DDGI trace that sphere-marches the same near/far distance field
- [DDGI overview](../ddgi-overview/) — the probe irradiance that overrides the occluded analytic sky by coverage
- [IBL overview](../../image-based-lighting/ibl-overview/) — the analytic environment the cone occludes
- [GTAO](../../screen-space-and-post/gtao/) — the contact-scale occlusion behind the specular AO term
- [SSGI](../../screen-space-and-post/ssgi/) — the denoise kernels the prepasses reuse
