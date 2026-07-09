# Phase 5 — Frontend bridge shim + de-Tauri the frontend imports

**Status:** COMPLETED — the bridge is exercised end to end by the running editor on the real display
(the user drives `invoke`, events, window controls, dialogs, and storefront `Channel` progress through
it under `just run`); the remaining flow-by-flow GUI confirmation is Phase 9 §2's. The bridge is
`editor/src/shell/index.ts` — the one drop-in replacement for the `@tauri-apps/*` APIs, all crossing
to native through `window.cefQuery` (the message router) and receiving events by the shell executing
`window.__saffronShellEvent(name, payload)` in the frame. It provides: `invoke` (rejecting with an
`InvokeError { message, code }` shaped for `client.ts`'s `toControlError` and `flash.ts`'s
`errorText`); `listen`/`UnlistenFn` + a `Channel` (progress via `channel:{id}` events); `getCurrentWindow()`
(controls marshaled to the shell main thread, `scaleFactor`/`isMaximized` reads, `onResized` off the
shell's `window-resized` event); `getCurrentWebview().onDragDropEvent`; and `open`/`save` dialogs. All
**17** `@tauri-apps` import sites were swapped to `../shell` (duplicate imports merged), and the
`@tauri-apps/api`, `@tauri-apps/plugin-dialog`, `@tauri-apps/cli` deps + the `tauri`/`tauri:dev`
scripts were removed from `package.json`. `bun run check` (gen:protocol + `tsc --noEmit`) and
`bun run lint` pass; no `@tauri-apps`/`__TAURI` references remain in `src`. The `invoke`/`listen`/window
paths hit the real Phase-4 dispatch; the `dialog_*`, `drag-drop`, and `channel:*` native sides landed in
Phase 7.

**Scope:** `editor/src` (a new internal `src/shell/` module set + a mechanical import-specifier swap
across the ~17 files that import `@tauri-apps/*`), `editor/package.json` (delete the three
`@tauri-apps/*` dependencies + the now-dead `tauri`/`tauri:dev` scripts), `editor/vite.config.ts`
(drop the dead `TAURI_` env prefix). No React component body, no `control/client.ts` logic, no
`storefront/types.ts` wire type, and no generated protocol artifact changes — only import lines and
the new shim modules.
**Depends on:** phase-4-ipc-bridge-and-command-surface.md (the shim is a *thin* front over the CEF
bridge that phase stands up: `window.cefQuery` for request/response, the injected `window.__saffron`
bootstrap for pushed events, and the re-hosted command surface those call into).

## Goal

Point the React/Vite frontend at the CEF bridge through **one** internal shell shim, so every
component keeps the exact `@tauri-apps/*` call shapes it uses today while the transport underneath
becomes CEF, and then **delete every `@tauri-apps` dependency** — no dual path, no compat re-export
kept alive. After this phase the frontend targets the CEF shell only: the `src/shell/` modules are
backed by `window.__saffron` / `window.cefQuery` (Phase 4), which the WebKitGTK/Tauri shell never
injects, so the frontend no longer runs under the old shell. That is the intended cutover of the
*frontend* onto CEF; the physical deletion of the `src-tauri` crate and the dev-loop/justfile rewrite
are Phase 8's, but the frontend's commitment to the CEF bridge lands here.

The design is deliberately mechanical: the frontend's coupling to Tauri is narrow (five `@tauri-apps`
submodules across a countable set of import lines, all with stable, well-understood shapes), so the
correct move is to reproduce those shapes exactly in `src/shell/` and rewrite import specifiers,
rather than touch any call site's logic. The load-bearing invariants — the JSON-over-unix-socket
control plane funneled through `control/client.ts`'s single `call()`, the `{ message, code }`
rejection shape its `toControlError` reads, entity-IDs-as-strings, the generated protocol types, and
the `CommandName` union — pass through untouched because no code that depends on them changes.

## Background — the exact Tauri surface the frontend imports today

Verified against the tree, the frontend touches Tauri through five `@tauri-apps` submodules, spread
over 17 files (23 import statements):

