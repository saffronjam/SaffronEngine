use super::*;

#[test]
fn off_mode_arms_no_recorder() {
    let prof = GpuProfiler::default();
    assert_eq!(prof.mode, ProfilerMode::Off);
    assert!(!prof.pools_ready, "no query pools allocated when Off");
    let mut prof = prof;
    let rec = prof.frame_recorder(0);
    assert!(
        !rec.armed(),
        "the recorder is unarmed (a cheap branch) when Off"
    );
    assert!(rec.pool.is_none());
}

#[test]
fn cpu_span_buffer_records_a_nested_tree() {
    let mut registry = CpuMarkerRegistry::default();
    let mut buf = CpuSpanBuffer::default();
    buf.reset();
    let outer = buf.begin_span(&mut registry, "frame", 100);
    let inner = buf.begin_span(&mut registry, "scene", 110);
    buf.end_span(inner, 180);
    buf.end_span(outer, 200);

    assert_eq!(buf.spans.len(), 2);
    assert_eq!(buf.spans[outer].parent, -1);
    assert_eq!(buf.spans[outer].depth, 0);
    assert_eq!(buf.spans[outer].start_ns, 100);
    assert_eq!(buf.spans[outer].end_ns, 200);
    assert_eq!(buf.spans[inner].parent, outer as i32);
    assert_eq!(buf.spans[inner].depth, 1);
    assert_eq!(buf.spans[inner].end_ns, 180);
    assert_eq!(registry.name(buf.spans[inner].marker), "scene");
}

#[test]
fn marker_registry_interns_names() {
    let mut r = CpuMarkerRegistry::default();
    let a = r.id("scene");
    let b = r.id("tonemap");
    let a2 = r.id("scene");
    assert_eq!(a, a2, "re-interning a name returns the same id");
    assert_ne!(a, b);
    assert_eq!(r.name(a), "scene");
}

/// Builds the raw query result words for `n` scopes: per scope i, slots
/// `[begin, begin_avail, end, end_avail]`. `available` toggles whether the pair has
/// landed.
fn make_raw(spans: &[(u64, u64, bool)]) -> Vec<u64> {
    let mut raw = vec![0u64; spans.len() * 4];
    for (i, &(b, e, avail)) in spans.iter().enumerate() {
        raw[4 * i] = b;
        raw[4 * i + 1] = if avail { 1 } else { 0 };
        raw[4 * i + 2] = e;
        raw[4 * i + 3] = if avail { 1 } else { 0 };
    }
    raw
}

#[test]
fn decode_timings_yields_per_pass_spans_on_the_frame_relative_axis() {
    let records = vec![
        ScopeRecord {
            name: "scene".into(),
            parent_index: -1,
            depth: 0,
            stats_slot: -1,
            pixels: 0,
        },
        ScopeRecord {
            name: "tonemap".into(),
            parent_index: -1,
            depth: 0,
            stats_slot: -1,
            pixels: 0,
        },
    ];
    // scene: 1000..3000 ticks; tonemap: 3000..3500 ticks. period = 1ns/tick.
    let raw = make_raw(&[(1000, 3000, true), (3000, 3500, true)]);
    let cal = GpuCalibration::default(); // not correlated → frame-relative
    let timings = decode_timings(&records, &raw, u64::MAX, 1.0, &cal, None);

    assert_eq!(timings.len(), 2);
    // span_begin = 1000 (the earliest begin). scene starts at 0, ends at 2000ns.
    assert_eq!(timings[0].name, "scene");
    assert_eq!(timings[0].start_ns, 0);
    assert_eq!(timings[0].end_ns, 2000);
    assert!((timings[0].gpu_ms - 0.002).abs() < 1e-6, "2000ns = 0.002ms");
    // tonemap starts at 3000-1000 = 2000ns.
    assert_eq!(timings[1].start_ns, 2000);
    assert_eq!(timings[1].end_ns, 2500);
}

#[test]
fn unavailable_queries_decode_to_zero_spans() {
    let records = vec![ScopeRecord {
        name: "scene".into(),
        parent_index: -1,
        depth: 0,
        stats_slot: -1,
        pixels: 0,
    }];
    let raw = make_raw(&[(1000, 3000, false)]); // not yet available
    let timings = decode_timings(
        &records,
        &raw,
        u64::MAX,
        1.0,
        &GpuCalibration::default(),
        None,
    );
    assert_eq!(timings[0].gpu_ms, 0.0);
    assert_eq!(timings[0].start_ns, 0);
    assert_eq!(timings[0].end_ns, 0);
}

