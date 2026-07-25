+++
title = 'Renderer profiling'
weight = 9
+++

# Renderer profiling

A profiler capture records exact, un-smoothed timing spans for every render-graph pass — CPU and
GPU on one timeline — over a bounded window of frames, and hands the result back as a file a trace
viewer opens. [Performance telemetry](../performance-telemetry/) answers whether a frame is slow; a
capture answers which pass, on which processor, in which exact frame.

A capture is request-scoped. A control command arms it, a bounded number of frames record, and one
drain returns the result. The profiler mode the capture needs is switched on for its duration and
restored on stop, so the always-on baseline stays uninstrumented.

## Spans on two lanes

A capture is a flat list of spans. Each carries a name, a lane (CPU or GPU), a
`[start_ns, end_ns)` interval, and its nesting as `parent_index` plus `depth`. The list stays flat
rather than a literal tree because GPU scopes can overlap in time; the consumer rebuilds the tree
from the indices, and overlapping siblings remain representable.

- **CPU spans** are monotonic-clock intervals recorded on the render thread into a
  `CpuSpanBuffer`, with names interned once in `CpuMarkerRegistry`. The run loop opens the
  top-level phases (`build-frame-graph`, `execute-render-graph`, `submit-present`), the graph
  opens one span per pass, and a pass body opens children through `NestedScopeRecorder::scope`.
  The scene pass records `scene-opaque`, `scene-submissions`, and `scene-translucent` this way.
- **GPU spans** come from `RgTimestamps`: graph batch recording brackets each timed pass with a
  timestamp-query pair written by `cmd_write_timestamp2` (`TOP_OF_PIPE` at begin,
  `BOTTOM_OF_PIPE` at end) and pushes a `ScopeRecord` with the same parent/depth tagging.

Each frame slot owns a timestamp pool of 320 queries: 160 scopes, two queries per scope. The
read-back never blocks — a slot's pool is read `MAX_FRAMES_IN_FLIGHT` (2) frames later, at the
begin-frame fence wait, once that slot's GPU work has provably finished.

## Why per-pass numbers are relative

A GPU timestamp is a device tick: the raw counter is masked to the common valid-bit width of the
timed graphics and compute queues, then scaled by `timestamp_period` nanoseconds per tick. A
compute queue reporting zero valid bits still executes its work but contributes no scopes. If the
graphics queue cannot timestamp, the profiler clamps itself to `Off`.

Adjacent GPU passes can execute concurrently, and a parent scope brackets its children, so
per-pass durations do not sum to a frame total. The frame total is the span from the earliest
begin to the latest end across all available scopes (`frame_span_ms`), and the editor labels each
pass's share "% of span" for the same reason.

## One clock for both lanes

