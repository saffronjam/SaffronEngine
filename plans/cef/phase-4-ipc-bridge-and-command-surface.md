# Phase 4 — JS↔native IPC bridge (invoke + events + Channel) and portable command re-host

**Status:** COMPLETED — the transport, the full command surface, window controls, and the Rust→JS event
push are all built and exercised end to end by the running editor; the flow-by-flow GUI confirmation is
folded into Phase 9 §2. The control passthrough (the one socket round-trip the whole bridge is built
on) is ported into `editor/shell` (`src/control.rs`) verbatim and shell-agnostic: newline-delimited JSON
over the per-PID unix socket, the `CONTROL_IO` single-flight serialization, and the engine's `ok:false`
reply surfaced as a typed `ControlError { message, code }`. **Unit-tested and passing** against a mock
control server (ok→result with the correct `{id,cmd,params}` envelope; error→message+code). The JS↔native message-router
transport is now **wired end to end in Rust** (`src/ipc.rs`, compile-verified, shell smoke-clean): the
browser-side `BrowserSideRouter` + a `ControlQueryHandler` that parses `{cmd, params}` and runs the
control passthrough **on a worker thread** (never the UI thread), completing the query with
`onSuccess(result)` / `onFailure(code, {message, code})`; the `Client` forwards
`on_process_message_received` and returns the router's `LifeSpanHandler` (`on_before_close` cancels
pending queries); and the `App` returns a `RenderProcessHandler` that drives a `RendererSideRouter`
(`on_context_created`/`on_context_released`/`on_process_message_received`) so `window.cefQuery` is
registered in each V8 context.

The transport carries a **name-dispatched command surface** (`src/commands.rs`, `dispatch(state,
command, args)`): the query request is `{command, args}` — `invoke(name, args)`'s wire form. Wired and
unit-tested (6 tests green, headless smoke clean): the generic **`control` passthrough** (all ~120 typed
control commands); **engine supervision** (`start_engine`/`quit_engine`/`engine_alive` via
`src/engine.rs` — `spawn_engine` with the viewport/shm/socket env + NVIDIA ICD, `try_wait` liveness,
quit→kill→unlink teardown); the **file/OS/trace helpers** (`write_file`, `open_external`,
`open_in_vscode`, `open_project_folder`, `app_data_info`, `serve_trace`); and **settings/recents**
(`load`/`save_editor_settings`, `list`/`remember_recent_project` via `src/settings.rs`). The two
remaining command groups return a typed `unimplemented` failure (the UI toasts it, never hangs) until
their owning phase lands: the **presenter** commands (`set_viewport_bounds`/`set_viewport_parked`/
`viewport_refresh_hz`) with Phase 6, and the **store/connector** commands with Phase 7.

**Now complete:** window controls are wired (`minimize`/`toggle_maximize`/`start_drag`/`show` posted as
`ShellRequest::Window` through the state inbox and applied on the main thread in
`Shell::apply_window_action`, where the winit window lives); the Rust→JS **event push path** runs
(`Shell::emit_to_js` executes `window.__saffronShellEvent(event, payload)` in the main frame, feeding
`engine-phase`, `drag-drop`, and the storefront `channel:{id}` progress streams). The whole bridge is
exercised end to end by the running editor — the user drives menus, modals, the control passthrough,
engine supervision, and storefront import through it. The one remaining check is the flow-by-flow GUI
walk, which is Phase 9 §2's job.

This is the phase that makes the CEF shell *drivable* by the frontend. Phase 3 has the React/Vite build
painting into the host-owned Wayland toplevel via CEF OSR; this phase gives that page a native transport
so it can call Rust and receive pushes back — replacing Tauri's `invoke`/`emit`/`listen`/`Channel` with a
CEF `CefMessageRouter` bridge (`window.cefQuery`) plus a browser→render `CefProcessMessage` event path — and
re-hosts every shell-agnostic command handler currently in `editor/src-tauri/src/lib.rs` on that bridge.
The load-bearing invariant it must not disturb: the JSON-over-unix-socket **control plane** — the single
generic `control(cmd, params)` passthrough (`lib.rs:607`) that all ~120 typed wrappers in
`editor/src/control/client.ts` funnel through, its `control_request_with_params` round-trip helper
(`lib.rs:493`), its `CONTROL_IO` single-flight mutex (`lib.rs:462`), and its typed
`ControlError { message, code }` rejection (`lib.rs:468`) — moves **verbatim**; only the `#[tauri::command]`
attribute and `State<T>` injection are replaced. Lifecycle + presenter commands
(`start_engine` / `set_viewport_bounds` / `set_viewport_parked` / `viewport_refresh_hz` / `quit_engine` /
`engine_alive`) are **Phase 6**; native-primitive services (file dialogs, OS drag-drop, the `saffron-img://`
scheme) are **Phase 7**. This phase owns the transport plus the 22 commands that are pure
std/reqwest/keyring Rust.

