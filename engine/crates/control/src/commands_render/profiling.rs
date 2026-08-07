use std::io::Write;

use saffron_protocol::{
    ActiveAlarmDto, ActiveAlarmsDto, AlarmEventDto, AlarmSeverityDto, AlarmStateDto,
    CaptureModeDto, CaptureStartParams, CaptureStartResult, CaptureStateDto, CaptureStatusResult,
    CaptureStopResult, DrainAlarmsParams, DrainAlarmsResult, EmptyParams, FrameHistoryDto,
    FrameHistoryParams, FrameSampleDto, PerfConfigDto, PipelineStatsDto, ProfileCaptureDto,
    ProfileCaptureMetadataDto, ProfileLaneDto, ProfileSpanDto, ProfilerModeDto, ProfilerModeResult,
    ProfilerSetModeParams, RenderPassTimingsDto, SetPerfConfigParams,
};
use saffron_rendering::{
    ActiveAlarm, AlarmDrain, AlarmEvent, AlarmEventKind, AlarmSeverity, CaptureMode, CaptureState,
    PerfConfig, ProfileCapture, ProfileLane, ProfilerMode,
};

use super::*;
use crate::registry::{CommandRegistry, ControlRenderer};

pub(crate) fn profiler_mode_to_dto(mode: ProfilerMode) -> ProfilerModeDto {
    match mode {
        ProfilerMode::Off => ProfilerModeDto::Off,
        ProfilerMode::Timestamps => ProfilerModeDto::Timestamps,
        ProfilerMode::PipelineStats => ProfilerModeDto::PipelineStats,
    }
}

pub(crate) fn profiler_mode_from_dto(mode: ProfilerModeDto) -> ProfilerMode {
    match mode {
        ProfilerModeDto::Off => ProfilerMode::Off,
        ProfilerModeDto::Timestamps => ProfilerMode::Timestamps,
        ProfilerModeDto::PipelineStats => ProfilerMode::PipelineStats,
    }
}

pub(crate) fn capture_mode_to_dto(mode: CaptureMode) -> CaptureModeDto {
    match mode {
        CaptureMode::Single => CaptureModeDto::Single,
        CaptureMode::Frames => CaptureModeDto::Frames,
        CaptureMode::Rolling => CaptureModeDto::Rolling,
    }
}

pub(crate) fn capture_mode_from_dto(mode: CaptureModeDto) -> CaptureMode {
    match mode {
        CaptureModeDto::Single => CaptureMode::Single,
        CaptureModeDto::Frames => CaptureMode::Frames,
        CaptureModeDto::Rolling => CaptureMode::Rolling,
    }
}

pub(crate) fn capture_state_to_dto(state: CaptureState) -> CaptureStateDto {
    match state {
        CaptureState::Idle => CaptureStateDto::Idle,
        CaptureState::Arming => CaptureStateDto::Arming,
        CaptureState::Recording => CaptureStateDto::Recording,
        CaptureState::Ready => CaptureStateDto::Ready,
    }
}

pub(crate) fn profile_lane_to_dto(lane: ProfileLane) -> ProfileLaneDto {
    match lane {
        ProfileLane::Cpu => ProfileLaneDto::Cpu,
        ProfileLane::Gpu => ProfileLaneDto::Gpu,
    }
}

pub(crate) fn alarm_severity_to_dto(severity: AlarmSeverity) -> AlarmSeverityDto {
    match severity {
        AlarmSeverity::Info => AlarmSeverityDto::Info,
        AlarmSeverity::Warning => AlarmSeverityDto::Warning,
        AlarmSeverity::Critical => AlarmSeverityDto::Critical,
    }
}

pub(crate) fn perf_config_dto(config: PerfConfig) -> PerfConfigDto {
    PerfConfigDto {
        target_fps: config.target_fps,
        budget_ms: config.budget_ms(),
        green_budget_frac: config.green_budget_frac,
        green_median_mul: config.green_median_mul,
        amber_median_mul: config.amber_median_mul,
        frozen_ms: config.frozen_ms,
        vram_warn_frac: config.vram_warn_frac,
        vram_crit_frac: config.vram_crit_frac,
        auto_quality: config.auto_quality,
    }
}