- **`@tauri-apps/api/core` → `invoke` (and `Channel`).** `control/client.ts` imports `invoke` and
  funnels **every** engine call through one private `call<C>(cmd, params)` that does
  `invoke<...>("control", { cmd, params: params ?? {} })` and, on reject, `throw toControlError(raw)`.
  The ~120 typed wrappers on the exported `client` object (`listEntities`, `inspect`, …) all bottom
  out there. The storefront calls `invoke` directly for its shell-local commands
  (`storefront/types.ts`, `StoreResultsGrid.tsx`, `ProviderModal.tsx`, `StoreCredits.tsx`,
  `AssetDetailModal.tsx`), and `ProjectMenu.tsx` / `CaptureControls.tsx` / `ScriptSlots.tsx` invoke
  named editor-local commands (`open_in_vscode`, `open_project_folder`, `write_file`, `open_external`).
  `storefront/types.ts` additionally imports `Channel` (`new Channel<number>()`, `channel.onmessage`)
  and passes it into `invoke("store_import", …)` for download progress.
- **`@tauri-apps/api/event` → `listen` + `UnlistenFn`.** `App.tsx` subscribes `engine-phase` and
  `viewport-error`; `useMouseBindings.ts` and `SettingsModal.tsx` subscribe `mouse-button`. Every
  handler reads `event.payload`; every subscription returns a `Promise<UnlistenFn>` and is unlistened
  idempotently (React StrictMode double-mounts). The bus is **push-only** — no JS→Rust `emit` anywhere.
- **`@tauri-apps/api/window` → `getCurrentWindow`.** `WindowTitlebar.tsx` calls it **at module-eval**
  (`const appWindow = getCurrentWindow()` at top level), then uses `.minimize()`, `.toggleMaximize()`,
  `.isMaximized()`, `.close()`, `.startDragging()`, `.onResized(cb)`. `App.tsx` calls `.show()`
  (reveal-on-first-paint), `ProjectMenu.tsx` calls `.close()`, and `useSubsurfaceBounds.ts` calls
  `.scaleFactor()`. Because of the module-eval call, `getCurrentWindow()` **must return synchronously**
  (a stable object) before first render and before `window.__saffron` necessarily exists.
- **`@tauri-apps/api/webview` → `getCurrentWebview`.** `AssetsPanel.tsx` calls
  `.onDragDropEvent(cb)`; the callback reads `event.payload` with `type: "enter" | "over" | "leave"
  | "drop"`, a `position: { x, y }`, and (on `drop`) `paths: string[]`, and returns a
  `Promise<UnlistenFn>`.
- **`@tauri-apps/plugin-dialog` → `open` / `save`.** Six sites (`ProjectMenu.tsx`,
  `ProjectStartupModal.tsx`, `AssetsPanel.tsx`, `ScriptSlots.tsx`, `CaptureControls.tsx`,
  `ExportModal.tsx`) call `open({ directory?, multiple?, filters })` or `save({ defaultPath?,
  filters })`. All rely on **null-on-cancel** (`state/store.ts` `withNativeDialog` re-entry lock plus
  every caller branching on a falsy result).

`vite.config.ts` declares `envPrefix: ["VITE_", "TAURI_"]`, but there is **no** `import.meta.env.TAURI_`
read anywhere in `src/` (only `VITE_SAFFRON_DEV_MODE`), so the `TAURI_` prefix is dead. `cachedImage.ts`
builds a `saffron-img://fetch/?u=<enc>` URL string and imports nothing from Tauri — it is untouched
here (the scheme is served by the CEF scheme handler in Phase 7).

## Build plan

### 1. Add the `src/shell/` module set — one thin front per Tauri submodule

Create `editor/src/shell/` with one module per submodule the frontend imports, each re-exporting the
**exact** names and shapes the call sites use. Every module is built on the Phase-4 primitives
(`window.cefQuery`, `window.__saffron`) — `window.ts`, `webview.ts`, and `dialog.ts` are themselves
implemented **on top of** `ipc.ts` (invoke) and `events.ts` (listen), so the shim adds one native
transport, not five.

- **`shell/ipc.ts` — `invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>`.** Marshals
  `args` as a single named-key object and forwards to the Phase-4 `window.cefQuery` request/response
  router; the named keys reach the re-hosted native command unchanged (Phase 4 re-hosts each command
  to accept the exact JS key names the frontend already sends — e.g. `set_viewport_bounds` with
  `{ view, bounds, resizeEngine }`, `control` with `{ cmd, params }`, `remember_recent_project` with
  `{ project }`). **The rejection SHAPE is load-bearing:** on a failed query, `invoke` MUST reject
  with the bridge's `{ message, code }` **object**, not a flattened string, because
  `control/client.ts`'s `toControlError` reads `.message` and `.code` off the rejected value and
  `isBusyLoading` keys off `code === "busy-loading"` to drop background-poll noise. A bare-string
  reject (for a non-`control` command whose native side failed generically) is fine — `toControlError`
  already handles both — but the `code`-carrying object path must survive. **Re-export `Channel` here**
  (from `shell/channel`) so a site that imports `{ Channel, invoke }` from `@tauri-apps/api/core`
  (i.e. `storefront/types.ts`) needs only a one-specifier swap, not a member split.
