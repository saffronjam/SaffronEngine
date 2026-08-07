//! The OS window wrapper: a facade over `winit` that publishes input and lifecycle
//! events as typed [`SubscriberList`] signals, plus the `raw-window-handle` pair
//! `ash-window` consumes for surface creation.
//!
//! [`Window::new`] builds a real winit window; [`Window::headless`] builds the
//! windowless facade the editor host takes, where the signals stay usable but the
//! handle accessors report [`HandleError::NotSupported`] rather than a sentinel, so
//! no Vulkan surface is ever built on that path.
//!
//! The event loop that drives `poll → on_update → … → present` is owned by
//! `saffron-host`; this crate provides the signals it publishes into and the
//! translation from a winit [`WindowEvent`] ([`Window::dispatch_window_event`]).

#![deny(unsafe_code)]

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use saffron_signal::SubscriberList;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::keyboard::PhysicalKey;
use winit::window::{Window as WinitWindow, WindowAttributes};

pub use winit::application::ApplicationHandler;
pub use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
pub use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
pub use winit::keyboard;
pub use winit::window::WindowId;

/// The keycode carried by [`Window::on_key_pressed`] / [`Window::on_key_released`]:
/// winit's location-stable physical key identity.
pub type KeyCode = PhysicalKey;

/// Errors from windowed construction.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The OS failed to create the window.
    #[error("failed to create window: {0}")]
    Create(#[from] winit::error::OsError),
}

/// A `Result` whose error is this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The window creation parameters.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// The window title bar text.
    pub title: String,
    /// The initial window width in logical pixels.
    pub width: u32,
    /// The initial window height in logical pixels.
    pub height: u32,
    /// Whether the window starts hidden.
    pub hidden: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Saffron".to_string(),
            width: 1600,
            height: 900,
            hidden: false,
        }
    }
}

/// The OS window and its typed event signals.
pub struct Window {
    handle: Option<WinitWindow>,
    width: u32,
    height: u32,
    should_close: bool,

    /// Fires when the window is asked to close.
    pub on_close: SubscriberList<()>,
    /// Fires on a pixel-size change with the new `(width, height)`.
    pub on_resize: SubscriberList<(u32, u32)>,
    /// Fires on a key press with `(keycode, is_repeat)`.
    pub on_key_pressed: SubscriberList<(KeyCode, bool)>,
    /// Fires on a key release with the keycode.
    pub on_key_released: SubscriberList<KeyCode>,
    /// Fires on a dropped file with its path.
    pub on_file_dropped: SubscriberList<std::path::PathBuf>,
    /// Fires for every raw winit [`WindowEvent`] before typed dispatch.
    pub on_raw_event: SubscriberList<WindowEvent>,
}

impl Window {
    /// Creates a real OS window on the [`ActiveEventLoop`] the host owns.
    pub fn new(event_loop: &ActiveEventLoop, config: &WindowConfig) -> Result<Self> {
        let attributes = WindowAttributes::default()
            .with_title(config.title.clone())
            .with_inner_size(LogicalSize::new(config.width, config.height))
            .with_resizable(true)
            .with_visible(!config.hidden);
        let handle = event_loop.create_window(attributes)?;
        let size = handle.inner_size();
        Ok(Self::with_handle(Some(handle), size.width, size.height))
    }

    /// Creates the windowless facade the editor host takes. The initial size is
    /// `0×0` until a resize event arrives.
    pub fn headless() -> Self {
        Self::with_handle(None, 0, 0)
    }

    fn with_handle(handle: Option<WinitWindow>, width: u32, height: u32) -> Self {
        Self {
            handle,
            width,
            height,
            should_close: false,
            on_close: SubscriberList::new(),
            on_resize: SubscriberList::new(),
            on_key_pressed: SubscriberList::new(),
            on_key_released: SubscriberList::new(),
            on_file_dropped: SubscriberList::new(),
            on_raw_event: SubscriberList::new(),
        }
    }

    /// Returns `true` when a window was created (the standalone host path).
    pub fn is_windowed(&self) -> bool {
        self.handle.is_some()
    }

    /// The current width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The current height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Whether a close has been requested (set by a close/quit event).
    pub fn should_close(&self) -> bool {
        self.should_close
    }

    /// Latches the close request programmatically.
    pub fn request_close(&mut self) {
        self.should_close = true;
    }

    /// Borrows the underlying winit window; `None` when headless.
    pub fn winit_window(&self) -> Option<&WinitWindow> {
        self.handle.as_ref()
    }

    /// Translates a winit [`WindowEvent`] into the typed signals, publishing to
    /// [`on_raw_event`](Self::on_raw_event) first.
    pub fn dispatch_window_event(&mut self, event: &WindowEvent) {
        self.on_raw_event.publish(event.clone());

        match event {
            WindowEvent::CloseRequested => {
                self.should_close = true;
                self.on_close.publish(());
            }
            WindowEvent::Resized(PhysicalSize { width, height }) => {
                self.width = *width;
                self.height = *height;
                self.on_resize.publish((self.width, self.height));
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.dispatch_key(event.physical_key, event.state, event.repeat);
            }
            WindowEvent::DroppedFile(path) => {
                self.on_file_dropped.publish(path.clone());
            }
            _ => {}
        }
    }

