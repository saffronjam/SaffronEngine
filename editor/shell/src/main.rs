//! The Saffron editor shell: one winit-owned toplevel hosting CEF windowless OSR. CEF renders the
//! React UI off-screen; the shell composites each `on_paint` frame onto the toplevel surface and
//! keeps the engine's Vulkan viewport frames on their own surfaces below it.

mod appscheme;
mod async_rt;
mod backend;
mod cefapp;
mod commands;
mod connectors;
mod control;
mod dialog;
mod dnd;
mod engine;
mod fly;
mod geometry;
mod handlers;
mod ipc;
mod ipc_render;
mod keymap;
mod os;
mod scheme;
mod schemes;
mod settings;
mod shell;
mod state;
mod store_commands;
mod viewport;
mod window;

pub(crate) use os::open_url_in_browser;

use backend::UiCompositor;
use cef::{args::Args, *};
use cefapp::{AppBuilder, ShellApp};
use shell::Shell;
use state::ShellRequest;
use state::ShellState;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};

/// The shell's error type; the wire-string contract lives at the IPC boundary.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("window system not supported by the compiled shell backend")]
    UnsupportedWindowSystem,
    #[error("window handle: {0}")]
    Handle(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("engine: {0}")]
    Engine(String),
}

fn main() -> std::process::ExitCode {
    // Whatever must happen before the first CEF call — the AppKit backend loads the CEF framework
    // here; a platform that links libcef directly is a no-op.
    if let Err(err) = backend::bootstrap::load_cef() {
        eprintln!("fatal: load CEF: {err}");
        return std::process::ExitCode::from(1);
    }
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let cmd = args.as_cmd_line().unwrap();
    let type_switch = CefString::from("type");
    let is_browser_process = cmd.has_switch(Some(&type_switch)) != 1;

    let mut app = AppBuilder::build(ShellApp);
    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );

    if is_browser_process {
        assert_eq!(ret, -1, "browser process execute_process should return -1");
    } else {
        return std::process::ExitCode::from(0);
    }

    // Install the shared `saffron-log` subscriber so the shell's own lines carry the same
    // timestamp · level · subsystem format as the spawned host's — one uniform stream in the combined
    // stdout. Browser process only; the CEF helper subprocesses returned above.
    saffron_log::init_logging();

    // CEF's profile/cache root: a per-repo dir so it never shares the default profile (which forces
    // a cross-process singleton — the "profile in use by another Chromium process" crash — and is
    // what the `root_cache_path` warning is about). Created before `initialize`.
    let cache_dir = geometry::app_data_dir().join("cef-cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let settings = Settings {
        windowless_rendering_enabled: true as _,
        external_message_pump: true as _,
        root_cache_path: CefString::from(cache_dir.to_string_lossy().as_ref()),
        ..Default::default()
    };
    assert_eq!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        ),
        1,
        "cef initialize failed"
    );

    // On a platform whose run loop is shared with winit, CEF is pumped from run-loop context (see
    // the backend contract); where the shell's loop pumps directly this is a no-op.
    backend::bootstrap::install_cef_pump();

    // The engine-facing state container (socket, trace server). Constructing it starts the
    // profiler-trace loopback server.
    let state = Arc::new(ShellState::default());
    tracing::info!(
        target: "shell",
        "state ready; socket={} trace_port={:?}",
        state.socket_path, state.trace_port
    );

    // Serve `saffron-img://` thumbnails from the shared connector cache (browser process, post-init).
    register_scheme_handler_factory(
        Some(&schemes::IMG_SCHEME_NAME.into()),
        None,
        Some(&mut scheme::factory(state.connectors.cache())),
    );

    // Serve the bundled React UI over `saffron-app://` in a packaged build. `SAFFRON_UI_DIR` is set by
    // the AppImage's AppRun; it is empty in dev, where the UI loads from Vite via `SAFFRON_DEV_URL`.
    let ui_dir = std::env::var_os("SAFFRON_UI_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    register_scheme_handler_factory(
        Some(&schemes::APP_SCHEME_NAME.into()),
        None,
        Some(&mut appscheme::factory(ui_dir.clone())),
    );

    // The browser-side IPC router (created after `initialize`, on the UI thread) that carries
    // `cefQuery` requests to the control passthrough.
    let router = ipc::browser_router(Arc::clone(&state));

    // A fixed pacing override (headless measurement); otherwise the shell paces to the monitor's
    // refresh once the toplevel maps. Provisional 60 Hz until then (raised, not lowered, on detect).
    let target_hz_override: Option<f64> = std::env::var("SAFFRON_SHELL_TARGET_HZ")
        .ok()
        .and_then(|s| s.parse().ok());

    // The React UI URL. The dev loop sets `SAFFRON_DEV_URL` (Vite); a packaged build has
    // `SAFFRON_UI_DIR` set and loads the bundle over the `saffron-app://` scheme. With neither the
    // shell is misconfigured — show a plain theme-colored notice rather than a blank window.
    let url = std::env::var("SAFFRON_DEV_URL").unwrap_or_else(|_| {
        if ui_dir.as_os_str().is_empty() {
            concat!(
                "data:text/html,",
                "<html><body style='margin:0;background:%230a0a0a;color:%23888;",
                "font:14px sans-serif;display:grid;place-items:center;height:100vh'>",
                "no UI source: run via `just run` or a packaged build</body></html>"
            )
            .to_string()
        } else {
            appscheme::INDEX_URL.to_string()
        }
    });

    let paints = Arc::new(AtomicU64::new(0));
    let mut shell = Shell::new(
        router,
        Arc::clone(&state),
        url,
        target_hz_override,
        Arc::clone(&paints),
    );

    let mut event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);

    // 0 = run until the window is closed; a positive value bounds a headless measurement run.
    let measure_secs: u64 = std::env::var("SAFFRON_SHELL_MEASURE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut frame_dt = Duration::from_secs_f64(1.0 / shell.refresh_hz);
    let mut last_refresh_check = Instant::now();

    tracing::info!(target: "shell", "event loop up; CEF self-driven, pacing to monitor refresh");

    // No host is spawned here: the frontend starts a project session (`session_start`) when a
    // project is picked or when the environment names one.
    let start = Instant::now();
    let mut next_frame = Instant::now();
    let mut last_report = Instant::now();
    let mut total: u64 = 0;
    let mut reports: u64 = 0;

    let code = loop {
        // Where the loop pumps CEF itself (Wayland), the pump returns immediately and the sleep
        // below paces the iteration. Where the run loop is shared (AppKit), the loop blocks HERE
        // for up to a frame: winit's handler is only installed inside `pump_app_events`, so all
        // run-loop dispatch — CEF's timer-driven pump, the display link, input NSEvents — must
        // happen within this window or the events are dropped.
        let pump_timeout = if backend::bootstrap::PUMPS_IN_LOOP {
            Duration::ZERO
        } else {
            frame_dt
        };
        if let PumpStatus::Exit(code) = event_loop.pump_app_events(Some(pump_timeout), &mut shell) {
            break code as u8;
        }

        if backend::bootstrap::PUMPS_IN_LOOP {
            do_message_loop_work();
        }

        // Apply requests posted by IPC worker threads (window controls + JS event emits) on this,
        // the thread that owns the winit window and CEF browser.
        let requests: Vec<ShellRequest> = state
            .inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain(..)
            .collect();
        for request in requests {
            match request {
                ShellRequest::Window(action) => shell.apply_window_action(action),
                ShellRequest::Emit { event, payload } => shell.emit_to_js(&event, &payload),
                ShellRequest::Fly(request) => shell.apply_fly(request, &state.socket_path),
            }
        }

        // Drain the compositor's OS file drag-drop steps (received on the backend's own drag
        // source) and re-emit them to the frontend. Owned `Vec` so the compositor borrow ends
        // before `emit_dnd`.
        let dnd = shell
            .compositor
            .borrow_mut()
            .as_mut()
            .map(UiCompositor::pump_dnd)
            .unwrap_or_default();
        for ev in &dnd {
            shell.emit_dnd(ev);
        }

        // Stream the fly-cam sample (accumulated locked-pointer motion + key state) straight to
        // the engine: one `fly-input` per iteration, so the stream is paced at the monitor
        // refresh with no CEF hop.
        if let Some(fly) = shell.fly.as_mut() {
            let look = std::mem::take(&mut shell.look_accum);
            if !fly.send_sample(look, true) {
                tracing::warn!(target: "shell", "fly stream connection lost");
                shell.fly = None;
            }
        }

        // Pace to the monitor's refresh: pick it up once the surface maps, and follow a move to a
        // different-refresh output. Throttled — `current_monitor` locks Wayland surface state.
        if last_refresh_check.elapsed() >= Duration::from_millis(200) {
            last_refresh_check = Instant::now();
            if let Some(hz) = shell.retarget_refresh() {
                frame_dt = Duration::from_secs_f64(1.0 / hz);
            }
        }

        // Reveal only once the UI has actually painted, so the toplevel maps with real pixels
        // rather than flashing empty (the "reveal after first React frame" contract).
        if !shell.revealed && paints.load(Ordering::Relaxed) > 0 {
            if let Some(shell_window) = shell.shell_window.as_ref() {
                shell_window.reveal();
            }
            shell.revealed = true;
            tracing::debug!(target: "shell", "revealed after first paint");
        }

        // The per-second paint-rate report is a measurement aid, not routine output — only accumulate
        // + print it during a bounded measurement run (`SAFFRON_SHELL_MEASURE_SECS`).
        if measure_secs > 0 && last_report.elapsed() >= Duration::from_secs(1) {
            let n = paints.swap(0, Ordering::Relaxed);
            total += n;
            reports += 1;
            tracing::info!(target: "shell", "on_paint: {n} paints/s");
            last_report = Instant::now();
        }
        if measure_secs > 0 && start.elapsed() >= Duration::from_secs(measure_secs) {
            // Funnel through the one exit path: the flag is picked up by `new_events` inside the
            // next pump, which tears down and exits the loop.
            state.exit_requested.store(true, Ordering::Relaxed);
        }

        // Sleep-pace only when the pump returns immediately; a blocking pump already waited.
        if backend::bootstrap::PUMPS_IN_LOOP {
            next_frame += frame_dt;
            let now = Instant::now();
            if next_frame > now {
                std::thread::sleep(next_frame - now);
            } else {
                next_frame = now;
            }
        }
    };

    if let Some(avg) = total.checked_div(reports) {
        tracing::info!(
            target: "shell",
            "SUSTAINED on_paint average: {avg} paints/s over {reports}s (target {} Hz)",
            shell.refresh_hz
        );
    }

    // The loop-entangled teardown already ran inside the loop (the staged exit in `new_events`).
    // Only CEF finalization remains, which must happen OUTSIDE any handler callback (CEF aborts on
    // a nested-context shutdown): the browser ref is already released, and the CEF callbacks that
    // can fire during the drain no-op against the emptied state. The drain also bounces stray
    // platform events off winit's global hooks (its application subclass and delegates), which
    // winit logs-and-ignores since its loop is finished — that target carries no signal past this
    // boundary. Nothing may pump the winit loop again after this point.
    saffron_log::silence_target("winit");
    cef::shutdown();
    drop(shell);
    drop(event_loop);
    std::process::ExitCode::from(code)
}
