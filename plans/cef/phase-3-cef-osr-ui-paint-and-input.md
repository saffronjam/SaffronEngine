# Phase 3 — CEF OSR paints the React UI into the toplevel + input/DPI

**Status:** COMPLETED — user-confirmed on the real display: the UI paints at the monitor refresh,
keyboard + pointer input work (winit `KeyboardInput`/`ModifiersChanged` → CEF `RAWKEYDOWN`/`CHAR`/
`KEYUP` with a VK map + held-button flag on moves), and dock-tab drag works. Pacing is CEF self-drive
at `windowless_frame_rate` = the monitor refresh (external-begin-frame was removed — it capped ~223;
see phase-1), and `on_paint` damages only CEF's dirty rects (a full-surface damage every frame stalled
Mutter at high refresh × large surface). The core is: a `wl_shm` compositor
(`src/compositor.rs`) uploads CEF's `on_paint` BGRA to the toplevel `wl_surface` as `Argb8888`
(byte-for-byte, alpha preserved, no opaque region set), sharing winit's `wl_display` via
`Backend::from_foreign_display` — **that foreign-display integration is confirmed working on winit's
display** (the load-bearing Phase 3/6 assumption), the first UI commit succeeds, reveal is gated on
first paint, double-buffered, and the lifecycle tears down cleanly (a teardown drop-order segfault was
found and fixed: the foreign-display `Connection` must drop before winit's `EventLoop` frees the
display, and the CEF browser released via `shutdown` first). Sustained ~177 composited paints/s under
`weston`, no protocol errors, exit 0. The Vite dev URL is wired (`SAFFRON_DEV_URL`, else the CSS test
page), and **pointer input is forwarded** winit → CEF (`CursorMoved`/`CursorLeft` →
`send_mouse_move_event`, `MouseInput` → `send_mouse_click_event`, `MouseWheel` →
`send_mouse_wheel_event`, `Focused` → `set_focus`; compiles + runs, awaits interactive validation).
**Remaining:** keyboard + IME forwarding (`send_key_event` — needs the winit→Chromium `windows_key_code`
table); modifier flags on mouse events; cursor-shape via the CEF handler; DPI/fractional-scale for click
hit-testing; the prod app scheme (embed-vs-co-locate); and **visual + interactive confirmation on a real
high-refresh display** that the React UI renders, composites transparently over the viewport, and
responds to input — the parts that need eyes/hands on the running editor, not a headless check.

## Goal

Make the existing React/Vite frontend appear — transparently, at true monitor refresh, and fully
interactive — inside the host-owned Wayland toplevel that Phase 2 stood up. This phase brings CEF from
"initialized but drawing nothing" to "a windowless browser whose pixels are the editor UI, composited by
us onto the toplevel `wl_surface` with alpha preserved, driven by winit input at the correct DPI." No
engine is spawned and no JS↔native IPC bridge exists yet (those are Phases 6 and 4), so the frontend's
own bridge-backed calls (`invoke`, `listen`, `getCurrentWindow().show()`) are inert this phase — but the
DOM mounts and paints, and DOM-level interaction (hover, focus, text entry, dock resize, DOM popovers)
works. That is exactly the surface this phase proves: **paint + input + transparency + refresh rate**,
the three things the whole migration exists to fix.

The page itself needs **zero** changes. `editor/index.html` already sets `html, body, #root { background:
transparent }` precisely so "the viewport panel is a hole down to the engine's subsurface"; every opaque
region is painted by the React panels. CEF renders that transparent page into a BGRA buffer, we upload it
to the toplevel surface without forcing an opaque region, and the transparent regions reveal whatever
sits below — verified this phase with a placeholder colored backdrop, replaced by the real engine
subsurfaces in Phase 6.

## What Phase 2 hands this phase (the seam)

Phase 3 consumes, and does not re-implement, these Phase 2 outputs:

- The **shell crate** (the new CEF/Rust crate that replaces `editor/src-tauri` at the Phase 8 cutover),
  its `main` calling CEF's `execute_process` first so render/GPU/utility helpers early-return, then
  `initialize`/`shutdown` in the browser process.