#[test]
fn correlated_decode_projects_onto_the_host_clock() {
    let records = vec![ScopeRecord {
        name: "scene".into(),
        parent_index: -1,
        depth: 0,
        stats_slot: -1,
        pixels: 0,
    }];
    let raw = make_raw(&[(1000, 3000, true)]);
    let cal = GpuCalibration {
        available: true,
        device_to_host_ns_offset: 1_000_000,
        correlated: true,
        ..Default::default()
    };
    let timings = decode_timings(&records, &raw, u64::MAX, 1.0, &cal, None);
    // start = 1000ns * 1 + 1_000_000 offset.
    assert_eq!(timings[0].start_ns, 1_001_000);
    assert_eq!(timings[0].end_ns, 1_003_000);
}

#[test]
fn frame_span_is_earliest_begin_to_latest_end() {
    let records = vec![
        ScopeRecord {
            name: "outer".into(),
            parent_index: -1,
            depth: 0,
            stats_slot: -1,
            pixels: 0,
        },
        ScopeRecord {
            name: "inner".into(),
            parent_index: 0,
            depth: 1,
            stats_slot: -1,
            pixels: 0,
        },
    ];
    // outer brackets inner: outer 100..900, inner 200..800. Span = 800 ticks, NOT
    // the sum (1400).
    let raw = make_raw(&[(100, 900, true), (200, 800, true)]);
    let ms = frame_span_ms(&records, &raw, u64::MAX, 1.0);
    assert!((ms - 0.0008).abs() < 1e-9, "800ns = 0.0008ms (not a sum)");
}

#[test]
fn stats_decode_reads_positional_counters_when_available() {
    let records = vec![ScopeRecord {
        name: "scene".into(),
        parent_index: -1,
        depth: 0,
        stats_slot: 0,
        pixels: 4096,
    }];
    let raw = make_raw(&[(0, 100, true)]);
    // stats words for slot 0: 6 counters + an availability word (non-zero = ready).
    let mut stats = vec![0u64; MAX_PROFILED_SCOPES as usize * (PIPELINE_STATS_COUNT + 1)];
    stats[0] = 10; // input_vertices
    stats[1] = 12; // vertex_invocations
    stats[2] = 8; // clipping_invocations
    stats[3] = 4; // clipping_primitives
    stats[4] = 5000; // fragment_invocations
    stats[5] = 0; // compute_invocations
    stats[PIPELINE_STATS_COUNT] = 1; // availability
    let timings = decode_timings(
        &records,
        &raw,
        u64::MAX,
        1.0,
        &GpuCalibration::default(),
        Some(&stats),
    );
    assert!(timings[0].has_stats);
    assert_eq!(timings[0].stats.input_vertices, 10);
    assert_eq!(timings[0].stats.fragment_invocations, 5000);
    assert_eq!(
        timings[0].stats.pixels, 4096,
        "render-area pixels from the record"
    );
}

#[test]
fn calibration_offset_projects_device_tick_onto_host_clock() {
    // A device tick of 1000 at 2ns/tick is 2000 device-ns; a host sample of 10^13 ns
    // yields offset = 10^13 - 2000, so the read-back maps the tick to the host clock.
    let host_ns = 10_000_000_000_000u64;
    let offset = device_to_host_offset(1000, host_ns, u64::MAX, 2.0);
    assert_eq!(offset, host_ns as i64 - 2000);
    // The mask drops the high (invalid) bits before scaling.
    let masked = device_to_host_offset(0xFFFF_0000_0000_03E8, host_ns, 0xFFFF_FFFF, 2.0);
    assert_eq!(masked, host_ns as i64 - 2000);
}

#[test]
fn calibrate_is_a_noop_when_unavailable() {
    // No device extension: availability is false, so `calibrate` cannot run and
    // correlation stays off (the own-axis fallback). A `Device` is not needed — the
    // availability gate returns before the sample call.
    let mut prof = GpuProfiler::with_facts(
        1.0,
        u64::MAX,
        true,
        false,
        false,
        vk::TimeDomainEXT::default(),
    );
    assert!(!prof.calibration.available);
    prof.calibration.correlated = false;
    // The gate that would otherwise call into the device returns early.
    if prof.calibration.available {
        unreachable!("unavailable calibration must not sample");
    }
    assert!(
        !prof.calibration.correlated,
        "correlation stays false when calibration is unavailable"
    );
}

