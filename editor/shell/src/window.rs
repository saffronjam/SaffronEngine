//! The host toplevel wrapper: owns the winit `Window` and recovers its raw `wl_display` /
//! `wl_surface` — reconstructed from winit's raw handles
//! — the raw `wl_display`/`wl_surface` pointers. The Phase-6
//! presenter consumes those handles (`Backend::from_foreign_display` + subsurfaces under the
//! toplevel `wl_surface`); the window-control methods are the surface the Phase-4 IPC bridge calls.

use crate::ShellError;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use std::sync::Arc;
use winit::window::Window;

pub struct ShellWindow {
    window: Arc<Window>,
    /// Raw `wl_display` pointer as an address (kept `Send`), for the Phase-6 presenter's
    /// `Backend::from_foreign_display`.
    wl_display: usize,
    /// Raw `wl_surface` pointer of the toplevel — the presenter drives subsurfaces under it.
    wl_surface: usize,
}

impl ShellWindow {
    /// Wrap the winit toplevel and recover its raw Wayland handles. Hard-errors on a non-Wayland
    /// session — the shell is Wayland-native.
    pub fn new(window: Arc<Window>) -> Result<Self, ShellError> {
        let RawDisplayHandle::Wayland(dh) = window
            .display_handle()
            .map_err(|e| ShellError::Handle(e.to_string()))?
            .as_raw()
        else {
            return Err(ShellError::NotWayland);
        };
        let RawWindowHandle::Wayland(wh) = window
            .window_handle()
            .map_err(|e| ShellError::Handle(e.to_string()))?
            .as_raw()
        else {
            return Err(ShellError::NotWayland);
        };
        Ok(Self {
            window,
            wl_display: dh.display.as_ptr() as usize,
            wl_surface: wh.surface.as_ptr() as usize,
        })
    }

    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }

    pub fn wl_display(&self) -> usize {
        self.wl_display
    }

    pub fn wl_surface(&self) -> usize {
        self.wl_surface
    }
}

/// Window-control operations exposed as plain winit calls. Wired to the Phase-4 IPC bridge (the
/// custom-titlebar minimize/maximize/close/drag the frontend drives); not called yet.
#[allow(dead_code)]
impl ShellWindow {
    pub fn minimize(&self) {
        self.window.set_minimized(true);
    }

    pub fn toggle_maximize(&self) {
        self.window.set_maximized(!self.window.is_maximized());
    }

    pub fn is_maximized(&self) -> bool {
        self.window.is_maximized()
    }

    pub fn drag_window(&self) {
        let _ = self.window.drag_window();
    }

    /// Begin an interactive resize from `direction` (an `xdg_toplevel.resize`, like `drag_window` is
    /// an interactive move) — the borderless toplevel has no server resize edges, so the frontend's
    /// window-frame strips drive this.
    pub fn drag_resize(&self, direction: winit::window::ResizeDirection) {
        let _ = self.window.drag_resize_window(direction);
    }

    /// Grab + hide the pointer for the RMB fly-cam (`true`), or release + show it (`false`). CEF's
    /// windowless OSR can't service DOM pointer lock, so the shell locks the cursor at the winit level:
    /// `Locked` freezes it in place and routes relative motion through `DeviceEvent::MouseMotion`
    /// (falling back to `Confined` if the compositor won't lock).
    pub fn set_pointer_lock(&self, locked: bool) {
        if locked {
            if self
                .window
                .set_cursor_grab(winit::window::CursorGrabMode::Locked)
                .is_err()
            {
                let _ = self
                    .window
                    .set_cursor_grab(winit::window::CursorGrabMode::Confined);
            }
            self.window.set_cursor_visible(false);
        } else {
            let _ = self
                .window
                .set_cursor_grab(winit::window::CursorGrabMode::None);
            self.window.set_cursor_visible(true);
        }
    }

    pub fn scale_factor(&self) -> f64 {
        self.window.scale_factor()
    }

    /// Replaces `getCurrentWindow().show()`. Phase 3 gates this behind CEF's first paint.
    pub fn reveal(&self) {
        self.window.set_visible(true);
    }
}
