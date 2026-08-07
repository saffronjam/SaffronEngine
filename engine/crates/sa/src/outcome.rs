//! Reply presentation and the process exit-code contract.

use std::process::ExitCode;

use saffron_control_client as wire;
use saffron_protocol::ControlFailureDto;
use serde_json::Value;

use crate::OutputMode;
use crate::format::{format_text, pretty};

pub(crate) enum Outcome {
    /// `ok: true` — the result was printed; exit 0.
    Ok,
    /// A runtime failure (connect/parse) or an `ok: false` engine error; exit 1.
    Error {
        failure: ControlFailureDto,
        text: String,
        mode: OutputMode,
    },
}

impl Outcome {
    pub(crate) fn code(&self) -> ExitCode {
        match self {
            Outcome::Ok => ExitCode::SUCCESS,
            Outcome::Error {
                failure,
                text,
                mode,
            } => {
                match mode {
                    OutputMode::Text => eprintln!("sa: {text}"),
                    OutputMode::Json => {
                        let json = serde_json::to_string_pretty(failure)
                            .expect("ControlFailureDto serializes");
                        eprintln!("{json}");
                    }
                }
                ExitCode::FAILURE
            }
        }
    }
}

/// Routes the shared client's call outcome to the printer or the error path: a decoded `result` is
/// printed in `mode`; an [`wire::Error`] becomes an `sa:`-prefixed message, gaining a nearest-name
/// `did you mean…?` hint when the command is absent from the shared table.
pub(crate) fn present_outcome(
    cmd: &str,
    outcome: wire::Result<Value>,
    mode: OutputMode,
) -> Outcome {
    match outcome {
        Ok(result) => {
            print_result(cmd, &result, mode);
            Outcome::Ok
        }
        Err(wire::Error::MalformedReply) => error_outcome(
            ControlFailureDto::MalformedReply {
                message: "malformed reply".to_owned(),
            },
            mode,
        ),
        Err(wire::Error::Transport { path, source }) => error_outcome(
            ControlFailureDto::Transport {
                message: format!("cannot connect to {path}: {source}"),
            },
            mode,
        ),
        Err(wire::Error::Decode { source, .. }) => error_outcome(
            ControlFailureDto::MalformedReply {
                message: format!("malformed reply: {source}"),
            },
            mode,
        ),
        Err(wire::Error::Engine { failure, .. }) => {
            let mut text = failure.message().to_owned();
            // The CLI never gates a forward (an unknown command still reaches the engine, which
            // answers `unknown command '<name>'`); but when the command is absent from the shared
            // table, offer the nearest registered name as a hint, computed offline from `COMMANDS`.
            if !is_known_command(cmd)
                && let Some(suggestion) = did_you_mean(cmd)
            {
                text.push_str(&format!("  (did you mean '{suggestion}'?)"));
            }
            Outcome::Error {
                failure: *failure,
                text,
                mode,
            }
        }
    }
}

pub(crate) fn error_outcome(failure: ControlFailureDto, mode: OutputMode) -> Outcome {
    let text = failure.message().to_owned();
    Outcome::Error {
        failure,
        text,
        mode,
    }
}

/// Whether `cmd` is a name in the shared [`saffron_protocol::COMMANDS`] table or the reflective
/// `help` builtin (which is not in the typed table but is a real command).
pub(crate) fn is_known_command(cmd: &str) -> bool {
    cmd == "help" || saffron_protocol::COMMANDS.iter().any(|c| c.name == cmd)
}

/// The nearest [`saffron_protocol::COMMANDS`] name to `cmd` by Levenshtein distance, when one is
/// close enough to be a plausible typo. The threshold is a third of the longer name, floored at 2 so
/// a single transposition in a short name still matches, and capped at 3.
pub(crate) fn did_you_mean(cmd: &str) -> Option<&'static str> {
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
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
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

/// Prints a successful result: pretty JSON in `json` mode, the command-keyed formatter in `text`
/// mode, falling through to UTF-8-unescaped pretty JSON for an unmatched command.
pub(crate) fn print_result(cmd: &str, result: &Value, mode: OutputMode) {
    if mode == OutputMode::Json {
        println!("{}", pretty(result));
        return;
    }
    for line in format_text(cmd, result) {
        println!("{line}");
    }
}