GPU ticks and the host monotonic clock count from unrelated epochs.
[`VK_EXT_calibrated_timestamps`](https://raw.githubusercontent.com/KhronosGroup/Vulkan-Docs/main/appendices/VK_EXT_calibrated_timestamps.adoc)
samples the `DEVICE` and `CLOCK_MONOTONIC` domains at effectively the same instant, which yields
an additive offset. The read-back then projects every tick onto the host axis as
`host_ns = tick × period + offset`, so a GPU pass visibly executes after the CPU span that
submitted it. `GpuProfiler::calibrate` samples once when profiling starts and re-samples every 64
frames to track drift.

Without the extension, or without a matching host time domain, the GPU lane keeps its own
frame-relative zero and the capture's `correlated` flag is false. The editor then draws the GPU
lane on its own axis and says so in a notice.

## The capture state machine

`profiler.capture-start` arms a `CaptureRecorder` in one of three modes: `single` (one frame),
`frames` (a window clamped to 256 frames), or `rolling` (recorded forward, identical to `frames`).
Arming escalates the profiler to `Timestamps`, or to `PipelineStats` when statistics are requested
and supported; stopping restores whatever mode ran before.

The recorder walks `Arming → Recording → Ready`. Arming burns three warm-up frames
(`MAX_FRAMES_IN_FLIGHT + 1`) so every recorded frame reflects the arm-time settings despite the
read-back lag. Each recording frame appends the merged CPU and GPU spans read back at begin-frame,
rebasing every `parent_index` into the capture's growing index space; both lanes describe the same
frame, recorded two frames earlier.

`profiler.capture-status` reports progress without draining. A duplicate `capture-stop` echoes the
last completed capture instead of an empty one, so an overlapping poll never clobbers a good
result.

```sh
sa profiler.capture-start --mode frames --frames 120
# armed capture id=1  (stop with: sa profiler.capture-stop)
sa profiler.capture-stop
# captured 120 frame(s), 2640 spans  [correlated]
# trace: /tmp/saffron-profile-52114.json  (open in chrome://tracing or ui.perfetto.dev)
```

## Pipeline statistics

`ProfilerMode::PipelineStats` adds a per-frame `PIPELINE_STATISTICS` query pool beside the
timestamp pools. The graph reserves one statistics slot per top-level graphics pass and stamps the
pass's render-area pixel count into its `ScopeRecord`; the read-back decodes a slot only when its
availability word is set. Six counters ride each query, and the consumer derives the ratios that
explain a slow pass:

| Counters | What it answers |
|---|---|
| fragment invocations ÷ render-area pixels | overdraw |
| clipping primitives ÷ clipping invocations | culling efficiency |
| vertex invocations ÷ input-assembly vertices | vertex reuse |
| compute invocations | compute work recorded inside a graphics pass, normally zero |

Async-compute passes do not reserve statistics slots. Their duration still comes from timestamp
scopes when the compute queue supports them.

The counters are invocation counts, not times, so they mean the same thing on a software
rasterizer as on hardware.

## Interchange

The engine serializes a capture to the Chrome trace-event JSON that
[chrome://tracing](https://www.chromium.org/developers/how-tos/trace-event-profiling-tool/),
[Perfetto](https://perfetto.dev/), and [speedscope](https://github.com/jlfwong/speedscope) all
ingest: two `M` metadata events name the "CPU render thread" and "GPU queue" tracks, one `X`
complete event per span carries microsecond `ts`/`dur`, and the capture facts (device name,
`softwareGpu`, `correlated`, mode, frame count) ride in `otherData`. A downloaded trace is
therefore self-documenting.

A `single` capture returns its trace inline; a multi-frame capture is written to
`/tmp/saffron-profile-<pid>.json` and the command returns the path, which keeps the wire payload
bounded. The structured spans always come back inline so the editor can render any capture. The
editor also encodes the same capture as a Perfetto protobuf (the
[synthetic TrackEvent](https://perfetto.dev/docs/reference/synthetic-track-event) subset), so the
engine carries no protobuf dependency.

## Software rasterizers

A device whose name matches llvmpipe, lavapipe, or SwiftShader (or whose type is `CPU`) sets
`software_gpu`. The flag rides the capture metadata and both exports, and the editor's Profiler
panel shows a banner: GPU spans on such a device measure CPU rasterization time, not hardware
timing. [Performance telemetry](../performance-telemetry/) carries the same caveat on its always-on
numbers.

## In the code

| What | File | Symbols |
|---|---|---|
| GPU scopes + query pools | `profiler.rs` | `RgTimestamps`, `ScopeRecord`, `GpuProfiler`, `MAX_PROFILED_SCOPES` |
| CPU spans | `profiler.rs` | `CpuSpanBuffer`, `CpuMarkerRegistry`, `cpu_now_ns` |
| Per-pass bracketing | `render_graph.rs`, `nested_scopes.rs` | `record_submission_plan_profiled`, `ProfileRecorders`, `NestedScopeRecorder::scope` |
| Clock correlation + read-back | `profiler.rs` | `GpuProfiler::calibrate`, `GpuCalibration`, `GpuProfiler::readback` |
| Capture state machine | `profiler.rs` | `CaptureRecorder`, `CaptureMode`, `CaptureState`, `ProfileCapture`, `ProfileCaptureMeta` |
| Capture drive | `renderer.rs` | `Renderer::start_profile_capture`, `Renderer::stop_profile_capture` |
| Commands + JSON export | `control/src/commands_render.rs` | `profiler.capture-start`, `profiler.capture-stop`, `profiler.capture-status`, `to_chrome_trace` |
| Editor panel + Perfetto export | `ProfilerPanel.tsx`, `perfettoExport.ts` | `ProfilerPanel`, `CaptureTable`, `toPerfettoTrace` |

## Related

- [Performance telemetry](../performance-telemetry/) — the always-on smoothed numbers a capture complements
- [Performance alarms](../performance-alarms/) — automatic thresholds over the same telemetry
- [Render graph overview](../render-graph-overview/) — the passes every scope brackets
