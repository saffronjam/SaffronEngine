//! The 35 render-domain control commands: render-stats, the profiler/capture group,
//! perf config, frame history, alarms, the AA / view-mode / clustering / IBL / sky-occlusion /
//! SSAO / shadow / GI / skinning / displacement / depth-prepass toggles, native viewport info +
//! size, exposure, and reflection-probe management.
//!
//! Every handler reaches only [`EngineContext::renderer`] (the `recapture-probes` /
//! `list-probes` pair additionally reads the active scene's reflection-probe state),
//! making this the cleanest domain: a thin DTO ↔ renderer-query/setter bridge through
//! the [`ControlRenderer`] trait. The DTOs come from `saffron-protocol`; the kebab-case
//! enum mapping and the decimal-string id emit are inherited from the typed-register
//! wrapper, so the handlers translate values only.

use std::io::Write;

use saffron_protocol::{
    AaModeDto, ActiveAlarmDto, ActiveAlarmsDto, AlarmEventDto, AlarmSeverityDto, AlarmStateDto,
    AnamorphicParams, CaptureModeDto, CaptureStartParams, CaptureStartResult, CaptureStateDto,
    CaptureStatusResult, CaptureStopResult, DrainAlarmsParams, DrainAlarmsResult, EmptyParams,
    FrameHistoryDto, FrameHistoryParams, FrameSampleDto, GetTaaParamsResult, GetUpscaleResult,
    GiModeDto, ListProbesResult, PerfConfigDto, PipelineStatsDto, ProbeRef, ProfileCaptureDto,
    ProfileCaptureMetadataDto, ProfileLaneDto, ProfileSpanDto, ProfilerModeDto, ProfilerModeResult,
    ProfilerSetModeParams, RecaptureProbesResult, RenderPassTimingDto, RenderPassTimingsDto,
    RenderQualityResult, RenderStatsDto, SetAaParams, SetAaResult, SetBloomParams, SetBloomResult,
    SetClusteredResult, SetColorGradingParams, SetColorGradingResult, SetDepthPrepassResult,
    SetDisplacementResult, SetExposureParams, SetExposureResult, SetGdfResult, SetGiParams,
    SetGiResult, SetIblResult, SetPerfConfigParams, SetProbesParams, SetProbesResult,
    SetRenderQualityParams, SetRestirResult, SetRtReflectionsResult, SetRtShadowsResult,
    SetShadowsResult, SetSkinningResult, SetSkyOcclusionResult, SetSsrResult, SetTaaParamsParams,
    SetTaaParamsResult, SetTessellationQualityParams, SetTessellationQualityResult,
    SetTonemapParams, SetUpscaleParams, SetUpscaleResult, SetViewModeParams, SetViewModeResult,
    SetViewportPowerStateParams, SetViewportSizeParams, SetViewportSizeResult, ToggleParams,
    TonemapResult, UpscaleDto, Uuid, Vec3, ViewModeDto, ViewportNativeInfoResult,
    ViewportPowerStateResult,
};
use saffron_rendering::{
    ActiveAlarm, AlarmDrain, AlarmEvent, AlarmEventKind, AlarmSeverity, CaptureMode, CaptureState,
    PassTiming, PerfConfig, ProfileCapture, ProfileLane, ProfilerMode, RenderStatsFull, ViewId,
    ViewMode,
};
use saffron_scene::ReflectionProbe;

use crate::error::Error;
use crate::registry::{CommandRegistry, ControlRenderer, EngineContext};
use crate::server::control_socket_path;

/// Converts a glam world vector into the wire `Vec3`.
/// Builds the `RenderQualityResult` reply from the renderer's current tier + resolved per-effect
/// state (shared by `set-render-quality` and `get-render-quality`).
fn render_quality_result(ctx: &EngineContext<'_>) -> RenderQualityResult {
    RenderQualityResult {
        tier: ctx.renderer.render_quality_tier(),
        ssgi: ctx.renderer.ssgi_enabled(),
        gtao: ctx.renderer.ssao_enabled(),
        contact_shadows: ctx.renderer.contact_shadows_enabled(),
    }
}

fn to_vec3(v: saffron_geometry::glam::Vec3) -> Vec3 {
    Vec3 {
        x: v.x,
        y: v.y,
        z: v.z,
    }
}

/// Maps the wire AA mode to a `(samples, fxaa, taa)` selection.
fn aa_selection(mode: AaModeDto) -> (u32, bool, bool) {
    match mode {
        AaModeDto::Off => (1, false, false),
        AaModeDto::Fxaa => (1, true, false),
        AaModeDto::Taa => (1, false, true),
        AaModeDto::Msaa2 => (2, false, false),
        AaModeDto::Msaa4 => (4, false, false),
        AaModeDto::Msaa8 => (8, false, false),
    }
}

/// The AA mode read back from the renderer's mode name.
fn aa_mode_from_name(name: &str) -> AaModeDto {
    match name {
        "fxaa" => AaModeDto::Fxaa,
        "taa" => AaModeDto::Taa,
        "msaa2" => AaModeDto::Msaa2,
        "msaa4" => AaModeDto::Msaa4,
        "msaa8" => AaModeDto::Msaa8,
        _ => AaModeDto::Off,
    }
}

fn view_mode_to_dto(mode: ViewMode) -> ViewModeDto {
    match mode {
        ViewMode::Lit => ViewModeDto::Lit,
        ViewMode::Unlit => ViewModeDto::Unlit,
        ViewMode::Wireframe => ViewModeDto::Wireframe,
        ViewMode::LitWireframe => ViewModeDto::LitWireframe,
        ViewMode::DetailLighting => ViewModeDto::DetailLighting,
        ViewMode::LightingOnly => ViewModeDto::LightingOnly,
        ViewMode::Reflections => ViewModeDto::Reflections,
        ViewMode::Albedo => ViewModeDto::Albedo,
        ViewMode::Normal => ViewModeDto::Normal,
        ViewMode::Roughness => ViewModeDto::Roughness,
        ViewMode::Metallic => ViewModeDto::Metallic,
        ViewMode::Emissive => ViewModeDto::Emissive,
        ViewMode::Depth => ViewModeDto::Depth,
        ViewMode::AmbientOcclusion => ViewModeDto::AmbientOcclusion,
        ViewMode::Gi => ViewModeDto::Gi,
        ViewMode::LightComplexity => ViewModeDto::LightComplexity,
        ViewMode::MotionVectors => ViewModeDto::MotionVectors,
        ViewMode::Fog => ViewModeDto::Fog,
        ViewMode::CloudDensity => ViewModeDto::CloudDensity,
    }
}

fn view_mode_from_dto(mode: ViewModeDto) -> ViewMode {
    match mode {
        ViewModeDto::Lit => ViewMode::Lit,
        ViewModeDto::Unlit => ViewMode::Unlit,
        ViewModeDto::Wireframe => ViewMode::Wireframe,
        ViewModeDto::LitWireframe => ViewMode::LitWireframe,
        ViewModeDto::DetailLighting => ViewMode::DetailLighting,
        ViewModeDto::LightingOnly => ViewMode::LightingOnly,
        ViewModeDto::Reflections => ViewMode::Reflections,
        ViewModeDto::Albedo => ViewMode::Albedo,
        ViewModeDto::Normal => ViewMode::Normal,
        ViewModeDto::Roughness => ViewMode::Roughness,
        ViewModeDto::Metallic => ViewMode::Metallic,
        ViewModeDto::Emissive => ViewMode::Emissive,
        ViewModeDto::Depth => ViewMode::Depth,
        ViewModeDto::AmbientOcclusion => ViewMode::AmbientOcclusion,
        ViewModeDto::Gi => ViewMode::Gi,
        ViewModeDto::LightComplexity => ViewMode::LightComplexity,
        ViewModeDto::MotionVectors => ViewMode::MotionVectors,
        ViewModeDto::Fog => ViewMode::Fog,
        ViewModeDto::CloudDensity => ViewMode::CloudDensity,
    }
}

