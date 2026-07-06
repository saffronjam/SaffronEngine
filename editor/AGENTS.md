# editor — Tauri/React editor

The editor is a **Tauri 2 / React 19 / TypeScript** app. It spawns the Rust `saffron-host`
present-only viewport host headless, presents the host's shared-memory frames on a Wayland subsurface below its
transparent window (the viewport panel is a hole the render shows through), and drives
every operation over the JSON-over-unix-socket control plane. The engine renders; this
app is the UI shell composited over the live viewport.

## Layout

```
src/
  app/         shell (App.tsx), docking layout, menu/topbar, lifecycle wiring
  panels/      Hierarchy, Inspector, Assets, Environment, Render(+Stats), Viewport, Topbar,
               MaterialEditor + MaterialGraph, Profiler, Timeline, Physics, ScriptLogs (+ tree helpers);
               the dock/tab layout lives in `components/dock/` + `state/dockLayout.ts`
  components/  shadcn/ui (ui/) + field renderers (NumberDrag, ColorField, VectorEditor, …); plus
               reusable subsystems: dock/ (the docking model), timeline/ (animation timeline surface
               + transport), anima/ (a generic keyword:value chip-search — *not* animation)
  control/     typed control client over the Tauri bridge (client.ts)
  state/       Zustand store + the reconcile poll (store.ts)
  materials/   material node-graph model shared with the engine wire format (graph.ts) — backs the React Flow editor
  storefront/  the in-editor Asset Store: browse/import models/textures/HDRIs/materials from external
               providers, a `store` ViewTab; talks to editor-local `store_*`/`connector_*` Tauri
               commands (its own AGENTS.md), NOT the control plane
  protocol/    GENERATED TypeScript types — do not edit by hand
  lib/         utilities
  assets/      static assets (fonts + storefront provider logos)
scripts/gen-protocol.ts   re-runs `cargo run -p xtask -- gen-protocol` → src/protocol/sa-types.ts
src-tauri/     Rust bridge (lib.rs + wayland_viewport.rs + connectors/): engine spawn, control passthrough,
               subsurface presenter, and the Asset Store connector backend (connectors/, its own AGENTS.md)
```

Stack (see `package.json`): React 19, Tauri 2, Zustand 5, Vite 7, Tailwind v4
(`@tailwindcss/vite`), shadcn/ui (Radix), `react-resizable-panels` (docking), `@xyflow/react`
(material node graph), `flame-chart-js` + `uplot` (profiler / frame-time stats), `react-colorful`,
`lucide-react` (icons), and `sonner` (toasts). Lint/format via **oxc** (`oxlint` + `oxfmt`, configs in
`.oxlintrc.json` / `.oxfmtrc.json`).

## Workflow

```sh
bun install
bun run check    # gen:protocol + tsc --noEmit
bun run build    # gen:protocol + tsc + vite build
bun run tauri:dev  # launches the app; needs a Wayland session for the subsurface presenter
```

`bun run gen:protocol` regenerates `src/protocol/sa-types.ts` from the `saffron-protocol` DTOs
(`engine/crates/protocol/src/dto.rs`, via `cargo run -p xtask -- gen-protocol`; it also emits the
OpenRPC + command-manifest JSON under `schemas/control/`). `index.ts` is the hand-kept re-export shim.

## Debugging runtime/GUI bugs you can't see (log, then ask)

An agent has **no view of the running editor** — you cannot see the viewport, a flicker, a wrong frame,
or the Wayland presenter's state. Do **not** guess at a runtime/GUI bug's cause and ship a "fix" on a
hypothesis; that wastes the user's time and erodes trust. When a bug can't be pinned from code + tests
alone, **instrument first, then ask the user to capture data:**

