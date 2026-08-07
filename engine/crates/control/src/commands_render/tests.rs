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

/// `set-hierarchy-cut` pins the cut, reads it back, and refuses nothing — an unknown value is
/// not reachable through the typed enum, and omitting the field reads rather than resets.
#[test]
fn set_hierarchy_cut_pins_reads_and_returns_to_auto() {
    let mut stub = StubRenderer::default();
    let read = run(&mut stub, "set-hierarchy-cut", json!({}));
    assert_eq!(read["result"]["cut"], json!("auto"));

    let coarse = run(&mut stub, "set-hierarchy-cut", json!({ "cut": "coarse" }));
    assert_eq!(coarse["result"]["cut"], json!("coarse"));
    // Reading again must not reset it: an omitted field is a question, not an instruction.
    let again = run(&mut stub, "set-hierarchy-cut", json!({}));
    assert_eq!(again["result"]["cut"], json!("coarse"));

    let auto = run(&mut stub, "set-hierarchy-cut", json!({ "cut": "auto" }));
    assert_eq!(auto["result"]["cut"], json!("auto"));
}

/// `set-mesh-executor` switches the executor, reads it back without changing it, and reports
/// whether the device qualifies — the pair the parity suite drives one host through.
#[test]
fn set_mesh_executor_switches_reads_and_reports_support() {
    let mut stub = StubRenderer::default();
    let read = run(&mut stub, "set-mesh-executor", json!({}));
    assert_eq!(read["result"]["enabled"], json!(false));
    assert_eq!(read["result"]["supported"], json!(true));

    let enabled = run(&mut stub, "set-mesh-executor", json!({ "enabled": true }));
    assert_eq!(enabled["result"]["enabled"], json!(true));
    // An omitted field is a question, not an instruction.
    let again = run(&mut stub, "set-mesh-executor", json!({}));
    assert_eq!(again["result"]["enabled"], json!(true));

    let disabled = run(&mut stub, "set-mesh-executor", json!({ "enabled": false }));
    assert_eq!(disabled["result"]["enabled"], json!(false));
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
        shadows["error"]["message"],
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
    assert_eq!(result["sceneGatherMs"], json!(0.0));
    assert_eq!(result["instanceUploadBytes"], json!(0));
    assert_eq!(result["retainedMeshCpuBytes"], json!(0));
    assert_eq!(result["shadowDrawCalls"], json!(0));
    assert_eq!(result["rtInstances"], json!(0));
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
    assert_eq!(reply["error"]["message"], json!("unknown view 'nope'"));
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
                    owner: String::new(),
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
                    owner: String::new(),
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
            owner: String::new(),
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
