//! The frame-time history ring, its percentile/consistency summary, and the perf-degradation alarm
//! engine.
//!
//! The ring records every frame regardless of the profiler mode — the distribution stays honest
//! only if it sees every frame, un-smoothed. The alarm detectors run on the smoothed series (an
//! EMA), never raw per-frame values, against the [`PerfConfig`] budget.

mod alarms;

pub use alarms::*;

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

#[cfg(test)]
mod tests;
