//! The bounded capture state machine: the armed frame window, the accumulated lanes and
//! spans, and the finished [`ProfileCapture`] the control plane drains.

use super::*;

/// The lane a [`ProfileSpan`] sits on in a merged capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileLane {
    /// The CPU render-thread timeline.
    Cpu,
    /// The GPU timeline (projected onto the CPU axis when correlated).
    Gpu,
}

/// The merged span record the whole profiler stack speaks: a CPU pass span or a GPU
/// scope projected onto the CPU axis. Flat-and-tagged; `parent_index`/`depth` decode
/// the tree, `lane` separates the two timelines.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSpan {
    /// The span name.
    pub name: String,
    /// Which timeline lane.
    pub lane: ProfileLane,
    /// Begin (ns).
    pub start_ns: u64,
    /// End (ns).
    pub end_ns: u64,
    /// The enclosing span index in this capture, or `-1` at top level.
    pub parent_index: i32,
    /// Nesting depth.
    pub depth: u32,
    /// Whether pipeline statistics are present.
    pub has_stats: bool,
    /// Pipeline statistics, when `has_stats`.
    pub stats: PipelineStats,
}

/// Self-documenting capture metadata: the honesty flags plus the device + clock facts a
/// downloaded trace needs to be interpreted on its own.
#[derive(Clone, Debug, Default)]
pub struct ProfileCaptureMeta {
    /// Whether GPU timings are software-rasterizer (llvmpipe) times.
    pub software_gpu: bool,
    /// Whether GPU spans were correlated onto the CPU clock.
    pub correlated: bool,
    /// The physical-device name.
    pub device_name: String,
    /// ns per timestamp tick.
    pub timestamp_period: f32,
    /// The target FPS the capture was taken at.
    pub target_fps: f32,
    /// The profiler mode the capture ran in.
    pub mode: ProfilerMode,
    /// The pass-name prefix filter (a view hint).
    pub filter: String,
    /// The number of frames recorded.
    pub frame_count: u32,
}

/// A bounded profiler capture: the merged spans plus the metadata.
#[derive(Clone, Debug, Default)]
pub struct ProfileCapture {
    /// The merged CPU+GPU spans across all recorded frames.
    pub spans: Vec<ProfileSpan>,
    /// The capture metadata.
    pub meta: ProfileCaptureMeta,
}

/// Bounded capture: a single frame, a fixed N-frame window, or rolling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    /// One frame (the default snapshot).
    #[default]
    Single,
    /// A fixed N-frame window.
    Frames,
    /// A recent rolling window (recorded forward like Frames in v1).
    Rolling,
}

/// Hard cap on a capture's frame count so the span buffer cannot OOM.
pub const MAX_CAPTURE_FRAMES: u32 = 256;

/// The capture state machine. `Arming` warms up for the GPU read-back delay so every
/// recorded frame reflects the arm-time settings; `Recording` copies each finalized
/// frame's merged spans; `Ready` holds the result until drained.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureState {
    /// Not capturing.
    #[default]
    Idle,
    /// Warming up to flush the read-back delay.
    Arming,
    /// Copying finalized frames.
    Recording,
    /// The capture is complete, awaiting drain.
    Ready,
}

/// The capture recorder driven by `profiler.capture-start/stop`.
pub struct CaptureRecorder {
    /// The current state.
    pub state: CaptureState,
    /// The capture mode.
    pub mode: CaptureMode,
    /// Frames to record before going `Ready`.
    pub target_frames: u32,
    /// Frames copied so far.
    pub captured_frames: u32,
    /// `Arming` frames left (covers the read-back delay).
    pub warmup: u32,
    /// Pass-name prefix carried into metadata (a view hint).
    pub filter: String,
    /// Whether to include the CPU lane.
    pub include_cpu: bool,
    /// Whether PipelineStats mode was requested (if supported).
    pub include_stats: bool,
    /// The profiler mode restored on stop.
    pub prior_mode: ProfilerMode,
    /// The `sub_scopes` flag restored on stop.
    pub prior_sub_scopes: bool,
    /// Id of the in-flight / last capture.
    pub capture_id: u32,
    pub(super) next_capture_id: u32,
    /// The capture accumulating while `Recording`.
    pub capture: ProfileCapture,
    /// The last completed (non-empty) drain, echoed by a redundant `stop` so a duplicate
    /// stop never returns an empty capture in place of a good one.
    pub(super) last_capture: ProfileCapture,
}

