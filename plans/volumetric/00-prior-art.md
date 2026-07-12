# Prior art & chosen approach

**Status:** reference

This is the research record behind `plans/volumetric/`. It surveys how the four production stacks Anima measures itself against — Unreal Engine 5, Unity HDRP, Blender (Cycles + EEVEE Next), and Godot 4 — build atmospheric fog, across two concerns the planset treats as one continuous system: cheap **analytic exponential height/distance fog** and full **froxel volumetric fog**, plus their two dependents, **local fog volumes** and **aerial perspective**. Every stack converges on the same shape — an always-on closed-form height term, a frustum-aligned voxel grid for lit/shadowed in-scatter, one placeable local-density primitive, and a separate long-range atmosphere layer — and every stack composites all of it into scene-linear HDR before the tonemapper. Anima already ships the exact substrate those pipelines are built on (a clustered light cull with exponential-Z froxels, all four shadow families, the Hillaire atmosphere LUTs, an FSR2-style TAA, and a pre-tonemap bloom + grade pass), so the chosen approach is not a fresh subsystem but a reuse of that substrate one step earlier in the frame. This page fixes the one modern-correct destination for each concern and records why the cheaper alternatives are deliberately not offered as parallel paths, so the phase files can stay short.

## Where Anima sits today

The frame builds opaque lit HDR into `ViewTarget.offscreen` (rgba16f, `STORAGE | SAMPLED`; `engine/crates/rendering/src/view_target.rs`), the sky pass paints the background from the Hillaire LUTs, and then — after motion vectors and TAA — the post-processing planset's bloom pyramid and the tonemap/grade compute pass turn that HDR into display pixels. There is no fog anywhere in that chain.

- **The clustered light cull is a froxel grid already.** `light_cull.slang` culls lights into a 16×9×24 exponential view-space grid (`engine/crates/rendering/src/lighting.rs`, `CLUSTER_GRID_X`/`Y`/`Z`, `cull_clusters_cpu`, the `cluster_grid_matches_shader` CPU-mirror test), and the per-cluster light list + punctual light SSBO are exposed as reusable getters (`cluster_buffer_with_size`, `light_list_buffer`) that ReSTIR already binds into a standalone compute set. A fog froxel grid maps its centre to a cluster via `clusterIndexFor` and reads that list with zero new culling.
- **All four shadow families and an HG phase are in the tree.** `lighting.slang` carries `pcfShadow` (directional/spot), `pointShadow` (cube), and `rayQueryShadow` (inline TLAS ray-query); `atmos_skyview.slang` carries the only `hgPhase` in the codebase. The atmosphere LUT chain (transmittance/sky-view/multiscatter) lives in `engine/crates/rendering/src/ibl.rs`, and the TAA NDC jitter is `taa.slang` `Push.jitter`.
- **The render graph is 3D-ready but nothing 3D is transient.** `render_graph.rs` has `import_image_3d` (barriers derive dimension-agnostically) and `RgUsage::StorageImageRwCompute`/`SampledReadCompute`, and `global_sdf.rs` is a working precedent for a persistent rgba16f `Image3D` written by a compute pass and sampled through the graph. But the transient pool is 2D-only (`resources.rs` `ImageDesc` holds a `vk::Extent2D`; `Image::new` hardcodes `TYPE_2D` + `depth:1`), and `Renderer::add_compute_pass` hardcodes `groups_z = 1`.
- **Scene-wide atmosphere state has an authoring surface; fog does not.** `SceneEnvironment` holds `AtmosphereSettings` (`engine/crates/scene/src/environment.rs`), serialized by `atmosphere_to_json` (`serde.rs`) and driven by the `set-atmosphere` control command over `SetAtmosphereParams`/`EnvironmentDto` (`engine/crates/protocol/src/dto.rs`, `engine/crates/control/src/commands_scene.rs`). There is no `FogSettings`, no `set-fog`, no `FogVolume` component, and no `submit_fog` on the `SceneRenderer` trait. The word "froxel" in the tree today means the light-cull *buffer*, not a 3D image.

## Analytic height & distance fog — the four stacks

