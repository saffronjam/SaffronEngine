//! Window-geometry memory on winit. Only SIZE and MAXIMIZED are
//! acted on; position/monitor are captured but never applied — native Wayland (GNOME/Mutter in
//! particular) gives a client no way to place its own toplevel or choose an output, so the
//! compositor owns placement. Persisted to `appdata/state.json` (transient UI memory, not settings):
//! a missing or corrupt file means no memory, and the window fills the current monitor.

use crate::ShellError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::window::Window;

pub const MAIN_WINDOW_WIDTH: f64 = 1600.0;
pub const MAIN_WINDOW_HEIGHT: f64 = 900.0;
pub const MAIN_WINDOW_MIN_WIDTH: f64 = 1200.0;
pub const MAIN_WINDOW_MIN_HEIGHT: f64 = 720.0;

/// The generic "remember where I left it" bucket. New top-level blocks can be added as more
/// transient state is remembered.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RememberedState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
}

/// The editor window's last-known geometry, in physical pixels so it round-trips 1:1 with
/// `inner_size()`. `scale`/`monitor` are advisory hints (Wayland cannot force an output).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
    #[serde(default)]
    pub scale: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
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

/// The live in-memory geometry snapshot, flushed to `state.json` on exit. Updated on every resize
/// while not maximized; the maximized flag tracks every change.
#[derive(Default)]
pub struct WindowStateTracker(pub Mutex<WindowState>);

pub(crate) fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The app-data root (CEF cache, settings, window state, recent projects). `$SAFFRON_APPDATA_DIR`
/// when set and non-empty — dev points it at the repo's `appdata/` so its state stays in-tree and
/// isolated from an installed build; otherwise the XDG data directory (`$XDG_DATA_HOME`, else
/// `~/.local/share`) under `saffron-anima`, the location an installed build uses. The shell forwards
/// this to the spawned host via `SAFFRON_APPDATA_DIR`, so both agree on one directory.
pub fn app_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SAFFRON_APPDATA_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("saffron-anima")
}

pub(crate) fn userdata_dir() -> PathBuf {
    app_data_dir().join("userdata")
}

pub fn ensure_app_dirs() -> Result<(), ShellError> {
    fs::create_dir_all(userdata_dir())?;
    Ok(())
}

fn state_path() -> PathBuf {
    app_data_dir().join("state.json")
}

/// A missing or corrupt state file falls back to defaults (no remembered window).
pub fn read_state_file() -> RememberedState {
    let Ok(text) = fs::read_to_string(state_path()) else {
        return RememberedState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn write_state_file(state: &RememberedState) -> Result<(), ShellError> {
    ensure_app_dirs()?;
    let text = serde_json::to_string_pretty(state)?;
    fs::write(state_path(), text)?;
    Ok(())
}

/// Re-apply a remembered geometry: SIZE then MAXIMIZED. Size before maximize so the un-maximize
/// restore-size equals the remembered normal size.
pub fn apply_window_state(window: &Window, want: &WindowState) {
    let _ = window.request_inner_size(PhysicalSize::new(want.width, want.height));
    if want.maximized {
        window.set_maximized(true);
    }
}

/// The no-memory default: size to the current (else primary) monitor. Returns the applied geometry
/// so the caller seeds the live tracker (recording the monitor's position/name even though only its
/// size is acted on).
pub fn fill_current_monitor(window: &Window) -> WindowState {
    if let Some(monitor) = window
        .current_monitor()
        .or_else(|| window.primary_monitor())
    {
        let position = monitor.position();
        let size = monitor.size();
        let _ = window.request_inner_size(PhysicalSize::new(size.width, size.height));
        WindowState {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            maximized: false,
            scale: monitor.scale_factor(),
            monitor: monitor.name(),
        }
    } else {
        let _ = window.request_inner_size(LogicalSize::new(MAIN_WINDOW_WIDTH, MAIN_WINDOW_HEIGHT));
        WindowState::default()
    }
}

/// Fold the window's current geometry into the live tracker (called on every resize / scale change).
/// The maximized flag always tracks; x/y/width/height only update while not maximized, because a
/// maximized window reports the maximized bounds and we must remember the last normal geometry.
pub fn capture_window_geometry(window: &Window, tracker: &WindowStateTracker) {
    let mut guard = tracker
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let maximized = window.is_maximized();
    guard.maximized = maximized;
    // Monitor + scale are best-effort hints reflecting current reality regardless of maximized, so a
    // window opened-and-kept maximized never keeps a stale monitor from a previous session.
    guard.scale = window.scale_factor();
    if let Some(monitor) = window.current_monitor() {
        guard.monitor = monitor.name();
    }
    if maximized {
        return;
    }
    let size = window.inner_size();
    if size.width > 0 && size.height > 0 {
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
/// before the window is revealed. Returns the applied geometry so the caller seeds the live tracker.
pub fn configure_main_window(window: &Window) -> WindowState {
    window.set_title("Saffron Anima");
    window.set_min_inner_size(Some(LogicalSize::new(
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
