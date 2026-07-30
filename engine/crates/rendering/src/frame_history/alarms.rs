//! The perf-degradation alarm engine: the severity ladder, the per-alarm fingerprint, and the
//! state machine that raises, sustains, and clears each alarm off the smoothed frame series.

use super::*;

/// How serious an alarm is. Ordered Info < Warning < Critical so an escalation is a
/// simple comparison; `Warning` is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlarmSeverity {
    /// Log only (a single PSO-compile hitch, a TAA reset).
    Info,
    /// Throttled toast + highlight the offending row.
    #[default]
    Warning,
    /// Persistent log entry + active-alarms badge.
    Critical,
}

/// Whether an alarm event is the alarm firing or resolving.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlarmEventKind {
    /// The alarm started (or escalated).
    #[default]
    Firing,
    /// The alarm cleared.
    Resolved,
}

/// A currently-firing alarm, keyed by `fingerprint = hash(metric + "|" + pass)` so a
/// repeated breach coalesces into one entry (count/peak update in place).
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveAlarm {
    /// `hash(metric + "|" + pass)` — the coalescing key.
    pub fingerprint: u64,
    /// The metric: `frame-budget`, `frame-hitch`, `burn-rate`, `vram`, `pso-compile`,
    /// `visibility-overflow`, `visibility-pressure`.
    pub metric: String,
    /// The offending pass, empty for whole-frame alarms.
    pub pass: String,
    /// What the breach belongs to, empty for whole-frame alarms — a vegetation cell, a plant
    /// family and the asset it came from, or whatever else raised it. An alarm without an owner
    /// says a budget broke; one with an owner says which content broke it, which is the
    /// difference between a number to watch and a thing to fix.
    pub owner: String,
    /// The (escalating) severity.
    pub severity: AlarmSeverity,
    /// The current breached value (ms for time metrics, % for vram/burn).
    pub value: f32,
    /// The threshold it crossed, same units.
    pub threshold: f32,
    /// The worst value seen while active.
    pub peak: f32,
    /// The frame the alarm started firing.
    pub since_frame: u64,
    /// The ns the alarm started firing.
    pub since_ns: u64,
    /// The ns the alarm was last re-observed.
    pub last_seen_ns: u64,
    /// Times re-observed while active.
    pub count: u32,
}

/// An append-only, seq-stamped FIRING/RESOLVED event drained over a non-blocking cursor.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlarmEvent {
    /// Monotonic sequence number assigned on push.
    pub seq: u64,
    /// The alarm's fingerprint.
    pub fingerprint: u64,
    /// The metric.
    pub metric: String,
    /// The offending pass, empty for whole-frame alarms.
    pub pass: String,
    /// What the breach belongs to; see [`ActiveAlarm::owner`].
    pub owner: String,
    /// The severity.
    pub severity: AlarmSeverity,
    /// Firing or resolved.
    pub kind: AlarmEventKind,
    /// The breached value.
    pub value: f32,
    /// The threshold crossed.
    pub threshold: f32,
    /// The frame the alarm started.
    pub since_frame: u64,
    /// Times re-observed while active.
    pub count: u32,
    /// Wall-clock duration in ms (RESOLVED only).
    pub duration_ms: f32,
}

/// The snapshot [`AlarmState::drain`] returns: events with `seq > since`, the
/// high-water seq, the oldest still-retained seq, and whether the ring dropped events
/// past `since`.
#[derive(Clone, Debug, Default)]
pub struct AlarmDrain {
    /// The events newer than the cursor.
    pub events: Vec<AlarmEvent>,
    /// The highest seq assigned so far.
    pub high_water_seq: u64,
    /// The oldest seq still retained in the ring.
    pub oldest_seq: u64,
    /// Whether events between `since` and `oldest` fell off the ring.
    pub overflowed: bool,
}

