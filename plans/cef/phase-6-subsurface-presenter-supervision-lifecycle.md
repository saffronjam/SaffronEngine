# Wayland subsurface presenter re-wire + engine supervision + lifecycle commands

**Status:** COMPLETED — scene-in-panel confirmed on the real display. The user ran `just run` on the
Mutter + NVIDIA session and observed the viewport scene render in its panel (`[shell] viewport 'scene'
first subsurface commit`, monitor-paced 144→240 Hz), closing the last on-display step. The subsurface
present loop is
ported to `editor/shell/src/presenter.rs` — a worker thread on a `from_foreign_display` connection
creates one `wl_subsurface` per view below the toplevel (above `compositor.rs`'s backdrop, below the
UI), maps the engine's shm ring, `wp_viewport`-scales each frame to the pane rect, paces on
`wl_surface.frame` callbacks, learns the true refresh from `wp_presentation`, and detaches on park. It
is wired in `resumed()` (`presenter::install`) and its Wayland setup **smoke-passed on the real
Mutter**: globals bind (incl. `wp_viewporter`/`wp_presentation`), the toplevel surface reconstructs,
the subsurfaces create, and it waits cleanly for the engine's shm — no crash. Engine supervision is
ported
to `editor/shell/src/engine.rs` and **hardware-verified**: the shell auto-starts the present-only host
at launch (`auto_start` — spawn with the viewport/shm/socket env + NVIDIA ICD, then a watchdog thread
that polls `viewport-native-info` and emits `engine-phase` `starting`→`attaching` / `viewport-error`
via the Rust→JS event push), the `start_engine` command is the idempotent ensure the loading overlay
calls, `engine_alive` reports `try_wait` liveness, and shell teardown quits→kills→unlinks the
socket/shm. A headless run brought the real host up on the **NVIDIA RTX 3070 Ti** (`vulkan ready — gpu
'NVIDIA GeForce RTX 3070 Ti' (discrete)`) alongside the shell compositing its own UI at ~180 paints/s,
exiting clean. The viewport lifecycle commands (`set_viewport_bounds`/`set_viewport_parked`/
`viewport_refresh_hz`) are wired to a ported `ViewportShared`/`Viewports` data model
(`editor/shell/src/presenter.rs`) — bounds/park write the shared atomic cells and `set_viewport_bounds`
still drives the engine `set-viewport-size` on settled bounds.

**Remaining (display-gated, visual/timing — built + tuned against a real display, not headless):** the
**subsurface present loop** — create a `wl_subsurface` per view *below* the toplevel on the Phase-3
compositor's connection (the `from_foreign_display` mechanism already proven for the UI surface), map
the engine's shm ring, `wp_viewport`-scale each frame to the pane rect from `ViewportShared`, park by
detaching, pace on frame callbacks, and learn the true refresh from `wp_presentation` feedback (feeds
`refresh_mhz`). The data model + its readers (`ViewportShared::read`, `View::wire`, `unpack_pair`) are
in place for it.

---


## Goal

Realize the transparent-UI-over-engine-subsurface architecture on the CEF shell: port the engine-viewport
presenter off GTK/GDK/WebKitGTK onto the host-owned winit Wayland toplevel, spawn and supervise the engine
child under the shell's lifecycle, and wire the six lifecycle/presenter commands onto the Phase-4 IPC
bridge — so the engine's Vulkan viewport frames composite in the viewport pane **below** the transparent
CEF UI at true monitor refresh, exactly as the Tauri build does today, minus GTK.

This is the phase where the migration's whole point becomes observable together: the UI paints at the
monitor rate (CEF OSR, Phase 3) **and** the viewport composites at the monitor rate (this presenter),
clocked off one shared `wp_presentation`-feedback loop. Everything the current presenter does correctly —
the seqlock shm ring reader, per-view subsurfaces stacked below the toplevel, `wp_viewporter` destination
stretch, the opaque backdrop, park/unpark, and the presentation-feedback → `refresh_mhz` derivation —
ports **verbatim**. Only the five inputs `install()` extracts from GTK are re-sourced, and the GTK glue
that produced them is deleted.

## What ports verbatim, what is deleted, what is rebuilt

