//! The native `sa` control CLI: one blocking JSON-over-unix-socket round-trip.
//!
//! `sa <command> [args...] [-o text|json]` builds a request envelope, connects to the engine's
//! control socket, sends one `<json>\n` line, reads one reply line, prints the result, and exits
//! with the scriptable code contract (`0` ok, `1` runtime/engine error, `2` usage error).
//!
//! Links only `saffron-protocol` (the DTOs) and `saffron-control-client` (the one shared wire
//! client) — no renderer, no Jolt, no engine subsystem — so it runs on the host outside the build
//! toolbox. The framing lives in the shared client; the CLI owns only its argument coercion
//! (`build_params`) and its text formatters, so there is one wire implementation in the tree.

#![deny(unsafe_code)]

use std::io;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;
use saffron_control_client::{self as wire, Client};
use serde_json::{Map, Value};

/// The human-readable vs raw-JSON presentation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum OutputMode {
    /// One-line, command-keyed formatting (default); falls through to UTF-8-unescaped pretty JSON.
    Text,
    /// `serde_json` pretty JSON, for piping to `jq`.
    Json,
}

/// The `sa` argument surface: a global `-o/--output`, the two built-in subcommands (`start`,
/// `completions`), and a free-form `<command> [args...]` capture for every control command.
///
/// The control command is forwarded verbatim through the [`Subcmd::External`] arm — `sa` declares
/// no per-command subcommand, so a command is reachable the moment the engine registers it.
/// `disable_help_subcommand` keeps `sa help` an external (engine-forwarded) command rather than
/// clap's built-in help, because the running engine's live `help` reply is the authoritative list.
#[derive(Debug, Parser)]
#[command(
    name = "sa",
    about = "sa — Saffron Anima control CLI",
    long_about = "sa — Saffron Anima control CLI\n\n\
                  Sends one control command over the engine's unix socket and prints the reply.\n\
                  Run `sa help` against a running engine for the live command list.",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputMode::Text, global = true)]
    output: OutputMode,

    #[command(subcommand)]
    command: Option<Subcmd>,
}

/// The parsed top-level command: the two built-in launchers/affordances, or an external control
/// command forwarded to the engine over the socket.
#[derive(Debug, Subcommand)]
enum Subcmd {
    /// Launch the engine host in the toolbox, polling the socket for readiness.
    Start {
        /// Run the engine in the foreground instead of detaching it.
        #[arg(long)]
        attach: bool,
        /// Build the engine first (`cargo build --bin saffron-host`) before launching.
        #[arg(long)]
        build: bool,
    },
    /// Print a shell-completion script (sourced from the shared command table) to stdout.
    Completions {
        /// The shell to generate completions for.
        shell: Shell,
    },
    /// Export the loaded project as a platform-native application (the `export-app` command with a
    /// typed app manifest).
    Export {
        /// The destination path for the staged app.
        output_dir: String,
        /// The app + window title (default: "Saffron App").
        #[arg(long)]
        title: Option<String>,
        /// The window width in pixels (default: 1280).
        #[arg(long)]
        width: Option<u32>,
        /// The window height in pixels (default: 720).
        #[arg(long)]
        height: Option<u32>,
        /// Start the app fullscreen.
        #[arg(long)]
        fullscreen: bool,
        /// Present without vsync (vsync is on by default).
        #[arg(long)]
        no_vsync: bool,
    },
    /// Any control command name and its free-form arguments, forwarded to the engine verbatim.
    #[command(external_subcommand)]
    External(Vec<String>),
}

/// The derived clap [`clap::Command`] enriched with the offline command list from
/// [`saffron_protocol::COMMANDS`]: the long `--help` then lists every registered command name and
/// points at `sa help` for the live list, so the CLI is discoverable with no engine running. The
/// list derives from the one shared table, so it cannot drift from what the engine serves.
fn enriched_command() -> clap::Command {
    Cli::command().after_long_help(command_list_help())
}

/// The `available commands:` block appended to the long help: the [`saffron_protocol::COMMANDS`]
/// names in registration order, two-space indented, capped at the table, then the `sa help`
/// pointer to the live, authoritative list.
fn command_list_help() -> String {
    let mut text = String::from("available commands (static; run `sa help` for the live list):\n");
    for spec in saffron_protocol::COMMANDS {
        text.push_str("  ");
        text.push_str(spec.name);
        text.push('\n');
    }
    text.push_str("\nRun `sa help` against a running engine for command summaries.");
    text
}

/// A clap [`clap::Command`] shaped for completion generation: the parsing command with every
/// [`saffron_protocol::COMMANDS`] name attached as a candidate for the forwarded-command position,
/// so the generated script offers them. This command is *not* used for parsing — attaching the
/// names as possible values there would reject any other (forwardable) command — so completions
/// list the known commands while the engine still answers an unknown command's forward.
fn completion_command() -> clap::Command {
    let names: Vec<&'static str> = saffron_protocol::COMMANDS.iter().map(|c| c.name).collect();
    Cli::command().arg(
        clap::Arg::new("forwarded-command")
            .help("a control command to forward to the engine")
            .num_args(0..)
            .value_parser(clap::builder::PossibleValuesParser::new(names)),
    )
}

