+++
title = 'Screen-space & post'
weight = 11
bookCollapseSection = true
+++

# Screen-space & post

Screen-space effects approximate lighting and shading from the rendered image itself, then a final
color step maps the result to the display. A thin G-buffer of view-space normal and depth feeds
ambient occlusion, contact shadows, SSGI, and the temporal passes; tonemapping reduces the linear
HDR scene to the range a display can show.

## Pages

| Page | Covers | Code |
|---|---|---|
| [thin-gbuffer](thin-gbuffer/) | view-space normal + depth (rgba16f) + roughness (r8) prepass | `gbuffer.slang`; `scene_pass.rs` · `record_executor_depth_family` |
| [gtao](gtao/) | horizon-based ambient occlusion, modulating only the indirect term | `gtao.slang`; `lighting.slang` · `aoMap` |
| [contact-shadows](contact-shadows/) | screen-space ray march that darkens the directional direct term | `contact.slang`; `lighting.slang` · `contactMap`, `screenFlags.x` |
| [ssgi](ssgi/) | one-bounce screen-space indirect radiance added to the ambient term | `ssgi.slang`; `lighting.slang` · `ssgiMap`, `screenFlags.y` |
| [render-quality-tiers](render-quality-tiers/) | one tier knob (low/medium/high/ultra) driving the SSGI/GTAO/contact step counts + enable flags | `quality.rs` · `QualityTier`; `commands_render.rs` · `set-render-quality` |
| [motion-vectors](motion-vectors/) | camera + object reprojection velocity for temporal reuse | `motion.slang`; `aa.rs` · `MotionPush` |
| [taa](taa/) | Halton jitter + reconstruct-to-display + variance clip + locks/reactive + RCAS sharpen (native TAA + TAAU upsampling) | `taa.slang`; `aa.rs` · `TaaParams`, `TaaPush` |
| [fog](height-fog/) | scene fog on one authoring surface: the analytic closed-form height/distance integral, or the `volumetric` froxel path (Wronski/Hillaire inject → integrate → composite) reusing the cluster cull + shadow families for shadowed god-rays — the height density injected as the froxel base medium, never double-counted; a ping-pong history reprojects the linear scatter (5 % blend, TAA-shared Halton jitter) so a coarse `low/medium/high` grid reads smooth, per-light `volumetricScattering` / `castVolumetricShadow` shape each shaft, forward transparents self-sample the volume, and a `fog` view mode visualizes it; placeable `FogVolume` components (box/sphere, soft edges, height slab, tiling-noise erosion + wind) inject bounded local density into the same grid | `height_fog.slang`, `fog_inject.slang`, `fog_integrate.slang`; `froxel_fog.rs` · `FroxelFog`, `FroxelQuality`, `FogVolumeGpu`; `component.rs` · `FogVolume`; `renderer.rs` · `add_froxel_fog_passes`, `ViewMode::Fog` |
| [aerial-perspective](aerial-perspective/) | Hillaire-2020 aerial perspective: a 32³ froxel volume marched from the atmosphere transmittance/multiscatter LUTs (bounded at each froxel's distance), folded into the fog composite as an independent multiplied medium on one transmittance ledger (`T = T_fog·T_aerial`) so distant geometry blue-shifts coherently with the sky — driven by `aerialPerspective`/`aerialIntensity` on the same `set-fog` merge, gated by an active atmosphere | `aerial_perspective.slang`, `height_fog.slang`; `froxel_fog.rs` · `AerialPerspective`, `AP_GRID`, `ap_slice_view_z`; `renderer.rs` · `add_aerial_perspective_pass`; `ibl.rs` · `transmittance_view`, `multi_scatter_view` |
| [bloom](bloom/) | thresholdless energy-conserving mip pyramid (Karis downsample + tent upsample), lerp-composited into scene-linear `color` before the tonemap; plus lens-dirt, anamorphic streaks, and per-mip tint art direction | `bloom.slang`; `renderer.rs` · `add_bloom_pass` |
| [color-grading](color-grading/) | scene-linear grade folded into the tonemap pass before the view transform: white balance, contrast-around-pivot, saturation, global ASC-CDL (surfaced as Lift/Gamma/Gain), per-range Shadows/Midtones/Highlights CDL, a 3×3 channel mixer, split-toning, plus a post-transform display-space creative `.cube` 3D-LUT look (tetrahedral, intensity dial) and a `bake-look` GPU fold into a `33³` log2-shaper `.slut` for the player; authored in the editor Post panel via `GradingWheel` trackballs + a `ToneCurve` spline | `tonemap_ops.slang` · `grade`, `sampleCubeTetrahedral`; `overlay.rs` · `ColorGrade`, `GradeUniform`; `cube.rs` · `parse_cube`, `BakedLut`; `PostProcessPanel.tsx` |
| [tonemap-and-exposure](tonemap-and-exposure/) | exposure, the selectable view/display transform (Reinhard/ACES/AgX/PBR Neutral), gamma 2.2, in-place on the HDR offscreen | `tonemap.slang`; `overlay.rs` · `TonemapPush` |
| [compute-post-process-pattern](compute-post-process-pattern/) | `StorageImageRwCompute`, RMW transitions, dispatch in the graph | `render_graph.rs` · `RgUsage`, `RgPass` |
