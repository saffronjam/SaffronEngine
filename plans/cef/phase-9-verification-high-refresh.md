# Phase 9 — Final verification + high-refresh confirmation

**Status:** §1 / §3 / §4 VERIFIED (headless, real hardware); §2 partially confirmed live (scene,
thumbnails, drag-and-drop, resize + cursors, high-refresh, remote debugging) — the remaining flow walk
is the only open item, pending the user.
- **§1 fps (the acceptance metric):** the UI's composited `on_paint` cadence sustains **~235–240/s,
  self-driven to the 240 Hz monitor refresh** (`[shell] vsync: pacing CEF to 240 Hz`), measured on the
  **real Mutter + NVIDIA GPU** (not headless weston) — vs WebKitGTK's ~62.5. The 144 Hz output + the
  on-page RAF read-out remain a user data-point; the 240 Hz number is recorded.
- **§3 control-plane single-flight:** verified by `control::tests::control_io_serializes_a_concurrent_burst`
  — a 12-thread concurrent burst through `control_request_with_params` against a widened mock server sees
  **peak-in-flight == 1**, all served, none timed out (the `CONTROL_IO` mutex holds).
- **§4 teardown:** a `SAFFRON_SHELL_MEASURE_SECS` self-exit run unlinks its own socket + both `/dev/shm`
  viewport segments (verified: the run's PID left no socket/shm behind); no orphan host/CEF-helper.
- **Docs + builds:** `hugo` clean (241 pages, no broken links); `grep -rniE 'tauri|wry|webkit2gtk|
  gtk_window|gdk_wayland' editor/` empty; `test ! -d editor/src-tauri` holds; shell + frontend build green.
- **e2e:** the failures are the harness's own documented software-GPU PSO-compilation timeout under
  headless weston (`harness.ts:145`), orthogonal to the cutover (engine/control/`tests/e2e` untouched);
  raise `SAFFRON_E2E_CALL_TIMEOUT_MS` or run on the NVIDIA GPU.
- **Phases 6–7 visual confirmations DONE:** the user ran `just run` on the Mutter + NVIDIA session and
  confirmed the viewport scene renders in its panel (Phase 6) and the storefront thumbnails load through
  `saffron-img://` (Phase 7). Those two phases are now `COMPLETED`.
- **§2 confirmed live this session (Mutter + NVIDIA):** asset-tile drag-and-drop lands on its targets;
  window edge/corner resize works and the pointer shows the right cursor across the UI — buttons, text
  fields, dock splitters, resize edges (the `DisplayHandler::on_cursor_change` forwarding + the
  `WindowResizeFrame` strips); UI holds the monitor refresh (144→240). Remote debugging serves the CEF
  149 DevTools endpoint (attach via `chrome://inspect`).
- **§2 still needs a human at the display** (perceptual/interaction — no headless probe judges "crisp /
  no tearing / drag lands / keys fly"): docking re-parent state-survival; native open/save dialogs; OS
  file drag-drop (file-manager → editor); the native gizmo overlay compositing over the viewport;
  viewport park on tab-switch + resize/scale. And a re-eyeball of the input-polish fixes not yet watched:
  RMB fly-cam WASD/Space/Shift, double-click-an-asset-to-open, RMB-on-unselected-asset selecting it
  first; plus that the CEF context-menu error lines are gone and React `console.*` now shows as
  `INFO webview …`.
- **Not flipped to COMPLETED:** claiming the remaining §2 walk without observing it would be an
  unverified report.

This is the closing phase. It builds no new shell code; it *proves* the migration met its purpose and
that every editor flow has behavioral parity with the retired Tauri shell. The purpose was concrete and
measured before this planset began: the WebKitGTK webview UI is pinned to ~62.5fps on this NVIDIA+Wayland
machine because NVIDIA's driver returns `EOPNOTSUPP` on `drmWaitVBlank` and WebKitGTK's
`DisplayVBlankMonitor` falls back to a hardcoded 60Hz timer (WebKitGTK's GTK port has no Wayland
frame-callback fallback), while the exact same React UI in Chrome measured 238.7fps on the same monitor.
The CEF/OSR shell built across Phases 1–8 pace-drives its paint off the presenter's `wp_presentation`
feedback loop, so this phase's headline job is to put a number on the win — UI repainting at the true
monitor rate on the 144/240Hz outputs — and then to walk every flow the shell owns and confirm none
regressed against Tauri.

Because Phase 8 has already deleted the Tauri shell wholesale (no dual-shell to fall back to, per the
NO-LEGACY rule), this phase is where the tree is exercised end to end for the first time with no Tauri
code path anywhere. Nothing here should discover a *missing* capability — every capability was built in
its owning phase — but this is the phase that catches a capability that is present yet subtly wrong
(mis-scaled clicks, a dropped event, a leaked shm segment). It ends by marking the whole `plans/cef/`
planset `COMPLETED`.

## Depends on

[`phase-8-cutover-devloop-packaging.md`](phase-8-cutover-devloop-packaging.md) — the cutover must be
landed (Tauri deleted; the new shell binary launched by the rewritten `just run*` recipes; `tools/ci/check.sh`
and `tests/e2e` boot paths repointed) before a whole-system verification is meaningful. This phase also
transitively exercises every earlier phase's deliverable: the host-owned winit Wayland toplevel (Phase 2),
CEF OSR paint + input + DPI (Phase 3), the IPC bridge and re-hosted command surface (Phase 4), the frontend
`src/shell/` bridge shim (Phase 5), the subsurface presenter re-wire + engine supervision + lifecycle
commands (Phase 6), and the native shell services — dialogs, OS drag-drop, the `saffron-img` scheme
(Phase 7).

## Goal

1. **Measure and record** the UI refresh on the target 144/240Hz outputs and confirm the
   ~62.5fps→144/240 win, now that *both* the UI and the viewport are presentation-fed off one clock.
2. **Exercise every editor flow** the shell owns, end to end, against a checklist, with a
   Vulkan-validation-clean engine log throughout.
3. **Confirm control-plane parity** — a representative sweep of `client.ts` wrappers round-trips, the
   `ok:false` rejection shape survives, and the single-flight serialization holds under a burst.
4. **Confirm clean teardown** (no leaked child, socket, or `/dev/shm` segment) and geometry persistence
   across restarts (size + maximized only, per the Wayland rule).
5. Discharge the AGENTS.md keep-current obligations (docs), and mark the planset `COMPLETED`.

## Why a dedicated verification phase, and what it is not

The migration's entire justification is a measured performance claim, so the migration is not *done*
until that claim is re-measured on the delivered shell — a green build proves nothing about frame cadence.
Equally, "the shell compiles and the UI appears" does not prove parity: the Tauri shell exposed ~28
commands, three events, a progress `Channel`, native dialogs, OS drag-drop, a custom image scheme, window
controls, and a transparent-toplevel-over-subsurface compositor, and each had a specific contract the
frontend leans on (rejected `{message, code}` values, `null`-on-cancel dialogs, logical-vs-device pixel
math). This phase is the structured walk that confirms each survived byte-for-byte.

This phase is **read-and-drive only** — it must not introduce a new code path to "make verification
easier". If a flow fails, the fix belongs in its owning phase (Phases 2–7) and the fix must delete the
broken path, not add a compat shim beside it. The one exception is a *development-only* on-page cadence
readout (below), which is a measurement instrument, not product behavior, and is gated behind a
`import.meta.env.DEV` guard so it never ships.

## Verification plan

### 1. High-refresh measurement — the migration's raison d'être

Two independent clocks must be shown to agree at the monitor rate, because a subjective impression of
smoothness is not a number, and the whole planset exists to replace a number (~62.5) with a bigger one
(144/240).

- **The presenter clock (already built, reuse verbatim).** The subsurface presenter derives the true
  monitor refresh from `wp_presentation` feedback: `PresentationStats` in
  `editor/src-cef/.../wayland_viewport.rs` (the module ported off GTK in Phase 6) counts `Presented`
  vs `Discarded` feedback events and `refresh_mhz` on `ViewportShared` holds the compositor-reported
  refresh of the surface's current output. Launch with `SAFFRON_VIEWPORT_STATS` set (the env probe
  `std::env::var_os("SAFFRON_VIEWPORT_STATS")` in the presenter's `run`) so the presenter logs
  presented/discarded cadence for the scene subsurface. On a 144Hz output `refresh_mhz` reads ≈144000
  and the presented-frame cadence tracks it; on a 240Hz output ≈240000. This proves the *viewport* layer
  is presentation-fed at the true rate (it always was — only WebKit's DOM repaint was capped).

- **The UI clock (the actual win).** The CEF OSR paint is driven by app-issued external-begin-frame
  clocked off that same `wp_presentation` feedback (Phase 6's pacing wiring). To measure the resulting
  DOM repaint cadence, add a **dev-only** on-page cadence counter: a small React hook that samples
  `requestAnimationFrame` deltas, keeps a rolling window, and displays instantaneous fps + average
  inter-frame gap (the same `avgGap` metric that read 4.19ms in the Chrome baseline). Guard it behind
  `import.meta.env.DEV` (or a `SAFFRON_SHOW_CADENCE` toggle read from the shell) so it is absent from a
  prod build. On the 144Hz output this must read ≈144fps / ≈6.9ms gap; on the 240Hz output ≈240fps /
  ≈4.2ms gap — versus the ~62.5fps / ~16ms the WebKitGTK shell produced on the identical UI. Record the
  numbers per output in the phase completion note.

- **Cross-check the two clocks.** With both instruments live, drag a window across a 144Hz and a 240Hz
  monitor (or reconfigure the outputs) and confirm the on-page RAF cadence *and* `refresh_mhz` both
  re-settle to the new output's rate together — i.e. UI and viewport share one presentation-driven cadence
  rather than the two diverging (UI at 60, viewport at 144) as they did under WebKit. This closes the open
  question of whether `viewport_refresh_hz` is still needed on the UI hot path: confirm `RenderPanel.tsx`'s
  `resolveTargetFps`/`refreshHz` Default-fps path still receives the true refresh (now from the presenter's
  `wp_presentation` feedback, surfaced through the retained lifecycle command), while Chromium's true-rate
  RAF paces the DOM without it.

- **Guard against a false positive.** Confirm the measured rate is real vsync-locked presentation, not a
  free-running uncapped loop that merely *samples* high: if Phase 1 landed on the external-begin-frame path
  the presented cadence must lock to `refresh_mhz` (no tearing, discarded count near zero under a steady UI);
  if Phase 1 fell back to the uncap-and-sample path (`--disable-frame-rate-limit` + `--disable-gpu-vsync`),
  note that explicitly and confirm the *presented* cadence (not the paint cadence) is what tops out at the
  monitor rate. Either way the acceptance number is the presented/visible rate on the output.

### 2. Flow-by-flow parity checklist

Walk each flow the shell owns on the running editor (`just run`) against the retired Tauri behavior. Each
line is a pass/fail with the engine log asserted validation-clean at the end (the debug messenger prints
no `ERROR vulkan [validation]` lines). Group by the shell surface each exercises so a failure points at
its owning phase.

- **Docking / re-parenting (Phase 3 input + DPI).** Drag a panel between dock zones in the dock system;
  confirm the panel is **re-parented, not remounted** — its internal React state (scroll position, an
  open sub-tree, an in-flight edit) survives the move, and no iframe/webview reload occurs. This is a pure
  DOM behavior, but it only works if OSR delivers pointer-move/down/up at the correct **device-vs-logical**
  coordinates and DPI so the drag hit-tests the target zone; a mis-scaled cursor (the classic OSR
  `device_scale_factor` bug) shows up here first. Repeat on a fractional-scale output (e.g. 1.5×) to
  exercise `wp_fractional_scale`.

- **Storefront browse + import with progress (Phase 4 Channel, Phase 7 scheme).** Open the Asset Store,
  browse a provider (thumbnails load through the `saffron-img://fetch/?u=…` scheme served from the
  connector `ResourceCache`), search + virtual-scroll (`store_search_session`/`store_search_more`), open an
  asset's parts/gallery, and import one. The import must stream download progress: `storefront/types.ts`
  constructs the bridge `Channel<number>`, and the re-hosted `store_import` must push intermediate
  `0.0–1.0` fractions via the persistent `cefQuery` progress stream before resolving. Confirm the progress
  bar advances (not a single 0→100 jump) and the imported model/texture/HDRI lands in the catalog. Confirm
  a provider that needs credentials round-trips through the keyring (`connector_login` →
  `connector_secret_status`) and the OAuth loopback listener completes.

- **Native open/save dialogs (Phase 7 rfd).** Exercise all six open sites and five save sites — project
  pick (`ProjectMenu.tsx`, `ProjectStartupModal.tsx`), model/texture import (`AssetsPanel.tsx`), export
  target (`ExportModal.tsx`), script open (`ScriptSlots.tsx`), trace/capture save (`CaptureControls.tsx`).
  Confirm each honors `directory`/`multiple`/`filters`/`defaultPath`, returns the chosen absolute path(s),
  and returns `null` on cancel (the `withNativeDialog` re-entry lock in `state/store.ts` must still gate
  re-entry and every caller must branch correctly on the falsy). Confirm the xdg-desktop-portal file
  chooser actually appears under GNOME/Mutter from inside the launch environment (the `$DBUS_SESSION_BUS_ADDRESS`
  reachability risk called out in Phase 7).

- **OS file drag-drop (Phase 7 winit drops).** Drag files from the desktop onto the Assets panel. Confirm
  the panel receives `enter`/`over`/`leave`/`drop` frames with a `position` and, on drop, the absolute
  `paths`, in the same shape `AssetsPanel.tsx`'s former `getCurrentWebview().onDragDropEvent` consumer
  expects — and that the drop coordinates are correct (the winit-`DroppedFile`-lacks-cursor-position
  workaround: the shell tracks last pointer position). Confirm intra-page HTML5 DnD (dragging a catalog
  tile within the UI) still works and is not confused with the OS drop channel.

- **Profiler trace → Perfetto (Phase 4 command, unchanged loopback).** Capture a profiler trace, click
  through to Perfetto: `CaptureControls.tsx` calls `serve_trace` (which stashes the bytes and returns a
  `http://127.0.0.1:9001/...` URL) and `open_external` to open it. Confirm Perfetto loads the trace over
  the loopback — Chromium enforces Private Network Access, so the `Access-Control-Allow-Private-Network: true`
  header the trace server already sends must be present and honored (it was added for exactly this; it is
  already Chromium-correct, unlike WebKit which did not need it). Confirm `write_file` + `open_external`
  for a client-generated download also work under Chromium.

- **Undo/redo (frontend + control plane).** Perform a sequence of edits (transform, reparent, material
  assign), then undo/redo the whole stack. The editor's per-tab undo is reconstructed from inverse control
  calls, so this exercises the `control` passthrough under a burst of successive invokes — confirm each inverse
  call round-trips and the scene state matches, and that the single-flight serialization (§3) is not
  tripped by the burst.

- **Viewport park / resize / DPI / fractional-scale (Phase 6 presenter).** Resize the window and the
  viewport pane; confirm the subsurface tracks the pane rect with no lag, tear, or one-frame gap at the
  hole edge (the parent-commit nudge that replaced `gtk_window.queue_draw()` in `set_viewport_bounds`).
  Collapse/park the viewport pane and confirm `set_viewport_parked` hides the subsurface and unpark
  restores it. Change output scale (integer 2× and fractional 1.5×) and confirm `set_viewport_bounds`
  receives the correct device-pixel render size — `useSubsurfaceBounds.ts` multiplies the logical CSS rect
  by the window `scaleFactor`, so a wrong scale yields a blurry or mis-sized viewport. Confirm the reveal:
  the window is created hidden and shown from JS after first paint (the former `getCurrentWindow().show()`),
  with no white/opaque flash before the transparent UI + subsurface compose.

- **Native gizmo overlay compositing (engine-side, viewport hole).** Select an entity and manipulate the
  translate/rotate/scale gizmo. The gizmo is a native overlay the engine renders into the viewport frame
  (`build_scene_edit_overlay` in `saffron-host`; `submit_overlay`/`OverlayVertex` in `saffron-rendering`),
  so this confirms the subsurface-below-transparent-UI stack composites correctly: the overlay must appear
  crisp *inside* the viewport hole, occluded by UI panels that overlap the pane, with no z-fighting between
  the UI's transparent regions and the subsurface. Side-buttons: confirm mouse back/forward (buttons 8/9)
  now arrive natively via winit and reach the DOM as expected (the resolution of Phase 5's open question —
  if Chromium delivers them, the former native `mouse-button` event path is gone and `useMouseBindings.ts`/
  `SettingsModal.tsx` are pure DOM handlers).

- **Two-axis engine/project lifecycle (Phase 6 supervision).** Exercise both independent axes. The
  *engine* axis: `App.tsx` listens for `engine-phase` (`starting`|`attaching`) and `viewport-error`;
  confirm a fresh boot pushes `starting`→`attaching` and the viewport attaches (the presenter's readiness
  probe gates the attach, not the event, so confirm no attach race). The *project* axis: open a project;
  confirm the non-blocking loader drives `project-status` to `ready` (the frontend's `awaitProjectReady`
  equivalent), and that background-poll commands rejected with `busy-loading` during `Loading` are dropped
  silently (§3). Confirm `quit_engine` and `engine_alive` behave, and that closing the window from the
  custom titlebar (`WindowTitlebar.tsx` `close()`) runs the same teardown as an app quit (§4).

### 3. Control-plane parity

The control plane is the invariant that had to survive the migration byte-for-byte; this section proves it
did across the new IPC bridge.

- **Wrapper round-trip sweep.** The ~120 typed wrappers in `editor/src/control/client.ts` all funnel
  through one `invoke("control", { cmd, params })`. Drive a representative sweep (a read like
  `listEntities`/`render-stats`, a mutation like an add/transform, a lifecycle like `engineAlive`, a store
  command, a settings load/save) and confirm each returns the same JSON shape the Tauri bridge produced.
  Confirm camelCase→snake_case arg-key mapping is preserved for the dedicated commands (e.g.
  `setViewportBounds({ view, bounds, resizeEngine })` reaches the native `resize_engine`), or that the
  re-hosted commands were re-declared to accept the exact JS keys — either way the wire keys are unchanged.

- **Rejection shape.** Force an `ok:false` from the engine (e.g. an invalid command or a command rejected
  during `Loading`) and confirm the bridge **rejects with the `{message, code}` object**, not a flattened
  string: `toControlError` in `client.ts` reads `.message` and `.code` off the rejected value, and
  `isBusyLoading(err)` keys on `err.code === "busy-loading"` to drop background-poll errors. A bridge that
  stringifies the rejection silently breaks every busy-loading drop across the editor, so this is a
  load-bearing check, not a nicety. Confirm a non-control command may still reject with a bare string
  (`toControlError` handles both).

- **Single-flight under burst.** The socket round-trip is serialized under the `CONTROL_IO` mutex
  (`static CONTROL_IO: Mutex<()>` guarding `control_request_with_params`) so exactly one request is
  outstanding — concurrent invokes otherwise pile into the engine's per-frame drain and trip its 5s read
  timeout (`os error 11`). Confirm this serialization survives in the CEF invoke bridge's thread model:
  fire a burst of concurrent invokes (the undo/redo replay and the storefront virtual-scroll both produce
  bursts naturally, or drive one synthetically) and confirm none times out and all complete in order. This
  is the single most subtle thing the new thread model could get wrong.

### 4. Teardown + geometry persistence

- **Clean teardown — no leaks.** On window close and on app quit, the shell must run the full teardown
  the Tauri `RunEvent::ExitRequested` arm ran (re-hosted on the CEF/winit close/quit lifecycle in Phase 6):
  send `control "quit"`, kill+wait the engine child, unlink the per-PID socket, and unlink **both**
  `/dev/shm` viewport segments (scene + assetPreview). Verify no orphan after quit: no `saffron-host`
  child in the process table, the socket path under `$XDG_RUNTIME_DIR` gone, and no leftover
  `viewport_shm_name("scene")` / `viewport_shm_name("assetPreview")` segments in `/dev/shm`. Repeat with a
  hard kill of the shell (SIGKILL) and confirm the *next* launch's teardown/startup still cleans stale
  segments (the presenter re-creates them per PID; a stale segment from a crashed session must not wedge a
  fresh boot). Also confirm no leaked CEF helper subprocess (render/GPU/utility) survives the parent.

- **Geometry persistence across restarts.** Resize and maximize the window, quit, relaunch. Confirm only
  **size + maximized** are restored (the Wayland rule: a client cannot place its toplevel or choose an
  output). The persistence layer (`RememberedState`/`WindowState`, `read_state_file`/`write_state_file`,
  `state.json` under appdata) is shell-agnostic and moved verbatim; only the apply/capture calls were
  re-mapped to winit in Phase 2. Confirm `apply_window_state` restores size and re-applies maximized before
  the reveal, `capture_window_geometry` folds resize/maximize into the live snapshot (size/x/y update only
  while *not* maximized; monitor/scale refresh every event), and x/y/monitor are **captured but never
  applied**. Confirm a first-ever launch with no `state.json` falls back to the fill-current-monitor
  default and the 1200×720 minimum inner size holds.

## Scope

- **No product code changes** beyond the dev-only on-page cadence readout (guarded behind
  `import.meta.env.DEV` / a `SAFFRON_SHOW_CADENCE` toggle) used to measure §1. Everything else is
  drive-and-observe against the shell delivered by Phases 1–8.
- **`tests/e2e`:** confirm the suite is green against the new boot path and that it is unaffected by the
  cutover where it should be — this resolves the open question of whether `tests/e2e` touches the shell at
  all. It boots a headless host over the control plane (`Engine.boot` in `tests/e2e/harness.ts`, driving
  the JSON socket, asserting `validationErrors()` empty), so it is shell-agnostic and should pass
  unchanged; confirm that, and confirm no test depended on a Tauri-specific artifact. The high-refresh and
  GUI-flow checks in §1–§2 are **GPU-with-eyes on presenting hardware**, not e2e — the e2e suite covers the
  engine/control surface, not the shell's compositor.
- **`docs/`:** confirm the Phase 8 doc rewrites landed and read correctly — the editor/viewport-bridge
  concept page (`docs/content/explanations/ui-and-editor/tauri-editor-and-viewport-bridge.md`, which must
  be renamed/rewritten off "Tauri" to the CEF/OSR shell), `viewport-compositing.md`, `viewport-panel.md`,
  and their hub `_index.md` rows, plus `editor/AGENTS.md` and the root `AGENTS.md` Stack table (the
  `Editor` row that named Tauri 2 + WebKitGTK). This phase verifies the docs describe the shipped shell and
  contain no dangling reference to Tauri/WebKitGTK/wry; any residual reference is a link-check failure to
  fix here.
- **`plans/cef/`:** flip every `phase-*.md` `**Status:**` line and the `README.md` `**Status:**` to
  `COMPLETED`.

## Verification (the gate)

- **Recorded fps numbers.** A table in this phase's completion note with the measured on-page cadence
  (fps + avgGap) and presenter `refresh_mhz` for each target output (144Hz and 240Hz), demonstrating
  high-refresh UI versus the ~62.5fps WebKitGTK baseline. This is the acceptance number the whole planset
  exists to produce.
- **Checklist run.** Every flow in §2 passes on the running editor, each with a Vulkan-validation-clean
  engine log (no `ERROR vulkan [validation]` line, captured as in the headless harness before any `pkill`).
- **Control-plane parity.** The §3 sweep passes: wrappers round-trip, `ok:false` rejections carry
  `{message, code}` (verified by an `isBusyLoading` drop actually firing), and a concurrent-invoke burst
  completes without an engine read-timeout.
- **Teardown + persistence.** The §4 checks pass: no leaked child/socket/shm/helper after quit or hard
  kill; size+maximized restored across a restart with x/y/monitor never applied.
- **`just check` green.** The reproducible gate (`tools/ci/check.sh`: workspace build + shaders →
  present-only smoke → control-schema contract → frontend `bun` build) is clean against the CEF shell,
  and `just e2e` is green.
- **Docs.** `cd docs && hugo` builds clean (SCSS compiles, no broken intra-site links) with the shell
  pages rewritten and no Tauri/WebKitGTK reference remaining.
- **Planset COMPLETED.** Every `plans/cef/phase-*.md` and the `README.md` `**Status:**` set to
  `COMPLETED`.

## Risks

- **A green build that still repaints at 60.** The dominant risk is declaring victory on a shell that
  compiles and paints but is not actually presentation-paced — if external-begin-frame is silently
  no-op-ing (CEF #2800's black-frame/no-OnPaint history) the UI can look correct while topping out at 60.
  The §1 measurement is the guard: require a *recorded* number ≥ the output rate from the presented-frame
  clock, and cross-check the on-page RAF cadence against `refresh_mhz`. Do not accept "it feels smooth."
- **DPI/fractional-scale click drift is easy to miss.** A `device_scale_factor` mismatch between CEF's
  physical-pixel paint and winit's logical-pixel input produces subtly offset clicks that pass a casual
  look but break docking hit-tests and gizmo grabs. §2's docking + fractional-scale + gizmo lines exist to
  force this out; test on a fractional (1.5×) output specifically, which the old GTK path never fully
  exercised.
- **The single-flight serialization is the subtlest regression.** If the CEF invoke bridge runs invokes on
  multiple threads without preserving the `CONTROL_IO` single-flight, most calls still work and only a
  burst trips the engine's 5s read timeout — an intermittent failure that hides in casual use. §3's burst
  test must be run deliberately, not assumed.
- **Teardown leaks accrete silently.** A missed shm unlink or an orphaned CEF helper does not fail the
  session that leaks it — it fails the *next* boot, or exhausts `/dev/shm` after many runs. §4 must check
  the process table and `/dev/shm` explicitly after both a clean quit and a hard kill, not just observe
  that the window closed.
- **The `{message, code}` rejection shape is load-bearing and invisible when wrong.** A bridge that
  flattens rejections to strings does not throw — it silently turns every `busy-loading` background-poll
  error into a surfaced error, spamming the UI during project load. Verify by making an `isBusyLoading`
  drop *actually fire*, not by inspecting the type.
- **Cross-vendor is deferred, not claimed.** The measurement is on this NVIDIA+Wayland+GNOME machine; the
  shell is architected portable, but AMD/Intel and other compositors are not provable in the toolbox (no
  alternate hardware GPU). State the measured environment; do not claim a rate proven on hardware not run.
- **Marking COMPLETED prematurely.** The planset is not done while any checklist line fails or any doc
  still says "Tauri". A failing flow is fixed in its owning phase (never patched with a shim here) before
  the Status lines flip.