/// Generates the completion script for `shell` from [`completion_command`] and writes it to stdout.
fn generate_completions(shell: Shell) {
    let mut command = completion_command();
    clap_complete::generate(shell, &mut command, "sa", &mut io::stdout());
}

/// Maps a free-form arg list onto a params object: bare tokens become `params["args"]` (added only
/// when non-empty), and `--key value` / `--key=value` / a bare `--key` map to `params[key]`. Each
/// value is run through [`coerce`].
///
/// The walk is a two-step advance: a `--` flag splits on its first `=` for the `--key=value` form;
/// otherwise it consumes the next token as the value when that token is not itself a `--` flag, else
/// it is a bare `--key = true`.
fn build_params(args: &[String]) -> Value {
    let mut params = Map::new();
    let mut positional: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if let Some(key) = arg.strip_prefix("--") {
            if let Some((k, v)) = key.split_once('=') {
                params.insert(k.to_owned(), coerce(v));
                i += 1;
            } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                params.insert(key.to_owned(), coerce(&args[i + 1]));
                i += 2;
            } else {
                params.insert(key.to_owned(), Value::Bool(true));
                i += 1;
            }
        } else {
            positional.push(coerce(arg));
            i += 1;
        }
    }
    if !positional.is_empty() {
        params.insert("args".to_owned(), Value::Array(positional));
    }
    Value::Object(params)
}

/// Coerces one token to a JSON value by a fixed precedence ladder: `true`/`false`/`null` literals →
/// inline JSON when the token opens with `{`/`[`/`"` → unsigned integer (only if the token does not
/// open with `-`) → signed integer → float → bare string.
///
/// The ordering is load-bearing: unsigned-first keeps a large positive id (up to `u64::MAX`) an
/// unsigned number rather than lossily widening it to a float, and the `-` guard routes a negative
/// token straight to the signed parse. An inline-JSON parse failure falls through to the numeric
/// ladder, where it then fails every numeric parse and ends as the bare string. Each numeric parse
/// is whole-string (no trailing garbage).
fn coerce(token: &str) -> Value {
    match token {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if matches!(token.as_bytes().first(), Some(b'{' | b'[' | b'"')) {
        if let Ok(value) = serde_json::from_str::<Value>(token) {
            return value;
        }
    }
    if !token.starts_with('-') {
        if let Ok(unsigned) = token.parse::<u64>() {
            return Value::from(unsigned);
        }
    }
    if let Ok(signed) = token.parse::<i64>() {
        return Value::from(signed);
    }
    if let Ok(float) = token.parse::<f64>() {
        if let Some(number) = serde_json::Number::from_f64(float) {
            return Value::Number(number);
        }
    }
    Value::String(token.to_owned())
}

/// The transport outcome, mapped to an exit code by `main`.
enum Outcome {
    /// `ok: true` — the result was printed; exit 0.
    Ok,
    /// A runtime failure (connect/parse) or an `ok: false` engine error; exit 1.
    Error(String),
}

impl Outcome {
    fn code(&self) -> ExitCode {
        match self {
            Outcome::Ok => ExitCode::SUCCESS,
            Outcome::Error(msg) => {
                eprintln!("sa: {msg}");
                ExitCode::FAILURE
            }
        }
    }
}

/// Routes the shared client's call outcome to the printer or the error path: a decoded `result` is
/// printed in `mode`; an [`wire::Error`] becomes an `sa:`-prefixed message, gaining a nearest-name
/// `did you mean…?` hint when the command is absent from the shared table.
fn present_outcome(cmd: &str, outcome: wire::Result<Value>, mode: OutputMode) -> Outcome {
    match outcome {
        Ok(result) => {
            print_result(cmd, &result, mode);
            Outcome::Ok
        }
        Err(wire::Error::MalformedReply) => Outcome::Error("malformed reply".to_owned()),
        Err(wire::Error::Transport { path, source }) => {
            Outcome::Error(format!("cannot connect to {path}: {source}"))
        }
        Err(wire::Error::Decode { source, .. }) => {
            Outcome::Error(format!("malformed reply: {source}"))
        }
        Err(wire::Error::Engine { message, .. }) => {
            let mut message = message;
            // The CLI never gates a forward (an unknown command still reaches the engine, which
            // answers `unknown command '<name>'`); but when the command is absent from the shared
            // table, offer the nearest registered name as a hint, computed offline from `COMMANDS`.
            if !is_known_command(cmd) {
                if let Some(suggestion) = did_you_mean(cmd) {
                    message.push_str(&format!("  (did you mean '{suggestion}'?)"));
                }
            }
            Outcome::Error(message)
        }
    }
}

/// Whether `cmd` is a name in the shared [`saffron_protocol::COMMANDS`] table or the reflective
/// `help` builtin (which is not in the typed table but is a real command).
fn is_known_command(cmd: &str) -> bool {
    cmd == "help" || saffron_protocol::COMMANDS.iter().any(|c| c.name == cmd)
}

/// Returns the nearest [`saffron_protocol::COMMANDS`] name to `cmd` by Levenshtein distance, when
/// one is close enough to be a plausible typo. The threshold scales with the longer name (a third
/// of its length) but is floored at 2 so a single transposition in a short name (which plain
/// Levenshtein scores as 2) is still caught, and capped at 3 so a wholly different token suggests
/// nothing. A purely advisory hint — it never changes what is sent, only what an error suggests.
fn did_you_mean(cmd: &str) -> Option<&'static str> {
    let mut best: Option<(&'static str, usize)> = None;
    for spec in saffron_protocol::COMMANDS {
        let distance = levenshtein(cmd, spec.name);
        if best.is_none_or(|(_, d)| distance < d) {
            best = Some((spec.name, distance));
        }
    }
    let (name, distance) = best?;
    let threshold = (cmd.len().max(name.len()) / 3).clamp(2, 3);
    (distance <= threshold && distance > 0).then_some(name)
}

/// The Levenshtein edit distance between two strings (a two-row dynamic-programming table), used
/// only by [`did_you_mean`]'s nearest-name search.
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0_usize; b_chars.len() + 1];
    for (i, ac) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, &bc) in b_chars.iter().enumerate() {
            let cost = usize::from(ac != bc);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b_chars.len()]
}

