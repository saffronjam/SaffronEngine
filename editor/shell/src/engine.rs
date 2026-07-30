//! Engine (host) supervision: spawn the present-only `saffron-host`
//! child with the viewport/shm/socket env, watch its startup on a failure-only watchdog thread
//! (`viewport-error` on death / a socket that never binds — the frontend's viewport probe owns
//! the attach), report liveness via `try_wait` (never `Option::is_some`,
//! which reports a crashed engine as alive), and tear down (quit → kill → unlink socket + shm).
//! Shell-agnostic (`std::process` + the control passthrough + the event push). `auto_start` runs
//! once at shell launch (at launch); the `start_engine` command is the idempotent
//! ensure the frontend's loading overlay calls.

use crate::ShellError;
use crate::backend;
use crate::control::control_request;
use crate::geometry::{app_data_dir, ensure_app_dirs, repo_root};
use crate::state::{ShellState, viewport_shm_name};
use serde_json::json;
use std::fs;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn engine_binary() -> String {
    std::env::var("SAFFRON_ANIMA_BIN").unwrap_or_else(|_| {
        repo_root()
            .join("engine/target/debug/saffron-host")
            .to_string_lossy()
            .into_owned()
    })
}

/// Spawn the present-only host: it publishes each view's frames into its own shm segment for the
/// presenter instead of presenting to a swapchain.
pub fn spawn_engine(socket_path: &str) -> Result<Child, ShellError> {
    let _ = fs::remove_file(socket_path);
    ensure_app_dirs()?;
    let mut command = Command::new(engine_binary());
    command
        .env("SAFFRON_EDITOR_NATIVE_VIEWPORT", "1")
        .env("SAFFRON_CONTROL_SOCK", socket_path)
        .env("SAFFRON_APPDATA_DIR", app_data_dir())
        .env("SAFFRON_VIEWPORT_SHM_SCENE", viewport_shm_name("scene"))
        .env(
            "SAFFRON_VIEWPORT_SHM_ASSET",
            viewport_shm_name("assetPreview"),
        );
    // Platform GPU/loader env so the engine renders on hardware. The host paces itself, so there
    // is no launch-time fps cap.
    backend::env::engine_env(&mut command);
    command
        .current_dir(repo_root())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| ShellError::Engine(format!("failed to start engine host: {err}")))
}

/// The idempotent ensure the frontend's loading overlay calls: if the engine is already up, no-op;
/// otherwise spawn and stash it. No watchdog/emit — that is `auto_start`'s launch-time job.
pub fn start_engine(state: &Arc<ShellState>) -> Result<(), ShellError> {
    if child_alive(&state.engine) {
        return Ok(());
    }
    let child = spawn_engine(&state.socket_path)?;
    state
        .engine
        .lock()
        .map_err(|_| ShellError::Engine("engine lock poisoned".to_owned()))?
        .replace(child);
    Ok(())
}

/// Spawn the engine at shell launch and watch its startup on a background thread. The frontend
/// owns the attach (it holds the viewport rect) and its viewport probe is the single source of
/// truth for `attaching → ready`, so this thread is only a *failure* watchdog — it reports the
/// engine dying or the control socket never coming up, and deliberately emits NO success/attaching
/// phase (that would race the probe and could revert an already-ready viewport back to
/// "Attaching viewport…").
pub fn auto_start(state: &Arc<ShellState>) -> Result<(), ShellError> {
    let child = spawn_engine(&state.socket_path)?;
    state
        .engine
        .lock()
        .map_err(|_| ShellError::Engine("engine lock poisoned".to_owned()))?
        .replace(child);

    let monitor = Arc::clone(state);
    std::thread::spawn(move || {
        let mut delay = 50u64;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(delay));
            delay = (delay * 2).min(800);
            if !child_alive(&monitor.engine) {
                monitor.emit("viewport-error", json!("engine exited during startup"));
                return;
            }
            // Socket up = startup succeeded; the frontend probe takes it from here. Stop watching.
            if control_request(&monitor.socket_path, "viewport-native-info").is_ok() {
                return;
            }
        }
        monitor.emit(
            "viewport-error",
            json!("engine control socket did not come up"),
        );
    });
    Ok(())
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

/// Quit the engine cleanly, then force-kill and unlink the socket + shm segments.
pub fn teardown(state: &ShellState) {
    let _ = control_request(&state.socket_path, "quit");
    if let Ok(mut guard) = state.engine.lock()
        && let Some(mut child) = guard.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = fs::remove_file(&state.socket_path);
    // The engine unlinks its shm on clean exit; cover the killed case too (both views).
    for view in ["scene", "assetPreview"] {
        backend::env::remove_viewport_shm(&viewport_shm_name(view));
    }
}
