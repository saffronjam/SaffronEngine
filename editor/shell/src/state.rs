//! The per-shell state container — an `Arc<ShellState>` shared with command handlers, plus the
//! engine-facing socket/shm naming and the profiler-trace loopback server — all shell-agnostic and
//! Command handlers receive an `Arc<ShellState>`.

use crate::geometry::WindowStateTracker;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// A request from an IPC worker thread to the main (UI) thread, drained each pump tick. Window ops
/// and JS event emits must run on the thread that owns the winit window + CEF browser; a `cefQuery`
/// handler runs off it, so it posts here instead of touching either directly.
pub enum ShellRequest {
    /// Apply a window control (titlebar buttons / drag).
    Window(WindowAction),
    /// Push an event to the frontend by executing `window.__saffronShellEvent(event, payload)` in
    /// the main frame; `payload` is a ready JSON literal.
    Emit { event: String, payload: String },
}

/// A window control marshaled from the frontend's `getCurrentWindow()` to the main thread.
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    StartDrag,
    /// Begin an interactive resize from the grabbed edge/corner (the frontend's window-frame strips).
    StartResize(ResizeEdge),
    Show,
}

/// Which window edge or corner an interactive resize is dragged from. Maps to winit's
/// `ResizeDirection` in the main thread; a shell-local enum so `state` carries no winit type.
pub enum ResizeEdge {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

impl ResizeEdge {
    /// Parse the frontend's `startResizeDragging(direction)` string; `None` for an unknown value.
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "north" => Self::North,
            "south" => Self::South,
            "east" => Self::East,
            "west" => Self::West,
            "north-east" => Self::NorthEast,
            "north-west" => Self::NorthWest,
            "south-east" => Self::SouthEast,
            "south-west" => Self::SouthWest,
            _ => return None,
        })
    }
}

/// The per-shell state: engine slot + socket + trace server, plus the
/// shared window-geometry tracker (read by `window_scale_factor`/`window_is_maximized`), the
/// main-thread request inbox, and the exit flag `window_close` raises. `ConnectorRuntime` (Phase 7)
/// is added when that module ports.
pub struct ShellState {
    /// Empty until `start_engine` spawns the host.
    pub engine: Mutex<Option<Child>>,
    pub socket_path: String,
    /// The latest profiler-trace bytes, served on the loopback port below so Perfetto fetches them
    /// itself (`?url=`). Replaced on each "Open in Perfetto"; `None` until the first.
    pub trace: Arc<Mutex<Option<Vec<u8>>>>,
    /// The loopback port the trace server bound, or `None` if it could not start.
    pub trace_port: Option<u16>,
    /// The live window geometry, updated by the main loop on resize/scale and read by the window
    /// query commands. Shared so an IPC worker can read it without touching the winit window.
    pub window: Arc<WindowStateTracker>,
    /// Window ops + event emits posted from IPC threads, drained on the main thread each pump tick.
    pub inbox: Mutex<Vec<ShellRequest>>,
    /// Raised by `window_close`; the main loop breaks its pump when set.
    pub exit_requested: AtomicBool,
    /// Per-view viewport geometry the lifecycle commands write and the present loop reads.
    pub viewports: crate::presenter::Viewports,
    /// The Asset Store connector runtime (registry + resource cache + live search sessions).
    pub connectors: crate::connectors::ConnectorRuntime,
}

impl Default for ShellState {
    fn default() -> Self {
        let trace: Arc<Mutex<Option<Vec<u8>>>> = Arc::default();
        let trace_port = start_trace_server(Arc::clone(&trace));
        Self {
            engine: Mutex::new(None),
            socket_path: socket_path(),
            trace,
            trace_port,
            window: Arc::default(),
            inbox: Mutex::new(Vec::new()),
            exit_requested: AtomicBool::new(false),
            viewports: crate::presenter::Viewports::default(),
            connectors: crate::connectors::ConnectorRuntime::new(),
        }
    }
}

impl ShellState {
    /// Stash the latest profiler trace for the loopback server and return the URL Perfetto fetches
    /// (`?url=` handoff). Backs the `serve_trace` command.
    pub fn serve_trace(&self, bytes: Vec<u8>) -> Result<String, String> {
        let port = self.trace_port.ok_or("trace server failed to start")?;
        *self.trace.lock().map_err(|_| "trace lock poisoned")? = Some(bytes);
        Ok(format!("http://127.0.0.1:{port}{TRACE_PATH}"))
    }

    /// Post a request for the main thread to apply on its next pump tick.
    pub fn post(&self, request: ShellRequest) {
        self.inbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
    }

    /// Emit a frontend event (`listen(event, …)`) with a JSON payload — marshaled to the main
    /// thread, executed as `window.__saffronShellEvent(event, payload)` in the browser's main frame.
    pub fn emit(&self, event: &str, payload: Value) {
        self.post(ShellRequest::Emit {
            event: event.to_owned(),
            payload: payload.to_string(),
        });
    }

    /// Read the live window scale factor (advisory DPI hint; served to `window_scale_factor`).
    pub fn window_scale(&self) -> f64 {
        self.window
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scale
    }

    /// Read the live maximized state (served to `window_is_maximized`).
    pub fn window_maximized(&self) -> bool {
        self.window
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .maximized
    }

    /// Raise the exit flag; the main loop breaks its pump on the next tick.
    pub fn request_exit(&self) {
        self.exit_requested.store(true, Ordering::Relaxed);
    }
}

/// Per-PID socket in `XDG_RUNTIME_DIR` so two editor instances get distinct engines/sockets.
pub fn socket_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{dir}/saffron-editor-{}.sock", std::process::id())
}

/// Per-PID, per-view shm segment the engine publishes that view's viewport frames into. The token
/// MUST be the engine's wire name ("scene" / "assetPreview"). Consumed by the Phase-6 engine spawn.
#[allow(dead_code)]
pub fn viewport_shm_name(view: &str) -> String {
    format!("/saffron-viewport-{}-{}", view, std::process::id())
}

const TRACE_PATH: &str = "/trace.perfetto-trace";

/// A loopback HTTP server that serves the most-recent profiler trace with permissive CORS, so
/// Perfetto (opened with `?url=`) fetches and loads it itself. Bound on :9001 because Perfetto's CSP
/// only allows loopback fetches from that port; an ephemeral fallback keeps downloads working if
/// :9001 is taken. The toolbox shares the host network namespace, so a 127.0.0.1 bind is reachable
/// from the host browser. The accept loop runs for the app's lifetime.
pub fn start_trace_server(trace: Arc<Mutex<Option<Vec<u8>>>>) -> Option<u16> {
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
    // origin (ui.perfetto.dev) fetching a loopback address sends a preflight this must echo, or the
    // request is blocked before it reaches us.
    const CORS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, HEAD, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Max-Age: 86400\r\n";

    if method.eq_ignore_ascii_case("OPTIONS") {
        let resp = format!(
            "HTTP/1.1 204 No Content\r\n{CORS}Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(resp.as_bytes())?;
        return stream.flush();
    }

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