/// Prints a successful result. `json` mode is pretty JSON; `text` mode runs the command-keyed
/// `match`, falling through to UTF-8-unescaped pretty JSON for any unmatched command.
///
/// A closed `match cmd` over the command name, each arm reading specific result fields with lenient
/// defaults via the `field_*` readers, so an arm never panics on a missing or mistyped field.
fn print_result(cmd: &str, result: &Value, mode: OutputMode) {
    if mode == OutputMode::Json {
        println!("{}", pretty(result));
        return;
    }
    for line in format_text(cmd, result) {
        println!("{line}");
    }
}

/// Renders the `text`-mode line(s) for a command. Returning a `Vec<String>` instead of writing to
/// stdout keeps every formatter a pure function of `&Value`, so each arm is directly
/// `#[test]`-coverable; the one side-effecting arm (`profiler.capture-stop`, which writes an inline
/// trace to a temp file) does its I/O here before returning its line(s).
fn format_text(cmd: &str, result: &Value) -> Vec<String> {
    match cmd {
        "help" if result.get("commands").is_some() => format_help(result),
        "ping" => vec![format!(
            "pong  engine={}  version={}  pid={}",
            field_str(result, "engine"),
            field_str(result, "version"),
            field_i64(result, "pid"),
        )],
        "list-entities" if result.get("entities").is_some() => format_list_entities(result),
        "list-components" if result.get("components").is_some() => {
            field_array(result, "components")
                .iter()
                .map(|c| format!("  {}", c.as_str().unwrap_or("")))
                .collect()
        }
        "list-assets" if result.get("assets").is_some() => format_list_assets(result),
        "get-asset-model" => format_asset_model(result),
        "project-status" | "cancel-load" | "new-project" | "open-project" | "load-project"
        | "reload-project" => vec![format_project_status(result)],
        "list-clips" if result.get("clips").is_some() => field_array(result, "clips")
            .iter()
            .map(|c| format_clip(c, "", 32))
            .collect(),
        "render-stats" => vec![format_render_stats(result)],
        "profiler.set-mode" => vec![format!(
            "mode={}  timestamps={}  pipeline-stats={}{}",
            field_str(result, "mode"),
            yes_no(field_bool(result, "timestampsSupported")),
            yes_no(field_bool(result, "pipelineStatsSupported")),
            software_gpu_suffix(result),
        )],
        "pass-timings" => format_pass_timings(result),
        "profiler.capture-start" => vec![format!(
            "armed capture id={}  (stop with: sa profiler.capture-stop)",
            field_u64(result, "captureId"),
        )],
        "profiler.capture-stop" => format_capture_stop(result),
        "frame-history" => vec![format_frame_history(result)],
        "get-perf-config" | "set-perf-config" => vec![format_perf_config(result)],
        "drain-alarms" => format_drain_alarms(result),
        "list-active-alarms" => format_active_alarms(result),
        "play" | "pause" | "stop" | "step" | "get-play-state" => vec![format!(
            "state={}  playVersion={}  sceneVersion={}  camera={}",
            field_str(result, "state"),
            field_i64(result, "playVersion"),
            field_i64(result, "sceneVersion"),
            if field_bool(result, "hasPrimaryCamera") {
                "ok"
            } else {
                "missing"
            },
        )],
        "physics-state" => vec![format!(
            "physics={}  bodies={}  dynamic={}",
            if field_bool(result, "active") {
                "active"
            } else {
                "inactive"
            },
            field_i64(result, "bodyCount"),
            field_i64(result, "dynamicCount"),
        )],
        "fit-collider" => vec![format_fit_collider(result)],
        "raycast" | "shapecast" => vec![format_raycast(result)],
        "enable-ragdoll" | "set-ragdoll" | "get-ragdoll" => vec![format!(
            "ragdoll={}  active={}  bodyWeight={:.2}  bones={}",
            if field_bool(result, "present") {
                "present"
            } else {
                "none"
            },
            yes_no(field_bool(result, "active")),
            field_f64(result, "bodyWeight"),
            field_i64(result, "bones"),
        )],
        "move-character" => {
            let p = result.get("position");
            vec![format!(
                "position=({:.3}, {:.3}, {:.3})  onGround={}",
                vec_component(p, "x"),
                vec_component(p, "y"),
                vec_component(p, "z"),
                yes_no(field_bool(result, "onGround")),
            )]
        }
        "set-kinematic-bones" => vec![format!(
            "kinematic-bones={}  entity={}  bones={}",
            if field_bool(result, "enabled") {
                "on"
            } else {
                "off"
            },
            field_str(result, "entity"),
            field_i64(result, "boneCount"),
        )],
        "drain-contacts" => format_drain_contacts(result),
        "viewport-native-info" => vec![format!(
            "{}  {}  {}x{}  sock={}",
            field_str(result, "status"),
            field_str(result, "transport"),
            field_u64(result, "width"),
            field_u64(result, "height"),
            field_str(result, "controlSocket"),
        )],
        "set-active-view" => vec![format!("view={}", field_str(result, "view"))],
        "get-selection" => vec![format_selection(result)],
        "add-entity" | "copy-entity" => vec![format!(
            "{}  id={}",
            field_str(result, "name"),
            field_str(result, "id"),
        )],
        "get-gizmo" | "set-gizmo" => vec![format!(
            "op={}  space={}",
            field_str(result, "op"),
            field_str(result, "space"),
        )],
        "gizmo-pointer" => vec![format!(
            "hovered={}  dragging={}",
            result
                .get("hovered")
                .and_then(Value::as_str)
                .unwrap_or("none"),
            yes_no(field_bool(result, "dragging")),
        )],
        "pick" => vec![if field_bool(result, "hit") {
            format!(
                "{}  {}  id={}",
                field_str(result, "kind"),
                field_str(result, "name"),
                field_str(result, "id"),
            )
        } else {
            "no hit".to_owned()
        }],
        "pick-skeleton-joint" => vec![if field_bool(result, "found") {
            format!(
                "joint node={}",
                result
                    .get("nodeIndex")
                    .and_then(Value::as_i64)
                    .unwrap_or(-1),
            )
        } else {
            "no joint".to_owned()
        }],
        "get-camera" | "set-camera" => {
            let p = result.get("position");
            vec![format!(
                "pos=({:.2}, {:.2}, {:.2})  yaw={:.1}  pitch={:.1}  fov={:.1}",
                vec_component(p, "x"),
                vec_component(p, "y"),
                vec_component(p, "z"),
                field_f64(result, "yaw"),
                field_f64(result, "pitch"),
                field_f64(result, "fov"),
            )]
        }
        "get-thumbnail" | "view-asset" => {
            let b64_len = field_str(result, "base64").len();
            vec![format!(
                "{} {}x{}  ~{} bytes (base64 {} chars)",
                field_str(result, "format"),
                field_u64(result, "width"),
                field_u64(result, "height"),
                (b64_len / 4) * 3,
                b64_len,
            )]
        }
        _ => vec![pretty(result)],
    }
}