| Stack | Model & math | Key params | Composite | Notable |
|-------|--------------|-----------|-----------|---------|
| **UE5 Exponential Height Fog** | Closed-form line integral of `ρ(z)=FogDensity·exp2(-HeightFalloff·(z-FogHeight))`; factored as `RayOrigin·[(1-exp2(-F))/F]·RayLength` with a Taylor fallback near horizontal rays (`HeightFogCommon.ush`) | FogDensity, HeightFalloff, MaxOpacity, StartDistance, CutoffDistance, a second summed layer, directional inscattering (color + exponent + start) | Full-screen over depth, after opaque, before bloom; injected as base froxel density when volumetric mode is on | One exp + one divide/pixel; directional lobe is a `pow(dot(view,-sun))` sun-glow for near-free sun-through-haze |
| **Unity HDRP Fog** | Exponential height fog blended between Base Height and Maximum Height, driven by an attenuation distance (mean free path) | Attenuation Distance, Base/Maximum Height, Max Fog Distance, Color mode (constant / sky-tinted via mip-fog) | Volume-framework override, applied pre-tonemap; the same override gates the volumetric path | Height fog and volumetric fog are one authoring component; sky-color mode reddens fog toward the atmosphere |
| **Godot Environment fog** | `FOG_MODE_EXPONENTIAL`: Beer-Lambert on distance × an exponential height term; `FOG_MODE_DEPTH`: linear begin/end ramp shaped by a curve | fog_density, fog_height + fog_height_density, fog_light_color/energy, fog_sun_scatter, fog_aerial_perspective | Analytic per-pixel blend late in the opaque/sky fragment stage; infinite range | Explicitly kept as the cheap far-field the finite froxel grid can't reach |
| **Blender (Cycles/EEVEE)** | None analytic — height fog is authored as nodes: `Geometry.Position.z → Map Range → Density` into a real Volume Scatter medium, integrated by the path tracer or the froxel march | Any node graph feeding Density | Full volumetric pass; no closed-form term | The "density is any function of position" abstraction is elegant, but routing trivial haze through the whole grid is overkill |

**Consensus.** Everyone except Blender ships a closed-form exponential height + distance fog as the always-on base layer, because the exponential density model makes the view-ray optical depth analytic — one `exp` and one divide per pixel, no marching. The canonical form is UE's, mirrored by Quilez and the Zero Radiance derivation: `opticalDepth = RayOrigin(cameraZ) · lineInt(rayDirZ) · RayLength`, with the `(1-exp2(-F))/F` line-integral factor and its Taylor fallback near horizontal rays the one numerically-critical detail. A max-opacity clamp keeps distant sky readable, a second summed layer gives a ground-haze slope under a broad layer, and a directional inscattering lobe adds sun-through-haze for free. All four composite in scene-linear HDR before the tonemapper, and the two engines with a physical sky (UE, HDRP) tint the fog inscatter from the sky/atmosphere so fog and horizon agree in hue. Blender's omission is the outlier the field treats as a gap, not a model to copy.

## Froxel volumetric fog — the four stacks

| Stack | Grid | Pass structure | Temporal | Phase / lights | God-rays |
|-------|------|----------------|----------|----------------|----------|
| **UE5 Volumetric Fog** (Wronski 2014) | Frustum voxels, 8px XY tiles × 128 Z slices, exponential depth; far plane = actor View Distance | Density inject → per-light in-scatter → front-to-back integrate | Sub-froxel jitter + ~5–10% history reproject (mandatory) | Henyey-Greenstein `g`; per-light Volumetric Scattering Intensity + Cast Volumetric Shadow | Emergent from shadowed sun in-scatter |
| **Unity HDRP Volumetric Fog** | Vbuffer froxel grid sized by a screen budget %, slice distribution uniformity knob | Inject → scatter (clustered lights) → integrate | Reprojection / Gaussian / both denoisers | Global anisotropy + multiple-scattering term; per-light Volumetric Dimmer + Volumetric Shadow Dimmer | Emergent; shadowed froxel scatter |
| **Godot volumetric fog** (Wronski 2014) | 64³-ish froxels, exponential detail-spread, optional filter; `volumetric_fog_length` far cutoff | `MODE_DENSITY` (inject + scatter + reproject) → `MODE_FILTER` → `MODE_FOG` (Beer-Lambert integrate) | Halton jitter + `to_prev_view` reproject, amount ~0.9 | HG `k(1-g²)/pow(...,1.5)`; every light contributes, gated by Volumetric Shadows | Emergent from per-froxel shadow-map sampling |
| **Blender EEVEE Next** (Hillaire 2015) | Froxel 3D texture, Resolution (px tile) × Steps (Z), linear↔exponential distribution | Property inject → shadowed light scatter → integrate → resolve composite | Temporal reproject + dithered sampling (4.2) | HG anisotropy; every light auto-contributes | Emergent via the Volumetric Shadows toggle |

