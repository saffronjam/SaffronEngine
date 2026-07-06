use gtk::prelude::WidgetExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalSize, RunEvent, State, WindowEvent,
};

mod connectors;
mod wayland_viewport;

/// The NVIDIA Vulkan ICD the toolbox lacks but the host provides; also the marker that
/// this is an NVIDIA box (the WebKit env workarounds below key off it).
const NVIDIA_ICD: &str = "/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json";

fn nvidia_present() -> bool {
    std::path::Path::new(NVIDIA_ICD).exists() || std::path::Path::new("/sys/module/nvidia").exists()
}

struct EditorState {
    engine: Mutex<Option<Child>>,
    socket_path: String,
    viewports: Arc<wayland_viewport::Viewports>,
    /// The latest profiler trace bytes, served on the loopback port below so Perfetto can fetch
    /// them itself (`?url=`). Replaced on each "Open in Perfetto"; None until the first.
    trace: Arc<Mutex<Option<Vec<u8>>>>,
    /// The loopback port the trace server bound, or None if it could not start.
    trace_port: Option<u16>,
}

const MAIN_WINDOW_WIDTH: f64 = 1600.0;
const MAIN_WINDOW_HEIGHT: f64 = 900.0;
const MAIN_WINDOW_MIN_WIDTH: f64 = 1200.0;
const MAIN_WINDOW_MIN_HEIGHT: f64 = 720.0;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppDataInfo {
    app_data_dir: String,
    userdata_dir: String,
    env_project: bool,
    scratch_project: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecentProject {
    path: String,
    name: String,
    display_name: String,
    last_opened_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RecentProjects {
    projects: Vec<RecentProject>,
}

/// Editor-wide settings persisted as deltas in appdata/settings.json: `key_bindings`
/// holds only the commands the user changed (command id → key-string); defaults live
/// in the frontend registry. An untyped map so adding commands never touches Rust.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct EditorSettings {
    #[serde(default)]
    key_bindings: std::collections::HashMap<String, String>,
}

/// The generic "remember where I left it" bucket, persisted hand-rolled to appdata/state.json — a
/// sibling of settings.json / recent-projects.json. This is transient UI memory, not configured
/// settings: a missing or corrupt file means no memory, so the window falls back to filling the
/// current monitor. New top-level blocks can be added as more transient state is remembered.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RememberedState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window: Option<WindowState>,
}

/// The editor window's last-known geometry, in physical pixels so it round-trips 1:1 with
/// `inner_size()` / `outer_position()` and the monitor rects used for clamping. `maximized`
/// re-applies the maximized state on top of the remembered normal-size geometry. `scale` and
/// `monitor` are best-effort hints — native Wayland cannot force an output or a position, so they
/// are advisory (`scale` records the DPI the size was captured at, for a future cross-DPI pass).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
    #[serde(default)]
    scale: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    monitor: Option<String>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: MAIN_WINDOW_WIDTH as u32,
            height: MAIN_WINDOW_HEIGHT as u32,
            maximized: false,
            scale: 1.0,
            monitor: None,
        }
    }
}

/// The live in-memory geometry snapshot, managed as Tauri state and flushed to state.json on exit.
/// Updated on every resize/move while not maximized; the maximized flag tracks every change.
#[derive(Default)]
struct WindowStateTracker(Mutex<WindowState>);

impl Default for EditorState {
    fn default() -> Self {
        let trace: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
        let trace_port = start_trace_server(Arc::clone(&trace));
        Self {
            engine: Mutex::new(None),
            socket_path: socket_path(),
            viewports: Arc::default(),
            trace,
            trace_port,
        }
    }
}

// The path the trace is served on; everything else 404s (see serve_trace_conn).
const TRACE_PATH: &str = "/trace.perfetto-trace";

// A loopback HTTP server that serves the most-recent profiler trace with permissive CORS, so
// Perfetto (opened with `?url=`) fetches and loads it itself — the `postMessage` handoff it
// documents cannot cross the webview -> desktop-browser process boundary, but a plain GET can.
// Bound on :9001 because Perfetto's own CSP only allows loopback fetches from that port (its
// trace_processor httpd port); an ephemeral fallback keeps downloads working if :9001 is taken,
// though auto-import then can't pass the CSP. The toolbox shares the host network namespace, so a
// 127.0.0.1 bind is reachable from the host browser. The accept loop runs for the app's lifetime.
fn start_trace_server(trace: Arc<Mutex<Option<Vec<u8>>>>) -> Option<u16> {
    let listener = TcpListener::bind("127.0.0.1:9001")
        .or_else(|_| TcpListener::bind("127.0.0.1:0"))
        .ok()?;
    let port = listener.local_addr().ok()?.port();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let trace = Arc::clone(&trace);
            thread::spawn(move || {
                let _ = serve_trace_conn(stream, &trace);
            });
        }
    });
    Some(port)
}