/// The `help` two-column table: each command name left-padded to 22 columns, then its summary, both
/// indented two spaces.
fn format_help(result: &Value) -> Vec<String> {
    field_array(result, "commands")
        .iter()
        .map(|entry| {
            format!(
                "  {:<22}  {}",
                field_str(entry, "name"),
                field_str(entry, "help"),
            )
        })
        .collect()
}

/// The `list-entities` table: id, name, parent id, each in a 24-wide column.
fn format_list_entities(result: &Value) -> Vec<String> {
    field_array(result, "entities")
        .iter()
        .map(|e| {
            format!(
                "  {:<24}  {:<24}  {}",
                field_str(e, "id"),
                field_str(e, "name"),
                field_str(e, "parentId"),
            )
        })
        .collect()
}

/// The `list-assets` table: type (8 wide), name (32 wide), id.
fn format_list_assets(result: &Value) -> Vec<String> {
    field_array(result, "assets")
        .iter()
        .map(|a| {
            format!(
                "  {:<8}  {:<32}  {}",
                field_str(a, "type"),
                field_str(a, "name"),
                field_str(a, "id"),
            )
        })
        .collect()
}

/// The `get-asset-model` report: header, capability counts, the indented bone tree, then the clip
/// list. The bone indent walks each bone's `parent` chain with a 256-iteration cycle guard so a
/// malformed/cyclic parent index cannot hang.
fn format_asset_model(result: &Value) -> Vec<String> {
    let mut lines = vec![format!(
        "model {}  (mesh {})",
        field_str(result, "name"),
        field_str(result, "mesh"),
    )];
    let caps = result.get("capabilities");
    lines.push(format!(
        "  meshes={}  materials={}  nodes={}  rig={}  bones={}  clips={}",
        nested_i64(caps, "meshCount"),
        nested_i64(caps, "materialCount"),
        nested_i64(caps, "nodeCount"),
        yes_no(nested_bool(caps, "hasRig")),
        nested_i64(caps, "boneCount"),
        nested_i64(caps, "clipCount"),
    ));
    let bones = field_array(result, "bones");
    for bone in &bones {
        let mut depth = 0_usize;
        let mut parent = bone.get("parent").and_then(Value::as_i64).unwrap_or(-1);
        let mut guard = 0;
        while parent >= 0 && (parent as usize) < bones.len() && guard < 256 {
            depth += 1;
            parent = bones[parent as usize]
                .get("parent")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            guard += 1;
        }
        lines.push(format!(
            "  {:indent$}{}{}",
            "",
            field_str(bone, "name"),
            if field_bool(bone, "joint") {
                "  [joint]"
            } else {
                ""
            },
            indent = depth * 2,
        ));
    }
    for clip in field_array(result, "clips") {
        lines.push(format_clip(&clip, "clip ", 28));
    }
    lines
}