The presenter today (`editor/src-tauri/src/wayland_viewport.rs`) is already built on raw `wayland-client`
over a **foreign** `wl_display` pointer with a **private** event queue (`Backend::from_foreign_display` +
`Connection::new_event_queue`), coexisting with GDK's own dispatching. That design is precisely what makes
it portable: it never depended on GTK to *drive* Wayland, only to *hand it* a display + surface + widget
offset + a redraw nudge. Under CEF-OSR the shell owns the winit toplevel and its `wl_display` / `wl_surface`
directly, so the same worker gets the same five inputs from a cleaner source.

| Concern | Today (GTK/WebKitGTK) | Phase 6 (winit toplevel + CEF-OSR) |
|---|---|---|
| The presenter worker | `run()` / `step_view()` / `ViewSurface` on a `thread::spawn` over GDK's foreign display | **Unchanged** — same thread, same private queue, over winit's foreign display |
| shm ring reader | `open_shm` / `stat_shm`, seqlock header, 4-slot ring, remap-on-inode-change | **Unchanged** |
| Subsurface stack | scene + assetPreview subsurfaces `place_below` the toplevel; backdrop below both | **Unchanged** (parent = winit toplevel surface) |
| Stretch / pacing / stats | `wp_viewporter` `set_destination`; `wl_surface.frame`; `wp_presentation` feedback → `refresh_mhz` | **Unchanged** |
| `wl_display` source | `gdk_wayland_display_get_wl_display` FFI extern | winit `RawDisplayHandle::Wayland` |
| `wl_surface` source | `gdk_wayland_window_get_wl_surface` FFI extern | winit `RawWindowHandle::Wayland` |
| Webview origin offset | `gtk_window.allocation()` + `webview.translate_coordinates()` | winit toplevel-origin/rect math (zero inset under decorationless CEF-OSR) |
| Parent-commit nudge | `gtk_window.queue_draw()` | winit `Window::request_redraw()` |
| Anti-cull + transparency | `set_app_paintable`, `connect_draw` 2×2 dot, `set_background_color(0)`, `set_opaque_region(None)` | **Deleted** — Phase 2/3's toplevel owns transparency + empty opaque region; real CEF UI content never culls |
| Mouse side-buttons 8/9 | `webview.connect_event` → `emit("mouse-button")` | **Deleted** — winit delivers them natively (see Open questions / Phase 5) |
| WebView discovery | `default_vbox()` + find `WebKitWebView` child | **Deleted** — no widget tree; the shell owns the surface |

## Build plan

### 1. The presenter worker ports intact — re-source `wl_display` + `wl_surface` from winit

`run()` takes `display_addr: usize` and `parent_addr: usize`, reconstructs a `Backend` from the display
pointer, and imports the parent surface with `ObjectId::from_ptr(WlSurface::interface(), parent_addr …)` +
`WlSurface::from_id`. Nothing in `run()`, `step_view()`, `ViewSurface`, `State`, `PresentationStats`,
`open_shm`, `stat_shm`, or `backdrop_pixel_fd` references GTK — they operate purely on those two pointers
and the per-view `Arc<ViewportShared>`. **This entire body moves into the shell crate unchanged.**

The change is where the two pointers come from. Phase 2 owns the winit `Window`; take the raw handles:

- `wl_display`: `window.display_handle()` → `RawDisplayHandle::Wayland(WaylandDisplayHandle { display })`;
  `display.as_ptr() as usize` is the `display_addr` fed to `Backend::from_foreign_display`. This shares
  winit's live connection — mandatory, because a subsurface's parent and child must be on the **same**
  `wl_display` connection. (The current code proves the pattern: the presenter binds its **own**
  `wl_compositor` over the shared connection via its private registry and parents its child surfaces onto
  GTK's foreign parent surface. Under winit the parent surface is winit's toplevel surface; same
  connection, different `wl_compositor` proxy — a valid, already-exercised construction.)
- `wl_surface`: `window.window_handle()` → `RawWindowHandle::Wayland(WaylandWindowHandle { surface })`;
  `surface.as_ptr() as usize` is the `parent_addr`.

