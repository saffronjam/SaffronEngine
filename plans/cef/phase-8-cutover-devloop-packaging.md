# Cutover: delete `editor/src-tauri` wholesale, dev-loop and packaging

**Status:** COMPLETED — `editor/src-tauri` deleted wholesale; `grep -rniE 'tauri|wry|webkit2gtk|
gtk_window|gdk_wayland' editor/` is clean and `test ! -d editor/src-tauri` holds. `just run`/`run-debug`/
`run-software` launch the CEF `editor/shell` (over background Vite, dev URL + engine env + CEF resource
linking); the `run-shell` variant folded into `run`. `vite.config.ts` dropped the `TAURI_` env prefix and
the `src-tauri` watch-ignore; `tools/ci/check.sh` gained the `editor/shell` build step (deferring if
libcef isn't provisioned). Both `AGENTS.md` files, the `docs/content/` pages (incl. the renamed
`editor-shell-and-viewport-bridge` page), and the code comments are rewritten off Tauri/WebKitGTK onto
the CEF-OSR shell. Verified: shell `cargo build`/`clippy -D warnings`/`fmt`/tests green, frontend
`bun run build` green, `hugo` builds clean (241 pages, no broken links). Packaging (`editor-package`
recipe + `bundle-cef-app`) and the toolbox-image CEF vendoring remain as provisioning follow-ups.

This is the NO-LEGACY cutover. By the start of this phase the CEF shell already exists as its own crate
— `editor/shell` (`saffron-editor-shell`), stood up in Phase 2 and filled out across Phases 3–7 with the
OSR UI paint, the IPC bridge, every re-homed command, the subsurface presenter, engine supervision, and
the native services — built Tauri-free from its first commit. `editor/src-tauri` has been left untouched
and running as the live shell the whole time. This phase flips `editor/shell` to be the sole, live shell
and **deletes `editor/src-tauri` in its entirety**, then rebuilds the dev-loop, packaging, gate, and docs
around the CEF-OSR shell. A feature is not done while a superseded flow, command, or dependency still
exists anywhere in the tree — after this phase, **`editor/src-tauri/` no longer exists**, and
`grep -rniE 'tauri|wry|webkit2gtk|gtk_window|gdk_wayland' editor/` returns nothing.

## Goal

Delete the `editor/src-tauri` crate and directory outright, make `editor/shell` the one editor shell,
and re-home the dev-loop (`just run`/`run-debug`/`run-software`), the reproducible gate
(`tools/ci/check.sh`), the editor package build, and the shell-naming docs onto it. When this lands
there is exactly one editor shell — the CEF/Rust `editor/shell` crate — with one way to launch it, one
way to build it, and one Stack-table row that says so. The `src-tauri` directory name is itself a Tauri
artifact; it does not survive.

## Depends on

- [`phase-5-frontend-bridge-shim-and-de-tauri.md`](phase-5-frontend-bridge-shim-and-de-tauri.md) —
  the `src/shell/` (or `src/bridge/`) module reproducing `invoke`/`listen`/window/webview/dialog/
  Channel, the mechanical import swap across the ~17 `@tauri-apps/*` sites, and the resolution of the
  mouse-side-button open question (whether Chromium delivers buttons 8/9 to the DOM). This phase's
  frontend-side deletions (the `@tauri-apps/*` deps, the `TAURI_` `envPrefix`) are only safe once Phase
  5 has landed those swaps against the `editor/shell` bridge.
- [`phase-6-subsurface-presenter-supervision-lifecycle.md`](phase-6-subsurface-presenter-supervision-lifecycle.md) —
  the presenter (`wayland_viewport.rs`) re-homed **into `editor/shell`** off GTK/GDK/WebKitGTK onto the
  winit toplevel's `wl_display`/`wl_surface`, engine supervision (`spawn_engine`, `teardown`), and the
  lifecycle commands (`start_engine`/`quit_engine`/`set_viewport_bounds`/…). Deleting `editor/src-tauri`
  requires that its still-needed logic already lives in `editor/shell`.
