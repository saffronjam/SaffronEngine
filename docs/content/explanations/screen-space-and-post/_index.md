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
| [thin-gbuffer](thin-gbuffer/) | view-space normal + depth in one rgba16f target | `gbuffer.slang`; `scene_pass.rs` · `record_gbuffer` |
| [gtao](gtao/) | horizon-based ambient occlusion, modulating only the indirect term | `gtao.slang`; `lighting.slang` · `aoMap` |
| [contact-shadows](contact-shadows/) | screen-space ray march that darkens the directional direct term | `contact.slang`; `lighting.slang` · `contactMap`, `screenFlags.x` |
| [ssgi](ssgi/) | one-bounce screen-space indirect radiance added to the ambient term | `ssgi.slang`; `lighting.slang` · `ssgiMap`, `screenFlags.y` |
| [render-quality-tiers](render-quality-tiers/) | one tier knob (low/medium/high/ultra) driving the SSGI/GTAO/contact step counts + enable flags | `quality.rs` · `QualityTier`; `commands_render.rs` · `set-render-quality` |
| [motion-vectors](motion-vectors/) | camera + object reprojection velocity for temporal reuse | `motion.slang`; `aa.rs` · `record_motion` |
| [taa](taa/) | Halton jitter + reconstruct-to-display + variance clip + locks/reactive + RCAS sharpen (native TAA + TAAU upsampling) | `taa.slang`; `aa.rs` · `TaaParams`, `TaaPush` |
| [tonemap-and-exposure](tonemap-and-exposure/) | exposure, Reinhard, gamma 2.2, in-place on the HDR offscreen | `tonemap.slang`; `overlay.rs` · `TonemapPush` |
| [compute-post-process-pattern](compute-post-process-pattern/) | `StorageImageRwCompute`, RMW transitions, dispatch in the graph | `render_graph.rs` · `RgUsage`, `RgPass` |
