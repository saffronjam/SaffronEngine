+++
title = 'Performance alarms'
weight = 8
math = true
+++

# Performance alarms

Telemetry answers a question when the user asks it; an alarm notices the moment performance
degrades, with every panel closed. Beyond the thresholds themselves the alarm engine has two jobs:
never spam (a raw per-frame value crosses any line constantly) and never stall the render loop (the
control socket is drained once per frame on the render thread). This page covers the detection
math, the severity model, and the poll-drained delivery channel.

## Smooth, then gate

Detectors never judge raw per-frame values. `finalize_frame_telemetry` pushes each raw frame into
the [history ring](../performance-telemetry/) and then runs `AlarmState::tick` over it, handing the
detectors the render-thread wall-clock frame time, the VRAM usage/budget pair, and the count of
pipelines compiled this frame. Everything is graded against the shared `PerfConfig` budget
(`1000 / targetFps` ms). Three mechanisms keep the output quiet:

- **Irregular-interval EMA.** Frames are not evenly spaced, so the smoothed frame time uses the
  time-constant form of [exponential smoothing](https://en.wikipedia.org/wiki/Exponential_smoothing):
  $\alpha = 1 - e^{-\Delta t / \tau}$ with $\tau \approx 300$ ms, then
  `ema += alpha × (sample − ema)`.
- **Hysteresis.** The sustained detector enters at `1.2 × budget` and exits at `1.0 × budget`;
  between the two thresholds the state holds, so an alarm cannot chatter at the boundary.
- **Debounce.** The smoothed value must stay over the enter threshold for 0.3 s before anything
  fires. Hysteresis stops edge oscillation; debounce stops a single slow frame from firing at all.

The frame-time detectors also gate on focus. An unfocused viewport is paced down to 6 FPS, and the
first frames after focus returns are the temporal effects re-converging; both are transients, not
hitches. The frame-time detectors therefore stay held until the viewport has been focused for
`ALARM_RESUME_SETTLE_FRAMES` (64) consecutive frames — one full hitch-detector window, so the
recent-frame baseline is representative before they read it. The same gate swallows the startup
burst.

While the gate holds, any active frame-time alarm is resolved and the debounce resets, so a
pre-blur toast never lingers into the resumed session. Leaving the focused state (including an
occluded viewport, which stops rendering entirely) restarts the settle window via
`reset_focus_settle`. The VRAM and PSO-compile detectors run regardless of focus.

## Five detectors

- **frame-budget** (sustained). The EMA over `1.2 × budget` for 0.3 s raises a warning; holding
  over `2 × budget` for 0.5 s escalates it to critical (under 30 FPS at a 60 Hz budget). It clears
  when the EMA drops back under the budget, the hysteresis exit.
- **frame-hitch** (spike). A [modified z-score](https://www.itl.nist.gov/div898/handbook/eda/section3/eda35h.htm)
  over the most recent 64 ring frames: $M = 0.6745\,(x - \tilde{x}) / \mathrm{MAD}$, fired at
  $M > 3.5$ on a frame that is also over budget. Median and MAD resist the very outlier being
  hunted (a spike inflates a mean/stddev baseline and masks itself); a 0.05 ms floor guards
  `MAD == 0`. The spike is info, or warning past `2 × budget`, and auto-resolves after 10 clean
  frames.
- **burn-rate** (sustained user pain). The SLI is the fraction of frames over budget, checked over
  a 60-frame and a 600-frame window (≈1 s and ≈10 s at 60 Hz) in the style of
  [multiwindow burn-rate alerting](https://sre.google/workbook/alerting-on-slos/): both windows
  must breach, so detection is fast yet a lone burst cannot fire it. Both over 10% is a warning,
  both over 50% critical; it clears once the short window falls under 5%.
- **vram.** Usage as a fraction of the device-local budget: `vramWarnFrac` (default 80%) is a
  warning, `vramCritFrac` (95%) critical. The detector runs only while a budget is known
  (`vram_budget_bytes > 0`) and clears just below the warn threshold.
- **pso-compile.** A pipeline built mid-frame (`RenderStats::pipelines_created > 0`) is a compile
  hitch on an otherwise steady frame: an info entry, cleared on the next frame that compiles
  nothing.

| Severity | Fires for | In the editor |
|---|---|---|
| info | a hitch under `2 × budget`; a mid-frame PSO compile | alarm log only |
| warning | sustained over budget; a `2 × budget` spike; both burn windows over 10% | toast (throttled to one per fingerprint per 10 s) + row highlight |
| critical | EMA over `2 × budget` for 0.5 s; both burn windows over 50%; VRAM ≥ 95% | persistent toast + the active-alarms badge |

## Deliver without blocking

The control socket is drained once per frame on the render thread, so a long-poll handler that
held its request open until an event arrived would block rendering for the hold. The design is the
inverse: detectors append events to a fixed 256-entry ring (`ALARM_EVENT_RING_CAPACITY`), a pure
CPU append, and `drain-alarms` snapshots the ring and returns immediately. The editor's metrics
lane polls it on the metrics refresh interval (default 1 s).

### The cursor

Every event carries a monotonic `seq`. A client sends `drain-alarms {since}` and receives the
events with `seq > since` plus the new `highWaterSeq` — the
[`Last-Event-ID`](https://html.spec.whatwg.org/multipage/server-sent-events.html) resume contract
from server-sent events, done over polling. A dropped or late poll catches up on the next one, and
an already-seen event is never re-sent. When events past `since` have fallen off the ring, the
reply sets `overflowed: true` with the `oldestSeq` still retained, and the client resyncs from
`list-active-alarms` instead of silently losing history.

### One FIRING, one RESOLVED

Two structures back the wire surface. The **active set** holds the alarms firing right now, keyed
by an FNV-1a fingerprint of `metric + "|" + pass`, and drives the badge (`list-active-alarms`);
the whole-frame detectors leave `pass` empty. The **event ring** is the append-only, seq-stamped
FIRING/RESOLVED history behind the cursor.

While a fingerprint is active, a repeat breach updates its `count` and `peak` in place; a second
FIRING is emitted only on a severity escalation. When the metric recovers past its exit threshold,
the alarm leaves the active set and emits one RESOLVED carrying the duration and peak, and the
editor dismisses the matching toast without a user click.

## Driving it

The target FPS lives on the upscale surface (`set-upscale {targetMs}`, with
`targetFps = 1000 / targetMs`), so an impossibly tight budget forces a breach. The detectors judge
a focused, settled viewport: allow the 64-frame settle window plus the 0.3 s debounce before
expecting the first FIRING.

```sh
sa set-upscale --targetMs 0.5    # targetFps=2000 → every frame is over budget
sa drain-alarms --since 0        #   #1  firing  warning  frame-budget  ...
sa list-active-alarms            #   warning  frame-budget  ...
sa set-upscale --targetMs 16.67  # a 60 Hz budget again
sa drain-alarms --since 0        # ... plus the RESOLVED event, with duration + peak
```

## In the code

| What | File | Symbols |
|---|---|---|
| Per-frame detector tick + focus gate | `frame_history.rs` | `AlarmState::tick`, `AlarmInputs`, `ALARM_RESUME_SETTLE_FRAMES`, `reset_focus_settle` |
| Active set, event ring, fingerprint | `frame_history.rs` | `AlarmState`, `ActiveAlarm`, `AlarmEvent`, `AlarmSeverity`, `alarm_fingerprint`, `ALARM_EVENT_RING_CAPACITY` |
| Non-blocking drain + cursor | `frame_history.rs`, `renderer.rs` | `AlarmState::drain`, `AlarmDrain`, `Renderer::drain_alarms`, `active_alarms` |
| Shared thresholds | `frame_history.rs` | `PerfConfig`, `budget_ms` |
| Wire surface | `protocol/src/dto.rs`, `control/src/commands_render.rs` | `AlarmEventDto`, `ActiveAlarmDto`, `DrainAlarmsResult`, `drain-alarms`, `list-active-alarms` |
| Editor routing | `alarmToasts.ts`, `store.ts` | `routeAlarmToasts`, `startReconcile` |

## Related

- [Performance telemetry](../performance-telemetry/) — the frame ring, counters, and shared `PerfConfig` the detectors read
- [Control plane](../../tooling-and-control/control-plane-architecture/) — the once-per-frame socket drain the delivery is shaped around
- [Renderer profiling](../renderer-profiling/) — the capture tool for digging into what an alarm flagged
