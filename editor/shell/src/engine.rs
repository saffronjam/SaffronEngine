//! Host session supervision: a project session spawns the present-only `saffron-host` child with
//! the viewport/shm/socket env and the project boot intent, a per-session watcher reports startup
//! failure (`viewport-error`) and any exit (`session-exited`, with the exit code, whether the stop
//! was requested, and the tail of the host log), and teardown quits → kills → unlinks socket + shm.
//! The shell never spawns a host on its own — the frontend starts a session when a project is
//! picked (or when the environment names one) and stops it to return to the picker.
//! Shell-agnostic (`std::process` + the control passthrough + the event push).

use crate::ShellError;
use crate::backend;
use crate::control::control_request;
use crate::geometry::{app_data_dir, ensure_app_dirs, repo_root};
use crate::state::{SessionMeta, ShellState, viewport_shm_name};
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The host log ring capacity: enough context for a crash card without holding a session's
/// whole output.
const LOG_TAIL_CAP: usize = 60;

/// How many trailing host log lines a `session-exited` event carries.
const LOG_TAIL_REPORTED: usize = 12;

/// The project boot intent a session starts with: an existing project selection, a fresh project
/// to create, or empty (the host resolves `SAFFRON_PROJECT` / `SAFFRON_SCRATCH_PROJECT` / a cwd
/// `project.json` from the inherited environment).
#[derive(Default)]
pub struct SessionIntent {
    /// A project selection (a `project.json` path, a project dir, or a userdata name) to open.
    pub path: Option<String>,
    /// A fresh project to create: `(name, display_name)`.
    pub create: Option<(String, String)>,
}

impl SessionIntent {
    /// The label recorded in the session meta (what `session_status` reports as `path`).
    fn label(&self) -> String {
        if let Some(path) = &self.path {
            return path.clone();
        }
        if let Some((name, _)) = &self.create {
            return name.clone();
        }
        String::new()
    }
}

fn engine_binary() -> String {
    std::env::var("SAFFRON_ANIMA_BIN").unwrap_or_else(|_| {
        repo_root()
            .join("engine/target/debug/saffron-host")
            .to_string_lossy()
            .into_owned()
    })
}

/// Spawn the present-only host with the session's boot intent: it publishes each view's frames
/// into its own shm segment for the presenter instead of presenting to a swapchain. stdout/stderr
/// are piped through the log tee so the shell terminal still shows every host line while the tail
/// ring keeps crash context.
fn spawn_engine(state: &Arc<ShellState>, intent: &SessionIntent) -> Result<Child, ShellError> {
    let _ = fs::remove_file(&state.socket_path);
    ensure_app_dirs()?;
    let mut command = Command::new(engine_binary());
    command
        .env("SAFFRON_EDITOR_NATIVE_VIEWPORT", "1")
        .env("SAFFRON_CONTROL_SOCK", &state.socket_path)
        .env("SAFFRON_APPDATA_DIR", app_data_dir())
        .env("SAFFRON_VIEWPORT_SHM_SCENE", viewport_shm_name("scene"))
        .env(
            "SAFFRON_VIEWPORT_SHM_ASSET",
            viewport_shm_name("assetPreview"),
        );
    if let Some(path) = &intent.path {
        command.env("SAFFRON_PROJECT", path);
    }
    if let Some((name, display_name)) = &intent.create {
        command.env("SAFFRON_PROJECT", name);
        command.env("SAFFRON_PROJECT_DISPLAY_NAME", display_name);
    }
    // Platform GPU/loader env so the engine renders on hardware. The host paces itself, so there
    // is no launch-time fps cap.
    backend::env::engine_env(&mut command);
    let mut child = command
        .current_dir(repo_root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ShellError::Engine(format!("failed to start engine host: {err}")))?;
    if let Ok(mut tail) = state.host_log_tail.lock() {
        tail.clear();
    }
    if let Some(stdout) = child.stdout.take() {
        tee_host_stream(HostStream::Stdout(stdout), Arc::clone(state));
    }
    if let Some(stderr) = child.stderr.take() {
        tee_host_stream(HostStream::Stderr(stderr), Arc::clone(state));
    }
    Ok(child)
}

/// A host output stream handed to the log tee.
enum HostStream {
    Stdout(ChildStdout),
    Stderr(ChildStderr),
}

/// Pass a host output stream through to the shell's own stdout/stderr line by line while keeping
/// the tail ring current. The thread ends when the pipe closes (host exit).
fn tee_host_stream(stream: HostStream, state: Arc<ShellState>) {
    std::thread::spawn(move || {
        let push = |line: &str| {
            if let Ok(mut tail) = state.host_log_tail.lock() {
                if tail.len() == LOG_TAIL_CAP {
                    tail.pop_front();
                }
                tail.push_back(line.to_owned());
            }
        };
        match stream {
            HostStream::Stdout(stdout) => {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    push(&line);
                    let mut out = std::io::stdout().lock();
                    let _ = writeln!(out, "{line}");
                }
            }
            HostStream::Stderr(stderr) => {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    push(&line);
                    let mut out = std::io::stderr().lock();
                    let _ = writeln!(out, "{line}");
                }
            }
        }
    });
}