fn profiler_mode_to_dto(mode: ProfilerMode) -> ProfilerModeDto {
    match mode {
        ProfilerMode::Off => ProfilerModeDto::Off,
        ProfilerMode::Timestamps => ProfilerModeDto::Timestamps,
        ProfilerMode::PipelineStats => ProfilerModeDto::PipelineStats,
    }
}

fn profiler_mode_from_dto(mode: ProfilerModeDto) -> ProfilerMode {
    match mode {
        ProfilerModeDto::Off => ProfilerMode::Off,
        ProfilerModeDto::Timestamps => ProfilerMode::Timestamps,
        ProfilerModeDto::PipelineStats => ProfilerMode::PipelineStats,
    }
}

fn capture_mode_to_dto(mode: CaptureMode) -> CaptureModeDto {
    match mode {
        CaptureMode::Single => CaptureModeDto::Single,
        CaptureMode::Frames => CaptureModeDto::Frames,
        CaptureMode::Rolling => CaptureModeDto::Rolling,
    }
}

fn capture_mode_from_dto(mode: CaptureModeDto) -> CaptureMode {
    match mode {
        CaptureModeDto::Single => CaptureMode::Single,
        CaptureModeDto::Frames => CaptureMode::Frames,
        CaptureModeDto::Rolling => CaptureMode::Rolling,
    }
}

fn capture_state_to_dto(state: CaptureState) -> CaptureStateDto {
    match state {
        CaptureState::Idle => CaptureStateDto::Idle,
        CaptureState::Arming => CaptureStateDto::Arming,
        CaptureState::Recording => CaptureStateDto::Recording,
        CaptureState::Ready => CaptureStateDto::Ready,
    }
}

fn profile_lane_to_dto(lane: ProfileLane) -> ProfileLaneDto {
    match lane {
        ProfileLane::Cpu => ProfileLaneDto::Cpu,
        ProfileLane::Gpu => ProfileLaneDto::Gpu,
    }
}

fn alarm_severity_to_dto(severity: AlarmSeverity) -> AlarmSeverityDto {
    match severity {
        AlarmSeverity::Info => AlarmSeverityDto::Info,
        AlarmSeverity::Warning => AlarmSeverityDto::Warning,
        AlarmSeverity::Critical => AlarmSeverityDto::Critical,
    }
}

/// Builds the `render-stats` DTO from the renderer's full snapshot plus its individual
/// toggle queries.
/// Echoes the applied grade back as the flat `set-color-grading` result (the same shape the panel
/// reads from `render-stats`).
fn result_from_grading(p: SetColorGradingParams) -> SetColorGradingResult {
    SetColorGradingResult {
        temperature: p.temperature,
        tint: p.tint,
        contrast: p.contrast,
        pivot: p.pivot,
        saturation: p.saturation,
        slope: p.slope,
        offset: p.offset,
        power: p.power,
        shadows: p.shadows,
        midtones: p.midtones,
        highlights: p.highlights,
        shadows_max: p.shadows_max,
        highlights_min: p.highlights_min,
        channel_mixer: p.channel_mixer,
        split_tone: p.split_tone,
        creative_lut_asset: p.creative_lut_asset,
        creative_lut_intensity: p.creative_lut_intensity,
    }
}

fn render_stats_dto(renderer: &dyn ControlRenderer) -> RenderStatsDto {
    let stats: RenderStatsFull = renderer.render_stats();
    RenderStatsDto {
        draw_calls: stats.draw.draw_calls as i32,
        batches: stats.draw.batches as i32,
        instances: stats.draw.instances as i32,
        frame_ms: stats.frame_ms,
        fps: stats.fps,
        gpu_ms: stats.gpu_ms,
        cpu_frame_ms: stats.cpu_frame_ms,
        gpu_frame_ms: stats.gpu_ms,
        cpu_wait_ms: stats.cpu_wait_ms,
        triangles: stats.draw.triangles as i32,
        descriptor_binds: stats.draw.descriptor_binds as i32,
        command_buffers: stats.draw.command_buffers as i32,
        queue_submits: stats.draw.queue_submits as i32,
        pipelines_created: stats.draw.pipelines_created as i32,
        vram_usage_bytes: stats.vram_usage_bytes,
        vram_budget_bytes: stats.vram_budget_bytes,
        software_gpu: stats.software_gpu,
        profiler_mode: profiler_mode_to_dto(stats.profiler_mode),
        clustered: renderer.clustered_enabled(),
        depth_prepass: renderer.depth_prepass_enabled(),
        shadows: renderer.shadows_enabled(),
        ibl: renderer.ibl_enabled(),
        ssao: renderer.ssao_enabled(),
        contact_shadows: renderer.contact_shadows_enabled(),
        ssgi: renderer.ssgi_enabled(),
        render_scale: renderer.render_scale(),
        sky_occlusion: renderer.sky_occlusion_enabled(),
        gdf: renderer.gdf_enabled(),
        quality: renderer.render_quality_tier(),
        tonemap: renderer.tonemap_mode(),
        idle: renderer.reactive_idle(),
        converged: renderer.reactive_converged(),
        redraw_reasons: renderer.redraw_reasons(),
        power_state: renderer.power_state(),
        ddgi: renderer.ddgi_enabled(),
        rt_supported: renderer.rt_supported(),
        rt_shadows: renderer.rt_shadows_enabled(),
        restir: renderer.restir_enabled(),
        ssr: renderer.ssr_enabled(),
        rt_reflections: renderer.rt_reflections_enabled(),
        blas_count: renderer.rt_blas_count() as i32,
        pipelines: renderer.pipeline_count() as i32,
        bindless_textures: renderer.bindless_texture_count() as i32,
        bindless_free: renderer.bindless_free_count() as i32,
        hdr: true,
        exposure_ev: stats.exposure_ev,
        color_grading: renderer.color_grading(),
        creative_lut: renderer.creative_lut().map(|(asset, size, intensity)| {
            saffron_protocol::CreativeLutStat {
                asset: Uuid(asset),
                intensity,
                size,
            }
        }),
        bloom_enabled: renderer.bloom_enabled(),
        bloom_intensity: renderer.bloom_intensity(),
        bloom_scatter: renderer.bloom_scatter(),
        bloom_tint: renderer.bloom_tint(),
        bloom_threshold: renderer.bloom_threshold(),
        bloom_dirt_texture: Uuid(renderer.bloom_dirt_texture()),
        bloom_dirt_intensity: renderer.bloom_dirt_intensity(),
        bloom_dirt_tint: renderer.bloom_dirt_tint(),
        bloom_anamorphic: AnamorphicParams {
            enabled: renderer.bloom_anamorphic_enabled(),
            ratio: renderer.bloom_anamorphic_ratio(),
            tint: renderer.bloom_anamorphic_tint(),
            intensity: renderer.bloom_anamorphic_intensity(),
        },
        bloom_per_mip_tint: renderer.bloom_mip_tint(),
        aa: aa_mode_from_name(&renderer.aa_mode()),
        view_mode: view_mode_to_dto(stats.view_mode),
    }
}