> The frontend still imports `invoke`/`listen`/`Channel` from `@tauri-apps/api` at the end of this phase.
> The `src/shell/` bridge shim and the mechanical import swap across the 17 call sites are **Phase 5**; this
> phase installs the native `window.__saffron` bootstrap the shim will wrap, and is verified by driving
> that bootstrap directly (devtools / a one-off harness) against a manually-started engine socket. The
> Tauri `#[tauri::command]` originals are deleted **wholesale in Phase 8**, not kept as a parallel path —
> per NO-LEGACY there is one shell at any point in the shipped tree; the new shell crate is the in-progress
> construction of the replacement, and this phase authors its command surface.

## Goal

Stand up the two IPC directions on the `cef` crate (tauri-apps/cef-rs) and re-host the shell-agnostic
commands on them, preserving the exact string/JSON contract the frontend already speaks:

1. **`invoke(cmd, args) → Promise`** over `CefMessageRouter` (`window.cefQuery`): the JS side marshals
   `args` as a **single named-key object** whose camelCase keys map onto the Rust command's params exactly
   as Tauri did (`{ view, bounds, resizeEngine }`, `{ cmd, params }`, `{ project }`, `{ settings }`, …); the
   Rust side is **re-declared** to accept those exact keys via one `#[derive(Deserialize)]
   #[serde(rename_all = "camelCase")]` params struct per command, so there is exactly one deserialization
   rule and no hidden auto-conversion magic. A command that fails rejects the Promise with the **original**
   error value — `ControlError { message, code }` for the control passthrough (so `client.ts`'s
   `toControlError`/`isBusyLoading` still read `.message` and `.code`), a bare string for a non-control
   command (whose Rust body returns `Result<T, String>`).

2. **Control passthrough preserved verbatim**: `control_request_with_params` + `control_request` + the
   `CONTROL_IO` mutex move byte-for-byte into the shell crate; the invoke bridge keeps **exactly one socket
   round-trip outstanding** regardless of how many handlers run concurrently.

3. **Rust→JS event push**: a browser→render `CefProcessMessage` delivered by an injected `window.__saffron`
   bootstrap (installed in `on_context_created`) as a DOM `CustomEvent`, exposing `listen(name, cb) →
   UnlistenFn` for `engine-phase` / `viewport-error` / `mouse-button`, with multiple subscribers per name
   and idempotent unlisten (React StrictMode double-mounts).

4. **`Channel<f64>` equivalent** for `store_import` download progress: a channel id allocated in JS and
   passed as a command arg; the native handler pushes `{ channelId, fraction }` frames over a persistent
   `cefQuery`, then resolves the outer invoke — the connector `download()` already takes a plain
   `Fn(f64)` (`ProgressFn`, `connectors/mod.rs:339`), so its body is untouched.

5. **Re-host** `control`, `app_data_info`, `list_recent_projects`, `remember_recent_project`,
   `load_editor_settings`, `save_editor_settings`, `write_file`, `open_external`, `open_in_vscode`,
   `open_project_folder`, `serve_trace`, and the full `store_*` / `connector_*` set (with the `connectors/`
   module + `credentials` keyring moved verbatim) — 22 command handlers, bodies unchanged, only the wrapper
   changing.

## Build plan (grounded in the current `lib.rs` + `connectors/` + `client.ts`)

