//! The frame-time history ring, its percentile/consistency summary, and the
//! perf-degradation alarm engine.
//!
//! The GPU-free instrumentation: the rolling [`FrameSample`] ring, the on-demand
//! [`FrameHistoryStats`] percentiles, the stutter detector, the shared [`PerfConfig`]
//! thresholds, and the whole alarm state machine.
//!
//! The frame ring is recorded every frame regardless of the profiler mode — the
//! distribution stays honest only if it sees every frame, un-smoothed. The alarm
//! detectors run on the smoothed series (an EMA), never raw per-frame values, against
//! the [`PerfConfig`] budget.

/// Frames kept in the rolling history ring (≈8–17 s at 60–120 Hz).
pub const FRAME_HISTORY_CAPACITY: usize = 1024;

/// Capacity of the alarm event ring (FIRING/RESOLVED history).
pub const ALARM_EVENT_RING_CAPACITY: usize = 256;

/// Focused frames the frame-time detectors wait for after the viewport regains focus (or at
/// startup) before judging performance again. It spans the frame-hitch MAD window (64), so the
/// recent-frame baseline is entirely representative before the detectors read it: the paced-down
/// frames recorded while unfocused and the TAA/GI re-convergence burst on resume are transients,
/// not real hitches, and must never raise an alarm.
pub const ALARM_RESUME_SETTLE_FRAMES: u32 = 64;

/// One frame's raw (un-smoothed) timing, pushed once per frame at end-of-frame. The
/// frame time used for percentiles + stutter is `cpu_ms + cpu_wait_ms` (the
/// render-thread wall clock: work plus the fence wait, which absorbs GPU-bound stalls).
/// `gpu_ms` is `0` unless the profiler is enabled; the history itself is always
/// recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameSample {
    /// Absolute, monotonic frame index; lets the editor accumulate a long history.
    pub frame_index: u64,
    /// Smoothed-source render-thread CPU busy time (ms) for this frame, un-smoothed here.
    pub cpu_ms: f32,
    /// GPU frame time (ms); `0` until the profiler is enabled.
    pub gpu_ms: f32,
    /// Time blocked on fences (ms).
    pub cpu_wait_ms: f32,
}

impl FrameSample {
    /// The render-thread wall-clock frame time the percentiles + stutter rule use.
    fn frame_ms(&self) -> f32 {
        self.cpu_ms + self.cpu_wait_ms
    }
}

/// Percentile / consistency summary computed on demand over the ring. The p99 frame
/// time is the 1%-low; average FPS is deliberately absent (it hides hitches).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameHistoryStats {
    /// 50th percentile (median) frame time (ms).
    pub p50_ms: f32,
    /// 95th percentile frame time (ms).
    pub p95_ms: f32,
    /// 99th percentile (1%-low) frame time (ms).
    pub p99_ms: f32,
    /// 99.9th percentile (0.1%-low) frame time (ms).
    pub p999_ms: f32,
    /// Worst frame time in the window (ms).
    pub max_ms: f32,
    /// Mean frame time (ms).
    pub mean_ms: f32,
    /// Standard deviation of the frame time (ms).
    pub stddev_ms: f32,
    /// Per-session stutter count.
    pub stutter_count: u64,
    /// Number of samples the stats were computed over.
    pub sample_count: u32,
}

/// The single source of truth for green/amber/red, shared over the wire so the engine,
/// the editor HUD, and e2e tests all agree. `budget = 1000 / target_fps`. A frame is
/// over budget (a dropped frame) past `1.0×` budget; the multipliers grade it against
/// the running median; `frozen_ms` is a hard-hitch floor that is always red.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PerfConfig {
    /// Target FPS (30/60/90/120 + custom); the budget derives from this.
    pub target_fps: f32,
    /// `< green_budget_frac × budget` (with the median check) is green.
    pub green_budget_frac: f32,
    /// `< green_median_mul × median` is consistent (green).
    pub green_median_mul: f32,
    /// `green_median_mul..amber_median_mul × median` is amber; beyond is red.
    pub amber_median_mul: f32,
    /// A hard hitch in ms → always red.
    pub frozen_ms: f32,
    /// `vram_warn_frac` of the VRAM budget = warn.
    pub vram_warn_frac: f32,
    /// `vram_crit_frac` = critical (≥ 100% = over).
    pub vram_crit_frac: f32,
    /// Auto-quality: when set, the frame-budget controller steps the render-quality tier down under
    /// sustained over-budget frames (and back up when there is headroom) to hold the budget. Off by
    /// default — the tier is then whatever the user / project set.
    pub auto_quality: bool,
}

