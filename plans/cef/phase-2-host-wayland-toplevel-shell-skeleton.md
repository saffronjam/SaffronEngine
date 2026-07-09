# Host-owned raw Wayland toplevel + shell-crate skeleton

**Status:** COMPLETED — the shell skeleton is built and validated headlessly (clean exit 0 under
`weston`), and the toplevel now maps with real CEF pixels (the placeholder-buffer deliverable landed as
Phase 3's `wl_shm` compositor rather than a throwaway clear). The one remaining refinement — the
scheduled `on_schedule_message_pump_work` pump / idle efficiency — is folded into Phase 6 (presenter
clock); the current paced-poll loop services CEF correctly and drives external-begin-frame at target.
Visual confirmation of the mapped toplevel is shared with Phase 3's (pending a real-display run).
**Delivered:** the module split (`main`/`geometry`/`state`/`window`); the winit-owned
transparent/undecorated toplevel + event loop pumping CEF with no deadlock; the verbatim
geometry-persistence port onto winit (`RememberedState`/`WindowState`/`WindowStateTracker`, restore +
capture + flush to `appdata/state.json`, the Wayland size+maximized rule — confirmed: `state.json` is
written on exit); the raw `wl_display`/`wl_surface` extraction via `raw-window-handle` (resolved
non-null, stashed on `ShellWindow` for the Phase-6 presenter — the direct GDK replacement); the
`ShellState` container (engine slot, `socket_path`, trace loopback server — socket + `trace_port`
logged); the window-control methods (`minimize`/`toggle_maximize`/`is_maximized`/`drag_window`/
`scale_factor`/`reveal`, wired to Phase-4 IPC); a `thiserror` `ShellError`; clippy + fmt clean.
Sustained `on_paint` holds ~226–239/s (>> the WebKitGTK 60-cap). **Folded forward by design:** the
transparent placeholder `wl_shm` buffer that *maps* the toplevel is best sourced from real paint, so it
lands with **Phase 3** (CEF `on_paint` → `wl_shm`, alpha preserved) rather than as a throwaway clear;
and the scheduled `on_schedule_message_pump_work` pump / idle efficiency ties to the presenter clock,
so it is refined in **Phase 6** (the current paced-poll loop services CEF correctly and drives
external-begin-frame at target, validated). The `main.rs` browser creation is the Phase-3 seam, kept
because it already paints.

Stand up the new editor shell crate — the one that replaces `editor/src-tauri` — as a `winit`
event loop that owns **exactly one** transparent, undecorated Wayland toplevel, with the CEF process
lifecycle and external message pump running alongside it, geometry persistence ported verbatim, the
window controls exposed as plain `winit` calls, and a per-shell state container standing in for
Tauri's `manage`/`State<T>` dependency injection. This is the surface everything else in the
migration composites onto: Phase 3 paints the React UI into this toplevel via CEF OSR, and Phase 6
re-attaches the engine's viewport subsurfaces *below* it. Phase 2 lands **no** CEF paint, **no**
browser, and **no** engine child — just the host-owned toplevel, its raw `wl_display`/`wl_surface`,
and the scaffolding the later phases hang off.

The current shell owns its toplevel through Tauri/GTK: `tauri.conf.json` declares
`app.windows[0]` (`decorations:false`, `visible:false`, `transparent:true`, `width:1600`,
`height:900`, `minWidth:1200`, `minHeight:720`), Tauri builds a GTK3 toplevel hosting a WebKitGTK
webview, and `wayland_viewport::install` reaches through GDK
(`gdk_wayland_display_get_wl_display` / `gdk_wayland_window_get_wl_surface`) to recover the raw
`wl_display` + `wl_surface`. Phase 2 replaces the *window ownership* half of that: `winit` creates
and owns the toplevel, and `raw-window-handle` yields the same two Wayland pointers directly — no
GTK, no GDK, no webview widget-tree walk.

## Why a second crate exists during the migration (the one sanctioned coexistence)

The repo's NO-LEGACY rule forbids two live code paths for one job. This migration is the sanctioned
exception the plan README frames: the Tauri→CEF cutover is one logical replacement landed across a
dependency-ordered phase set, and the new shell is built up over Phases 2–7 before the old shell is
deleted **wholesale** in Phase 8 (delete `editor/src-tauri`, its `tauri.conf.json`, `capabilities/`,
`build.rs`, and every Tauri dependency in the same change that completes the cutover). Phase 2
therefore **adds** the new crate beside `editor/src-tauri` and touches nothing in the old crate — the
old shell stays the one that actually launches (`just run` still drives `bun run tauri:dev`) until
Phase 8 flips the launch path and removes it. The coexistence is migration scaffolding with a
committed deletion date, not a retained compat path: at no point do both shells run, and the old one
is gone the moment the new one is complete.

## Goal

A launchable `winit`-driven binary in a new crate that:

- Creates one transparent, undecorated toplevel — 1600×900 default, min 1200×720, title
  `Saffron Anima`, built with `visible:false` and revealed only after the loop is live.
- Recovers that toplevel's `wl_display` + `wl_surface` through `raw-window-handle`
  (`RawDisplayHandle::Wayland` / `RawWindowHandle::Wayland`) and stashes them for the Phase-6
  presenter — the direct replacement for the two GDK FFI calls.
- Brings up the CEF process bootstrap from Phase 1 (helper-subprocess early-return + browser-process
  `initialize`) and drives CEF's external message pump from the `winit` loop with no deadlock.
- Restores and persists window geometry against `appdata/state.json` under the exact Wayland rule the
  current shell holds (act on size + maximized only; capture-but-never-apply x/y/monitor).
- Exposes `set_minimized`, `set_maximized`/`is_maximized`, `close`, `drag_window`, `scale_factor`,
  and a `reveal` entry point as ordinary `winit` calls, ready for the Phase-4 IPC bridge to call.
- Constructs the per-shell state container (engine child slot, `socket_path`, `Viewports`, trace
  state) that replaces Tauri's `State<EditorState>` DI, handed to every future command handler.

Nothing in Phase 2 renders the React UI or spawns the host. The toplevel is transparent and empty by
design; the acceptance signal is that it *maps* at the right size, remembers its geometry, and that
CEF's helper processes and pump co-exist with the loop.

## Build plan (grounded in the current code + the Phase-1 CEF bootstrap)

### 1. The new shell crate — `editor/shell/`

Add a standalone crate at `editor/shell/` (package `saffron-editor-shell`, one `[[bin]]`), a sibling
of `editor/src-tauri` and, like it, **not** a member of the engine Cargo workspace (it depends on
engine crates only by path, exactly as `editor/src-tauri/Cargo.toml` pulls `saffron-log` via
`path = "../../engine/crates/log"`). Dependencies:

- **`cef`** (tauri-apps/cef-rs) + its `cef-dll-sys` pair, pinned to the exact 149.x version Phase 1
  provisioned in the toolbox (CEF binaries reached via `CEF_PATH`/`LD_LIBRARY_PATH` baked into the
  justfile recipes). Phase 2 uses only its process-bootstrap and message-pump surface
  (`execute_process`, `initialize`, `shutdown`, `Settings`, `do_message_loop_work`, the
  browser-process handler's `on_schedule_message_pump_work`); the browser/`RenderHandler` surface is
  Phase 3's.
- **`winit` 0.30** and **`raw-window-handle` 0.6** — the exact versions already pinned in
  `engine/Cargo.toml` `[workspace.dependencies]` and used by `saffron-window`
  (`engine/crates/window/src/lib.rs`: `ApplicationHandler`, `ActiveEventLoop`, `EventLoop`,
  `WindowAttributes`, `window_handle()`/`display_handle()`).
- **`wayland-client` 0.31 / `wayland-backend` 0.3 (`client_system`) / `wayland-protocols` 0.32** —
  identical pins to the ones already in `editor/src-tauri/Cargo.toml`, carried forward so the
  Phase-6 presenter (ported from `wayland_viewport.rs`) drives the toplevel surface over the same
  raw `wayland-client` stack it uses today. Phase 2 needs only enough of it to attach one placeholder
  buffer so the toplevel maps (§7).
- **`glib` 0.18** — retained not for GTK but as the Linux external-pump substrate: CEF's
  `on_schedule_message_pump_work(delay)` is wired into a `glib` timeout/fd source, the standard
  cef-rs Linux external-pump pattern (§3). (If Phase 1 settled on a pure `winit`
  `EventLoopProxy` waker instead of a `glib` source, drop this dep and follow that decision — it is
  the one thing §3 defers to Phase 1's proven result.)
- **`serde` / `serde_json`** for the geometry structs, **`libc`** as today, and **`saffron-log`** +
  **`tracing`** for the compact log format the current bridge shares.

The connector/store/keyring/reqwest/tokio/zip stack from `editor/src-tauri/Cargo.toml` is **not**
added yet — the `connectors/` module and its command wrappers port in Phase 4, so Phase 2's manifest
is the window + CEF + geometry + logging set only. No `tauri`, `tauri-build`, `tauri-plugin-dialog`,
`gtk`, `gdk`, or `webkit2gtk`.

Proposed module layout (concrete, so later phases have named seams to fill):

- `src/main.rs` — the CEF process split (§3) and the `EventLoop` construction.
- `src/app.rs` — the `ApplicationHandler` impl: window creation in `resumed`, event routing in
  `window_event`, pump + control-flow in `about_to_wait`, geometry flush in `exiting`.
- `src/window.rs` — the toplevel wrapper: raw-handle extraction, the window-control methods, the
  placeholder map buffer.
- `src/geometry.rs` — `RememberedState`, `WindowState`, `WindowStateTracker`, `read_state_file`,
  `write_state_file`, `state_path`, `apply_window_state`, `fill_current_monitor`,
  `capture_window_geometry`, `configure_main_window` (ported per §4).
- `src/state.rs` — the `ShellState` container (§6).
- `src/cef_pump.rs` — the external-message-pump integration (§3).

Errors are a per-crate `thiserror` enum (`ShellError`) propagated with `?`, not `Result<T, String>` —
the new crate holds the modern-Rust bar that the old `#[tauri::command] -> Result<T, String>`
signatures did not, and no engine-facing behaviour depends on the string shape here (that contract
lives at the IPC boundary, wired in Phase 4).

### 2. The `winit` toplevel + raw Wayland handle extraction

`winit` 0.30 only creates windows from an `ActiveEventLoop` inside `ApplicationHandler::resumed`
(the pattern `saffron-window`'s `Window::new` already follows). In `resumed`, build the toplevel:

```rust
let attrs = WindowAttributes::default()
    .with_title("Saffron Anima")
    .with_inner_size(LogicalSize::new(1600.0, 1600.0 * 9.0 / 16.0)) // 1600×900
    .with_min_inner_size(LogicalSize::new(1200.0, 720.0))
    .with_decorations(false)
    .with_transparent(true)
    .with_resizable(true)
    .with_visible(false);
let window = event_loop.create_window(attrs)?;
```

The four size/title constants port straight from `lib.rs`
(`MAIN_WINDOW_WIDTH`/`MAIN_WINDOW_HEIGHT`/`MAIN_WINDOW_MIN_WIDTH`/`MAIN_WINDOW_MIN_HEIGHT`), and the
`decorations:false` / `visible:false` / `transparent:true` flags are the same three that
`tauri.conf.json` `app.windows[0]` sets — moved from JSON config into `WindowAttributes` because
there is no more `generate_context!`/`tauri.conf.json` to read them from.

Immediately after creation, recover the raw Wayland handles and validate them:

```rust
let RawDisplayHandle::Wayland(dh) = window.display_handle()?.as_raw() else { return Err(ShellError::NotWayland) };
let RawWindowHandle::Wayland(wh)  = window.window_handle()?.as_raw()  else { return Err(ShellError::NotWayland) };
// dh.display : NonNull<c_void>  -> the wl_display  (replaces gdk_wayland_display_get_wl_display)
// wh.surface : NonNull<c_void>  -> the wl_surface  (replaces gdk_wayland_window_get_wl_surface)
```

These two pointers are the exact inputs `wayland_viewport::install` recovers from GDK today; the
current presenter attaches to that foreign `wl_display` via `wayland-backend`'s
`Backend::from_foreign_display` and drives subsurfaces under that `wl_surface`. Phase 2 only
**extracts and stashes** them on the `ShellState`/window wrapper (and logs that they resolved to the
`Wayland` variant, non-null); the presenter that consumes them is Phase 6. A non-Wayland session
(X11/headless) is a hard error here — the whole architecture is Wayland-native, and the current shell
already assumes it. `winit` keeps the `wl_surface` alive for the window's lifetime, so the stashed
pointer stays valid until the window drops on exit.

`about_to_wait` sets `event_loop.set_control_flow(ControlFlow::Wait)` and pumps CEF (§3); the loop is
not a busy spinner. `window_event` handles `Resized`, `CloseRequested`, and `ScaleFactorChanged`
(§4); `winit` on Wayland never emits `Moved` (the compositor owns placement), which is why x/y are
captured-but-never-applied — the same reality the current shell documents around `outer_position()`
erroring on Wayland.

### 3. CEF process bootstrap + external message pump inside the `winit` loop

CEF is multi-process. `main` calls `execute_process` **first**, before the `EventLoop` or any window
exists: it returns `>= 0` in a render/GPU/utility helper (which returns from `main` immediately,
doing no window or engine work) and `-1` in the browser process, which proceeds to run the shell.
This is the single-binary self-relaunch model Phase 1 stood up (or the `cef-helper` exe, whichever
Phase 1 settled) — Phase 2 consumes that decision, it does not re-litigate it.

The browser process then:

1. Builds `Settings { external_message_pump: true, windowless_rendering_enabled: true, .. }` and calls
   `initialize(...)`. `external_message_pump` hands frame-pump control to the shell; the OSR global
   flag is set now so Phase 3 need only add the browser + `RenderHandler`. **No browser is created in
   Phase 2** — CEF is initialized and pumped, but paints nothing.
2. Integrates the pump with the `winit` loop. CEF calls the browser-process handler's
   `on_schedule_message_pump_work(delay_ms)` whenever it needs servicing; the shell wakes the loop
   after `delay_ms` and calls `do_message_loop_work()` from `ApplicationHandler::about_to_wait`. On
   Linux the cef-rs reference wires `on_schedule_message_pump_work` into a `glib` `MainContext`
   timeout/fd source that co-runs with the `winit` loop (the substrate the `glib` dep is retained
   for); the alternative is a `winit` `EventLoopProxy` waker that posts a user event after the delay.
   Phase 1 proved which of these is reliable on this NVIDIA+Wayland build — **Phase 2 lands
   that proven mechanism**, and if neither the external pump nor the uncap-and-sample fallback from
   Phase 1 is being used for frame *production*, the pump here is still needed for CEF's own internal
   work (network, timers, IPC), independent of paint pacing.
3. Calls `shutdown()` after the loop exits (in `exiting`, after the geometry flush), so CEF tears down
   cleanly before the process ends.

The co-existence acceptance signal is a **heartbeat from both sides**: a throttled `tracing` line
from `about_to_wait` (or the `glib` pump source) each time `do_message_loop_work` runs, and a
throttled line from a CEF browser-process callback (`on_schedule_message_pump_work` firing, or a
periodic CEF task) — both advancing, neither starving, proving the two loops interleave without
deadlock. This is the concrete answer to the plan's dominant Phase-2 risk (a CEF pump that blocks the
`winit` loop, or a `winit` `Wait` that never services CEF).

### 4. Geometry persistence ported verbatim (holding the Wayland rule)

The persistence *data* layer is pure `std::fs` + `serde_json` and moves byte-for-byte from `lib.rs`
into `src/geometry.rs`: `RememberedState` (the `{ window: Option<WindowState> }` bucket),
`WindowState` (`x/y/width/height/maximized/scale/monitor`, physical px, its `Default` seeded from the
four window constants), `state_path` (`app_data_dir().join("state.json")`), `read_state_file`
(missing/corrupt ⇒ `RememberedState::default()`), and `write_state_file` (`ensure_app_dirs` then
`to_string_pretty`). `app_data_dir` (`repo_root().join("appdata")`) and its `ensure_app_dirs` port
unchanged. These have zero Tauri coupling — only the *window API* calls around them change.

Rewire the four geometry-application functions from `tauri::WebviewWindow` onto `winit::window::Window`,
preserving each behaviour exactly:

- **`configure_main_window(&Window) -> WindowState`** — `window.set_title("Saffron Anima")`;
  `window.set_min_inner_size(Some(LogicalSize::new(1200.0, 720.0)))`; then, if
  `read_state_file().window` is `Some(want)` with positive extents, `apply_window_state`, else
  `fill_current_monitor`. Called in `resumed`, **before** the window is revealed (the current shell's
  "restore before show, no visible jump" contract — here the window is still `visible:false`).
- **`apply_window_state(&Window, &WindowState)`** — `window.request_inner_size(PhysicalSize::new(w, h))`
  then, if `maximized`, `window.set_maximized(true)`. Size before maximize so the un-maximize
  restore-size equals the remembered normal size — identical to today. (`winit`'s
  `request_inner_size` is the `set_size` equivalent; on Wayland the compositor may adjust, which is
  fine — only size + maximized are advisory here anyway.)
- **`fill_current_monitor(&Window) -> WindowState`** — the no-memory default: `window.current_monitor()`
  (falling back to `primary_monitor()`), read its `.size()`/`.position()`/`.scale_factor()`/`.name()`
  (all present on `winit`'s `MonitorHandle`), `request_inner_size` to the monitor size, and return the
  seeded `WindowState`. Same shape as the current Tauri version.
- **`capture_window_geometry(&Window, &WindowStateTracker)`** — fold current geometry into the live
  snapshot: `guard.maximized = window.is_maximized()`; refresh `scale` from `window.scale_factor()`
  and `monitor` from `window.current_monitor().name()` **every** event (so an always-maximized window
  never keeps a stale monitor); update `width/height` from `window.inner_size()` **only while not
  maximized**; keep last-known x/y (`window.outer_position()` errors on Wayland — hold the prior
  value). This is the current function's exact three-part rule, unchanged.

`WindowStateTracker(Mutex<WindowState>)` ports as-is, but it is now an owned field on the
`ApplicationHandler` struct rather than a Tauri-`manage`d resource. The event wiring:

- The current shell registers `on_window_event(|e| if Resized|Moved { capture })` in `setup` and
  flushes in the `run` closure's `RunEvent::ExitRequested` arm. Phase 2 maps these onto
  `ApplicationHandler`: `window_event` calls `capture_window_geometry` on `WindowEvent::Resized(_)`
  **and** `WindowEvent::ScaleFactorChanged { .. }` (Tauri folded scale into resize; `winit` splits it
  into a distinct event, so both must feed the snapshot — this is the one behavioural addition, and it
  strengthens the "refresh scale every event" rule); `Moved` never fires on Wayland, so it is not
  wired.
- `ApplicationHandler::exiting` replaces `RunEvent::ExitRequested`: clone the tracker snapshot and
  `write_state_file(&RememberedState { window: Some(snapshot) })`. The **engine teardown** the current
  `ExitRequested` arm also does (quit + kill child, unlink socket/shm) is **not** added here — there
  is no engine child in Phase 2; Phase 6 (engine supervision) extends `exiting` with teardown. Phase 2
  flushes geometry only.

`WindowEvent::CloseRequested` calls `event_loop.exit()`, which drives `winit` to `exiting` and the
flush — the same "close ⇒ persist then quit" path the titlebar's close button will later trigger over
IPC (§5).

### 5. Window controls exposed as `winit` calls (for later IPC wiring)

The frontend's custom titlebar (`WindowTitlebar.tsx`, `App.tsx`, `ProjectMenu.tsx`,
`useSubsurfaceBounds.ts`) drives window ops that today reach Tauri's
`getCurrentWindow().minimize()/toggleMaximize()/isMaximized()/close()/startDragging()/scaleFactor()/show()`.
Phase 2 provides each as a method on the window wrapper (`src/window.rs`), callable from the shell,
so the Phase-4 IPC bridge can invoke them without touching window internals:

- `minimize()` → `window.set_minimized(true)`.
- `toggle_maximize()` → `window.set_maximized(!window.is_maximized())`; `is_maximized() -> bool`.
- `close()` → request loop exit (`event_loop.exit()` via the stored proxy / a control-flow flag),
  which runs the geometry flush in `exiting`.
- `drag_window()` → `window.drag_window()` (winit starts an interactive `xdg_toplevel.move` on
  Wayland — the CSD titlebar drag that replaces `startDragging()` / `data-tauri-drag-region`).
- `scale_factor() -> f64` → `window.scale_factor()` (the value `useSubsurfaceBounds` multiplies a
  pane's logical rect by to get device-pixel viewport bounds — it must be the true `wl_output` scale,
  which `winit` reports).
- `reveal()` → `window.set_visible(true)`, the entry point that replaces `getCurrentWindow().show()`.

None of these are *called from JS* in Phase 2 (no IPC bridge yet). For Phase 2's own verification the
loop calls `reveal()` once it is live and the placeholder buffer (§7) is attached, so the transparent
borderless toplevel is observable at the restored/default size. Phase 3 moves the `reveal()` call
behind CEF's first paint (the true "reveal after first React frame" gate); the method itself is
unchanged.

### 6. The per-shell state container (replacing Tauri `manage`/`State<T>`)

Tauri injects `EditorState` (`engine: Mutex<Option<Child>>`, `socket_path: String`,
`viewports: Arc<Viewports>`, `trace: Arc<Mutex<Option<Vec<u8>>>>`, `trace_port: Option<u16>`),
`WindowStateTracker`, and `ConnectorRuntime` via `.manage(...)` + `State<T>` extraction in each
command. There is no such DI in a plain `winit` binary, so Phase 2 builds a `ShellState` the
`ApplicationHandler` owns and later hands (by `Arc` clone) to every command handler:

```rust
struct ShellState {
    engine: Mutex<Option<std::process::Child>>,   // empty until Phase 6 spawns the host
    socket_path: String,                          // socket_path(): per-PID under $XDG_RUNTIME_DIR
    viewports: Arc<Viewports>,                    // the presenter's shared handle (ported in Phase 6)
    trace: Arc<Mutex<Option<Vec<u8>>>>,           // profiler-trace bytes, served on loopback
    trace_port: Option<u16>,                      // the loopback port, or None
    // wl_display / wl_surface raw pointers stashed in §2, consumed by the Phase-6 presenter
}
```

Field-for-field this is `EditorState`. `socket_path()` (per-PID `saffron-editor-{pid}.sock` under
`$XDG_RUNTIME_DIR`) and `viewport_shm_name()` port from `lib.rs` verbatim — they are engine-facing
and shell-agnostic. `Viewports`/`ViewportShared` (`wayland_viewport.rs`) are moved into the new crate
as the shared handle type; Phase 2 constructs a default `Arc<Viewports>` and stashes it, but does
**not** run the presenter worker (that is Phase 6). The trace loopback server (`start_trace_server`,
a `std::net::TcpListener` on `127.0.0.1:9001` with the Chromium-PNA `Access-Control-Allow-Private-Network`
header) is likewise shell-agnostic and may start here (it costs nothing and matches `EditorState::default`),
but its `serve_trace` command handler is Phase 4 — Phase 2 only constructs the `trace`/`trace_port`
slots. `ConnectorRuntime` is **not** constructed here; it and the whole `connectors/` module port in
Phase 4 with the store command wrappers.

The container is created once in `main` (browser process) and moved into the `ApplicationHandler`;
future command handlers receive an `Arc<ShellState>` clone, the direct analogue of `State<EditorState>`.
The single-flight `CONTROL_IO` serialization the control passthrough depends on is **not** a Phase-2
concern (no control command exists yet); it is preserved in Phase 4 where the IPC bridge re-hosts
`control`.

### 7. Mapping the toplevel: the transparent placeholder attach seam

A Wayland toplevel does not appear until a buffer is attached and committed to its `wl_surface` —
`winit` creates the `xdg_toplevel` but does not paint. With no CEF paint and no presenter in Phase 2,
the toplevel would stay unmapped, and the verification ("launch shows a transparent, borderless
toplevel at the restored/default size") could not be met. Phase 2 therefore attaches **one
fully-transparent `wl_shm` buffer** (ARGB8888, alpha 0, sized to the window's physical inner size,
re-sized on `Resized`) to the toplevel `wl_surface` recovered in §2, over the same raw
`wayland-client` stack the presenter uses. This is the minimal top-surface attach — it maps the
window, proves the surface is genuinely transparent (the desktop shows through, because nothing opaque
is painted and no opaque region is set), and confirms geometry restore visually.

This placeholder is not a retained parallel path: it **is** the top-surface buffer seam Phase 3
extends. Phase 3 swaps the buffer *source* from a transparent clear to CEF's `on_paint` BGRA output
(uploaded to a `wl_shm` buffer, alpha preserved), reusing this same attach/commit/resize plumbing —
one code path, the clear replaced by real pixels. Keeping the toplevel's non-opaque region unset here
is exactly the property the engine subsurfaces below depend on in Phase 6, so getting it right now is
load-bearing and permanent.

## Scope

- **New crate** `editor/shell/` (`saffron-editor-shell`): the `winit` `ApplicationHandler` loop, the
  single transparent/undecorated toplevel, raw Wayland handle extraction, CEF process bootstrap +
  external message pump, ported geometry persistence, window-control methods, the `ShellState`
  container, and the transparent placeholder map buffer. `Viewports`/`ViewportShared`,
  `socket_path`/`viewport_shm_name`, and the `RememberedState`/`WindowState` structs + read/write
  helpers move into this crate; the presenter worker, engine spawn, trace/store command handlers, and
  `ConnectorRuntime` do **not** (later phases).
- **No CEF browser, no `RenderHandler`, no `on_paint`** (Phase 3). **No engine child, no presenter
  worker, no viewport subsurfaces** (Phase 6). **No IPC bridge, no command handlers** (Phase 4). **No
  frontend change** (Phase 5) — `editor/src` and the `@tauri-apps/*` imports are untouched.
- **`editor/src-tauri` is not modified and not deleted** — it remains the launching shell until Phase
  8. Phase 2 adds only the new crate and whatever minimal build/launch invocation the milestone gate
  needs (a temporary toolbox `cargo run -p saffron-editor-shell` with `CEF_PATH`/`LD_LIBRARY_PATH`
  set); the justfile dev-loop rewrite and packaging are Phase 8.

## Depends on

- **`phase-1-cef-toolbox-and-osr-foundation.md`** — Phase 2 consumes Phase 1's outputs directly: the
  exact-pinned `cef`/`cef-dll-sys` versions and the toolbox CEF-binary provisioning
  (`export-cef-dir`, `CEF_PATH`/`LD_LIBRARY_PATH` in the justfile), the settled process-split model
  (single-binary self-relaunch vs `cef-helper` exe), and the **proven** external-message-pump
  mechanism (`glib` fd/timeout source vs `winit` `EventLoopProxy` waker, or the uncap-and-sample
  fallback if the external pump proved unreliable). Phase 2 lands the shell skeleton on top of those
  decisions; it does not re-derive them.

## Verification

Against the repo gate (`cargo build`, `cargo clippy -- -D warnings`, the editor `bun run build`), plus
a run that confirms the toplevel maps at monitor refresh:

- **Build + lint clean** — inside the `saffron-build` toolbox with `CEF_PATH`/`LD_LIBRARY_PATH` set
  (from Phase 1): `cargo build -p saffron-editor-shell` and `cargo clippy -p saffron-editor-shell --
  -D warnings` are clean (no `Result<T, String>`, per-crate `thiserror` errors, `unsafe` confined to
  the CEF/Wayland FFI seams). `just engine` stays green trivially (the engine workspace is untouched),
  and `just editor` (`bun run build`) still builds (the frontend is untouched). `just prepare-for-commit`
  covers the workspace + editor TS; the standalone shell crate is fmt+clippy'd explicitly in the
  toolbox as above, mirroring how `editor/src-tauri` is handled outside the engine workspace.
- **Toplevel maps correctly** — a launch shows a **transparent, borderless** toplevel at the
  restored (or, with no `state.json`, current-monitor) size, titled `Saffron Anima`, with the desktop
  visible through it (nothing opaque painted, no opaque region set). Resizing then maximizing and
  relaunching restores the remembered normal size and re-applies maximized; inspecting
  `appdata/state.json` shows the geometry persisted (and confirms **only** size + maximized are
  honoured — a moved x/y or forced monitor is captured in the file but never re-applied, per the
  Wayland rule).
- **CEF + pump co-exist without deadlock** — CEF helper subprocesses spawn (visible in the process
  tree / launch logs) and the two heartbeat streams from §3 (the `winit`/`glib` pump side and a CEF
  browser-process callback) both advance steadily across the whole run, neither starving. Because
  frame pacing is (per Phase 1) tied to the presentation-feedback clock the presenter will drive, and
  no browser paints yet, this phase's heartbeat is the pump-liveness proof, not a UI-fps measurement —
  the true high-refresh confirmation is Phase 9, once the React UI actually paints (Phase 3) and the
  presenter feeds the monitor-rate clock (Phase 6).
- **Milestone gate** — `just engine` then `just prepare-for-commit` clean, plus the standalone shell
  crate's toolbox fmt/clippy. The old `editor/src-tauri` shell still launches unchanged via
  `just run` (Phase 2 touched nothing in it), so the editor as a product is unaffected this phase.

## Risks

- **CEF external pump vs the `winit` loop.** The dominant Phase-2 risk is a message-pump integration
  that deadlocks or starves — CEF servicing that blocks `winit`, or a `ControlFlow::Wait` that never
  wakes to call `do_message_loop_work`. Mitigated by consuming Phase 1's *proven* pump mechanism
  rather than inventing one here, and by the dual-heartbeat verification that fails loudly if either
  side stalls. The pump is needed for CEF's internal work even under the uncap-and-sample paint
  fallback, so it is not optional.
- **A Wayland toplevel will not map with no buffer.** Without §7's placeholder attach the window never
  appears and the geometry-restore verification is unobservable — this is a real `winit`-on-Wayland
  property (the app must present), not an optional nicety. The placeholder is the minimal correct
  answer and is the exact seam Phase 3 fills, so it is not wasted work; getting its **non-opaque
  region** right (unset, so subsurfaces below show through) is load-bearing for Phase 6.
- **`winit`/`raw-window-handle` version skew with `cef`.** The shell links both the `cef` crate and
  `winit` 0.30 / `raw-window-handle` 0.6; Tauri's own `tauri-runtime-cef` pins `winit 0.31-beta`,
  which hints the `cef`↔`winit` pairing can be version-sensitive. Pin to the engine workspace's exact
  `winit`/`raw-window-handle` versions (proven to build with `raw-window-handle` 0.6's
  `RawWindowHandle::Wayland` shape) and confirm the `cef` crate tolerates them; if `cef` forces a
  newer `winit`, resolve the pin in Phase 2 and record it, since every later phase builds on this
  crate.
- **`request_inner_size` is advisory on Wayland.** The compositor may not honour an exact size, and a
  fractional-scale output complicates the logical↔physical mapping the geometry structs store in
  physical px. This matches the current shell's reality (it already treats size as advisory and
  documents `outer_position()` erroring); the correct-scale requirement bites harder in Phase 3 (DPI
  for click hit-testing) and Phase 6 (subsurface device-pixel bounds), where `scale_factor()` must be
  the true `wl_output` scale — Phase 2 only needs the restored size to be visibly right.
- **Non-Wayland sessions.** The crate hard-errors if `display_handle()` is not `Wayland`. That is
  correct for this Wayland-native architecture (the current shell assumes it too), but it means the
  new shell cannot run under X11/XWayland *for its own toplevel* — note that CEF's internal GPU/renderer
  subprocess may still run under XWayland (`--ozone-platform=x11`), a separate concern validated in
  Phase 1 and orthogonal to the host toplevel being native Wayland.
- **The coexistence window.** Two shell crates exist Phases 2–7. The risk is drift — the new crate
  silently diverging from behaviour the old one still owns (geometry rule, window constants). Mitigated
  by porting the geometry structs and constants **verbatim** (not re-deriving them) and by the deletion
  being committed to Phase 8, not open-ended: the feature is not done while `editor/src-tauri` still
  exists, and Phase 2 must not leak any assumption that it will stay.