impl Default for CaptureRecorder {
    fn default() -> Self {
        Self {
            state: CaptureState::Idle,
            mode: CaptureMode::Single,
            target_frames: 1,
            captured_frames: 0,
            warmup: 0,
            filter: String::new(),
            include_cpu: true,
            include_stats: false,
            prior_mode: ProfilerMode::Off,
            prior_sub_scopes: false,
            capture_id: 0,
            next_capture_id: 1,
            capture: ProfileCapture::default(),
            last_capture: ProfileCapture::default(),
        }
    }
}

impl CaptureRecorder {
    /// Starts a capture, arming the profiler to the requested level (PipelineStats only
    /// when stats are wanted + supported, else Timestamps) and warming up to flush the
    /// read-back delay. Returns the new capture id. The profiler's prior mode/sub-scopes
    /// are saved for `stop`.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        device: &Device,
        profiler: &mut GpuProfiler,
        mode: CaptureMode,
        frames: u32,
        filter: String,
        include_cpu: bool,
        include_stats: bool,
    ) -> u32 {
        self.mode = mode;
        self.target_frames = if mode == CaptureMode::Single {
            1
        } else {
            frames.clamp(1, MAX_CAPTURE_FRAMES)
        };
        self.captured_frames = 0;
        self.warmup = (MAX_FRAMES_IN_FLIGHT + 1) as u32; // flush the read-back delay
        self.filter = filter;
        self.include_cpu = include_cpu;
        self.include_stats = include_stats && profiler.pipeline_stats_supported;
        self.capture = ProfileCapture::default();
        self.capture_id = self.next_capture_id;
        self.next_capture_id += 1;
        self.prior_mode = profiler.mode;
        self.prior_sub_scopes = profiler.sub_scopes;
        let wanted = if self.include_stats {
            ProfilerMode::PipelineStats
        } else {
            ProfilerMode::Timestamps
        };
        if profiler.mode != wanted {
            profiler.set_mode(device, wanted);
        }
        profiler.sub_scopes = true; // capture the full nested tree; restored on stop
        self.state = CaptureState::Arming;
        self.capture_id
    }

    /// Advances the state machine once per finalized frame (at the read-back seam),
    /// appending the merged CPU+GPU spans for each `Recording` frame. The merged spans
    /// come from the current frame's CPU buffer + the profiler's last read-back.
    pub fn tick(&mut self, cpu: &CpuProfiler, cpu_slot: usize, profiler: &GpuProfiler) {
        match self.state {
            CaptureState::Arming => {
                self.warmup = self.warmup.saturating_sub(1);
                if self.warmup == 0 {
                    self.state = CaptureState::Recording;
                }
            }
            CaptureState::Recording => {
                self.append_frame(cpu, cpu_slot, profiler);
                self.captured_frames += 1;
                if self.captured_frames >= self.target_frames {
                    self.state = CaptureState::Ready;
                }
            }
            _ => {}
        }
    }

    /// Appends one frame's CPU spans (when enabled) then GPU passes, rebasing each
    /// span's `parent_index` into the growing capture's index space.
    pub(super) fn append_frame(
        &mut self,
        cpu: &CpuProfiler,
        cpu_slot: usize,
        profiler: &GpuProfiler,
    ) {
        let base = self.capture.spans.len();
        let mut cpu_count = 0usize;
        if self.include_cpu {
            for s in &cpu.buffers[cpu_slot].spans {
                self.capture.spans.push(ProfileSpan {
                    name: cpu.registry.name(s.marker).to_string(),
                    lane: ProfileLane::Cpu,
                    start_ns: s.start_ns,
                    end_ns: s.end_ns,
                    parent_index: if s.parent >= 0 {
                        base as i32 + s.parent
                    } else {
                        -1
                    },
                    depth: s.depth,
                    has_stats: false,
                    stats: PipelineStats::default(),
                });
                cpu_count += 1;
            }
        }
        let gpu_base = base + cpu_count;
        for t in &profiler.last_timings {
            self.capture.spans.push(ProfileSpan {
                name: t.name.clone(),
                lane: ProfileLane::Gpu,
                start_ns: t.start_ns,
                end_ns: t.end_ns,
                parent_index: if t.parent_index >= 0 {
                    gpu_base as i32 + t.parent_index
                } else {
                    -1
                },
                depth: t.depth,
                has_stats: t.has_stats,
                stats: t.stats,
            });
        }
    }

    /// Stops the capture, returning the accumulated [`ProfileCapture`] with its metadata
    /// filled, and restoring the profiler's prior mode/sub-scopes.
    ///
    /// Idempotent: a stop on an already-drained (`Idle`) recorder echoes the last completed
    /// capture rather than an empty one, so a duplicate stop — e.g. an overlapping control
    /// poll when frames are slow enough that round-trips exceed the poll interval — can never
    /// return an empty result in place of a good one.
    #[allow(clippy::too_many_arguments)]
    pub fn stop(
        &mut self,
        device: &Device,
        profiler: &mut GpuProfiler,
        software_gpu: bool,
        device_name: String,
        target_fps: f32,
    ) -> ProfileCapture {
        if self.state == CaptureState::Idle {
            // Already drained; the profiler was restored on the real stop. Echo the last
            // result without touching the profiler so a redundant stop is a no-op.
            return self.last_capture.clone();
        }
        // The metadata records the *capture's* facts, read before the profiler is restored.
        let correlated = profiler.calibration.correlated;
        let timestamp_period = profiler.timestamp_period;
        let mode = profiler.mode;
        profiler.sub_scopes = self.prior_sub_scopes;
        if self.prior_mode != profiler.mode {
            profiler.set_mode(device, self.prior_mode);
        }
        self.finish(ProfileCaptureMeta {
            software_gpu,
            correlated,
            device_name,
            timestamp_period,
            target_fps,
            mode,
            filter: self.filter.clone(),
            frame_count: 0,
        })
    }

    /// Drains the accumulated capture, stamping `meta` (its `frame_count` is filled here),
    /// resetting the recorder to `Idle`, and remembering a non-empty drain for a later
    /// redundant [`CaptureRecorder::stop`] to echo. Device-free, so it carries the testable
    /// idempotency logic: a call on an already-`Idle` recorder echoes the last completed
    /// capture rather than an empty one.
    pub(super) fn finish(&mut self, mut meta: ProfileCaptureMeta) -> ProfileCapture {
        if self.state == CaptureState::Idle {
            return self.last_capture.clone();
        }
        let mut out = std::mem::take(&mut self.capture);
        meta.frame_count = self.captured_frames;
        out.meta = meta;
        self.capture = ProfileCapture::default();
        let captured = self.captured_frames;
        self.captured_frames = 0;
        self.state = CaptureState::Idle;
        if captured > 0 {
            self.last_capture = out.clone();
        }
        out
    }
}

/// Raw `CLOCK_MONOTONIC` ns. This is the same axis the GPU
/// calibration projects device ticks onto (`VK_TIME_DOMAIN_CLOCK_MONOTONIC_EXT`), so a
/// correlated capture places CPU and GPU spans on one timeline.
pub fn cpu_now_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}
