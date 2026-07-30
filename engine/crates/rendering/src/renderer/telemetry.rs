use super::*;

/// The full per-frame render statistics: the draw-path counters plus the run-loop frame timings,
/// VRAM telemetry, and the current profiler / view-mode / exposure state.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderStatsFull {
    /// The draw-path counters from the last submitted frame.
    pub draw: RenderStats,
    /// Last completed frame's virtual-shadow residency activity.
    pub vsm: crate::VsmCounters,
    /// Wall-clock render-thread frame time (ms); `0` until the run loop records it.
    pub frame_ms: f32,
    /// Frames per second derived from `frame_ms` (`0` when `frame_ms` is `0`).
    pub fps: f32,
    /// GPU frame time (ms); `0` until the profiler runs.
    pub gpu_ms: f32,
    /// CPU busy time (ms).
    pub cpu_frame_ms: f32,
    /// CPU time spent deriving the frame's deformation work and ray instances (ms).
    pub scene_gather_ms: f32,
    /// Instances that gather DERIVED facts for this frame.
    ///
    /// Zero on a steady frame: everything per-instance is cached on the scene mirror and
    /// invalidated by the same journal that drives its table uploads, so this counts what the
    /// frame changed rather than what the scene holds. It grows only with deforming instances
    /// (their palettes and morph weights are rewritten every frame) and with the frames a
    /// mutation or a reach-window step forces a re-cut.
    pub scene_gather_entities: u32,
    /// Fence-wait time (ms).
    pub cpu_wait_ms: f32,
    /// Instances published into the active frame TLAS.
    pub rt_instances: u32,
    /// TLAS instances placed through the aggregate-representation structure.
    pub rt_aggregate_instances: u32,
    /// Device-local VRAM usage in bytes (`0` until profiled).
    pub vram_usage_bytes: u64,
    /// Device-local VRAM budget in bytes (`0` until profiled).
    pub vram_budget_bytes: u64,
    /// Whether the device is a software rasterizer.
    pub software_gpu: bool,
    /// The active GPU profiler mode.
    pub profiler_mode: ProfilerMode,
    /// The active debug render-output mode.
    pub view_mode: ViewMode,
    /// The tonemap exposure in stops.
    pub exposure_ev: f32,
    /// The scene-linear color grade folded into the tonemap pass (white balance, contrast,
    /// saturation, ASC-CDL).
    pub color_grade: ColorGrade,
    /// Whether the pre-tonemap bloom pyramid is enabled.
    pub bloom_enabled: bool,
    /// The energy-conserving bloom composite weight.
    pub bloom_intensity: f32,
    /// The bloom tent-upsample scatter radius (UV units).
    pub bloom_scatter: f32,
    /// The bloom tint (multiplies the composited bloom).
    pub bloom_tint: [f32; 3],
    /// The bloom soft-knee prefilter threshold (`0.0` = thresholdless).
    pub bloom_threshold: f32,
    /// The lens-dirt mask asset id (`0` = none).
    pub bloom_dirt_texture: u64,
    /// The lens-dirt mix fraction.
    pub bloom_dirt_intensity: f32,
    /// The lens-dirt tint.
    pub bloom_dirt_tint: [f32; 3],
    /// Whether the anamorphic streak runs.
    pub bloom_anamorphic_enabled: bool,
    /// The anamorphic horizontal squeeze.
    pub bloom_anamorphic_ratio: f32,
    /// The anamorphic streak tint.
    pub bloom_anamorphic_tint: [f32; 3],
    /// The anamorphic streak add weight.
    pub bloom_anamorphic_intensity: f32,
}