The template is `editor/src-tauri/src/lib.rs`: nearly every command body is portable std/reqwest/keyring
Rust, and the couplings to replace are exactly the `#[tauri::command]` attribute, the `State<T>` extraction,
the `tauri::ipc::Channel<f64>` in `store_import` (`lib.rs:678`), the `handle.emit(...)` event sites
(`lib.rs:1124/1135/1139/1143`, `wayland_viewport.rs`), and — on the frontend — the `@tauri-apps/api/core`
`invoke`/`Channel` and `@tauri-apps/api/event` `listen` imports.

### 1. The message-router bridge — `invoke(cmd, args) → Promise`

Configure the renderer-side `MessageRouterConfig` (cef-rs `cef/src/wrapper/message_router.rs`) so it injects
`window.cefQuery({ request, persistent, onSuccess, onFailure })` into every frame, and register one
browser-side handler (`BrowserSideHandler::on_query_str`) that owns command dispatch. The wire envelope is a
single JSON string (`cefQuery`'s `request` is a string, so `invoke`'s named-key `args` object is serialized
into it), chosen so both directions carry structured data through one channel:

```
request  := { "kind": "invoke", "command": <string>, "args": <object>, "channels": { <argKey>: <channelId> } }
reply     := { "ok": true,  "value": <json> }
          |  { "ok": false, "error": <ControlError|string> }   // completed via onSuccess, NOT onFailure
```

The browser-side `on_query_str`:

1. parses the envelope, looks the command up by name in a static dispatch table;
2. deserializes `args` into that command's `#[derive(Deserialize)] #[serde(rename_all = "camelCase")]` params
   struct — this is the **re-declare-the-Rust-commands** decision: e.g. `set_viewport_bounds` (Phase 6)
   becomes `struct SetViewportBoundsArgs { view: String, bounds: ViewportBounds, resize_engine: bool }`
   deserialized from `{ view, bounds, resizeEngine }`, and `control` becomes `struct ControlArgs { cmd:
   String, params: Option<Value> }` from `{ cmd, params }`. One serde rule replaces Tauri's implicit
   camelCase→snake_case argument conversion, so the frontend's existing key names (`resizeEngine`,
   `connectorId`, `onProgress`, `project`, `settings`) are honored unchanged;
3. offloads the body to the shell's command executor (§2) and, on completion, completes the query.