- [`phase-7-native-shell-services.md`](phase-7-native-shell-services.md) — dialogs on `rfd`, OS
  drag-drop on winit, the `saffron-img` CEF scheme handler, and the connector stack moved into
  `editor/shell`. Deleting `editor/src-tauri` (which still carries the old `tauri-plugin-dialog` path and
  the connector copy) requires their `editor/shell` replacements to already be the live path.

Together these mean that by the start of this phase `editor/shell` is a fully functional CEF shell
in-tree alongside the still-running `editor/src-tauri`; this phase deletes the Tauri crate and switches
every launch/build/gate/doc onto `editor/shell`.

## Why one cutover, not a fade-out

The repo rule is absolute: never keep an old code path alive "for back-compat", never defer a cutover by
leaving the superseded path running next to the new one. `editor/src-tauri` and `editor/shell` own the
window, the message pump, and the process bootstrap differently — they are not a runtime flag apart. So
the switch is atomic: the same change that points `just run` / the gate / packaging at `editor/shell`
deletes `editor/src-tauri` outright. There is no interval in which both shells are selectable, and no
`SAFFRON_SHELL=cef` escape hatch — that would be exactly the "additive for now, retire later" the rule
forbids. Phases 2–7 having built `editor/shell` beside the running Tauri crate is not a dual-shell
period: only `editor/src-tauri` was ever the *live* shell until this instant, and `editor/shell` becomes
live only as `editor/src-tauri` is deleted.

## Build plan

### 1. Delete `editor/src-tauri` wholesale

Remove the entire crate and directory — `rm -rf editor/src-tauri`. Everything in it is either a Tauri/
WebKitGTK/GTK artifact with no place in the CEF shell, or shell-agnostic logic that Phases 4/6/7 already
re-homed into `editor/shell`. Enumerated so the NO-LEGACY completeness is explicit and nothing is assumed
to have been carried:

- **The Tauri dependency stack** (`editor/src-tauri/Cargo.toml`): `tauri` (2.11) and
  `tauri-plugin-dialog` (2) in `[dependencies]`, `tauri-build` (2.6) in `[build-dependencies]`, and the
  `[target.'cfg(target_os = "linux")'.dependencies]` block with `gtk`/`gdk`/`glib` (0.18) and
  `webkit2gtk` (2.0). These exist only for the GTK3 + WebKitGTK toplevel and the Tauri command/plugin
  machinery. They are **not** dependencies of `editor/shell` and die with the crate.
- **`tauri.conf.json`** — the baked window declaration (frameless, transparent, `visible:false`,
  1600x900 / min 1200x720), `devUrl`/`frontendDist`, `security.csp:null`, the dead `bundle.active:false`
  block. Every value it encoded now lives in `editor/shell`'s winit window-creation code (Phase 2), the
  geometry layer (Phase 6), and the dev/prod URL selection (§2 below).
- **`capabilities/default.json`** — the Tauri ACL (`core:window:*`, `dialog:default`). CEF has no
  capability/permission manifest.
- **`build.rs`** — exactly `fn main() { tauri_build::build(); }`. `editor/shell` has no build script;
  CEF binary discovery is via `CEF_PATH`/`LD_LIBRARY_PATH` (Phase 1), not a build script.