impl Renderer {
    /// The most recent frame's full draw + timing counters; `fps` derives from `frame_ms`.
    pub fn render_stats(&self) -> RenderStatsFull {
        let fps = if self.frame_ms > 0.0 {
            1000.0 / self.frame_ms
        } else {
            0.0
        };
        RenderStatsFull {
            draw: self.stats,
            vsm: self.vsm_residency.counters(),
            frame_ms: self.frame_ms,
            fps,
            gpu_ms: self.gpu_frame_ms,
            cpu_frame_ms: self.cpu_frame_ms,
            scene_gather_ms: self.scene_gather_ms,
            scene_gather_entities: self.scene_gather_entities,
            cpu_wait_ms: self.cpu_wait_ms,
            rt_instances: self.rt.frame_instance_count(),
            rt_aggregate_instances: self.rt.aggregate_instance_count(),
            vram_usage_bytes: self.vram_usage_bytes,
            vram_budget_bytes: self.vram_budget_bytes,
            software_gpu: self.software_gpu,
            profiler_mode: self.gpu_profiler.mode,
            view_mode: self.view_mode,
            exposure_ev: self.exposure_ev,
            color_grade: self.color_grade,
            bloom_enabled: self.bloom_enabled,
            bloom_intensity: self.bloom_intensity,
            bloom_scatter: self.bloom_scatter,
            bloom_tint: self.bloom_tint,
            bloom_threshold: self.bloom_threshold,
            bloom_dirt_texture: self.bloom_dirt_texture_id,
            bloom_dirt_intensity: self.bloom_dirt_intensity,
            bloom_dirt_tint: self.bloom_dirt_tint,
            bloom_anamorphic_enabled: self.bloom_anamorphic_enabled,
            bloom_anamorphic_ratio: self.bloom_anamorphic_ratio,
            bloom_anamorphic_tint: self.bloom_anamorphic_tint,
            bloom_anamorphic_intensity: self.bloom_anamorphic_intensity,
        }
    }

    /// Records the run loop's per-frame wall-clock timings for [`Renderer::render_stats`]
    /// and the frame-history percentiles.
    pub fn record_frame_timings(&mut self, frame_ms: f32, cpu_frame_ms: f32, cpu_wait_ms: f32) {
        self.frame_ms = frame_ms;
        self.cpu_frame_ms = cpu_frame_ms;
        self.cpu_wait_ms = cpu_wait_ms;
    }

    /// Records the CPU cost of the scene driver's per-frame derivation: its duration and how
    /// many instances it derived facts for.
    pub fn record_scene_gather(&mut self, elapsed: Duration, entities: u32) {
        self.scene_gather_ms = elapsed.as_secs_f32() * 1000.0;
        self.scene_gather_entities = entities;
    }

    /// Folds one frame's wall-clock delta (seconds) into the smoothed `frame_ms` headline the
    /// `render-stats` query reports: seed on the first frame, then a 0.9/0.1 EMA. A zero or
    /// non-finite delta is ignored.
    pub fn observe_frame_delta(&mut self, dt_seconds: f32) {
        if dt_seconds <= 0.0 || !dt_seconds.is_finite() {
            return;
        }
        let delta_ms = dt_seconds * 1000.0;
        self.frame_ms = if self.frame_ms == 0.0 {
            delta_ms
        } else {
            self.frame_ms * 0.9 + delta_ms * 0.1
        };
        // Accumulate a wrapping scene clock (seconds) for the fog-volume noise wind advection. The
        // 3600 s wrap keeps the float precise while the drift stays continuous across the seam.
        self.fog_time = (self.fog_time + dt_seconds) % 3600.0;
    }

    /// Folds one frame's CPU split (busy + fence-wait, ms) into the smoothed `cpu_frame_ms` /
    /// `cpu_wait_ms`: seed on the first frame, then a 0.9/0.1 EMA each. The busy span is the run
    /// loop's update + render window minus the GPU wait, so it is render-thread CPU work, not
    /// wall clock. A non-finite value is ignored.
    pub fn observe_cpu_frame(&mut self, busy_ms: f32, wait_ms: f32) {
        if busy_ms.is_finite() && busy_ms >= 0.0 {
            self.cpu_frame_ms = if self.cpu_frame_ms == 0.0 {
                busy_ms
            } else {
                self.cpu_frame_ms * 0.9 + busy_ms * 0.1
            };
        }
        if wait_ms.is_finite() && wait_ms >= 0.0 {
            self.cpu_wait_ms = if self.cpu_wait_ms == 0.0 {
                wait_ms
            } else {
                self.cpu_wait_ms * 0.9 + wait_ms * 0.1
            };
        }
    }

    /// Drops the frame-timing distribution + smoothed headlines and holds telemetry off for a
    /// short warm-up. Called when a project load completes: the prior frames and the cold-pipeline
    /// frames right after the swap are not steady state, so grading over them paints the HUD red.
    pub fn reset_frame_telemetry(&mut self) {
        self.frame_history.reset();
        self.frame_ms = 0.0;
        self.cpu_frame_ms = 0.0;
        self.cpu_wait_ms = 0.0;
        self.telemetry_warmup = TELEMETRY_WARMUP_FRAMES;
    }