- One **transparent, undecorated Wayland toplevel** owned by a winit event loop, created `visible:false`,
  with its raw `wl_display` + `wl_surface` reachable via `raw-window-handle`
  (`RawDisplayHandle::Wayland` / `RawWindowHandle::Wayland`) — the replacement for today's GDK FFI
  (`wayland_viewport.rs` externs `gdk_wayland_display_get_wl_display` / `gdk_wayland_window_get_wl_surface`).
- The **CEF message pump integrated into the winit loop** (`Settings { windowless_rendering_enabled: true,
  external_message_pump: true, .. }` + `do_message_loop_work()` driven each loop iteration, via the
  glib fd-source external pump on Linux).
- **Window controls + geometry** on the winit `Window` (`set_visible`, `set_minimized`,
  `set_maximized`/`is_maximized`, `drag_window`, `set_min_inner_size`, `scale_factor`, and the
  `Resized`/`ScaleFactorChanged` events), plus the `wp_viewporter` binding the presenter already relies
  on (`wayland_viewport.rs` `run`).

Nothing in this phase touches the JSON-over-unix-socket control plane, the generated protocol types, or
the frontend's TypeScript beyond loading it unchanged from Vite.

## Build plan

### 1. The `Client` + `RenderHandler`: a windowless, transparent browser

Create the browser windowless with a transparent background and get its pixels through a `RenderHandler`.
Using the pinned `cef` crate (149.x, per Phase 1), the shape is:

```rust
// WindowInfo: no native window — CEF renders offscreen and hands us buffers.
let mut window_info = WindowInfo::default();
window_info.windowless_rendering_enabled = true;
window_info.shared_texture_enabled = false;        // CPU on_paint baseline (accelerated path is Phase 6, gated)
window_info.external_begin_frame_enabled = pacing_is_external_begin_frame; // §3

// BrowserSettings: transparent background is the default in windowless mode unless
// background_color is set opaque — leave alpha 0 so the page's transparent regions stay transparent.
let mut browser_settings = BrowserSettings::default();
browser_settings.background_color = 0x0000_0000;   // ARGB, alpha 0
browser_settings.windowless_frame_rate = target_frame_rate; // §3

// Client carries the RenderHandler (and later the display/keyboard/scheme handlers).
let client = wrap_client!(EditorClient { render: EditorRenderHandler::new(surface_uploader) });

browser_host_create_browser(&window_info, client, &initial_url, &browser_settings, None, None);
```

The `RenderHandler` (declared with the crate's `wrap_render_handler!`-style macro and returned from
`Client::get_render_handler`) implements the callbacks OSR needs:

- **`get_view_rect(browser) -> Rect`** — reports the current UI size in **logical** (DIP) pixels: the
  toplevel's logical inner size from winit (`Window::inner_size()` ÷ `scale_factor`). CEF multiplies this
  by `device_scale_factor` (from `get_screen_info`) to decide its internal render resolution.
- **`get_screen_info(browser) -> ScreenInfo`** — fills `device_scale_factor` from winit's
  `Window::scale_factor()` (fractional on Wayland via `wp_fractional_scale`, which winit 0.30 surfaces),
  so CEF renders the page at physical resolution while all geometry the app hands CEF stays logical.
- **`on_paint(browser, type_, dirty_rects, buffer, width, height)`** — the CPU frame: `type_ ==
  PaintElementType::View` is the page; `Popup` is a native popup layer (§ note below). `buffer` is
  tightly-packed **BGRA** at `width`×`height` **device** pixels; `dirty_rects` are the changed regions in
  device pixels. This drives §2.
- **`on_accelerated_paint(...)`** — implemented as an empty stub this phase (the zero-copy dmabuf upgrade
  is Phase 6, behind a runtime capability probe); `shared_texture_enabled` stays `false` so it is never
  invoked here. NVIDIA's GBM path for accelerated OSR is unreliable (a Phase 1 finding), so the shell
  ships on the CPU `on_paint` path.
- **`on_cursor_change(browser, cursor, type_, custom_info)`** — routed to winit `Window::set_cursor` (§5).

