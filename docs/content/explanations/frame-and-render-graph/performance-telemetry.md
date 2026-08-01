+++
title = 'Performance telemetry'
weight = 7
+++

# Performance telemetry

A frame-time number alone cannot say *why* a frame is slow. The CPU records frame N+1 while the GPU
still executes frame N, so a slow frame can be CPU-bound (the GPU idles waiting for work) or
GPU-bound (the CPU blocks on a fence), and a correct total hides which pass regressed. The telemetry
layer separates the two clocks, times every [render-graph](../render-graph-overview/) pass on the
GPU, counts per-frame throughput, and keeps a raw frame-time history graded against a shared budget.

The GPU profiler is off by default and free when off: no query pools exist and the per-pass
recorders are unarmed, so each scope compiles down to a skipped branch. The frame-time ring and the
[performance alarms](../performance-alarms/) are plain CPU bookkeeping and run on every rendered
frame regardless of the profiler mode.

## The CPU/GPU split

The run loop brackets each rendered frame in `step_frame`: a busy window opens before `on_update`
and closes after the render, and the frame-fence wait inside `begin_frame` is timed separately.
Busy time is that span minus the wait, so it measures render-thread CPU work rather than wall
clock. `finalize_frame_telemetry` receives both numbers once per rendered frame.

- **`cpuFrameMs`** — render-thread busy time, the work the CPU did this frame.
- **`cpuWaitMs`** — time blocked on the GPU fence. High wait means GPU-bound; near-zero wait with
  `cpuFrameMs` close to the budget means CPU-bound.

