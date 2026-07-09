# Native shell services: dialogs, OS drag-drop, custom thumbnail scheme

**Status:** COMPLETED — store-grid thumbnails confirmed loading on the real display (the user ran
`just run` and saw the storefront thumbnails render through `saffron-img://`). The scheme handler is
built in `editor/shell/src/scheme.rs` — a `wrap_scheme_handler_factory!` over the shared
`ResourceCache` that percent-decodes the `u=` param, resolves `cache.bytes()` on the shared tokio
runtime, and serves a `StreamResourceHandler` (200 + `Content-Type` + `Cache-Control: …, immutable`,
or 502 on miss). The scheme is registered standard+secure+CORS+fetch in `App::on_register_custom_schemes`
and the factory via `register_scheme_handler_factory` after `initialize`. The **connectors backend** is ported
wholesale into `editor/shell/src/connectors/` (all 10 files — the `StoreConnector` framework, the four
providers, `ResourceCache`, credentials, OAuth loopback) and compiles clean; the only edits were
`crate::app_data_dir()` → `crate::geometry::app_data_dir()` and routing the OAuth browser-open through
the shared `crate::os::open_url_in_browser`. The 11 `store_*`/`connector_*` commands are wired in
`editor/shell/src/store_commands.rs`, running the async connector methods on a shared tokio runtime
(`src/async_rt.rs`) from the IPC worker thread; download progress streams to the frontend `Channel` as
`channel:{id}` events, and the final import crosses to the host over the control plane. **Native
dialogs** (`dialog_open`/`dialog_save`) are `rfd` over the XDG desktop portal (`src/dialog.rs`, no GTK,
run on the worker thread so the CEF pump never stalls), replacing `tauri-plugin-dialog`. **OS drag-drop**
maps winit's `HoveredFile`/`HoveredFileCancelled`/`DroppedFile` to the frontend's Tauri-shaped
`drag-drop` payload (`over`/`leave`/`drop`, last-pointer position). Build + clippy + fmt + 5 unit tests
green; headless smoke clean.

**Remaining (display-gated, visual):** the `saffron-img://` **CEF scheme handler** — a
`SchemeHandlerFactory` + `ResourceHandler` (registered as a custom standard scheme in
`on_register_custom_schemes` + `register_scheme_handler_factory`) that serves `ResourceCache::bytes(url)`
so the store grid's `cachedImage(url)` thumbnails load. The cache's serving path (`bytes`, the
in-memory cache, `ConnectorRuntime::cache`) is ported and in place (behind `#[allow(dead_code)]`) so the
scheme is drop-in; it needs a real browser to validate image loading. Drag-drop position fidelity (winit
gives none; we use the last pointer position) also wants a live check.

---


## Goal

Provide the three native primitives that have no direct CEF equivalent, so every editor-local flow
reaches full behavioural parity with the Tauri shell: **native file dialogs** (project pick, model /
texture import, trace + export save, script open), **OS file drag-drop** into the Assets panel, and the
**`saffron-img://` custom thumbnail scheme** the storefront loads every tile through. Under Tauri these
came from `@tauri-apps/plugin-dialog`, `getCurrentWebview().onDragDropEvent`, and
`register_asynchronous_uri_scheme_protocol` (all deleted in the Phase 8 cutover). This phase re-provides
each on the CEF/winit shell: dialogs via the `rfd` crate over the GNOME/Mutter XDG desktop portal,
drag-drop by translating winit's `HoveredFile`/`DroppedFile` events into a `file-drop` shell event, and
the thumbnail scheme as a CEF `SchemeHandlerFactory` + `ResourceHandler` served from the shared connector
`ResourceCache`. It also confirms the one cross-cutting concern these share — session-D-Bus reachability
from inside the `saffron-build` toolbox — since both the portal FileChooser and the OS keyring ride the
host session bus.