- **`shell/events.ts` — `listen<T>(event, handler): Promise<UnlistenFn>` + `type UnlistenFn = () =>
  void`.** Subscribes to the Phase-4 push channel: the injected `window.__saffron` bootstrap delivers
  each pushed event as a DOM `CustomEvent`, and `listen` adds a DOM listener that calls
  `handler({ payload })`, returning an idempotent unsubscribe. Must support **multiple independent
  subscribers per name** (`mouse-button` has two: `useMouseBindings.ts` + `SettingsModal.tsx`) and an
  unlisten that is safe to call twice (StrictMode). Event names are exactly `engine-phase`,
  `viewport-error`, `mouse-button` (plus any window events `shell/window` subscribes internally, see
  below).
- **`shell/window.ts` — `getCurrentWindow(): ShellWindow`.** Returns a **synchronously-constructed,
  module-level singleton** (so `const appWindow = getCurrentWindow()` at `WindowTitlebar.tsx`'s top
  level never awaits or throws before first paint). Its methods are async wrappers over `invoke`
  (`show`, `minimize`, `toggleMaximize`, `close`, `startDragging`, `isMaximized(): Promise<boolean>`,
  `scaleFactor(): Promise<number>`) and one over `listen` (`onResized(cb): Promise<UnlistenFn>`,
  backed by a native `window-resized` push event). These call the native window-control commands the
  shell provides for the winit toplevel (owned by Phase 4/Phase 6); `window.ts` stays decoupled from
  which native phase lands each command — it only names them through `invoke`.
- **`shell/webview.ts` — `getCurrentWebview(): ShellWebview`.** Returns a singleton whose
  `onDragDropEvent(cb): Promise<UnlistenFn>` subscribes (via `listen`) to the native `file-drop` push
  event and re-shapes it into the `{ payload: { type, position: { x, y }, paths } }` object
  `AssetsPanel.tsx` reads. The OS-drop payload is produced host-side in Phase 7 (winit
  `HoveredFile`/`DroppedFile` + a tracked last-pointer position for the drop coordinates); this module
  is only the JS-side re-shaper.