**Popups in OSR.** CEF renders native HTML popups (a real `<select>` dropdown, autofill) as a *separate*
`Popup` paint layer with `on_popup_show` / `on_popup_size` giving its rect, which the compositor of an OSR
host must draw over the view. This is mostly a non-issue here **by design**: `editor/AGENTS.md` mandates
shadcn/ui DOM primitives over native widgets — `components/ui/select.tsx` is a non-modal Popover, tooltips
are Radix `TooltipContent`, dropdowns are `DropdownMenu` — all rendered *inside* the page's DOM and thus
delivered through the normal `View` paint, never the `Popup` path. Implement `on_popup_show`/`on_popup_size`
and composite the `Popup` layer over the `View` buffer for correctness (native `<select>`/IME candidate
windows can still trigger it), but the editor's own menus and selects come through `View` and need no
special handling.

New files (in the Phase 2 shell crate): `src/osr/client.rs` (the `Client` wrapper + handler wiring) and
`src/osr/render_handler.rs` (the `RenderHandler` impl + the paint→surface bridge of §2).

### 2. Paint into the toplevel: `on_paint` → wl_shm, alpha preserved, dirty-rect uploads

The toplevel `wl_surface` **is** the UI layer (the top of the eventual stack; engine subsurfaces go
below in Phase 6). Upload CEF's BGRA buffer to it via `wl_shm`, preserving alpha:

- **Format.** `on_paint`'s BGRA byte order maps directly to `WL_SHM_FORMAT_ARGB8888` on little-endian
  (stored `0xAARRGGBB` → bytes `B,G,R,A`). Reuse the presenter's shm machinery (`wayland_viewport.rs`
  binds `wl_shm` and drives a per-view buffer ring in `run`/`step_view`) for the UI surface's pool — a
  double- or triple-buffered `wl_shm` pool sized to the device-pixel toplevel, recycled fence-safe the
  same way the viewport ring is.
- **Alpha is load-bearing.** Do **not** advertise an opaque region on the toplevel surface (the winit
  equivalent of today's `gdk_window.set_opaque_region(None)` in `install`), or the compositor culls
  everything under the transparent page regions and the viewport hole goes black. Wayland `ARGB8888`
  expects **premultiplied** alpha; verify CEF's OSR output is premultiplied (Chromium historically had a
  Linux OSR bug forcing alpha to `0xFF` — a Phase 1 check) and premultiply on upload if it hands straight
  alpha.
- **Dirty rects.** `on_paint` gives `dirty_rects`; copy only those device-pixel rectangles from the CEF
  buffer into the current shm buffer, then `wl_surface.damage_buffer` exactly those rects, `attach`, and
  `commit`. The editor UI is mostly static (a ~20 Hz focus-gated poll per `state/store.ts`), so partial
  uploads keep the CPU→surface bandwidth low even at 144/240 Hz over a 1600×900+ surface — the dominant
  cost of the CPU path, and the reason to honor damage rather than re-upload the whole frame.
- **Fractional scale.** The CEF buffer is `logical_size × device_scale_factor` device pixels. Bind
  `wp_viewporter` on the toplevel UI surface and set its destination to the **logical** surface size
  (exactly how `step_view` uses `wp_viewporter` to scale each viewport buffer), so a fractional
  `device_scale_factor` composites crisply without integer `buffer_scale` rounding.

New file: `src/osr/surface.rs` (the UI-surface shm ring + damage-tracked upload), factored to share the
pool/format helpers the Phase 6 presenter port will also use.

### 3. Frame production: pace CEF off the monitor-rate clock

OSR does **not** repaint at monitor rate on its own — `windowless_frame_rate` caps at 60 and defaults
low. Drive frames with **the pacing mechanism Phase 1 selected**, wired to a monitor-rate clock:

