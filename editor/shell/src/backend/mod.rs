//! The platform backend: one self-contained module per window system, selected at compile time by
//! `target_os` — the `std::sys` pattern. Exactly one backend exists per binary; the shared shell
//! (`main.rs`, `window.rs`, `state.rs`, `engine.rs`, `commands.rs`) compiles against the surface
//! below on every target, so a backend that drifts from the contract fails that target's build.
//!
//! # Contract — every backend module exports exactly this surface
//!
//! - `Handles` — the backend's raw window-system handles, recovered once from the winit window:
//!   `Handles::new(&winit::window::Window) -> Result<Handles, ShellError>`. Fields are
//!   backend-private; the type is `Send`-safe (raw pointers carried as addresses).
//! - `UiCompositor` — presents CEF's `on_paint` BGRA frames beneath nothing and above everything
//!   the backend stacks below the UI (backdrop, engine views):
//!   - `new(&Handles) -> Result<UiCompositor, ShellError>`
//!   - `paint(&mut self, bgra: &[u8], w: i32, h: i32, dirty: Option<&[cef::Rect]>) -> Result<(), ShellError>`
//!   - `pump_dnd(&mut self) -> Vec<crate::dnd::DndEvent>` — drain OS file-drag steps this tick
//!   - `observe_window_event(&mut self, &winit::event::WindowEvent)` — winit events the backend's
//!     drag source needs (a no-op where drags arrive outside winit)
//! - `presenter::install(&Handles, scene_shm: String, asset_shm: String, &crate::viewport::Viewports)`
//!   — start the engine-viewport present loop (frames from the shm rings, below the UI).
//! - `keys::native_from_keycode(winit::keyboard::KeyCode) -> i32` — the platform-native keycode
//!   CEF's `KeyEvent.native_key_code` expects (Chromium derives DOM `event.code` from it).
//! - `bootstrap::load_cef() -> Result<(), ShellError>` — whatever must happen before the first CEF
//!   call (macOS loads the framework; a linked platform is a no-op).
//! - `bootstrap::PUMPS_IN_LOOP: bool` + `bootstrap::install_cef_pump()` +
//!   `bootstrap::uninstall_cef_pump()` — how CEF's message loop is pumped. `true`: the shell's
//!   loop calls `do_message_loop_work` once per iteration and paces itself with a sleep (Wayland —
//!   CEF's pump never dispatches winit events there). `false`: the loop blocks inside
//!   `pump_app_events` and `install_cef_pump` (called once after `initialize`) schedules the pump
//!   on the platform's run loop, so every event dispatched while CEF pumps lands under winit's
//!   installed handler (AppKit — one shared `NSRunLoop`); `uninstall_cef_pump` (called before
//!   `cef::shutdown`) stops it.
//! - `window::drag_resize(&winit::window::Window, winit::window::ResizeDirection)` — interactive
//!   resize from a window-frame strip (native-resize platforms no-op).
//! - `window::set_pointer_lock(&winit::window::Window, locked: bool)` — grab/hide the pointer for
//!   the RMB fly-cam, honoring the platform's supported grab modes.
//! - `window::restore_position(&winit::window::Window, &crate::geometry::WindowState)` — re-apply a
//!   remembered window position where the platform permits self-placement (Wayland cannot).
//! - `window::osr_scale(&winit::window::Window) -> f64` — the device-scale factor CEF's OSR runs
//!   at: the view rect and input coordinates are physical ÷ this, and `screen_info` reports it
//!   (1.0 where the backend presents scale-1 buffers; the backing scale on AppKit/Retina).
//! - `window::attributes(winit::window::WindowAttributes) -> winit::window::WindowAttributes` —
//!   the backend's window chrome (borderless custom titlebar vs native decorations).
//! - `window::configure(&winit::window::Window)` — one-time platform setup after the toplevel
//!   exists, for what attributes can't express (AppKit disarms automatic titlebar dragging so the
//!   frontend's `window_start_drag` is the one drag authority; Wayland has nothing to configure).
//! - `env::engine_env(&mut std::process::Command)` — platform GPU/loader env for the spawned host.
//! - `env::remove_viewport_shm(name: &str)` — best-effort segment cleanup after a killed engine.
//! - `env::runtime_socket_dir() -> std::path::PathBuf` — where per-PID control sockets live.
//! - `env::platform_data_dir() -> std::path::PathBuf` — the installed-build app-data root.
//! - `env::browser_opener_candidates() -> &'static [&'static [&'static str]]` and
//!   `env::vscode_opener_candidates() -> …` — the `argv` prefixes `os.rs` tries in order.

#[cfg(target_os = "linux")]
#[path = "wayland/mod.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "appkit/mod.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("saffron-editor-shell has no backend for this target OS");

pub use imp::{Handles, UiCompositor, bootstrap, env, keys, presenter, window};
