+++
title = 'Metrics dashboard'
weight = 16
+++

# Metrics dashboard

The engine measures performance; the editor's metrics dashboard makes it legible. It consumes the
telemetry the control plane exposes — the CPU/GPU frame split, per-pass GPU timings, the percentile
quality metrics, the shared budget config, and the alarm event stream — and renders a live frame-time
graph, per-pass bars, a VRAM gauge, and the alarm UX. Everything is coloured from the *one* shared
`PerfConfig`, so the HUD agrees with the engine and the e2e tests on what green, amber, and red mean.

The dashboard ("Stats") is one of two performance **tools** — alongside the [Profiler](profiler-panel/)
— opened from the Topbar's **Tools** menu as a [dock panel](dock-system/).

## A separate, gated polling lane

The reconcile poll already syncs the cheap interactive state at ~20 Hz. Telemetry rides a **second**
lane at ~10 Hz so it never competes with selection/gizmo latency: alarms are drained every tick (the
badge stays live even with the panel closed), while the heavier `frame-history` and `pass-timings`
reads only run while the Stats tool is open (`isPanelOpen(state, "stats")`). Each fetch fills a Zustand slice; the alarm cursor
(`since`) only advances, so a missed poll just catches up next time and nothing is double-counted.

## Charting: Canvas for live, DOM for the rest

The webview composites *over* the live engine viewport, so editor CPU is not free. The live frame-time
graph is therefore **uPlot** (Canvas 2D, ~48 KB), not an SVG charting library where every point is a
DOM node and every tick a React re-render. The uPlot instance is created once; data is pushed
imperatively via `setData` on a `requestAnimationFrame` tick — never through React state. A dashed
budget line is drawn each frame from the shared config.

Raw per-frame data is far too jittery to read, so the graph never plots it directly. The client
accumulates a long history (`lib/frameSeries.ts`, a fixed typed-array ring ≈5 min, deduplicated by the
engine's absolute `frameIndex` since consecutive polls return overlapping windows). The graph then
downsamples Grafana-style, with three orthogonal knobs in one settings **shadcn Popover** (a compact
`range · bucket · refresh` chip):

- **Range** — how far back to show (10 s … 5 min), bounded by the ring.
- **Window** — the bucket / group-by interval (50 ms … 1 s); samples within it are averaged into one
  point. This is the *smoothness* knob — larger buckets, fewer points, calmer line.
- **Refresh** — how often the lane fetches (default 1/s); plus Pause, which freezes the whole dashboard.

Points = `range / bucket`, capped at 200 by coarsening (Grafana's min-interval behaviour). The x axis
reads as seconds-ago (newest at the right is "now"). The line is a monotone-cubic **spline** (no
overshoot below 0). The **Y axis is stable**: instead of re-fitting the data max every poll (which made
it jump), `scales.y.range` keeps a sticky, nice-rounded (1/2/5×10ⁿ), budget-anchored ceiling — it grows
immediately to fit a spike but shrinks only after the target has sat well below it for a few updates,
so the dashed budget line stays at a fixed height. Everything else (percentile cards, per-pass bars,
the VRAM gauge) is plain DOM, updated at the fetch rate.

## What it shows

- **Frame** — the budget-fill bar (frame time / budget), the CPU-bound-vs-GPU-bound call (from
  `cpuWaitMs` vs `cpuFrameMs`), and the p50/p95/p99/p99.9 lows. p50 is primary; average FPS is absent.
- **Per-pass GPU** — each pass in ms and %-of-budget, bar-coloured against its share, sorted heaviest
  first. A note flags that the numbers are relative (passes overlap on the GPU, so the parts do not
  sum to the frame total).
- **VRAM** — `usage / budget` of the device-local heaps, green < 80% / amber 80–95% / red ≥ 100%.
- **Controls** — a **Profiler** switch (`profiler.set-mode`, gating GPU + per-pass + VRAM) and a
  **Target FPS** dropdown (`set-perf-config`) that re-derives the budget and recolours the whole HUD.
- **Software-GPU banner** — under llvmpipe, a banner marks the GPU numbers as CPU rasterization time.

## Alarm UX

The drained alarm events route by severity, fatigue-aware: **info** stays in the dashboard log only;
**warning** raises a throttled `sonner` toast (≥ 10 s per fingerprint); **critical** raises a
persistent toast plus the topbar **active-alarms badge** (red if any critical, amber if any warning).
A RESOLVED event dismisses the toast its fingerprint raised, so a flapping alarm is one toast and
recovery clears it without a user dismiss. The badge opens the Stats dashboard, which holds the log.

## Color thresholds: one shared module

`lib/perfThresholds.ts` mirrors the engine's `PerfConfig` rule and returns green/amber/red for a
value — used by the graph, the numbers, the per-pass bars, and the gauge. No component hardcodes a
threshold: 🟢 `< 0.8 × budget` and consistent; 🟡 near budget or `1.5–2 × median`; 🔴 over budget,
`> 2 × median`, or the frozen band.

## In the code

| What | File | Symbols |
|---|---|---|
| Gated metrics poll + telemetry/alarm slices | `editor/src/state/store.ts` | `pollMetrics`, `appendAlarmEvents`, `setFrameHistory` |
| Client wrappers | `editor/src/control/client.ts` | `frameHistory`, `passTimings`, `setProfilerMode`, `setPerfConfig`, `drainAlarms` |
| Live graph (spline, sticky-Y, time axis) | `editor/src/components/FrameTimeGraph.tsx` | `niceCeil`, `yRange`, budget-line draw hook |
| History ring + Grafana bucketing | `editor/src/lib/frameSeries.ts` | `appendFrameSamples`, `bucketSeries` |
| Range / Window / Refresh / Pause control | `editor/src/components/MetricsRefreshControl.tsx` | `MetricsRefreshControl` |
| Shared thresholds | `editor/src/lib/perfThresholds.ts` | `frameTimeStatus`, `vramStatus`, `passStatus` |
| Dashboard + per-pass + VRAM + controls | `editor/src/panels/RenderStatsPanel.tsx` | `RenderStatsPanel` |
| Alarm toasts + badge | `editor/src/lib/alarmToasts.ts` · `components/AlarmBadge.tsx` | `routeAlarmToasts`, `AlarmBadge` |

## Related

- [Performance telemetry](../../frame-and-render-graph/performance-telemetry/) — the engine signals the dashboard reads
- [Performance alarms](../../frame-and-render-graph/performance-alarms/) — the alarm engine behind the toasts + badge
- [Editor shell + viewport bridge](../editor-shell-and-viewport-bridge/) — the control passthrough + reconcile poll
