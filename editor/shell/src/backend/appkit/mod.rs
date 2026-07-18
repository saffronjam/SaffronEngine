//! The AppKit backend: winit's NSView handle, native window decorations (transparent titlebar +
//! full-size content view + traffic lights), CEF loaded from the app bundle's framework at
//! runtime, macOS virtual keycodes, and the macOS spawn-env/paths. See `backend/mod.rs` for the
//! contract this implements.

pub mod bootstrap;
mod compositor;
pub mod env;
mod iosurface;
pub mod keys;
pub mod presenter;
mod pump;
pub mod window;

pub use compositor::UiCompositor;
pub use window::Handles;
