//! The Wayland backend: winit's Wayland toplevel handles, a `wl_shm` UI compositor with an opaque
//! backdrop subsurface, engine-viewport `wl_subsurface`s paced on frame callbacks, XKB native
//! keycodes, and the Linux spawn-env/paths. See `backend/mod.rs` for the contract this implements.

pub mod bootstrap;
mod compositor;
pub mod env;
pub mod keys;
pub mod presenter;
pub mod window;

pub use compositor::UiCompositor;
pub use window::Handles;