1. Add **temporary, clearly-prefixed** logging (`[vp-dbg] …`) at each step of the suspect chain — enough
   to disambiguate the competing hypotheses, not a firehose. Route it to **one stream the user can paste**:
   the terminal where `just run` prints. Rust bridge / engine logs already go there via `eprintln!` /
   stdout; for **React state** (effect firing order, `phase`/`revealed`, a computed rect, a store flag)
   add a temporary Tauri command that `eprintln!`s and call it from React via `invoke` — webview
   `console.log` does **not** reach the `tauri dev` terminal. Log **transitions**, not per-frame state,
   in hot loops, and delete the command + its calls once the bug is found.
2. Tell the user exactly what to do: restart `just run` (a full restart — **Vite HMR does not reliably
   apply Zustand store-shape changes or new commands to a live session**), reproduce the bug, and paste
   the `[vp-dbg]` lines (and/or a screenshot). State which questions the log answers.
3. Diagnose from the **real log**, then fix. **Remove the temporary logging** once the cause is confirmed
   (it is not part of the shipped change).

When you genuinely need more than logs (a screenshot of a specific frame, the exact repro steps, a
hardware/driver detail), **ask the user for it** rather than assuming. A bug is not "fixed" until the
user confirms it against real output — say "this should fix it, please verify with the log", never "fixed".

## Rules that are easy to break

- **`src/protocol/sa-types.ts` is generated.** Never edit it. Edit the DTOs in
  `engine/crates/protocol/src/dto.rs`, run `bun run gen:protocol`
  (`cargo run -p xtask -- gen-protocol`), and commit the result. `src/protocol/index.ts` is the
  hand-kept re-export shim (compat overrides live there), and `client.ts` layers the typed wrappers
  on top.
- **Entity IDs are strings end-to-end.** They are u64 in the engine; treat them as opaque
  strings in JS and **never `Number()` them** — that silently corrupts large IDs.
- **The viewport is a transparent hole down to the engine's subsurface.** The page-level
  backgrounds (index.html, body) stay transparent; every visible region paints its own
  opaque background. DOM freely composites over the viewport. When a modal or another tab
  owns the region, `viewportHidden` parks the subsurface and the panel paints opaque so the
  desktop never shows through.
- **The control client is one generic passthrough.** Rust exposes a single
  `control(cmd, params)` command (it rejects on `ok:false`); the ~120 typed wrappers in
  `client.ts` layer on top. Dedicated lifecycle/presenter commands (`start_engine`,
  `set_viewport_bounds`, `set_viewport_parked`, `viewport_refresh_hz`, `quit_engine`,
  `engine_alive`) are their own Tauri commands, separate from the passthrough. There is **no**
  runtime escape hatch for an untyped command: to add one, add its DTO in
  `engine/crates/protocol/src/dto.rs`, run `bun run gen:protocol`, then add a typed wrapper in
  `client.ts` — every dispatched name is checked against the generated `CommandName` union.
- **Browser file/URL APIs don't work in the webview — go through the bridge.** WebKitGTK
  ignores `<a download>`/blob downloads and `window.open` to an external URL. To save
  client-generated bytes (e.g. a profiler trace), pick a path with `save()` from
  `@tauri-apps/plugin-dialog` and write it with the `write_file(path, bytes)` Rust command;
  to open an external site (e.g. ui.perfetto.dev) use the `open_external(url)` command (which
  tries `flatpak-spawn --host xdg-open` first, since the toolbox has no `xdg-utils`). Perfetto's
  `postMessage` trace handoff can't cross the webview → desktop-browser boundary, so auto-import
  instead serves the trace from a loopback CORS server (`serve_trace`/`start_trace_server`) and
  opens `ui.perfetto.dev/#!/?url=…` pointing back at it — the response must carry
  `Access-Control-Allow-Private-Network: true` or Chromium's PNA blocks the loopback fetch.