/// The alarm engine: the active set, the seq-stamped event ring, and the per-detector
/// smoothing/debounce state advanced once per frame.
pub struct AlarmState {
    pub(super) active: Vec<ActiveAlarm>,
    pub(super) events: Box<[AlarmEvent; ALARM_EVENT_RING_CAPACITY]>,
    pub(super) event_head: usize,
    pub(super) event_count: usize,
    pub(super) next_seq: u64,
    pub(super) frame_counter: u64,
    /// tau ≈ 300 ms smoothed frame time (the sustained gate).
    pub(super) ema_frame_ms: f32,
    /// Debounce: seconds the smoothed frame time held over the warn-enter threshold.
    pub(super) budget_warn_held_sec: f32,
    /// Seconds it held over the critical threshold.
    pub(super) budget_crit_held_sec: f32,
    /// Clean frames since the last spike (to auto-resolve a hitch).
    pub(super) hitch_clear_frames: u32,
    /// Consecutive focused frames observed; the frame-time detectors gate on this reaching
    /// [`ALARM_RESUME_SETTLE_FRAMES`], so an unfocused→focused resume (and startup) settles before
    /// they judge again. Reset to 0 on any unfocused frame.
    pub(super) focused_streak: u32,
}

impl Default for AlarmState {
    fn default() -> Self {
        Self {
            active: Vec::new(),
            events: Box::new(std::array::from_fn(|_| AlarmEvent::default())),
            event_head: 0,
            event_count: 0,
            next_seq: 1,
            frame_counter: 0,
            ema_frame_ms: 0.0,
            budget_warn_held_sec: 0.0,
            budget_crit_held_sec: 0.0,
            hitch_clear_frames: 0,
            focused_streak: 0,
        }
    }
}

/// An alarm's identity: the metric, the pass it belongs to, and the content that owns it. All
/// three make the fingerprint, so they travel together rather than as three positional strings a
/// caller can transpose.
#[derive(Clone, Copy)]
pub(super) struct AlarmKey<'a> {
    pub(super) metric: &'a str,
    pub(super) pass: &'a str,
    pub(super) owner: &'a str,
}

impl<'a> AlarmKey<'a> {
    /// A whole-frame alarm: no pass, no owner.
    const fn frame(metric: &'a str) -> Self {
        Self {
            metric,
            pass: "",
            owner: "",
        }
    }

    /// A budget an outside subsystem owns.
    const fn owned(metric: &'a str, owner: &'a str) -> Self {
        Self {
            metric,
            pass: "",
            owner,
        }
    }
}

/// FNV-1a over `metric + "|" + pass + "|" + owner`, coalescing repeats. The owner is part of the
/// key so two cells over the same budget stay two alarms; a single coalesced one would name
/// whichever cell breached last and hide the rest.
pub(super) fn alarm_fingerprint(metric: &str, pass: &str, owner: &str) -> u64 {
    let mut hash = 14695981039346656037u64;
    let mut mix = |text: &str| {
        for c in text.bytes() {
            hash ^= c as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
    };
    mix(metric);
    mix("|");
    mix(pass);
    mix("|");
    mix(owner);
    hash
}

/// One budget breach a subsystem outside this crate observed, with the content that owns it.
///
/// The renderer's own detectors read frame timings and GPU counters and can name a pass at best.
/// A vegetation cell over its plant budget, or a family whose predicted blade density outruns
/// what the field can hold, is invisible from here — the crate has no vegetation dependency and
/// must not grow one. So the owner computes the breach and hands it in, and the alarm machinery
/// (coalescing, escalation, FIRING/RESOLVED events, the drain cursor) applies to it unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OwnedBudgetBreach {
    /// The metric name, e.g. `vegetation-cell-plants`.
    pub metric: String,
    /// What owns the breach — a cell key, a family and its source asset, a provenance string.
    pub owner: String,
    /// How bad it is.
    pub severity: AlarmSeverity,
    /// The observed value.
    pub value: f32,
    /// The budget it crossed.
    pub threshold: f32,
}