Because winit hands us the surface synchronously after window creation, the current `install()` machinery
that **polls** for the GDK window to exist (`glib::timeout_add_local(50ms …)` retrying
`gtk_window.window()`, plus the 20× startup redraw pump) is deleted. The shell spawns the presenter worker
once, right after the toplevel surface exists, with the two pointers and the two per-view
`(View, shm_name, Arc<ViewportShared>)` tuples — the exact `thread::spawn(move || run(...))` shape from
`install()`, minus the GDK-window wait.

Thread model, made explicit because it is the crux of the port: the presenter worker commits its **child**
subsurfaces on its own private queue over the shared connection; the **parent** toplevel surface is owned
and committed by the shell's main thread (Phase 3's CEF-paint loop commits it every UI frame). Subsurface
geometry (position, stretch) is double-buffered on the parent and only adopted on a parent commit — which
the shell's per-frame CEF paint already delivers. This is the identical division of labor as today (GDK's
main-thread paint committed the parent; the presenter worker committed the children); the shell's CEF-paint
loop simply takes GDK's place as the parent-committing main-thread owner. libwayland's `prepare_read`
discipline that already makes the worker's `queue.roundtrip` multi-reader-safe against GDK is equally safe
against winit's own dispatching — the presenter keeps a private queue and never touches winit's.

### 2. Delete the GTK/WebKitGTK glue in `install()`

`install()` is rewritten as a small shell-side setup function (no `tauri::WebviewWindow`, no GTK). These
pieces are **deleted outright**, not translated:

- The `gdk_wayland_display_get_wl_display` / `gdk_wayland_window_get_wl_surface` `unsafe extern "C"` block
  and both call sites — replaced by the raw-window-handle sources in §1.
- `window.gtk_window()`, `window.default_vbox()`, and the `WebKitWebView`-child search — there is no widget
  tree; the shell owns the one surface.
- `gtk_window.set_app_paintable(true)` and `webview.set_background_color(RGBA(0,0,0,0))` — transparency is
  now a property of the shell's toplevel surface: Phase 3 uploads the CEF UI as BGRA with alpha preserved
  and Phase 2/3 leave the toplevel's opaque region empty, so translucent/unpainted UI regions reveal the
  subsurfaces below. The presenter no longer reaches in to configure transparency.
- `gtk_window.connect_draw` (the 0.02-alpha 2×2 anti-cull dot) — deleted. It existed because a fully
  transparent GTK toplevel could be culled by the compositor, starving GTK of frame callbacks. Under
  CEF-OSR the toplevel surface is committed every UI frame carrying real, partly-opaque React content, so
  it is never a fully-transparent surface and never culled — the anti-cull complexity disappears.
- `gdk_window.set_opaque_region(None)` — the shell owns the toplevel and sets its own empty opaque region
  (Phase 2/3). This phase depends on that invariant (no opaque region on the toplevel) for the subsurfaces
  to be visible; it does not re-clear it from the presenter.
- The `webview.connect_event` hook that intercepts GDK buttons 8/9 and `emit("mouse-button", button)` —
  deleted. WebKitGTK swallowed the side buttons for its own history nav; winit delivers pointer buttons
  (including `MouseButton::Back` / `MouseButton::Forward`, i.e. 8/9) to the shell natively, which Phase 3
  forwards into CEF's OSR input. Whether a native `mouse-button` event path is still needed is Phase 5's
  determination (does Chromium surface them to the DOM as buttons 3/4?); either way the GTK hook is gone,
  and there is no GTK-specific replacement to build here.

### 3. Offset + backdrop geometry from winit, not GTK widget math

`ViewportShared` keeps its four packed atomics — `pos`, `size`, `offset`, `window` — and `step_view()`
keeps reading them verbatim (`combined = pos + offset`; `wp_viewport.set_destination` from `size`; the
backdrop stretch from `window`). Only their **producers** change.

- `pos` / `size` continue to arrive from the frontend via `set_viewport_bounds` (§6) — the pane's logical
  CSS rect. Unchanged.
