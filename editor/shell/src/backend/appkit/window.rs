//! AppKit window-system handles + the window operations whose behavior is macOS-specific.

use crate::ShellError;
use crate::geometry::WindowState;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::dpi::PhysicalPosition;
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::window::{CursorGrabMode, ResizeDirection, Window, WindowAttributes};

/// The raw `NSView` pointer recovered from winit, carried as an address so it stays `Send`. The
/// UI compositor and the viewport presenter hang their `CALayer`s off this view (main-thread
/// AppKit access only).
pub struct Handles {
    ns_view: usize,
}

impl Handles {
    /// Recover the raw AppKit handle. Hard-errors on a non-AppKit window system — this backend is
    /// AppKit-native.
    pub fn new(window: &Window) -> Result<Self, ShellError> {
        let RawWindowHandle::AppKit(wh) = window
            .window_handle()
            .map_err(|e| ShellError::Handle(e.to_string()))?
            .as_raw()
        else {
            return Err(ShellError::UnsupportedWindowSystem);
        };
        Ok(Self {
            ns_view: wh.ns_view.as_ptr() as usize,
        })
    }

    pub(crate) fn ns_view(&self) -> usize {
        self.ns_view
    }

    /// A short description of the recovered handles for the bring-up log line.
    pub fn describe(&self) -> String {
        format!("ns_view={:#x}", self.ns_view)
    }
}

/// The macOS window chrome: native decorations with a transparent titlebar over a full-size
/// content view — real traffic lights and native edge-resize, with the UI (and its tab strip)
/// extending under the titlebar area.
pub fn attributes(base: WindowAttributes) -> WindowAttributes {
    base.with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
        .with_title_hidden(true)
}

/// One-time platform setup after the toplevel exists. AppKit's automatic window dragging is
/// turned off (`NSWindow.isMovable = false`): the UI's tab strip lives in the titlebar region,
/// and the automatic titlebar drag runs *in parallel* with event delivery — a tab drag would
/// reorder the tab and move the window at once. The frontend is the drag authority instead: its
/// empty titlebar regions invoke `window_start_drag` → `drag_window()` →
/// `performWindowDragWithEvent`, which operates regardless of `isMovable`. Edge-resize and the
/// traffic lights are unaffected.
pub fn configure(window: &Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(wh) = handle.as_raw() else {
        return;
    };
    let view = unsafe { wh.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
    if let Some(ns_window) = view.window() {
        ns_window.setMovable(false);
    }
}

/// Native decorations resize at the window edges; the frontend's resize strips are inert here.
pub fn drag_resize(_window: &Window, _direction: ResizeDirection) {}

/// Grab + hide the pointer for the RMB fly-cam (`true`), or release + show it (`false`). macOS
/// supports `Locked` (`CGAssociateMouseAndMouseCursorPosition`); `Confined` does not exist here,
/// so there is no fallback mode.
pub fn set_pointer_lock(window: &Window, locked: bool) {
    if locked {
        let _ = window.set_cursor_grab(CursorGrabMode::Locked);
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}

/// macOS permits self-placement, so the remembered position is re-applied (unlike Wayland, where
/// the compositor owns placement and the captured position stays advisory).
pub fn restore_position(window: &Window, want: &WindowState) {
    window.set_outer_position(PhysicalPosition::new(want.x, want.y));
}

/// The device-scale factor CEF's OSR runs at: the window's backing scale (2 on Retina), so CEF
/// lays out in logical points and paints at full device resolution.
pub fn osr_scale(window: &Window) -> f64 {
    window.scale_factor()
}