    /// The per-frame telemetry tail the run loop calls once after each rendered frame: folds the
    /// CPU busy/wait split into the smoothed headline, pushes the raw frame into the history ring,
    /// runs the perf-alarm detectors, and advances the profiler-capture state machine.
    ///
    /// `busy_ms` is the loop's update+render span minus the GPU fence-wait, `wait_ms` is that wait,
    /// and `dt_sec` is the wall-clock delta since the prior frame (it drives the alarm EMA's
    /// irregular-interval alpha).
    pub fn finalize_frame_telemetry(&mut self, busy_ms: f32, wait_ms: f32, dt_sec: f32) {
        let now_ns = cpu_now_ns();
        self.last_frame_ns = now_ns;
        // Warm-up frames after a project load (PSO compiles, acceleration-structure builds) are
        // not representative, so they stay out of the headline, the history, and the detectors.
        if self.telemetry_warmup > 0 {
            self.telemetry_warmup -= 1;
            return;
        }
        self.observe_cpu_frame(busy_ms, wait_ms);

        // The raw frame goes into the history ring un-smoothed (the distribution stays honest only
        // if it sees every frame), then the alarm detectors run on it — after the push, so the
        // MAD / burn-rate windows include this frame.
        let frame_time_ms = busy_ms + wait_ms;
        self.frame_history.record(
            busy_ms,
            self.gpu_profiler.last_gpu_total_ms,
            wait_ms,
            self.perf_config.budget_ms(),
            now_ns,
        );
        let inputs = AlarmInputs {
            frame_time_ms,
            dt_sec,
            now_ns,
            vram_usage_bytes: self.vram_usage_bytes,
            vram_budget_bytes: self.vram_budget_bytes,
            pipelines_created: self.stats.pipelines_created,
            // Word 2 is the overflow set and word 4 the pressure set, both from the fence-gated
            // readback the rest of the counters ride.
            visibility_overflow_flags: self.visibility_counters[2],
            visibility_pressure_flags: self.visibility_counters[4],
            owned_budgets: self.owned_budgets.clone(),
            focused: self.reactive.power_state == PowerState::Focused,
        };
        self.alarms
            .tick(&self.frame_history, &self.perf_config, &inputs);

        // Auto-quality steps the render-quality tier (then, below the tier floor, the render scale)
        // to hold the frame budget. The frame work time (busy + GPU-fence wait, before the loop's
        // pacing sleep) is the signal. Off by default.
        if self.perf_config.auto_quality {
            let active_scale = self.views[self.active_view.index()].render_scale;
            match self.budget_controller.update(
                frame_time_ms,
                self.perf_config.budget_ms(),
                self.render_quality.tier,
                active_scale,
            ) {
                Some(BudgetStep::Tier(tier)) => self.set_render_quality(tier.resolve()),
                // A resolution change reallocates the render targets; defer it to the next frame's
                // safe resize point — the just-submitted frame still references the current targets.
                Some(BudgetStep::Scale(scale)) => self.pending_render_scale = Some(scale),
                None => {}
            }
        }
    }

    /// The current GPU profiler mode.
    pub fn profiler_mode(&self) -> ProfilerMode {
        self.gpu_profiler.mode
    }

    /// Selects the GPU profiler mode, allocating the query pools on first non-`Off`
    /// request and clamping to what the device supports.
    pub fn set_profiler_mode(&mut self, mode: ProfilerMode) {
        self.gpu_profiler.set_mode(&self.device, mode);
    }

    /// Whether the device's graphics queue supports timestamp queries.
    pub fn profiler_timestamps_supported(&self) -> bool {
        self.gpu_profiler.timestamps_supported
    }

    /// Whether the device supports pipeline-statistics queries.
    pub fn profiler_pipeline_stats_supported(&self) -> bool {
        self.gpu_profiler.pipeline_stats_supported
    }

    /// The last frame's per-pass GPU timings; empty unless the profiler ran in a timestamps mode.
    pub fn pass_timings(&self) -> &[PassTiming] {
        &self.gpu_profiler.last_timings
    }

    /// The last frame's total GPU span across all passes (ms).
    pub fn pass_timings_total_ms(&self) -> f32 {
        self.gpu_profiler.last_gpu_total_ms
    }

    /// Arms a profiler capture, returning its id.
    pub fn start_profile_capture(
        &mut self,
        mode: CaptureMode,
        frames: u32,
        filter: String,
        include_cpu: bool,
        include_stats: bool,
    ) -> u32 {
        self.capture.start(
            &self.device,
            &mut self.gpu_profiler,
            mode,
            frames,
            filter,
            include_cpu,
            include_stats,
        )
    }