pub(crate) fn frame_history_dto(renderer: &dyn ControlRenderer, samples: i32) -> FrameHistoryDto {
    let stats = renderer.frame_history_stats();
    let mut out = FrameHistoryDto {
        p50_ms: stats.p50_ms,
        p95_ms: stats.p95_ms,
        p99_ms: stats.p99_ms,
        p999_ms: stats.p999_ms,
        max_ms: stats.max_ms,
        mean_ms: stats.mean_ms,
        stddev_ms: stats.stddev_ms,
        stutter_count: stats.stutter_count as i64,
        sample_count: stats.sample_count as i32,
        budget_ms: renderer.perf_config().budget_ms(),
        samples: Vec::new(),
    };
    if samples > 0 {
        out.samples = renderer
            .frame_samples(samples as u32)
            .iter()
            .map(|sample| FrameSampleDto {
                frame_index: sample.frame_index as i64,
                cpu_ms: sample.cpu_ms,
                gpu_ms: sample.gpu_ms,
                cpu_wait_ms: sample.cpu_wait_ms,
            })
            .collect();
    }
    out
}

pub(crate) fn alarm_event_dto(event: &AlarmEvent) -> AlarmEventDto {
    let state = match event.kind {
        AlarmEventKind::Firing => AlarmStateDto::Firing,
        AlarmEventKind::Resolved => AlarmStateDto::Resolved,
    };
    AlarmEventDto {
        seq: event.seq as i64,
        fingerprint: event.fingerprint.to_string(),
        metric: event.metric.clone(),
        pass: event.pass.clone(),
        owner: event.owner.clone(),
        severity: alarm_severity_to_dto(event.severity),
        state,
        value: event.value,
        threshold: event.threshold,
        since_frame: event.since_frame as i64,
        count: event.count as i32,
        duration_ms: event.duration_ms,
    }
}

pub(crate) fn drain_alarms_dto(renderer: &dyn ControlRenderer, since: i64) -> DrainAlarmsResult {
    let since_seq = if since >= 0 { since as u64 } else { 0 };
    let drain: AlarmDrain = renderer.drain_alarms(since_seq);
    DrainAlarmsResult {
        events: drain.events.iter().map(alarm_event_dto).collect(),
        high_water_seq: drain.high_water_seq as i64,
        oldest_seq: drain.oldest_seq as i64,
        overflowed: drain.overflowed,
    }
}

pub(crate) fn active_alarms_dto(renderer: &dyn ControlRenderer) -> ActiveAlarmsDto {
    let alarms: Vec<ActiveAlarmDto> = renderer
        .active_alarms()
        .iter()
        .map(|alarm: &ActiveAlarm| ActiveAlarmDto {
            fingerprint: alarm.fingerprint.to_string(),
            metric: alarm.metric.clone(),
            pass: alarm.pass.clone(),
            owner: alarm.owner.clone(),
            severity: alarm_severity_to_dto(alarm.severity),
            value: alarm.value,
            threshold: alarm.threshold,
            since_frame: alarm.since_frame as i64,
            count: alarm.count as i32,
        })
        .collect();
    ActiveAlarmsDto { alarms }
}

pub(crate) fn profile_capture_dto(capture: &ProfileCapture) -> ProfileCaptureDto {
    let spans = capture
        .spans
        .iter()
        .map(|s| {
            let pipeline_stats = if s.has_stats {
                Some(PipelineStatsDto {
                    input_vertices: s.stats.input_vertices,
                    vertex_invocations: s.stats.vertex_invocations,
                    clipping_invocations: s.stats.clipping_invocations,
                    clipping_primitives: s.stats.clipping_primitives,
                    fragment_invocations: s.stats.fragment_invocations,
                    compute_invocations: s.stats.compute_invocations,
                    pixels: s.stats.pixels,
                })
            } else {
                None
            };
            ProfileSpanDto {
                name: s.name.clone(),
                lane: profile_lane_to_dto(s.lane),
                start_ns: s.start_ns,
                end_ns: s.end_ns,
                parent_index: s.parent_index,
                depth: s.depth,
                pipeline_stats,
            }
        })
        .collect();
    ProfileCaptureDto {
        spans,
        metadata: ProfileCaptureMetadataDto {
            software_gpu: capture.meta.software_gpu,
            correlated: capture.meta.correlated,
            device_name: capture.meta.device_name.clone(),
            timestamp_period: capture.meta.timestamp_period,
            target_fps: capture.meta.target_fps,
            mode: profiler_mode_to_dto(capture.meta.mode),
            filter: capture.meta.filter.clone(),
            frame_count: capture.meta.frame_count,
        },
    }
}