- **If Phase 1 chose external-begin-frame:** set `external_begin_frame_enabled = true` on `WindowInfo`
  and call `browser.host().send_external_begin_frame()` once per monitor tick. This phase has no engine
  presenter yet (that lands in Phase 6), so the monitor-rate clock is the **toplevel surface's own
  `wl_surface.frame` callback** (request one each commit; it fires at the compositor's refresh cadence),
  optionally cross-checked against `wp_presentation` feedback on the toplevel. This is the same signal
  Phase 6's presenter formalizes as `refresh_mhz` (`wayland_viewport.rs` `refresh_out` /
  `Viewports::refresh_mhz`); Phase 3 uses the raw frame callback directly and Phase 6 unifies the two
  onto one clock.
- **If Phase 1 chose the uncap-and-sample fallback:** append `--disable-frame-rate-limit` and
  `--disable-gpu-vsync` in `App::on_before_command_line_processing`, let CEF free-run `on_paint`, and
  upload the most-recent painted buffer to the toplevel on each `wl_surface.frame` callback (monitor
  rate). Wasteful of render work but independent of the finicky `send_external_begin_frame` path.

Write the chosen wiring; the other is dead. The command-line-switch hook (`App::
on_before_command_line_processing`) also carries the Ozone/GL flags Phase 1 pinned for CEF's GPU
subprocess on this NVIDIA box (e.g. `--ozone-platform`, `--use-angle`).

### 4. Load the frontend: dev Vite URL, prod custom app scheme

Replace `tauri.conf.json`'s `build.devUrl` / `frontendDist` (which `generate_context!` baked in; both are
gone with Tauri) with:

- **Dev** — read a `SAFFRON_DEV_URL` env var / CLI arg and pass it as the browser's initial URL; the
  default target is the existing Vite dev server `http://127.0.0.1:1420` (`editor/vite.config.ts` pins
  `server.port = 1420`, `strictPort`). Vite HMR works unchanged — the React/Vite frontend is untouched.
  The Phase 8 justfile rewrite starts `bun run dev` and launches the shell with this var; Phase 3 can set
  it by hand to bring the UI up.
- **Prod** — register a custom **app scheme** (e.g. `app://saffron/`) via `App::on_register_custom_schemes`
  (registering it standard + secure + CORS-enabled) plus a `SchemeHandlerFactory` / `ResourceHandler`
  that serves `editor/dist` — `index.html` at the root and the hashed JS/CSS with correct `Content-Type`
  (`text/html`, `text/javascript`, `text/css`, `application/wasm`) and an immutable `Cache-Control` for
  hashed assets. Assets are either embedded with `rust-embed` into the shell binary or read from a
  co-located `dist/` dir next to the executable; the embed-vs-co-located choice is an open packaging
  question deferred to Phase 8 — Phase 3 wires the scheme handler behind a single `AssetStore` trait so
  either backing works. Prod packaging is greenfield (today `bundle.active:false`, no `tauri build`
  exists), so there is nothing to port, only to build.

New file: `src/assets/scheme.rs` (the app-scheme registrar + resource handler). The `saffron-img://`
thumbnail scheme is a *separate* handler landing in Phase 7; this phase registers only the app scheme.

### 5. Input, focus, IME, cursor, and DPI: winit → CEF

The host owns the window, so it owns input forwarding. Map winit events onto `BrowserHost` calls:

- **Pointer.** winit `CursorMoved` → `send_mouse_move_event`; `MouseInput` → `send_mouse_click_event`
  (down/up, click count); `MouseWheel` → `send_mouse_wheel_event`. **Coordinates are logical/DIP**: CEF
  mouse events are in view (DIP) coordinates, so convert winit **physical** positions to logical by
  dividing by `scale_factor` before sending. This is the same logical/physical discipline
  `lib/useSubsurfaceBounds.ts` already applies (it reads `getCurrentWindow().scaleFactor()` to convert
  the pane's logical CSS rect into the engine's device-pixel size); getting it wrong yields clicks that
  miss their targets, especially under fractional scale — a real hit-testing correctness item, not a
  checkbox. CEF's `MouseButtonType` covers only Left/Middle/Right; the **side buttons (8/9)** have no CEF
  mouse representation and are deferred to Phase 4/5 as a native event (mirroring today's `mouse-button`
  emit in `wayland_viewport.rs` `install`), where the open question of whether Chromium delivers them to
  the DOM is resolved. Phase 3 forwards Left/Middle/Right/move/wheel only.