**Rejection shape (invariant).** A command **never** completes through `cefQuery`'s `onFailure(code:int,
msg:string)` for its own error — that two-arg form cannot carry `{ message, code }`. Instead every completed
command calls `success_str(json)` with the `reply` envelope above, and the JS wrapper branches on `ok`:

```js
window.__saffron.invoke = (command, args) => new Promise((resolve, reject) => {
  window.cefQuery({
    request: JSON.stringify(buildEnvelope(command, args)),
    onSuccess: (res) => { const r = JSON.parse(res); r.ok ? resolve(r.value) : reject(r.error); },
    onFailure: (_code, msg) => reject(msg),   // bridge/transport failure only → bare string
  });
});
```

So the control passthrough's `ControlError { message, code }` reaches JS as an **object** with both fields
(`toControlError` at `client.ts:153` reads `.message`/`.code`; `isBusyLoading` at `client.ts:147` matches
`code === "busy-loading"` to drop background-poll errors), and a non-control command's `String` error reaches
JS as a bare string (`toControlError` handles both, `client.ts:157-164`). `onFailure` is reserved for the
router itself failing (renderer/browser process gone) and rejects with a bare string, which `toControlError`
also coerces.

### 2. Threading + the `CONTROL_IO` single-flight (invariant)

`BrowserSideHandler::on_query_str` runs on the CEF browser (UI) thread — the same thread that drives
`do_message_loop_work()` from the winit loop (Phase 2/3). Blocking the unix-socket round-trip there would
stall the message pump and the UI. So the handler **offloads**: the shell owns a small command executor (a
`tokio` runtime — the `store_*` handlers are already `async` and `connector_login` already uses
`spawn_blocking`, `lib.rs:855`), and `on_query_str` spawns the body onto it, then completes the query's
`BrowserSideCallback` from the worker (the callback is refcounted and completable off the UI thread — verify
in the pinned cef-rs, see Risks).

`control_request_with_params` / `control_request` / the `static CONTROL_IO: Mutex<()>` move **verbatim** into
the shell crate (`lib.rs:462-544`). Because `CONTROL_IO` is a process `static`, it serializes the actual
socket round-trip across every worker regardless of the thread model — exactly one connect+write+read is
outstanding at a time, which is the property that keeps concurrent invokes from piling into the engine's
per-frame control drain and tripping its 5 s read timeout (`os error 11`). This matches Tauri's model
(async commands ran concurrently on Tauri's runtime, serialized by `CONTROL_IO`); the CEF bridge reproduces
it (concurrent handlers, one socket round-trip). The `Duration::from_millis(5000)` read timeout, the
newline-delimited framing, and the `ok:false → Err(ControlError)` decode are unchanged.

### 3. The shell state container (replacing `State<EditorState>` / `State<ConnectorRuntime>`)

Tauri's `manage`/`State<T>` DI (`lib.rs:1255-1257`) is replaced by a shell-owned `ShellState`, constructed
once at startup and handed to the browser-side handler (an `Arc<ShellState>` field on the handler struct).
For this phase's command set it carries:

- `socket_path: String` (`socket_path()`, `lib.rs:228`) — needed by `control`, `store_import`,
  `store_import_part`;
- the trace loopback state `trace: Arc<Mutex<Option<Vec<u8>>>>` + `trace_port: Option<u16>`, with
  `start_trace_server` bound at construction (`lib.rs:126-134`, `148`) — needed by `serve_trace`;
- `connectors: ConnectorRuntime` (`connectors/mod.rs:262`, `ConnectorRuntime::new`) — the Asset Store
  backend for the `store_*` / `connector_*` set;
- the command executor (§2) and a handle to the browser/main frame for event emit (§4).

The engine-child `Mutex<Option<Child>>` and `viewports` fields (`lib.rs:27-29`) are **Phase 6** (lifecycle).
`connectors::Credentials::global()` (`lib.rs:825`) is a process singleton and needs no injection. Free
functions with no state — `app_data_info`, `list_recent_projects`, `remember_recent_project`,
`load_editor_settings`, `save_editor_settings`, `write_file`, `open_external`, `open_in_vscode`,
`open_project_folder` (and their helpers `app_data_dir`/`ensure_app_dirs`/`read_*_file`/`write_*_file`/
`open_url_in_browser`) — move verbatim and are called directly from the dispatch table.

### 4. Rust→JS event push — `listen(name, cb) → UnlistenFn`

Replace `AppHandle::emit(name, payload)` (the three event sites) with a browser→render broadcast:

- **Browser side:** `ShellState::emit(name: &str, payload: impl Serialize)` builds a `ProcessMessage`
  (`{ event, payload }`) and `frame.send_process_message(ProcessId::RENDERER, msg)` to the browser's main
  frame. This is the primitive; the *emit call sites* for `engine-phase` / `viewport-error` are wired by
  **Phase 6** (they live in the lifecycle `auto_start`/`setup`/`teardown` paths, `lib.rs:1124-1143`), and
  `mouse-button` is wired by Phase 6 or deleted (Chromium may deliver mouse buttons 8/9 to the DOM directly
  — resolved in Phase 5). This phase lands `emit` + a dev-only test emit to prove the round-trip.
- **Render side:** a `RenderProcessHandler` whose `on_process_message_received` receives `{ event, payload }`
  and delivers it into JS via `frame.execute_java_script("window.__saffron.__dispatch(<event>, <payload>)")`
  (or a registered V8 callback). `on_context_created` installs the `window.__saffron` bootstrap **before**
  page scripts run, so `listen` exists at page load.
- **JS bootstrap:** `listen(name, cb)` registers `cb` in a `Map<name, Set<cb>>` (or via
  `window.addEventListener("saffron:" + name, …)`), and `__dispatch(name, payload)` invokes every registered
  callback with the Tauri-shaped event object `{ payload }` (App.tsx reads `event.payload`, `App.tsx:169`).
  `listen` returns an `UnlistenFn` that removes just that callback and is **idempotent** (removing twice is a
  no-op) so StrictMode's double-mount cleanup is safe. Multiple subscribers per name is native to the Set /
  DOM model — `mouse-button` has two (`useMouseBindings.ts:68`, `SettingsModal.tsx:199`). The bus is
  **push-only**: there is no JS→Rust `emit` anywhere in the frontend, so the render→browser direction of the
  event path is not needed.

### 5. `Channel<f64>` — persistent-query progress stream

`storefront/types.ts` constructs `new Channel<number>()`, sets `channel.onmessage = onProgress`, and passes
it as the `onProgress` arg to `store_import` (`types.ts:103-109`); the Rust `store_import` throttles the
connector's `download()` progress to whole-percent changes and `on_progress.send(f)` each (`lib.rs:689-695`).
Reproduce this as a keyed persistent stream:

- The JS `Channel` shim allocates a `channelId`, opens a **persistent** `cefQuery`
  (`{ request: JSON({ kind: "channel", channelId }), persistent: true, onSuccess: (frame) => onmessage(JSON.parse(frame).fraction) }`),
  and, when serialized as a command arg, emits a marker `{ __channel: channelId }` (carried in the envelope's
  `channels` map, §1).
- The browser-side handler, dispatching `store_import`, resolves `channelId` to the registered persistent
  query's `BrowserSideCallback` and constructs the `report: &ProgressFn` closure so each throttled tick calls
  `callback.success_str(json!({ "fraction": f }))` (a persistent query may complete `success` many times).
  The connector `download(&descriptor, &report)` body (`lib.rs:696`) is unchanged. When `store_import`
  returns, the **outer** invoke query resolves with the `ImportedAsset`, and the persistent progress query is
  torn down (final `success`/`failure`, or JS `cefQueryCancel`).

The `store_import` throttle (`AtomicU32` whole-percent gate, `lib.rs:689-695`) and the
`StoreKind`→importer routing (`lib.rs:711-721`, which calls `control_request_with_params` for the final
import) move verbatim. This is the one command needing the streaming primitive; every other `store_*` /
`connector_*` command is a plain request/response invoke. (A `CefProcessMessage` broadcast — the §4 event
path — was considered and rejected for progress: `Channel` semantics are per-call and keyed, so a dedicated
per-call persistent query is cleaner than a global event with client-side call correlation.)

### 6. Re-host the store / connector command set + the `connectors/` module

The `connectors/` module (registry, session, cache, oauth_loopback, credentials, polyhaven / ambientcg /
polypizza / sketchfab) is **verified shell-agnostic** — no `tauri::` references, only reqwest / keyring v3
(`Entry::new`) / tokio / zip / async-trait. It **moves verbatim** into the shell crate. Only the thin
command wrappers change (attribute + `State` → `ShellState` field + params struct):

- `store_list_connectors` → `connectors.infos()` (`lib.rs:622`);
- `store_search_session` → `connectors.start_session(query)` (`lib.rs:631`);
- `store_search_more` → `session(&session).next_batch(count)` → `StoreSearchMore { results, exhausted }`
  (`lib.rs:647`);
- `store_import` → the Channel command (§5), `connector.download(&descriptor, &report)` +
  `StoreKind`-routed `control_request_with_params` import (`lib.rs:673`);
- `store_asset_parts` / `store_asset_gallery` → `connector.parts/gallery(&result)` (`lib.rs:740/755`);
- `store_import_part` → `download_part` + routed import (`lib.rs:771`);
- `connector_set_secret` / `connector_clear_secret` / `connector_secret_status` →
  `Credentials::global().set_secret/delete_secret/has_secret` (`lib.rs:824/832/840`);
- `connector_login` → `oauth_config(&id)` + `run_loopback_login(&config)` on a blocking worker
  (`lib.rs:848`; the current `tauri::async_runtime::spawn_blocking` becomes the shell executor's blocking
  spawn).

`ConnectorRuntime` also backs the `saffron-img://` thumbnail scheme (`lib.rs:1262`, via
`ConnectorRuntime::cache()`), which is **Phase 7** — this phase constructs the runtime and the store command
handlers; the scheme handler that reads `runtime.cache()` is deferred there. `storefront/types.ts` stays
hand-authored (it mirrors the `connectors` camelCase wire types, not the generated protocol) and its
`invoke`/`Channel` import swap is a straight edit in **Phase 5**; its command names and arg/return shapes are
part of the invoke contract this phase preserves.