- `offset` — today `update_offset()` computed the webview widget's origin within the toplevel surface from
  `gtk_window.allocation()` + `webview.translate_coordinates(&gtk_window, 0, 0)` (CSD-aware, because GTK
  inset the WebKitWebView inside the vbox / CSD margins). Under CEF-OSR the CEF render area **is** the whole
  toplevel surface — the editor is decorationless (`with_decorations(false)`, the DOM draws its own
  titlebar in `WindowTitlebar.tsx`), so the page origin equals the surface origin and `getBoundingClientRect`
  already reports pane rects in surface-logical coordinates. The webview inset is therefore **zero**. Feed
  `offset` from winit's toplevel-origin math: it is `(0, 0)` today, and stays "CSD-aware" only in the sense
  that if a future build insets the CEF render area within the surface (a shell-drawn margin / server-side
  decoration), that known inset — not a GTK widget's `translate_coordinates` — is what gets written.
- `window` (the toplevel logical size the backdrop stretches to) — today from `gtk_window.allocation()`
  width/height. Now from winit `Window::inner_size()` converted to logical via `Window::scale_factor()`,
  refreshed on `WindowEvent::Resized` and `WindowEvent::ScaleFactorChanged`. Written to both views' shared
  state, exactly as `update_offset()` wrote to both today.

DPI/scale is preserved end to end and needs no presenter change: the presenter positions subsurfaces and
sets viewport destinations in **logical** units (Wayland surface-local), and the engine's device-pixel
render size is computed in `set_viewport_bounds` from `bounds.width * bounds.scale`. The `scale` value
still originates in the frontend `computeBounds` (`useSubsurfaceBounds.ts`) reading the window scale factor;
under the Phase-5 bridge shim that read resolves to winit `Window::scale_factor()` instead of GTK's, and
flows through unchanged.

### 4. The redraw nudge: winit `request_redraw()` replaces `queue_draw()`

Subsurface position and destination are double-buffered on the parent; they only take effect on the next
parent commit. Today three sites nudge that commit with `gtk_window.queue_draw()`:

- the `update_offset` closure (fired from `connect_size_allocate`),
- `set_viewport_bounds` (via `window.run_on_main_thread(|| gtk_window().queue_draw())`),
- `set_viewport_parked` (same pattern).

Replace each with a shell nudge that schedules a parent-surface commit: `Window::request_redraw()`, which
produces a `WindowEvent::RedrawRequested` on the winit loop → the shell's CEF-paint → a parent toplevel
commit that adopts the pending subsurface geometry. The command handlers (§6) run off the IPC bridge
thread, so the nudge must hop to the winit main thread — via the shell's event-loop proxy
(`EventLoopProxy::send_event`) or the equivalent main-thread channel Phase 2 established for cross-thread
window ops — the direct analog of `run_on_main_thread`. Because Phase 3's CEF-paint loop already commits
the parent every UI frame at monitor rate, a missed nudge self-heals on the next frame; the explicit nudge
just avoids a one-frame lag on an otherwise-idle UI.

### 5. Engine supervision — ported verbatim, teardown on the shell close/quit lifecycle

The engine-facing supervision in `lib.rs` is pure `std::process` / `std::fs` / `std::os::unix` and moves
into the shell crate **unchanged** except for the DI seam (Tauri `State`/`AppHandle` → the shell state
container Phase 4 established) and the lifecycle hook (Tauri `RunEvent::ExitRequested` → the winit
close/quit path):

- `spawn_engine(socket_path)` — ported byte-for-byte, including the full env set: `SAFFRON_EDITOR_NATIVE_VIEWPORT=1`,
  `SAFFRON_CONTROL_SOCK`, `SAFFRON_APPDATA_DIR`, `SAFFRON_VIEWPORT_SHM_SCENE` / `SAFFRON_VIEWPORT_SHM_ASSET`
  (from `viewport_shm_name("scene")` / `viewport_shm_name("assetPreview")`), `current_dir(repo_root())`,
  inherited stdout/stderr, and the conditional NVIDIA ICD guard (`VK_ICD_FILENAMES = NVIDIA_ICD` only when
  unset **and** the ICD path exists). This is engine-facing and independent of the UI toolkit — do not
  touch it. (Note: this is distinct from the WebKitGTK webview-render-path env block in `run()` —
  `__EGL_VENDOR_LIBRARY_FILENAMES`, `LIBGL_ALWAYS_SOFTWARE`, `__NV_DISABLE_EXPLICIT_SYNC`, `EGL_LOG_LEVEL`,
  `SAFFRON_WEBVIEW_HW`, `install_stderr_noise_filter` — which is deleted in the cutover, Phase 8; the
  engine ICD guard here has nothing to do with the webview and survives.)
