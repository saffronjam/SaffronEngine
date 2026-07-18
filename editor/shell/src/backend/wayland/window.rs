//! Wayland window-system handles + the window operations whose behavior is Wayland-specific.

use crate::ShellError;
use crate::geometry::WindowState;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::{CursorGrabMode, ResizeDirection, Window, WindowAttributes};

/// The raw `wl_display` / `wl_surface` pointers recovered from winit, carried as addresses so they
/// stay `Send`. These two integers are the foundation the UI compositor and the viewport presenter
/// build on (`Backend::from_foreign_display`, `WlSurface::from_id`).
pub struct Handles {
    wl_display: usize,
    wl_surface: usize,
}

impl Handles {
    /// Recover the raw Wayland handles. Hard-errors on a non-Wayland session — this backend is
    /// Wayland-native.
    pub fn new(window: &Window) -> Result<Self, ShellError> {
        let RawDisplayHandle::Wayland(dh) = window
            .display_handle()
            .map_err(|e| ShellError::Handle(e.to_string()))?
            .as_raw()
        else {
            return Err(ShellError::UnsupportedWindowSystem);
        };
        let RawWindowHandle::Wayland(wh) = window
            .window_handle()
            .map_err(|e| ShellError::Handle(e.to_string()))?
            .as_raw()
        else {
            return Err(ShellError::UnsupportedWindowSystem);
        };
        Ok(Self {
            wl_display: dh.display.as_ptr() as usize,
            wl_surface: wh.surface.as_ptr() as usize,
        })
    }

    pub(crate) fn wl_display(&self) -> usize {
        self.wl_display
    }

    pub(crate) fn wl_surface(&self) -> usize {
        self.wl_surface
    }

    /// A short description of the recovered handles for the bring-up log line.
    pub fn describe(&self) -> String {
        format!(
            "wl_display={:#x} wl_surface={:#x}",
            self.wl_display, self.wl_surface
        )
    }
}

/// The Wayland window chrome: borderless + transparent — the frontend draws the titlebar, and the
/// compositor blends the UI over the backdrop/viewport subsurfaces below it.
pub fn attributes(base: WindowAttributes) -> WindowAttributes {
    base.with_decorations(false).with_transparent(true)
}

/// One-time platform setup after the toplevel exists. A borderless Wayland toplevel has no
/// native drag regions to disarm — the frontend's strips drive every move/resize — so there is
/// nothing to configure.
pub fn configure(_window: &Window) {}

/// Begin an interactive resize from `direction` (an `xdg_toplevel.resize`) — the borderless
/// toplevel has no server resize edges, so the frontend's window-frame strips drive this.
pub fn drag_resize(window: &Window, direction: ResizeDirection) {
    let _ = window.drag_resize_window(direction);
}

/// Grab + hide the pointer for the RMB fly-cam (`true`), or release + show it (`false`). CEF's
/// windowless OSR can't service DOM pointer lock, so the shell locks the cursor at the winit level:
/// `Locked` freezes it in place and routes relative motion through `DeviceEvent::MouseMotion`
/// (falling back to `Confined` if the compositor won't lock).
pub fn set_pointer_lock(window: &Window, locked: bool) {
    if locked {
        if window.set_cursor_grab(CursorGrabMode::Locked).is_err() {
            let _ = window.set_cursor_grab(CursorGrabMode::Confined);
        }
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

/// Native Wayland gives a client no way to place its own toplevel or choose an output — the
/// compositor owns placement, so a remembered position is captured but never applied.
pub fn restore_position(_window: &Window, _want: &WindowState) {}

/// The device-scale factor CEF's OSR runs at. This backend runs OSR at scale 1: the `wl_shm`
/// compositor attaches buffers at buffer-scale 1, so CEF's view is sized in the same units the
/// buffer presents at.
pub fn osr_scale(_window: &Window) -> f64 {
    1.0
}