The consuming frontend does **not** change in this phase. Phase 5 already authored the bridge shims
(`shell/dialog.ts` exposing `open`/`save`, `shell/webview.ts` exposing `onDragDropEvent`) and swapped the
`@tauri-apps/*` import sites over to them; `state/store.ts`'s `withNativeDialog` re-entry lock,
`panels/AssetsPanel.tsx`'s drop hit-test, and `storefront/cachedImage.ts`'s `saffron-img://` URL builder
are all shell-agnostic and stay verbatim. Phase 7 lands only the native side those shims invoke and
listen to.

## Build plan

### 1. Native file dialogs backing `shell/dialog.ts` (`rfd` over the XDG portal)

The six `open` and two `save` sites all funnel through the Phase-5 `shell/dialog.ts` `open`/`save`
functions, which reproduce the `@tauri-apps/plugin-dialog` signatures exactly. Their live call shapes,
grounded in the current frontend:

- **`open` (six sites):** `openProject` in `app/ProjectMenu.tsx` (`{ directory: true, multiple: false }`);
  the folder-browse and project-file picks in `app/ProjectStartupModal.tsx`
  (`{ directory: true, multiple: false }` and `{ multiple: false, filters: PROJECT_JSON_FILTER }`);
  `onAssign` in `components/ScriptSlots.tsx` (`{ title, defaultPath, filters: [{ name, extensions: ["lua"] }], multiple: false }`);
  the export-target folder pick in `app/ExportModal.tsx` (`{ directory: true, multiple: false }`); and
  `onImportClick` in `panels/AssetsPanel.tsx` (`{ multiple: true, filters: [Models & Images, Models, Images] }`).
- **`save` (two sites):** save-as in `app/ProjectMenu.tsx` (`{ defaultPath, filters: JSON_FILTER }`) and the
  trace / capture export in `components/CaptureControls.tsx` (`{ defaultPath, filters }`).

Register two async IPC commands on the Phase-4 bridge, named to match the `shell/dialog.ts` targets:

```rust
// shell/services/dialog.rs
async fn dialog_open(args: OpenArgs) -> Result<DialogSelection, ShellError>  // string | string[] | null
async fn dialog_save(args: SaveArgs) -> Result<Option<String>, ShellError>   // string | null

struct OpenArgs   { directory: bool, multiple: bool, filters: Vec<Filter>, default_path: Option<String>, title: Option<String> }
struct SaveArgs   { filters: Vec<Filter>, default_path: Option<String>, title: Option<String> }
struct Filter     { name: String, extensions: Vec<String> }
```

Back them with `rfd::AsyncFileDialog`, whose portal backend drives the GNOME/Mutter
`org.freedesktop.portal.FileChooser` over session D-Bus and works natively under Wayland. Depend on it
**without** the default GTK backend (GTK is deleted in Phase 8), so the file dialog cannot re-introduce a
`gtk3` link:

```toml
rfd = { version = "<pin>", default-features = false, features = ["xdg-portal", "tokio"] }
```

Mapping, preserving the current return contract byte-for-byte (`null` on cancel):

- `open` with `directory: true` → `AsyncFileDialog::pick_folder()` → `Option<PathBuf>`; `Some` → the path
  string, `None` → JSON `null`.
- `open` with `directory: false, multiple: false` → `pick_file()` → `string | null`.
- `open` with `directory: false, multiple: true` → `pick_files()` → `Option<Vec<PathBuf>>` → `string[] | null`
  (the shape `panels/AssetsPanel.tsx` `importMany` expects — it already normalises `string | string[]`).
- `save` → `save_file()` → `string | null`.
- Each `Filter { name, extensions }` maps to `AsyncFileDialog::add_filter(name, &extensions)`; `default_path`
  maps to `set_directory` (folder picks / save dir) and/or `set_file_name` (the save leaf, e.g. the
  `defaultPath: project?.path ?? "project.json"` in `ProjectMenu.tsx`); `title` maps to `set_title`.