impl Default for PerfConfig {
    fn default() -> Self {
        Self {
            target_fps: 60.0,
            green_budget_frac: 0.8,
            green_median_mul: 1.5,
            amber_median_mul: 2.0,
            frozen_ms: 250.0,
            vram_warn_frac: 0.8,
            vram_crit_frac: 0.95,
            auto_quality: false,
        }
    }
}

impl PerfConfig {
    /// The per-frame budget in ms (`1000 / target_fps`), or `0` when `target_fps <= 0`.
    pub fn budget_ms(&self) -> f32 {
        if self.target_fps > 0.0 {
            1000.0 / self.target_fps
        } else {
            0.0
        }
    }

    /// Clamps a requested config into sane ranges.
    pub fn clamped(self) -> Self {
        let green_median_mul = self.green_median_mul.max(1.0);
        let vram_warn_frac = self.vram_warn_frac.clamp(0.0, 1.0);
        Self {
            target_fps: self.target_fps.clamp(1.0, 10000.0),
            green_budget_frac: self.green_budget_frac.clamp(0.0, 1.0),
            green_median_mul,
            amber_median_mul: self.amber_median_mul.max(green_median_mul),
            frozen_ms: self.frozen_ms.max(0.0),
            vram_warn_frac,
            vram_crit_frac: self.vram_crit_frac.clamp(vram_warn_frac, 1.0),
            auto_quality: self.auto_quality,
        }
    }
}

/// The rolling frame-time history: a fixed-capacity ring plus the stutter count.
///
/// Recorded every frame at end-of-frame. The percentiles and the stutter live in the
/// per-frame distribution, so the engine records every frame (no decimation).
pub struct FrameHistory {
    ring: Box<[FrameSample; FRAME_HISTORY_CAPACITY]>,
    head: usize,
    count: usize,
    frame_serial: u64,
    stutter_count: u64,
    /// Wall-clock ns of the last detected stutter.
    last_stutter_ns: u64,
}

impl Default for FrameHistory {
    fn default() -> Self {
        Self {
            ring: Box::new([FrameSample::default(); FRAME_HISTORY_CAPACITY]),
            head: 0,
            count: 0,
            frame_serial: 0,
            stutter_count: 0,
            last_stutter_ns: 0,
        }
    }
}

impl FrameHistory {
    /// The number of filled entries (saturates at [`FRAME_HISTORY_CAPACITY`]).
    pub fn count(&self) -> usize {
        self.count
    }

    /// The per-session stutter count.
    pub fn stutter_count(&self) -> u64 {
        self.stutter_count
    }

    /// The ns of the most recent detected stutter.
    pub fn last_stutter_ns(&self) -> u64 {
        self.last_stutter_ns
    }

    /// Empties the distribution (a fresh window) — used when a project load makes the prior
    /// frames unrepresentative, so the percentiles/mean the HUD grades don't span the load.
    pub fn reset(&mut self) {
        self.head = 0;
        self.count = 0;
        self.stutter_count = 0;
        self.last_stutter_ns = 0;
    }

    /// The oldest→newest physical index for logical position `i` in `[0, count)`.
    fn ring_index(&self, i: usize) -> usize {
        let start = (self.head + FRAME_HISTORY_CAPACITY - self.count) % FRAME_HISTORY_CAPACITY;
        (start + i) % FRAME_HISTORY_CAPACITY
    }