### 7. Re-host the editor-local fs / OS / trace command set

Pure std bodies, moved verbatim, wrapped as invoke handlers over their params structs:

- `app_data_info` (`lib.rs:969`) — `AppDataInfo { app_data_dir, userdata_dir, env_project, scratch_project }`,
  no args;
- `list_recent_projects` (`lib.rs:980`) / `remember_recent_project` (`lib.rs:1003`) — `RecentProjects`
  read/prune/write, arg `{ project: RecentProject }`;
- `load_editor_settings` (`lib.rs:992`) / `save_editor_settings` (`lib.rs:998`) — `EditorSettings`
  delta map, arg `{ settings }`;
- `write_file` (`lib.rs:1018`) — `{ path: String, bytes: Vec<u8> }` (client-generated trace bytes);
- `serve_trace` (`lib.rs:1025`) — stash bytes on the loopback server, return the `http://127.0.0.1:9001`
  URL; the `start_trace_server` / `serve_trace_conn` / `TRACE_PATH` loopback with
  `Access-Control-Allow-Private-Network: true` (`lib.rs:148-225`) moves **verbatim** (it is already
  Chromium-PNA-correct, and CEF is Chromium — the header is exactly what CEF's private-network preflight
  requires);
- `open_external` (`lib.rs:1085`) / `open_in_vscode` (`lib.rs:1035`) / `open_project_folder` (`lib.rs:1067`)
  — the `std::process::Command` **`flatpak-spawn --host` chain** (`open_url_in_browser`, `lib.rs:1091`) moves
  verbatim; do **not** substitute the `opener` crate (the chain is toolbox-correct — the container has no
  `xdg-utils`, so the host handler is reached via `flatpak-spawn --host` first). Args
  `{ url }` / `{ path }` / `{ path }`.