Both display values are smoothed with an
[exponential moving average](https://en.wikipedia.org/wiki/Exponential_smoothing): seeded on the
first frame, then `0.9 × old + 0.1 × new`. The headline `frameMs` (and the FPS derived from it) is
the same EMA over the wall-clock delta between rendered frames, so idle pacing never pollutes it.

A completed project load calls `reset_frame_telemetry`, which empties the history and withholds the
next `TELEMETRY_WARMUP_FRAMES` (12) frames from every consumer. The cold frames right after a scene
swap are dominated by PSO compiles and acceleration-structure builds, and grading them would paint
the HUD red the moment a project opens.

## Per-pass GPU timing

When a mode is armed, the renderer hands the graph an `RgTimestamps` recorder through
`ProfileRecorders`, and `record_submission_plan_profiled` opens a GPU scope around each timed pass: a begin
timestamp before the pass's derived barriers, an end timestamp after its body. A pass body can open
child scopes through `NestedScopeRecorder::scope` when `sub_scopes` is set. Each scope pushes a
`ScopeRecord` carrying the name plus `parent_index`/`depth`, so the tree stays flat-and-tagged.

The mechanics follow the Vulkan
[timestamp-query rules](https://docs.vulkan.org/spec/latest/chapters/queries.html):

- One `TIMESTAMP` query pool **per frame in flight**, sized `2 × MAX_PROFILED_SCOPES` (160 scopes),
  allocated by `GpuProfiler::allocate_pools` the first time a mode turns on.
- The slot's pool is reset on the command buffer at the top of the frame
  (`cmd_reset_query_pool`) — a query's result is undefined until its pool is reset.
- `cmd_write_timestamp2` stamps the begin at `TOP_OF_PIPE` and the end at `BOTTOM_OF_PIPE`.
- Read-back targets the pool written `MAX_FRAMES_IN_FLIGHT` frames ago, right after that slot's
  fence wait, with `TYPE_64 | WITH_AVAILABILITY`. It never blocks; a `NOT_READY` result keeps the
  last good read-back.
- Raw ticks are masked to the common valid-bit width of every timed queue and scaled by
  `timestamp_period` into nanoseconds. A compute batch is left uninstrumented when its queue reports
  zero timestamp bits.

> [!NOTE]
> Per-pass numbers are *relative*. Sibling scopes can overlap on the GPU and a parent brackets its
> children, so the parts do not sum to the whole. The reported total is the wall-clock span from
> the earliest begin to the latest end (`frame_span_ms`), never a sum.

## Throughput counters

The frame derives a `RenderStats` from the visibility chain's GPU readback (the counters lag
by the frames-in-flight depth — a slot reports its last use):

| Counter | Meaning |
|---|---|
| `drawCalls` | draw records the traversal emitted |
| `batches` | live executor draw buckets (distinct shader + PSO-bin combos) |
| `instances` | instances the visibility cull kept |
| `triangles` | triangles the emitted records rasterize |
| `sceneGatherMs` | CPU time spent deriving the frame's deformation work and ray instances |
| `sceneGatherEntities` | instances that derivation visited, counted before it resolves anything (zero on a steady scene, whatever its size) |
| `instanceUploadBytes` | GPU-scene table bytes staged this frame ((near-)zero when idle) |
| `retainedMeshCpuBytes` | mirrored-mesh host bytes retained for exact surface queries |
| `shadowDrawCalls` | counted-indirect draws recorded across the frame's virtual-shadow pages |
| `vsm` | shadow-page activity: `requested`, `hits`, `allocated`, `rendered`, `dirtied`, `evicted`, `overflow` |
| `rtInstances` | instances published into the active frame TLAS |
| `descriptorBinds` | descriptor-set binds recorded in the scene pass |
| `commandBuffers` | submitted primaries: prefix, graph batches, and tail |
| `queueSubmits` | matching `vkQueueSubmit2` calls |
| `asyncComputeQueue` | whether the device exposes an independent compute queue family |
| `asyncComputeBatches` | command buffers this frame's plan submitted on that queue |
| `pipelinesCreated` | PSOs compiled this frame |

`asyncComputeBatches` is where the async lane becomes observable. It counts what the frame plan
actually placed on the independent compute queue, so it falls to zero both when no pass asks for
the lane and when the graph declines a request it cannot derive an ownership transfer for. On a
device reporting `asyncComputeQueue: false` the same passes run on graphics and the count is
always zero.

`pipelinesCreated` is the signature of a PSO-compile hitch: non-zero on a steady-state frame means
a shader was built mid-frame, and it feeds the `pso-compile` alarm. The executor path binds
descriptor sets per pass rather than per draw, so `descriptorBinds` stays flat as the scene
grows, and `instanceUploadBytes` stays at (near-)zero on a steady scene — render preparation
scales with changes, not with instance count.

`sceneGatherEntities` is the CPU half of that same claim, and it counts a visit rather than a
result on purpose: a gather that walked every instance and found nothing to deform would
otherwise report zero cost. Its terms are the mirrored set whenever the reach cut is re-derived
plus every deformation candidate the frame resolves, so the displaced and morphing sets are
maintained incrementally rather than filtered out of the instance map each frame.

`render-stats` also carries `vramUsageBytes` / `vramBudgetBytes`, and `PerfConfig` carries warn and
crit fractions for grading them. Both are resampled every frame from the allocator's per-heap
budgets and summed over the heaps the device flags `DEVICE_LOCAL` — so a discrete adapter reports
its video memory and a unified-memory adapter reports system memory, which on each is the pool the
renderer competes for. The sample is taken ahead of the post-load warm-up gate, because occupancy
is a level rather than a distribution and a cold frame's reading is still true.

Where the driver offers
[`VK_EXT_memory_budget`](https://docs.vulkan.org/spec/latest/chapters/memory.html) the figures are
the driver's, so they include memory this allocator never handed out — swapchain images, pipelines,
descriptor heaps, and anything else sharing the adapter. Where it does not, the allocator falls back
to its own block totals against a fraction of each heap's size. Neither path can report a zero
budget on a running renderer, so the HUD gauge and the `vram` alarm always have a scale to grade
against.

## Frame history

Smoothness lives in the distribution of many frames, not in one number. The engine owns a
fixed-size ring of the last `FRAME_HISTORY_CAPACITY` (1024) frames, about seventeen seconds at
60 Hz, pushed once per rendered frame. It records the *raw* per-frame values, never the EMA-smoothed
display ones: smoothing belongs on the display path, and a smoothed series makes the distribution
lie. The frame time it tracks is `cpu_ms + cpu_wait_ms`, the render-thread wall clock whose fence
wait absorbs GPU-bound stalls.

The `frame-history` summary is computed on demand over the ring, whether or not the profiler is on:

- **Percentiles.** p50 (the median, the primary number), p95, p99, p99.9, plus mean, stddev, and
  the max. A high percentile frame time is a low frame rate: the p99 frame time corresponds to the
  1%-low FPS. Average FPS is absent by design — it oversamples fast frames and hides hitches.
- **Stutter.** A frame counts as a stutter only when its time exceeds **both** `2 ×` the
  previous-3-frame average **and** `2 ×` the budget. The relative rule catches hitches at any frame
  rate; the absolute floor rejects noise at trivially fast rates. The per-session count rides the
  wire as `stutterCount`.

At the default 60 fps target the floor is 33.3 ms. A 30 ms frame in a run of 5 ms frames is a
relative spike but not a stutter; a 40 ms frame clears both rules and counts.

## One shared threshold config

A single `PerfConfig` feeds the engine detectors, the editor HUD, and the e2e tests, so a threshold
is never hardcoded in a client. The budget derives as `1000 / targetFps`.

| Knob | Default | Meaning |
|---|---|---|
| `targetFps` | 60 | budget = `1000 / targetFps` = 16.67 ms |
| `greenBudgetFrac` | 0.8 | frame times below this fraction of budget can grade green |
| `greenMedianMul` | 1.5 | frame times past this multiple of the running median drop to amber |
| `amberMedianMul` | 2.0 | above this multiple of the median grades red |
| `frozenMs` | 250 | a hard hitch, always red |
| `vramWarnFrac` / `vramCritFrac` | 0.8 / 0.95 | VRAM grading fractions |
| `autoQuality` | off | frame-budget controller steps the quality tier to hold the budget |

The editor's `frameTimeStatus` applies the grading: **red** when the frame time exceeds the budget,
`frozenMs`, or `amberMedianMul × median`; **amber** when it reaches `greenBudgetFrac × budget` or
`greenMedianMul × median`; **green** otherwise. `get-perf-config` / `set-perf-config` read and
update the threshold knobs. The target itself is retargeted through `set-upscale {targetMs}`, which
sets `target_fps = 1000 / targetMs` and can enable dynamic resolution, so the grading budget and
the dynamic-resolution driver stay one number.

## Naming a hang while it hangs

Every timing number above is measured after the work completes, which makes them silent for the one
failure they matter most for: a submission that never completes. A fence wait is unbounded, so the
thread that would print the number is itself blocked.

A watchdog inverts that. Each submission registers a name — a one-off's label, or the frame serial
— before it waits, and unregisters when it returns. A background thread wakes twice a second and
reports anything registered longer than three seconds, once per elapsed second:

```text
ERROR rendering  GPU submission is still in flight — a hang, not a slow frame  submission="one-off 'bake_material_thumbnail'" seconds=4
```

The thread only sleeps and reads, so it keeps reporting while every other thread is blocked on a
fence that will never signal. A slow-but-finite submission is reported by the elapsed-time warns
instead; the watchdog line means the work did not finish.

It runs in every build, shipped games included: a hang in the field is where nobody can attach a
debugger, and that log line is the whole diagnosis. Its cost is one uncontended mutex acquisition
and a scan of sixteen fixed slots per submission — no allocation, nothing growable, and nothing
formatted until a report is actually due. A seventeenth concurrent submission goes untracked rather
than allocating.

When the hang matures into an `ERROR_DEVICE_LOST`, two diagnostic extensions turn the bare code
into a named culprit. With `VK_NV_device_diagnostic_checkpoints`, every render-graph pass and
one-off upload submission drops a named marker into its command stream, and the loss paths query
each queue for the last marker its front end and retirement reached — bracketing the wedged work
to one pass. With `VK_EXT_device_fault`, the driver adds what kind of fault it saw and, when it
knows them, the faulting GPU addresses. Both are enabled whenever the device offers them and cost
one driver call per pass; on hardware without them the loss report is just the error code, as
before:

```text
ERROR rendering  device loss checkpoint: 'wind-deform' reached TOP_OF_PIPE
ERROR rendering  device fault address: READ_INVALID at 0xf744246000 (precision 0x1000)
```

Each watchdog report asks for those diagnostics too, so a hang names its pass from the watchdog's
own thread rather than only from whichever thread the loss surfaces on. Both queries read valid
data only while the device is in the lost state, so the report asks again every second and prints
the checkpoint lines the moment the answer is yes; until then it is the submission name and the age
alone. The watchdog reaches the device weakly and takes the queue's external-synchronization lock
without blocking on it — a wedged `vkDeviceWaitIdle` holds that lock for the length of the hang,
which is exactly when the report has to come out.

## Modes and capability

`profiler.set-mode {off | timestamps | pipeline-stats}` selects the depth. `timestamps` allocates
the query pools and arms the per-pass recorders. `pipeline-stats` adds a `PIPELINE_STATISTICS` pool
per frame in flight — six counters per top-level graphics pass (input vertices, vertex, clipping,
fragment, compute invocations, and clipped primitives) — and requires the `pipelineStatisticsQuery`
device feature. Compute-queue passes receive timestamps but no pipeline-statistics query. The
bounded CPU+GPU capture built on these recorders is described in
[renderer profiling](../renderer-profiling/).

`GpuProfiler::set_mode` degrades a request the device cannot satisfy: no timestamp support means
`off`, no pipeline-statistics feature means `timestamps`, a pool-allocation failure means `off`.
The reply reports `timestampsSupported`, `pipelineStatsSupported`, and `softwareGpu` so the editor
can disable controls the device cannot drive.

> [!WARNING]
> On a software rasterizer (Mesa llvmpipe/lavapipe, common in headless and CI runs) "the GPU" is
> the CPU, and GPU timestamps measure CPU rasterization time. The `softwareGpu` flag rides every
> payload so downstream annotates or suppresses GPU-timing magnitudes. In-engine queries say
> *what* is slow; the micro-architectural *why* still needs a vendor profiler.

## Driving it

Everything is on the control plane, so the `sa` CLI drives it from a shell:

```sh
sa profiler.set-mode timestamps
# mode=timestamps  timestamps=yes  pipeline-stats=yes

sa render-stats
# cpu=2.41ms  gpu=3.87ms  wait=1.02ms  fps=144  draws=12  tris=48720  binds=3  pso+=0  vram=1223/6860MiB

sa pass-timings
#   depth-prepass                     0.412 ms
#   scene                             2.103 ms
#   tonemap                           0.298 ms
#   total (span)                      3.870 ms

sa frame-history
# p50=6.94  p95=7.61  p99=9.02  p99.9=14.75  max=21.30  stddev=1.12  budget=16.67ms  stutters=2  n=1024

sa get-perf-config
# targetFps=60  budget=16.67ms  green<0.80×budget  amber<2.0×median  frozen=250ms  vram warn/crit=80%/95%

sa profiler.set-mode off
```

`render-stats` carries the headline times and counters for the common poll; the per-pass array and
the sample ring sit behind the separate `pass-timings` and `frame-history` commands so the hot-path
query stays small. `frame-history` answers with or without the profiler, since the ring is always
recorded.

## In the code

| What | File | Symbols |
|---|---|---|
| Run-loop bracketing (busy vs. wait) | `app/src/lib.rs` | `step_frame` |
| CPU EMAs + the per-frame telemetry tail | `renderer.rs` | `Renderer::observe_cpu_frame`, `observe_frame_delta`, `finalize_frame_telemetry`, `reset_frame_telemetry`, `RenderStatsFull` |
| Profiler state, pools, modes, read-back | `profiler.rs` | `GpuProfiler`, `ProfilerMode`, `RgTimestamps`, `ScopeRecord`, `PassTiming`, `MAX_PROFILED_SCOPES`, `allocate_pools`, `set_mode`, `frame_recorder`, `readback`, `frame_span_ms` |
| Per-pass and nested scopes | `render_graph.rs`, `nested_scopes.rs` | `record_submission_plan_profiled`, `ProfileRecorders`, `NestedScopeRecorder` |
| Draw-path counters | `draw_list.rs` | `RenderStats` |
| Device-local memory sample | `resources/`, `device/` | `VramUsage`, `DeviceResources::vram_usage`, `create_allocator` |
| Hang watchdog | `watchdog.rs`, `upload.rs`, `renderer.rs` | `watch`, `InFlight`, `attach_device`, `with_one_off_commands`, `begin_offscreen_frame` |
| Device-loss diagnostics | `checkpoints.rs`, `device.rs`, `upload.rs`, `render_graph.rs` | `Checkpoints`, `DeviceFault`, `Device::log_hang_diagnostics`, `Device::log_device_loss_checkpoints`, `GpuQueue::reports_device_lost`, `Error::is_device_loss` |
| Frame ring, percentiles, stutter, config | `frame_history.rs` | `FrameHistory`, `FrameSample`, `FrameHistoryStats`, `PerfConfig`, `FRAME_HISTORY_CAPACITY` |
| HUD grading | `editor/src/lib/perfThresholds.ts` | `frameTimeStatus`, `vramStatus` |
| Wire surface | `protocol/src/dto.rs`, `control/src/commands_render.rs` | `RenderStatsDto`, `RenderPassTimingsDto`, `FrameHistoryDto`, `PerfConfigDto`, `profiler.set-mode`, `pass-timings`, `frame-history`, `get-perf-config`, `set-perf-config` |

## Related

- [Render graph](../render-graph-overview/) — the pass walk the timestamp scopes bracket
- [Renderer profiling](../renderer-profiling/) — the bounded CPU+GPU capture built on these recorders
- [Performance alarms](../performance-alarms/) — the detectors that watch these signals
- [Control plane](../../tooling-and-control/control-plane-architecture/) — the wire the telemetry travels on