    /// Publishes a keyboard transition to the typed key signals. It takes the three
    /// fields the translation reads rather than a `KeyEvent`, which carries a private
    /// field and so cannot be synthesized in a test.
    fn dispatch_key(&self, physical_key: KeyCode, state: ElementState, repeat: bool) {
        match state {
            ElementState::Pressed => {
                self.on_key_pressed.publish((physical_key, repeat));
            }
            ElementState::Released => {
                self.on_key_released.publish(physical_key);
            }
        }
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        match &self.handle {
            Some(window) => window.window_handle(),
            None => Err(HandleError::NotSupported),
        }
    }
}

impl HasDisplayHandle for Window {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        match &self.handle {
            Some(window) => window.display_handle(),
            None => Err(HandleError::NotSupported),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;
    use std::rc::Rc;
    use winit::dpi::PhysicalSize;
    use winit::keyboard::{KeyCode as WinitKeyCode, PhysicalKey};

    #[test]
    fn config_default_is_saffron_1600_by_900() {
        let config = WindowConfig::default();
        assert_eq!(config.title, "Saffron");
        assert_eq!(config.width, 1600);
        assert_eq!(config.height, 900);
        assert!(!config.hidden);
    }

    #[test]
    fn resize_publishes_size_and_updates_state() {
        let mut window = Window::headless();
        let seen = Rc::new(Cell::new((0u32, 0u32)));
        {
            let seen = Rc::clone(&seen);
            window.on_resize.subscribe(move |(w, h)| {
                seen.set((w, h));
                false
            });
        }

        window.dispatch_window_event(&WindowEvent::Resized(PhysicalSize::new(1280, 720)));

        assert_eq!(seen.get(), (1280, 720), "on_resize got (w, h)");
        assert_eq!(window.width(), 1280);
        assert_eq!(window.height(), 720);
    }

    #[test]
    fn key_press_publishes_keycode_and_repeat() {
        let window = Window::headless();
        let seen = Rc::new(Cell::new(None::<(KeyCode, bool)>));
        {
            let seen = Rc::clone(&seen);
            window.on_key_pressed.subscribe(move |payload| {
                seen.set(Some(payload));
                false
            });
        }

        let key = PhysicalKey::Code(WinitKeyCode::Escape);
        window.dispatch_key(key, ElementState::Pressed, true);

        assert_eq!(
            seen.get(),
            Some((key, true)),
            "on_key_pressed got (keycode, is_repeat)"
        );
    }

    #[test]
    fn key_release_publishes_keycode() {
        let window = Window::headless();
        let seen = Rc::new(Cell::new(None::<KeyCode>));
        {
            let seen = Rc::clone(&seen);
            window.on_key_released.subscribe(move |key| {
                seen.set(Some(key));
                false
            });
        }

        let key = PhysicalKey::Code(WinitKeyCode::KeyW);
        window.dispatch_key(key, ElementState::Released, false);

        assert_eq!(seen.get(), Some(key), "on_key_released got the keycode");
    }

    #[test]
    fn close_request_latches_and_fires_on_close() {
        let mut window = Window::headless();
        let fired = Rc::new(Cell::new(0));
        {
            let fired = Rc::clone(&fired);
            window.on_close.subscribe(move |()| {
                fired.set(fired.get() + 1);
                false
            });
        }

        assert!(!window.should_close());
        window.dispatch_window_event(&WindowEvent::CloseRequested);

        assert_eq!(fired.get(), 1, "on_close fired once");
        assert!(window.should_close(), "should_close latched");
    }

    #[test]
    fn dropped_file_publishes_path() {
        let mut window = Window::headless();
        let seen = Rc::new(RefCell::new(PathBuf::new()));
        {
            let seen = Rc::clone(&seen);
            window.on_file_dropped.subscribe(move |path| {
                *seen.borrow_mut() = path;
                false
            });
        }

        let dropped = PathBuf::from("/tmp/model.gltf");
        window.dispatch_window_event(&WindowEvent::DroppedFile(dropped.clone()));

        assert_eq!(&*seen.borrow(), &dropped, "on_file_dropped got the path");
    }

    #[test]
    fn raw_event_sink_fires_before_typed_dispatch() {
        let mut window = Window::headless();
        let order = Rc::new(RefCell::new(Vec::new()));
        {
            let order = Rc::clone(&order);
            window.on_raw_event.subscribe(move |_event| {
                order.borrow_mut().push("raw");
                false
            });
        }
        {
            let order = Rc::clone(&order);
            window.on_close.subscribe(move |()| {
                order.borrow_mut().push("typed");
                false
            });
        }

        window.dispatch_window_event(&WindowEvent::CloseRequested);

        assert_eq!(
            &*order.borrow(),
            &["raw", "typed"],
            "raw sink fires before typed dispatch"
        );
    }

    #[test]
    fn headless_yields_no_surface_handle() {
        let window = Window::headless();
        assert!(!window.is_windowed());
        assert!(
            window.window_handle().is_err(),
            "headless yields no window handle"
        );
        assert!(
            window.display_handle().is_err(),
            "headless yields no display handle"
        );
    }
}
