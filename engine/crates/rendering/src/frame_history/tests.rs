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
    assert!(
        drained.events.iter().any(|event| {
            event.owner == "cell 0,0,0 L0" && event.kind == AlarmEventKind::Resolved
        })
    );
}
