# Phase 2 — macOS window + CEF bootstrap + dev bundle

**Status:** COMPLETED

First light on macOS: the shell builds, assembles a dev `.app` bundle, initializes CEF, and paints.

- `backend/appkit/window.rs`: `Handles { ns_view }` from `RawWindowHandle::AppKit` (main-thread);
  window attributes = native decorations with transparent titlebar + full-size content view
  (winit `WindowAttributesExtMacOS`), `resizable(true)`; geometry restore incl. position (macOS
  self-places, unlike Wayland).
- `backend/appkit/bootstrap.rs`: `load_cef()` via `cef::library_loader::LibraryLoader` (browser
  variant; panics outside the bundle layout — dev always runs bundled); macOS CEF switches
  (`--use-mock-keychain` in dev; no Ozone).
- `src/helper_main.rs` (`[[bin]] saffron-editor-shell-helper`, body `cfg(macos)`):
  `LibraryLoader::new(exe, true)` + `execute_process` — the 5 helper `.app`s all run this.
- `src/bin/bundle.rs` (`[[bin]] bundle`, body `cfg(macos)`): assembles
  `Saffron Anima.app/Contents/{MacOS,Resources,Frameworks}` via `cef::build_util::mac` —
  framework copy + helper bundles + Info.plists. `[package.metadata.cef.bundle] helper_name`.
- `main.rs`: `backend::bootstrap::load_cef()?` before `api_hash`; platform switches routed through
  `backend::bootstrap`; NSApplication/`CefAppProtocol` via `cef::application_mac`.
- Provisioning: `cargo run -p export-cef-dir -- --force ~/.local/share/cef` → `CEF_PATH`.

## Verify (macOS)

`cargo run --bin bundle` produces the app with framework + 5 helpers; executing the inner binary
with `SAFFRON_DEV_URL` set logs CEF `initialize` OK, creates the browser, `on_paint` count > 0;
one `cefQuery` roundtrip via `chrome://inspect`. De-risk: winit `pump_app_events` + AppKit run
loop cooperation is this phase's core test.