- **Surface operation failures through the Toaster — NEVER invent a new error location.** There
  is exactly **one** place a user-triggered operation failure is shown: a Sonner toast via
  `notifyError(errorText(err))` from `lib/flash.ts` (`errorText` normalizes the engine's rejection
  string; `<Toaster />` is mounted once in `App.tsx`). This is absolute — do **not** hand-roll any
  alternative: no per-component `useState<string|null>` error banner, no inline destructive `<p>`
  strip at the bottom of a panel, no `console.error` left as the only signal, no `alert`. Every
  `catch` on a control call ends in `notifyError(errorText(err))` (a silently-swallowed `catch` is a
  bug — the user must see why an action did nothing). The Inspector's add/remove/fit-collider, every
  panel button, every drag-drop op: all route here. Use `notify(...)` for a non-error *result* toast
  (save/load/import) and `toast.error/warning` directly only for the fingerprint-keyed alarm stream
  (`alarmToasts.ts`). Panel-anchored *status* — the startup modal's inline name/validation line — is a
  local `useState` message inside that panel's own DOM, never a stand-in for a toast on a transient
  operation failure and never over the viewport.
- **State sync is a focus-gated poll, not push.** `store.ts` runs a cheap state lane at ~20 Hz
  (`FAST_RECONCILE_INTERVAL_MS = 50`), gated on `document.hasFocus()` and `phase === 'ready'`; heavier
  scene/inspect refreshes fire only when the engine's `sceneVersion` / `selectionVersion` stamps change,
  and metrics wake on a ~10 Hz base tick (`METRICS_BASE_TICK_MS`) but only *fetch* at `metricsRefreshMs`
  (default 1 Hz, user-configurable ~250 ms–5 s). High-frequency edits (field scrubs, gizmo drags) use coalescers
  and set `dragActive` to block the poll from clobbering optimistic local state.
- **UI affordances come from shadcn/ui, not raw HTML.** Use the primitives in
  `src/components/ui/` instead of hand-rolling controls or falling back to native browser
  widgets. In particular, tooltips are `Tooltip`/`TooltipTrigger asChild`/`TooltipContent`
  (the `TooltipProvider` wraps the app in `App.tsx`) — never a `title=` attribute, which
  the webview renders as an unstyled native tooltip. There are no native `title=` attributes in the
  tree; `CloseAffordance` in `components/dock/TabStrip.tsx` takes a `title` *prop* but folds it into
  `aria-label` (`Close {title}`), not a native attribute. When the trigger is also another Radix
  trigger, chain the `asChild` slots down to the real element
  (`TooltipTrigger asChild > DropdownMenuTrigger asChild > Button`).