fn serve_trace_conn(mut stream: TcpStream, trace: &Mutex<Option<Vec<u8>>>) -> std::io::Result<()> {
    // Read the request head (up to the blank line); the body, if any, is irrelevant for GET.
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&chunk[..n]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") || head.len() > 16384 {
            break;
        }
    }
    let request_line = String::from_utf8_lossy(&head);
    let mut tokens = request_line.split_whitespace();
    let method = tokens.next().unwrap_or("");
    let path = tokens.next().unwrap_or("");
    // `Allow-Private-Network` is required for Chromium's Private Network Access: a secure public
    // origin (ui.perfetto.dev) fetching a loopback address sends a preflight that this must echo,
    // or the request is blocked ("Failed to fetch") before it ever reaches us.
    const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, HEAD, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Max-Age: 86400\r\n";

    if method.eq_ignore_ascii_case("OPTIONS") {
        let resp = format!(
            "HTTP/1.1 204 No Content\r\n{CORS}Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(resp.as_bytes())?;
        return stream.flush();
    }

    // Serve bytes only on the trace path; every other path (notably Perfetto's `/status` probe on
    // :9001) gets a 404 so it doesn't mistake this for a live trace_processor RPC server.
    if path != TRACE_PATH {
        let resp = format!(
            "HTTP/1.1 404 Not Found\r\n{CORS}Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(resp.as_bytes())?;
        return stream.flush();
    }

    let body = trace.lock().ok().and_then(|guard| guard.clone());
    match body {
        Some(bytes) => {
            let header = format!(
                "HTTP/1.1 200 OK\r\n{CORS}Content-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            stream.write_all(header.as_bytes())?;
            if !method.eq_ignore_ascii_case("HEAD") {
                stream.write_all(&bytes)?;
            }
        }
        None => {
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\n{CORS}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(resp.as_bytes())?;
        }
    }
    stream.flush()
}

// Per-PID socket in XDG_RUNTIME_DIR so two editor instances get distinct engines/sockets.
fn socket_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{dir}/saffron-editor-{}.sock", std::process::id())
}

// Per-PID, per-view shm segment the engine publishes that view's viewport frames into; the
// presenter maps each. The token MUST be the engine's wire name ("scene" / "assetPreview").
fn viewport_shm_name(view: &str) -> String {
    format!("/saffron-viewport-{}-{}", view, std::process::id())
}

fn engine_binary() -> String {
    std::env::var("SAFFRON_ANIMA_BIN").unwrap_or_else(|_| {
        repo_root()
            .join("engine/target/debug/saffron-host")
            .to_string_lossy()
            .into_owned()
    })
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn app_data_dir() -> PathBuf {
    repo_root().join("appdata")
}

/// Percent-decode a `%XX` string (the `?u=` image url). Bytes that aren't a valid `%XX` escape pass
/// through, so an already-decoded url decodes to itself.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An empty error body for a failed `saffron-img://` fetch; the webview shows its broken-image glyph.
fn img_scheme_error() -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(502)
        .body(Vec::new())
        .expect("build image error response")
}

fn userdata_dir() -> PathBuf {
    app_data_dir().join("userdata")
}

fn recents_path() -> PathBuf {
    app_data_dir().join("recent-projects.json")
}

fn ensure_app_dirs() -> Result<(), String> {
    fs::create_dir_all(userdata_dir()).map_err(|err| format!("create app data dirs: {err}"))
}

fn read_recent_projects_file() -> RecentProjects {
    let path = recents_path();
    let Ok(text) = fs::read_to_string(path) else {
        return RecentProjects::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_recent_projects_file(recents: &RecentProjects) -> Result<(), String> {
    ensure_app_dirs()?;
    let text = serde_json::to_string_pretty(recents)
        .map_err(|err| format!("encode recent projects: {err}"))?;
    fs::write(recents_path(), text).map_err(|err| format!("write recent projects: {err}"))
}

fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

// A missing or corrupt settings file falls back to defaults (an empty delta map).
fn read_settings_file() -> EditorSettings {
    let Ok(text) = fs::read_to_string(settings_path()) else {
        return EditorSettings::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_settings_file(settings: &EditorSettings) -> Result<(), String> {
    ensure_app_dirs()?;
    let text = serde_json::to_string_pretty(settings)
        .map_err(|err| format!("encode editor settings: {err}"))?;
    fs::write(settings_path(), text).map_err(|err| format!("write editor settings: {err}"))
}

fn state_path() -> PathBuf {
    app_data_dir().join("state.json")
}

// A missing or corrupt state file falls back to defaults (no remembered window).
fn read_state_file() -> RememberedState {
    let Ok(text) = fs::read_to_string(state_path()) else {
        return RememberedState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_state_file(state: &RememberedState) -> Result<(), String> {
    ensure_app_dirs()?;
    let text = serde_json::to_string_pretty(state)
        .map_err(|err| format!("encode remembered state: {err}"))?;
    fs::write(state_path(), text).map_err(|err| format!("write remembered state: {err}"))
}

// Window-geometry restore. The editor window is the on-screen window (the host renders headless),
// so its geometry is remembered in appdata/state.json and re-applied here, before the window is
// revealed from JS.
//
// Only SIZE and MAXIMIZED are acted on. Position and monitor are still recorded (they may be useful
// on a platform that can honor them), but never applied: native Wayland — GNOME/Mutter in
// particular — gives a client no way to place its own toplevel or choose an output, so acting on
// them is pointless here. The compositor owns placement.

/// Re-apply a remembered geometry: SIZE then MAXIMIZED (position/monitor are recorded but not
/// acted on). Size is set before maximize so the un-maximize restore-size equals the remembered
/// normal size.
fn apply_window_state(window: &tauri::WebviewWindow, want: &WindowState) {
    let _ = window.set_size(PhysicalSize::new(want.width, want.height));
    if want.maximized {
        let _ = window.maximize();
    }
}

/// The no-memory default: size to the current (else primary) monitor. Returns the geometry applied
/// so the caller can seed the live tracker (recording the monitor's position/name even though only
/// its size is acted on).
fn fill_current_monitor(window: &tauri::WebviewWindow) -> WindowState {
    if let Ok(Some(monitor)) = window
        .current_monitor()
        .or_else(|_| window.primary_monitor())
    {
        let position = monitor.position();
        let size = monitor.size();
        let _ = window.set_size(PhysicalSize::new(size.width, size.height));
        WindowState {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            maximized: false,
            scale: monitor.scale_factor(),
            monitor: monitor.name().cloned(),
        }
    } else {
        let _ = window.set_size(LogicalSize::new(MAIN_WINDOW_WIDTH, MAIN_WINDOW_HEIGHT));
        WindowState::default()
    }
}

/// Fold the window's current geometry into the live tracker (called on every resize/move). The
/// maximized flag always tracks; x/y/width/height only update while not maximized, because a
/// maximized window reports the maximized bounds and we must remember the last normal geometry so
/// un-maximize (and next launch's restore-then-maximize) return to the right size.
fn capture_window_geometry(window: &tauri::WebviewWindow, tracker: &WindowStateTracker) {
    let mut guard = tracker.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let maximized = window.is_maximized().unwrap_or(false);
    guard.maximized = maximized;
    // Monitor + scale are best-effort hints that reflect current reality regardless of maximized;
    // if they only updated while restored, a window opened-and-kept maximized would keep a stale
    // monitor from a previous session.
    if let Ok(scale) = window.scale_factor() {
        guard.scale = scale;
    }
    if let Ok(Some(monitor)) = window.current_monitor() {
        guard.monitor = monitor.name().cloned();
    }
    // Size (and the normal position) must stay the last NON-maximized geometry, so un-maximize —
    // and next launch's restore-then-maximize — return to the right size.
    if maximized {
        return;
    }
    if let Ok(size) = window.inner_size()
        && size.width > 0
        && size.height > 0
    {
        guard.width = size.width;
        guard.height = size.height;
    }
    // outer_position() errors on native Wayland — keep the last-known x/y rather than lose it.
    if let Ok(pos) = window.outer_position() {
        guard.x = pos.x;
        guard.y = pos.y;
    }
}

/// Restore the window's remembered geometry (or fill the current monitor when there is no memory),
/// before the window is shown. Returns the applied geometry so the caller seeds the live tracker.
fn configure_main_window(window: &tauri::WebviewWindow) -> WindowState {
    let _ = window.set_title("Saffron Anima");
    let _ = window.set_min_size(Some(LogicalSize::new(
        MAIN_WINDOW_MIN_WIDTH,
        MAIN_WINDOW_MIN_HEIGHT,
    )));

    match read_state_file().window {
        Some(want) if want.width > 0 && want.height > 0 => {
            apply_window_state(window, &want);
            want
        }
        _ => fill_current_monitor(window),
    }
}

// Serializes every control-plane socket round-trip. The engine drains control once per frame, so
// concurrent invokes (the reconcile poll + per-edit material-update/preview-render + the graph
// editor's apply) otherwise pile sockets into that drain and a GPU-bound preview-render pushes some
// blocking read past its 5 s timeout → EAGAIN ("os error 11"). Holding this across the whole
// connect+write+read keeps exactly one round-trip outstanding, regardless of caller.
static CONTROL_IO: Mutex<()> = Mutex::new(());

// A control-plane failure surfaced to the webview: the engine's human message plus the
// machine-readable `code` from the reply envelope (present on every `ok:false`). Serialized to a
// `{ message, code }` object so the typed client can match on `code` — e.g. drop a `busy-loading`
// reply on a background poll lane instead of toasting it.
#[derive(Debug, Clone, serde::Serialize)]
struct ControlError {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
}

impl From<String> for ControlError {
    fn from(message: String) -> Self {
        Self {
            message,
            code: None,
        }
    }
}

impl From<ControlError> for String {
    fn from(err: ControlError) -> Self {
        err.message
    }
}

// The one socket round-trip helper the whole bridge is built on. Newline-delimited JSON;
// surfaces the engine's `ok:false` reply as a typed Err (message + envelope `code`) → a rejected
// JS promise.
fn control_request_with_params(
    socket_path: &str,
    command: &str,
    params: Value,
) -> Result<Value, ControlError> {
    // The guarded data is () (it carries no invariant), so recover a poisoned lock rather than fail.
    let _guard = CONTROL_IO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|err| format!("control socket unavailable: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(5000)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    let mut request = json!({ "id": 1, "cmd": command, "params": params }).to_string();
    request.push('\n');
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("send control request: {err}"))?;

    let mut reply = String::new();
    let mut buffer = [0_u8; 4096];
    while !reply.contains('\n') {
        let read = stream
            .read(&mut buffer)
            .map_err(|err| format!("read control reply: {err}"))?;
        if read == 0 {
            break;
        }
        reply.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    let value: Value =
        serde_json::from_str(reply.trim()).map_err(|err| format!("decode control reply: {err}"))?;
    if value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(value.get("result").cloned().unwrap_or_default());
    }
    Err(ControlError {
        message: value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("control command failed")
            .to_string(),
        code: value
            .get("code")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    })
}

fn control_request(socket_path: &str, command: &str) -> Result<Value, ControlError> {
    control_request_with_params(socket_path, command, json!({}))
}

fn spawn_engine(socket_path: &str) -> Result<Child, String> {
    let _ = fs::remove_file(socket_path);
    ensure_app_dirs()?;
    let mut command = Command::new(engine_binary());
    command
        .env("SAFFRON_EDITOR_NATIVE_VIEWPORT", "1")
        .env("SAFFRON_CONTROL_SOCK", socket_path)
        .env("SAFFRON_APPDATA_DIR", app_data_dir())
        // The engine publishes each view's frames into its own shared-memory segment for the
        // subsurface presenter instead of presenting to its (hidden) swapchain. One segment
        // per view so each pane's subsurface has a ring even while parked.
        .env("SAFFRON_VIEWPORT_SHM_SCENE", viewport_shm_name("scene"))
        .env(
            "SAFFRON_VIEWPORT_SHM_ASSET",
            viewport_shm_name("assetPreview"),
        );
    // The host paces itself: a reactive loop renders at the perf-config `target_fps` while the
    // scene is active and idles the GPU on a static viewport, so there is no launch-time fps cap.
    // The toolbox ships only Mesa ICD manifests; point Vulkan at the host's NVIDIA ICD
    // so the engine renders on hardware instead of llvmpipe.
    if std::env::var_os("VK_ICD_FILENAMES").is_none() && std::path::Path::new(NVIDIA_ICD).exists() {
        command.env("VK_ICD_FILENAMES", NVIDIA_ICD);
    }
    command
        .current_dir(repo_root())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| format!("failed to start engine host: {err}"))
}

// True only if the child is spawned AND has not exited — via try_wait (reaping), never
// Option::is_some (which reports a crashed engine as still running).
fn child_alive(engine: &Mutex<Option<Child>>) -> bool {
    let Ok(mut guard) = engine.lock() else {
        return false;
    };
    match guard.as_mut() {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

fn teardown(state: &EditorState) {
    let _ = control_request(&state.socket_path, "quit");
    if let Ok(mut guard) = state.engine.lock() {
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    let _ = fs::remove_file(&state.socket_path);
    // The engine unlinks its shm segments on clean exit; cover the killed case too (both views).
    for view in ["scene", "assetPreview"] {
        let _ = fs::remove_file(format!("/dev/shm{}", viewport_shm_name(view)));
    }
}

// ONE generic passthrough: any `sa` command reaches the engine with zero Rust changes.
// Async so the blocking socket round trip runs on a worker, never the main thread
// driving the webview event loop (a sync command would stall the UI during edit streams).
// [vp-dbg] TEMP: route a webview-side log line to the `just run` terminal (console.log does not
// reach it). Delete with the probe once the tab-switch stall is diagnosed.
#[tauri::command]
fn dbg_log(msg: String) {
    eprintln!("[vp-dbg] {msg}");
}

#[tauri::command]
async fn control(
    state: State<'_, EditorState>,
    cmd: String,
    params: Option<Value>,
) -> Result<Value, ControlError> {
    control_request_with_params(
        &state.socket_path,
        &cmd,
        params.unwrap_or_else(|| json!({})),
    )
}

/// The connectors enabled for the project, for the Store UI.
#[tauri::command]
fn store_list_connectors(
    connectors: State<'_, connectors::ConnectorRuntime>,
) -> Vec<connectors::ConnectorInfo> {
    connectors.infos()
}

/// Starts (or replaces) a search session for a committed query; returns its id. Errors when the
/// query names no known store.
#[tauri::command]
fn store_search_session(
    connectors: State<'_, connectors::ConnectorRuntime>,
    query: connectors::SearchQuery,
) -> Result<String, String> {
    connectors.start_session(query).map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoreSearchMore {
    results: Vec<connectors::StoreResult>,
    exhausted: bool,
}

/// The virtual-scroll driver: the next batch from the store plus whether it is exhausted.
#[tauri::command]
async fn store_search_more(
    connectors: State<'_, connectors::ConnectorRuntime>,
    session: String,
    count: usize,
) -> Result<StoreSearchMore, String> {
    let handle = connectors
        .session(&session)
        .ok_or_else(|| "search session expired".to_string())?;
    let mut guard = handle.lock().await;
    let results = guard.next_batch(count).await;
    Ok(StoreSearchMore {
        results,
        exhausted: guard.all_exhausted(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportedAsset {
    id: String,
    name: String,
}

/// Downloads a store result to a local path and imports it through the host control
/// plane, recording the asset's license/author/source as attribution.
#[tauri::command]
async fn store_import(
    state: State<'_, EditorState>,
    connectors: State<'_, connectors::ConnectorRuntime>,
    result: connectors::StoreResult,
    resolution: Option<String>,
    on_progress: tauri::ipc::Channel<f64>,
) -> Result<ImportedAsset, String> {
    use connectors::StoreKind;
    let connector = connectors
        .connector(&result.store.id)
        .ok_or_else(|| format!("unknown connector '{}'", result.store.id))?;
    // The chosen resolution rides on the descriptor; the connector selects the variant.
    let mut descriptor = result.import_descriptor.clone();
    descriptor.resolution = resolution;
    // Forward download progress over the channel, throttled to whole-percent changes so a
    // fast download doesn't flood the webview (the UI animates smoothly between values).
    let last_pct = std::sync::atomic::AtomicU32::new(u32::MAX);
    let report = move |f: f64| {
        let pct = (f.clamp(0.0, 1.0) * 100.0).round() as u32;
        if pct != last_pct.swap(pct, std::sync::atomic::Ordering::Relaxed) {
            let _ = on_progress.send(f);
        }
    };
    let path = connector
        .download(&descriptor, &report)
        .await
        .map_err(|err| err.to_string())?;
    let path = path.to_string_lossy().to_string();
    let attribution = json!({
        "licenseId": result.license.id,
        "requiresAttribution": result.license.requires_attribution,
        "licenseUrl": result.license.url,
        "author": result.author,
        "sourceUrl": result.source_url,
        "storeId": result.store.id,
    });
    // The kind selects the host importer: model → .smodel, material/texture → .smat,
    // hdri → a texture the scene can use as its environment.
    let (cmd, params) = match result.kind {
        StoreKind::Model => (
            "import-model",
            json!({ "path": path, "attribution": attribution }),
        ),
        StoreKind::Material | StoreKind::Texture => (
            "material-import",
            json!({ "path": path, "name": result.name, "attribution": attribution }),
        ),
        StoreKind::Hdri => (
            "import-texture",
            json!({ "path": path, "role": "hdri" }),
        ),
    };
    let reply = control_request_with_params(&state.socket_path, cmd, params)?;
    Ok(ImportedAsset {
        id: reply
            .get("id")
            .or_else(|| reply.get("texture"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        name: reply
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| result.name.clone()),
    })
}

/// The individually-selectable parts of a store result (for the Import dropdown).
#[tauri::command]
async fn store_asset_parts(
    connectors: State<'_, connectors::ConnectorRuntime>,
    result: connectors::StoreResult,
) -> Result<Vec<connectors::AssetPart>, String> {
    let connector = connectors
        .connector(&result.store.id)
        .ok_or_else(|| format!("unknown connector '{}'", result.store.id))?;
    connector
        .parts(&result)
        .await
        .map_err(|err| err.to_string())
}

/// The asset's preview images (for the gallery nav + the expand modal).
#[tauri::command]
async fn store_asset_gallery(
    connectors: State<'_, connectors::ConnectorRuntime>,
    result: connectors::StoreResult,
) -> Result<Vec<connectors::GalleryImage>, String> {
    let connector = connectors
        .connector(&result.store.id)
        .ok_or_else(|| format!("unknown connector '{}'", result.store.id))?;
    connector
        .gallery(&result)
        .await
        .map_err(|err| err.to_string())
}

/// Downloads and imports a single part: a map → a colorspace-correct texture; a whole
/// model/material → the same path as `store_import`.
#[tauri::command]
async fn store_import_part(
    state: State<'_, EditorState>,
    connectors: State<'_, connectors::ConnectorRuntime>,
    result: connectors::StoreResult,
    part: connectors::AssetPart,
    resolution: Option<String>,
) -> Result<ImportedAsset, String> {
    use connectors::StoreKind;
    let connector = connectors
        .connector(&result.store.id)
        .ok_or_else(|| format!("unknown connector '{}'", result.store.id))?;
    let path = connector
        .download_part(&part, resolution.as_deref())
        .await
        .map_err(|err| err.to_string())?;
    let path = path.to_string_lossy().to_string();
    let attribution = json!({
        "licenseId": result.license.id,
        "requiresAttribution": result.license.requires_attribution,
        "licenseUrl": result.license.url,
        "author": result.author,
        "sourceUrl": result.source_url,
        "storeId": result.store.id,
    });
    let (cmd, params) = match part.import_kind {
        // The engine owns the role→colorspace policy: send the connector's map role and let
        // `import-texture` derive the upload space (and persist the semantic role for previews).
        StoreKind::Texture => (
            "import-texture",
            json!({ "path": path, "role": part.role }),
        ),
        StoreKind::Hdri => (
            "import-texture",
            json!({ "path": path, "role": "hdri" }),
        ),
        StoreKind::Model => (
            "import-model",
            json!({ "path": path, "attribution": attribution }),
        ),
        StoreKind::Material => (
            "material-import",
            json!({ "path": path, "name": part.label, "attribution": attribution }),
        ),
    };
    let reply = control_request_with_params(&state.socket_path, cmd, params)?;
    Ok(ImportedAsset {
        id: reply
            .get("id")
            .or_else(|| reply.get("texture"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        name: part.label,
    })
}

/// Stores a connector's secret (API key / token) in the OS keyring. The value never
/// flows back to the webview.
#[tauri::command]
fn connector_set_secret(connector_id: String, secret: String) -> Result<(), String> {
    connectors::Credentials::global()
        .set_secret(&connector_id, &secret)
        .map_err(|err| err.to_string())
}

/// Removes a connector's stored secret.
#[tauri::command]
fn connector_clear_secret(connector_id: String) -> Result<(), String> {
    connectors::Credentials::global()
        .delete_secret(&connector_id)
        .map_err(|err| err.to_string())
}

/// Whether a connector has a stored secret (the boolean only — never the value).
#[tauri::command]
fn connector_secret_status(connector_id: String) -> bool {
    connectors::Credentials::global().has_secret(&connector_id)
}

/// Runs an `oauth_loopback` connector's login: opens the system browser to the provider's
/// authorize page, captures the implicit-flow token on a one-shot loopback listener, and
/// stores it in the keyring. Blocks on a worker thread so the UI stays responsive.
#[tauri::command]
async fn connector_login(
    connectors: State<'_, connectors::ConnectorRuntime>,
    connector_id: String,
) -> Result<(), String> {
    let config = connectors
        .oauth_config(&connector_id)
        .ok_or_else(|| format!("connector '{connector_id}' has no OAuth login"))?;
    tauri::async_runtime::spawn_blocking(move || connectors::run_loopback_login(&config))
        .await
        .map_err(|err| err.to_string())?
        .map(|_| ())
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn engine_alive(state: State<'_, EditorState>) -> bool {
    child_alive(&state.engine)
}

#[tauri::command]
fn start_engine(state: State<'_, EditorState>) -> Result<(), String> {
    if child_alive(&state.engine) {
        return Ok(());
    }
    let child = spawn_engine(&state.socket_path)?;
    state
        .engine
        .lock()
        .map_err(|_| "engine lock poisoned".to_string())?
        .replace(child);
    Ok(())
}

/// The viewport panel's logical CSS rect within the webview, plus the window scale
/// factor. The presenter positions the subsurface in logical coordinates; the engine
/// renders at device pixels.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct ViewportBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

// Resolve a wire view token ("scene" / "assetPreview") to its View, rejecting anything else.
fn viewport_for(view: &str) -> Result<wayland_viewport::View, String> {
    wayland_viewport::View::from_wire(view).ok_or_else(|| format!("unknown viewport view '{view}'"))
}

#[tauri::command]
async fn set_viewport_bounds(
    window: tauri::WebviewWindow,
    state: State<'_, EditorState>,
    view: String,
    bounds: ViewportBounds,
    resize_engine: bool,
) -> Result<(), String> {
    let which = viewport_for(&view)?;
    state.viewports.view(which).set_bounds(
        bounds.x.round() as i32,
        bounds.y.round() as i32,
        bounds.width.round() as i32,
        bounds.height.round() as i32,
    );
    // The subsurface position is double-buffered on the parent; nudge a parent commit.
    let nudge = window.clone();
    let _ = window.run_on_main_thread(move || {
        if let Ok(gtk_window) = nudge.gtk_window() {
            gtk_window.queue_draw();
        }
    });
    // Resizing the engine's render target recreates the offscreen chain (expensive), so
    // only the settled (debounced) bounds do it — live drag ticks stretch the current
    // frame via the subsurface instead. Ignore failures while the engine boots.
    if resize_engine {
        let width = ((bounds.width * bounds.scale).round() as i64).max(1);
        let height = ((bounds.height * bounds.scale).round() as i64).max(1);
        let _ = control_request_with_params(
            &state.socket_path,
            "set-viewport-size",
            json!({ "view": view, "width": width, "height": height }),
        );
    }
    Ok(())
}

#[tauri::command]
fn set_viewport_parked(
    window: tauri::WebviewWindow,
    state: State<'_, EditorState>,
    view: String,
    parked: bool,
) -> Result<(), String> {
    let which = viewport_for(&view)?;
    state.viewports.view(which).set_parked(parked);
    // The opaque underlay punches its hole per the parked flag; repaint it.
    let nudge = window.clone();
    let _ = window.run_on_main_thread(move || {
        if let Ok(gtk_window) = nudge.gtk_window() {
            gtk_window.queue_draw();
        }
    });
    Ok(())
}

/// The true monitor refresh (Hz) of the output the viewport composites on, from `wp_presentation`
/// feedback — the editor's WebKitGTK `requestAnimationFrame` is pinned to ~60 Hz and cannot see it.
/// `0.0` until the first presented frame reports it (the caller falls back to its current target).
#[tauri::command]
fn viewport_refresh_hz(state: State<'_, EditorState>) -> f64 {
    f64::from(state.viewports.refresh_mhz()) / 1000.0
}

#[tauri::command]
fn quit_engine(state: State<'_, EditorState>) -> Result<(), String> {
    teardown(&state);
    Ok(())
}

#[tauri::command]
fn app_data_info() -> Result<AppDataInfo, String> {
    ensure_app_dirs()?;
    Ok(AppDataInfo {
        app_data_dir: app_data_dir().to_string_lossy().into_owned(),
        userdata_dir: userdata_dir().to_string_lossy().into_owned(),
        env_project: std::env::var_os("SAFFRON_PROJECT").is_some(),
        scratch_project: std::env::var_os("SAFFRON_SCRATCH_PROJECT").is_some(),
    })
}

#[tauri::command]
fn list_recent_projects() -> Result<RecentProjects, String> {
    ensure_app_dirs()?;
    let mut recents = read_recent_projects_file();
    recents
        .projects
        .retain(|project| PathBuf::from(&project.path).exists());
    recents.projects.truncate(12);
    write_recent_projects_file(&recents)?;
    Ok(recents)
}

#[tauri::command]
fn load_editor_settings() -> Result<EditorSettings, String> {
    ensure_app_dirs()?;
    Ok(read_settings_file())
}

#[tauri::command]
fn save_editor_settings(settings: EditorSettings) -> Result<(), String> {
    write_settings_file(&settings)
}

#[tauri::command]
fn remember_recent_project(project: RecentProject) -> Result<RecentProjects, String> {
    ensure_app_dirs()?;
    let mut recents = read_recent_projects_file();
    recents
        .projects
        .retain(|recent| recent.path != project.path);
    recents.projects.insert(0, project);
    recents.projects.truncate(12);
    write_recent_projects_file(&recents)?;
    Ok(recents)
}

// Write client-generated bytes (a profiler trace) to a user-chosen path. The webview cannot
// `<a download>` a blob, so the editor picks a path via the save dialog and hands the bytes here.
#[tauri::command]
fn write_file(path: String, bytes: Vec<u8>) -> Result<(), String> {
    fs::write(&path, bytes).map_err(|err| format!("write {path}: {err}"))
}

// Stash trace bytes on the loopback server and return the URL Perfetto should fetch. The caller
// then opens `ui.perfetto.dev/#!/?url=<this>` so the trace loads with no manual download/drag.
#[tauri::command]
fn serve_trace(state: State<EditorState>, bytes: Vec<u8>) -> Result<String, String> {
    let port = state.trace_port.ok_or("trace server failed to start")?;
    *state.trace.lock().map_err(|_| "trace lock poisoned")? = Some(bytes);
    Ok(format!("http://127.0.0.1:{port}{TRACE_PATH}"))
}

// Open the project root in VS Code. `code` lives on the host, not in the toolbox, so
// `flatpak-spawn --host` is tried first (same reasoning as open_external). Runs exactly
// `code <path>` — no shell, no argument interpolation.
#[tauri::command]
fn open_in_vscode(path: String) -> Result<(), String> {
    // Engine-reported project paths are relative to the engine's cwd (the repo
    // root); `code` spawns with this process's cwd, so make the path absolute.
    let absolute = {
        let p = PathBuf::from(&path);
        if p.is_absolute() {
            p
        } else {
            repo_root().join(p)
        }
    };
    let candidates: &[&[&str]] = &[&["flatpak-spawn", "--host", "code"], &["code"]];
    let mut last_err = String::from("vs code not found");
    for argv in candidates {
        let (program, pre) = argv.split_first().expect("candidate is non-empty");
        match std::process::Command::new(program)
            .args(pre)
            .arg(&absolute)
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(err) => last_err = format!("{program}: {err}"),
        }
    }
    Err(format!("open {path} in vs code: {last_err}"))
}

// Open the project root in the OS file manager. Same opener chain as `open_external`
// (xdg-open handles a directory path), but the engine-reported root is relative to the
// engine's cwd (the repo root), so make it absolute before handing it off — the file
// manager spawns with this process's cwd, not the engine's.
#[tauri::command]
fn open_project_folder(path: String) -> Result<(), String> {
    let absolute = {
        let p = PathBuf::from(&path);
        if p.is_absolute() {
            p
        } else {
            repo_root().join(p)
        }
    };
    open_url_in_browser(&absolute.to_string_lossy())
}

// Open a URL in the OS default browser. `window.open`/postMessage do not work from the Tauri
// webview, so "Open in Perfetto" hands ui.perfetto.dev to the desktop browser instead. No single
// opener is guaranteed present — running inside a toolbox the container has no xdg-utils, so the
// host handler is reached via `flatpak-spawn --host` — so try the common handlers in turn and
// succeed on the first that spawns.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    open_url_in_browser(&url)
}

/// Opens a URL in the OS default browser, trying the handlers that exist in this
/// environment in turn. Shared by the `open_external` command and the OAuth loopback flow.
pub(crate) fn open_url_in_browser(url: &str) -> Result<(), String> {
    let candidates: &[&[&str]] = &[
        &["flatpak-spawn", "--host", "xdg-open"],
        &["xdg-open"],
        &["gio", "open"],
        &["gnome-open"],
        &["kde-open"],
    ];
    let mut last_err = String::from("no opener found");
    for argv in candidates {
        let (program, pre) = argv.split_first().expect("candidate is non-empty");
        match std::process::Command::new(program)
            .args(pre)
            .arg(url)
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(err) => last_err = format!("{program}: {err}"),
        }
    }
    Err(format!("open {url}: {last_err}"))
}

// Spawn the engine + poll readiness on a background thread, emitting engine-phase /
// viewport-error events. React drives the actual attach (it owns the viewport rect).
fn auto_start(handle: &AppHandle) -> Result<(), String> {
    let state = handle.state::<EditorState>();
    let child = spawn_engine(&state.socket_path)?;
    state
        .engine
        .lock()
        .map_err(|_| "engine lock poisoned".to_string())?
        .replace(child);
    let _ = handle.emit("engine-phase", "starting");

    let monitor = handle.clone();
    let socket = state.socket_path.clone();
    thread::spawn(move || {
        let mut delay = 50u64;
        for _ in 0..40 {
            thread::sleep(Duration::from_millis(delay));
            delay = (delay * 2).min(800);
            let st = monitor.state::<EditorState>();
            if !child_alive(&st.engine) {
                let _ = monitor.emit("viewport-error", "engine exited during startup");
                return;
            }
            if control_request(&socket, "viewport-native-info").is_ok() {
                let _ = monitor.emit("engine-phase", "attaching");
                return;
            }
        }
        let _ = monitor.emit("viewport-error", "engine control socket did not come up");
    });
    Ok(())
}

const STDERR_NOISE: [&str; 2] = ["pci id for fd ", "libEGL warning:"];

// Reroutes this process's stderr through a pipe and forwards every line except known
// Mesa GPU-probe chatter, which the loader prints unconditionally (no env gate) when
// WebKitGTK initializes EGL on an NVIDIA fd. The webview subprocesses and the engine
// inherit the filtered fd, so their stderr is covered too; engine logs go to stdout
// and are untouched. On any setup failure stderr is simply left as it was.
fn install_stderr_noise_filter() {
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;

    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    let real = unsafe { libc::dup(libc::STDERR_FILENO) };
    if real < 0 || unsafe { libc::dup2(fds[1], libc::STDERR_FILENO) } < 0 {
        return;
    }
    unsafe { libc::close(fds[1]) };

    let reader = unsafe { fs::File::from_raw_fd(fds[0]) };
    let mut sink = unsafe { fs::File::from_raw_fd(real) };
    thread::spawn(move || {
        let mut buffered = BufReader::new(reader);
        let mut line = Vec::new();
        let mut dropped_previous = false;
        loop {
            line.clear();
            match buffered.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&line);
            let trimmed = text.trim();
            let noise = STDERR_NOISE
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
                || (dropped_previous && trimmed.is_empty());
            if noise {
                dropped_previous = true;
                continue;
            }
            dropped_previous = false;
            if sink.write_all(&line).is_err() {
                return;
            }
        }
    });
}

pub fn run() {
    saffron_log::init_logging();

    // The webview render path on NVIDIA Wayland. The hardware DMABUF path crashes by default:
    // WebKit enables explicit sync (wp_linux_drm_syncobj) on its EGL render surface, but a
    // non-dmabuf wl_shm buffer reaches that surface and Mutter fatally rejects it — "Protocol
    // error 2 (unsupported_buffer): Explicit Sync only supported on dmabuf buffers" — tearing
    // down the whole shared Wayland connection (our SHM viewport subsurface is innocent but
    // rides the same connection, so it dies too). This is NOT fixed by a newer driver (it
    // reproduces on 590/595+), by WebKitGTK (WONTFIX'd auto-disabling DMABUF on NVIDIA), or by
    // Mutter (spec-required to reject). So by default we steer WebKit onto Mesa's software EGL:
    // a clean transparent webview, fill-rate bound; the engine still renders on the hardware ICD.
    //
    // SAFFRON_WEBVIEW_HW=1 takes the hardware path instead, with NVIDIA explicit sync disabled
    // (__NV_DISABLE_EXPLICIT_SYNC=1) so it no longer crashes. That trades the crash for possible
    // stale-frame ghosting (regions not clearing until hovered) — whether that is tolerable for
    // this UI-over-viewport overlay is a visual judgement, so it stays opt-in. Env values win;
    // AMD/Intel always take the hardware DMABUF path.
    #[cfg(target_os = "linux")]
    {
        let hardware = !nvidia_present() || std::env::var_os("SAFFRON_WEBVIEW_HW").is_some();
        if !hardware {
            if std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").is_none() {
                unsafe {
                    std::env::set_var(
                        "__EGL_VENDOR_LIBRARY_FILENAMES",
                        "/usr/share/glvnd/egl_vendor.d/50_mesa.json",
                    )
                };
            }
            if std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none() {
                unsafe { std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1") };
            }
        } else if nvidia_present() && std::env::var_os("__NV_DISABLE_EXPLICIT_SYNC").is_none() {
            // Without this the NVIDIA hardware path crashes on the explicit-sync protocol error.
            unsafe { std::env::set_var("__NV_DISABLE_EXPLICIT_SYNC", "1") };
        }
        tracing::info!(
            target: "saffron",
            "webview render path: {}",
            if hardware { "hardware" } else { "software (Mesa llvmpipe)" }
        );
    }

    // WebKitGTK's EGL init on NVIDIA prints Mesa GPU-probe chatter to stderr before
    // falling back cleanly. EGL_LOG_LEVEL gates the "libEGL warning:" lines (respecting
    // an explicit value); the bare Mesa loader lines ("pci id for fd …") have no env
    // gate at all, so the stderr filter below drops them instead.
    #[cfg(target_os = "linux")]
    if std::env::var_os("EGL_LOG_LEVEL").is_none() {
        unsafe { std::env::set_var("EGL_LOG_LEVEL", "fatal") };
    }
    install_stderr_noise_filter();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(EditorState::default())
        .manage(WindowStateTracker::default())
        .manage(connectors::ConnectorRuntime::new())
        // Thumbnails / gallery previews load through the shared resource cache instead of hitting
        // the provider CDN directly: the webview requests `saffron-img://fetch/?u=<provider-url>`,
        // and this serves the bytes from disk (fetching once, throttled) so a screenful of tiles
        // never stampedes the provider.
        .register_asynchronous_uri_scheme_protocol("saffron-img", |ctx, request, responder| {
            let cache = ctx
                .app_handle()
                .state::<connectors::ConnectorRuntime>()
                .cache();
            // Take everything after `u=` and percent-decode it. Robust whether the webview hands
            // the URI encoded or already decoded — the provider url's own `?`/`&` can't split it.
            let target = request
                .uri()
                .to_string()
                .split_once("u=")
                .map(|(_, rest)| percent_decode(rest));
            // [vp-dbg] TEMP: count + time every image request so a tab reveal that re-requests the
            // whole grid (and the modal's gallery) shows up here as a burst. Delete with the probe.
            static IMG_REQ_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dbg_n = IMG_REQ_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let dbg_t0 = std::time::Instant::now();
            let dbg_tail: String = target
                .as_deref()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .chars()
                .take(48)
                .collect();
            eprintln!("[vp-dbg] saffron-img #{dbg_n} start …{dbg_tail}");
            tauri::async_runtime::spawn(async move {
                let response = match target {
                    Some(url) => match cache.bytes(&url).await {
                        Ok((bytes, content_type)) => tauri::http::Response::builder()
                            .status(200)
                            .header(tauri::http::header::CONTENT_TYPE, content_type)
                            .header(
                                tauri::http::header::CACHE_CONTROL,
                                "public, max-age=31536000, immutable",
                            )
                            .body(bytes)
                            .unwrap_or_else(|_| img_scheme_error()),
                        Err(_) => img_scheme_error(),
                    },
                    None => img_scheme_error(),
                };
                eprintln!(
                    "[vp-dbg] saffron-img #{dbg_n} done {:.1}ms",
                    dbg_t0.elapsed().as_secs_f64() * 1000.0
                );
                responder.respond(response);
            });
        })
        .invoke_handler(tauri::generate_handler![
            dbg_log,
            control,
            start_engine,
            set_viewport_bounds,
            set_viewport_parked,
            viewport_refresh_hz,
            quit_engine,
            engine_alive,
            app_data_info,
            list_recent_projects,
            remember_recent_project,
            load_editor_settings,
            save_editor_settings,
            write_file,
            open_external,
            open_in_vscode,
            open_project_folder,
            serve_trace,
            store_list_connectors,
            store_search_session,
            store_search_more,
            store_import,
            store_asset_parts,
            store_asset_gallery,
            store_import_part,
            connector_set_secret,
            connector_clear_secret,
            connector_secret_status,
            connector_login
        ])
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                // Restore the remembered geometry (or fill the monitor) before the window is shown,
                // then seed the tracker so a snapshot persists even if no resize/move fires first.
                let seed = configure_main_window(&window);
                *app.state::<WindowStateTracker>()
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = seed;
                let handle = app.handle().clone();
                let tracked = window.clone();
                window.on_window_event(move |event| {
                    if matches!(event, WindowEvent::Resized(_) | WindowEvent::Moved(_)) {
                        capture_window_geometry(&tracked, &handle.state::<WindowStateTracker>());
                    }
                });
                let viewports = Arc::clone(&app.state::<EditorState>().viewports);
                if let Err(err) = wayland_viewport::install(
                    &window,
                    viewport_shm_name("scene"),
                    viewport_shm_name("assetPreview"),
                    &viewports,
                ) {
                    let _ = app.handle().emit("viewport-error", err);
                }
            }
            if let Err(err) = auto_start(app.handle()) {
                let _ = app.handle().emit("viewport-error", err);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build Saffron Anima")
        .run(|handle, event| {
            if let RunEvent::ExitRequested { .. } = event {
                let state = handle.state::<EditorState>();
                teardown(&state);
                let snapshot = handle
                    .state::<WindowStateTracker>()
                    .0
                    .lock()
                    .map(|guard| guard.clone())
                    .unwrap_or_default();
                let _ = write_state_file(&RememberedState {
                    window: Some(snapshot),
                });
            }
        });
}