Parent the portal request to the shell window: pass the Phase-2 winit toplevel's `RawWindowHandle`
(via `rfd::AsyncFileDialog::set_parent(&window)`) so the chooser is modal to the editor on Wayland — this
is exactly why the phase depends on Phase 2. `AsyncFileDialog` is non-blocking (the portal runs
out-of-process), so the CEF UI thread never stalls; the IPC bridge resolves the JS promise when the
future completes. The `withNativeDialog` lock in `state/store.ts` stays untouched — it is a frontend
re-entry guard, and its `try/finally` releases correctly as long as the native command resolves or
rejects cleanly (map any portal error to a rejected promise so the lock is never stranded). No filter
means "all files", matching the current no-filter `open` calls.

### 2. OS file drag-drop backing `shell/webview.ts` `onDragDropEvent`

`panels/AssetsPanel.tsx` subscribes to `getCurrentWebview().onDragDropEvent` and reacts to a payload of
shape `{ type: "enter" | "over" | "leave" | "drop", position: { x, y }, paths: string[] }` — importing
via `importMany(payload.paths)` only when `payload.type === "drop"` and `isInsidePanel(payload.position)`
is true. This is a **distinct channel** from the HTML5 `application/x-sa-asset` tile DnD (which stays
entirely inside the DOM and CEF handles internally); it carries desktop-originated file drops.

Under CEF OSR the shell owns the window, so an OS drag lands on the **winit toplevel**, not the page.
The shell translates winit's file-drop events into the `file-drop` shell event the Phase-4 event bus
pushes to JS (which `shell/webview.ts` re-emits as `onDragDropEvent`). Do not route these through CEF's
own `DragHandler` — forwarding the raw absolute paths directly is what preserves the "separate channel"
semantics AssetsPanel relies on.

Two winit realities shape the translation:

- **winit `DroppedFile(PathBuf)` carries no cursor position**, and winit emits **one event per file**
  (`HoveredFile`, `HoveredFileCancelled`, `DroppedFile`), whereas the payload wants a single event with a
  `paths` array and a position. So the shell keeps two pieces of state in the event loop: the **last
  pointer position** (updated from `WindowEvent::CursorMoved`, in physical device pixels) and a **pending
  drop-path buffer**. Map the winit stream to shell events as:
  - first `HoveredFile` of a gesture → `enter` (subsequent `HoveredFile` → `over`), position = last pointer
    position;
  - `HoveredFileCancelled` → `leave`;
  - `DroppedFile` → append the path to the pending buffer; **coalesce** the paths accumulated during the
    current event-loop poll and flush a **single** `drop` event (with the buffered `paths` and the last
    pointer position) on `ApplicationHandler::about_to_wait`, then clear the buffer.
- **Coordinate space must stay physical device pixels.** `isInsidePanel` in `panels/AssetsPanel.tsx`
  documents that the drop `position` is in physical pixels and divides by `window.devicePixelRatio` to
  compare against a CSS-pixel `getBoundingClientRect`. winit's `CursorMoved` delivers `PhysicalPosition`,
  which is precisely that space, so forwarding it verbatim keeps `isInsidePanel` — and AssetsPanel's whole
  hit-test — unchanged. Because CEF renders at the same `device_scale_factor` the shell feeds it (Phase 3),
  the page's `devicePixelRatio` matches, so the division still lands.

