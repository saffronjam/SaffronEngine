+++
title = 'Performance alarms'
weight = 8
+++

# Performance alarms

Telemetry tells you what happened when you look; an alarm tells you the *moment* performance
degrades, without making you stare at a panel. The hard parts are not the thresholds — they are
*not spamming* (a raw per-frame value crosses any line constantly) and *not stalling the render
loop* (the control plane is pumped synchronously, once per frame, on the render thread). This page
covers the detection math, the severity model, and the non-blocking delivery channel that fits the
existing request/response control plane.

## Detect: smooth, then gate

Detectors run on a *smoothed* series, never raw per-frame values. The inputs are the Phase-1/2
per-frame signals (frame time, the history ring, VRAM, the throughput counters).

- **Irregular-interval EMA.** Frames are not evenly spaced, so the smoothing factor is
  `alpha = 1 − exp(−dt / tau)`, then `ema += alpha × (sample − ema)`. The sustained gate uses
  `tau ≈ 300 ms`.
- **Hysteresis (deadband).** Separate enter/exit thresholds so an alarm does not chatter at the
  boundary. Frame time vs. the Phase-2 budget: **enter** at `1.2 × budget`, **exit** at
  `1.0 × budget`. Between them the state holds.
- **Debounce.** The enter condition must hold for ~300 ms before the alarm fires at all. Hysteresis
  stops edge oscillation; debounce stops a single slow frame from ever firing. They are orthogonal.
- **Focus gate.** The three frame-*time* detectors only judge a viewport that is focused and has
  settled. An unfocused viewport is paced down (the reactive loop caps it to a few FPS), and the
  first frames back are the temporal effects re-converging — both are transients, not real hitches.
  So the frame-time detectors stay held until the viewport has been focused for a full settle window
  (`ALARM_RESUME_SETTLE_FRAMES`, one frame-hitch MAD window, so the recent-frame baseline is entirely
  representative before they read it); the same gate swallows the startup burst. While held, any
  active frame-time alarm is resolved so a pre-blur toast never lingers. VRAM and PSO-compile are
  focus-independent and always run.

## Three (plus two) detectors → severity

- **frame-budget (sustained).** The smoothed frame time over budget, gated by hysteresis + debounce.
  Escalates to **critical** when it holds over `2 × budget` (≈ < 30 FPS at a 60 Hz budget) for 0.5 s.
- **frame-hitch (robust spike).** A modified z-score over a recent window of the history ring:
  `mod_z = 0.6745 × (x − median) / MAD`, where `MAD = median(|xᵢ − median|)`; fire at `mod_z > 3.5`.
  Median/MAD beat mean/stddev because the very outlier you are hunting inflates stddev and masks
  itself; a small floor guards `MAD == 0`. → **warning**, auto-resolved after clean frames.
- **burn-rate (sustained user-pain).** The SLI is the fraction of frames over budget; a **short and a
  long window must both breach** (fast detection, low false-positive, clears quickly). Frame-scaled:
  short ≈ 1 s and long ≈ 10 s over the ring; > 10% → warning, > 50% → critical.
- **vram.** Usage as a fraction of the device-local budget: ≥ `vramWarnFrac` → warning,
  ≥ `vramCritFrac` → critical (only meaningful while the profiler is on, so the budget is known).
- **pso-compile (info).** A PSO built mid-frame (`RenderStats::pipelines_created > 0`) is a compile
  hitch on an otherwise steady-state frame → an **info** entry in the log.

All thresholds come from the `PerfConfig`, so they stay data-driven and shared with the HUD.

| Severity | Example | UX (Phase 4) |
|---|---|---|
| info | single PSO-compile hitch | alarm log only |
| warning | z-score hitch; sustained over budget; short+long burn breach | throttled toast + highlight |
| critical | sustained < 30 FPS > 0.5 s; both burn windows breach; VRAM over budget | persistent log + badge |

> [!NOTE]
> Per-pass **drift / regression** ("this pass got 30% slower after that asset change") is the one
> detector deferred to a follow-up — it needs a per-pass baseline captured on scene load. The
> whole-frame sustained-regression case is already covered by frame-budget + burn-rate.

## Deliver: a non-blocking queue, not a long-poll

The control plane is drained synchronously, once per frame, on the render thread. A long-poll handler
that *held the request open* until an event was ready would block **rendering** for the hold
duration — exactly wrong here. The fit is the inverse:

- Detectors run each frame and **append** events to a fixed-size engine-side ring
  (`ALARM_EVENT_RING_CAPACITY = 256`). Appending is pure CPU bookkeeping — no stall.
- `drain-alarms` returns the queued events *immediately*; its handler only snapshots the ring. The
  editor calls it alongside `render-stats` in the existing ~6 Hz reconcile loop.

### Cursor: never miss, never double-count

Every event carries a monotonic `seq`. The client sends `drain-alarms { since: <lastSeq> }` and gets
events with `seq > since` plus the new `highWaterSeq` — SSE's `Last-Event-ID` contract done over
polling. A dropped or late poll just catches up next time; already-seen events are never re-sent. If
the ring dropped events past `since`, the result sets `overflowed: true` and reports `oldestSeq` so
the client resyncs from `list-active-alarms` instead of silently losing events.

### Two structures, coalesced

- **Active set**, keyed by `fingerprint = hash(metric + "|" + pass)` — the current firing alarms;
  drives the badge and row highlights (`list-active-alarms`).
- **Event log** (append-only, seq-stamped) — the FIRING/RESOLVED history behind the cursor.

While a fingerprint is already active, repeats update its `count` / `peak` in place — **one** FIRING
on entry (plus one more only on a severity escalation). When the metric recovers past the hysteresis
*exit* threshold, the alarm leaves the active set and emits **one** RESOLVED (same fingerprint, with
duration + peak) so the editor auto-clears the toast without a user dismiss.

### Why not push (yet)

SSE / a second async socket would shave the ≤ 167 ms poll latency, but each needs a persistent
connection and an off-thread send path the engine does not have. For an editor perf monitor ≤ 167 ms
is imperceptible, so the non-blocking queue on the existing poll wins; a push channel is a noted
later upgrade, not built now.

## Driving it

```sh
sa set-perf-config --targetFps 5000  # an impossibly tight budget, to force a breach
sa drain-alarms --since 0            # FIRING events + high-water/oldest/overflow cursor
sa list-active-alarms                # the current firing set (the badge)
sa set-perf-config --targetFps 60    # relax → the next drain shows the RESOLVED
```

## In the code

| What | File | Symbols |
|---|---|---|
| Detectors + the per-frame tick | `frame_history.rs` | `AlarmState::tick`, `raise`, `clear`, `AlarmInputs` (`focused`), `ALARM_RESUME_SETTLE_FRAMES` |
| Alarm state: active set, event ring, seq, fingerprint | `frame_history.rs` | `AlarmState`, `ActiveAlarm`, `AlarmEvent`, `AlarmSeverity`, `AlarmEventKind`, `alarm_fingerprint` |
| Non-blocking delivery + cursor | `frame_history.rs`, `renderer.rs` | `AlarmState::drain`, `AlarmDrain`, `Renderer::drain_alarms`, `active_alarms` |
| Wire surface | `protocol/src/dto.rs`, `control/src/commands_render.rs` | `AlarmEventDto`, `ActiveAlarmDto`, `drain-alarms`, `list-active-alarms` |

## Related

- [Performance telemetry](../performance-telemetry/) — the per-frame signals + the shared `PerfConfig` the detectors read
- [Render graph](../render-graph-overview/) — where the per-pass timings the alarms can attribute to come from