/// The per-frame inputs the alarm detectors read besides the frame history.
pub struct AlarmInputs {
    /// The render-thread wall-clock frame time (ms) for this frame.
    pub frame_time_ms: f32,
    /// The wall-clock delta since the last frame (s); drives the EMA + debounce.
    pub dt_sec: f32,
    /// The current wall-clock time (ns); stamps event timing.
    pub now_ns: u64,
    /// VRAM usage in bytes (0 = unknown / not profiling).
    pub vram_usage_bytes: u64,
    /// VRAM budget in bytes (0 = unknown / not profiling).
    pub vram_budget_bytes: u64,
    /// PSOs compiled this frame (a mid-frame compile is a hitch).
    pub pipelines_created: u32,
    /// The visibility pass's overflow flag word: a capacity that filled and CLAMPED.
    ///
    /// Nonzero means geometry the frame should have drawn was silently dropped, which is the one
    /// failure that looks like a correct render. The flags existed on the wire and raised nothing,
    /// so noticing required a person to go and read them.
    pub visibility_overflow_flags: u32,
    /// The visibility pass's pressure flag word: a capacity approaching its limit but not yet lost.
    pub visibility_pressure_flags: u32,
    /// This frame's owned budget breaches from outside this crate — see [`OwnedBudgetBreach`].
    /// The list is the COMPLETE set of breaches that hold right now: an owned alarm whose breach
    /// is absent resolves, so a caller that stops reporting a cell resolves its alarm rather than
    /// leaving it firing forever.
    pub owned_budgets: Vec<OwnedBudgetBreach>,
    /// Whether the viewport is focused (rendering at the full target rate). While unfocused the
    /// loop paces render down and on resume it re-converges the temporal effects, so those frame
    /// timings are unrepresentative; the frame-time detectors stay quiet until the viewport has
    /// been focused for [`ALARM_RESUME_SETTLE_FRAMES`] frames.
    pub focused: bool,
}

impl AlarmState {
    /// The currently-firing alarms.
    pub fn active(&self) -> &[ActiveAlarm] {
        &self.active
    }

    /// The frame counter the detectors advance.
    pub fn frame_counter(&self) -> u64 {
        self.frame_counter
    }

    /// Restart the post-resume settle window (see [`ALARM_RESUME_SETTLE_FRAMES`]). Called when the
    /// viewport leaves the focused state, so any later return — including from an occluded viewport,
    /// which stops rendering (and so ticking) entirely — re-settles before the frame-time detectors
    /// judge the re-convergence burst.
    pub fn reset_focus_settle(&mut self) {
        self.focused_streak = 0;
    }

    pub(super) fn push_event(&mut self, mut event: AlarmEvent) -> u64 {
        let seq = self.next_seq;
        event.seq = seq;
        self.next_seq += 1;
        self.events[self.event_head] = event;
        self.event_head = (self.event_head + 1) % ALARM_EVENT_RING_CAPACITY;
        if self.event_count < ALARM_EVENT_RING_CAPACITY {
            self.event_count += 1;
        }
        seq
    }

    /// Raise (or refresh) an alarm. The first breach emits one FIRING; while active the
    /// count/peak update in place and only a severity escalation emits another FIRING.
    pub(super) fn raise(
        &mut self,
        now_ns: u64,
        key: AlarmKey<'_>,
        severity: AlarmSeverity,
        value: f32,
        threshold: f32,
    ) {
        let AlarmKey {
            metric,
            pass,
            owner,
        } = key;
        let fingerprint = alarm_fingerprint(metric, pass, owner);
        if let Some(active) = self
            .active
            .iter_mut()
            .find(|a| a.fingerprint == fingerprint)
        {
            active.last_seen_ns = now_ns;
            active.count += 1;
            active.value = value;
            active.threshold = threshold;
            active.peak = active.peak.max(value);
            if severity > active.severity {
                active.severity = severity;
                let escalation = AlarmEvent {
                    fingerprint,
                    metric: metric.to_string(),
                    pass: pass.to_string(),
                    owner: owner.to_string(),
                    severity,
                    kind: AlarmEventKind::Firing,
                    value,
                    threshold,
                    since_frame: active.since_frame,
                    count: active.count,
                    ..Default::default()
                };
                self.push_event(escalation);
            }
            return;
        }
        let fresh = ActiveAlarm {
            fingerprint,
            metric: metric.to_string(),
            pass: pass.to_string(),
            owner: owner.to_string(),
            severity,
            value,
            threshold,
            peak: value,
            since_frame: self.frame_counter,
            since_ns: now_ns,
            last_seen_ns: now_ns,
            count: 1,
        };
        let since_frame = fresh.since_frame;
        self.active.push(fresh);
        let firing = AlarmEvent {
            fingerprint,
            metric: metric.to_string(),
            pass: pass.to_string(),
            owner: owner.to_string(),
            severity,
            kind: AlarmEventKind::Firing,
            value,
            threshold,
            since_frame,
            count: 1,
            ..Default::default()
        };
        self.push_event(firing);
    }