- **A tooltip must add information.** Only tooltip an element whose meaning is not obvious
  from what's on screen: a cryptic icon button (the hierarchy bone toggle), a keyboard
  shortcut ("Scale (R)"), or why a control is disabled (RenderStatsPanel's RT toggles). No
  tooltip that repeats the element's own visible text or adjacent labels, and none on
  universally understood controls (window min/max/close, an X in a panel corner, back/forward
  arrows) — give those an `aria-label` instead.
- **Overlays must not run whole-document modal machinery, and hidden regions must be cheap.** The
  whole editor DOM stays mounted (every dock panel; the scene / store / asset-editor tab regions are
  `display:none` when inactive, never unmounted). On that DOM, a *modal* Radix overlay is a ~250–500 ms
  stall: `react-remove-scroll` writes the `--removed-body-scroll-bar-size` custom property to `<body>`,
  and a custom-property change on an inherited ancestor forces WebKitGTK to recalc style for the entire
  document. So: (1) the app's `Select` (`components/ui/select.tsx`) is built on a **non-modal Popover**,
  **not** `SelectPrimitive` (which has no `modal={false}` and always mounts the scroll-lock) — keep it
  that way; (2) `DropdownMenu` / `ContextMenu` default `modal={false}` (opt-in `modal` for a rare true
  blocker); (3) dialogs are the non-modal scoped-container pattern (`storefront/` dialogs + `dialog.tsx`
  `DialogScopedOverlay`). Every dockable panel is a `contain: layout paint` boundary (`.contain-panel`
  in `styles.css`, applied in `DockPanelsHost.hostFor` + the tab regions in `App.tsx`), and the root is
  `overflow:hidden` so a scroll-lock's body write is a no-op. A large grid uses **one shared overlay per
  surface**, never a Radix root per row (the store grid's shared truncation tooltip). Dev-time, time an
  open with the `perfLabel` prop on any overlay Root (`lib/overlayPerf.ts`, gated on dev mode).
- **Panel surfaces paint with the semantic theme tokens, never raw `neutral`.** A panel's
  opaque region is `bg-background` (every sibling panel uses it) with `text-foreground` /
  `text-muted-foreground` and `border-border`; inset surfaces (cards, node bodies, recessed
  inputs) use `bg-card` / `bg-muted`. Never `bg-neutral-*` / `text-neutral-*` /
  `border-neutral-*` — those bypass the dark theme in `styles.css` and render the wrong shade
  (the Material panel's original `bg-neutral-900` read as a lighter grey than the rest). Accent
  fills that *encode meaning* (a graph pin's `!bg-sky-500`/`!bg-emerald-500`, a recording tint)
  are not theme neutrals and stay.
- **Field labels are Sentence case via `humanizeFieldName()`**, never the raw camelCase key and
  never the `capitalize` class. A component/material field key (`emissiveStrength`,
  `albedoTexture`) renders through `humanizeFieldName()` from `lib/humanize.ts` ("Emissive
  strength", "Albedo texture") — the same helper the Inspector and ScriptSlots use. `capitalize`
  only upper-cases the first letter of a run-together word ("EmissiveStrength"), so it is wrong.
- **A major view is a main tab via the `ViewTab` system, never a `fixed inset-0` overlay.**
  Anything that owns the whole work area (asset viewer, flame graph, material graph) is a
  `ViewTab` variant in `store.ts` with an `open…Tab` action and a workspace body rendered in
  `App.tsx` (gated by `activeKind`); closing it is `closeViewTab`, and the `sceneTabActive`
  effect parks the viewport for free. A `fixed inset-0` full-screen overlay is only for transient
  modals/dialogs (startup, settings, delete-confirm) — a persistent view rendered as an overlay
  loses the tab strip, the viewport-park wiring, and its state across navigation.
- **Control calls are serialized; high-frequency or expensive ones must be coalesced.** Every
  control-plane request goes through the one Rust socket helper (`control_request_with_params`)
  under the `CONTROL_IO` mutex, so exactly one round-trip is outstanding at a time — concurrent
  invokes otherwise pile into the engine's per-frame drain and trip the 5 s read timeout
  ("read control reply: Resource temporarily unavailable (os error 11)"). On the UI side never
  fire a control call per keystroke/scrub-tick: buffer through a `makeCoalescer` (one
  `preview-render` per edit-burst, not one per field) and keep the heavy GPU calls
  (`preview-render`, thumbnail readback) off the hot path.
- **A large list re-renders only the rows that changed, never the whole list.** A grid/tree
  whose rows number in the hundreds (Assets tiles, Hierarchy rows) follows three rules so a
  selection click costs two row renders, not N (verify with the dev-mode `logRender` counters
  in the status footer — enable dev mode with five clicks on the footer fps counter, or
  `VITE_SAFFRON_DEV_MODE=1`; `[renders/s] AssetTile×2` is healthy, `×300` is the bug). (1) Each row
  is a `memo()` component that subscribes to its OWN derived primitive
  (`useEditorStore((s) => s.selectedAssetIds.has(id))`, `(s) => s.selectedId === id`) — never
  the whole `Set`/array, and never a slice every row shares (the old `TreeRow` read
  `componentsBySelected`, so every row re-rendered on each inspect poll; that list now lives in
  its own `ComponentSubrows` child). Per-row varying state that drives this lives in the store
  (`store.ts` — UI-only state there is fine, like `selectedAssetIds`/`devMode`), with actions
  that bail out identity-stable (`return {}` when nothing changed, a fresh `Set` only on
  change). (2) Every prop the row receives is referentially stable: `useMemo` the derived list
  (`visibleAssets`), `useCallback` every handler, and bind the row's own key inside the row
  (`FolderTile`/`TreeRow` take `path`/`id` and call `onSelect(path, e)`) so one function
  identity serves all rows. (3) ONE shared context menu per surface, not a Radix root per row:
  the row carries a `data-*` id (`data-asset-tile-id`, `data-entity-id`), a single
  `onContextMenu` on the trigger resolves the target via `closest()` into a ref, and the menu
  body renders at open time (Radix unmounts closed content) reading that ref. Never switch a
  row's element *tree shape* on a selection-dependent prop (the old per-tile
  `contextMenuDisabled` flipped `<div>` ↔ `<ContextMenu>`, remounting every thumbnail `<img>`
  on each 0↔1 selection crossing).
- **The Asset Store is editor-local, not the control plane.** `storefront/` (the `store` ViewTab,
  `openStoreTab`) browses and imports assets from external providers through `store_*`/`connector_*`
  Tauri commands implemented in `src-tauri/` (`connectors/`) — the **only** outbound HTTP in the
  product; the engine crates make none. Only the final import crosses to the host (into the catalog).
  The *enabled provider list* is control-plane, per-project state (`get-stores`/`set-stores`, saved in
  `project.json`, team-shared); API keys / OAuth tokens live only in the OS keyring (a presence boolean
  reaches the webview, never the secret). `storefront/types.ts` is **hand-authored** and must mirror the
  Rust `connectors` camelCase wire types — the generated-types rule (first bullet) is scoped to
  `src/protocol/sa-types.ts` and does **not** reach it. Full contract: `storefront/AGENTS.md` +
  `src-tauri/src/connectors/AGENTS.md`.
- **Panel bodies render once and are re-parented, never remounted.** Each dockable panel body renders a
  single time at the app root (`components/dock/DockPanelsHost.tsx` `LeafBody`) and is moved into its leaf
  via `appendChild`; **never** render a panel body inside the leaf tree, or a dock move remounts it and
  destroys its component state, refs, and the live Wayland subsurface. Layout is pure
  `DockLayout → DockLayout` functions in `state/dockLayout.ts`, not in components. There are two disjoint
  dockspace islands (`DockSpaceKind` `'scene' | 'assetEditor'`, kept apart only by disjoint id sets); a
  `locked` leaf is the transparent viewport hole (rejects drops), a `persistent` leaf can collapse but is
  never deleted.
- **Undo/redo is editor-only, reconstructed from inverse control calls.** The engine has no undo; each
  undoable action pushes an `UndoableEdit` (a paired `undo`/`redo` control call) via `pushEdit` into a
  per-main-tab history (`historyByTab`, keyed by `ViewTab.id`, `HISTORY_CAP = 200`; `lib/undo.ts` +
  `state/store.ts`). Only editable tab kinds record (`HISTORY_TAB_KINDS`: scene / materialGraph /
  assetEditor); snapshot-model tabs (the material graph) use `useTabSnapshotHistory`; nothing records
  during play. Shortcuts in `app/useUndoRedoShortcuts.ts`, buttons in `panels/Topbar.tsx`. When you add a
  mutating action, record its inverse — an edit with no `pushEdit` is silently un-undoable.
- **Two independent lifecycle axes — do not conflate them.** `engineStatus.phase` (`EnginePhase`:
  idle → starting → attaching → ready → error) tracks the host/renderer process; `projectLoad.phase`
  (`ProjectLoadPhase`: idle / loading / ready / error) tracks project loading and can run while the engine
  stays `ready` (a reload) or before it is (bootstrap). One entry point (`startProjectLoad`), polled by
  `app/useProjectLoadPoll.ts`, surfaced by the single `app/ProjectStartupModal.tsx` (picker / loading /
  error). Gate viewport-ready UI on the engine axis, project-content UI on the load axis.
- **Shortcuts are a registry, never inline key comparisons.** Every shortcut is a command in
  `lib/keybindings.ts` (`COMMANDS`) with a kind (`press`/`hold`/`mouse`) and a scope
  (`global`/`hierarchy`/`assets`/`fly`/`tabs`); handlers match with `matchesBinding`, never by comparing
  `e.key` literals. Add a shortcut by registering a command. Bindings are rebound in `app/SettingsModal.tsx`
  (capture mode + per-scope conflict detection) and persisted **delta-only** in `appdata/settings.json`. A
  few (the redo alias `Ctrl+Y`, the `Ctrl+P` play family) are intentionally non-rebindable.
- **Window geometry is remembered state, not a setting — and only size + maximized are acted on.** The
  editor window's last size / position / monitor / maximized state lives in `appdata/state.json` (a
  generic "remember where I left it" bucket, separate from `settings.json`; missing file = no memory =
  size to the current monitor). `configure_main_window` (`lib.rs`) restores it before the window is
  shown (no visible jump); a `WindowStateTracker` folds every resize/move into a live snapshot flushed
  on `ExitRequested`. On **native Wayland** (the `just run` path — GNOME/Mutter in particular) a client
  cannot place its own toplevel or choose an output, so `configure_main_window` **only re-applies size +
  maximized**. Position and `monitor` are still *captured and persisted* (they may be useful on a
  platform that can honor them) but deliberately **never applied** — do not re-add a `set_position` /
  monitor-clamp restore path. `capture_window_geometry` records size/x/y only while **not** maximized
  (so a restored maximized window un-maximizes to the right size) and refreshes monitor/scale every
  event (else an always-maximized window keeps a stale monitor); `outer_position()` may `Err` on
  Wayland, so it keeps the last-known x/y rather than dropping the snapshot.

The Rust bridge sets a per-PID socket under `$XDG_RUNTIME_DIR` and a per-PID, per-view shm
segment for each viewport (scene + asset preview), spawns `$SAFFRON_ANIMA_BIN` (default
`engine/target/debug/saffron-host`) with `SAFFRON_VIEWPORT_SHM_SCENE` +
`SAFFRON_VIEWPORT_SHM_ASSET` (and the NVIDIA `VK_ICD_FILENAMES` guard), and presents via
`wayland_viewport.rs` — one subsurface per view, each glued to its pane,
plus a shared opaque backdrop below both. A watchdog flips the UI to an error overlay if the
engine dies.

The bridge also picks the **webview render path** in `run()` (`lib.rs`), logging it at startup
(`[saffron] webview render path: …`). On NVIDIA it defaults to the **software** (Mesa llvmpipe) path,
because the hardware DMABUF path hits the `wp_linux_drm_syncobj_surface_v1` "unsupported buffer" crash
(WebKit enables explicit sync on its EGL surface, then a non-dmabuf buffer reaches it and Mutter fatally
rejects it — a driver/WebKit/Mutter interaction *not* fixed by newer drivers, only sidestepped). AMD and
Intel always take the hardware path. `SAFFRON_WEBVIEW_HW` opts *into* hardware on NVIDIA — it is
**presence-checked** (`var_os(…).is_some()`), so *any* value including `0` selects hardware; to force
software you must **unset** it, not set it to `0`. The hardware path then sets
`__NV_DISABLE_EXPLICIT_SYNC=1` to dodge that crash (the lone tradeoff is possible stale-frame ghosting).
The `just run` recipe `export`s `SAFFRON_WEBVIEW_HW=1` inside the toolbox-bound recipe, so just-run
launches are hardware; a host-side `SAFFRON_WEBVIEW_HW=1 just run` would **not** cross the toolbox
boundary into the recipe (see the root `AGENTS.md` toolbox-env rule).