fn pass_timings_dto(renderer: &dyn ControlRenderer) -> RenderPassTimingsDto {
    let passes: Vec<RenderPassTimingDto> = renderer
        .pass_timings()
        .iter()
        .map(|t: &PassTiming| RenderPassTimingDto {
            name: t.name.clone(),
            gpu_ms: t.gpu_ms,
        })
        .collect();
    RenderPassTimingsDto {
        passes,
        gpu_total_ms: renderer.pass_timings_total_ms(),
        software_gpu: renderer.software_gpu(),
        profiler_mode: profiler_mode_to_dto(renderer.profiler_mode()),
    }
}

fn perf_config_dto(config: PerfConfig) -> PerfConfigDto {
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

/// The live TAAU upscale surface off the renderer: the fixed ratio, the dynamic-resolution toggle +
/// budget (from the perf config), and the current input/display extents. A partial `set-upscale`
/// merges onto this — reading it back through the same helper keeps the two in one shape.
fn upscale_dto(renderer: &dyn ControlRenderer) -> UpscaleDto {
    let config = renderer.perf_config();
    let (input_width, input_height) = renderer.input_extent();
    let (display_width, display_height) = renderer.display_extent();
    UpscaleDto {
        ratio: renderer.render_scale(),
        dynamic: config.auto_quality,
        target_ms: config.budget_ms(),
        input_width,
        input_height,
        display_width,
        display_height,
    }
}

fn frame_history_dto(renderer: &dyn ControlRenderer, samples: i32) -> FrameHistoryDto {
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

fn alarm_event_dto(event: &AlarmEvent) -> AlarmEventDto {
    let state = match event.kind {
        AlarmEventKind::Firing => AlarmStateDto::Firing,
        AlarmEventKind::Resolved => AlarmStateDto::Resolved,
    };
    AlarmEventDto {
        seq: event.seq as i64,
        fingerprint: event.fingerprint.to_string(),
        metric: event.metric.clone(),
        pass: event.pass.clone(),
        severity: alarm_severity_to_dto(event.severity),
        state,
        value: event.value,
        threshold: event.threshold,
        since_frame: event.since_frame as i64,
        count: event.count as i32,
        duration_ms: event.duration_ms,
    }
}

fn drain_alarms_dto(renderer: &dyn ControlRenderer, since: i64) -> DrainAlarmsResult {
    let since_seq = if since >= 0 { since as u64 } else { 0 };
    let drain: AlarmDrain = renderer.drain_alarms(since_seq);
    DrainAlarmsResult {
        events: drain.events.iter().map(alarm_event_dto).collect(),
        high_water_seq: drain.high_water_seq as i64,
        oldest_seq: drain.oldest_seq as i64,
        overflowed: drain.overflowed,
    }
}

fn active_alarms_dto(renderer: &dyn ControlRenderer) -> ActiveAlarmsDto {
    let alarms: Vec<ActiveAlarmDto> = renderer
        .active_alarms()
        .iter()
        .map(|alarm: &ActiveAlarm| ActiveAlarmDto {
            fingerprint: alarm.fingerprint.to_string(),
            metric: alarm.metric.clone(),
            pass: alarm.pass.clone(),
            severity: alarm_severity_to_dto(alarm.severity),
            value: alarm.value,
            threshold: alarm.threshold,
            since_frame: alarm.since_frame as i64,
            count: alarm.count as i32,
        })
        .collect();
    ActiveAlarmsDto { alarms }
}

fn profile_capture_dto(capture: &ProfileCapture) -> ProfileCaptureDto {
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
fn to_chrome_trace(capture: &ProfileCapture) -> String {
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

/// Registers the 34 render-domain commands, in registration order, onto `reg`.
pub fn register_render_commands(reg: &mut CommandRegistry) {
    reg.register::<EmptyParams, RenderStatsDto>(
        "render-stats",
        "last frame's scene draw counters",
        |ctx, _params| Ok(render_stats_dto(ctx.renderer)),
    );

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

    reg.register::<EmptyParams, GetUpscaleResult>(
        "get-upscale",
        "get-upscale — the TAAU ratio, dynamic-resolution state, and input/display extents",
        |ctx, _params| {
            Ok(GetUpscaleResult {
                upscale: upscale_dto(ctx.renderer),
            })
        },
    );

    reg.register::<SetUpscaleParams, SetUpscaleResult>(
        "set-upscale",
        "set-upscale {ratio,dynamic,targetMs} — the TAAU input:display scale + dynamic resolution",
        |ctx, params| {
            if let Some(ratio) = params.ratio {
                ctx.renderer.set_render_scale(ratio);
            }
            let mut config = ctx.renderer.perf_config();
            if let Some(dynamic) = params.dynamic {
                config.auto_quality = dynamic;
            }
            if let Some(ms) = params.target_ms {
                // targetMs <= 0 means uncapped (target_fps = 0), matching the budget controller's
                // budget_ms <= 0 hold.
                config.target_fps = if ms > 0.0 { 1000.0 / ms } else { 0.0 };
            }
            ctx.renderer.set_perf_config(config);
            Ok(SetUpscaleResult {
                upscale: upscale_dto(ctx.renderer),
            })
        },
    );

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

    reg.register::<SetAaParams, SetAaResult>(
        "set-aa",
        "set-aa {off|fxaa|taa|msaa2|msaa4|msaa8} — anti-aliasing mode",
        |ctx, params| {
            let (samples, fxaa, taa) = aa_selection(params.mode.unwrap_or(AaModeDto::Off));
            ctx.renderer
                .set_aa(samples, fxaa, taa)
                .map_err(Error::Command)?;
            Ok(SetAaResult {
                aa: aa_mode_from_name(&ctx.renderer.aa_mode()),
            })
        },
    );

    reg.register::<EmptyParams, GetTaaParamsResult>(
        "get-taa-params",
        "get-taa-params — current TAA blend/sharpen parameters",
        |ctx, _params| {
            Ok(GetTaaParamsResult {
                params: ctx.renderer.taa_params(),
            })
        },
    );

    reg.register::<SetTaaParamsParams, SetTaaParamsResult>(
        "set-taa-params",
        "set-taa-params {feedbackMin,feedbackMax,velocityRejection,clipGamma,sharpness} — tune TAA (partial update)",
        |ctx, p| {
            // Partial update: read current, overlay only the provided fields, write back.
            let mut cur = ctx.renderer.taa_params();
            if let Some(v) = p.feedback_min {
                cur.feedback_min = v;
            }
            if let Some(v) = p.feedback_max {
                cur.feedback_max = v;
            }
            if let Some(v) = p.velocity_rejection {
                cur.velocity_rejection = v;
            }
            if let Some(v) = p.clip_gamma {
                cur.clip_gamma = v;
            }
            if let Some(v) = p.sharpness {
                cur.sharpness = v;
            }
            ctx.renderer.set_taa_params(cur.clone());
            Ok(SetTaaParamsResult { params: cur })
        },
    );

    reg.register::<SetViewModeParams, SetViewModeResult>(
        "set-view-mode",
        "set-view-mode {lit|unlit|wireframe|lit-wireframe|detail-lighting|lighting-only|reflections|albedo|normal|roughness|metallic|emissive|depth|ambient-occlusion|gi|light-complexity|motion-vectors} — debug render-output (transient)",
        |ctx, params| {
            ctx.renderer
                .set_view_mode(view_mode_from_dto(params.mode.unwrap_or(ViewModeDto::Lit)));
            Ok(SetViewModeResult {
                view_mode: view_mode_to_dto(ctx.renderer.view_mode()),
            })
        },
    );

    reg.register::<ToggleParams, SetClusteredResult>(
        "set-clustered",
        "set-clustered {0|1} — toggle clustered light culling",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_clustered(enabled);
            Ok(SetClusteredResult { clustered: enabled })
        },
    );

    reg.register::<ToggleParams, SetIblResult>(
        "set-ibl",
        "set-ibl {0|1} — toggle image-based ambient (vs flat ambient)",
        |ctx, params| {
            ctx.renderer.set_ibl(params.enabled.unwrap_or(true));
            Ok(SetIblResult {
                ibl: ctx.renderer.ibl_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetSkyOcclusionResult>(
        "set-sky-occlusion",
        "set-sky-occlusion {0|1} — occlude the reflected skybox with the Global SDF reflection-occlusion cone",
        |ctx, params| {
            ctx.renderer
                .set_sky_occlusion(params.enabled.unwrap_or(true));
            Ok(SetSkyOcclusionResult {
                sky_occlusion: ctx.renderer.sky_occlusion_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetGdfResult>(
        "set-gdf",
        "set-gdf {0|1} — composite per-mesh SDFs into the Global Distance Field cascade clipmap \
         (the far-field cone-march tap, O(1) in instance count)",
        |ctx, params| {
            ctx.renderer.set_gdf(params.enabled.unwrap_or(true));
            Ok(SetGdfResult {
                gdf: ctx.renderer.gdf_enabled(),
            })
        },
    );

    reg.register::<SetRenderQualityParams, RenderQualityResult>(
        "set-render-quality",
        "set-render-quality {low|medium|high|ultra} — the SSGI/GTAO/contact-shadow quality knob",
        |ctx, params| {
            if !ctx.renderer.set_render_quality(&params.tier) {
                return Err(Error::command(format!(
                    "unknown render-quality tier '{}' (expected low|medium|high|ultra)",
                    params.tier
                )));
            }
            Ok(render_quality_result(ctx))
        },
    );

    reg.register::<EmptyParams, RenderQualityResult>(
        "get-render-quality",
        "get-render-quality — the active render-quality tier + resolved per-effect state",
        |ctx, _params| Ok(render_quality_result(ctx)),
    );

    reg.register::<SetTonemapParams, TonemapResult>(
        "set-tonemap",
        "set-tonemap {reinhard|aces|agx|pbr-neutral} — the HDR→display tonemap operator",
        |ctx, params| {
            if !ctx.renderer.set_tonemap(&params.mode) {
                return Err(Error::command(format!(
                    "unknown tonemap '{}' (expected reinhard|aces|agx|pbr-neutral)",
                    params.mode
                )));
            }
            Ok(TonemapResult {
                mode: ctx.renderer.tonemap_mode(),
            })
        },
    );

    reg.register::<ToggleParams, SetRtShadowsResult>(
        "set-rt-shadows",
        "set-rt-shadows {0|1} — hardware ray-query shadows (if supported)",
        |ctx, params| {
            if !ctx.renderer.rt_supported() {
                return Err(Error::command("ray tracing not supported on this device"));
            }
            ctx.renderer.set_rt_shadows(params.enabled.unwrap_or(true));
            Ok(SetRtShadowsResult {
                rt_shadows: ctx.renderer.rt_shadows_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetRestirResult>(
        "set-restir",
        "set-restir {0|1} — ReSTIR stochastic many-light direct (if RT supported)",
        |ctx, params| {
            if !ctx.renderer.rt_supported() {
                return Err(Error::command("ray tracing not supported on this device"));
            }
            ctx.renderer.set_restir(params.enabled.unwrap_or(true));
            Ok(SetRestirResult {
                restir: ctx.renderer.restir_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetSsrResult>(
        "set-ssr",
        "set-ssr {0|1} — screen-space reflections (sharp mirror reflections on smooth surfaces)",
        |ctx, params| {
            ctx.renderer.set_ssr(params.enabled.unwrap_or(true));
            Ok(SetSsrResult {
                ssr: ctx.renderer.ssr_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetRtReflectionsResult>(
        "set-rt-reflections",
        "set-rt-reflections {0|1} — ray-traced reflections (off-screen-aware, if RT supported)",
        |ctx, params| {
            if !ctx.renderer.rt_supported() {
                return Err(Error::command("ray tracing not supported on this device"));
            }
            ctx.renderer
                .set_rt_reflections(params.enabled.unwrap_or(true));
            Ok(SetRtReflectionsResult {
                rt_reflections: ctx.renderer.rt_reflections_enabled(),
            })
        },
    );

    reg.register::<SetGiParams, SetGiResult>(
        "set-gi",
        "set-gi {off|ddgi} — DDGI probe global illumination (multi-bounce)",
        |ctx, params| {
            ctx.renderer.set_ddgi(params.mode == GiModeDto::Ddgi);
            Ok(SetGiResult {
                ddgi: ctx.renderer.ddgi_enabled(),
            })
        },
    );

    reg.register::<ToggleParams, SetShadowsResult>(
        "set-shadows",
        "set-shadows {0|1} — toggle the directional shadow map",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_shadows(enabled);
            Ok(SetShadowsResult { shadows: enabled })
        },
    );

    reg.register::<ToggleParams, SetSkinningResult>(
        "set-skinning",
        "set-skinning {0|1} — toggle the GPU skinning path",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_skinning(enabled);
            Ok(SetSkinningResult { skinning: enabled })
        },
    );

    reg.register::<ToggleParams, SetDisplacementResult>(
        "set-displacement",
        "set-displacement {0|1} — toggle the GPU compute-displacement path",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_displacement(enabled);
            Ok(SetDisplacementResult {
                displacement: enabled,
            })
        },
    );

    reg.register::<SetExposureParams, SetExposureResult>(
        "set-exposure",
        "set-exposure {ev} — tonemap exposure in stops (exp2)",
        |ctx, params| {
            ctx.renderer.set_exposure(params.ev);
            Ok(SetExposureResult {
                exposure_ev: ctx.renderer.exposure_ev(),
            })
        },
    );

    reg.register::<SetBloomParams, SetBloomResult>(
        "set-bloom",
        "set-bloom {enabled} {intensity} {scatter} {tint} {threshold} [dirtTexture] [dirtIntensity] \
         [dirtTint] [anamorphic] [perMipTint] — pre-tonemap bloom pyramid + art direction",
        |ctx, p| {
            if !(p.intensity >= 0.0 && (0.0..=1.0).contains(&p.scatter) && p.threshold >= 0.0) {
                return Err(Error::command("bloom parameters out of range"));
            }
            ctx.renderer
                .set_bloom(p.enabled, p.intensity, p.scatter, p.tint, p.threshold);
            // Lens dirt: a supplied `dirtTexture` resolves (id 0 clears) and rebinds the mask; the
            // mix + tint are a separate patch so a caller can tune them without re-sending the asset.
            if let Some(id) = p.dirt_texture {
                let mut resolved = None;
                if id.value() != 0 {
                    let assets = &mut *ctx.assets;
                    let core_id = saffron_core::Uuid(id.value());
                    ctx.renderer.with_gpu_uploader(&mut |gpu| {
                        resolved = assets.load_texture_asset(gpu, core_id);
                    });
                }
                ctx.renderer.set_bloom_dirt_texture(id.value(), resolved);
            }
            if p.dirt_intensity.is_some() || p.dirt_tint.is_some() {
                let intensity = p
                    .dirt_intensity
                    .unwrap_or_else(|| ctx.renderer.bloom_dirt_intensity());
                let tint = p.dirt_tint.unwrap_or_else(|| ctx.renderer.bloom_dirt_tint());
                ctx.renderer.set_bloom_dirt_params(intensity, tint);
            }
            if let Some(a) = &p.anamorphic {
                ctx.renderer
                    .set_bloom_anamorphic(a.enabled, a.ratio, a.tint, a.intensity);
            }
            if let Some(stack) = p.per_mip_tint {
                ctx.renderer.set_bloom_mip_tint(stack);
            }
            Ok(SetBloomResult {
                enabled: ctx.renderer.bloom_enabled(),
                intensity: ctx.renderer.bloom_intensity(),
                scatter: ctx.renderer.bloom_scatter(),
                tint: ctx.renderer.bloom_tint(),
                threshold: ctx.renderer.bloom_threshold(),
                dirt_texture: Uuid(ctx.renderer.bloom_dirt_texture()),
                dirt_intensity: ctx.renderer.bloom_dirt_intensity(),
                dirt_tint: ctx.renderer.bloom_dirt_tint(),
                anamorphic: AnamorphicParams {
                    enabled: ctx.renderer.bloom_anamorphic_enabled(),
                    ratio: ctx.renderer.bloom_anamorphic_ratio(),
                    tint: ctx.renderer.bloom_anamorphic_tint(),
                    intensity: ctx.renderer.bloom_anamorphic_intensity(),
                },
                per_mip_tint: ctx.renderer.bloom_mip_tint(),
            })
        },
    );

    reg.register::<SetColorGradingParams, SetColorGradingResult>(
        "set-color-grading",
        "set-color-grading {temperature} {tint} {contrast} {pivot} {saturation} {slope} {offset} \
         {power} — the scene-linear grade (white balance, contrast, saturation, ASC-CDL)",
        |ctx, params| {
            let finite3 = |v: [f32; 3]| v.iter().all(|c| c.is_finite());
            let range_valid = |r: &saffron_protocol::GradeRangeDto| {
                finite3(r.slope)
                    && finite3(r.offset)
                    && finite3(r.power)
                    && r.power.iter().all(|c| *c > 0.0)
                    && r.saturation.is_finite()
                    && r.saturation >= 0.0
                    && r.contrast.is_finite()
                    && r.contrast >= 0.0
            };
            let valid = params.temperature.is_finite()
                && params.tint.is_finite()
                && params.contrast.is_finite()
                && params.contrast >= 0.0
                && params.saturation.is_finite()
                && params.saturation >= 0.0
                && params.pivot.is_finite()
                && params.pivot > 0.0
                && finite3(params.slope)
                && finite3(params.offset)
                && finite3(params.power)
                && params.power.iter().all(|c| *c > 0.0)
                && range_valid(&params.shadows)
                && range_valid(&params.midtones)
                && range_valid(&params.highlights)
                && params.shadows_max.is_finite()
                && params.highlights_min.is_finite()
                && params.channel_mixer.iter().all(|c| c.is_finite())
                && finite3(params.split_tone.shadow)
                && finite3(params.split_tone.highlight)
                && params.split_tone.balance.is_finite();
            if !valid {
                return Err(Error::command(
                    "color grade out of range (pivot/power > 0, contrast/saturation ≥ 0, all finite)",
                ));
            }
            if !((0.0..=1.0).contains(&params.creative_lut_intensity)) {
                return Err(Error::command("creativeLutIntensity out of range (expected 0..=1)"));
            }
            let lut_id = params.creative_lut_asset.value();
            let lut_intensity = params.creative_lut_intensity;
            ctx.renderer.set_color_grading(params);
            // The creative-LUT slot rides the same command: resolve the asset (id 0 clears to the
            // identity default) and rebind, mirroring the lens-dirt mask. The intensity rides the grade
            // UBO, so an intensity-only change re-resolves the (cached) asset without a descriptor rewrite.
            let mut resolved = None;
            if lut_id != 0 {
                let assets = &mut *ctx.assets;
                let core_id = saffron_core::Uuid(lut_id);
                ctx.renderer.with_gpu_uploader(&mut |gpu| {
                    resolved = assets.load_cube_lut_asset(gpu, core_id);
                });
                if resolved.is_none() {
                    return Err(Error::command(format!(
                        "creative lut asset {lut_id} could not be resolved"
                    )));
                }
            }
            ctx.renderer
                .set_creative_lut_texture(lut_id, resolved, lut_intensity);
            Ok(result_from_grading(ctx.renderer.color_grading()))
        },
    );

    reg.register::<saffron_protocol::BakeLookParams, saffron_protocol::BakeLookResult>(
        "bake-look",
        "bake-look [name] — fold grade + view transform + creative LUT into a 33³ log2-shaper .slut",
        |ctx, params| {
            let name = params.name.unwrap_or_else(|| "Baked Look".to_owned());
            let (size, rgb) = ctx.renderer.bake_look_lut().map_err(Error::command)?;
            let (asset, path) = ctx
                .assets
                .import_baked_lut(&name, size, rgb)
                .map_err(|e| Error::command(e.to_string()))?;
            Ok(saffron_protocol::BakeLookResult {
                asset: Uuid(asset.value()),
                path,
                size,
            })
        },
    );

    reg.register::<SetTessellationQualityParams, SetTessellationQualityResult>(
        "set-tessellation-quality",
        "set-tessellation-quality [factorCap] [minFactor] [edgeLengthTarget] — displacement dice budget",
        |ctx, params| {
            ctx.renderer.set_tessellation_quality(
                params.factor_cap,
                params.min_factor,
                params.edge_length_target,
            );
            let (factor_cap, min_factor, edge_length_target) = ctx.renderer.tessellation_quality();
            Ok(SetTessellationQualityResult {
                factor_cap,
                min_factor,
                edge_length_target,
            })
        },
    );

    reg.register::<ToggleParams, SetDepthPrepassResult>(
        "set-depth-prepass",
        "set-depth-prepass {0|1} — toggle the depth pre-pass",
        |ctx, params| {
            let enabled = params.enabled.unwrap_or(true);
            ctx.renderer.set_depth_prepass(enabled);
            Ok(SetDepthPrepassResult {
                depth_prepass: enabled,
            })
        },
    );

    reg.register::<EmptyParams, ViewportNativeInfoResult>(
        "viewport-native-info",
        "native viewport bridge status",
        |ctx, _params| {
            Ok(ViewportNativeInfoResult {
                platform: "linux".to_owned(),
                transport: "wayland-subsurface".to_owned(),
                status: "engine-ready".to_owned(),
                control_socket: control_socket_path(),
                width: ctx.renderer.viewport_width() as i32,
                height: ctx.renderer.viewport_height() as i32,
                message: "engine renders offscreen; the editor presents frames from shared \
                          memory on a wayland subsurface"
                    .to_owned(),
            })
        },
    );

    reg.register::<SetViewportPowerStateParams, ViewportPowerStateResult>(
        "set-viewport-power-state",
        "set-viewport-power-state {state} — editor viewport visibility (focused/unfocused/occluded) \
         for idle throttling; occluded suppresses rendering",
        |ctx, params| {
            if !ctx.renderer.set_viewport_power_state(&params.state) {
                return Err(Error::command(format!(
                    "unknown power state '{}' (expected focused|unfocused|occluded)",
                    params.state
                )));
            }
            Ok(ViewportPowerStateResult {
                state: ctx.renderer.power_state(),
            })
        },
    );

    reg.register::<SetViewportSizeParams, SetViewportSizeResult>(
        "set-viewport-size",
        "set-viewport-size {view, width, height} — set a view's offscreen render size (device pixels)",
        |ctx, params| {
            let wire = params.view.unwrap_or_else(|| "scene".to_owned());
            let view = ViewId::from_wire(&wire)
                .ok_or_else(|| Error::command(format!("unknown view '{wire}'")))?;
            let width = params
                .width
                .unwrap_or(ctx.renderer.viewport_width() as i32)
                .max(1);
            let height = params
                .height
                .unwrap_or(ctx.renderer.viewport_height() as i32)
                .max(1);
            ctx.renderer
                .set_view_desired_size(view, width as u32, height as u32)
                .map_err(Error::Command)?;
            Ok(SetViewportSizeResult { width, height })
        },
    );

    reg.register::<SetProbesParams, SetProbesResult>(
        "set-probes",
        "set-probes {0|1} — toggle reflection-probe specular sampling",
        |ctx, params| {
            ctx.renderer
                .set_reflection_probes(params.enabled.unwrap_or(true));
            Ok(SetProbesResult {
                probes: ctx.renderer.reflection_probes_enabled(),
            })
        },
    );

    reg.register::<EmptyParams, RecaptureProbesResult>(
        "recapture-probes",
        "recapture-probes — mark every reflection probe dirty (forces re-capture)",
        |ctx, _params| {
            let mut marked = 0u32;
            ctx.scene_edit
                .active_scene()
                .for_each::<(&mut ReflectionProbe,), _>(|_, (probe,)| {
                    probe.dirty = true;
                    marked += 1;
                });
            Ok(RecaptureProbesResult { marked })
        },
    );

    reg.register::<EmptyParams, ListProbesResult>(
        "list-probes",
        "list-probes — captured reflection probes (origin/radius/intensity/valid)",
        |ctx, _params| {
            let enabled = ctx.renderer.reflection_probes_enabled();
            let probes: Vec<ProbeRef> = ctx
                .renderer
                .reflection_probes()
                .iter()
                .enumerate()
                .map(|(slot, probe)| ProbeRef {
                    slot: slot as u32,
                    entity: Uuid::from(probe.entity),
                    origin: to_vec3(probe.origin),
                    influence_radius: probe.influence_radius,
                    intensity: probe.intensity,
                    box_projection: probe.box_projection,
                    valid: probe.valid,
                    dirty: probe.dirty,
                })
                .collect();
            Ok(ListProbesResult {
                enabled,
                count: probes.len() as u32,
                probes,
            })
        },
    );
}

#[cfg(test)]
mod tests {
    use saffron_rendering::{
        ActiveAlarm, AlarmDrain, AlarmEvent, AlarmEventKind, AlarmSeverity, FrameHistoryStats,
        FrameSample, PerfConfig,
    };
    use serde_json::{Value, json};

    use crate::registry::{CommandRegistry, register_builtin_commands};
    use crate::test_support::{StubRenderer, with_stub};

    /// A registry with the builtins + render commands registered.
    fn registry() -> CommandRegistry {
        let mut reg = CommandRegistry::new();
        register_builtin_commands(&mut reg);
        reg
    }

    /// Dispatches `cmd` with `params` against a fresh stub and returns the reply.
    fn run(stub: &mut StubRenderer, cmd: &str, params: Value) -> Value {
        let reg = registry();
        with_stub(stub, |ctx| {
            reg.dispatch(ctx, &json!({ "id": 1, "cmd": cmd, "params": params }))
        })
    }

    #[test]
    fn set_aa_msaa4_returns_applied_samples() {
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "set-aa", json!({ "mode": "msaa4" }));
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["aa"], json!("msaa4"));
        assert_eq!(stub.aa_samples, 4);
        assert!(!stub.aa_fxaa && !stub.aa_taa);
    }

    #[test]
    fn set_aa_maps_fxaa_taa_and_off() {
        let mut stub = StubRenderer::default();
        assert_eq!(
            run(&mut stub, "set-aa", json!({ "mode": "fxaa" }))["result"]["aa"],
            json!("fxaa")
        );
        assert!(stub.aa_fxaa);

        let mut stub = StubRenderer::default();
        assert_eq!(
            run(&mut stub, "set-aa", json!({ "mode": "taa" }))["result"]["aa"],
            json!("taa")
        );
        assert!(stub.aa_taa);

        let mut stub = StubRenderer::default();
        assert_eq!(
            run(&mut stub, "set-aa", json!({ "mode": "off" }))["result"]["aa"],
            json!("off")
        );
        assert_eq!(stub.aa_samples, 1);
    }

    #[test]
    fn set_aa_unknown_mode_is_a_typed_error() {
        // An unknown kebab value fails the enum deserialize → envelope error, not a
        // silent default.
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "set-aa", json!({ "mode": "msaa16" }));
        assert_eq!(reply["ok"], json!(false));
        assert!(reply.get("error").is_some());
    }

    #[test]
    fn toggles_echo_the_applied_boolean() {
        // Each `Toggle*` command echoes the bool through its distinct result field.
        let cases: &[(&str, &str)] = &[
            ("set-clustered", "clustered"),
            ("set-ibl", "ibl"),
            ("set-shadows", "shadows"),
            ("set-skinning", "skinning"),
            ("set-displacement", "displacement"),
            ("set-depth-prepass", "depthPrepass"),
            ("set-probes", "probes"),
        ];
        for (cmd, field) in cases {
            let mut stub = StubRenderer::default();
            let on = run(&mut stub, cmd, json!({ "enabled": true }));
            assert_eq!(on["ok"], json!(true), "{cmd}");
            assert_eq!(on["result"][field], json!(true), "{cmd} on");

            let mut stub = StubRenderer::default();
            let off = run(&mut stub, cmd, json!({ "enabled": false }));
            assert_eq!(off["result"][field], json!(false), "{cmd} off");
        }
    }

    #[test]
    fn render_quality_tier_applies_echoes_and_rejects_unknown() {
        // `set-render-quality` applies a tier and echoes the resolved per-effect state;
        // `get-render-quality` reads it back. The SSGI / GTAO / contact flags follow the tier.
        let mut stub = StubRenderer::default();
        let low = run(&mut stub, "set-render-quality", json!({ "tier": "low" }));
        assert_eq!(low["ok"], json!(true));
        assert_eq!(low["result"]["tier"], json!("low"));
        assert_eq!(low["result"]["ssgi"], json!(false), "low disables SSGI");
        assert_eq!(low["result"]["gtao"], json!(false), "low disables GTAO");

        let high = run(&mut stub, "set-render-quality", json!({ "tier": "high" }));
        assert_eq!(high["result"]["ssgi"], json!(true), "high enables SSGI");
        assert_eq!(
            high["result"]["contactShadows"],
            json!(true),
            "high enables contact shadows"
        );

        // The read-back command reports the tier just applied.
        let got = run(&mut stub, "get-render-quality", json!({}));
        assert_eq!(got["result"]["tier"], json!("high"));

        // An unknown tier name is a typed error, not a silent default.
        let bad = run(
            &mut stub,
            "set-render-quality",
            json!({ "tier": "cinematic" }),
        );
        assert_eq!(bad["ok"], json!(false));
        assert!(bad.get("error").is_some());
    }

    #[test]
    fn rt_toggles_require_rt_support() {
        // On a software device the RT-gated toggles return a typed error.
        let mut stub = StubRenderer::default();
        let shadows = run(&mut stub, "set-rt-shadows", json!({ "enabled": true }));
        assert_eq!(shadows["ok"], json!(false));
        assert_eq!(
            shadows["error"],
            json!("ray tracing not supported on this device")
        );

        // With RT support the toggle applies and echoes back.
        let mut stub = StubRenderer {
            rt_supported: true,
            ..StubRenderer::default()
        };
        let shadows = run(&mut stub, "set-rt-shadows", json!({ "enabled": true }));
        assert_eq!(shadows["ok"], json!(true));
        assert_eq!(shadows["result"]["rtShadows"], json!(true));
    }

    #[test]
    fn set_gi_maps_off_and_ddgi() {
        let mut stub = StubRenderer::default();
        let off = run(&mut stub, "set-gi", json!({ "mode": "off" }));
        assert_eq!(off["result"]["ddgi"], json!(false));
        assert!(!stub.ddgi);

        let mut stub = StubRenderer::default();
        let ddgi = run(&mut stub, "set-gi", json!({ "mode": "ddgi" }));
        assert_eq!(ddgi["result"]["ddgi"], json!(true));
        assert!(stub.ddgi);
    }

    #[test]
    fn set_view_mode_round_trips_wireframe() {
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "set-view-mode", json!({ "mode": "wireframe" }));
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["viewMode"], json!("wireframe"));
    }

    #[test]
    fn set_view_mode_round_trips_every_channel() {
        for mode in [
            "lit",
            "wireframe",
            "albedo",
            "normal",
            "roughness",
            "metallic",
            "emissive",
        ] {
            let mut stub = StubRenderer::default();
            let reply = run(&mut stub, "set-view-mode", json!({ "mode": mode }));
            assert_eq!(reply["result"]["viewMode"], json!(mode), "{mode}");
        }
    }

    #[test]
    fn set_exposure_reads_back_the_applied_ev() {
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "set-exposure", json!({ "ev": 2.5 }));
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["exposureEv"], json!(2.5));
        assert_eq!(stub.exposure_ev, 2.5);
    }

    #[test]
    fn set_tessellation_quality_clamps_and_reads_back_the_applied_budget() {
        // A partial request tunes only the named knobs and echoes the full clamped budget.
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "set-tessellation-quality",
            json!({ "factorCap": 32.0, "edgeLengthTarget": 6.0 }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["factorCap"], json!(32.0));
        assert_eq!(reply["result"]["edgeLengthTarget"], json!(6.0));
        // min_factor was not sent → unchanged at the default.
        assert_eq!(reply["result"]["minFactor"], json!(1.0));
        assert_eq!(stub.tess_factor_cap, 32.0);

        // Out-of-range values are clamped, not rejected: cap to [1,2048], edge target to ≥1.
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "set-tessellation-quality",
            json!({ "factorCap": 9999.0, "minFactor": 0.1, "edgeLengthTarget": 0.0 }),
        );
        assert_eq!(reply["result"]["factorCap"], json!(2048.0));
        assert_eq!(reply["result"]["minFactor"], json!(1.0));
        assert_eq!(reply["result"]["edgeLengthTarget"], json!(1.0));

        // A min_factor above the cap is pinned down to the cap (min ≤ cap invariant).
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "set-tessellation-quality",
            json!({ "factorCap": 4.0, "minFactor": 10.0 }),
        );
        assert_eq!(reply["result"]["factorCap"], json!(4.0));
        assert_eq!(reply["result"]["minFactor"], json!(4.0));
    }

    #[test]
    fn profiler_set_mode_reports_support_and_software_flag() {
        // Off → Off (always allowed); the support flags + software flag come straight
        // from the renderer so the editor can grey out unsupported modes.
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "profiler.set-mode", json!({ "mode": "off" }));
        assert_eq!(reply["result"]["mode"], json!("off"));
        assert_eq!(reply["result"]["timestampsSupported"], json!(false));
        assert_eq!(reply["result"]["softwareGpu"], json!(true));

        // A timestamps request clamps to Off when the device lacks timestamp support.
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "profiler.set-mode",
            json!({ "mode": "timestamps" }),
        );
        assert_eq!(reply["result"]["mode"], json!("off"));

        // With support the requested mode sticks.
        let mut stub = StubRenderer {
            timestamps_supported: true,
            ..StubRenderer::default()
        };
        let reply = run(
            &mut stub,
            "profiler.set-mode",
            json!({ "mode": "timestamps" }),
        );
        assert_eq!(reply["result"]["mode"], json!("timestamps"));
        assert_eq!(reply["result"]["timestampsSupported"], json!(true));
    }

    #[test]
    fn render_stats_reports_toggles_and_kebab_enums() {
        let mut stub = StubRenderer {
            view_mode: saffron_rendering::ViewMode::Albedo,
            ..StubRenderer::default()
        };
        let reply = run(&mut stub, "render-stats", json!({}));
        assert_eq!(reply["ok"], json!(true));
        let result = &reply["result"];
        assert_eq!(result["clustered"], json!(true));
        assert_eq!(result["softwareGpu"], json!(true));
        assert_eq!(result["hdr"], json!(true));
        assert_eq!(result["viewMode"], json!("albedo"));
        assert_eq!(result["aa"], json!("off"));
        assert_eq!(result["profilerMode"], json!("off"));
    }

    #[test]
    fn capture_start_acks_with_an_id() {
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "profiler.capture-start",
            json!({ "mode": "single", "frames": 1 }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["ack"], json!(true));
        assert_eq!(reply["result"]["captureId"], json!(1));
    }

    #[test]
    fn set_upscale_sets_ratio_budget_and_reports_extents() {
        let mut stub = StubRenderer::default();
        let target_ms = 1000.0 / 30.0; // → target_fps 30, budget ≈ 33.33ms
        let reply = run(
            &mut stub,
            "set-upscale",
            json!({ "ratio": 0.5, "dynamic": true, "targetMs": target_ms }),
        );
        assert_eq!(reply["ok"], json!(true));
        let up = &reply["result"]["upscale"];
        assert_eq!(up["ratio"], json!(0.5));
        assert_eq!(up["dynamic"], json!(true));
        assert!((up["targetMs"].as_f64().unwrap() - target_ms).abs() < 1e-2);
        // The input extent is the display extent scaled by the ratio (1280×720 × 0.5).
        assert_eq!(up["inputWidth"], json!(640));
        assert_eq!(up["displayWidth"], json!(1280));
    }

    #[test]
    fn set_upscale_is_a_partial_merge() {
        let mut stub = StubRenderer::default();
        run(
            &mut stub,
            "set-upscale",
            json!({ "ratio": 0.5, "dynamic": true }),
        );
        // A follow-up with only `ratio` must not clear `dynamic`.
        let reply = run(&mut stub, "set-upscale", json!({ "ratio": 0.75 }));
        let up = &reply["result"]["upscale"];
        assert_eq!(up["ratio"], json!(0.75));
        assert_eq!(up["dynamic"], json!(true), "omitted field keeps its value");
    }

    #[test]
    fn drain_alarms_defaults_to_since_zero() {
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "drain-alarms", json!({}));
        assert_eq!(reply["ok"], json!(true));
        assert!(reply["result"]["events"].as_array().unwrap().is_empty());
        assert_eq!(reply["result"]["overflowed"], json!(false));
    }

    #[test]
    fn set_viewport_size_rejects_unknown_view() {
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "set-viewport-size",
            json!({ "view": "nope", "width": 800, "height": 600 }),
        );
        assert_eq!(reply["ok"], json!(false));
        assert_eq!(reply["error"], json!("unknown view 'nope'"));
    }

    #[test]
    fn set_viewport_size_applies_to_scene_view() {
        let mut stub = StubRenderer::default();
        let reply = run(
            &mut stub,
            "set-viewport-size",
            json!({ "view": "scene", "width": 800, "height": 600 }),
        );
        assert_eq!(reply["ok"], json!(true));
        assert_eq!(reply["result"]["width"], json!(800));
        assert_eq!(reply["result"]["height"], json!(600));
        assert_eq!(stub.width, 800);
    }

    #[test]
    fn viewport_native_info_reports_the_bridge_status() {
        let mut stub = StubRenderer::default();
        let reply = run(&mut stub, "viewport-native-info", json!({}));
        assert_eq!(reply["result"]["platform"], json!("linux"));
        assert_eq!(reply["result"]["transport"], json!("wayland-subsurface"));
        assert_eq!(reply["result"]["width"], json!(1280));
    }

    #[test]
    fn get_upscale_reports_the_ratio_and_derived_extents() {
        // A non-native render scale reads back with the fixed display extent and an input
        // extent scaled down from it (input = round(display × ratio), ratio in (0, 1]).
        let mut stub = StubRenderer {
            render_scale: 0.75,
            ..StubRenderer::default()
        };
        let reply = run(&mut stub, "get-upscale", json!({}));
        assert_eq!(reply["ok"], json!(true));
        let up = &reply["result"]["upscale"];
        assert_eq!(up["ratio"], json!(0.75));
        assert_eq!(up["displayWidth"], json!(1280));
        assert_eq!(up["displayHeight"], json!(720));
        assert_eq!(up["inputWidth"], json!(960)); // round(1280 × 0.75)
        assert_eq!(up["inputHeight"], json!(540)); // round(720 × 0.75)
        assert!(up["inputWidth"].as_u64().unwrap() <= up["displayWidth"].as_u64().unwrap());
    }

    #[test]
    fn frame_history_maps_stats_and_samples_over_the_perf_budget() {
        // A controlled percentile summary + raw ring maps field-for-field; the reported budget
        // is the perf config's derived budget (1000 / target_fps), the one shared source.
        let mut stub = StubRenderer {
            perf_config: PerfConfig {
                target_fps: 30.0, // budget ≈ 33.33ms
                ..PerfConfig::default()
            },
            frame_stats: FrameHistoryStats {
                p50_ms: 8.0,
                p95_ms: 12.0,
                p99_ms: 16.0,
                p999_ms: 20.0,
                max_ms: 24.0,
                mean_ms: 9.0,
                stddev_ms: 2.0,
                stutter_count: 3,
                sample_count: 128,
            },
            frame_history_samples: vec![
                FrameSample {
                    frame_index: 100,
                    cpu_ms: 7.0,
                    gpu_ms: 5.0,
                    cpu_wait_ms: 1.0,
                },
                FrameSample {
                    frame_index: 101,
                    cpu_ms: 8.0,
                    gpu_ms: 6.0,
                    cpu_wait_ms: 2.0,
                },
            ],
            ..StubRenderer::default()
        };

        // No samples requested → summary only, empty samples array.
        let summary = run(&mut stub, "frame-history", json!({}));
        let r = &summary["result"];
        assert_eq!(r["p50Ms"], json!(8.0));
        assert_eq!(r["p95Ms"], json!(12.0));
        assert_eq!(r["p99Ms"], json!(16.0));
        assert_eq!(r["p999Ms"], json!(20.0));
        assert_eq!(r["maxMs"], json!(24.0));
        assert_eq!(r["stutterCount"], json!(3));
        assert_eq!(r["sampleCount"], json!(128));
        assert!((r["budgetMs"].as_f64().unwrap() - 1000.0 / 30.0).abs() < 1e-2);
        assert!(r["samples"].as_array().unwrap().is_empty());

        // Percentiles come from a sorted distribution → monotone non-decreasing.
        assert!(r["p50Ms"].as_f64().unwrap() <= r["p95Ms"].as_f64().unwrap());
        assert!(r["p95Ms"].as_f64().unwrap() <= r["p99Ms"].as_f64().unwrap());
        assert!(r["p99Ms"].as_f64().unwrap() <= r["p999Ms"].as_f64().unwrap());
        assert!(r["p999Ms"].as_f64().unwrap() <= r["maxMs"].as_f64().unwrap());

        // A samples request returns the recent raw frames (truncated to the request), each with
        // a monotonic absolute frame index — the field the editor dedups windows on.
        let with_samples = run(&mut stub, "frame-history", json!({ "samples": 32 }));
        let samples = with_samples["result"]["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0]["frameIndex"], json!(100));
        assert_eq!(samples[0]["cpuMs"], json!(7.0));
        assert!(
            samples[1]["frameIndex"].as_i64().unwrap() > samples[0]["frameIndex"].as_i64().unwrap()
        );
    }

    #[test]
    fn drain_alarms_maps_events_and_respects_the_since_cursor() {
        // A firing event then its resolve map field-for-field — the firing/resolved state enum,
        // the fingerprint as a decimal string, the resolve's duration — and the `since` cursor
        // filters out everything at or below it (the never-double-count contract).
        let mut stub = StubRenderer {
            alarm_drain: AlarmDrain {
                events: vec![
                    AlarmEvent {
                        seq: 1,
                        fingerprint: 42,
                        metric: "frame-budget".to_owned(),
                        pass: String::new(),
                        severity: AlarmSeverity::Warning,
                        kind: AlarmEventKind::Firing,
                        value: 20.0,
                        threshold: 16.6,
                        since_frame: 10,
                        count: 4,
                        duration_ms: 0.0,
                    },
                    AlarmEvent {
                        seq: 2,
                        fingerprint: 42,
                        metric: "frame-budget".to_owned(),
                        pass: String::new(),
                        severity: AlarmSeverity::Warning,
                        kind: AlarmEventKind::Resolved,
                        value: 8.0,
                        threshold: 16.6,
                        since_frame: 10,
                        count: 4,
                        duration_ms: 250.0,
                    },
                ],
                high_water_seq: 2,
                oldest_seq: 1,
                overflowed: false,
            },
            active_alarm_list: vec![ActiveAlarm {
                fingerprint: 42,
                metric: "frame-budget".to_owned(),
                pass: String::new(),
                severity: AlarmSeverity::Critical,
                value: 20.0,
                threshold: 16.6,
                peak: 22.0,
                since_frame: 10,
                since_ns: 0,
                last_seen_ns: 0,
                count: 4,
            }],
            ..StubRenderer::default()
        };

        let drained = run(&mut stub, "drain-alarms", json!({ "since": 0 }));
        let events = drained["result"]["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        // Seqs come back strictly increasing — the cursor's monotonic contract.
        assert!(events[1]["seq"].as_i64().unwrap() > events[0]["seq"].as_i64().unwrap());
        let firing = &events[0];
        assert_eq!(firing["metric"], json!("frame-budget"));
        assert_eq!(firing["state"], json!("firing"));
        assert_eq!(firing["fingerprint"], json!("42"));
        assert_eq!(firing["severity"], json!("warning"));
        let resolved = &events[1];
        assert_eq!(resolved["state"], json!("resolved"));
        assert_eq!(resolved["fingerprint"], firing["fingerprint"]);
        assert!(resolved["durationMs"].as_f64().unwrap() > 0.0);
        assert_eq!(drained["result"]["highWaterSeq"], json!(2));
        assert_eq!(drained["result"]["overflowed"], json!(false));

        // A drain from the high-water cursor re-sends nothing at or below it.
        let tail = run(&mut stub, "drain-alarms", json!({ "since": 2 }));
        assert!(tail["result"]["events"].as_array().unwrap().is_empty());

        // The active set is the badge source; it maps the coalesced entry.
        let active = run(&mut stub, "list-active-alarms", json!({}));
        let alarms = active["result"]["alarms"].as_array().unwrap();
        assert_eq!(alarms.len(), 1);
        assert_eq!(alarms[0]["metric"], json!("frame-budget"));
        assert_eq!(alarms[0]["fingerprint"], json!("42"));
        assert_eq!(alarms[0]["severity"], json!("critical"));
    }
}