    /// Clear an active alarm if present, emitting one RESOLVED (with duration + peak).
    pub(super) fn clear(&mut self, now_ns: u64, key: AlarmKey<'_>) {
        let AlarmKey {
            metric,
            pass,
            owner,
        } = key;
        let fingerprint = alarm_fingerprint(metric, pass, owner);
        if let Some(pos) = self
            .active
            .iter()
            .position(|a| a.fingerprint == fingerprint)
        {
            let a = self.active.remove(pos);
            let resolved = AlarmEvent {
                fingerprint,
                metric: a.metric,
                pass: a.pass,
                owner: a.owner,
                severity: a.severity,
                kind: AlarmEventKind::Resolved,
                value: a.peak,
                threshold: a.threshold,
                since_frame: a.since_frame,
                count: a.count,
                duration_ms: (now_ns - a.since_ns) as f32 / 1.0e6,
                ..Default::default()
            };
            self.push_event(resolved);
        }
    }

    /// One per-frame alarm tick: smooth, then gate. Runs detectors on the smoothed
    /// series (never raw per-frame values) against the shared [`PerfConfig`].
    pub fn tick(&mut self, history: &FrameHistory, config: &PerfConfig, inputs: &AlarmInputs) {
        self.frame_counter += 1;
        let budget = config.budget_ms();
        let now_ns = inputs.now_ns;
        let frame_time_ms = inputs.frame_time_ms;

        // Irregular-interval EMA (tau ≈ 300 ms): alpha = 1 − exp(−dt / tau).
        if inputs.dt_sec > 0.0 {
            let alpha = 1.0 - (-inputs.dt_sec / 0.3).exp();
            if self.ema_frame_ms == 0.0 {
                self.ema_frame_ms = frame_time_ms;
            } else {
                self.ema_frame_ms += alpha * (frame_time_ms - self.ema_frame_ms);
            }
        } else if self.ema_frame_ms == 0.0 {
            self.ema_frame_ms = frame_time_ms;
        }

        // Frame-time perf gate. While the viewport is unfocused the loop paces render down, and on
        // resume it re-converges the temporal effects — both make the frame timings unrepresentative,
        // so the frame-time detectors (budget / hitch / burn-rate) would fire false positives every
        // time the user returns to the viewport. Hold them until the viewport has been focused for a
        // full settle window (also swallows the startup burst), and while held clear any that were
        // active and reset the debounce so a pre-blur alarm and the resume transient never surface.
        // VRAM and PSO-compile are focus-independent and run unconditionally below.
        let perf_ready = if inputs.focused {
            self.focused_streak = (self.focused_streak + 1).min(ALARM_RESUME_SETTLE_FRAMES);
            self.focused_streak >= ALARM_RESUME_SETTLE_FRAMES
        } else {
            self.focused_streak = 0;
            false
        };
        if !perf_ready {
            self.budget_warn_held_sec = 0.0;
            self.budget_crit_held_sec = 0.0;
            self.hitch_clear_frames = 0;
            self.clear(now_ns, AlarmKey::frame("frame-budget"));
            self.clear(now_ns, AlarmKey::frame("frame-hitch"));
            self.clear(now_ns, AlarmKey::frame("burn-rate"));
        }

        // frame-budget: sustained over-budget with hysteresis (enter 1.2× / exit 1.0×) +
        // a debounce; escalates to critical at 2× budget.
        if perf_ready && budget > 0.0 {
            let enter_th = 1.2 * budget;
            let exit_th = budget;
            let critical_th = 2.0 * budget;
            if self.ema_frame_ms > enter_th {
                self.budget_warn_held_sec += inputs.dt_sec;
            } else {
                self.budget_warn_held_sec = 0.0;
            }
            if self.ema_frame_ms > critical_th {
                self.budget_crit_held_sec += inputs.dt_sec;
            } else {
                self.budget_crit_held_sec = 0.0;
            }
            let warn_ready = self.budget_warn_held_sec >= 0.3;
            let crit_ready = self.budget_crit_held_sec >= 0.5;
            if warn_ready {
                let severity = if crit_ready {
                    AlarmSeverity::Critical
                } else {
                    AlarmSeverity::Warning
                };
                self.raise(
                    now_ns,
                    AlarmKey::frame("frame-budget"),
                    severity,
                    self.ema_frame_ms,
                    enter_th,
                );
            } else if self.ema_frame_ms < exit_th {
                self.clear(now_ns, AlarmKey::frame("frame-budget"));
            }
        }

        // frame-hitch: a robust spike via the modified z-score over a recent window
        // (median/MAD beat mean/stddev — the outlier inflates stddev and masks itself).
        let window = history.count.min(64);
        if perf_ready && window >= 8 {
            let mut sorted = history.window_times(window);
            sorted.sort_by(f32::total_cmp);
            let median = sorted[sorted.len() / 2];
            for v in &mut sorted {
                *v = (*v - median).abs();
            }
            sorted.sort_by(f32::total_cmp);
            let mad = sorted[sorted.len() / 2].max(0.05); // floor guards MAD == 0
            let mod_z = 0.6745 * (frame_time_ms - median) / mad;
            if mod_z > 3.5 && budget > 0.0 && frame_time_ms > budget {
                self.hitch_clear_frames = 0;
                let severity = if frame_time_ms > 2.0 * budget {
                    AlarmSeverity::Warning
                } else {
                    AlarmSeverity::Info
                };
                self.raise(
                    now_ns,
                    AlarmKey::frame("frame-hitch"),
                    severity,
                    frame_time_ms,
                    median + mad * 3.5 / 0.6745,
                );
            } else {
                self.hitch_clear_frames += 1;
                if self.hitch_clear_frames >= 10 {
                    self.clear(now_ns, AlarmKey::frame("frame-hitch"));
                }
            }
        }

        // burn-rate: a short and a long window must both breach (fast detect, low
        // false-positive, clears quickly when the problem stops).
        if perf_ready && budget > 0.0 && history.count >= 60 {
            let sli_short = history.window_over_budget(60, budget); // ~1 s @ 60 Hz
            let sli_long = history.window_over_budget(600, budget); // ~10 s
            if sli_short > 0.5 && sli_long > 0.5 {
                self.raise(
                    now_ns,
                    AlarmKey::frame("burn-rate"),
                    AlarmSeverity::Critical,
                    sli_short * 100.0,
                    50.0,
                );
            } else if sli_short > 0.1 && sli_long > 0.1 {
                self.raise(
                    now_ns,
                    AlarmKey::frame("burn-rate"),
                    AlarmSeverity::Warning,
                    sli_short * 100.0,
                    10.0,
                );
            } else if sli_short < 0.05 {
                self.clear(now_ns, AlarmKey::frame("burn-rate"));
            }
        }

        // capacity: a clamp has already lost geometry, so it is CRITICAL rather than a warning —
        // there is no recovering the draw that was dropped, and a frame that looks right while
        // missing content is the failure mode the flags exist to make loud. Pressure is the warning
        // ahead of it: the budget is nearly gone but nothing has been lost yet.
        if inputs.visibility_overflow_flags != 0 {
            self.raise(
                now_ns,
                AlarmKey::frame("visibility-overflow"),
                AlarmSeverity::Critical,
                f32::from(
                    u16::try_from(inputs.visibility_overflow_flags.count_ones()).unwrap_or(0),
                ),
                0.0,
            );
        } else {
            self.clear(now_ns, AlarmKey::frame("visibility-overflow"));
        }
        if inputs.visibility_pressure_flags != 0 {
            self.raise(
                now_ns,
                AlarmKey::frame("visibility-pressure"),
                AlarmSeverity::Warning,
                f32::from(
                    u16::try_from(inputs.visibility_pressure_flags.count_ones()).unwrap_or(0),
                ),
                0.0,
            );
        } else {
            self.clear(now_ns, AlarmKey::frame("visibility-pressure"));
        }

        // vram: usage fraction of the device-local budget (only known when profiling).
        if inputs.vram_budget_bytes > 0 {
            let frac = inputs.vram_usage_bytes as f32 / inputs.vram_budget_bytes as f32;
            if frac >= config.vram_crit_frac {
                self.raise(
                    now_ns,
                    AlarmKey::frame("vram"),
                    AlarmSeverity::Critical,
                    frac * 100.0,
                    config.vram_crit_frac * 100.0,
                );
            } else if frac >= config.vram_warn_frac {
                self.raise(
                    now_ns,
                    AlarmKey::frame("vram"),
                    AlarmSeverity::Warning,
                    frac * 100.0,
                    config.vram_warn_frac * 100.0,
                );
            } else if frac < config.vram_warn_frac * 0.95 {
                self.clear(now_ns, AlarmKey::frame("vram"));
            }
        }

        // pso-compile: a PSO built mid-frame is a hitch on a steady-state frame (info).
        if inputs.pipelines_created > 0 {
            self.raise(
                now_ns,
                AlarmKey::frame("pso-compile"),
                AlarmSeverity::Info,
                inputs.pipelines_created as f32,
                0.0,
            );
        } else {
            self.clear(now_ns, AlarmKey::frame("pso-compile"));
        }

        // Owned budgets: raised by whoever can see the content, resolved by absence. Clearing the
        // stale ones first would drop and immediately re-raise an alarm that is still breaching —
        // one FIRING/RESOLVED pair per frame in the drain — so this raises, then clears only what
        // this frame did not report.
        for breach in &inputs.owned_budgets {
            self.raise(
                now_ns,
                AlarmKey::owned(&breach.metric, &breach.owner),
                breach.severity,
                breach.value,
                breach.threshold,
            );
        }
        let stale: Vec<(String, String)> = self
            .active
            .iter()
            .filter(|alarm| !alarm.owner.is_empty())
            .filter(|alarm| {
                !inputs
                    .owned_budgets
                    .iter()
                    .any(|breach| breach.metric == alarm.metric && breach.owner == alarm.owner)
            })
            .map(|alarm| (alarm.metric.clone(), alarm.owner.clone()))
            .collect();
        for (metric, owner) in stale {
            self.clear(now_ns, AlarmKey::owned(&metric, &owner));
        }
    }

    /// Drain events newer than the `since` cursor over the non-blocking ring.
    pub fn drain(&self, since: u64) -> AlarmDrain {
        let mut out = AlarmDrain {
            high_water_seq: self.next_seq - 1,
            ..Default::default()
        };
        if self.event_count > 0 {
            let oldest_idx = (self.event_head + ALARM_EVENT_RING_CAPACITY - self.event_count)
                % ALARM_EVENT_RING_CAPACITY;
            out.oldest_seq = self.events[oldest_idx].seq;
        }
        // Events between `since+1` and `oldest-1` fell off: the client must resync.
        out.overflowed = out.oldest_seq > since + 1;
        for i in 0..self.event_count {
            let idx = (self.event_head + ALARM_EVENT_RING_CAPACITY - self.event_count + i)
                % ALARM_EVENT_RING_CAPACITY;
            if self.events[idx].seq > since {
                out.events.push(self.events[idx].clone());
            }
        }
        out
    }
}