/// Start a project session: refuse if one is live, spawn the host with the intent, and arm the
/// per-session watcher. Returns as soon as the child is spawned — the frontend's viewport probe
/// and project-status poll both retry until the control socket answers.
pub fn start_session(state: &Arc<ShellState>, intent: SessionIntent) -> Result<(), ShellError> {
    if child_alive(&state.engine) {
        return Err(ShellError::Engine("a session is already running".into()));
    }
    state.session_expected_stop.store(false, Ordering::SeqCst);
    let generation = state.session_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let child = spawn_engine(state, &intent)?;
    state
        .engine
        .lock()
        .map_err(|_| ShellError::Engine("engine lock poisoned".to_owned()))?
        .replace(child);
    if let Ok(mut meta) = state.session_meta.lock() {
        *meta = Some(SessionMeta {
            label: intent.label(),
        });
    }
    watch_session(state, generation);
    Ok(())
}

/// Stop the current session (the frontend's return-to-picker / restart path): retire the watcher
/// so no `session-exited` fires for a requested stop, then tear the child down.
pub fn stop_session(state: &Arc<ShellState>) {
    state.session_expected_stop.store(true, Ordering::SeqCst);
    teardown(state);
}

/// The `session_status` payload: whether a host is live and which project it was started for.
pub fn session_status(state: &Arc<ShellState>) -> serde_json::Value {
    let running = child_alive(&state.engine);
    let path = state
        .session_meta
        .lock()
        .ok()
        .and_then(|meta| meta.as_ref().map(|m| m.label.clone()))
        .unwrap_or_default();
    json!({ "running": running, "path": path })
}

/// The per-session watcher: report startup failure (the control socket never answering) as a
/// `viewport-error`, then watch for the child exiting and emit `session-exited` with the exit
/// code, whether the stop was requested, and the log tail. The frontend's viewport probe owns the
/// success path (`attaching → ready`), so this thread deliberately emits NO success/attaching
/// phase — that would race the probe.
fn watch_session(state: &Arc<ShellState>, generation: u64) {
    let monitor = Arc::clone(state);
    std::thread::spawn(move || {
        let superseded = || monitor.session_gen.load(Ordering::SeqCst) != generation;
        let mut delay = 50u64;
        let mut socket_up = false;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(delay));
            delay = (delay * 2).min(800);
            if superseded() {
                return;
            }
            if let Some(code) = try_reap(&monitor) {
                emit_exited(&monitor, code);
                return;
            }
            if control_request(&monitor.socket_path, "viewport-native-info").is_ok() {
                socket_up = true;
                break;
            }
        }
        if !socket_up {
            monitor.emit(
                "viewport-error",
                json!("engine control socket did not come up"),
            );
        }
        loop {
            std::thread::sleep(Duration::from_millis(300));
            if superseded() {
                return;
            }
            if let Some(code) = try_reap(&monitor) {
                emit_exited(&monitor, code);
                return;
            }
        }
    });
}

/// Reap the child if it has exited, returning its exit code (`None` while it runs; `-1` for a
/// signal-terminated child with no code).
fn try_reap(state: &ShellState) -> Option<i32> {
    let mut guard = state.engine.lock().ok()?;
    let child = guard.as_mut()?;
    match child.try_wait() {
        Ok(Some(status)) => {
            guard.take();
            Some(status.code().unwrap_or(-1))
        }
        Ok(None) => None,
        Err(_) => {
            guard.take();
            Some(-1)
        }
    }
}

/// Emit `session-exited` with the exit code, whether the stop was requested, and the host log tail.
fn emit_exited(state: &ShellState, code: i32) {
    let expected = state.session_expected_stop.load(Ordering::SeqCst);
    let tail: Vec<String> = state
        .host_log_tail
        .lock()
        .map(|tail| {
            tail.iter()
                .rev()
                .take(LOG_TAIL_REPORTED)
                .rev()
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if let Ok(mut meta) = state.session_meta.lock() {
        *meta = None;
    }
    state.emit(
        "session-exited",
        json!({ "code": code, "expected": expected, "logTail": tail }),
    );
}

/// True only if the child is spawned AND has not exited — via `try_wait` (reaping), never
/// `Option::is_some` (which reports a crashed engine as still running).
pub fn child_alive(engine: &Mutex<Option<Child>>) -> bool {
    let Ok(mut guard) = engine.lock() else {
        return false;
    };
    match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

/// Quit the engine cleanly, then force-kill and unlink the socket + shm segments. Retires the
/// session watcher first so a torn-down child never reports as a crash.
pub fn teardown(state: &ShellState) {
    state.session_gen.fetch_add(1, Ordering::SeqCst);
    let _ = control_request(&state.socket_path, "quit");
    if let Ok(mut guard) = state.engine.lock()
        && let Some(mut child) = guard.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    if let Ok(mut meta) = state.session_meta.lock() {
        *meta = None;
    }
    let _ = fs::remove_file(&state.socket_path);
    // The engine unlinks its shm on clean exit; cover the killed case too (both views).
    for view in ["scene", "assetPreview"] {
        backend::env::remove_viewport_shm(&viewport_shm_name(view));
    }
}
