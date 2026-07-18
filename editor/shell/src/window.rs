//! The host toplevel wrapper: owns the winit `Window` plus the backend's raw window-system
//! handles ([`crate::backend::Handles`]). The backend's compositor + presenter consume the
//! handles; the window-control methods are the surface the IPC bridge calls. Operations whose
//! behavior differs per window system (interactive resize, pointer lock) delegate to
//! `backend::window`.

use crate::ShellError;
use crate::backend;
use std::sync::Arc;
use winit::window::Window;

pub struct ShellWindow {
    window: Arc<Window>,
    handles: backend::Handles,
}

impl ShellWindow {
    /// Wrap the winit toplevel and recover the backend's raw window-system handles.
    pub fn new(window: Arc<Window>) -> Result<Self, ShellError> {
        let handles = backend::Handles::new(&window)?;
        Ok(Self { window, handles })
    }

    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }

    pub fn handles(&self) -> &backend::Handles {
        &self.handles
    }
}

/// Window-control operations exposed as plain winit calls (or backend delegates where the window
/// systems diverge). Wired to the IPC bridge — the custom-titlebar minimize/maximize/close/drag the
/// frontend drives.
impl ShellWindow {
    pub fn minimize(&self) {
        self.window.set_minimized(true);
    }

    pub fn toggle_maximize(&self) {
        self.window.set_maximized(!self.window.is_maximized());
    }

    #[allow(dead_code)]
    pub fn is_maximized(&self) -> bool {
        self.window.is_maximized()
    }

    pub fn drag_window(&self) {
        let _ = self.window.drag_window();
    }

    /// Begin an interactive resize from `direction` — the frontend's window-frame strips drive
    /// this on backends whose windows are borderless; a native-decorations backend no-ops.
    pub fn drag_resize(&self, direction: winit::window::ResizeDirection) {
        backend::window::drag_resize(&self.window, direction);
    }

    /// Grab + hide the pointer for the RMB fly-cam (`true`), or release + show it (`false`). CEF's
    /// windowless OSR can't service DOM pointer lock, so the shell locks the cursor at the winit
    /// level and routes relative motion through `DeviceEvent::MouseMotion`.
    pub fn set_pointer_lock(&self, locked: bool) {
        backend::window::set_pointer_lock(&self.window, locked);
    }

    #[allow(dead_code)]
    pub fn scale_factor(&self) -> f64 {
        self.window.scale_factor()
    }

    /// Replaces `getCurrentWindow().show()`; gated behind CEF's first paint.
    pub fn reveal(&self) {
        self.window.set_visible(true);
    }
}