- `socket_path()` (per-PID `$XDG_RUNTIME_DIR/saffron-editor-<pid>.sock`), `viewport_shm_name(view)`
  (per-PID `/saffron-viewport-<view>-<pid>`), `engine_binary()` (`$SAFFRON_ANIMA_BIN` else
  `engine/target/debug/saffron-host`), `repo_root()`, `child_alive()` (liveness via `try_wait`, never
  `Option::is_some`) — all ported unchanged.
- `teardown(state)` — ported unchanged: send control `quit`, `child.kill()` + `child.wait()`, unlink the
  socket, and unlink both `/dev/shm/saffron-viewport-{scene,assetPreview}-<pid>` segments. The one change
  is **when** it runs: today it is called from `RunEvent::ExitRequested` and from `quit_engine`. Under the
  shell it hangs off the winit close/quit lifecycle — `WindowEvent::CloseRequested` (and the loop-exit
  path) invokes `teardown` before the loop returns, and `quit_engine` still calls it directly. Phase 2 owns
  the same close path for the window-geometry flush to `appdata/state.json`; this phase adds the engine
  teardown to it. Both must complete before the process exits so `/dev/shm` and `$XDG_RUNTIME_DIR` are left
  clean (verified below).
- `auto_start(handle)` — ported: spawn the engine, push `engine-phase = starting`, then spawn the
  readiness-poll thread that backs off (50ms → ×2 → cap 800ms, up to 40 tries) polling the control command
  `viewport-native-info`; on success push `engine-phase = attaching`, on child death or timeout push
  `viewport-error`. The three `handle.emit(...)` calls become shell event-push calls (§6). `auto_start`
  runs from the shell's startup, after the toplevel surface exists so the presenter is already spawned.

### 6. Lifecycle + presenter commands on the IPC bridge

Phase 4 stood up the JS↔native invoke dispatch and the browser→render event push and re-hosted the
**portable** commands (the `control` passthrough, fs/settings, store/connectors). This phase adds the six
commands that depend on the engine child **and** the Wayland presenter, registering them as IPC handlers on
the Phase-4 bridge with unchanged names, argument keys, and return shapes (the frontend's
`control/client.ts` wrappers `startEngine` / `setViewportBounds` / `setViewportParked` / `viewportRefreshHz`
/ `quitEngine` / `engineAlive` and their call sites in `LoadingOverlay.tsx`, `useSubsurfaceBounds.ts`,
`RenderPanel.tsx`, `App.tsx`, `store.ts` do not change — only the transport underneath does):

- `start_engine` — spawn if `!child_alive`, store the `Child` in shell state. Body unchanged.
- `quit_engine` — `teardown(&state)`. Body unchanged.
- `engine_alive` — `child_alive(&state.engine)`. Body unchanged.
- `set_viewport_bounds(view, bounds: ViewportBounds { x, y, width, height, scale }, resize_engine)` —
  resolve the wire view token via `viewport_for` (`View::from_wire`, rejecting anything but
  `scene`/`assetPreview`), write `ViewportShared::set_bounds` (rounded logical rect), request the parent
  redraw nudge (§4), and — only when `resize_engine` — round-trip `set-viewport-size` with the device-pixel
  `(width*scale, height*scale)`. The **only** edit versus today is swapping the `run_on_main_thread → gtk_window().queue_draw()`
  nudge for the winit `request_redraw` proxy hop; the rest is verbatim.
- `set_viewport_parked(view, parked)` — resolve the view, `ViewportShared::set_parked`, request the redraw
  nudge. Same single-line nudge swap.
- `viewport_refresh_hz()` — return `refresh_mhz() as f64 / 1000.0`. Body unchanged. **Keep the presenter's
  feedback path that produces it** (see §7): even if Chromium's true-rate `requestAnimationFrame` makes the
  UI-side read redundant (an Open question, verified below), the **engine** still consumes this value for
  its Default-fps target-fps mode — `RenderPanel.tsx` `resolveTargetFps` reads `viewportRefreshHz()` and
  feeds it to the engine's target-fps setting — so the command and the `wp_presentation` → `refresh_mhz`
  derivation both stay.