/// Serializes a capture to Chrome Trace Event JSON: `M` (metadata) events name the two
/// lanes, `X` (complete) events carry each span's microsecond ts/dur; the honesty flags
/// + device facts ride in `otherData`.
pub(crate) fn to_chrome_trace(capture: &ProfileCapture) -> String {
    use serde_json::{Value, json};

    let cpu_tid = 1;
    let gpu_tid = 2;
    let mut events: Vec<Value> = vec![
        json!({ "ph": "M", "pid": "SaffronAnima", "name": "process_name",
                "args": { "name": "SaffronAnima" } }),
        json!({ "ph": "M", "pid": "SaffronAnima", "tid": cpu_tid, "name": "thread_name",
                "args": { "name": "CPU render thread" } }),
        json!({ "ph": "M", "pid": "SaffronAnima", "tid": gpu_tid, "name": "thread_name",
                "args": { "name": "GPU queue" } }),
    ];
    for s in &capture.spans {
        let ts_us = s.start_ns as f64 / 1000.0;
        let dur_us = if s.end_ns > s.start_ns {
            (s.end_ns - s.start_ns) as f64 / 1000.0
        } else {
            0.0
        };
        let mut args = json!({ "depth": s.depth });
        if s.has_stats {
            args["fragmentInvocations"] = json!(s.stats.fragment_invocations);
            args["vertexInvocations"] = json!(s.stats.vertex_invocations);
            args["inputVertices"] = json!(s.stats.input_vertices);
            args["clippingInvocations"] = json!(s.stats.clipping_invocations);
            args["clippingPrimitives"] = json!(s.stats.clipping_primitives);
            args["computeInvocations"] = json!(s.stats.compute_invocations);
            args["pixels"] = json!(s.stats.pixels);
        }
        let lane_tid = if s.lane == ProfileLane::Gpu {
            gpu_tid
        } else {
            cpu_tid
        };
        events.push(json!({
            "ph": "X",
            "pid": "SaffronAnima",
            "tid": lane_tid,
            "name": s.name,
            "ts": ts_us,
            "dur": dur_us,
            "args": args,
        }));
    }
    let mode_name = if profiler_mode_to_dto(capture.meta.mode) == ProfilerModeDto::Timestamps {
        "timestamps"
    } else {
        "pipeline-stats"
    };
    let doc = json!({
        "traceEvents": events,
        "displayTimeUnit": "ns",
        "otherData": {
            "softwareGpu": capture.meta.software_gpu,
            "correlated": capture.meta.correlated,
            "deviceName": capture.meta.device_name,
            "mode": mode_name,
            "targetFps": capture.meta.target_fps,
            "frameCount": capture.meta.frame_count,
            "filter": capture.meta.filter,
        },
    });
    doc.to_string()
}

