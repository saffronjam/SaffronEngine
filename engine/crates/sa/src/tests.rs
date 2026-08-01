use super::*;
use crate::format::*;
use crate::outcome::*;
use crate::start::*;
use saffron_control_client as wire;
use serde_json::json;

/// Composes the CLI's args-to-envelope path the production `forward` uses (`build_params`
/// coercion + the shared client's `request_envelope`), so the composition stays asserted even
/// though `forward` hands the params to `Client::call_raw` rather than pre-building the envelope.
fn build_request(cmd: &str, args: &[String]) -> Value {
    wire::request_envelope(1, cmd, build_params(args))
}

#[test]
fn request_envelope_shape() {
    let request = build_request("ping", &[]);
    assert_eq!(request["cmd"], json!("ping"));
    assert_eq!(request["id"], json!(1));
    assert!(request["params"].is_object());
    assert_eq!(request["params"], json!({}));
}

#[test]
fn request_envelope_maps_positionals_and_flags() {
    let args = vec![
        "1".to_owned(),
        "--yaw".to_owned(),
        "90".to_owned(),
        "--wireframe".to_owned(),
    ];
    let request = build_request("set-camera", &args);
    assert_eq!(request["cmd"], json!("set-camera"));
    assert_eq!(request["params"]["args"], json!([1]));
    assert_eq!(request["params"]["yaw"], json!(90));
    assert_eq!(request["params"]["wireframe"], json!(true));
}

#[test]
fn flag_with_equals_splits_on_first_equals() {
    let request = build_request("x", &["--key=a=b".to_owned()]);
    assert_eq!(request["params"]["key"], json!("a=b"));
}

fn args(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|t| (*t).to_owned()).collect()
}

#[test]
fn coerce_boolean_and_null_literals() {
    assert_eq!(coerce("true"), json!(true));
    assert_eq!(coerce("false"), json!(false));
    assert_eq!(coerce("null"), Value::Null);
}

#[test]
fn coerce_unsigned_integer() {
    let value = coerce("42");
    assert_eq!(value, json!(42));
    assert!(value.is_u64());
}

#[test]
fn coerce_signed_integer_takes_signed_path() {
    let value = coerce("-42");
    assert_eq!(value, json!(-42));
    assert!(value.is_i64());
    // The `-` guard skips the unsigned parse, so a negative never lands on `is_u64`.
    assert!(!value.is_u64());
}

#[test]
#[allow(clippy::approx_constant)] // 3.14 is a coercion fixture, not an approximation of PI.
fn coerce_float() {
    let value = coerce("3.14");
    assert_eq!(value, json!(3.14));
    assert!(value.is_f64());
}

#[test]
fn coerce_u64_max_stays_unsigned() {
    // u64::MAX must serialize as an unsigned number, not a lossy float or a bare string —
    // this is what the unsigned-before-float ordering guarantees.
    let value = coerce("18446744073709551615");
    assert!(value.is_u64());
    assert_eq!(value.as_u64(), Some(u64::MAX));
    assert!(!value.is_f64());
    assert!(!value.is_string());
}