The three shell→UI events these paths emit — `engine-phase` (`starting` | `attaching`),
`viewport-error` (string), and (pending Phase 5) `mouse-button` (8 | 9) — go through the Phase-4 event push
(browser→render `CefProcessMessage` → the injected `window.__saffron` bootstrap → DOM `CustomEvent` the
`listen()` shim consumes). The frontend consumers are unchanged: `App.tsx` `listen<EnginePhaseEvent>("engine-phase")`
/ `listen<string>("viewport-error")` gate the startup modal and the reveal, and the presenter-attach
sequencing there relies on a control-command **probe** (`viewport-native-info`) as its gate, not on event
buffering, so the push need not be replayed to late subscribers — preserve that (the events are advisory
progress, the probe is the real readiness signal).

### 7. Unify the frame clock: drive CEF external-begin-frame off the presentation-feedback loop

The presenter's `State` (`Dispatch<WpPresentationFeedback>`) already reads each presented frame's `refresh`
(ns/vblank) and publishes the output's true refresh into `Viewports::refresh_mhz` (mHz), plus per-second
presented/discarded/vblank-Δ stats gated on `SAFFRON_VIEWPORT_STATS`. This is the one authoritative
monitor-rate clock in the shell, and the entire reason it exists — WebKitGTK's `requestAnimationFrame`
could not see the true rate.

If Phase 1 selected **external-begin-frame** for CEF OSR (`external_begin_frame_enabled` +
`BrowserHost::send_external_begin_frame`), clock it off this same loop so the UI and the viewport share one
cadence: pace `send_external_begin_frame` at the presenter's reported refresh (drive it from the
presentation-feedback signal / the derived `refresh_mhz`, not a fixed `windowless_frame_rate`). The result
is a single presentation-fed heartbeat: CEF produces one UI frame per monitor vblank, the presenter commits
one viewport frame per vblank, and both composite together. Because the presenter worker owns the feedback
events on its private queue, expose the clock to the shell main thread via the existing
`Viewports::refresh_mhz` atomic (and, if per-vblank pacing rather than rate-derived pacing is chosen, a
lightweight main-thread signal the worker sets on each `Presented`), so the CEF begin-frame issuance stays
on the browser UI thread where CEF requires it.

If Phase 1 instead selected the **uncap-and-sample** fallback (`--disable-frame-rate-limit` +
`--disable-gpu-vsync`, sampling the latest painted OSR buffer at monitor rate), this section reduces to:
keep the presentation-feedback path exactly as-is for `refresh_mhz` / the engine's Default-fps mode and the
`SAFFRON_VIEWPORT_STATS` diagnostics, and let CEF free-run; the presenter is unaffected either way. The
feedback path is load-bearing for the engine regardless of which pacing CEF uses — it is never removed.

### 8. Optional — accelerated dmabuf presenter path behind a runtime capability probe

Baseline is the CPU path that already works: CEF `on_paint` → `wl_shm` upload for the toplevel UI surface
(Phase 3), and the engine's own `wl_shm` viewport frames for the subsurfaces (this presenter, unchanged).
That path ships and is the always-available fallback.

As an optional zero-copy upgrade, behind the Phase-1 runtime capability probe on this specific
NVIDIA + Mutter stack, consume CEF's `on_accelerated_paint` (`AcceleratedPaintNativePixmapPlaneInfo`, the
Linux dmabuf/native-pixmap planes) and import the UI frame as a `zwp_linux_dmabuf_v1` `wl_buffer` attached
to the toplevel surface — avoiding the GPU→CPU→GPU round-trip for the UI layer. This is gated, not
default: the accelerated shared-texture OSR path is documented to fail on NVIDIA's GBM backend, so the
probe must **prove** a usable dmabuf on this GPU before the shell attaches one, and must fall back to
`on_paint` → `wl_shm` when it does not. The engine's viewport subsurfaces are untouched by this either way —
they remain `wl_shm` frames from the engine's shared-memory ring; only the **UI-layer** upload can be
upgraded. This section adds no presenter-worker change; it is a toplevel-surface upload-path choice owned by
the shell's CEF-paint loop, listed here because it composes with the presentation-fed cadence in §7.