This slots into the Phase-2 winit event loop (the shell's window module); no new event source is created,
only new arms on the existing `WindowEvent` match plus the `about_to_wait` flush.

### 3. `saffron-img://` as a CEF scheme handler over `ResourceCache`

The storefront loads every thumbnail and gallery tile through `storefront/cachedImage.ts`, which rewrites
a remote URL to `saffron-img://fetch/?u=<percent-encoded-url>` (passing `data:`/`blob:`/already-wrapped
URLs through). The Tauri shell served this via `register_asynchronous_uri_scheme_protocol("saffron-img", …)`,
splitting the URI on `u=`, percent-decoding, and streaming `ConnectorRuntime::cache().bytes(url).await` —
`(bytes, content_type)` on hit, `502` on miss (`img_scheme_error`). Re-provide it in CEF as a
registered custom scheme plus a factory:

- **Register the scheme** in `App::on_register_custom_schemes` via the `SchemeRegistrar`, with options
  **standard + secure + CORS-enabled + fetch-enabled**. Chromium gates `<img src>` and `fetch()` on scheme
  capabilities far more strictly than WebKitGTK did; registering `saffron-img` as a standard secure scheme
  (not an opaque one) is what lets `storefront/StoreResultsGrid.tsx` `<img>` tags and any `fetch` succeed,
  and CORS-enabled avoids opaque-response breakage on cross-origin image loads.
- **Register a `SchemeHandlerFactory`** for `"saffron-img"` after CEF init (in the browser process, once
  the context is initialised), holding an `Arc<ResourceCache>` obtained from the shell's
  `ConnectorRuntime` (the Phase-4 state container, the same runtime the store commands use). The factory
  returns a `ResourceHandler` per request.
- **The `ResourceHandler`** ports the current body: take the request URI, `split_once("u=")`, `percent_decode`
  the tail, then resolve `cache.bytes(&url).await`. CEF's `ResourceHandler` is pull-based
  (`open`/`get_response_headers`/`read`) rather than Tauri's push `responder.respond(...)`, so drive the
  async cache read on the tokio runtime and signal completion through the handler's `Callback::cont()`;
  then report `200` with `Content-Type` (the cache's returned content type) and
  `Cache-Control: public, max-age=31536000, immutable` and serve the buffered bytes, or `502` with an empty
  body on any error (the broken-image glyph, exactly as today).
- **Port `percent_decode` and `img_scheme_error` verbatim.** `percent_decode` is pure Rust and moves with
  no change. `img_scheme_error`'s logic (status `502`, empty body) moves; only its return type changes from
  `tauri::http::Response<Vec<u8>>` to the CEF handler's response representation.

**Caching decision to verify.** WebKitGTK did **not** cache custom-scheme responses, so the
`ResourceCache` in-RAM LRU (`MemCache` in `connectors/cache.rs`, evicted at `MEM_CACHE_BUDGET_BYTES`) was
what kept a screenful of repeat requests off disk. Chromium honours `Cache-Control` on a standard scheme
and will cache aggressively — a repeat `saffron-img://` request for the same URL may be served from
Chromium's own HTTP cache and never reach the `ResourceHandler` at all. That is the desired outcome for
content-addressed provider thumbnails (fewer handler hits, no stampede) and does not conflict with the LRU
— the LRU simply serves the requests that still reach the handler (cold cache, eviction, cross-session).
Confirm during verification that Chromium (a) honours the `immutable` directive for the custom scheme and
(b) still calls the handler for uncached URLs so the disk-backed blob + LRU path stays exercised; if
Chromium's cache proves too sticky for a force-refresh case, drop `immutable` (keep `max-age`) rather than
re-adding a per-request cache-buster.

### 4. Portal + Secret Service reachability from inside the toolbox

The dialogs (§1) and the OS keyring (`connectors/credentials.rs`, unchanged, ported verbatim in an
earlier phase) both reach host services over the session D-Bus: `rfd`'s portal backend needs
`org.freedesktop.portal.Desktop`, and `keyring` v3 needs the Secret Service — both addressed by
`$DBUS_SESSION_BUS_ADDRESS`. The shell binary is cargo-built and launched inside `saffron-build`, and the
existing `flatpak-spawn --host` chain in `open_external` / `open_in_vscode` is a standing hint that
host-service reachability across the toolbox boundary is delicate.

Confirm (not build — this is a verification/provisioning item):

- `$DBUS_SESSION_BUS_ADDRESS` is set inside the toolbox and points at a live socket (toolbox normally
  bind-mounts `/run/user/$UID`, so the host session bus — and thus the Mutter portal and Secret Service —
  is reachable). The keyring already degrades gracefully if not: `keyring_reachable()` in
  `connectors/credentials.rs` probes an `Entry::get_password` and falls back to the in-memory backend on
  `PlatformFailure`/`NoStorageAccess`, gated further by `SAFFRON_NO_KEYRING`. That degrade path is
  untouched and remains the safety net for the keyring half.
- The `rfd` portal FileChooser actually appears from a shell launched inside the toolbox. If the bus does
  not cross, that is a toolbox-provisioning fix (forward the socket / export the address in the `just`
  recipe), **not** a code change, and it must be resolved here because dialogs are load-bearing for project
  open/save and import. Do not fall back to `zenity` (rfd's non-portal fallback) — the toolbox has no
  `zenity`, and the correct target is the GNOME portal that the machine already runs.

## Scope

- **Shell crate (the CEF/winit crate replacing `editor/src-tauri`):**
  - `services/dialog.rs` — the `dialog_open` / `dialog_save` IPC handlers over `rfd::AsyncFileDialog`,
    parented to the winit toplevel.
  - `services/scheme.rs` — the `saffron-img` scheme registration (`on_register_custom_schemes`), the
    `SchemeHandlerFactory`, the `ResourceHandler` over `Arc<ResourceCache>`, and the ported
    `percent_decode` / `img_scheme_error`.
  - The window module (Phase 2) gains the drag-drop arms: `WindowEvent::{HoveredFile, HoveredFileCancelled,
    DroppedFile, CursorMoved}` handling, the last-pointer-position + pending-path state, and the
    `about_to_wait` coalesced `drop` flush emitting the `file-drop` event over the Phase-4 bus.
  - `Cargo.toml`: add `rfd` (`default-features = false`, `features = ["xdg-portal", "tokio"]`); the scheme
    handler uses the `cef` crate's scheme APIs already present from Phases 3–4. No new GTK/GDK dependency.
- **Frontend:** no change. `shell/dialog.ts`, `shell/webview.ts` (Phase 5), `panels/AssetsPanel.tsx`,
  `state/store.ts::withNativeDialog`, and `storefront/cachedImage.ts` are all consumers that already exist
  and stay verbatim.
- **No** protocol, `.smat`/mesh-format, engine, or control-plane change. The `control` passthrough and the
  generated protocol types are untouched.

## Depends on

- **`phase-2-host-wayland-toplevel-shell-skeleton.md`** — supplies the winit toplevel that is (a) the
  source of the `HoveredFile`/`DroppedFile`/`CursorMoved` events §2 translates, and (b) the
  `RawWindowHandle` parent §1 hands `rfd` for a modal portal chooser.
- **`phase-4-ipc-bridge-and-command-surface.md`** — supplies the IPC bridge the `dialog_open`/`dialog_save`
  commands register on, the event bus §2 pushes `file-drop` through, the shell state container holding the
  `ConnectorRuntime`/`ResourceCache` §3 reads, and the CEF `App` whose `on_register_custom_schemes` §3
  extends.

Independent of Phases 5, 6, 8, 9: it adds native backing for surfaces Phase 5 already wired, touches no
presenter/lifecycle code (Phase 6), and lands before the Phase-8 delete removes the Tauri fallback.

## Verification

Concrete against the repo gate (`just engine` build + shaders, `cargo clippy --workspace -- -D warnings`,
`bun run build`/oxlint for the frontend, and a real run):

- **Build + lint clean.** `cargo build --workspace` + `cargo clippy --workspace -- -D warnings` inside the
  toolbox with `rfd` (portal, no GTK) linked and the scheme handler compiling against the `cef` crate;
  `bun run lint` (oxlint) clean — the frontend is unchanged, so this only confirms no stray import broke.
- **Dialogs open the real portal and return correct paths.** Drive each flow and confirm the GNOME/Mutter
  chooser appears and the returned path is correct, `null` on cancel: project pick (`ProjectMenu` /
  `ProjectStartupModal`), model + texture import (`AssetsPanel` multi-select), trace + capture export save
  (`CaptureControls`), save-as project (`ProjectMenu`), and script open (`ScriptSlots` `.lua` filter).
  Confirm `withNativeDialog` still greys the spawning control for the dialog's lifetime and re-enables it
  after cancel (the lock is released on the rejected/None path).
- **OS file-drop imports with correct coordinates.** Drag desktop files onto the Assets panel: the
  `enter → over → leave/drop` sequence fires, `isInsidePanel` gates the import to the panel's rect
  (dropping over the viewport does **not** import), a multi-file drop arrives as one `drop` with all paths,
  and `importMany` runs. Verify a drop just outside the panel is ignored and the drop coordinate tracks the
  cursor (the last-pointer-position workaround for winit's position-less `DroppedFile`).
- **Thumbnails load via `saffron-img://` from the cache.** Open the storefront; gallery/result tiles resolve
  through `saffron-img://fetch/?u=…` (200 + `Content-Type` + immutable `Cache-Control`), a screenful does
  not stampede the provider, and a cache miss shows the broken-image glyph (`502`). Confirm the caching
  interaction: repeat views are served without re-hitting the provider, and uncached URLs still reach the
  `ResourceHandler` (LRU/disk path exercised).
- **Portal + keyring reachable from the toolbox.** The dialog run above proves the portal crosses the
  boundary; separately confirm a connector credential set/get round-trips through the Secret Service (or
  cleanly degrades to in-memory with `SAFFRON_NO_KEYRING`), so `$DBUS_SESSION_BUS_ADDRESS` reachability is
  demonstrated for both consumers.
- **Milestone gate:** `just engine` then `just prepare-for-commit` (format + lint) clean.

## Risks

- **Portal / Secret Service D-Bus does not cross the toolbox boundary.** `rfd`'s portal backend and
  `keyring` both need `$DBUS_SESSION_BUS_ADDRESS`. Mitigation: §4 verifies it up front; the keyring already
  degrades to in-memory, but dialogs are load-bearing, so if the bus is absent the fix is toolbox
  provisioning (forward the socket in the `just` recipe), resolved in this phase rather than deferred.
- **winit Wayland file-drop fidelity.** winit's Wayland backend delivers file drag-drop over
  `wl_data_device`, but `DroppedFile` carries no position and fires one-per-file. Mitigation: the
  last-pointer-position + `about_to_wait` coalescing in §2 is exactly the workaround; verify the physical
  `CursorMoved` position is fresh at drop time on this compositor (Wayland may not send `CursorMoved` during
  a drag over a foreign surface — if the position is stale, fall back to the pointer position from the drag
  enter).
- **Chromium caches the custom scheme differently than WebKitGTK.** Chromium may serve `saffron-img://`
  repeats from its own HTTP cache and bypass both the handler and the in-RAM LRU. Generally desirable, but
  verify it does not defeat a force-refresh; §3 documents dropping `immutable` (keeping `max-age`) as the
  lever if the cache is too sticky.
- **cef-rs binding gaps for the scheme handler.** The binding is pre-1.0; the `SchemeHandlerFactory` /
  `ResourceHandler` / `SchemeRegistrar` surface must be confirmed present and ergonomic before depending on
  it. Mitigation: verify against the pinned `cef` crate version early; if the pull-based `ResourceHandler`
  cannot express the async cache read cleanly, buffer the full body before `get_response_headers` (the
  thumbnails are small) rather than streaming.
- **rfd default features re-linking GTK.** Enabling `rfd`'s default `gtk3` backend would drag GTK back into
  the tree the Phase-8 cutover exists to delete. Mitigation: pin `default-features = false` + `xdg-portal`
  as in §1, and confirm the dependency graph carries no `gtk`/`gdk` after the change.
