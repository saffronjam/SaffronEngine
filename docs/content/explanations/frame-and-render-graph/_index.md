+++
title = 'Frame & render graph'
weight = 4
bookCollapseSection = true
+++

# Frame & render graph

A render graph describes one frame of rendering as a set of passes and the resources they read and
write. Each pass declares its usage of a resource; the graph derives the barriers and layout
transitions and records the pass body. No pass writes a pipeline barrier by hand. Application
layers add their own passes to the cull → scene → UI frame.

## Pages

| Page | Covers | Code |
|---|---|---|
| [Render graph](render-graph-overview/) | why declared usage, the build-execute-per-frame model | `render_graph.rs` |
| [Passes](passes-and-attachments/) | `RgPass`, MRT `colors`, depth, load/store/clear, the execute closure | `render_graph.rs` |
| [Barrier derivation](usage-and-barrier-derivation/) | `RgUsage`, `usage_info`, `apply_access`, hazard + layout logic | `render_graph.rs` |
| [Cross-frame layouts](cross-frame-layouts/) | the external-layout slot write-back, imported images, seeded source scope | `render_graph.rs` |
| [Adding passes](who-can-add-passes/) | engine passes in `begin_frame_graph` vs. layer `on_render_graph` | `app/src/lib.rs`, `renderer.rs` |
| [Limits](limits-and-seams/) | single queue, no transient aliasing, no async compute, 3D transient volumes + 3D dispatch, the seams left | `render_graph.rs`, `transient.rs`, `froxel_fog.rs` |
| [Performance telemetry](performance-telemetry/) | CPU/GPU split, per-pass GPU timestamps, throughput counters, VRAM budget, the profiler mode gate | `renderer.rs`, `profiler.rs` |
| [Performance alarms](performance-alarms/) | EMA + hysteresis + debounce, MAD-spike / burn-rate detectors, severity, the non-blocking `drain-alarms` seq cursor | `frame_history.rs`, `renderer.rs` |
| [Renderer profiling](renderer-profiling/) | the capture model (merged CPU+GPU spans, nesting, calibration), timestamp caveats, capture modes, Chrome-Trace + Perfetto export, pipeline statistics, software-GPU honesty | `profiler.rs`, `render_graph.rs` |
| [Compute skinning](compute-skinning/) | deform-once into a base-layout buffer, the deformed buffer + per-instance dispatch, compute→vertex barrier | `skin.slang`, `skinning.rs` |
| [Compute displacement](compute-displacement/) | height-map displacement by adaptive compute tessellation into transient VB/IB — diced, displaced, welded watertight, read by every raster pass + the RT BLAS as one geometry | `tessellate.slang`, `tessellation.rs`, `rt.rs` |