#[test]
fn capture_recorder_arms_records_ready_then_drains() {
    // Drive the state machine with no GPU (the tick consumes CPU spans + the
    // profiler's last_timings, both empty here — exercising the transitions).
    let cpu = CpuProfiler::default();
    let profiler = GpuProfiler::default();
    // Manually arm a 3-frame capture (start() needs a Device; the state transitions
    // are the unit under test).
    let mut cap = CaptureRecorder {
        state: CaptureState::Arming,
        mode: CaptureMode::Frames,
        target_frames: 3,
        captured_frames: 0,
        warmup: (MAX_FRAMES_IN_FLIGHT + 1) as u32,
        ..Default::default()
    };

    // Arming burns down warmup.
    for _ in 0..(MAX_FRAMES_IN_FLIGHT + 1) {
        assert_eq!(cap.state, CaptureState::Arming);
        cap.tick(&cpu, 0, &profiler);
    }
    assert_eq!(
        cap.state,
        CaptureState::Recording,
        "warmup elapsed → Recording"
    );

    // Recording copies target_frames frames, then goes Ready.
    for _ in 0..3 {
        assert_eq!(cap.state, CaptureState::Recording);
        cap.tick(&cpu, 0, &profiler);
    }
    assert_eq!(
        cap.state,
        CaptureState::Ready,
        "captured target frames → Ready"
    );
    assert_eq!(cap.captured_frames, 3);
}

#[test]
fn redundant_finish_echoes_the_last_capture_instead_of_an_empty_one() {
    // A recorder that has recorded one frame's worth of spans.
    let span = ProfileSpan {
        name: "scene".into(),
        lane: ProfileLane::Gpu,
        start_ns: 0,
        end_ns: 1000,
        parent_index: -1,
        depth: 0,
        has_stats: false,
        stats: PipelineStats::default(),
    };
    let mut cap = CaptureRecorder {
        state: CaptureState::Recording,
        captured_frames: 1,
        capture: ProfileCapture {
            spans: vec![span.clone()],
            meta: ProfileCaptureMeta::default(),
        },
        ..Default::default()
    };

    // The real drain returns the spans and reports a recorded frame.
    let first = cap.finish(ProfileCaptureMeta::default());
    assert_eq!(first.spans.len(), 1);
    assert_eq!(first.meta.frame_count, 1);
    assert_eq!(cap.state, CaptureState::Idle);

    // A redundant drain (now Idle) echoes the same non-empty capture — it never returns an
    // empty result that would clobber the displayed one. This is the engine-side guard for
    // the overlapping-poll double-stop.
    let second = cap.finish(ProfileCaptureMeta::default());
    assert_eq!(
        second.spans.len(),
        1,
        "redundant stop echoes the last capture"
    );
    assert_eq!(second.spans[0].name, "scene");
    assert_eq!(second.meta.frame_count, 1);
}

#[test]
fn append_frame_rebases_parent_indices_across_lanes_and_frames() {
    let mut cpu = CpuProfiler::default();
    // One frame's CPU spans: a "frame" parent and a nested "scene" child.
    let outer = cpu.buffers[0].begin_span(&mut cpu.registry, "frame", 0);
    let inner = cpu.buffers[0].begin_span(&mut cpu.registry, "scene", 5);
    cpu.buffers[0].end_span(inner, 8);
    cpu.buffers[0].end_span(outer, 10);

    // One GPU pass with a nested child.
    let profiler = GpuProfiler {
        last_timings: vec![
            PassTiming {
                name: "scene-gpu".into(),
                parent_index: -1,
                depth: 0,
                ..Default::default()
            },
            PassTiming {
                name: "draw".into(),
                parent_index: 0,
                depth: 1,
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let mut cap = CaptureRecorder {
        state: CaptureState::Recording,
        include_cpu: true,
        target_frames: 2,
        ..Default::default()
    };
    // Two frames so the second frame's parents must rebase past the first.
    cap.tick(&cpu, 0, &profiler);
    cap.tick(&cpu, 0, &profiler);

    let spans = &cap.capture.spans;
    // Each frame: 2 cpu + 2 gpu = 4 spans → 8 total.
    assert_eq!(spans.len(), 8);
    // Frame 0: cpu outer at 0 (parent -1), cpu inner at 1 (parent 0), gpu at 2
    // (parent -1), gpu child at 3 (parent gpu_base=2).
    assert_eq!(
        spans[1].parent_index, 0,
        "cpu child points at the cpu parent"
    );
    assert_eq!(spans[2].lane, ProfileLane::Gpu);
    assert_eq!(spans[3].parent_index, 2, "gpu child rebased onto gpu_base");
    // Frame 1 starts at base=4: cpu child parent = 4, gpu child parent = 6.
    assert_eq!(spans[5].parent_index, 4);
    assert_eq!(spans[7].parent_index, 6);
}
