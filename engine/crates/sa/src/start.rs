//! The `sa start` launcher: build, spawn, and wait for the host's control socket.

use std::path::Path;
use std::process::ExitCode;
use std::process::{Command, Stdio};
use std::time::Duration;

use saffron_control_client::{self as wire, Client};

pub(crate) const TOOLBOX_NAME: &str = "saffron-build";
pub(crate) const ENGINE_BIN_TARGET: &str = "saffron-host";

/// The launch outcome of [`start`], mapped to a process exit code. `start` never goes through
/// [`Outcome`] because it is not a socket round-trip — it owns its own readiness reporting.
pub(crate) enum StartOutcome {
    /// The engine is up (already running, or launched and the socket appeared); exit 0.
    Ready(String),
    /// A build/launch failure; the message is printed to stderr, exit 1.
    Failed(String),
}

impl StartOutcome {
    pub(crate) fn code(&self) -> ExitCode {
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
pub(crate) fn start(attach: bool, build: bool) -> StartOutcome {
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
pub(crate) fn run_engine_build() -> Result<(), String> {
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
pub(crate) fn engine_binary_path() -> String {
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
pub(crate) fn engine_running(path: &str) -> bool {
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
pub(crate) fn poll_for_readiness(path: &str) -> StartOutcome {
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