- **`shell/dialog.ts` — `open(opts?)` / `save(opts?)`.** Async wrappers over `invoke` to the native
  file-chooser commands (rfd, Phase 7). `open` resolves `string | string[] | null`, `save` resolves
  `string | null`, both returning **`null` on cancel** — the semantic every caller and
  `withNativeDialog` branch on. Options mirror the plugin's used surface: `{ directory?, multiple?,
  filters?: { name, extensions }[], defaultPath? }`.
- **`shell/channel.ts` — `class Channel<T>` with `onmessage: (msg: T) => void`.** Reproduces the Tauri
  `Channel` used by `storefront/types.ts` (`new Channel<number>()`, set `channel.onmessage`, pass the
  instance inside the `invoke("store_import", …)` args). Backed by the Phase-4 progress-streaming
  primitive: the channel allocates an id, registers its `onmessage` against `window.__saffron`, and
  serializes (via `toJSON`) to a plain `{ channelId }` marker when embedded in the invoke params, so
  the native side streams N `{ channelId, payload }` frames the shim dispatches to `onmessage` before
  the outer `invoke` promise resolves with the final result.

Add an ambient type declaration (e.g. `shell/global.d.ts`) for `window.__saffron` / `window.cefQuery`
so `tsc --noEmit` sees the bridge globals. Keep the shim's public types identical to what the call
sites already expect (`UnlistenFn`, the drag-drop payload union, the dialog option/return unions) so
no consumer's type annotations change.

### 2. Mechanical import-specifier swap across the 17 files

With the shim in place, rewrite only the import specifiers — no imported member names change, no
component body changes:

- `@tauri-apps/api/core` → `shell/ipc` — `control/client.ts`, `storefront/types.ts`
  (`{ Channel, invoke }`, both re-exported by `shell/ipc`), `storefront/StoreResultsGrid.tsx`,
  `storefront/ProviderModal.tsx`, `storefront/StoreCredits.tsx`, `storefront/AssetDetailModal.tsx`,
  `app/ProjectMenu.tsx`, `components/CaptureControls.tsx`, `components/ScriptSlots.tsx`.
- `@tauri-apps/api/event` → `shell/events` — `app/App.tsx`, `app/useMouseBindings.ts`,
  `app/SettingsModal.tsx`.
- `@tauri-apps/api/window` → `shell/window` — `app/App.tsx`, `app/WindowTitlebar.tsx`,
  `app/ProjectMenu.tsx`, `lib/useSubsurfaceBounds.ts`.
- `@tauri-apps/api/webview` → `shell/webview` — `panels/AssetsPanel.tsx`.
- `@tauri-apps/plugin-dialog` → `shell/dialog` — `app/ProjectMenu.tsx`, `app/ProjectStartupModal.tsx`,
  `panels/AssetsPanel.tsx`, `components/ScriptSlots.tsx`, `components/CaptureControls.tsx`,
  `app/ExportModal.tsx`.

`control/client.ts`'s `call`/`toControlError`/`ControlError` and its ~120 wrappers, `storefront/types.ts`'s
wire types and `Channel` construction, and `storefront/cachedImage.ts` stay byte-identical apart from
`client.ts`'s and `types.ts`'s single import lines. Prefer a path alias (a `shell` import root via the
existing tsconfig/vite alias convention) or consistent relative paths so the swap is uniform.

### 3. Delete the `@tauri-apps` dependencies and the dead env prefix

Once no `src/` file imports `@tauri-apps/*` (see Verification), remove from `editor/package.json`:
`@tauri-apps/api` and `@tauri-apps/plugin-dialog` (the runtime deps the shim replaces) and
`@tauri-apps/cli` (the dev dep), plus the now-orphaned `tauri` / `tauri:dev` scripts, which only wrap
the removed CLI and target a shell the frontend no longer runs under. Keep `build`
(`gen:protocol + tsc + vite build`), `dev` (`vite`), `check`, `format`, `lint` — none of them touch
Tauri. Refresh the lockfile. The justfile `run` / `run-debug` / `run-software` recipe rewrite (to
launch the CEF binary instead of `bun run tauri dev`) is **Phase 8's**; by this phase the working dev
launch is already the CEF shell loading the Vite dev URL (established in Phase 3), so the removed
scripts are superseded, not merely dropped.

In `editor/vite.config.ts`, change `envPrefix: ["VITE_", "TAURI_"]` → `envPrefix: ["VITE_"]`. Verified
there is no `import.meta.env.TAURI_` read in the frontend, so this is a pure dead-code removal;
`VITE_SAFFRON_DEV_MODE` (read in `state/store.ts`) keeps working under the retained `VITE_` prefix.

### 4. Resolve the mouse side-button open question (buttons 8/9)

Today `useMouseBindings.ts` and `SettingsModal.tsx` `listen("mouse-button", …)` for back/forward
(codes 8/9) because WebKitGTK swallows them and the GTK shell re-emits them as a native event. Under
CEF OSR the host owns all input forwarding, so this phase must **empirically determine** whether
Chromium delivers those buttons to the DOM:

- **If Chromium delivers them to the DOM** (as `mousedown` / `pointerdown` with `button` 3/4 when the
  host forwards the raw winit button presses into the page): convert `useMouseBindings.ts` and
  `SettingsModal.tsx` to plain DOM handlers, delete the `listen("mouse-button", …)` usage, drop the
  `mouse-button` name from `shell/events.ts`'s expected set, and delete the native `mouse-button`
  event path (the host's winit intercept + shell event) — one code path, no bridge event for buttons.
- **If Chromium does not** (the strongly-indicated case: CEF's injected mouse-input surface carries
  only `LEFT`/`MIDDLE`/`RIGHT` button types, so back/forward presses cannot be delivered through the
  standard OSR input injection to reach the DOM): keep the native path — the host detects the winit
  button 8/9 and pushes a `mouse-button` event, and `useMouseBindings.ts` / `SettingsModal.tsx` stay
  exactly as they are (only their event-import specifier changed in step 2). No frontend logic change
  either way.

This decision is confined to these two files and the shell event surface; it does not affect the
control plane, the storefront, or any other shim module. Whichever branch holds, obey NO-LEGACY:
there is exactly one delivery path for buttons 8/9 afterward, not both.

## Invariants held (unchanged by this phase)

- **The control plane.** `control/client.ts`'s single `call()` still does `invoke("control",
  { cmd, params })`; only its `invoke` import origin changed. The unix-socket round-trip, the
  `CONTROL_IO` single-flight serialization, and the engine `ok:false → reject` semantics live on the
  native side (Phase 4) and are not touched here.
- **The rejection shape.** `ControlError` / `toControlError` / `isBusyLoading` are unchanged; they keep
  working because `shell/ipc.ts` rejects with `{ message, code }`. The `notifyError` / Toaster error
  path that consumes `ControlError` is therefore intact.
- **Entity IDs stay opaque strings** end-to-end; nothing in the shim parses or `Number()`s an id.
- **Generated protocol types** (`src/protocol/sa-types.ts`) and the **`CommandName` union** are
  untouched — the shim is transport, not schema.
- **`storefront/types.ts`** stays hand-authored to mirror the connectors' camelCase wire types (it is
  *not* under the generated-protocol rule); only its `@tauri-apps/api/core` import line changes.

## Verification

Concrete against the repo gate (`just prepare-for-commit` = format + lint; `bun run check`; `bun run
build`), plus a run that confirms every migrated surface operates through the shim:

- **Zero `@tauri-apps` imports remain.** `grep -rn "@tauri-apps" editor/src` returns nothing, and
  `grep -rn "@tauri-apps" editor/package.json` shows only the (removed) history — after the edit the
  three deps and the two scripts are gone. `grep -rn "TAURI_" editor/vite.config.ts` returns nothing.
- **`bun run check` passes** (`gen:protocol` + `tsc --noEmit`) with the shim types resolving every
  former Tauri import — no `any` leakage, `UnlistenFn`/`Channel`/dialog-return/drag-drop-payload types
  identical to before at every call site.
- **`bun run build`** (`gen:protocol + tsc + vite build`) produces `editor/dist`, and the built UI
  **loads in the CEF shell** with every migrated surface exercised end-to-end: the titlebar controls
  (minimize / toggle-maximize / close / drag / reveal-on-first-paint) act on the winit toplevel;
  `open`/`save` dialogs open the native chooser and return `null` on cancel; the Assets panel receives
  OS file drops through `onDragDropEvent`; the storefront import streams progress through the
  `Channel` shim; and a `control` call that the engine rejects with `code: "busy-loading"` is dropped
  by `isBusyLoading` (proving the `{ message, code }` reject shape survived the shim).
- **`oxlint` (`bun run lint`) is clean** — no unused imports left by the swap, no shim-module lint
  violations.
- **Mouse side-buttons:** whichever branch of step 4 is taken, back/forward buttons drive the tab
  back/forward commands in the running CEF shell (either via DOM events or via the retained
  `mouse-button` push), and the settings capture flow records them.

CPU-gate-able portions (`bun run check` + `oxlint`) gate without a running shell; the loads-in-the-CEF-shell
and mouse-button checks require the Phase-2/3/4 shell to be up and are the GPU-with-a-window checks for
this phase.

## Risks

- **Rejection-shape regression is silent.** If `shell/ipc.ts` ever flattens a `code`-carrying reject to
  a string, `bun run check` and lint still pass, but `isBusyLoading` stops dropping background-poll
  errors and the editor spams toasts under load. Treat the `{ message, code }` object reject as a
  contract with a targeted test, not a detail — it is the one behavioral coupling the mechanical swap
  can break.
- **Synchronous `getCurrentWindow()` is a hard constraint.** `WindowTitlebar.tsx` calls it at
  module-eval; a lazy/async-only `shell/window` that awaits `window.__saffron` at construction would
  crash the initial render. The singleton must be constructable before the bridge is ready, deferring
  the actual `invoke` to method-call time.
- **Channel serialization coupling.** The `Channel` shim must serialize to the exact marker the Phase-4
  progress router expects when embedded inside an `invoke` params object; a mismatch means
  `store_import` resolves but never fires `onmessage`, so the import progress bar is dead while
  everything else works — easy to miss without exercising a real storefront import.
- **Multiple-subscriber / idempotent-unlisten correctness.** `mouse-button` has two independent
  subscribers and every subscription is torn down under StrictMode double-mount; a shim `listen` that
  clobbers a prior subscriber or throws on double-unlisten produces intermittent, mount-order-dependent
  bugs. Reproduce the multi-subscriber + double-unlisten path in the shell before trusting it.
- **Frontend commits to CEF at this phase.** After the swap the frontend only runs under the CEF shell
  (the shim needs `window.__saffron` / `window.cefQuery`, which the Tauri shell never injects). This is
  the intended cutover, but it means Phases 5–7 develop the frontend exclusively against the CEF shell;
  the old `bun run tauri dev` path is no longer a fallback for frontend work, so the CEF shell's dev
  launch (Phase 3) must be reliable before this lands.
- **Path-alias / bundler config.** If the `shell` import root is an alias, both `tsc` and Vite must
  resolve it; a mismatch shows up as build-only or check-only failures. Prefer reusing the existing
  alias convention rather than introducing a new one.
