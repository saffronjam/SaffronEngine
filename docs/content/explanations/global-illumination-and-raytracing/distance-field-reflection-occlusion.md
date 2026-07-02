+++
title = 'Distance field reflection occlusion'
weight = 11
math = true
+++

# Distance field reflection occlusion

After the distance-field GI work reconciled to one indirect path, the signed distance field has exactly
one per-pixel consumer left: it occludes the reflected skybox. The indirect *diffuse* occlusion that a
distance-field ambient-occlusion prepass used to compute is gone — that job now belongs to
[DDGI](../ddgi-overview/), where the sky enters a probe's irradiance only on a ray that escapes to open
space, plus a small-radius [GTAO](../../screen-space-and-post/gtao/) for the contact detail the coarse
probe grid cannot resolve. Running a wide screen-space AO *and* DDGI would double-count the same
occlusion, so the prepass was deleted outright.

What remains is specular. The analytic [IBL](../../image-based-lighting/ibl-overview/) prefiltered cube
treats the whole environment as visible from everywhere, so a polished floor under an overhang mirrors
the open sky it cannot actually see. `sdfReflectionOcclusion` cuts that back: it sphere-marches one cone
along the reflection vector against the Global Distance Field and returns a `[0,1]` factor that
multiplies the specular IBL only where the reflection direction is blocked.

## Why specular is occluded separately

Dimming the reflected skybox by a diffuse AO scalar would be wrong: a chrome surface facing open sky
should keep a full reflection even when the diffuse hemisphere around it is half-enclosed. Reflection
occlusion is directional — it asks "is *this* reflected ray blocked," not "how much of the hemisphere is
open." So it marches a single cone along $R$ rather than reusing any diffuse term.

The cone's half-angle widens with roughness. A mirror sees a thin pencil of the environment (a narrow
cone), a rough surface a broad one; the half-angle lerps from `0.05` at mirror-smooth to `~0.6` at full
roughness. Along the march it accumulates the penumbra ratio

$$
\text{occ} = \min\!\left(\text{occ}, \; \frac{d(t)}{\theta \cdot t}\right)
$$

where $d(t)$ is the field's distance to the nearest surface at march distance $t$ and $\theta$ is the
cone half-angle. The running minimum is the most-closed point along the reflection; the result
multiplies `specularIBL` after the energy-compensation and horizon terms. A glossy surface facing an
opening keeps its reflection; one facing a wall loses it.

## It taps the Global Distance Field, not per-mesh fields

The cone reads the **Global Distance Field** (GDF) clipmap — the camera-centered cascade volumes the
[Global SDF](../../) composite builds each frame by binning the per-mesh Mesh Distance Field bricks into
one alias-free field. `gdfDistance` selects the finest cascade whose bounds contain the sample, converts
world space to that cascade's toroidal UVW, and reads one trilinear tap; near a cascade's outer face it
blends into the next coarser cascade so the handoff shows no shell. When the march leaves the coarsest
cascade the field can say nothing more, so the cone stops and keeps the open fraction accumulated so far
(the reflection then reaches the open sky the analytic environment intends).

Reading the GDF rather than the per-mesh fields is what keeps this O(1) per tap: the cone never iterates
an instance list. The per-mesh bricks still exist, but their only consumers are the GDF composite
(compute) and the DDGI trace's near field (compute) — the fragment path touches only the composited
clipmap (light set bindings 9/10). Distance-field detail near a surface is lower-frequency in the GDF
than in a per-mesh field, which is acceptable here: specular reflection occlusion is a low-frequency
term by nature.

## Where it sits in the shader

The cone runs inline in the lighting übershader's ambient block, gated by the sky-occlusion enable bit
so the fragment marches the field only on the frames the GDF cascade clipmap actually composited:

```hlsl
float specSkyVis = 1.0;
if (globals.sdfOcclusion.y != 0) {
    specSkyVis = sdfReflectionOcclusion(input.worldPos, R, roughness);
}
// ... later, after energy comp + horizon:
specularIBL *= specAO * specSkyVis;   // specAO = contact GTAO; specSkyVis = the GDF cone
```

The two specular occlusion terms are distinct and never double-applied: `specAO` is the
roughness-aware specular AO driven by the contact-scale GTAO (`ao`), occluding reflections in creases;
`specSkyVis` is the directional GDF cone, occluding the reflected skybox under overhangs.

## The indirect diffuse, for contrast

The diffuse side carries no distance-field term at all. The analytic sky irradiance is sampled along the
shading normal as a residual; where DDGI has coverage its already-occluded irradiance *replaces* the sky
by coverage weight, and the only further occlusion is contact-scale (the material occlusion texture ×
small-radius GTAO):

```hlsl
float3 indirectIrr = irradiance;                       // analytic sky, residual where DDGI is absent
if (screenFlags.z != 0) {
    float4 ddgi = ddgiSampleIrradiance(worldPos, n);
    indirectIrr = lerp(indirectIrr, ddgi.rgb, ddgi.w); // DDGI ray-miss is the large-range occlusion
}
ambient = kd * indirectIrr * albedo * ao + specularIBL; // ao = material × contact GTAO
```

So the large-range indirect occlusion is DDGI (a surface in an enclosed interior receives sky only
through rays that escape), the contact-range occlusion is GTAO, and the distance field contributes only
the reflection cone above.

## In the code

| What | File | Symbols |
|---|---|---|
| Reflection-occlusion cone | `engine/assets/shaders/sdf.slang` | `sdfReflectionOcclusion`, `gdfDistance`, `gdfEnabled` |
| Mesh-side application | `engine/assets/shaders/lighting.slang` | the ambient block, `specSkyVis`, `globals.sdfOcclusion` |
| Frame enable gate | `engine/crates/rendering/src/renderer.rs` | `set_sky_occlusion`, `want_sky_occlusion` |
| Lighting UBO bit | `engine/crates/rendering/src/lighting.rs` | `set_frame_sdf_occlusion` |
| Sky-occlusion toggle | `engine/crates/control/src/commands_render.rs` | `set-sky-occlusion` |
| Contact GTAO radius | `engine/crates/rendering/src/ssao.rs` | `Ssao::radius`, `gtao_push` |

## Related

- [DDGI overview](../ddgi-overview/) — the probe diffuse whose ray-miss is the large-range indirect occlusion
- [Software ray trace](../software-ray-trace/) — the DDGI trace that sphere-marches the near MDF → far GDF field
- [IBL overview](../../image-based-lighting/ibl-overview/) — the analytic specular the reflection cone occludes
- [GTAO](../../screen-space-and-post/gtao/) — the small-radius contact AO that fills in what the probe grid cannot resolve