    /// Records one frame: detects a stutter against the previous-3 average + the
    /// `2× budget` floor (before the push), then pushes the sample.
    ///
    /// A frame is a stutter when its time exceeds **both** `2×` the previous-3 average
    /// and an absolute floor of `2× budget` — the relative rule catches hitches at any
    /// frame rate, the floor rejects noise. Returns the just-written sample.
    pub fn record(
        &mut self,
        cpu_ms: f32,
        gpu_ms: f32,
        cpu_wait_ms: f32,
        budget_ms: f32,
        now_ns: u64,
    ) -> FrameSample {
        let frame_time = cpu_ms + cpu_wait_ms;
        if self.count >= 3 {
            let mut sum3 = 0.0f32;
            for k in 1..=3 {
                let idx = (self.head + FRAME_HISTORY_CAPACITY - k) % FRAME_HISTORY_CAPACITY;
                sum3 += self.ring[idx].frame_ms();
            }
            let avg3 = sum3 / 3.0;
            if frame_time > 2.0 * avg3 && frame_time > 2.0 * budget_ms {
                self.stutter_count += 1;
                self.last_stutter_ns = now_ns;
            }
        }

        let sample = FrameSample {
            frame_index: self.frame_serial,
            cpu_ms,
            gpu_ms,
            cpu_wait_ms,
        };
        self.ring[self.head] = sample;
        self.frame_serial += 1;
        self.head = (self.head + 1) % FRAME_HISTORY_CAPACITY;
        if self.count < FRAME_HISTORY_CAPACITY {
            self.count += 1;
        }
        sample
    }

    /// The on-demand percentile / consistency summary over the whole ring.
    pub fn stats(&self) -> FrameHistoryStats {
        let mut out = FrameHistoryStats {
            sample_count: self.count as u32,
            stutter_count: self.stutter_count,
            ..Default::default()
        };
        if self.count == 0 {
            return out;
        }
        let mut times: Vec<f32> = Vec::with_capacity(self.count);
        let mut sum = 0.0f32;
        for i in 0..self.count {
            let t = self.ring[self.ring_index(i)].frame_ms();
            times.push(t);
            sum += t;
        }
        out.mean_ms = sum / times.len() as f32;
        let mut variance = 0.0f32;
        for &t in &times {
            let d = t - out.mean_ms;
            variance += d * d;
        }
        out.stddev_ms = (variance / times.len() as f32).sqrt();
        times.sort_by(f32::total_cmp);
        let percentile = |p: f32| -> f32 {
            let last = times.len() - 1;
            let idx = (p * last as f32 + 0.5) as usize;
            times[idx.min(last)]
        };
        out.p50_ms = percentile(0.50);
        out.p95_ms = percentile(0.95);
        out.p99_ms = percentile(0.99);
        out.p999_ms = percentile(0.999);
        out.max_ms = *times.last().unwrap();
        out
    }

    /// The most recent `max_samples` frames, oldest→newest.
    pub fn samples(&self, max_samples: u32) -> Vec<FrameSample> {
        let take = (max_samples as usize).min(self.count);
        (self.count - take..self.count)
            .map(|i| self.ring[self.ring_index(i)])
            .collect()
    }

    /// The fraction of the most recent `window` frames that exceeded `budget` — the
    /// SLI the burn-rate detector reads.
    fn window_over_budget(&self, window: usize, budget: f32) -> f32 {
        let w = window.min(self.count);
        if w == 0 {
            return 0.0;
        }
        let over = (self.count - w..self.count)
            .filter(|&i| self.ring[self.ring_index(i)].frame_ms() > budget)
            .count();
        over as f32 / w as f32
    }

    /// The frame times of the most recent `window` frames, oldest→newest.
    fn window_times(&self, window: usize) -> Vec<f32> {
        let w = window.min(self.count);
        (self.count - w..self.count)
            .map(|i| self.ring[self.ring_index(i)].frame_ms())
            .collect()
    }
}

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
    active: Vec<ActiveAlarm>,
    events: Box<[AlarmEvent; ALARM_EVENT_RING_CAPACITY]>,
    event_head: usize,
    event_count: usize,
    next_seq: u64,
    frame_counter: u64,
    /// tau ≈ 300 ms smoothed frame time (the sustained gate).
    ema_frame_ms: f32,
    /// Debounce: seconds the smoothed frame time held over the warn-enter threshold.
    budget_warn_held_sec: f32,
    /// Seconds it held over the critical threshold.
    budget_crit_held_sec: f32,
    /// Clean frames since the last spike (to auto-resolve a hitch).
    hitch_clear_frames: u32,
    /// Consecutive focused frames observed; the frame-time detectors gate on this reaching
    /// [`ALARM_RESUME_SETTLE_FRAMES`], so an unfocused→focused resume (and startup) settles before
    /// they judge again. Reset to 0 on any unfocused frame.
    focused_streak: u32,
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

