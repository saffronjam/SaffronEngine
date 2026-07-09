+++
title = 'Explanations'
weight = 10
bookCollapseSection = true
+++

# Explanations

Explanations describe how each part of the engine works and why it is built that
way. Pages are grouped into subsystems and ordered bottom-up, so a section reads
top to bottom.

The shortest path through the rendering pipeline is:

1. [Error handling](core-and-conventions/error-handling/) and the Go-flavoured style
2. [Main loop](app-lifecycle-and-window/main-loop-and-run/)
3. [Render graph](frame-and-render-graph/render-graph-overview/)
4. [Cook-Torrance BRDF](lighting-and-brdf/cook-torrance-brdf/)
5. [Tonemap and exposure](screen-space-and-post/tonemap-and-exposure/)

That spans error reporting to a lit pixel on screen; the other sections fill in
around this spine.

## The subsystems

| Section | Covers |
|---|---|
| [Core & conventions](core-and-conventions/) | `Result<T>`, `Ref<T>`, signals, design philosophy |
| [App lifecycle & window](app-lifecycle-and-window/) | run loop, layers, SDL3 window and events |
| [Vulkan foundation](vulkan-foundation/) | device, VMA, sync2, dynamic rendering, RAII wrappers |
| [Frame & render graph](frame-and-render-graph/) | declared usage → automatic barriers, per-frame sync |
| [Geometry & assets](geometry-and-assets/) | mesh import, `.smesh`, GPU upload, asset catalog |
| [Scene & ECS](scene-and-ecs/) | hecs, component registry, serialization, picking |
| [Materials & pipelines](materials-and-pipelines/) | übershader, PSO cache, descriptor sets, bindless |
| [Lighting & BRDF](lighting-and-brdf/) | clustered forward, Cook-Torrance, lights, HDR |
| [Image-based lighting](image-based-lighting/) | sky, irradiance, prefilter, BRDF LUT |
| [Shadows & culling](shadows-and-culling/) | directional/spot/point shadows, light culling |
| [Screen-space & post](screen-space-and-post/) | G-buffer, GTAO, motion vectors, TAA, tonemap |
| [Global illumination & ray tracing](global-illumination-and-raytracing/) | DDGI probes, voxel trace, BLAS/TLAS, ray-query shadows, ReSTIR |
| [Anti-aliasing](anti-aliasing/) | MSAA, FXAA, mode switching |
| [UI & editor](ui-and-editor/) | the CEF/React editor shell, viewport compositing, gizmo, inspector, thumbnails |
| [Asset store & connectors](asset-store-and-connectors/) | the editor Store tab, connectors to external asset services, download → import |
| [Tooling & control](tooling-and-control/) | control plane and the `sa` CLI |
| [Scripting](scripting/) | the embedded Lua 5.5 VM, sandboxing, script errors as values |
| [Animation](animation/) | skeletal clips, the decomposed pose + blend layer, clip sampling |
| [Physics](physics/) | the Jolt-backed simulation: per-play world lifecycle, rigid bodies, the ragdoll |
| [Architecture & conventions](architecture-and-conventions/) | module DAG, build, coding style |
