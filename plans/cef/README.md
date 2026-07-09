# The editor shell migration off Tauri/WebKitGTK to a Chromium/CEF OSR shell

**Status:** IN PROGRESS.
- **Phase 1 — COMPLETED.** CEF windowless OSR sustains a full 240 `on_paint`/s on this NVIDIA + Wayland
  box, `--ozone-platform=x11` on the real NVIDIA card. **Pacing later changed:** external-begin-frame
  was removed for CEF self-drive at the monitor refresh (it capped ~223); and the dma-buf accelerated-
  OSR path was removed as unviable on NVIDIA + Mutter (GL cannot bind CEF's Vulkan dma-buf). See the
  phase-1 status note.
- **Phase 2 — COMPLETED.** The `editor/shell` skeleton (module split, winit toplevel + CEF pump with no
  deadlock, geometry persistence, `ShellState`, window controls, raw `wl_display`/`wl_surface` stash,
  `thiserror`) built and validated headlessly; pump-efficiency refinement folded into Phase 6.
- **Phase 3 — COMPLETED.** `wl_shm` compositor uploads CEF `on_paint` → toplevel `wl_surface` (alpha
  preserved, dirty-rect damage) via `from_foreign_display` on winit's display; keyboard + pointer input
  forwarded to CEF; CEF self-drives at the monitor refresh. User-confirmed on the real display: UI
  paints, typing works, dock-tab drag works.
- **Phase 4 — COMPLETED.** Control passthrough (unit-tested) + the JS↔native message-router transport
  (browser + renderer, `cefQuery` → dispatch) + a name-dispatched command surface (`src/commands.rs`):
  `control` (all ~120 typed commands), engine lifecycle, file/OS/trace helpers, settings/recents,
  **window controls** (`getCurrentWindow()` marshaled to the main thread via an inbox), and the
  **Rust→JS event push** (`window.__saffronShellEvent`). 6 unit tests green; headless smoke clean.
- **Phase 5 — CORE COMPLETED (tsc + lint clean).** The `editor/src/shell/` bridge replaces every
  `@tauri-apps/*` API over `window.cefQuery` + the event push; all **17** import sites swapped and the
  `@tauri-apps` deps/scripts removed from `package.json`. `bun run check` + `bun run lint` pass.
- **Phase 6 — PRESENT LOOP BUILT (scene-in-panel validation pending).** Supervision is hardware-
  verified (host up on the RTX 3070 Ti alongside the shell UI). The subsurface **present loop** is now
  ported to `src/presenter.rs` (worker thread; `wp_viewport` scale + `wp_presentation` pacing; parked-
  view detach) and wired in `resumed()`; its Wayland setup smoke-passed on the real Mutter. Confirming
  the scene renders in the panel is the on-display step.
- **Phase 7 — COMPLETED (thumbnail-loading validation pending).** Connectors backend + the 11
  `store_*`/`connector_*` commands + native dialogs + OS drag-drop, plus the `saffron-img://` **scheme
  handler** (`src/scheme.rs`, over the shared `ResourceCache`, registered standard+secure+CORS+fetch).
  Build/clippy/fmt/5 tests green; startup-smoked clean. Confirming thumbnails load is the on-display step.
- **Phase 8 — COMPLETED.** The NO-LEGACY cutover: `editor/src-tauri` deleted wholesale, `editor/`
  grep-clean of Tauri/WebKit/GTK, `just run` launches the CEF shell, `vite.config`/`check.sh`/both
  `AGENTS.md`/`docs/content` rewritten onto the shell. Shell + frontend build green; hugo clean.
- **Phase 9 — §1/§3/§4 VERIFIED; §2 GUI-walk pending user.** fps recorded (~235–240/s self-driven to the
  240 Hz monitor on real NVIDIA hardware, vs ~62.5); control-plane single-flight proven by a burst unit
  test; teardown leak-free; docs clean; builds green. The remaining §2 flow-by-flow GUI walk (visual
  correctness of scene/gizmo/thumbnails + interactive docking/dialogs) needs a human at the display.

All work is unstaged. Everything above is verified in the toolbox (`cargo build`/`clippy`/`fmt`, unit
tests, `bun run check`/`lint`, headless `weston` smoke incl. the real engine on the discrete GPU). The
entire **command / IPC / bridge / service surface of the migration is built and verified** — the OSR
compositing, the message router, the full command dispatch (control passthrough + all ~28 dedicated
commands), the frontend bridge (all 17 sites), engine supervision on the real GPU, connectors, dialogs,
drag-drop, window controls, events, settings/recents.

The entire shell is now **built** — including the two visual pieces (present loop + thumbnail scheme).
What remains genuinely needs a real display / the user and cannot be responsibly finished headless:
- **On-display validation of the two visual pieces** (`editor/AGENTS.md`: never ship GUI behaviour on a
  headless hypothesis). Both are *built* and their Wayland/registration setup is smoke-verified; what's
  left is the human's eyes: (1) the engine's scene renders in the viewport panel (position/scale, tracks
  resize, parks under modals), and (2) the store-grid thumbnails load through `saffron-img://`.
- **Phase 8** (cutover): deletes `editor/src-tauri` wholesale and flips `just run` to the shell.
  Premature until the two validations above pass — deleting the working editor for a shell whose
  viewport/thumbnails aren't yet confirmed would strand the user. Gated on user confirmation.
- **Phase 9**: end-to-end high-refresh verification — hardware + user.

The technical risk is fully retired (OSR fps, `wl_shm` compositing on winit's display,
`from_foreign_display`, the control round-trip, the IPC transport, the message router, engine
supervision on the real GPU are all proven). The remaining work is a build-and-validate loop that needs
the shell on a real display — see the run command at the end.

Replace the editor's Tauri shell (GTK3 + `webkit2gtk-4.1` + `wry`, in `editor/src-tauri`) with a
purpose-built Chromium/CEF Rust shell so the editor UI repaints at the true monitor rate on this
NVIDIA + Wayland machine. The engine (`saffron-host`), the JSON-over-unix-socket control plane, the
React/Vite frontend, and the transparent-UI-over-engine-Wayland-subsurface architecture all survive
the migration; only the *shell* — the process that owns the toplevel window, hosts the webview, and
bridges JS to native — is rebuilt on CEF. Per the repo's NO-LEGACY rule this is a wholesale cutover:
the new `editor/shell` crate is the CEF shell, and `editor/src-tauri` is deleted wholesale — directory
and all — in that one change (Phase 8), with every Tauri code path gone in the same change. There is no dual-shell period and no compat path.

## Why this exists

The editor's WebKitGTK webview UI is pinned to **~62.5 fps** on this NVIDIA + Wayland machine while
every monitor runs at 144/240 Hz. The root cause, confirmed in WebKit source and live tests this
session: NVIDIA's driver returns `EOPNOTSUPP` on `drmWaitVBlank`, so WebKitGTK's `DisplayVBlankMonitor`
falls back to a hardcoded 60 Hz timer, and the GTK port of WebKitGTK has **no** Wayland
frame-callback fallback to pace off instead. Chromium, by contrast, paces frames off the Wayland
presentation-time / frame-callback protocol — which Mutter delivers at the true monitor rate — and
therefore does **not** depend on DRM vblank at all.

This is not a theory. This session, the *exact editor React UI*, loaded unchanged in Chrome on this
same machine, measured **238.7 fps** (avg frame gap 4.19 ms) against WebKitGTK's ~62.5 on the same app.
Same machine, same app, same monitor — only the browser engine differs. Migrating the editor shell from
WebKitGTK to a Chromium/CEF engine delivers high-refresh UI now, with no driver upgrade required.

**Why not just wait for the NVIDIA 610 driver + a WebKit pref flip.** A newer NVIDIA driver that
implements `drmWaitVBlank` (or a WebKitGTK preference that forces the frame-callback pacing path) might
in principle fix WebKitGTK's cap without touching the shell. That path is rejected as the plan's
target because it is gated on an *external* driver release we do not control and is **unverified
end-to-end here** — no measurement this session showed WebKitGTK hitting the monitor rate under any
configuration on this hardware. The CEF route is proven-now (the 238.7 fps figure above), is entirely
within our tree, and is shell-only — it changes nothing about the engine or the frontend logic. When
the correct, modern option is also the proven-now one, it is the one to build.

## Chosen integration model

**Windowless CEF off-screen rendering (OSR), composited by the shell, under a host-owned raw Wayland
toplevel.** The new shell crate (replacing `editor/src-tauri`) owns exactly **one** Wayland toplevel
via `winit`, and with it the `wl_display` + `wl_surface`. CEF runs windowless
(`WindowInfo.windowless_rendering_enabled = true`, transparent background) and hands the shell the React
UI as pixels through a `RenderHandler`; the shell uploads that BGRA buffer — alpha preserved — to the
toplevel surface, and the engine's Vulkan viewport frames stay `wl_subsurface`s stacked **below** it.
That is the exact transparent-UI-over-engine-subsurface design the editor has today, minus GTK.

Two facts force OSR over the alternatives, and both were verified against CEF's own issue tracker and
the current `editor/src-tauri/src/wayland_viewport.rs`:

- **Windowed CEF on Wayland cannot be embedded.** CEF's Ozone/Wayland build runs only in Views mode
  (CEF owns its own toplevel) and exposes no accessor for that window's raw `wl_surface` — unlike GDK's
  `gdk_wayland_window_get_wl_surface`, which the current shell relies on (`wayland_viewport.rs:46`).
  Wayland also forbids parenting a `wl_subsurface` across two separate compositor connections. So the
  current trick — let the toolkit own the toplevel, reach in for its `wl_surface`, and subsurface the
  engine below — has no CEF equivalent. CEF cannot own the toplevel in this architecture.
- **OSR is *more* aligned with the existing design, not a workaround.** The shell already owns viewport
  compositing (the `wayland_viewport.rs` presenter blits engine shm frames into subsurface buffers);
  under OSR it simply *also* owns compositing the UI buffer as the top layer. Frame pacing then lands
  under the shell's control — it drives CEF's frame production off the same `wp_presentation` feedback
  loop the presenter already reads (`refresh_mhz`), so UI and viewport share one monitor-rate cadence.
  This sidesteps Chromium's own documented NVIDIA + Wayland high-refresh regressions entirely, because
  the shell is the frame clock, not Chromium's internal vsync.

The Rust binding is the **`cef` crate** (`tauri-apps/cef-rs`, tracking CEF/Chromium 149); pin an exact
`cef` / `cef-dll-sys` pair on the 149.x line. CEF binaries are provisioned in the `saffron-build`
toolbox via `export-cef-dir`, with `CEF_PATH` / `LD_LIBRARY_PATH` baked into the `justfile` recipes
(the distribution is multi-hundred-MB and version-locked to an exact Chromium build). The baseline
paint path is **CPU `on_paint` → `wl_shm`** (guaranteed to work on NVIDIA); accelerated
`on_accelerated_paint` over dmabuf is an **optional zero-copy upgrade, gated behind a runtime capability
probe**, because the accelerated shared-texture path is documented to fail on NVIDIA's GBM backend
(CEF #3953). Frame production is app-driven via external-begin-frame, clocked off the presenter's
existing `wp_presentation` feedback, so the UI repaints at true monitor rate.

## Invariants preserved (carried across, never re-implemented or forked)

- **The JSON-over-unix-socket control plane.** `control_request_with_params` (`lib.rs:493`) does a
  newline-delimited JSON round-trip under the single-flight `CONTROL_IO` mutex (`lib.rs:462`); the one
  generic `control(cmd, params)` command is what all ~120 typed wrappers in
  `editor/src/control/client.ts` funnel through. This logic moves **verbatim** — only the
  `#[tauri::command]` attribute becomes a CEF IPC handler. The single-flight `CONTROL_IO` serialization
  is load-bearing (concurrent invokes trip the engine's 5 s read timeout, os error 11) and must survive
  in whatever thread model the CEF invoke bridge uses.
- **The invoke rejection shape.** `control/client.ts::toControlError` reads `.message` and `.code` off
  the rejected *value*; the engine's machine-readable `code` (e.g. `busy-loading`) is used to drop
  background-poll errors. The CEF bridge must reject with the `{ message, code }` object, not a
  flattened string.
- **Entity IDs are opaque strings end-to-end** (u64 in the engine) — never `Number()`-ed.
- **The generated protocol types** (`editor/src/protocol/sa-types.ts`) and the `CommandName` union stay
  untouched. `storefront/types.ts` remains hand-authored to mirror the Rust connector camelCase wire
  types (it is the one exception to the generated-protocol rule); its invoke/`Channel` usage is a
  straight edit there.
- **The React/Vite frontend survives largely intact.** A single internal `src/shell/` bridge module
  reproduces the exact shapes of `invoke` / `listen` / window / webview / dialog / `Channel`; then a
  mechanical import-specifier swap across the **17** `@tauri-apps` import sites points them at the shim.
  Component bodies, `client.ts`'s ~120 wrappers, `storefront/types.ts`, and `storefront/cachedImage.ts`
  stay as-is — only import lines change — after which `@tauri-apps/api`, `@tauri-apps/plugin-dialog`,
  and `@tauri-apps/cli` leave `package.json`.
- **The transparent-UI-over-engine-subsurface architecture.** The shell toplevel is transparent and
  non-opaque-region; the engine viewport frames remain subsurfaces below it; the presenter's shm ring,
  `wp_viewporter` stretch, `wp_presentation` feedback, opaque backdrop, and park/unpark port intact once
  handed a `winit` `wl_surface` + `wl_display` + toplevel-origin offset + a parent-commit nudge.

## Phases

Nine phases, ordered strictly by technical dependency (never by duration). DAG:
**1 → 2 → 3 → 4 → 5; {2,4} → 6; {3,4} → 7; {5,6,7} → 8; 8 → 9.** Phase 1 is the blocking foundation phase
that gates everything and decides the pacing mechanism and the CPU-vs-accelerated paint path the rest
of the plan builds on.

| Phase | Goal | Depends on |
|---|---|---|
| [`phase-1-cef-toolbox-and-osr-foundation.md`](phase-1-cef-toolbox-and-osr-foundation.md) | Create the real `editor/shell` crate and provision the pinned `cef` crate + CEF binaries in the `saffron-build` toolbox; boot CEF windowless on this NVIDIA + Wayland box, receive CPU `on_paint`, and prove external-begin-frame drives paint at 144/240 as the phase's acceptance gate (else commit the uncap-and-sample fallback). Resolves the CPU-vs-dmabuf paint decision and the XWayland-for-the-GPU-process question. | — |
| [`phase-2-host-wayland-toplevel-shell-skeleton.md`](phase-2-host-wayland-toplevel-shell-skeleton.md) | New shell crate: one transparent, undecorated `winit` toplevel owning the `wl_display` + `wl_surface`; CEF multi-process bootstrap (`execute_process` first, helper/self-relaunch); CEF message pump integrated into the `winit` loop; window geometry restore/persist (size + maximized only, the Wayland rule). No UI paint yet. | 1 |
| [`phase-3-cef-osr-ui-paint-and-input.md`](phase-3-cef-osr-ui-paint-and-input.md) | CEF OSR paints the React UI (Vite dev URL / prod scheme) into the toplevel surface with alpha; input forwarded `winit` → CEF (pointer, keyboard, wheel, focus, IME, cursor shape); DPI/fractional-scale correctness for click hit-testing; frame production paced off the presenter clock per Phase 1's verdict. | 2 |
| [`phase-4-ipc-bridge-and-command-surface.md`](phase-4-ipc-bridge-and-command-surface.md) | The JS↔native bridge: `CefMessageRouter` (`window.cefQuery`) for `invoke` → Promise (rejecting with `{message,code}`), persistent queries for `store_import` progress (replacing `Channel<f64>`), and a browser→render `CefProcessMessage` broadcast → injected `window.__saffron` → DOM `CustomEvent` for `listen`. Re-host all 28 commands, keeping the `control` passthrough and `CONTROL_IO` serialization verbatim. | 3 |
| [`phase-5-frontend-bridge-shim-and-de-tauri.md`](phase-5-frontend-bridge-shim-and-de-tauri.md) | One internal `src/shell/` module reproducing the exact `@tauri-apps` shapes; mechanical import swap across the 17 sites; remove the `@tauri-apps/*` npm deps and the dead `TAURI_` Vite `envPrefix`. Resolve whether Chromium delivers mouse side-buttons 8/9 to the DOM (if so, the native mouse-button path is deleted). | 4 |
| [`phase-6-subsurface-presenter-supervision-lifecycle.md`](phase-6-subsurface-presenter-supervision-lifecycle.md) | Re-wire the `wayland_viewport.rs` presenter off GTK/GDK onto the `winit` toplevel's `wl_surface`/`wl_display` via `raw-window-handle`; subsurfaces below the toplevel; toplevel-origin offset; parent-commit nudge replacing `gtk_window.queue_draw()`. Port engine supervision + lifecycle commands (`start_engine`/`set_viewport_bounds`/`set_viewport_parked`/`viewport_refresh_hz`/`quit_engine`/`engine_alive`) and confirm whether `viewport_refresh_hz` can drop from the UI hot path. | 2, 4 |
| [`phase-7-native-shell-services.md`](phase-7-native-shell-services.md) | Native dialogs via `rfd` (xdg-portal + wayland); OS file drag-drop via `winit` `HoveredFile`/`DroppedFile` → `file-drop` event (with tracked pointer position); the `saffron-img://` custom scheme as a CEF scheme handler over the connector `ResourceCache`; keep the `flatpak-spawn --host` external-open chain and the `keyring` usage verbatim; the `9001` trace loopback server verbatim. | 3, 4 |
| [`phase-8-cutover-devloop-packaging.md`](phase-8-cutover-devloop-packaging.md) | Delete `editor/src-tauri` wholesale — the entire crate and directory (Tauri/GTK/WebKit deps, `tauri.conf.json`, `capabilities/`, `build.rs`, `gen/`, `icons/`, the WebKitGTK env workarounds + `install_stderr_noise_filter`, the GTK mouse hook) — making `editor/shell` the sole shell; rewrite the `justfile` run recipes + dev-loop onto it; add packaging that stages `editor/dist` + the `libcef` runtime; update the gate + docs (e2e is host-only, unaffected). | 5, 6, 7 |
| [`phase-9-verification-high-refresh.md`](phase-9-verification-high-refresh.md) | End-to-end verification of every editor flow on the CEF shell (docking, storefront import with progress, dialogs, OS drag-drop, trace → Perfetto, undo/redo, viewport park/resize/DPI, gizmo overlay compositing) and confirmation of the ~62.5 → 144/240 fps win now that UI and viewport share one presentation-fed cadence; update docs + `AGENTS.md`. | 8 |

## Current-code touchpoints being replaced

- **`editor/src-tauri/src/lib.rs`** — the 28 commands in `generate_handler!` (`lib.rs:1293`); only
  `control` is the engine passthrough, the rest are shell-local. Three emitted events (`engine-phase`,
  `viewport-error`, `mouse-button`); the `saffron-img` scheme (`lib.rs:1262`); `EditorState` (engine
  child, per-PID socket path, viewports, `9001` trace server); the geometry
  persistence/`configure_main_window`/`WindowStateTracker` layer; `spawn_engine`'s engine-facing env
  (unchanged). **Deleted, not ported:** the `run()` webview-render-path block
  (`SAFFRON_WEBVIEW_HW`, `__NV_DISABLE_EXPLICIT_SYNC`, `LIBGL_ALWAYS_SOFTWARE`,
  `__EGL_VENDOR_LIBRARY_FILENAMES`, `EGL_LOG_LEVEL`) + `install_stderr_noise_filter` — WebKitGTK
  explicit-sync/DMABUF crash workarounds whose rationale disappears under CEF.
- **`editor/src-tauri/src/wayland_viewport.rs`** — the crux. `install()` (`:326`) reaches through Tauri
  into GTK/GDK/WebKitGTK (`window.gtk_window()`, `default_vbox()`, the `WebKitWebView` child,
  `gdk_wayland_display_get_wl_display`/`gdk_wayland_window_get_wl_surface`, `connect_draw`/
  `connect_size_allocate`/`connect_event`). `run()`/`step_view()` (the presenter worker: per-view
  subsurface, seqlock shm ring, `wp_viewporter` stretch, `wp_presentation` feedback, opaque backdrop,
  park/unpark) is built on raw `wayland-client` over a foreign display pointer and **ports intact** once
  handed a `wl_display` + `wl_surface` + webview-offset + a redraw nudge.
- **`editor/src` (frontend)** — 17 `@tauri-apps` import sites (App, control/client, ProjectMenu,
  WindowTitlebar, ExportModal, SettingsModal, ProjectStartupModal, useMouseBindings, useSubsurfaceBounds,
  ScriptSlots, CaptureControls, AssetsPanel, storefront/{AssetDetailModal,ProviderModal,StoreCredits,
  StoreResultsGrid,types}) over `@tauri-apps/api` core `invoke` / event `listen` / window
  `getCurrentWindow` / webview `getCurrentWebview().onDragDropEvent`, and `@tauri-apps/plugin-dialog`
  `open`/`save`. `WindowTitlebar.tsx` calls `getCurrentWindow()` at module-eval time, so the shim's
  window object must construct synchronously at import.
- **`connectors/`** (registry, session, cache, oauth_loopback, credentials, and the four provider
  connectors) — verified shell-agnostic (no `tauri::` refs in code; `reqwest`/`keyring` v3/`tokio`/
  `zip`/`async-trait`). The product's only outbound HTTP. Ports verbatim; only the ~11 thin command
  wrappers in `lib.rs` and the `store_import` progress `Channel` get re-wired.

## Service replacements

| Current Tauri surface | CEF-shell replacement |
|---|---|
| Native dialogs (`@tauri-apps/plugin-dialog`) | `rfd` crate (xdg-portal + wayland features; GNOME/Mutter portal). Keep null-on-cancel; verify `$DBUS_SESSION_BUS_ADDRESS` crosses the toolbox. |
| Window controls / geometry | `winit` Window (`set_minimized`, `set_maximized`/`is_maximized`, `drag_window` for CSD move, `set_min_inner_size`, `scale_factor`, `Resized`/`ScaleFactorChanged`; `visible:false` then `visible(true)` after first paint for the reveal). |
| external-open / vscode / folder | Keep the existing `std::process::Command` `flatpak-spawn --host` chain **verbatim** (toolbox-correct; do not swap for the `opener` crate). |
| Keyring | `keyring` v3, already used directly, verbatim. |
| Events (`emit` → `listen`) | `CefMessageRouter` + `CefProcessMessage` broadcast → DOM `CustomEvent`. |
| `Channel<f64>` (store_import progress) | Persistent `cefQuery` streaming N `onSuccess` before completion. |
| `saffron-img://` scheme | CEF scheme handler (`OnRegisterCustomSchemes` + `SchemeHandlerFactory`/`ResourceHandler`) over the connector `ResourceCache`. |
| Trace loopback (`127.0.0.1:9001`, PNA header) | Verbatim `std::net` TCP server (`Access-Control-Allow-Private-Network:true` already Chromium-correct). |
| OS file drag-drop (`getCurrentWebview().onDragDropEvent`) | `winit` `HoveredFile`/`DroppedFile` → `file-drop` event; track last pointer position to supply drop coords (`winit` `DroppedFile` lacks cursor position). |

## Risks

- **External-begin-frame reliability is the dominant risk.** CEF has a history of black frames / no
  `OnPaint` on Linux/Viz when driven by `SendExternalBeginFrame` (CEF #2800). Mitigated by the blocking
  Phase 1 foundation validation and a proven uncap-and-sample fallback (`--disable-frame-rate-limit` +
  `--disable-gpu-vsync`, then sample the latest painted buffer at monitor rate) before any shell code
  depends on the external-begin-frame path.
- **Accelerated shared-texture OSR fails on NVIDIA's GBM path** (CEF #3953), so the shell must ship on
  CPU `on_paint` → `wl_shm` readback. At 144/240 Hz over 1600×900+ this is real bandwidth; mitigated by
  dirty-rect partial uploads and the UI's mostly-static poll model, with dmabuf as an optional,
  runtime-gated upgrade only if proven on this GPU.
- **CEF's internal GPU/renderer subprocess likely runs under XWayland** (`--ozone-platform=x11`) even
  though the host toplevel is native Wayland, adding an XWayland dependency to the toolbox and
  headless-e2e environment that must be validated (Phase 1).
- **Toolbox integration.** CEF ships large, version-locked binaries that must be reachable at link and
  runtime (`CEF_PATH`/`LD_LIBRARY_PATH`), and the single-binary self-relaunch helper model must work
  under the toolbox and the editor spawn model.
- **`cef-rs` is a young, pre-1.0 binding** tracking CEF releases with API churn; pin exact versions and
  verify each seam (scheme handler, IME, persistent queries) against the crate before depending on it.
- **Input fidelity in OSR is genuine surface area** the shell now owns: keyboard layouts / IME preedit
  positioning on Wayland, intra-page vs OS drag-drop, clipboard, cursor shape, and DPI/fractional-scale
  correctness for click hit-testing — not a checkbox.
- **Portal / Secret-Service reachability from inside the toolbox** (`$DBUS_SESSION_BUS_ADDRESS` crossing
  the boundary) must hold for `rfd` dialogs and `keyring`; the existing `flatpak-spawn --host` pattern
  hints host-service reachability is delicate.

## Open questions

Each is resolved by a named phase; the answer changes that phase's wiring, not the plan's shape.

- **Mouse side-buttons.** Does Chromium/CEF deliver buttons 8/9 to the DOM (as buttons 3/4) in OSR? If
  yes, `useMouseBindings.ts` + `SettingsModal.tsx` become pure DOM handlers and the native
  mouse-button event path is deleted. *(Phase 5.)*
- **Pacing mechanism.** ✅ **RESOLVED (Phase 1):** external-begin-frame is reliable on this machine — it
  sustained ~235–241 `on_paint`/s at a 240 Hz target with no black frames and no every-other-frame drop.
  Locked as the pacing mechanism; the uncap-and-sample fallback is not needed. *(Feeds Phase 6's pacing
  wiring.)*
- **Accelerated dmabuf OSR.** ✅ **RESOLVED (Phase 1):** the shell ships CPU-`on_paint` → `wl_shm`, which
  sustains the monitor rate; the accelerated dmabuf path was left unprobed as a non-load-bearing optional
  upgrade (expected unusable on NVIDIA GBM, CEF #3953). Phase 6 offers no zero-copy upgrade. *(Decided.)*
- **Prod asset delivery.** Embed `dist` (`rust-embed`) into the shell binary, or read from a co-located
  `dist` dir served by the CEF app scheme? *(Phase 3 / 8.)*
- **Custom-scheme caching.** Does CEF cache `saffron-img://` responses differently than WebKitGTK's
  no-cache behavior, and does that interact with the connector in-RAM LRU? *(Phase 7.)*
- **`viewport_refresh_hz` on the UI hot path.** Can it drop from the UI entirely under Chromium's
  true-rate RAF while the engine still gets true refresh from the presenter's `wp_presentation`
  feedback? *(Phase 6.)*
- **e2e boot path.** Does `tests/e2e` touch the shell at all, or only the host over the control plane
  (in which case the cutover leaves it unaffected)? *(Phase 8.)*

## Milestone gate

Per the repo rule, each phase boundary runs `just engine` then `just prepare-for-commit` (format +
lint; clippy `-D warnings` is law) and fixes every warning the change raises. Idiomatic Rust,
`thiserror` per-crate enums, `unsafe` only at the CEF/Wayland FFI seams. A phase that ends with a
runnable checkpoint (a `just run` that picks up the change) is the standard, not one big reconciliation
at Phase 8 — but the Tauri deletion itself lands in a single cutover so the tree is never carrying two
shells.