## Scope

`editor/` — the new CEF shell crate (stood up in Phase 2): the `CefMessageRouter` browser-side handler + the
dispatch table, the per-command `#[derive(Deserialize)]` params structs, the offloading executor, the moved
`control_request_with_params` / `control_request` / `CONTROL_IO`, the `RenderProcessHandler` +
`on_context_created` `window.__saffron` bootstrap (invoke + listen + Channel), `ShellState`, and the 22
re-hosted command bodies (moved verbatim from `lib.rs`) with the `connectors/` module + `credentials`
keyring relocated verbatim. No frontend files change in this phase (the `src/shell/` shim and the import
swap are Phase 5); the frontend contract (`client.ts`'s `call`/`toControlError`/`isBusyLoading`,
`storefront/types.ts`'s `Channel`, the three `listen` sites) is the spec this phase satisfies.

Out of scope: lifecycle + presenter commands and their `engine-phase`/`viewport-error`/`mouse-button` emit
call sites (**Phase 6**); native file dialogs, OS drag-drop, and the `saffron-img://` scheme (**Phase 7**);
the frontend import swap + deleting `@tauri-apps/*` from `package.json` (**Phase 5**); deleting the Tauri
`#[tauri::command]` originals (**Phase 8** cutover).

## Depends on

`phase-3-cef-osr-ui-paint-and-input.md` — the React/Vite page must be loaded and painting in the CEF OSR
browser (the `CefMessageRouter` injects `window.cefQuery` into that page's frames, and
`on_context_created`/`on_process_message_received` run against that browser's render process) before the
bridge has anything to bind to. Phase 3 in turn depends on the Phase 2 host-owned Wayland toplevel + shell
skeleton and the Phase 1 CEF toolbox provisioning.

## Verification (against the repo gate — `just engine`, `clippy -D warnings`, editor `bun run build`)

- **Build/lint:** the shell crate builds and `cargo clippy -- -D warnings` is clean (per-crate `thiserror`
  errors for the bridge; `unsafe` only at the CEF FFI seam). `just prepare-for-commit` clean on the touched
  crates.
- **Control round-trip returns typed data:** with an engine host started manually on a known socket
  (`SAFFRON_CONTROL_SOCK`), call `window.__saffron.invoke("control", { cmd: "list-entities", params: {} })`
  from devtools (the same envelope `client.ts`'s `call` builds, `client.ts:179`) and confirm it resolves
  with the engine's result JSON. (The definitive UI-driven check — a `client.ts` wrapper resolving to its
  declared type — lands with the Phase 5 import swap.)
- **`ok:false` rejects with `{ message, code }`:** drive a command the engine refuses mid-load and confirm
  the invoke rejects with a value carrying **both** `.message` and `.code` (e.g. `code === "busy-loading"`),
  so `isBusyLoading(err)` (`client.ts:147`) would be `true` — i.e. the object shape survived, not a
  flattened string. A non-control command's failure (e.g. `open_external` with an unopenable URL) rejects
  with a bare string.
- **Channel emits multiple frames then resolves:** run `store_import` (or a stub connector whose `download`
  reports increasing fractions) and confirm the JS `Channel.onmessage` fires **multiple** times (whole-percent
  throttled) **before** the outer invoke resolves with the `ImportedAsset`.
- **Event push reaches a subscriber:** a `listen("engine-phase", cb)` subscriber receives an event emitted by
  a dev-only `ShellState::emit("engine-phase", "starting")` as `{ payload: "starting" }`; a second subscriber
  on the same name also fires; unlisten then re-fire delivers to neither, and calling the returned
  `UnlistenFn` twice does not throw (StrictMode).
- **Single-flight held:** fire several `invoke("control", …)` calls concurrently from JS against the real
  engine and confirm no `os error 11` (5 s read timeout) — the `CONTROL_IO` serialization keeps one socket
  round-trip outstanding.

## Risks

- **`BrowserSideCallback` off-thread completion.** The design offloads the socket round-trip to a worker and
  completes the `cefQuery` callback from there (§2). If the pinned cef-rs requires the callback to be
  completed on the CEF UI thread, marshal the completion back with `post_task(ThreadId::UI, …)` instead of
  calling it directly from the worker — verify the callback's thread affinity in
  `cef/src/wrapper/message_router.rs` before wiring, and keep the socket I/O off the UI thread either way.
- **Persistent-query lifecycle for `Channel`.** A persistent `cefQuery` must be explicitly completed or
  cancelled or it leaks a browser-side callback; the channel registry must tear down the progress query when
  `store_import` resolves *or* rejects *or* the page navigates. Tie the progress query's lifetime to the
  outer invoke and cancel on both completion and failure.
- **Rejection-shape regression is silent.** If a refactor ever routes a command error through `cefQuery`'s
  `onFailure(code, msg)` (which can only carry an int + string) instead of the `{ ok:false, error }`
  envelope, the control passthrough's machine-readable `code` is lost and every background poll lane starts
  toasting `busy-loading` errors (`isBusyLoading` silently returns `false`). The envelope-only completion for
  command results is load-bearing — `onFailure` is transport-only.
- **`CONTROL_IO` must stay a process `static`.** If the moved mutex is accidentally scoped per-handler or
  per-`ShellState` instead of `static`, concurrent invokes stop serializing and the engine's 5 s control
  read times out under edit-stream load. It guards `()` and recovers from poisoning (`lib.rs:499`) — keep
  both properties.
- **Injection timing.** `window.__saffron` must exist before the page's own modules evaluate (Phase 5's
  `WindowTitlebar` will read the window shim at module-eval time). Installing the bootstrap in
  `on_context_created` guarantees this; installing it later (e.g. after `DidFinishLoad`) would race the first
  render. This phase only needs `invoke`/`listen`/`Channel` present at load; the window controls are Phase 6,
  but the bootstrap object must construct synchronously regardless.
- **Toolbox host-service reachability (store/keyring).** `connector_login` opens the system browser via the
  `flatpak-spawn --host` chain and `Credentials` talks to the Secret Service over D-Bus; both already work
  under Tauri in the toolbox, but the CEF shell must inherit `$DBUS_SESSION_BUS_ADDRESS` and the host-spawn
  path so `keyring` (`connectors/credentials.rs`, with its `SAFFRON_NO_KEYRING` / reachability-probe fallback)
  and the OAuth loopback still reach the host session. This is a move-verbatim risk, not a redesign — verify
  the env crosses into the shell process the same way it does for Tauri today.
