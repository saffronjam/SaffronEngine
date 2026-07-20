//! CEF bootstrap for the Wayland backend. Linux links `libcef` directly (the cef-dll-sys build
//! script emits the link and copies the runtime beside the binary), so nothing loads at runtime.

use crate::ShellError;

/// The shell's loop pumps CEF itself (`do_message_loop_work` once per iteration): Wayland events
/// ride winit's own calloop queue, so CEF's pump never dispatches winit events and the two loops
/// can interleave freely.
pub const PUMPS_IN_LOOP: bool = true;

/// The loop pumps CEF directly on this platform; nothing to install.
pub fn install_cef_pump() {}

/// Nothing installed, nothing to stop.
pub fn uninstall_cef_pump() {}

/// Nothing to load before the first CEF call on this platform.
pub fn load_cef() -> Result<(), ShellError> {
    Ok(())
}