- **Keyboard.** winit `KeyboardInput` → `send_key_event` (RawKeyDown/KeyUp) plus a `Char` event for
  text; map the full winit keycode/`KeyCode` space to CEF's `windows_key_code` / native-key-code fields
  (the pinned `cef` crate exposes the full CEF key event, not a truncated enum). Honor layout via winit's
  logical-key text so shortcuts and typed characters both work.
- **Focus.** winit `Focused` → `BrowserHost::set_focus` so the page's focus ring, caret, and keyboard
  routing follow the toplevel.
- **IME.** winit `Ime` (`Preedit` / `Commit`) → `BrowserHost::ime_set_composition` /
  `ime_commit_text`, and report the caret rect back via `on_ime_composition_range_changed` so the
  candidate window positions correctly on Wayland. CJK/dead-key correctness is genuine surface area the
  host now owns; wire the round-trip here and treat candidate-window placement as a follow-up polish item.
- **Cursor.** the `RenderHandler`'s `on_cursor_change` (§1) → winit `Window::set_cursor` with the mapped
  `CursorIcon`, so hovering links/resize handles shows the right pointer.
- **DPI.** feed `Window::scale_factor()` into `get_screen_info().device_scale_factor` (§1) and, on winit
  `ScaleFactorChanged` / `Resized`, call `BrowserHost::was_resized` (and
  `notify_screen_info_changed`) so CEF re-lays-out and re-paints at the new resolution.

New files: `src/input/mod.rs` (the winit→CEF forwarding) and `src/input/keymap.rs` (the winit→CEF keycode
map).

### 6. Reveal on first paint (temporary native trigger)