## Scope

- **New shell crate (Phase 2's replacement for `editor/src-tauri`):** the presenter module (today
  `wayland_viewport.rs`) moves in with `run()` / `step_view()` / `ViewSurface` / `State` /
  `PresentationStats` / `Viewports` / `ViewportShared` / `View` / `open_shm` / `stat_shm` /
  `backdrop_pixel_fd` intact; `install()` rewritten as a GTK-free setup that spawns the worker with the
  raw-window-handle display/surface pointers; the GDK externs and all GTK glue deleted. The supervision
  functions (`spawn_engine`, `socket_path`, `viewport_shm_name`, `engine_binary`, `repo_root`,
  `child_alive`, `teardown`, `auto_start`) move in with the DI + lifecycle seams re-pointed. The six
  lifecycle/presenter commands register on the Phase-4 IPC bridge.
- **Frontend:** no code change. `client.ts` wrappers, `useSubsurfaceBounds.ts`, `RenderPanel.tsx`,
  `App.tsx`, `LoadingOverlay.tsx`, and `store.ts` keep calling the same command names / event names; the
  Phase-5 bridge shim already re-points the underlying transport and `scaleFactor()`.
- **Engine, control plane, protocol types, `.smat`/mesh formats:** untouched. `SAFFRON_EDITOR_NATIVE_VIEWPORT`,
  the shm wire tokens (`scene` / `assetPreview`), the seqlock header layout, `set-viewport-size`, and
  `viewport-native-info` are engine-facing invariants preserved byte-for-byte.
- **Not in scope:** the frontend import de-Tauri swap (Phase 5), the wholesale Tauri deletion and the
  WebKitGTK render-path env block removal (Phase 8), and the final high-refresh confirmation (Phase 9).

## Depends on

- **`phase-2-host-wayland-toplevel-shell-skeleton.md`** — the host-owned winit Wayland toplevel whose
  `wl_display` / `wl_surface` this presenter rides, its decorationless + empty-opaque-region surface config,
  its main-thread cross-thread hop (`EventLoopProxy` / equivalent) for `request_redraw`, and the
  window-close path this phase adds engine teardown to.
- **`phase-3-cef-osr-ui-paint-and-input.md`** — CEF OSR painting the transparent React UI onto the toplevel
  surface (alpha preserved, no opaque region) so the subsurfaces show through, the per-frame parent-surface
  commit that adopts subsurface geometry, and the winit→CEF input forwarding that makes the GTK
  mouse-button hook unnecessary.
- **`phase-4-ipc-bridge-and-command-surface.md`** — the invoke dispatch + `{message, code}` rejection
  shape, the browser→render event push (`engine-phase` / `viewport-error` / `mouse-button`), and the shell
  state container that replaces Tauri `State`/`AppHandle` DI.

The Phase-1 outcomes (external-begin-frame vs uncap-and-sample; dmabuf usable or not) parametrize
§7 and §8 but do not block the rest of this phase.

## Verification

Grounded in the repo gate (`just engine` → workspace build + shaders; `just prepare-for-commit` → `cargo fmt`
+ `cargo clippy --workspace -- -D warnings` + oxfmt/oxlint; `bun run build` for the editor), plus a run that
observes the composite and measures UI refresh:

- **Build + lint clean:** the shell crate builds and passes `clippy -D warnings` with `unsafe` confined to
  the Wayland/raw-window-handle and CEF FFI seams; the editor `bun run build` is green (no frontend change,
  so this is a no-op regression check).
- **Composite correctness (GPU-with-eyes):** with the shell running, engine frames appear in the viewport
  pane **below** the transparent CEF UI — the viewport hole shows the live render, and the native gizmo
  overlay composites over the render intact. Panel seams and a parked view's hole resolve against the
  opaque backdrop, not the desktop.
- **Geometry / park / DPI:** resizing, dock-splitting, and DPI/fractional-scale changes of the viewport
  pane reposition and stretch the subsurface correctly (the `set_viewport_bounds` path + the winit
  `request_redraw` nudge); switching a tab **parks** the inactive view (its subsurface detaches, the pane
  paints opaque against the backdrop) and unparks cleanly (the retained frame re-attaches instantly);
  moving the window across outputs of different refresh updates `refresh_mhz`.