/// One clip line shared by `list-clips` (no prefix, name width 32) and `get-asset-model` (a `clip `
/// prefix, name width 28): `[prefix]name  duration.s  id`.
fn format_clip(clip: &Value, prefix: &str, name_width: usize) -> String {
    format!(
        "  {prefix}{:<name_width$}  {:>8.3}s  {}",
        field_str(clip, "name"),
        field_f64(clip, "duration"),
        field_str(clip, "id"),
    )
}

/// The `render-stats` one-liner.
fn format_render_stats(result: &Value) -> String {
    format!(
        "cpu={:.2}ms  gpu={:.2}ms  wait={:.2}ms  fps={:.0}  draws={}  tris={}  binds={}  pso+={}{}",
        field_f64(result, "cpuFrameMs"),
        field_f64(result, "gpuFrameMs"),
        field_f64(result, "cpuWaitMs"),
        field_f64(result, "fps"),
        field_i64(result, "drawCalls"),
        field_i64(result, "triangles"),
        field_i64(result, "descriptorBinds"),
        field_i64(result, "pipelinesCreated"),
        software_gpu_suffix(result),
    )
}

/// The `pass-timings` table: an optional software-gpu note, one line per pass, then the span total.
fn format_pass_timings(result: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if field_bool(result, "softwareGpu") {
        lines.push("[software-gpu: timings are CPU rasterization time, not hardware]".to_owned());
    }
    for pass in field_array(result, "passes") {
        lines.push(format!(
            "  {:<28}  {:>8.3} ms",
            field_str(&pass, "name"),
            field_f64(&pass, "gpuMs"),
        ));
    }
    lines.push(format!(
        "  {:<28}  {:>8.3} ms",
        "total (span)",
        field_f64(result, "gpuTotalMs"),
    ));
    lines
}

