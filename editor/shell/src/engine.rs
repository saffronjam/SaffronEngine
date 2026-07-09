//! Engine (host) supervision: spawn the present-only `saffron-host`
//! child with the viewport/shm/socket env, poll readiness on a watchdog thread emitting
//! `engine-phase`/`viewport-error` events, report liveness via `try_wait` (never `Option::is_some`,
//! which reports a crashed engine as alive), and tear down (quit → kill → unlink socket + shm).
//! Shell-agnostic (`std::process` + the control passthrough + the event push). `auto_start` runs
//! once at shell launch (at launch); the `start_engine` command is the idempotent
//! ensure the frontend's loading overlay calls.

use crate::ShellError;
use crate::control::control_request;
use crate::geometry::{app_data_dir, ensure_app_dirs, repo_root};
use crate::state::{ShellState, viewport_shm_name};
use serde_json::json;
use std::fs;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The host's NVIDIA ICD, mounted from the host into the toolbox. The engine is a Vulkan/ash
/// program; pointing it here keeps it on hardware instead of llvmpipe. (CEF's own GPU process
/// reaches the GPU via GL/EGL/ANGLE, not this var — see Phase 1.)
const NVIDIA_ICD: &str = "/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json";

fn engine_binary() -> String {
    std::env::var("SAFFRON_ANIMA_BIN").unwrap_or_else(|_| {
        repo_root()
            .join("engine/target/debug/saffron-host")
            .to_string_lossy()
            .into_owned()
    })
}

/// Spawn the present-only host: it publishes each view's frames into its own shm segment for the
/// subsurface presenter (Phase 6) instead of presenting to a swapchain.
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
    // The toolbox ships only Mesa ICD manifests; point Vulkan at the host's NVIDIA ICD so the
    // engine renders on hardware. The host paces itself, so there is no launch-time fps cap.
    if std::env::var_os("VK_ICD_FILENAMES").is_none() && std::path::Path::new(NVIDIA_ICD).exists() {
        command.env("VK_ICD_FILENAMES", NVIDIA_ICD);
    }
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

/// Spawn the engine at shell launch and poll its readiness on a background thread, emitting
/// `engine-phase` (`starting` → `attaching`) / `viewport-error` events. The frontend drives the
/// actual attach (it owns the viewport rect); this only reports the process coming up.
pub fn auto_start(state: &Arc<ShellState>) -> Result<(), ShellError> {
    let child = spawn_engine(&state.socket_path)?;
    state
        .engine
        .lock()
        .map_err(|_| ShellError::Engine("engine lock poisoned".to_owned()))?
        .replace(child);
    state.emit("engine-phase", json!("starting"));

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
            if control_request(&monitor.socket_path, "viewport-native-info").is_ok() {
                monitor.emit("engine-phase", json!("attaching"));
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
        let _ = fs::remove_file(format!("/dev/shm{}", viewport_shm_name(view)));
    }
}