    /// Finishes the armed capture and returns the accumulated spans + metadata.
    pub fn stop_profile_capture(&mut self) -> ProfileCapture {
        let software_gpu = self.software_gpu;
        let device_name = self.device_name.clone();
        let target_fps = self.perf_config.target_fps;
        self.capture.stop(
            &self.device,
            &mut self.gpu_profiler,
            software_gpu,
            device_name,
            target_fps,
        )
    }

    /// Advances the capture recorder once per finalized frame, appending the merged CPU+GPU spans
    /// while `Recording`. The run loop calls this each frame after the profiler read-back.
    pub fn tick_profile_capture(&mut self, cpu_slot: usize) {
        self.capture
            .tick(&self.cpu_profiler, cpu_slot, &self.gpu_profiler);
    }

    /// The CPU span profiler, recorded into by the run loop's per-pass CPU markers.
    pub fn cpu_profiler_mut(&mut self) -> &mut CpuProfiler {
        &mut self.cpu_profiler
    }

    /// The capture's mode.
    pub fn profile_capture_mode(&self) -> CaptureMode {
        self.capture.mode
    }

    /// The capture state machine's current state.
    pub fn profile_capture_state(&self) -> CaptureState {
        self.capture.state
    }

    /// Frames copied into the in-flight capture so far.
    pub fn profile_capture_captured_frames(&self) -> u32 {
        self.capture.captured_frames
    }

    /// The in-flight capture's target frame count.
    pub fn profile_capture_target_frames(&self) -> u32 {
        self.capture.target_frames
    }

    /// The rolling frame-time percentile / stutter summary.
    pub fn frame_history_stats(&self) -> FrameHistoryStats {
        self.frame_history.stats()
    }

    /// The most recent `max_samples` frame samples, oldest→newest.
    pub fn frame_samples(&self, max_samples: u32) -> Vec<FrameSample> {
        self.frame_history.samples(max_samples)
    }

    /// The shared frame-budget / threshold config.
    pub fn perf_config(&self) -> PerfConfig {
        self.perf_config
    }

    /// Replaces the perf config, clamping it into sane ranges.
    pub fn set_perf_config(&mut self, config: PerfConfig) {
        self.perf_config = config.clamped();
    }

    /// Drains perf-alarm events with `seq > since`.
    pub fn drain_alarms(&self, since: u64) -> AlarmDrain {
        self.alarms.drain(since)
    }

    /// The currently-firing perf alarms.
    pub fn active_alarms(&self) -> &[ActiveAlarm] {
        self.alarms.active()
    }

    /// Records an already-measured CPU span into this frame's profiler buffer.
    ///
    /// The seam for work outside this crate that already knows when it started and how long it
    /// took — a capture would otherwise show a gap where that work ran. A no-op when profiling off.
    pub fn record_cpu_span(&mut self, name: &str, start_ns: u64, duration_ns: u64) {
        if self.gpu_profiler.mode == ProfilerMode::Off {
            return;
        }
        // Queued rather than written straight through: this is called from outside the frame,
        // where the slot index the span buffers are keyed by is not in scope.
        self.pending_cpu_spans
            .push((name.to_owned(), start_ns, duration_ns));
    }

    /// Resets this slot's GPU timestamp (and pipeline-stats) query pool on the recording command
    /// buffer, so the graph's per-pass scopes write into a clean pool. A no-op when the profiler
    /// is `Off` or its pools are not allocated.
    pub(super) fn reset_profiler_pools(&self, cmd: vk::CommandBuffer, slot: usize) {
        if self.gpu_profiler.mode == ProfilerMode::Off || !self.gpu_profiler.pools_ready {
            return;
        }
        let raw = self.device.raw();
        if let Some(pool) = self.gpu_profiler.timestamp_pool(slot) {
            // SAFETY: the ash seam. `cmd` is recording; the slot's prior GPU work completed at
            // the begin-frame fence wait, so the pool is free to reset. Two queries per scope.
            unsafe {
                raw.cmd_reset_query_pool(cmd, pool, 0, 2 * crate::profiler::MAX_PROFILED_SCOPES);
            }
        }
        if self.gpu_profiler.mode == ProfilerMode::PipelineStats
            && let Some(pool) = self.gpu_profiler.stats_pool(slot)
        {
            // SAFETY: the ash seam. As above; one stats query per top-level graphics pass.
            unsafe {
                raw.cmd_reset_query_pool(cmd, pool, 0, crate::profiler::MAX_PROFILED_SCOPES);
            }
        }
    }
}