/// The `profiler.capture-stop` report: frame/span counts and a trace path. When the reply carries no
/// `path` but an inline `chromeTrace` string, that trace is written to
/// `<temp_dir>/saffron-profile.json` and that path is printed.
fn format_capture_stop(result: &Value) -> Vec<String> {
    if !field_bool(result, "ready") {
        return vec!["no capture ready (arm one with: sa profiler.capture-start)".to_owned()];
    }
    let capture = result.get("capture");
    let meta = capture.and_then(|c| c.get("metadata"));
    let span_count = capture
        .and_then(|c| c.get("spans"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut lines = vec![format!(
        "captured {} frame(s), {} spans  [{}{}]",
        field_u64(result, "frameCount"),
        span_count,
        if nested_bool(meta, "correlated") {
            "correlated"
        } else {
            "uncorrelated"
        },
        if nested_bool(meta, "softwareGpu") {
            ", software-gpu"
        } else {
            ""
        },
    )];
    let mut path = field_str(result, "path").to_owned();
    if path.is_empty() {
        let trace = field_str(result, "chromeTrace");
        if !trace.is_empty() {
            let target = std::env::temp_dir().join("saffron-profile.json");
            if std::fs::write(&target, trace).is_ok() {
                path = target.to_string_lossy().into_owned();
            }
        }
    }
    if !path.is_empty() {
        lines.push(format!(
            "trace: {path}  (open in chrome://tracing or ui.perfetto.dev)"
        ));
    }
    lines
}

/// The `frame-history` percentiles line.
fn format_frame_history(result: &Value) -> String {
    format!(
        "p50={:.2}  p95={:.2}  p99={:.2}  p99.9={:.2}  max={:.2}  stddev={:.2}  budget={:.2}ms  stutters={}  n={}",
        field_f64(result, "p50Ms"),
        field_f64(result, "p95Ms"),
        field_f64(result, "p99Ms"),
        field_f64(result, "p999Ms"),
        field_f64(result, "maxMs"),
        field_f64(result, "stddevMs"),
        field_f64(result, "budgetMs"),
        field_i64(result, "stutterCount"),
        field_i64(result, "sampleCount"),
    )
}

/// The `get-perf-config`/`set-perf-config` line; the vram fractions print as percentages.
fn format_perf_config(result: &Value) -> String {
    format!(
        "targetFps={:.0}  budget={:.2}ms  green<{:.2}×budget  amber<{:.1}×median  frozen={:.0}ms  vram warn/crit={:.0}%/{:.0}%",
        field_f64(result, "targetFps"),
        field_f64(result, "budgetMs"),
        field_f64(result, "greenBudgetFrac"),
        field_f64(result, "amberMedianMul"),
        field_f64(result, "frozenMs"),
        field_f64(result, "vramWarnFrac") * 100.0,
        field_f64(result, "vramCritFrac") * 100.0,
    )
}

/// The `drain-alarms` table: one line per event, then a summary footer.
fn format_drain_alarms(result: &Value) -> Vec<String> {
    let events = field_array(result, "events");
    let mut lines: Vec<String> = events
        .iter()
        .map(|e| {
            format!(
                "  #{:<5}  {:<8} {:<9} {:<13}  {:>8.2} / {:<8.2}  x{}",
                field_i64(e, "seq"),
                field_str(e, "state"),
                field_str(e, "severity"),
                field_str(e, "metric"),
                field_f64(e, "value"),
                field_f64(e, "threshold"),
                field_i64(e, "count"),
            )
        })
        .collect();
    lines.push(format!(
        "  high={}  oldest={}  overflowed={}  ({} events)",
        field_i64(result, "highWaterSeq"),
        field_i64(result, "oldestSeq"),
        yes_no(field_bool(result, "overflowed")),
        events.len(),
    ));
    lines
}

/// The `list-active-alarms` table: a "no active alarms" line when empty, else one line per alarm
/// with an optional `pass=` suffix.
fn format_active_alarms(result: &Value) -> Vec<String> {
    let alarms = field_array(result, "alarms");
    if alarms.is_empty() {
        return vec!["no active alarms".to_owned()];
    }
    alarms
        .iter()
        .map(|a| {
            let pass = field_str(a, "pass");
            format!(
                "  {:<9} {:<13}  {:>8.2} / {:<8.2}  x{}{}{}",
                field_str(a, "severity"),
                field_str(a, "metric"),
                field_f64(a, "value"),
                field_f64(a, "threshold"),
                field_i64(a, "count"),
                if pass.is_empty() { "" } else { "  pass=" },
                pass,
            )
        })
        .collect()
}

/// The `fit-collider` line: the fitted shape, entity, half-extents, and offset.
fn format_fit_collider(result: &Value) -> String {
    let he = result.get("halfExtents");
    let off = result.get("offset");
    format!(
        "fitted {}  entity={}  halfExtents=({:.3}, {:.3}, {:.3})  offset=({:.3}, {:.3}, {:.3})",
        field_str(result, "shape"),
        field_str(result, "entity"),
        vec_component(he, "x"),
        vec_component(he, "y"),
        vec_component(he, "z"),
        vec_component(off, "x"),
        vec_component(off, "y"),
        vec_component(off, "z"),
    )
}

/// The `raycast`/`shapecast` line: the hit detail or `no hit`.
fn format_raycast(result: &Value) -> String {
    if !field_bool(result, "hit") {
        return "no hit".to_owned();
    }
    let p = result.get("point");
    let n = result.get("normal");
    format!(
        "hit entity={}  point=({:.3}, {:.3}, {:.3})  normal=({:.2}, {:.2}, {:.2})  dist={:.3}",
        field_str(result, "entity"),
        vec_component(p, "x"),
        vec_component(p, "y"),
        vec_component(p, "z"),
        vec_component(n, "x"),
        vec_component(n, "y"),
        vec_component(n, "z"),
        field_f64(result, "distance"),
    )
}

/// The `drain-contacts` table: one line per event, then a summary footer.
fn format_drain_contacts(result: &Value) -> Vec<String> {
    let events = field_array(result, "events");
    let mut lines: Vec<String> = events
        .iter()
        .map(|e| {
            format!(
                "  #{:<5}  {:<6} {:<6}  {} <-> {}",
                field_i64(e, "seq"),
                field_str(e, "kind"),
                if field_bool(e, "sensor") {
                    "sensor"
                } else {
                    "solid"
                },
                field_str(e, "entityA"),
                field_str(e, "entityB"),
            )
        })
        .collect();
    lines.push(format!(
        "  high={}  oldest={}  overflowed={}  ({} events)",
        field_i64(result, "highWaterSeq"),
        field_i64(result, "oldestSeq"),
        yes_no(field_bool(result, "overflowed")),
        events.len(),
    ));
    lines
}

/// The `project-status` line: the project name, phase, boot stage, `n/m` when determinate, and the
/// human label — or the error on a failed load.
fn format_project_status(result: &Value) -> String {
    let name = field_str(result, "name");
    let phase = field_str(result, "phase");
    let error = field_str(result, "error");
    if !error.is_empty() {
        return format!("{name}  {phase}  error: {error}");
    }
    let stage = field_str(result, "stage");
    let label = field_str(result, "label");
    let (done, total) = (field_i64(result, "done"), field_i64(result, "total"));
    let progress = if total > 0 {
        format!("  {done}/{total}")
    } else {
        String::new()
    };
    format!("{name}  {phase}  [{stage}]{progress}  {label}")
}

/// The `get-selection` line: the selected entity name, or "no selection", with the selection/scene
/// versions.
fn format_selection(result: &Value) -> String {
    let sel_version = field_u64(result, "selectionVersion");
    let scene_version = field_u64(result, "sceneVersion");
    match result.get("entity") {
        Some(entity) if entity.is_object() => format!(
            "selected: {}  (sel v{sel_version}, scene v{scene_version})",
            field_str(entity, "name"),
        ),
        _ => format!("no selection  (sel v{sel_version}, scene v{scene_version})"),
    }
}

/// Pretty-prints a `Value` with UTF-8 left unescaped (`serde_json`'s default) so non-ASCII renders
/// literally.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `"yes"`/`"no"` for a bool flag.
fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

/// The `  [software-gpu]` suffix appended when the `softwareGpu` flag is set, else empty.
fn software_gpu_suffix(result: &Value) -> &'static str {
    if field_bool(result, "softwareGpu") {
        "  [software-gpu]"
    } else {
        ""
    }
}

/// Reads a string field, defaulting to `""`.
fn field_str<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Reads an integer field, defaulting to `0`.
fn field_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// Reads an unsigned field, defaulting to `0`.
fn field_u64(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Reads a float field, defaulting to `0.0`.
fn field_f64(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// Reads a boolean field, defaulting to `false`.
fn field_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Reads an array field, defaulting to an empty slice (cloned into an owned `Vec` so the caller can
/// index it for the bone-tree walk).
fn field_array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Reads an integer field from an optional nested object, defaulting to `0`.
fn nested_i64(parent: Option<&Value>, key: &str) -> i64 {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// Reads a boolean field from an optional nested object.
fn nested_bool(parent: Option<&Value>, key: &str) -> bool {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Reads one component (`x`/`y`/`z`) of an optional vector object, defaulting to `0.0`.
fn vec_component(parent: Option<&Value>, key: &str) -> f64 {
    parent
        .and_then(|p| p.get(key))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

/// Forwards one control command to the engine over the socket: build the envelope, round-trip,
/// and print the reply. The single non-`start`/`completions` path.
/// Builds the `export-app` params from the typed `export` flags and forwards the command. The
/// `app` manifest is partial — omitted fields fall back to the engine's [`AppManifest`] defaults
/// (`#[serde(default)]`), so `sa export <dir>` exports with the default window + title.
#[allow(clippy::fn_params_excessive_bools)] // the two bools are independent CLI flags, not a mode.
fn export(
    output_dir: String,
    title: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    fullscreen: bool,
    no_vsync: bool,
    mode: OutputMode,
) -> Outcome {
    let mut app = Map::new();
    if let Some(title) = title {
        app.insert("title".to_owned(), Value::String(title));
    }
    if let Some(width) = width {
        app.insert("width".to_owned(), Value::from(width));
    }
    if let Some(height) = height {
        app.insert("height".to_owned(), Value::from(height));
    }
    if fullscreen {
        app.insert("fullscreen".to_owned(), Value::Bool(true));
    }
    if no_vsync {
        app.insert("vsync".to_owned(), Value::Bool(false));
    }
    let mut params = Map::new();
    params.insert("outputDir".to_owned(), Value::String(output_dir));
    params.insert("app".to_owned(), Value::Object(app));
    let mut client = Client::from_env();
    present_outcome(
        "export-app",
        client.call_raw("export-app", Value::Object(params)),
        mode,
    )
}

fn forward(tokens: &[String], mode: OutputMode) -> Outcome {
    let Some((cmd, args)) = tokens.split_first() else {
        // The `external_subcommand` arm only matches with at least one token, so this is
        // unreachable; a truly missing command is the `None` arm handled in `main`.
        return Outcome::Error("missing command".to_owned());
    };
    let params = build_params(args);
    let mut client = Client::from_env();
    present_outcome(cmd, client.call_raw(cmd, params), mode)
}

/// The toolbox container the engine host runs in, and the `cargo build` target / output path for
/// the present-only host that `start` consumes.
const TOOLBOX_NAME: &str = "saffron-build";
const ENGINE_BIN_TARGET: &str = "saffron-host";

/// The launch outcome of [`start`], mapped to a process exit code. `start` never goes through
/// [`Outcome`] because it is not a socket round-trip — it owns its own readiness reporting.
enum StartOutcome {
    /// The engine is up (already running, or launched and the socket appeared); exit 0.
    Ready(String),
    /// A build/launch failure; the message is printed to stderr, exit 1.
    Failed(String),
}

impl StartOutcome {
    fn code(&self) -> ExitCode {
        match self {
            StartOutcome::Ready(message) => {
                println!("sa: {message}");
                ExitCode::SUCCESS
            }
            StartOutcome::Failed(message) => {
                eprintln!("sa: {message}");
                ExitCode::FAILURE
            }
        }
    }
}

/// The `start` launcher: optionally build the engine, skip if it is already up (unlinking a stale
/// socket), then launch the host inside the toolbox — detached by default, foreground under
/// `--attach` — and poll the socket for readiness.
fn start(attach: bool, build: bool) -> StartOutcome {
    if build {
        println!("sa: building…");
        if let Err(message) = run_engine_build() {
            return StartOutcome::Failed(message);
        }
    }

    let path = wire::socket_path();
    if engine_running(&path) {
        return StartOutcome::Ready("engine already running".to_owned());
    }

    let engine_bin = engine_binary_path();
    if !Path::new(&engine_bin).exists() {
        return StartOutcome::Failed(format!(
            "engine binary not found: {engine_bin}\nsa: hint: sa start --build"
        ));
    }

    let mut command = Command::new("toolbox");
    command.args(["run", "-c", TOOLBOX_NAME, &engine_bin]);
    if attach {
        // Foreground: hand the terminal to the engine and surface its exit code directly.
        match command.status() {
            Ok(status) => StartOutcome::Ready(format!("engine exited ({status})")),
            Err(err) => StartOutcome::Failed(format!("failed to launch engine: {err}")),
        }
    } else {
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        match command.spawn() {
            Ok(_) => poll_for_readiness(&path),
            Err(err) => StartOutcome::Failed(format!("failed to launch engine: {err}")),
        }
    }
}

/// Runs the engine build inside the toolbox (`cargo build --bin saffron-host`).
fn run_engine_build() -> Result<(), String> {
    let status = Command::new("toolbox")
        .args([
            "run",
            "-c",
            TOOLBOX_NAME,
            "cargo",
            "build",
            "--bin",
            ENGINE_BIN_TARGET,
        ])
        .status()
        .map_err(|err| format!("failed to run build: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("build failed ({status})"))
    }
}

/// The `target/<profile>/saffron-host` path the workspace build produces, resolved relative to this
/// `sa` binary's own location (both are workspace targets under the same `target/<profile>/` dir),
/// overridable by `SAFFRON_ANIMA_BIN` (the parallel-binary knob the editor and e2e honor).
fn engine_binary_path() -> String {
    if let Ok(override_bin) = std::env::var("SAFFRON_ANIMA_BIN") {
        return override_bin;
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        return dir.join(ENGINE_BIN_TARGET).to_string_lossy().into_owned();
    }
    ENGINE_BIN_TARGET.to_owned()
}

/// Whether the engine is up: ask the shared client; if the path exists but refuses the connection
/// it is a stale socket, which is unlinked so a fresh launch can re-bind.
fn engine_running(path: &str) -> bool {
    if Client::new(path).is_up() {
        return true;
    }
    if Path::new(path).exists() {
        let _ = std::fs::remove_file(path);
    }
    false
}

/// Polls the socket for up to ~5s after a detached launch (20 × 250ms), reporting readiness when a
/// connection succeeds.
fn poll_for_readiness(path: &str) -> StartOutcome {
    let client = Client::new(path);
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(250));
        if client.is_up() {
            return StartOutcome::Ready("engine started".to_owned());
        }
    }
    // Not a failure: the engine may still be initialising, so this exits 0.
    StartOutcome::Ready(
        "engine launched but socket not yet ready — it may still be initialising".to_owned(),
    )
}

fn main() -> ExitCode {
    let matches = enriched_command().get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };
    match cli.command {
        None => {
            eprintln!("sa: missing command");
            ExitCode::from(2)
        }
        Some(Subcmd::Completions { shell }) => {
            generate_completions(shell);
            ExitCode::SUCCESS
        }
        Some(Subcmd::Start { attach, build }) => start(attach, build).code(),
        Some(Subcmd::Export {
            output_dir,
            title,
            width,
            height,
            fullscreen,
            no_vsync,
        }) => export(
            output_dir, title, width, height, fullscreen, no_vsync, cli.output,
        )
        .code(),
        Some(Subcmd::External(tokens)) => forward(&tokens, cli.output).code(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            message: message.to_owned(),
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
        // The engine's `error` string is carried verbatim into the outcome (an unknown command may
        // additionally gain a `did you mean` hint, covered by its own test — assert the prefix).
        match present_outcome(
            "nope",
            engine_err("nope", "unknown command 'nope'"),
            OutputMode::Text,
        ) {
            Outcome::Error(msg) => assert!(msg.starts_with("unknown command 'nope'")),
            Outcome::Ok => panic!("expected an error outcome"),
        }
    }

    #[test]
    fn malformed_reply_is_error() {
        match present_outcome("ping", Err(wire::Error::MalformedReply), OutputMode::Text) {
            Outcome::Error(msg) => assert_eq!(msg, "malformed reply"),
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
            Outcome::Error(msg) => {
                assert!(msg.starts_with("unknown command 'pign'"));
                assert!(msg.contains("did you mean 'ping'?"), "got: {msg}");
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
            Outcome::Error(msg) => assert_eq!(msg, "no primary camera"),
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
            "softwareGpu": true,
        });
        assert_eq!(
            format_text("render-stats", &result),
            vec![
                "cpu=4.50ms  gpu=3.25ms  wait=0.50ms  fps=144  draws=120  tris=50000  binds=12  pso+=2  [software-gpu]"
            ]
        );
    }

    #[test]
    fn format_render_stats_omits_software_suffix() {
        let result = json!({"fps": 60.0});
        let lines = format_text("render-stats", &result);
        assert!(lines[0].starts_with("cpu=0.00ms"));
        assert!(!lines[0].contains("software-gpu"));
        assert!(lines[0].ends_with("pso+=0"));
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
            "hit": true, "entity": "42",
            "point": {"x": 1.0, "y": 2.0, "z": 3.0},
            "normal": {"x": 0.0, "y": 1.0, "z": 0.0},
            "distance": 5.5,
        });
        assert_eq!(
            format_text("raycast", &result),
            vec![
                "hit entity=42  point=(1.000, 2.000, 3.000)  normal=(0.00, 1.00, 0.00)  dist=5.500"
            ]
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
}