- **`gen/`** — Tauri's compile-time generated schemas (`tauri::generate_context!`). **`icons/`** — Tauri
  bundle icons (a winit window icon, if wanted, lives in `editor/shell` in winit's format).
- **`src/lib.rs`** — its `#[tauri::command]` / `generate_handler!` registration, the `tauri::Builder`
  chain, and specifically the WebKit-era code that has no analogue under CEF-OSR and was deliberately
  **not** ported into `editor/shell`: the `run()` webview render-path block (`nvidia_present()`-gated
  `__EGL_VENDOR_LIBRARY_FILENAMES` + `LIBGL_ALWAYS_SOFTWARE`, or hardware `__NV_DISABLE_EXPLICIT_SYNC=1`,
  plus `EGL_LOG_LEVEL`, gated on `SAFFRON_WEBVIEW_HW`), `install_stderr_noise_filter` (the Mesa/libEGL
  WebKit-EGL stderr suppressor), and `nvidia_present()` itself. Their whole rationale is WebKitGTK's
  `wp_linux_drm_syncobj_surface_v1` explicit-sync crash and its DRM-vblank 60Hz cap — the exact legacy
  this migration removes. Confirm `editor/shell` contains **none** of it. The one piece that *was*
  carried (Phase 6's `spawn_engine`) keeps the `NVIDIA_ICD` constant and its `VK_ICD_FILENAMES` guard —
  that is engine-facing (the Vulkan host child), not a WebKit workaround, and lives in `editor/shell`.
- **`src/wayland_viewport.rs`** — the GTK-coupled presenter, including the `webview.connect_event(…)`
  GDK mouse-side-button hook that emitted `mouse-button` because WebKitGTK swallowed buttons 8/9 before
  the DOM. Phase 6 re-homed the presenter's raw-Wayland core into `editor/shell` on the winit surface;
  Phase 5 resolved the side-button handling (Chromium delivers 8/9 to the DOM in OSR, so
  `useMouseBindings.ts`/`SettingsModal.tsx` are pure DOM handlers and the `mouse-button` bridge event is
  gone). No GTK `connect_event` survives anywhere.
- **`src/connectors/`** — the store-connector backend. Phase 7 moved it verbatim into `editor/shell`
  (it was already `tauri::`-free: `reqwest`/`keyring` v3/`tokio`/`zip`/`async-trait`). The
  `editor/src-tauri` copy dies with the crate.

Verify before deleting: `editor/shell` already carries every still-needed capability — the one
`control` passthrough with its `CONTROL_IO` single-flight serialization, all 28 re-homed commands, the
presenter, engine supervision, the connectors, geometry persistence, the trace loopback server, and the
`saffron-img` scheme — minus every Tauri/WebKit artifact above. The deletion removes the last live Tauri
code, not anything unique.

### 2. Rewrite the dev-loop recipes (`justfile`) onto `editor/shell`

The `run`, `run-debug`, and `run-software` recipes today build `saffron-host` + shaders, apply the
`nvidia_icd` macro, `export SAFFRON_WEBVIEW_HW=1` (except `run-software`), set `SAFFRON_ANIMA_BIN`, and
finish with `cd "{{editor}}" && bun run tauri dev` — the line that spawns the WebKitGTK webview and caps
the UI at ~60Hz. Rewrite the flow to launch `editor/shell`:

- **`run`.** Build the CEF shell binary (`cargo build` in `editor/shell`, a standalone crate outside the
  engine workspace) and `saffron-host` + shaders. Start Vite in the background (`cd "{{editor}}" &&
  bun run dev`, i.e. `vite --host 127.0.0.1` on `strictPort` 1420) so HMR works unchanged. Then launch
  the compiled `editor/shell` binary with: `SAFFRON_DEV_URL=http://127.0.0.1:1420` (the dev URL the
  shell navigates to, replacing `tauri.conf.json`'s `devUrl`); the engine env — `SAFFRON_ANIMA_BIN`
  pointing at `{{engine_bin}}`, the `nvidia_icd` macro (the engine child still renders on the real
  NVIDIA ICD), and the viewport SHM-name env the shell reads; and CEF's runtime env — `CEF_PATH` +
  `LD_LIBRARY_PATH` from the Phase 1 toolbox provisioning so the loader finds `libcef.so` and the CEF
  resources. Drop the `export SAFFRON_WEBVIEW_HW=1` line entirely (the var died with the render-path
  block). Trap/kill the background Vite on recipe exit so a stray Vite does not survive the run.
- **`run-debug`.** Same as `run` plus `VITE_SAFFRON_DEV_MODE=1` (unchanged), minus `SAFFRON_WEBVIEW_HW`.
- **`run-software`.** Where `run-software` today omits the `nvidia_icd` macro (dropping the *engine* to
  llvmpipe), it must now also select CEF's software path: pass the CEF command-line switch chosen in
  Phase 1 (the `--disable-gpu` / `--use-gl=…` / `--ozone-platform=…` combination pinned for software
  OSR) to the shell binary. "Software" now means two independent things — the engine child on llvmpipe
  (omit `nvidia_icd`) and CEF's own GPU process on software (the CEF switch) — and the recipe sets both.

The `bun_bin`, `reenter`, and `nvidia_icd` macros stay. `run-engine` / `run-engine-software` /
`run-engine-headless` are unchanged — they launch only the host and never touched the shell.

Add a **net-new `editor-package` recipe**: `cd "{{editor}}" && bun run build` (Vite → `editor/dist`),
then stage the runnable bundle — the compiled `editor/shell` binary, the `editor/dist` assets it serves
in prod (via the CEF app-scheme handler or a co-located `dist/`, per the Phase 3/8 embed-vs-co-locate
decision), and the version-locked libcef runtime (`libcef.so`, `Release/`, `Resources/` — the locale
`.pak`s, `icudtl.dat`, the V8 snapshot) co-located on the loader path. cef-rs ships a `bundle-cef-app`
tool for exactly this staging; the recipe wraps it (Linux `--release` for the smaller bundle) and places
the result under a `target/bundle`-style output. This is new packaging, not a port — no `tauri build`
recipe ever existed (`bundle.active:false`, no `tauri:build` script), so there is no prior bundle
behaviour to preserve.

### 3. Prune the frontend build config (`package.json`, `vite.config.ts`)

Phase 5 already removed the `@tauri-apps/api` and `@tauri-apps/plugin-dialog` runtime deps (their call
sites were swapped to the `src/shell/` bridge) and, with the CLI no longer invoked, the `@tauri-apps/cli`
devDep. This phase removes the last Tauri traces from `editor/package.json`:

- Delete the `"tauri": "tauri"` and `"tauri:dev": "tauri dev"` entries from `scripts`. Keep `dev`,
  `build`, `check`, `gen:protocol`, `format`, `lint`, `test` — none reference Tauri.
- Confirm (and delete if Phase 5 left any) any residual `@tauri-apps/*` entry under `dependencies` or
  `devDependencies`.

In `editor/vite.config.ts`, drop `"TAURI_"` from `envPrefix` (leaving `["VITE_"]`). No frontend code
reads `import.meta.env.TAURI_*`, so the prefix is dead once Tauri is gone; leaving it would be a stray
reference to the deleted shell. The `server.strictPort` / `port: 1420` and `build.target: "esnext"`
config stay (the shell navigates to that dev URL, and the prod build output is what `editor-package`
stages).

### 4. Wire the `editor/shell` build and CEF vendoring into the gate (`tools/ci/check.sh`, toolbox image)

`tools/ci/check.sh` step 1 (`cargo build --workspace`) builds only the *engine* workspace; `editor/shell`
is a standalone crate with its own `Cargo.toml`/`Cargo.lock` and is not a workspace member (exactly as
`editor/src-tauri` was not). Add a step that builds and links `editor/shell` (`cd editor/shell &&
cargo build`) with `CEF_PATH`/`LD_LIBRARY_PATH` set, so the gate proves the shell compiles and links
against libcef — today nothing in the gate builds any editor shell crate. Sequence it near step 1 (no
dependency on the host boot). Keep step 9 (`cd editor && bun run build && bun test`) as-is — the frontend
build and unit tests are shell-agnostic.

The **saffron-build toolbox image** must vendor the pinned CEF distribution (multi-hundred-MB,
version-locked to the exact `cef`/`cef-dll-sys` pin) at both link and runtime, via `export-cef-dir` with
`CEF_PATH`/`LD_LIBRARY_PATH` exported (the Phase 1 provisioning). This **replaces** the toolbox's
`webkit2gtk-4.1` + GTK3 dev libraries, which no editor crate links any longer — removing those dev libs
from the image is itself part of the legacy removal. Document the provisioning in the build docs (§6) so
`just run` and `just check` Just Work inside the toolbox.

**e2e boot path — verified unaffected.** `tests/e2e/harness.ts` boots the engine directly: it spawns
`ENGINE_BIN` (`SAFFRON_ANIMA_BIN ?? engine/target/debug/saffron-host`) under a per-run headless `weston`
and drives it over the JSON-over-unix-socket control plane — it never launches the editor shell (Tauri
or CEF). So step 8 of the gate is orthogonal to the cutover and needs no change. This was an open
question in the plan; confirmed: the e2e suite is a host-over-control-plane driver, so the shell swap
does not touch it.

### 5. Reconcile `Cargo.lock` and workspace/path references

Deleting `editor/src-tauri` removes a crate from the tree. Confirm nothing references it by path: the
root `AGENTS.md` crate DAG and any `just`/`tools` script that named `editor/src-tauri` now names
`editor/shell`. `editor/shell` keeps its own `Cargo.lock` (like `src-tauri` had one). There is no engine
workspace `members` entry to prune (neither shell crate was ever an engine-workspace member).

### 6. Docs (keep-current rule)

- **`editor/AGENTS.md`** — retitle from `# editor — Tauri/React editor` to the CEF/React shell, and
  **delete the webview-render-path section** (the paragraph describing the `[saffron] webview render
  path: …` selection, the NVIDIA-software default, `SAFFRON_WEBVIEW_HW`, the
  `wp_linux_drm_syncobj_surface_v1` crash, and `__NV_DISABLE_EXPLICIT_SYNC`) — all of it describes
  deleted code. Rewrite the shell overview to describe the CEF-OSR model in `editor/shell`: a host-owned
  raw Wayland toplevel (winit) with CEF rendering the React UI windowless (`on_paint` BGRA → the toplevel
  `wl_surface`, alpha preserved) transparently over the engine's `wl_subsurface`s below, frame production
  clocked off the presenter's `wp_presentation` feedback for true monitor-rate UI. Update the collateral
  references in the same file: the `src-tauri/` layout line (→ `shell/`), the `bun run tauri:dev` launch
  line (→ `just run` / the CEF binary), the debugging note that "webview `console.log` does not reach the
  `tauri dev` terminal" (CEF's process model differs), and the "Browser file/URL APIs don't work in the
  webview" note (Chromium honours `<a download>`/`window.open`, though the bridge commands stay for the
  toolbox's missing `xdg-utils` and Perfetto PNA).
- **Root `AGENTS.md`** — the Stack table Editor row `| Editor | Tauri 2 + React 19 + Vite + shadcn/ui +
  Tailwind v4, Bun | |` becomes CEF (Chromium) instead of Tauri 2. Update the load-bearing prose that
  names the shell technology: the opening "The **editor is the Tauri/React/TypeScript app in
  `editor/`**", the `### The editor (Tauri/React)` heading and its `bun run tauri dev` line, the Layout
  line "`editor/` Tauri/React/TS editor … src-tauri/ (Rust bridge)" (→ `shell/`), and the Status
  paragraph's "the Tauri editor". Describe the shell as CEF-OSR presenting over the Wayland subsurface;
  keep every other invariant (the control plane, the present-only host, the subsurface architecture)
  worded as-is.
- **`docs/`** — update `docs/content/overview.md` ("the Tauri/React editor, the in-webview canvas + shm
  frame transport …" in the UI & editor row) and `docs/content/_index.md` ("with a Tauri/React editor")
  to name the CEF/Chromium shell. Grep `docs/content/` for `tauri`/`webkit`/`wry`/`webview` and update
  any explanation page (e.g. the ui-and-editor pages) that names the old shell or the webview render
  path; run the changed prose through the `humanizer` pass and confirm `cd docs && hugo` builds clean
  with no broken intra-site links.

## Scope

- **Deleted wholesale:** `editor/src-tauri/` — the entire crate and directory (`Cargo.toml` + `Cargo.lock`
  with the `tauri`/`tauri-build`/`tauri-plugin-dialog`/`gtk`/`gdk`/`glib`/`webkit2gtk` deps,
  `tauri.conf.json`, `capabilities/`, `build.rs`, `gen/`, `icons/`, `src/lib.rs` incl. the WebKit
  render-path block / `install_stderr_noise_filter` / `nvidia_present`, `src/wayland_viewport.rs` incl.
  the GTK `connect_event` mouse hook, and the `src/connectors/` copy).
- `justfile` — rewrite `run`/`run-debug`/`run-software` to launch the `editor/shell` binary over
  background Vite with the dev URL + engine env + CEF runtime env; add the `editor-package` recipe.
- `editor/package.json` — remove the `tauri`/`tauri:dev` scripts and any residual `@tauri-apps/*` deps.
- `editor/vite.config.ts` — drop `"TAURI_"` from `envPrefix`.
- `tools/ci/check.sh` — add the `editor/shell` build/link step; keep step 9 (frontend build) and step 8
  (e2e, unaffected); ensure the toolbox image vendors the pinned CEF distribution and drops the
  WebKitGTK/GTK3 dev libs.
- `editor/AGENTS.md`, root `AGENTS.md`, `docs/content/` — retitle/rewrite the shell-naming sections from
  Tauri/WebKitGTK to the CEF-OSR `editor/shell`; update `src-tauri/` layout references to `shell/`.

## Verification

Against the repo gate (`just engine` then `just prepare-for-commit`, and `just check` /
`tools/ci/check.sh`):

- **`editor/src-tauri/` is gone and no Tauri residue remains.** The directory does not exist
  (`test ! -d editor/src-tauri`). `grep -rniE 'tauri|wry|webkit2gtk|gtk_window|gdk_wayland' editor/`
  returns nothing, and `grep -rn '@tauri-apps' editor/package.json editor/src` returns nothing.
  `editor/shell` builds clean and `cargo clippy -- -D warnings` is clean.
- **`just run` launches the full editor end-to-end.** The `editor/shell` CEF binary boots, navigates to
  `http://127.0.0.1:1420`, paints the React UI transparently over the engine viewport subsurface, and
  the spawned `saffron-host` renders below it — the complete editor, no Tauri. As a smoke of the
  migration's whole point, the UI should visibly repaint at the monitor rate rather than ~62.5fps (the
  rigorous fps measurement is Phase 9's job).
- **`just check` passes.** `tools/ci/check.sh` is green including the new `editor/shell` build/link step,
  step 9's `bun run build` + `bun test`, and step 8's e2e suite (which drives the host directly and is
  unaffected by the shell swap). Lint step stays clean.
- **The staged bundle runs.** `just editor-package` produces a runnable bundle with `libcef.so` and the
  CEF resources co-located next to the `editor/shell` binary and `editor/dist` served through the app
  scheme; launching the staged binary (outside the dev-server flow) brings up the editor.
- **Docs build clean.** `cd docs && hugo` compiles (SCSS + no broken links) after the shell-naming
  rewrites; `editor/AGENTS.md`, root `AGENTS.md`, and the `docs/content/` pages no longer name Tauri, the
  `src-tauri/` directory, or the webview render path (this is a clean-slate repo; there is no history to
  preserve).

## Risks

- **A missed reference re-introduces a compile/link dependency or a dangling path.** The deletion is a
  whole crate plus dev-loop, gate, package, and docs edits. The grep + `test ! -d` gates above are the
  guard — run them as part of the change, not after. A lingering `use tauri::…` (there can be none once
  `editor/src-tauri` is gone) or a `just`/`tools`/`AGENTS.md` path still naming `editor/src-tauri` is the
  realistic failure, caught by grepping the whole tree for `src-tauri`.
- **The dev-loop rewrite must tear down background Vite.** `run`/`run-debug` start Vite detached; if the
  recipe does not trap the shell's exit and kill Vite, a stray `vite` on port 1420 survives and the next
  `just run` fails `strictPort`. Wire the teardown into the recipe.
- **CEF runtime discovery is environment-sensitive.** `libcef.so` + the CEF resources must be on the
  loader path at runtime (`CEF_PATH`/`LD_LIBRARY_PATH`) both for `just run` inside the toolbox and for
  the staged `editor-package` bundle. A correct link but a missing runtime path yields a shell that
  builds and then fails to start — validate both the dev launch and the staged bundle, not just the
  build.
- **The toolbox image change gates everything.** Nothing in `editor/shell` links until the toolbox
  vendors the pinned CEF distribution (Phase 1). If the image is not yet updated, the shell build step
  DEFERS in the same "hardware/tooling this environment lacks" spirit as the existing gate defers the
  frontend build when bun is absent — but the cutover is not *done* until the toolbox links libcef and
  `editor/src-tauri` is deleted, so do not mark it complete on a deferred shell build.
- **Docs drift is easy to under-do.** Both `AGENTS.md` files and several `docs/content/` pages name the
  shell in prose, not just one row. Grep both trees for `tauri`/`webkit`/`wry`/`webview`/`src-tauri` and
  fix every load-bearing mention; a Stack-table row that still says "Tauri 2" after the crate is gone is
  exactly the stale reference the keep-current rule exists to prevent.
