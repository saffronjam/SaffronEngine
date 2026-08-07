//! The native `sa` control CLI: one blocking JSON-over-unix-socket round-trip.
//!
//! `sa <command> [args...] [-o text|json]` builds a request envelope, round-trips it against the
//! engine's control socket, prints the result, and exits with the scriptable code contract (`0` ok,
//! `1` runtime or engine error, `2` usage error).
//!
//! Links only `saffron-protocol` and `saffron-control-client`, so it runs on the host outside the
//! build toolbox. The framing lives in the shared client; the CLI owns argument coercion and the
//! text formatters.

#![deny(unsafe_code)]

use std::io;
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;
use saffron_control_client::Client;
use saffron_protocol::ControlFailureDto;
use serde_json::{Map, Value};

mod format;
mod outcome;
mod start;
#[cfg(test)]
mod tests;

use crate::outcome::{Outcome, error_outcome, present_outcome};
use crate::start::start;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OutputMode {
    /// One-line, command-keyed formatting (default); falls through to UTF-8-unescaped pretty JSON.
    Text,
    /// `serde_json` pretty JSON, for piping to `jq`.
    Json,
}

/// The `sa` argument surface: a global `-o/--output`, the built-in subcommands, and a free-form
/// `<command> [args...]` capture forwarded verbatim through [`Subcmd::External`], so a command is
/// reachable the moment the engine registers it. `disable_help_subcommand` keeps `sa help` an
/// engine-forwarded command, because the live reply is the authoritative list.
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

/// The derived clap command, with the offline [`saffron_protocol::COMMANDS`] list appended to the
/// long `--help` so the CLI is discoverable with no engine running.
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

/// A clap command shaped for completion generation, with every [`saffron_protocol::COMMANDS`] name
/// attached as a candidate for the forwarded position. It is never used for parsing: possible values
/// there would reject any other forwardable command.
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

/// Maps a free-form arg list onto a params object: bare tokens become `params["args"]`, and
/// `--key value` / `--key=value` / a bare `--key` map to `params[key]`, each value run through
/// [`coerce`]. A bare `--key` whose next token is another flag is `true`.
pub(crate) fn build_params(args: &[String]) -> Value {
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

/// Coerces one token to a JSON value by a fixed precedence ladder: `true`/`false`/`null`, then
/// inline JSON when the token opens with `{`/`[`/`"`, then unsigned integer (unless it opens with
/// `-`), then signed integer, then float, then the bare string.
///
/// The order is load-bearing: unsigned first keeps an id up to `u64::MAX` exact instead of widening
/// it to a float, and every numeric parse is whole-string.
pub(crate) fn coerce(token: &str) -> Value {
    match token {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        "null" => return Value::Null,
        _ => {}
    }
    if matches!(token.as_bytes().first(), Some(b'{' | b'[' | b'"'))
        && let Ok(value) = serde_json::from_str::<Value>(token)
    {
        return value;
    }
    if !token.starts_with('-')
        && let Ok(unsigned) = token.parse::<u64>()
    {
        return Value::from(unsigned);
    }
    if let Ok(signed) = token.parse::<i64>() {
        return Value::from(signed);
    }
    if let Ok(float) = token.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(float)
    {
        return Value::Number(number);
    }
    Value::String(token.to_owned())
}

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

/// Forwards one control command to the engine over the socket: build the envelope, round-trip, and
/// print the reply.
fn forward(tokens: &[String], mode: OutputMode) -> Outcome {
    let Some((cmd, args)) = tokens.split_first() else {
        // The `external_subcommand` arm only matches with at least one token, so this is
        // unreachable; a truly missing command is the `None` arm handled in `main`.
        return error_outcome(
            ControlFailureDto::Bridge {
                message: "missing command".to_owned(),
            },
            mode,
        );
    };
    let params = build_params(args);
    let mut client = Client::from_env();
    present_outcome(cmd, client.call_raw(cmd, params), mode)
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