/// Registers the profiler, capture, frame-history, and perf-config commands.
pub(crate) fn register_profiling(reg: &mut CommandRegistry) {
    reg.register::<ProfilerSetModeParams, ProfilerModeResult>(
        "profiler.set-mode",
        "profiler.set-mode {off|timestamps|pipeline-stats} — per-pass GPU timing + counters",
        |ctx, params| {
            let mode = profiler_mode_from_dto(params.mode.unwrap_or(ProfilerModeDto::Off));
            ctx.renderer.set_profiler_mode(mode);
            Ok(ProfilerModeResult {
                mode: profiler_mode_to_dto(ctx.renderer.profiler_mode()),
                timestamps_supported: ctx.renderer.profiler_timestamps_supported(),
                pipeline_stats_supported: ctx.renderer.profiler_pipeline_stats_supported(),
                software_gpu: ctx.renderer.software_gpu(),
            })
        },
    );

    reg.register::<EmptyParams, RenderPassTimingsDto>(
        "pass-timings",
        "last frame's per-pass GPU timings (needs profiler timestamps mode)",
        |ctx, _params| Ok(pass_timings_dto(ctx.renderer)),
    );

    reg.register::<CaptureStartParams, CaptureStartResult>(
        "profiler.capture-start",
        "profiler.capture-start {mode,frames,filter,includeCpu,includePipelineStats} — arm a capture",
        |ctx, params| {
            let mode = capture_mode_from_dto(params.mode.unwrap_or(CaptureModeDto::Single));
            let frames = params.frames.unwrap_or(60).max(1) as u32;
            let id = ctx.renderer.start_profile_capture(
                mode,
                frames,
                params.filter.unwrap_or_default(),
                params.include_cpu.unwrap_or(true),
                params.include_pipeline_stats.unwrap_or(false),
            );
            Ok(CaptureStartResult {
                capture_id: id,
                ack: true,
            })
        },
    );

    reg.register::<EmptyParams, CaptureStopResult>(
        "profiler.capture-stop",
        "profiler.capture-stop — finish + return the armed capture (inline single, file for frames:N)",
        |ctx, _params| {
            let mode = ctx.renderer.profile_capture_mode();
            let capture = ctx.renderer.stop_profile_capture();
            let ready = capture.meta.frame_count > 0;
            // The structured spans always come back inline so the editor can render any
            // capture. The Chrome-Trace string rides inline for a small single-frame
            // capture; a multi-frame one is written to a file (path returned) to keep the
            // wire payload bounded.
            let inlined = mode == CaptureMode::Single || !ready;
            let mut chrome_trace = String::new();
            let mut path = String::new();
            if inlined {
                if ready {
                    chrome_trace = to_chrome_trace(&capture);
                }
            } else {
                let file = std::env::temp_dir()
                    .join(format!("saffron-profile-{}.json", std::process::id()));
                if let Ok(mut stream) = std::fs::File::create(&file) {
                    let _ = stream.write_all(to_chrome_trace(&capture).as_bytes());
                }
                path = file.to_string_lossy().into_owned();
            }
            Ok(CaptureStopResult {
                ready,
                mode: capture_mode_to_dto(mode),
                frame_count: capture.meta.frame_count,
                inlined,
                capture: profile_capture_dto(&capture),
                chrome_trace,
                path,
                pending: false,
            })
        },
    );

    reg.register::<EmptyParams, CaptureStatusResult>(
        "profiler.capture-status",
        "profiler.capture-status — non-destructive capture progress (poll until ready, then stop)",
        |ctx, _params| {
            Ok(CaptureStatusResult {
                state: capture_state_to_dto(ctx.renderer.profile_capture_state()),
                captured_frames: ctx.renderer.profile_capture_captured_frames(),
                target_frames: ctx.renderer.profile_capture_target_frames(),
                mode: capture_mode_to_dto(ctx.renderer.profile_capture_mode()),
                pipeline_stats_supported: ctx.renderer.profiler_pipeline_stats_supported(),
            })
        },
    );

    reg.register::<FrameHistoryParams, FrameHistoryDto>(
        "frame-history",
        "frame-time percentiles + stutter count (+ optional recent samples)",
        |ctx, params| Ok(frame_history_dto(ctx.renderer, params.samples.unwrap_or(0))),
    );

    reg.register::<EmptyParams, PerfConfigDto>(
        "get-perf-config",
        "the shared frame-budget / green-amber-red threshold config",
        |ctx, _params| Ok(perf_config_dto(ctx.renderer.perf_config())),
    );

    reg.register::<SetPerfConfigParams, PerfConfigDto>(
        "set-perf-config",
        "set-perf-config {greenBudgetFrac,...} — green/amber/red alarm thresholds",
        |ctx, params| {
            let mut config = ctx.renderer.perf_config();
            if let Some(v) = params.green_budget_frac {
                config.green_budget_frac = v;
            }
            if let Some(v) = params.green_median_mul {
                config.green_median_mul = v;
            }
            if let Some(v) = params.amber_median_mul {
                config.amber_median_mul = v;
            }
            if let Some(v) = params.frozen_ms {
                config.frozen_ms = v;
            }
            if let Some(v) = params.vram_warn_frac {
                config.vram_warn_frac = v;
            }
            if let Some(v) = params.vram_crit_frac {
                config.vram_crit_frac = v;
            }
            ctx.renderer.set_perf_config(config);
            Ok(perf_config_dto(ctx.renderer.perf_config()))
        },
    );
}

/// Registers the performance-alarm drain and the active-alarm listing.
pub(crate) fn register_alarms(reg: &mut CommandRegistry) {
    reg.register::<DrainAlarmsParams, DrainAlarmsResult>(
        "drain-alarms",
        "drain-alarms {since} — perf-alarm events with seq > since (non-blocking)",
        |ctx, params| Ok(drain_alarms_dto(ctx.renderer, params.since.unwrap_or(0))),
    );

    reg.register::<EmptyParams, ActiveAlarmsDto>(
        "list-active-alarms",
        "currently firing perf alarms (the badge + row highlights)",
        |ctx, _params| Ok(active_alarms_dto(ctx.renderer)),
    );
}