- **Lifecycle drive-through:** `start_engine` / `quit_engine` / `engine_alive` spawn, tear down, and report
  liveness from the UI (`LoadingOverlay.tsx`'s restart, `store.ts`'s liveness poll); the startup modal
  advances on `engine-phase` `starting` → `attaching` and the reveal fires, with `viewport-error` surfaced
  on a boot failure.
- **Clean teardown (the explicit gate):** after closing the shell window, `/dev/shm` contains **no**
  `saffron-viewport-{scene,assetPreview}-<pid>` segments and `$XDG_RUNTIME_DIR` contains **no**
  `saffron-editor-<pid>.sock` — confirm with `ls /dev/shm $XDG_RUNTIME_DIR` before and after exit. The
  engine child is killed and reaped (no orphaned `saffron-host`).
- **True refresh confirmed:** `SAFFRON_VIEWPORT_STATS=1` shows the presenter presenting at the monitor rate
  (e.g. `refresh 144 Hz` / `presented ~144/s`, `discarded` near zero) on a 144/240 Hz output — the
  viewport composites at true refresh, and (with §7's external-begin-frame path) the CEF UI shares that
  cadence rather than the WebKitGTK ~62.5 Hz cap. (The end-to-end UI-fps confirmation is Phase 9's; this
  phase confirms the presenter clock and the composite.)
- **Milestone gate:** `just engine` then `just prepare-for-commit` clean at the phase boundary.

## Risks

- **Parent/child commit coordination across two threads.** The presenter worker commits child subsurfaces
  on its private queue while the shell's CEF-paint commits the parent toplevel; subsurface geometry only
  lands on a parent commit. If the shell's paint loop can idle (no CEF frame to commit) while a
  `set_viewport_bounds` nudge is pending, subsurface geometry could lag. Mitigation: the winit
  `request_redraw` nudge (§4) forces a parent commit on the next loop turn, and Phase 3's paint loop commits
  every UI frame anyway; the current GTK build handles the identical hazard with `queue_draw`. Validate the
  no-lag case with an interactive dock-drag of the viewport pane.
- **External-begin-frame reliability on NVIDIA+Wayland+CEF (§7).** CEF has a documented history of black
  frames / no `OnPaint` when driven by `send_external_begin_frame` on Linux/Viz. This is the dominant risk
  and is why Phase 1 is a blocking foundation phase with a proven uncap-and-sample fallback; §7 is written to work with
  either outcome, and the presenter's feedback path is unaffected by the choice.
- **dmabuf OSR on NVIDIA (§8).** Accelerated shared-texture OSR is documented to fail on NVIDIA's GBM path,
  so the accelerated UI-upload path is gated behind a runtime probe and the CPU `on_paint` → `wl_shm` path
  is the always-available baseline. The engine's viewport subsurfaces are `wl_shm` regardless, so a dmabuf
  failure never affects the viewport itself.
- **Foreign-connection sharing under winit.** The presenter shares winit's `wl_display` via
  `Backend::from_foreign_display` and a private queue — the exact pattern proven against GDK today. The risk
  is that winit's own event dispatching and the worker's `roundtrip` contend on the socket; libwayland's
  `prepare_read` discipline (already relied on) makes this safe, but it must be re-confirmed against winit's
  dispatch rather than GDK's (watch for a hang or a dropped frame-callback under load).
- **Teardown ordering.** The engine teardown and the window-geometry flush (Phase 2) both hang off the same
  winit close path; if the loop returns before `teardown` completes its `child.wait()` + shm/socket unlink,
  the clean-exit gate fails. Sequence teardown synchronously on the close/loop-exit path before returning,
  mirroring the current `RunEvent::ExitRequested` arm which runs to completion before the process exits.
- **Zero-inset assumption for `offset` (§3).** Feeding `offset = (0,0)` is correct only while the CEF render
  area fills the whole toplevel surface (decorationless, DOM-drawn titlebar). If a later build adds a
  shell-drawn CSD margin or server-side decoration that insets the render area, `offset` must be fed that
  known inset or the subsurfaces mis-register against the pane. The invariant is explicit here so a future
  decoration change updates it deliberately rather than silently shifting the viewport.