**Consensus.** Every real-time stack implements the identical Wronski-2014 / Hillaire-2015 pipeline: a frustum-aligned 3D texture with an exponential Z distribution, three compute stages (inject participating-media density + albedo + extinction → accumulate per-light in-scatter weighted by a Henyey-Greenstein phase and attenuated by each light's shadow → front-to-back integrate scattering and transmittance), sampled per-pixel at the fragment's depth slice and composited as `scene·T + inScatter`. Two points are unanimous and load-bearing. First, the grid is deliberately low-resolution, so **temporal reprojection with per-frame sub-froxel jitter is not optional** — it is what lets an 8px/64-slice grid look smooth, and the reprojected quantity must be the *linear* scatter+extinction, never the non-linear integrated transmittance (Hillaire's energy note). Second, **god-rays fall out for free** from shadow-sampling lights inside the injection stage; the modern engines have all deleted the legacy screen-space radial-blur godray pass (Mitchell 2007) because it can't shaft behind occluders or work off-screen. The energy-conserving analytic slice integral `S_int=(S-S·exp(-σd))/σ` (Hillaire) is the accepted fix for Wronski's high-density banding. UE and HDRP add the two per-light knobs — a scattering-intensity multiplier and a cast-volumetric-shadow gate — that let artists pick which lights throw shafts; Blender and Godot make every light contribute globally, which the field regards as the weaker authoring model.

## Local fog volumes — the four stacks

| Stack | Primitive | Density authoring | When volumetrics on |
|-------|-----------|-------------------|---------------------|
| **UE5** | Local Fog Volume (sphere, radial fade) + Sparse Volume Textures (VDB) | Analytic sphere; SVT page-table + physical-tile for baked/animated detail | Voxelized additively into the froxel grid — gains real lighting + volumetric shadows |
| **Unity HDRP** | Local Volumetric Fog (box) with a 3D density mask texture | Single-scattering albedo + fog distance + per-axis blend falloff + scrollable 3D mask | Injected into the Vbuffer; lit and shadowed by the same passes |
| **Godot** | FogVolume node (ellipsoid/cone/cylinder/box/world) + FogMaterial or a `fog` shader | Density/albedo/emission/height-falloff/edge-fade + optional NoiseTexture3D; a programmable `fog()` per-froxel seam | Rasterized into the shared froxel buffer via atomics (signed density → volumes can subtract), order-independent |
| **Blender** | Closed manifold mesh + Volume material, or an OpenVDB Volume datablock | Node graph into Density, or named VDB grids (density/color/temperature) | Voxelized into the froxel grid; empty-space skipping by object bounds |

**Consensus.** A local fog volume is a bounded region (box or sphere the common case) that contributes density into the *same* froxel grid the global fog uses, so it is lit, shadowed, temporally reprojected, and integrated by the identical stages with no special path — this is the modern unification (Godot's single-buffer model; UE's "same authoring, froxel backend when volumetrics are on"). All four give it a soft edge (SDF/blend falloff so volumes fade rather than hard-clip), per-volume albedo/emissive/phase, and an optional per-volume height falloff so a box can itself be a local height-fog slab. Animated detail is 3D noise sampled at `worldPos·freq - wind·time` — a low-frequency base plus a subtracted high-frequency erosion octave (Frostbite/Horizon), drifted by a wind vector. On the authoring side the field splits: Godot and Unity treat the box extent as numeric properties with a passive wireframe/gizmo, while Blender uses mesh bounds; none of them expose a drag-resize handle that Anima would need to build. Godot's atomic-packed signed-density buffer is the correct end-state for many overlapping volumes, but every stack starts with a simple summed loop for a handful of volumes.

## Aerial perspective — the four stacks

| Stack | Approach | Applies to scene geometry? |
|-------|----------|---------------------------|
| **UE5** | Sky Atmosphere computes physically-based Rayleigh/Mie aerial perspective, composited as the outermost layer (AP → height fog → local volumes → volumetric fog) | Yes — distance blue-shift/desaturation on geometry |
| **Unity HDRP** | Physically Based Sky + fog sky-color mode; atmospheric scattering feeds the fog tint | Partial — via the fog's sky-tinted color and PBSky |
| **Godot** | `fog_aerial_perspective` blends the analytic fog color toward the sky/reflected environment with distance | Approximate — analytic stand-in beyond the froxel range |
| **Blender** | None — a world Volume Scatter fills space to fake distance haze (manual warns it's a poor physical assumption) | Only through a global world medium |

**Consensus.** Aerial perspective is a *separate medium* from near/mid fog — long-range planetary Rayleigh/Mie scattering, not a participating-media haze — and the correct implementation (Hillaire 2020) is a small aerial-perspective froxel volume (32³ over ~32 km) fed by ray-marching precomputed transmittance/multiscatter LUTs, storing `(inScatter.rgb, mean-transmittance.a)` and applied to geometry with the *same* `color·T + L` operator as the fog. The insight the field converges on: **the AP volume and the fog volume are the same data structure differing only in which medium is injected**, so they compose as independent multiplied media on one shared transmittance ledger (`T_total = T_fog·T_aerial`) and neither double-darkens the other's in-scatter. Anima's sky pass already runs the Hillaire LUTs but does *not* tint geometry with distance — the same gap UE closes and Blender leaves open.

## Chosen approach — Anima

The destination is the modern-correct Wronski-2014 / Hillaire-2015 froxel pipeline unified with a closed-form analytic exponential height/distance base, composited into `ViewTarget.offscreen` before the bloom pyramid and the tonemap/grade pass. Each stage is built the technically-right way and the cheaper legacy shape is not offered as a parallel path.

**Analytic exponential height fog is the always-on base, shipped first, and it is never a throwaway.** Phase 1 builds the UE/Quilez/Zero-Radiance closed-form line integral as a single full-screen compute pass over scene depth — `opticalDepth = RayOrigin·lineInt·max(RayLength-StartDistance,0)` with the Taylor fallback near horizontal rays, a `MaxOpacity` transmittance floor, a second summed layer, and a `pow(dot(view,-sun))` directional inscattering lobe, its inscatter tinted by sampling the existing sky-view LUT (`ibl.rs`) so fog reddens toward the sun for free. This needs no new render-graph primitive and carries the far field beyond the froxel range for every later phase. It is not the screen-space godray fake (Mitchell 2007) and it is not folded into the surface shader — it is a real participating-media term composited with the shared `scene·T + inScatter` operator that the froxel and aerial-perspective volumes reuse verbatim.

**The froxel core reuses the clustered cull, all shadow families, and the TAA jitter — it is `lighting.slang` minus the BRDF plus a phase.** Phase 3's `fog_inject.slang` reconstructs each froxel's jittered world position, evaluates `σ_t` (analytic height density as the base medium + base density + local volumes), maps the froxel to its containing 16×9×24 cluster via `clusterIndexFor`, and accumulates `radiance·attenuation·shadow·hgPhase(dot(view,l),g)·σ_s` over the cluster's light list using the existing `clusters`/`lights` SSBOs and the existing PCF/cube/ray-query shadows — the exact ReSTIR reuse pattern. `fog_integrate.slang` marches front-to-back with the energy-conserving analytic slice `S_int=(S-S·exp(-σd))/max(σ,1e-5)`. This mandates promoting `lighting.slang`'s file-private helpers into a public resource-parameterized shared module (the `giprobe` module is the template) and lifting `hgPhase` from `atmos_skyview.slang`. God-rays emerge from shadowed sun in-scatter — no screen-space godray pass is built, ever.

**The froxel volumes are 3D transient images, which is the one real infra gap this planset fills.** Phase 2 teaches the transient pool to allocate `depth>1`/`TYPE_3D` images (`resources.rs` `ImageDesc`/`Image::new`, `transient.rs` `acquire_image_3d`) and adds a `groups_z` path to `Renderer::add_compute_pass` — a genuinely new render-graph capability (3D transient images + 3D compute dispatch), stated as such. The three rgba16f volumes — `scatterExtinction` current, its ping-pong history, and the sampled `integration` volume — are the correct transient fit for a frustum scratch that resizes with the grid; `global_sdf.rs`'s persistent `Image3D` is the write/sample precedent. Temporal reprojection (Phase 4) blends the *linear* scatter+extinction at 5% new / 95% history through the previous view-proj, resetting on camera cuts, and reuses `taa.slang` `Push.jitter` for coherent sub-froxel supersampling.

**Fog composites before bloom and tonemap — this is the load-bearing ordering decision.** The fog stage runs in `ViewTarget.offscreen` (rgba16f, scene-linear) after the sky pass and before motion vectors / TAA / the bloom pyramid / the tonemap+grade compute pass. Fog must attenuate distant bright emitters *first* so the energy-conserving bloom reads already-fogged HDR and blooms them less; applying bloom first would let far highlights bloom at full strength and then get covered, punching physically-wrong halos through the fog. One composite operator (`scene·T + inScatter`) and one `(inScatter.rgb, transmittance.a)` storage convention are shared by the analytic pass, the froxel integration volume, and the aerial-perspective volume, so the three swap in transparently.

**Scene-wide fog is `SceneEnvironment` state driven by `set-fog`; local fog is a `FogVolume` entity — one authoring surface each, no parallel path.** `FogSettings` mirrors `AtmosphereSettings` on `SceneEnvironment` (serialized through `environment_to_json`, edited by a `set-fog` command cloning `set-atmosphere`, persisted in the project's `environment` block — *not* in `RenderSettings`). A `fog.mode` flag (`analytic | volumetric`) selects the backend, and in volumetric mode the height density is injected as the froxel base medium instead of being applied twice, so the two never double-count — the analytic term is a strict subset of the froxel path (NO-LEGACY: the cutover deletes nothing because there is no parallel path to delete). `FogVolume` (Phase 5) mirrors `ReflectionProbe` — a hecs component placed by an `AddEntityPreset::FogVolume` arm, edited by the generic `set-component-field`, injected into the same grid during density evaluation so it is lit and shadowed by the identical stages. Per-light control (Phase 4) is `volumetricScattering` + `castVolumetricShadow` packed into `GpuLight`, surfaced through the registry-driven Inspector with two `FIELD_HINTS` rows and no per-component code.

**Aerial perspective closes the loop by sharing the froxel infra, not by growing a second system.** Phase 6 fills a 32³ transient volume (Phase 2's infra + `import_image_3d`) by ray-marching the existing `ibl.rs` transmittance/multiscatter LUTs along the shared exponential-Z froxel centres, composited as an independent multiplied medium on the shared ledger (`T_total = T_fog·T_aerial`, `L_out = scene·T_total + inScatter_fog + T_fog·inScatter_aerial`). It applies aerial perspective to scene geometry, which the sky pass does not do today. It is a *separate* injection volume from the fog grid so the atmosphere medium is not double-counted — the natural Hillaire-2020 unification point, built by extending the froxel infra.

> **Scene HDR → { analytic height fog | froxel inject → integrate } → aerial-perspective volume → one shared composite (`scene·T_total + inScatter`) → bloom → tonemap.** The analytic term, the froxel integration volume, and the AP volume all speak the same `(inScatter, transmittance)` convention, so they compose once on a single transmittance ledger and never double-darken.

## Deliberately deferred (Future)

- **Sparse/NanoVDB imported density (UE Sparse Volume Textures, Blender OpenVDB).** The correct end-state for high-detail baked/animated smoke and clouds is a page-table + physical-tile sparse 3D texture (or NanoVDB) feeding the froxel injection. This planset leaves the injection seam clean (a per-volume density contribution) but does not build the sparse-texture streaming infra; procedural 3D noise covers the animated-detail case for v1.
- **Atomic-packed signed order-independent volume compositing (Godot's model).** For many overlapping local volumes, the correct approach is three `r32ui` textures written by atomics (signed density ×scale so volumes subtract, packed albedo/emission, overflow repaired via `atomicOr`). Phase 5 ships a simple summed loop sufficient for a handful of volumes; the atomic buffer is a Future refinement gated on volume counts growing.
- **A dedicated fog-froxel light cull.** The fog grid (160×90×128) reads the coarse 16×9×24 cluster's light list via `clusterIndexFor`. If the coarse per-cluster culling proves inadequate for fog in-scatter, a finer fog-specific cull is the refinement — but the shared cluster list is the correct starting point and avoids a second cull pass.
- **A froxel-marched volumetric shadow map for shadowless lights.** Dense local volumes could self-shadow via an accumulated-extinction march (Hillaire) so lights without shadow maps still cast shafts. The shadow-map/cube/ray-query families cover v1; this is a clean later addition.

## Decisions to record when building

- **Reproject the linear scatter+extinction, never the integrated transmittance.** Extinction is linear and temporally integrates cleanly; the non-linear integrated transmittance would ghost and lose energy (Hillaire).
- **Energy-conserving analytic slice integration is the default, not naive `accum·step`.** `S_int=(S-S·exp(-σd))/max(σ,1e-5)` removes Wronski's high-density darkening/banding for free.
- **One jitter sequence.** Reuse `taa.slang` `Push.jitter` as the froxel sub-slice jitter so the fog supersamples coherently with the TAA resolve rather than fighting it.
- **`fog.mode` selects a backend; the analytic term is a strict subset.** In volumetric mode the height density is the froxel base medium — never the closed form applied on top — so the two never double-count.
- **Composite strictly before bloom.** Load-bearing for a pre-tonemap-bloom engine; document it at the pass-ordering seam.
- **The froxel→cluster mapping gets its own CPU-mirror test.** A `froxel_grid_matches_shader` unit test analogous to `cluster_grid_matches_shader` locks the CPU/GPU exponential-Z depth mapping.
- **RT-shadowed sun in-scatter is opt-in behind quality/`castVolumetricShadow`.** One ray per froxel per light is expensive; cascade PCF is the default for the sun, ray-query the opt-in for crisp shafts where cascades don't reach.

## Phase map (where this lands)

| Concern | Chosen technique | Phase |
|---------|------------------|-------|
| Analytic exponential height + distance fog, directional lobe, sky-view-tinted inscatter | UE/Quilez/Zero-Radiance closed-form line integral, composite before bloom | `phase-1-analytic-height-fog.md` |
| 3D transient images + 3D dispatch + froxel module + `lighting.slang` shared-module refactor | new render-graph capability; `global_sdf.rs` `Image3D` precedent | `phase-2-froxel-infra-3d-transient.md` |
| Froxel inject / integrate / composite reusing the clustered cull + all shadows + HG phase | Wronski 2014 + Hillaire 2015 energy-conserving integration | `phase-3-froxel-inject-integrate-composite.md` |
| Temporal reprojection, quality tiers, per-light volumetric flags | Frostbite temporal reproject + Halton jitter; UE per-light knobs | `phase-4-temporal-quality-per-light.md` |
| Local FogVolume entities (box/sphere) + soft edges + animated noise + wind | Godot/UE local-volume model, injected into the shared grid | `phase-5-local-fog-volumes.md` |
| Hillaire-2020 aerial perspective sharing the froxel infra + one transmittance ledger | Hillaire 2020 AP froxel volume from the atmosphere LUTs | `phase-6-aerial-perspective.md` |

## References

Foundational techniques (froxel + analytic media + integration):

- Bart Wronski — *Volumetric Fog: Unified Compute Shader Based Solution to Atmospheric Scattering* (SIGGRAPH 2014): https://bartwronski.com/wp-content/uploads/2014/08/bwronski_volumetric_fog_siggraph2014.pdf
- Bart Wronski — *Publications* (Volumetric Fog talk + GPU Pro 6 chapter index): https://bartwronski.com/publications/
- Bart Wronski — *Assassin's Creed 4: Road to Next-Gen Graphics* (GDC 2014): https://bartwronski.com/wp-content/uploads/2014/03/ac4_gdc.pdf
- Sébastien Hillaire — *Physically Based and Unified Volumetric Rendering in Frostbite* (SIGGRAPH 2015): https://www.ea.com/frostbite/news/physically-based-unified-volumetric-rendering-in-frostbite
- Sébastien Hillaire — *Physically Based and Unified Volumetric Rendering in Frostbite* (SlideShare): https://www.slideshare.net/DICEStudio/physically-based-and-unified-volumetric-rendering-in-frostbite
- *Advances in Real-Time Rendering in Games* (SIGGRAPH 2015 course): https://advances.realtimerendering.com/s2015/
- Sébastien Hillaire — *A Scalable and Production Ready Sky and Atmosphere Rendering Technique* (2020): https://onlinelibrary.wiley.com/doi/abs/10.1111/cgf.14050
- Sébastien Hillaire — *Publications* (sky/atmosphere + aerial-perspective volume): https://sebh.github.io/publications/
- Lagarde & de Rousiers — *Moving Frostbite to Physically Based Rendering 3.0* (SIGGRAPH 2014 course notes): https://seblagarde.files.wordpress.com/2015/07/course_notes_moving_frostbite_to_pbr_v32.pdf
- Inigo Quilez — *Better Fog* (analytic height-fog integral + directional sun tint): https://iquilezles.org/articles/fog/
- Zero Radiance — *Sampling Analytic Participating Media* (exponential height-fog optical-depth closed form): https://zero-radiance.github.io/post/analytic-media/
- Ubpa/ExponentialHeightFog — UE `HeightFogCommon.ush` line-integral port (Taylor fallback): https://github.com/Ubpa/ExponentialHeightFog
- Mitchell — *Volumetric Light Scattering as a Post-Process* (GPU Gems 3, screen-space godrays — the approach we do NOT take): https://developer.nvidia.com/gpugems/gpugems3/part-ii-light-and-shadows/chapter-13-volumetric-light-scattering-post-process
- Flax Facts #14 — *Volumetric Fog* (concrete froxel impl: RGBA16F, Halton jitter, history blend): https://flaxengine.com/blog/flax-facts-14-volumetric-fog/
- Chetan Jags — *HDR Rendering: Tonemapping and Bloom* (pre-tonemap HDR compositing order): https://chetanjags.wordpress.com/2015/06/28/hdr-rendering-tonemapping-and-bloom/
- webgpu-sky-atmosphere — Hillaire atmosphere implementation (aerial-perspective volume, `color·T + inScatter`): https://jolifantobambla.github.io/webgpu-sky-atmosphere/

Unreal Engine 5:

- *Exponential Height Fog in Unreal Engine*: https://dev.epicgames.com/documentation/en-us/unreal-engine/exponential-height-fog-in-unreal-engine
- *Volumetric Fog in Unreal Engine*: https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-fog-in-unreal-engine
- *Local Fog Volumes in Unreal Engine*: https://dev.epicgames.com/documentation/en-us/unreal-engine/local-fog-volumes-in-unreal-engine
- *Sparse Volume Textures in Unreal Engine*: https://dev.epicgames.com/documentation/en-us/unreal-engine/sparse-volume-textures-in-unreal-engine
- *Using Light Shafts in Unreal Engine*: https://dev.epicgames.com/documentation/en-us/unreal-engine/using-light-shafts-in-unreal-engine
- Magnopus — *Mastering Fog: Four Levels of Fog in Unreal*: https://www.magnopus.com/blog/mastering-fog-four-levels-of-fog-in-unreal-engine
- Unreal Directive — `r.VolumetricFog.GridPixelSize`: https://unrealdirective.com/resources/console-variables/r-volumetricfog-gridpixelsize/
- Unreal Directive — `r.VolumetricFog.GridSizeZ`: https://unrealdirective.com/resources/console-variables/r-volumetricfog-gridsizez/
- Unreal Engine — *Set Volumetric Fog Scattering Distribution* (Blueprint API): https://dev.epicgames.com/documentation/en-us/unreal-engine/BlueprintAPI/Rendering/VolumetricFog/SetVolumetricFogScatteringDistri-
- Unreal Engine — `ExponentialHeightFogComponent` (Python API 5.5): https://dev.epicgames.com/documentation/en-us/unreal-engine/python-api/class/ExponentialHeightFogComponent?application_version=5.5

Unity HDRP:

- Unity HDRP — *Fog Volume Override* (exponential height fog + volumetric fog): https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@latest/index.html?subfolder=/manual/Override-Fog.html
- Unity HDRP — *Local Volumetric Fog* (box volume + 3D density mask): https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@latest/index.html?subfolder=/manual/Local-Volumetric-Fog.html
- Unity HDRP — *Volumetric Lighting* (Vbuffer froxel grid, denoising, per-light dimmers): https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@latest/index.html?subfolder=/manual/Volumetric-Lighting.html

Godot 4:

- *Volumetric fog and fog volumes* — Godot Engine documentation: https://docs.godotengine.org/en/stable/tutorials/3d/volumetric_fog.html
- *Fog Volumes arrive in Godot 4.0* — Godot Engine blog: https://godotengine.org/article/fog-volumes-arrive-in-godot-4/
- *Environment* — class reference (fog + volumetric_fog properties, FogMode): https://docs.godotengine.org/en/stable/classes/class_environment.html
- *FogVolume* — class reference (shape/size/material): https://docs.godotengine.org/en/stable/classes/class_fogvolume.html
- *FogMaterial* — class reference (density/albedo/emission/height_falloff/edge_fade/density_texture): https://docs.godotengine.org/en/stable/classes/class_fogmaterial.html
- *Fog shaders* — shader reference (`fog()` fn, DENSITY/ALBEDO/EMISSION/SDF/UVW built-ins): https://docs.godotengine.org/en/stable/tutorials/shaders/shader_reference/fog_shader.html
- `volumetric_fog_process.glsl` — Godot source (MODE_DENSITY/FILTER/FOG/COPY, HG phase, Beer-Lambert integration, temporal reprojection): https://github.com/godotengine/godot/blob/4.2/servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl
- `volumetric_fog.glsl` — Godot source (FogVolume SDF rasterization + atomic packing): https://github.com/godotengine/godot/blob/4.2/servers/rendering/renderer_rd/shaders/environment/volumetric_fog.glsl
- clayjohn — *FogVolumes, FogShaders, FogMaterial, and overhaul of VolumetricFog* (PR #53353): https://github.com/godotengine/godot/pull/53353

Blender (Cycles + EEVEE Next):

- *Volumetrics* — Blender Manual (EEVEE legacy render settings): https://docs.blender.org/manual/en/3.1/render/eevee/render_settings/volumetrics.html
- *Volumes* — Blender Manual (EEVEE Next: Resolution, Steps, Distribution, Volumetric Shadows): https://docs.blender.org/manual/en/latest/render/eevee/render_settings/volumes.html
- *Principled Volume* — Blender Manual (density/color/anisotropy/absorption/blackbody, attributes): https://docs.blender.org/manual/en/latest/render/shader_nodes/shader/volume_principled.html
- *Volume Scatter* — Blender Manual (Henyey-Greenstein anisotropy): https://docs.blender.org/manual/en/latest/render/shader_nodes/shader/volume_scatter.html
- *Volumes* (material components) — Blender Manual (world vs object volume, manifold mesh): https://docs.blender.org/manual/en/latest/render/materials/components/volume.html
- *Volume Shaders* — Blender Manual (Cycles world/object volume): https://docs.blender.org/manual/en/2.79/render/cycles/materials/volume.html
- *EEVEE Next Generation in Blender 4.2 LTS* — Blender Developers Blog: https://code.blender.org/2024/07/eevee-next-generation-in-blender-4-2-lts/
- *Blender 4.2 LTS: EEVEE* — release notes (volumes, dithering, light sampling): https://developer.blender.org/docs/release_notes/4.2/eevee/
- *EEVEE migration to Blender 4.2 LTS* (infinite world volume, convert-to-mesh): https://developer.blender.org/docs/release_notes/4.2/eevee_migration/
- *EEVEE Next: Volumes* (PR #107176, compute-shader port + empty-space skipping): https://projects.blender.org/blender/blender/pulls/107176
- *EEVEE-Next: Show world volume properties* (PR #119729): https://projects.blender.org/blender/blender/pulls/119729
- *EEVEE-Next: Convert a world volume to mesh* (PR #119734): https://projects.blender.org/blender/blender/pulls/119734
- *Volume* — Blender Developer Documentation (Volume datablock, OpenVDB, dense 3D texture in EEVEE, NanoVDB in Cycles): https://developer.blender.org/docs/features/objects/volume/
- *Understanding Volumetric Lighting in EEVEE* (froxel scatter/integration/resolve, Frostbite basis): https://learningblender3dsoftware.blogspot.com/2020/02/understanding-volumetric-lighting-in.html
- iRendering — *Blender 4.2: Explore what's new in EEVEE Next*: https://irendering.net/blender-4-2-explore-whats-new-in-eevee-next/