/// FNV-1a over `metric + "|" + pass + "|" + owner` — the alarm fingerprint that coalesces
/// repeats. The owner is part of the key so two cells over the same budget stay two alarms: a
/// single coalesced one would name whichever cell breached last and hide the rest.
/// An alarm's identity: the metric, the pass it belongs to, and the content that owns it. All
/// three make the fingerprint, so they travel together rather than as three positional strings a
/// caller can transpose.
#[derive(Clone, Copy)]
struct AlarmKey<'a> {
    metric: &'a str,
    pass: &'a str,
    owner: &'a str,
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

fn alarm_fingerprint(metric: &str, pass: &str, owner: &str) -> u64 {
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

    fn push_event(&mut self, mut event: AlarmEvent) -> u64 {
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
    fn raise(
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
    fn clear(&mut self, now_ns: u64, key: AlarmKey<'_>) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_yields_zero_stats() {
        let h = FrameHistory::default();
        let s = h.stats();
        assert_eq!(s.sample_count, 0);
        assert_eq!(s.p50_ms, 0.0);
        assert_eq!(s.mean_ms, 0.0);
    }

    #[test]
    fn ring_saturates_at_capacity_and_percentiles_compute() {
        let mut h = FrameHistory::default();
        // Push more than capacity; only the most recent CAPACITY are kept.
        for i in 0..(FRAME_HISTORY_CAPACITY + 100) {
            h.record(i as f32 * 0.01, 0.0, 0.0, 16.6, 0);
        }
        let s = h.stats();
        assert_eq!(
            s.sample_count, FRAME_HISTORY_CAPACITY as u32,
            "saturates at capacity"
        );
        // The window is the last CAPACITY frames: indices 100..(CAPACITY+100), times
        // 1.00..(CAPACITY+99)*0.01. The max is the last frame's time.
        let max_expected = (FRAME_HISTORY_CAPACITY + 99) as f32 * 0.01;
        assert!(
            (s.max_ms - max_expected).abs() < 1e-3,
            "max {} ~= {max_expected}",
            s.max_ms
        );
        assert!(
            s.p50_ms < s.p99_ms,
            "percentiles ordered: p50 {} < p99 {}",
            s.p50_ms,
            s.p99_ms
        );
        assert!(s.p99_ms <= s.max_ms);
    }

    #[test]
    fn percentile_of_a_uniform_window_is_near_the_value() {
        let mut h = FrameHistory::default();
        for _ in 0..200 {
            h.record(10.0, 0.0, 0.0, 16.6, 0);
        }
        let s = h.stats();
        assert_eq!(s.p50_ms, 10.0);
        assert_eq!(s.p99_ms, 10.0);
        assert_eq!(s.mean_ms, 10.0);
        assert_eq!(s.stddev_ms, 0.0);
    }

    #[test]
    fn a_2x_spike_over_the_floor_counts_as_a_stutter() {
        let mut h = FrameHistory::default();
        let budget = 16.6;
        // Steady baseline well under budget, then a single 100ms hitch (> 2× avg, > 2× budget).
        for _ in 0..4 {
            h.record(5.0, 0.0, 0.0, budget, 0);
        }
        assert_eq!(h.stutter_count(), 0);
        let s = h.record(100.0, 0.0, 0.0, budget, 12345);
        assert_eq!(s.cpu_ms, 100.0);
        assert_eq!(
            h.stutter_count(),
            1,
            "100ms > 2×avg(5) and > 2×budget(16.6)"
        );
        assert_eq!(h.last_stutter_ns(), 12345);
    }

    #[test]
    fn a_modest_miss_under_the_floor_is_not_a_stutter() {
        let mut h = FrameHistory::default();
        let budget = 16.6;
        for _ in 0..4 {
            h.record(5.0, 0.0, 0.0, budget, 0);
        }
        // 12ms is > 2× the 5ms average but < 2× budget (33.2ms) — the floor rejects it.
        h.record(12.0, 0.0, 0.0, budget, 0);
        assert_eq!(
            h.stutter_count(),
            0,
            "the 2×budget floor rejects relative-only spikes"
        );
    }

    #[test]
    fn perf_config_clamps_into_sane_ranges() {
        let c = PerfConfig {
            target_fps: -5.0,
            green_budget_frac: 2.0,
            green_median_mul: 0.5,
            amber_median_mul: 0.1,
            frozen_ms: -1.0,
            vram_warn_frac: 0.9,
            vram_crit_frac: 0.2,
            auto_quality: true,
        }
        .clamped();
        assert_eq!(c.target_fps, 1.0);
        assert_eq!(c.green_budget_frac, 1.0);
        assert_eq!(c.green_median_mul, 1.0);
        assert!(c.amber_median_mul >= c.green_median_mul);
        assert_eq!(c.frozen_ms, 0.0);
        assert!(c.vram_crit_frac >= c.vram_warn_frac, "crit floored at warn");
    }

    #[test]
    fn budget_ms_derives_from_target_fps() {
        assert!((PerfConfig::default().budget_ms() - 1000.0 / 60.0).abs() < 1e-3);
        let c = PerfConfig {
            target_fps: 0.0,
            ..Default::default()
        };
        assert_eq!(c.budget_ms(), 0.0);
    }

    /// Feed `frames` over-budget frames into the alarm engine and return its state. `focused` sets
    /// the per-frame power gate: unfocused frames are throttled, so the frame-time detectors stay
    /// quiet on them regardless of the timing.
    fn run_over_budget(frames: u32, frame_ms: f32, focused: bool) -> (FrameHistory, AlarmState) {
        let config = PerfConfig::default(); // 60fps → 16.6ms budget
        let mut history = FrameHistory::default();
        let mut alarms = AlarmState::default();
        let mut now_ns = 0u64;
        for _ in 0..frames {
            now_ns += 16_000_000;
            history.record(frame_ms, 0.0, 0.0, config.budget_ms(), now_ns);
            let inputs = AlarmInputs {
                frame_time_ms: frame_ms,
                dt_sec: 0.05, // big dt so the EMA converges quickly + debounce fills fast
                now_ns,
                vram_usage_bytes: 0,
                vram_budget_bytes: 0,
                pipelines_created: 0,
                visibility_overflow_flags: 0,
                visibility_pressure_flags: 0,
                owned_budgets: Vec::new(),
                focused,
            };
            alarms.tick(&history, &config, &inputs);
        }
        (history, alarms)
    }

    #[test]
    fn sustained_over_budget_fires_a_frame_budget_alarm() {
        // 40ms/frame is well over 1.2×16.6=20ms; the run first burns through the focus settle
        // window (ALARM_RESUME_SETTLE_FRAMES), then the EMA + 0.3s debounce fire frame-budget.
        let (_, alarms) = run_over_budget(128, 40.0, true);
        assert!(
            alarms.active().iter().any(|a| a.metric == "frame-budget"),
            "a sustained 40ms frame raises frame-budget"
        );
        let drain = alarms.drain(0);
        assert!(
            drain
                .events
                .iter()
                .any(|e| e.metric == "frame-budget" && e.kind == AlarmEventKind::Firing),
            "a FIRING event was emitted"
        );
        assert!(drain.high_water_seq >= 1);
    }

    #[test]
    fn a_steady_in_budget_session_raises_nothing() {
        let (_, alarms) = run_over_budget(120, 8.0, true); // 8ms < 16.6ms budget
        assert!(alarms.active().is_empty(), "no alarms under budget");
        let drain = alarms.drain(0);
        assert!(drain.events.is_empty());
    }

    #[test]
    fn unfocused_frames_raise_no_frame_time_alarm() {
        // Every frame is far over budget, but the viewport is unfocused (the loop paces render
        // down), so the frame-time detectors stay silent — no hitch / budget / burn toast fires
        // while the user is working in another window.
        let (_, alarms) = run_over_budget(200, 40.0, false);
        assert!(
            alarms.active().is_empty(),
            "unfocused over-budget frames raise nothing"
        );
        assert!(alarms.drain(0).events.is_empty(), "no events either");
    }

    #[test]
    fn a_resume_spike_is_swallowed_until_the_settle_window_passes() {
        // The core fix: returning to a paced-down viewport leaves a calm baseline of cheap frames
        // in the ring and re-converges the temporal effects, so the first focused frame looks like
        // a huge spike. The settle window must swallow it, then judge normally once representative.
        let config = PerfConfig::default(); // 16.6ms budget
        let mut history = FrameHistory::default();
        let mut alarms = AlarmState::default();
        let mut now_ns = 0u64;
        let budget = config.budget_ms();
        let tick = |alarms: &mut AlarmState, history: &FrameHistory, now_ns: u64, ms: f32| {
            alarms.tick(
                history,
                &config,
                &AlarmInputs {
                    frame_time_ms: ms,
                    dt_sec: 0.016,
                    now_ns,
                    vram_usage_bytes: 0,
                    vram_budget_bytes: 0,
                    pipelines_created: 0,
                    visibility_overflow_flags: 0,
                    visibility_pressure_flags: 0,
                    owned_budgets: Vec::new(),
                    focused: true,
                },
            );
        };

        // The cheap paced-down frames still sitting in the ring right after resume.
        for _ in 0..64 {
            now_ns += 16_000_000;
            history.record(4.0, 0.0, 0.0, budget, now_ns);
        }
        // Regain focus: the first focused frame is a resume transient spike; the settle window
        // must hold the hitch detector until it has ALARM_RESUME_SETTLE_FRAMES representative frames.
        for i in 0..ALARM_RESUME_SETTLE_FRAMES {
            now_ns += 16_000_000;
            let ms = if i == 0 { 40.0 } else { 4.0 };
            history.record(ms, 0.0, 0.0, budget, now_ns);
            tick(&mut alarms, &history, now_ns, ms);
        }
        assert!(
            !alarms.active().iter().any(|a| a.metric == "frame-hitch"),
            "the resume-transient spike is swallowed during the settle window"
        );

        // Past the settle window a genuine spike against the now-representative baseline fires.
        now_ns += 16_000_000;
        history.record(40.0, 0.0, 0.0, budget, now_ns);
        tick(&mut alarms, &history, now_ns, 40.0);
        assert!(
            alarms.active().iter().any(|a| a.metric == "frame-hitch"),
            "a real hitch after the settle window still fires"
        );
    }

    #[test]
    fn vram_over_critical_fires_then_clears() {
        let config = PerfConfig::default();
        let mut history = FrameHistory::default();
        let mut alarms = AlarmState::default();
        history.record(8.0, 0.0, 0.0, config.budget_ms(), 1);
        alarms.tick(
            &history,
            &config,
            &AlarmInputs {
                frame_time_ms: 8.0,
                dt_sec: 0.016,
                now_ns: 1_000_000,
                vram_usage_bytes: 99,
                vram_budget_bytes: 100, // 99% ≥ crit 95%
                pipelines_created: 0,
                visibility_overflow_flags: 0,
                visibility_pressure_flags: 0,
                owned_budgets: Vec::new(),
                focused: true,
            },
        );
        assert!(
            alarms
                .active()
                .iter()
                .any(|a| a.metric == "vram" && a.severity == AlarmSeverity::Critical)
        );

        history.record(8.0, 0.0, 0.0, config.budget_ms(), 2);
        alarms.tick(
            &history,
            &config,
            &AlarmInputs {
                frame_time_ms: 8.0,
                dt_sec: 0.016,
                now_ns: 2_000_000,
                vram_usage_bytes: 50,
                vram_budget_bytes: 100, // 50% < 0.8*0.95 = 76%
                pipelines_created: 0,
                visibility_overflow_flags: 0,
                visibility_pressure_flags: 0,
                owned_budgets: Vec::new(),
                focused: true,
            },
        );
        assert!(
            !alarms.active().iter().any(|a| a.metric == "vram"),
            "vram cleared"
        );
        let drain = alarms.drain(0);
        assert!(
            drain
                .events
                .iter()
                .any(|e| e.metric == "vram" && e.kind == AlarmEventKind::Resolved)
        );
    }

    #[test]
    fn drain_cursor_advances_and_reports_overflow() {
        let config = PerfConfig::default();
        let mut history = FrameHistory::default();
        let mut alarms = AlarmState::default();
        // Toggle pso-compile on/off many times to overflow the 256-event ring.
        for i in 0..(ALARM_EVENT_RING_CAPACITY as u32 + 50) {
            history.record(8.0, 0.0, 0.0, config.budget_ms(), i as u64);
            alarms.tick(
                &history,
                &config,
                &AlarmInputs {
                    frame_time_ms: 8.0,
                    dt_sec: 0.016,
                    now_ns: i as u64 * 1_000_000,
                    vram_usage_bytes: 0,
                    vram_budget_bytes: 0,
                    pipelines_created: if i % 2 == 0 { 1 } else { 0 },
                    visibility_overflow_flags: 0,
                    visibility_pressure_flags: 0,
                    owned_budgets: Vec::new(),
                    focused: true,
                },
            );
        }
        // The cursor at 0 falls behind the ring's oldest seq → overflow.
        let drain = alarms.drain(0);
        assert!(drain.overflowed, "the ring dropped events past seq 0");
        assert!(drain.oldest_seq > 1);
        // Draining at the high-water seq yields nothing.
        let caught_up = alarms.drain(drain.high_water_seq);
        assert!(caught_up.events.is_empty());
    }

    #[test]
    fn alarm_fingerprint_coalesces_by_metric_pass_and_owner() {
        assert_eq!(
            alarm_fingerprint("frame-budget", "", ""),
            alarm_fingerprint("frame-budget", "", "")
        );
        assert_ne!(
            alarm_fingerprint("frame-budget", "", ""),
            alarm_fingerprint("frame-hitch", "", "")
        );
        assert_ne!(
            alarm_fingerprint("vram", "scene", ""),
            alarm_fingerprint("vram", "tonemap", "")
        );
        // The owner is part of the key so two cells over the same budget stay two alarms; keying
        // on the metric alone would name whichever breached last and hide every other one.
        assert_ne!(
            alarm_fingerprint("vegetation-cell-plants", "", "cell 0,0,0 L0"),
            alarm_fingerprint("vegetation-cell-plants", "", "cell 1,0,0 L0")
        );
    }

    #[test]
    fn an_owned_budget_fires_while_reported_and_resolves_when_it_stops() {
        // The contract the reporter has to hold up: the breach set is COMPLETE every frame, so an
        // owned alarm resolves by absence. A reporter that published only on breach would leave
        // its alarms firing after the condition cleared, and nothing downstream would notice.
        let mut alarms = AlarmState::default();
        let history = FrameHistory::default();
        let config = PerfConfig::default();
        let breach = OwnedBudgetBreach {
            metric: "vegetation-cell-plants".to_owned(),
            owner: "cell 0,0,0 L0".to_owned(),
            severity: AlarmSeverity::Warning,
            value: 9.0,
            threshold: 4.0,
        };
        let inputs = |budgets: Vec<OwnedBudgetBreach>| AlarmInputs {
            frame_time_ms: 1.0,
            dt_sec: 0.016,
            now_ns: 1_000_000,
            vram_usage_bytes: 0,
            vram_budget_bytes: 0,
            pipelines_created: 0,
            visibility_overflow_flags: 0,
            visibility_pressure_flags: 0,
            owned_budgets: budgets,
            focused: true,
        };

        alarms.tick(&history, &config, &inputs(vec![breach.clone()]));
        let active = alarms.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].owner, "cell 0,0,0 L0");
        assert_eq!(active[0].metric, "vegetation-cell-plants");

        // Reported again: coalesced in place rather than raising a second alarm.
        alarms.tick(&history, &config, &inputs(vec![breach]));
        assert_eq!(alarms.active().len(), 1);
        assert_eq!(alarms.active()[0].count, 2);

        alarms.tick(&history, &config, &inputs(Vec::new()));
        assert!(alarms.active().iter().all(|alarm| alarm.owner.is_empty()));
        let drained = alarms.drain(0);
        assert!(drained.events.iter().any(|event| {
            event.owner == "cell 0,0,0 L0" && event.kind == AlarmEventKind::Resolved
        }));
    }
}