Today the window starts `visible:false` and the *frontend* reveals it after first React paint:
`App.tsx` `revealEditorWindow` calls `getCurrentWindow().show()`, fired once from the App effect. That
path depends on the `@tauri-apps/api/window` bridge, which does not exist until Phase 4/5. So Phase 3
reveals the toplevel **natively**: on the first `RenderHandler::on_paint` for the `View` layer (page is
up and has pixels), call winit `Window::set_visible(true)` exactly once. Wire the window controls Phase 2
provides so this fires cleanly. This is an explicitly **temporary hardcoded trigger**; Phase 5 restores
the JS-driven reveal (the frontend's `getCurrentWindow().show()` re-pointed onto the bridge shim) and
deletes the native first-paint reveal, per NO-LEGACY.

## Scope

- **Shell crate (Phase 2's crate):** new `src/osr/{client,render_handler,surface}.rs`,
  `src/input/{mod,keymap}.rs`, `src/assets/scheme.rs`; changes to the crate's window/loop entry to create
  the windowless browser, wire input in the winit loop, drive pacing (§3), and reveal on first paint.
- **No frontend changes.** `editor/index.html`, `editor/vite.config.ts`, and all of `editor/src` load
  unchanged; the page's existing transparency is exactly what OSR needs. (`vite.config.ts`'s dead
  `TAURI_` `envPrefix` is removed later, in the Phase 5 de-Tauri pass, not here.)
- **No engine, no IPC bridge, no control plane, no protocol, no `.smat`/mesh change.** `start_engine` and
  the subsurface presenter are Phase 6; the invoke/event/Channel bridge is Phase 4; the frontend import
  swap is Phase 5.

## Depends on

- **[`phase-2-host-wayland-toplevel-shell-skeleton.md`](phase-2-host-wayland-toplevel-shell-skeleton.md)**
  — for the shell crate, the winit-owned transparent Wayland toplevel and its raw `wl_display` /
  `wl_surface`, the CEF library init + message-pump integration, `wp_viewporter`, and the window controls
  this phase paints into and reveals. Phase 3 is the first phase where CEF actually creates a browser and
  produces pixels; everything before it is process/window plumbing.

## Verification

Gate at the phase boundary with `just engine` then `just prepare-for-commit`, plus the shell crate's own
build/lint and the editor bun build, and a run that measures UI fps:

- **Builds clean:** `cargo build` + `cargo clippy -- -D warnings` on the shell crate (unsafe confined to
  the CEF/Wayland FFI seams), and `cd editor && bun run build` unchanged (the frontend is untouched).
- **The UI renders and is interactive over the transparent toplevel.** Launch the shell against the Vite
  dev server; the full editor chrome paints (titlebar, dock, panels, the project-startup modal). Mouse
  and keyboard work at the DOM level: hover/focus states, text entry into an input, opening/closing a
  shadcn `Select`/`DropdownMenu`, dragging a `react-resizable-panels` splitter, tab switching. Bridge-
  backed actions (anything through `invoke`, `listen`, native dialogs, `getCurrentWindow().show()`) are
  expected to be **inert** this phase — the control poll erroring is not a regression, it is the
  no-IPC-yet state Phases 4/5 resolve.
- **Measured UI refresh confirms the high-refresh win.** On a 144/240 Hz output, sample the paint cadence
  — log the interval between `on_paint`/surface commits, or enable the frontend's dev-mode on-page fps
  counter (five clicks on the footer fps counter, or `VITE_SAFFRON_DEV_MODE=1`) — and confirm it tracks
  the monitor rate rather than WebKitGTK's ~62.5 fps. This is the entire motivation for the migration;
  Phase 3 is the first point it can be observed on the real UI.
- **The transparent hole works.** Commit a placeholder solid-color `wl_surface` (a temporary backdrop)
  *below* the toplevel and confirm the transparent page regions — the viewport panel area, any gap
  between panels — reveal that color through the toplevel, while opaque panels occlude it. This proves the
  no-opaque-region + premultiplied-alpha upload is correct before Phase 6 swaps the placeholder backdrop for
  the engine subsurfaces. Remove the placeholder backdrop before the phase closes.

## Risks

- **External-begin-frame reliability on this NVIDIA+Wayland+CEF build is the dominant risk.**
  `send_external_begin_frame` has a history of Linux/Viz breakage (black frames / no `on_paint`). Phase 1
  is the blocking foundation phase that decides the pacing mechanism; Phase 3 wires whichever it proved, and the
  uncap-and-sample fallback (`--disable-frame-rate-limit` + `--disable-gpu-vsync`) is the safety net if
  external-begin-frame regresses on this machine. Do not assume the external path works — verify the
  measured cadence.
- **Alpha/transparency on Linux OSR.** Chromium has forced OSR alpha to opaque on Linux in the past;
  if the transparent-hole check fails, confirm `background_color` alpha 0, that no opaque region is
  advertised on the toplevel, and that the buffer is premultiplied for the compositor — the hole is
  load-bearing for the entire architecture.
- **CPU readback bandwidth.** The CPU `on_paint` path costs a GPU→CPU copy per painted frame plus the
  CPU→surface upload; at 240 Hz over a large surface this is real. Dirty-rect partial uploads (§2) and
  the UI's mostly-static poll model are the mitigations; the accelerated dmabuf path stays a Phase 6
  runtime-gated upgrade, not a Phase 3 dependency.
- **DPI / fractional-scale hit-testing.** CEF renders at `device_scale_factor` to physical pixels while
  input is logical/DIP; a mismatch produces offset clicks or a blurry UI. The winit `scale_factor` →
  `screen_info` feed and the physical→logical mouse conversion (§5) must agree, and Wayland fractional
  scaling (`wp_fractional_scale`) is a case the current GTK path did not fully exercise — verify clicks
  land under a fractional scale before closing the phase.
- **CEF's GPU subprocess Ozone platform.** CEF's internal renderer/GPU process may run under XWayland
  (`--ozone-platform=x11`) even though the host toplevel is native Wayland; the exact Ozone/ANGLE switch
  combination that initializes cleanly and reaches the NVIDIA ICD is a Phase 1 finding this phase
  applies in `App::on_before_command_line_processing`. A wrong combination silently drops OSR to a
  degraded path.
- **IME and cursor fidelity.** Preedit/candidate-window placement and cursor-shape mapping are new host
  responsibilities under OSR; wire the round-trips (§5) and treat CJK candidate-window positioning as a
  follow-up polish item rather than assuming winit↔CEF parity out of the box.