#[test]
fn coerce_inline_json_objects_arrays_and_strings() {
    assert_eq!(coerce(r#"{"a":1}"#), json!({"a": 1}));
    assert_eq!(coerce("[1,2]"), json!([1, 2]));
    assert_eq!(coerce(r#""hi""#), json!("hi"));
}

#[test]
fn coerce_malformed_json_falls_through_to_string() {
    // A leading `{` triggers the JSON branch, the parse fails, and the value falls through the
    // numeric ladder to the bare string.
    assert_eq!(coerce("{nope"), json!("{nope"));
}

#[test]
fn coerce_bare_string() {
    assert_eq!(coerce("foo"), json!("foo"));
}

#[test]
fn coerce_empty_token_is_empty_string() {
    // An empty token opens with none of `{`/`[`/`"`, parses as no number, and stays the bare
    // (empty) string — the empty guards keep it off the JSON and unsigned paths.
    assert_eq!(coerce(""), json!(""));
}

#[test]
fn build_params_collects_positionals() {
    assert_eq!(
        build_params(&args(&["1", "2", "3"])),
        json!({"args": [1, 2, 3]})
    );
}

#[test]
fn build_params_flag_with_separate_value() {
    assert_eq!(build_params(&args(&["--yaw", "90"])), json!({"yaw": 90}));
}

#[test]
fn build_params_flag_with_equals() {
    assert_eq!(build_params(&args(&["--yaw=90"])), json!({"yaw": 90}));
}

#[test]
fn build_params_bare_flag_is_true() {
    assert_eq!(
        build_params(&args(&["--enabled"])),
        json!({"enabled": true})
    );
}

#[test]
fn build_params_mixes_positionals_and_flags() {
    assert_eq!(
        build_params(&args(&["cube", "--yaw", "90", "extra"])),
        json!({"args": ["cube", "extra"], "yaw": 90})
    );
}

#[test]
fn build_params_empty_has_no_args_key() {
    assert_eq!(build_params(&[]), json!({}));
}

#[test]
fn build_params_bare_flag_before_flag_vs_value() {
    // `--a` is followed by another flag, so it is a bare-true; `--b` is followed by a value.
    assert_eq!(
        build_params(&args(&["--a", "--b", "x"])),
        json!({"a": true, "b": "x"})
    );
}

#[test]
fn set_camera_request_composes_envelope_and_params() {
    let request = build_request("set-camera", &args(&["1", "2", "3", "--fov", "60"]));
    assert_eq!(
        request,
        json!({"cmd": "set-camera", "params": {"args": [1, 2, 3], "fov": 60}, "id": 1})
    );
}

/// Helpers building the wire client's call outcome the CLI presents (the envelope *parsing*
/// is the shared client's contract, tested there; these prove the CLI's presentation arm).
fn engine_err(cmd: &str, message: &str) -> wire::Result<Value> {
    Err(wire::Error::Engine {
        cmd: cmd.to_owned(),
        failure: Box::new(ControlFailureDto::Command {
            message: message.to_owned(),
        }),
    })
}

#[test]
fn ok_result_is_exit_zero() {
    let result = Ok(json!({ "engine": "Saffron Anima" }));
    assert!(matches!(
        present_outcome("ping", result, OutputMode::Text),
        Outcome::Ok
    ));
}

#[test]
fn engine_error_carries_message() {
    // The engine's message is carried verbatim into the text outcome (an unknown command may
    // additionally gain a `did you mean` hint, covered by its own test — assert the prefix).
    match present_outcome(
        "nope",
        engine_err("nope", "unknown command 'nope'"),
        OutputMode::Text,
    ) {
        Outcome::Error { text, .. } => {
            assert!(text.starts_with("unknown command 'nope'"));
        }
        Outcome::Ok => panic!("expected an error outcome"),
    }
}

#[test]
fn json_engine_error_preserves_the_complete_diagnostic() {
    let expected = ControlFailureDto::Diagnostic {
        message: "graph candidates limit exceeded: requested 16, limit 4".to_owned(),
        diagnostic: saffron_protocol::ControlDiagnosticDto::VegetationGraph(
            saffron_protocol::VegetationGraphDiagnosticDto::Limit {
                resource: "candidates".to_owned(),
                requested: "16".to_owned(),
                limit: "4".to_owned(),
            },
        ),
    };
    let outcome = present_outcome(
        "vegetation-compile-biome",
        Err(wire::Error::Engine {
            cmd: "vegetation-compile-biome".to_owned(),
            failure: Box::new(expected.clone()),
        }),
        OutputMode::Json,
    );

    match outcome {
        Outcome::Error {
            failure,
            text,
            mode,
        } => {
            assert_eq!(failure, expected);
            assert_eq!(text, expected.message());
            assert_eq!(mode, OutputMode::Json);
        }
        Outcome::Ok => panic!("expected an error outcome"),
    }
}

#[test]
fn malformed_reply_is_error() {
    match present_outcome("ping", Err(wire::Error::MalformedReply), OutputMode::Text) {
        Outcome::Error { text, failure, .. } => {
            assert_eq!(text, "malformed reply");
            assert_eq!(failure.code(), "malformed-reply");
        }
        Outcome::Ok => panic!("expected a malformed-reply error"),
    }
}

/// The forwarded-command tokens (and the global `-o`) survive into the external arm — the
/// free-form capture the whole CLI is built around.
#[test]
fn cli_parses_output_and_external_command() {
    let cli = Cli::try_parse_from(["sa", "-o", "json", "set-camera", "--yaw", "90"]).unwrap();
    assert_eq!(cli.output, OutputMode::Json);
    match cli.command {
        Some(Subcmd::External(tokens)) => assert_eq!(tokens, vec!["set-camera", "--yaw", "90"]),
        other => panic!("expected an external command, got {other:?}"),
    }
}

/// A bare `--flag` token after the command survives into the external capture (rather than
/// clap rejecting it as an unknown option), so `build_params` can map it.
#[test]
fn cli_forwards_hyphen_flags_into_external() {
    let cli = Cli::try_parse_from(["sa", "list-entities", "--all"]).unwrap();
    match cli.command {
        Some(Subcmd::External(tokens)) => assert_eq!(tokens, vec!["list-entities", "--all"]),
        other => panic!("expected an external command, got {other:?}"),
    }
}

/// `help` stays an external (engine-forwarded) command — `disable_help_subcommand` keeps clap's
/// built-in help off the `help` token, so the live engine reply is the authoritative list.
#[test]
fn cli_help_is_forwarded_not_clap_builtin() {
    let cli = Cli::try_parse_from(["sa", "help"]).unwrap();
    match cli.command {
        Some(Subcmd::External(tokens)) => assert_eq!(tokens, vec!["help"]),
        other => panic!("expected `help` forwarded, got {other:?}"),
    }
}

#[test]
fn cli_parses_start_flags() {
    let cli = Cli::try_parse_from(["sa", "start", "--attach", "--build"]).unwrap();
    match cli.command {
        Some(Subcmd::Start { attach, build }) => {
            assert!(attach);
            assert!(build);
        }
        other => panic!("expected start, got {other:?}"),
    }
    // Defaults: neither flag.
    let plain = Cli::try_parse_from(["sa", "start"]).unwrap();
    assert!(matches!(
        plain.command,
        Some(Subcmd::Start {
            attach: false,
            build: false
        })
    ));
}

#[test]
fn cli_parses_completions_shell() {
    let cli = Cli::try_parse_from(["sa", "completions", "bash"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Subcmd::Completions { shell: Shell::Bash })
    ));
}

#[test]
fn cli_allows_no_command() {
    let cli = Cli::try_parse_from(["sa"]).unwrap();
    assert!(cli.command.is_none());
}

/// A tripwire that the `saffron-protocol` edge is live: the shared command table is non-empty
/// and carries the known anchors. The `Uuid` byte-identity is proven in the protocol crate.
#[test]
fn protocol_command_table_is_live() {
    let names: Vec<&str> = saffron_protocol::COMMANDS.iter().map(|c| c.name).collect();
    assert!(!names.is_empty());
    assert!(names.contains(&"ping"));
    assert!(names.contains(&"quit"));
}

/// Help-enrichment: the long `--help` lists the static-table anchor names and points at
/// `sa help` for the live list — offline, from `COMMANDS`, with no engine running.
#[test]
fn long_help_lists_commands_and_points_at_sa_help() {
    let help = enriched_command().render_long_help().to_string();
    assert!(help.contains("ping"));
    assert!(help.contains("quit"));
    assert!(help.contains("sa help"));
}

/// Completions are sourced from `COMMANDS`: the generated bash script offers a command anchor.
#[test]
fn completions_script_carries_command_anchor() {
    let mut command = completion_command();
    let mut out = Vec::new();
    clap_complete::generate(Shell::Bash, &mut command, "sa", &mut out);
    let script = String::from_utf8(out).expect("completion script is UTF-8");
    assert!(script.contains("ping"));
    assert!(script.contains("render-stats"));
}

/// An unknown command is still *forwarded* (never gated by the CLI), and the rendered error
/// carries a nearest-name `did you mean…?` hint computed offline against `COMMANDS`.
#[test]
fn unknown_command_error_carries_did_you_mean() {
    // `pign` is one transposition from `ping`; the engine answers `unknown command`, and the
    // CLI appends the suggestion.
    match present_outcome(
        "pign",
        engine_err("pign", "unknown command 'pign'"),
        OutputMode::Text,
    ) {
        Outcome::Error { text, .. } => {
            assert!(text.starts_with("unknown command 'pign'"));
            assert!(text.contains("did you mean 'ping'?"), "got: {text}");
        }
        Outcome::Ok => panic!("expected an error outcome"),
    }
}

#[test]
fn did_you_mean_finds_near_names_and_ignores_far_ones() {
    assert_eq!(did_you_mean("pign"), Some("ping"));
    assert_eq!(did_you_mean("renderstats"), Some("render-stats"));
    // A known command is never suggested against itself (distance 0).
    assert_eq!(did_you_mean("ping"), None);
    // A wholly unrelated token has no near name.
    assert_eq!(did_you_mean("zzzzzzzzzzzzz"), None);
}

#[test]
fn known_command_error_has_no_hint() {
    // A real command that errors for another reason must not gain a `did you mean` tail.
    match present_outcome(
        "get-camera",
        engine_err("get-camera", "no primary camera"),
        OutputMode::Text,
    ) {
        Outcome::Error { text, .. } => assert_eq!(text, "no primary camera"),
        Outcome::Ok => panic!("expected an error outcome"),
    }
}

#[test]
fn levenshtein_basic_distances() {
    assert_eq!(levenshtein("ping", "ping"), 0);
    assert_eq!(levenshtein("pign", "ping"), 2);
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("kitten", "sitting"), 3);
}

/// `start`'s already-running detection (`engine_running`): a live listener on the socket reads
/// as running, so `start` short-circuits to "already running" rather than relaunching. This is
/// the no-shell-out half of the `start` precondition the integration launch builds on.
#[test]
fn engine_running_detects_live_listener() {
    let dir = std::env::temp_dir().join(format!("sa-running-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("live.sock");
    let path_str = path.to_string_lossy().into_owned();
    let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert!(engine_running(&path_str));
    // Cleanup: the listener drops here; remove the dir.
    drop(_listener);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A socket path with no listener is *not* running, and a stale file at the path is unlinked so
/// a fresh launch can re-bind.
#[test]
fn engine_running_unlinks_stale_socket() {
    let dir = std::env::temp_dir().join(format!("sa-stale-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stale.sock");
    let path_str = path.to_string_lossy().into_owned();
    // A plain file standing in for a stale socket — connect refuses, so it must be unlinked.
    std::fs::write(&path, b"stale").unwrap();
    assert!(path.exists());
    assert!(!engine_running(&path_str));
    assert!(!path.exists(), "stale socket path should be unlinked");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path with nothing there at all is simply not running (no unlink, no error).
#[test]
fn engine_running_false_for_missing_path() {
    let path = std::env::temp_dir()
        .join(format!("sa-absent-{}.sock", std::process::id()))
        .to_string_lossy()
        .into_owned();
    let _ = std::fs::remove_file(&path);
    assert!(!engine_running(&path));
}

/// The engine-binary path honors the `SAFFRON_ANIMA_BIN` override (the parallel-binary knob),
/// and otherwise resolves a `saffron-host`-named sibling of the running `sa` binary.
#[test]
fn engine_binary_path_resolution() {
    // The override is read live; this crate is `#![deny(unsafe_code)]`, so rather than mutate
    // the process env (which needs `unsafe`), assert the default branch's invariant: the
    // resolved path ends in the build target name.
    assert!(engine_binary_path().ends_with(ENGINE_BIN_TARGET));
}

#[test]
fn format_ping_line() {
    let result = json!({"engine": "SaffronAnima", "version": "0.1", "pid": 4242});
    assert_eq!(
        format_text("ping", &result),
        vec!["pong  engine=SaffronAnima  version=0.1  pid=4242"]
    );
}

#[test]
fn format_ping_missing_fields_read_defaults() {
    // A reply with no fields must not panic; each reader takes its default.
    assert_eq!(
        format_text("ping", &json!({})),
        vec!["pong  engine=  version=  pid=0"]
    );
}

#[test]
fn format_render_stats_line() {
    let result = json!({
        "cpuFrameMs": 4.5, "gpuFrameMs": 3.25, "cpuWaitMs": 0.5, "fps": 144.0,
        "drawCalls": 120, "triangles": 50000, "descriptorBinds": 12, "pipelinesCreated": 2,
        "vramUsageBytes": 1_610_612_736u64, "vramBudgetBytes": 8_589_934_592u64,
        "softwareGpu": true,
    });
    assert_eq!(
        format_text("render-stats", &result),
        vec![
            "cpu=4.50ms  gpu=3.25ms  wait=0.50ms  fps=144  draws=120  tris=50000  binds=12  pso+=2  vram=1536/8192MiB  [software-gpu]"
        ]
    );
}

#[test]
fn format_render_stats_omits_software_suffix() {
    let result = json!({"fps": 60.0});
    let lines = format_text("render-stats", &result);
    assert!(lines[0].starts_with("cpu=0.00ms"));
    assert!(!lines[0].contains("software-gpu"));
    assert!(lines[0].ends_with("vram=0/0MiB"));
}

#[test]
fn format_list_entities_table() {
    let result = json!({
        "entities": [
            {"id": "10", "name": "Camera", "parentId": ""},
            {"id": "11", "name": "Cube", "parentId": "10"},
        ]
    });
    assert_eq!(
        format_text("list-entities", &result),
        vec![
            "  10                        Camera                    ",
            "  11                        Cube                      10",
        ]
    );
}

#[test]
fn format_list_components_strings() {
    let result = json!({"components": ["Transform", "MeshRenderer"]});
    assert_eq!(
        format_text("list-components", &result),
        vec!["  Transform", "  MeshRenderer"]
    );
}

#[test]
fn format_help_two_column_table() {
    let result = json!({
        "commands": [
            {"name": "ping", "help": "liveness probe"},
            {"name": "render-stats", "help": "frame timing snapshot"},
        ]
    });
    assert_eq!(
        format_text("help", &result),
        vec![
            "  ping                    liveness probe",
            "  render-stats            frame timing snapshot",
        ]
    );
}

#[test]
fn format_raycast_hit_branch() {
    let result = json!({
        "hit": true, "target": {"kind": "scene-entity", "id": "42"},
        "point": {"x": 1.0, "y": 2.0, "z": 3.0},
        "normal": {"x": 0.0, "y": 1.0, "z": 0.0},
        "distance": 5.5,
    });
    assert_eq!(
        format_text("raycast", &result),
        vec!["hit entity=42  point=(1.000, 2.000, 3.000)  normal=(0.00, 1.00, 0.00)  dist=5.500"]
    );
    let plant = json!({
        "hit": true, "target": {"kind": "vegetation", "plant": "00ab"},
        "point": {"x": 0.0, "y": 0.0, "z": 0.0},
        "normal": {"x": 0.0, "y": 1.0, "z": 0.0},
        "distance": 1.0,
    });
    assert_eq!(
        format_text("raycast", &plant),
        vec!["hit plant=00ab  point=(0.000, 0.000, 0.000)  normal=(0.00, 1.00, 0.00)  dist=1.000"]
    );
}

#[test]
fn format_raycast_no_hit_branch() {
    assert_eq!(
        format_text("raycast", &json!({"hit": false})),
        vec!["no hit"]
    );
    // A reply with no `hit` field defaults to false → no hit, no panic.
    assert_eq!(format_text("shapecast", &json!({})), vec!["no hit"]);
}

#[test]
fn format_selection_selected_vs_none() {
    let selected = json!({
        "entity": {"name": "Cube"}, "selectionVersion": 7, "sceneVersion": 12,
    });
    assert_eq!(
        format_text("get-selection", &selected),
        vec!["selected: Cube  (sel v7, scene v12)"]
    );
    let none = json!({"selectionVersion": 3, "sceneVersion": 4});
    assert_eq!(
        format_text("get-selection", &none),
        vec!["no selection  (sel v3, scene v4)"]
    );
    // An `entity` that is not an object falls to the no-selection branch.
    let null_entity = json!({"entity": Value::Null, "selectionVersion": 0, "sceneVersion": 0});
    assert_eq!(
        format_text("get-selection", &null_entity),
        vec!["no selection  (sel v0, scene v0)"]
    );
}

#[test]
fn format_asset_model_indented_bone_tree_and_clips() {
    let result = json!({
        "name": "Knight", "mesh": "100",
        "capabilities": {
            "meshCount": 1, "materialCount": 2, "nodeCount": 4,
            "hasRig": true, "boneCount": 3, "clipCount": 1,
        },
        "bones": [
            {"name": "root", "parent": -1, "joint": true},
            {"name": "spine", "parent": 0, "joint": true},
            {"name": "head", "parent": 1, "joint": false},
        ],
        "clips": [
            {"name": "idle", "duration": 1.5, "id": "200"},
        ],
    });
    assert_eq!(
        format_text("get-asset-model", &result),
        vec![
            "model Knight  (mesh 100)".to_owned(),
            "  meshes=1  materials=2  nodes=4  rig=yes  bones=3  clips=1".to_owned(),
            "  root  [joint]".to_owned(),
            "    spine  [joint]".to_owned(),
            "      head".to_owned(),
            "  clip idle                             1.500s  200".to_owned(),
        ]
    );
}

#[test]
fn format_list_clips_has_no_clip_prefix() {
    // `list-clips` uses a 32-wide name column and no `clip ` prefix (unlike get-asset-model).
    let result = json!({"clips": [{"name": "walk", "duration": 2.0, "id": "300"}]});
    assert_eq!(
        format_text("list-clips", &result),
        vec!["  walk                                 2.000s  300"]
    );
}

#[test]
fn format_asset_model_cyclic_parent_terminates() {
    // A parent index that cycles must not hang — the 256-iteration guard bounds the walk.
    let result = json!({
        "name": "Bad", "mesh": "0",
        "bones": [
            {"name": "a", "parent": 1},
            {"name": "b", "parent": 0},
        ],
    });
    let lines = format_text("get-asset-model", &result);
    // Header + caps + two bone lines; the test reaching this assertion is the no-hang proof.
    assert_eq!(lines.len(), 4);
    assert!(lines[2].trim_start().starts_with('a'));
    assert!(lines[3].trim_start().starts_with('b'));
}

#[test]
fn format_play_state_line() {
    let result = json!({
        "state": "Playing", "playVersion": 5, "sceneVersion": 9, "hasPrimaryCamera": true,
    });
    let expected = vec!["state=Playing  playVersion=5  sceneVersion=9  camera=ok"];
    assert_eq!(format_text("play", &result), expected);
    assert_eq!(format_text("get-play-state", &result), expected);
    // Missing camera flag → "missing"; missing state → empty, no panic.
    assert_eq!(
        format_text("stop", &json!({})),
        vec!["state=  playVersion=0  sceneVersion=0  camera=missing"]
    );
}

#[test]
fn format_physics_state_line() {
    let result = json!({"active": true, "bodyCount": 8, "dynamicCount": 3});
    assert_eq!(
        format_text("physics-state", &result),
        vec!["physics=active  bodies=8  dynamic=3"]
    );
    assert_eq!(
        format_text("physics-state", &json!({})),
        vec!["physics=inactive  bodies=0  dynamic=0"]
    );
}

#[test]
fn format_thumbnail_base64_byte_math() {
    // 8 base64 chars → (8 / 4) * 3 = 6 bytes.
    let result = json!({"format": "png", "width": 64, "height": 64, "base64": "AAAAAAAA"});
    assert_eq!(
        format_text("get-thumbnail", &result),
        vec!["png 64x64  ~6 bytes (base64 8 chars)"]
    );
    assert_eq!(
        format_text("view-asset", &json!({})),
        vec![" 0x0  ~0 bytes (base64 0 chars)"]
    );
}

#[test]
fn format_capture_stop_not_ready() {
    assert_eq!(
        format_text("profiler.capture-stop", &json!({"ready": false})),
        vec!["no capture ready (arm one with: sa profiler.capture-start)"]
    );
}

#[test]
fn format_capture_stop_with_path_prints_path() {
    // With a `path` present, the arm prints it and the inline-write branch is skipped
    // (the write happens only when `path` is empty).
    let result = json!({
        "ready": true, "frameCount": 3, "path": "/some/where/trace.json",
        "capture": {"spans": [1, 2], "metadata": {"correlated": true}},
    });
    let lines = format_text("profiler.capture-stop", &result);
    assert_eq!(lines[0], "captured 3 frame(s), 2 spans  [correlated]");
    assert_eq!(
        lines[1],
        "trace: /some/where/trace.json  (open in chrome://tracing or ui.perfetto.dev)"
    );
}

#[test]
fn format_capture_stop_inline_trace_writes_temp_file() {
    // No `path` but an inline `chromeTrace` → the arm writes the trace to
    // `<temp_dir>/saffron-profile.json` and prints that path. Read it back to prove the bytes.
    let result = json!({
        "ready": true, "frameCount": 1,
        "capture": {"spans": [1], "metadata": {"softwareGpu": true}},
        "chromeTrace": "INLINE-TRACE-BYTES",
    });
    let lines = format_text("profiler.capture-stop", &result);
    assert_eq!(
        lines[0],
        "captured 1 frame(s), 1 spans  [uncorrelated, software-gpu]"
    );
    let written = std::env::temp_dir().join("saffron-profile.json");
    let path = written.to_string_lossy().into_owned();
    assert_eq!(
        lines[1],
        format!("trace: {path}  (open in chrome://tracing or ui.perfetto.dev)")
    );
    assert_eq!(
        std::fs::read_to_string(&written).unwrap(),
        "INLINE-TRACE-BYTES"
    );
}

#[test]
fn format_vegetation_nav_counts_obstacles_per_cell() {
    let result = json!({
        "cells": [{
            "cell": {"coordinates": ["1", "0", "-2"], "level": 0},
            "contributions": [
                {"plant": "40aa", "kind": "static-obstacle", "heightM": 4.0, "cost": 1.0,
                 "footprint": [], "bounds": {"minTicks": ["0","0","0"], "maxTicksExclusive": ["1","1","1"]}},
                {"plant": "40bb", "kind": "cost", "heightM": 0.5, "cost": 0.4,
                 "footprint": [], "bounds": {"minTicks": ["0","0","0"], "maxTicksExclusive": ["1","1","1"]}},
            ],
        }],
        "dirtyRegions": [{"minTicks": ["0","0","0"], "maxTicksExclusive": ["1","1","1"]}],
        "contributions": "2", "obstacles": "1", "dynamicObstacles": "0", "drained": true,
    });
    let lines = format_text("vegetation-nav-contributions", &result);
    assert_eq!(
        lines[0],
        "  cell 1,0,-2 L0                     2 contribution(s), 1 obstacle(s)"
    );
    assert_eq!(
        lines[1],
        "  dirty=1  contributions=2  obstacles=1 (dynamic 0)  drained=yes"
    );
}

/// The traversal terms are the only reason a query's cost is legible, so the counter block prints
/// each of them rather than the hit count alone.
#[test]
fn format_vegetation_telemetry_prints_every_query_traversal_term() {
    let result = json!({
        "last": {"residencyUs": 420, "promotionUs": 0, "collisionUs": 110, "navigationUs": 30,
                 "ecologyUs": 180, "totalUs": 740},
        "average": {"residencyUs": 390, "promotionUs": 10, "collisionUs": 90, "navigationUs": 40,
                    "ecologyUs": 60, "totalUs": 590},
        "work": {
            "synchronizations": "1284", "queries": "17", "queryHits": "402",
            "queryGenerationsVisited": "34", "queryNodesVisited": "511", "queryRowsTested": "1980",
            "mutations": "3", "mutationBytes": "612",
            "snapshots": "1", "snapshotBytes": "48210", "ecologyTicks": "96",
        },
        "collisionBodies": "142", "navigationContributions": "88", "promoted": "1",
    });
    let lines = format_text("vegetation-telemetry", &result);
    assert_eq!(
        lines[2],
        "  syncs=1284  queries=17 (hits 402, cells 34, nodes 511, rows 1980)  mutations=3 (612 bytes)"
    );
    assert_eq!(lines[3], "  snapshots=1 (48210 bytes)  ecologyTicks=96");
    assert_eq!(lines[4], "  bodies=142  navContributions=88  promoted=1");
}

#[test]
fn format_vegetation_events_lists_transitions_then_the_cursor() {
    let result = json!({
        "events": [
            {
                "seq": "7", "transaction": "12",
                "cell": {"coordinates": ["0", "0", "0"], "level": 0},
                "plant": "40aabbccddeeff00112233445566778899",
                "transition": {"kind": "damaged", "amount": 16000, "health": 40000},
            },
            {
                "seq": "8", "transaction": "12",
                "cell": {"coordinates": ["0", "0", "0"], "level": 0},
                "transition": {"kind": "disturbed", "categories": 3},
            },
        ],
        "highWaterSeq": "8", "oldestSeq": "1", "overflowed": false,
    });
    let lines = format_text("vegetation-drain-events", &result);
    assert_eq!(
        lines[0],
        "  #7      damaged             plant=40aabbccddeeff00112233445566778899"
    );
    // A cell-wide change (a disturbance tile) names no plant.
    assert_eq!(lines[1], "  #8      disturbed           cell-wide");
    assert_eq!(lines[2], "  high=8  oldest=1  overflowed=no  (2 events)");
}

#[test]
fn format_promotion_reports_the_committed_and_pending_states() {
    // A promotion request that has not committed yet carries no entity.
    let pending = json!({
        "plant": "40aabbccddeeff00112233445566778899",
        "state": {"state": "promoting"},
    });
    assert_eq!(
        format_text("vegetation-promote", &pending)[0],
        "plant=40aabbccddeeff00112233445566778899  state=promoting"
    );

    // A live view names the entity that owns the plant.
    let live = json!({
        "plant": "40aabbccddeeff00112233445566778899",
        "state": {"state": "demoting", "entity": "7"},
    });
    assert_eq!(
        format_text("vegetation-demote", &live)[0],
        "plant=40aabbccddeeff00112233445566778899  state=demoting  entity=7"
    );
}

#[test]
fn json_mode_ignores_command_match() {
    // `-o json` prints `to_string_pretty` regardless of the command name — the text `match` is
    // never consulted. `print_result` writes to stdout, so assert the underlying call directly:
    // a `render-stats` value renders as JSON, not the one-line text formatter.
    let result = json!({"fps": 60.0});
    let json_out = pretty(&result);
    assert!(json_out.contains("\"fps\""));
    // The text formatter would instead produce the cpu=… line — different output entirely.
    assert!(format_text("render-stats", &result)[0].starts_with("cpu="));
}

#[test]
fn fallback_unrecognized_command_is_pretty_json_utf8_unescaped() {
    // An unknown command falls through to pretty JSON; a non-ASCII em-dash stays literal.
    let result = json!({"note": "a — b"});
    let lines = format_text("totally-unknown-command", &result);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains('—'));
    assert!(!lines[0].contains("\\u"));
}

#[test]
fn pass_timings_table_with_total() {
    let result = json!({
        "softwareGpu": false,
        "passes": [
            {"name": "gbuffer", "gpuMs": 1.25},
            {"name": "lighting", "gpuMs": 2.5},
        ],
        "gpuTotalMs": 3.75,
    });
    assert_eq!(
        format_text("pass-timings", &result),
        vec![
            "  gbuffer                          1.250 ms",
            "  lighting                         2.500 ms",
            "  total (span)                     3.750 ms",
        ]
    );
}

#[test]
fn ecology_report_and_clock_lines() {
    let report = json!({
        "worldTick": "64",
        "regions": 2,
        "regionsCaughtUp": 1,
        "regionsAwaitingResidency": 1,
        "ticksRun": "8",
        "ticksOwed": "56",
        "ticksAwaitingResidency": "64",
        "workers": 3,
        "checkpoint": "9f21c4b0aa73deadbeef",
    });
    assert_eq!(
        format_text("vegetation-advance-ecology", &report),
        vec![
            "worldTick=64  ticksRun=8 on 3 workers  ticksOwed=56 (+64 awaiting residency)  regions=2 (1 caught up, 1 awaiting residency)  checkpoint=9f21c4b0aa73"
        ]
    );

    let clock = json!({
        "running": true,
        "tickMilliseconds": 60000,
        "pendingMilliseconds": "1500",
        "maxTicksPerSync": 8,
        "workers": 4,
        "water": 40000,
        "warmth": 45000,
        "ticksOwed": "56",
    });
    assert_eq!(
        format_text("vegetation-ecology-clock", &clock),
        vec![
            "clock=running  tick=60000ms (+1500ms pending)  budget=8 ticks/sync  workers=4  water=40000  warmth=45000  owed=56"
        ]
    );
}

#[test]
fn ecology_status_reports_the_clock_every_region_and_every_cell() {
    let result = json!({
        "worldTick": "8",
        "simulationVersion": 2,
        "checkpoint": "0011223344556677",
        "regionRadiusCells": 1,
        "clock": {
            "running": false,
            "tickMilliseconds": 1000,
            "pendingMilliseconds": "0",
            "maxTicksPerSync": 8,
            "workers": 1,
            "water": 40000,
            "warmth": 45000,
            "ticksOwed": "5",
        },
        "regions": [
            {
                "cells": [{"coordinates": ["0", "0", "0"], "level": 0}],
                "tick": "3",
                "caughtUp": false,
                "resident": true,
            },
            {
                "cells": [
                    {"coordinates": ["-9223372036854775808", "0", "40"], "level": 0},
                    {"coordinates": ["-9223372036854775807", "0", "40"], "level": 0},
                ],
                "tick": "0",
                "caughtUp": false,
                "resident": false,
            },
        ],
        "cells": [{
            "cell": {"coordinates": ["0", "0", "0"], "level": 0},
            "tick": "3",
            "plants": 41,
            "canopy": 20000,
            "health": 65535,
            "moisture": 30000,
            "fuel": 40000,
        }],
    });
    assert_eq!(
        format_text("vegetation-ecology-status", &result),
        vec![
            "worldTick=8  ruleSet=2  regionRadius=1 cells  regions=2  checkpoint=001122334455",
            "  clock=stopped  tick=1000ms (+0ms pending)  budget=8 ticks/sync  workers=1  water=40000  warmth=45000  owed=5",
            "  region 0,0,0 L0                      tick=3         behind",
            "  region -9223372036854775808,0,40 L0 +1  tick=0         awaiting residency",
            "  cell   0,0,0 L0                      tick=3         plants=41     canopy=20000  health=65535  moisture=30000  fuel=40000",
        ]
    );
}
