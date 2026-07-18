//! CEF bootstrap for the AppKit backend. macOS executables never link `libcef`; the browser
//! process loads `Chromium Embedded Framework.framework` from the app bundle at runtime
//! (`cef_load_library`) before any other CEF call. The bundle layout is produced by the `bundle`
//! bin (`cef::build_util::mac`); running the bare `target/debug` binary outside a bundle fails
//! here by design — Chromium's Mach bootstrap requires the bundle identity anyway.

use crate::ShellError;
use cef::library_loader::LibraryLoader;

/// The shell's loop must NOT call `do_message_loop_work` between pumps here: outside
/// `pump_app_events`, winit's handler is uninstalled and every event AppKit dispatches while CEF
/// runs the shared `NSRunLoop` is dropped (input included). CEF is pumped by the run-loop timer
/// `pump::install` schedules instead, and the loop blocks inside the pump.
pub const PUMPS_IN_LOOP: bool = false;

/// Start the `NSTimer`-driven CEF pump on the main run loop (after `initialize`).
pub fn install_cef_pump() {
    super::pump::install();
}

/// Stop the pump timer (before `cef::shutdown`, which drains the loop itself).
pub fn uninstall_cef_pump() {
    super::pump::uninstall();
}

/// Load the CEF framework from `../Frameworks/Chromium Embedded Framework.framework` relative to
/// the executable (the browser-process bundle layout). The loader is intentionally leaked: its
/// `Drop` unloads the framework, and CEF stays loaded for the process lifetime.
pub fn load_cef() -> Result<(), ShellError> {
    let exe =
        std::env::current_exe().map_err(|err| ShellError::Handle(format!("current_exe: {err}")))?;
    // `LibraryLoader::new` canonicalizes the framework path and panics when it is absent; check
    // first so a bare-binary launch fails with an actionable error instead.
    let framework_dir = exe
        .parent()
        .map(|dir| dir.join("../Frameworks/Chromium Embedded Framework.framework"))
        .filter(|dir| dir.exists());
    if framework_dir.is_none() {
        return Err(ShellError::Handle(
            "no CEF framework beside the executable — run from the assembled .app bundle \
             (`cargo run --bin bundle`, then launch Contents/MacOS/saffron-editor-shell)"
                .into(),
        ));
    }
    let loader = LibraryLoader::new(&exe, false);
    if !loader.load() {
        return Err(ShellError::Handle("cef_load_library failed".into()));
    }
    std::mem::forget(loader);
    Ok(())
}
