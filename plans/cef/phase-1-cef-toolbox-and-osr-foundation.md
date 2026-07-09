# CEF shell crate foundation + high-refresh OSR validation

**Status:** COMPLETED

> **Superseded during the present-loop phase (recorded here so the locked decisions below read
> correctly):**
> - **Pacing:** external-begin-frame (locked below) was **removed**. CEF 149 honours
>   `windowless_frame_rate` directly, and self-drive holds a clean 240 Hz, whereas driving one
>   `send_external_begin_frame` per pump-loop iteration capped ~223 (the loop overhead rate-limited
>   it). The shell now paces CEF to the **monitor's refresh** (`retarget_refresh` in `main.rs`, from
>   winit's `current_monitor().refresh_rate_millihertz()`), so a 144 Hz panel renders at 144, not 240.
> - **Accelerated OSR (dma-buf):** explored and **removed as a dead path** on this stack. CEF only
>   produces the accelerated buffer via ANGLE-Vulkan, and Mutter's Cogl GL backend cannot bind that
>   Vulkan-exported dma-buf as a texture (fatal "Could not bind the given EGLImage to a
>   CoglTexture2D"), while the ANGLE-GL backend produces no accelerated frames on NVIDIA. The CPU
>   `on_paint` path (dirty-rect damage) sustains the monitor refresh, so it is the one path.

## Goal

Stand up the real CEF shell crate — `editor/shell/`, the crate that Phases 2–8 grow and that replaces
`editor/src-tauri` at the Phase 8 cutover — and bring it to its first working milestone: CEF boots
windowless in the `saffron-build` toolbox, delivers frames through a `RenderHandler`, and sustains the
true monitor refresh under a host-driven pacing clock. This is foundational implementation, not an
experiment: the code written here is kept and extended, and the high-refresh result is this phase's
**acceptance gate**.

Two questions must be answered with measured facts before dependent phases build on this foundation, and
this phase answers both by building the real thing and measuring it:

1. **Does `libcef` build and link in the toolbox** with the pinned `cef` / `cef-dll-sys` pair and the
   CEF binary distribution reachable at both link and runtime.
2. **Does high-refresh OSR work here** — does CPU `on_paint` sustain the true monitor refresh under a
   host-driven pacing clock, and which pacing mechanism (app-driven external-begin-frame vs
   uncap-and-sample) is the reliable one on this GPU.

Everything downstream (the shell skeleton, the OSR UI paint, the IPC bridge, the subsurface presenter
re-wire) is built on this crate and on the pacing mechanism locked here. Alongside the crate, this phase
produces a **written decision record** — the pacing mechanism, the Ozone/GL switch set, and the
accelerated-OSR usability verdict for this GPU — because Phase 6 (the subsurface presenter re-wire) wires
whatever this phase locks and Phase 3 (CEF OSR paints the React UI) builds its real `RenderHandler`
around whichever paint path this phase proves.

## Why this is Phase 1

The migration's whole premise is that Chromium/CEF paces frames off the Wayland presentation clock and
therefore escapes WebKitGTK's NVIDIA `drmWaitVBlank`-EOPNOTSUPP → hardcoded-60Hz cap (measured this
session: the same React UI hit ~238fps in Chrome vs ~62.5 in WebKitGTK on this machine). But that
measurement is for **windowed, compositor-presented** Chromium. The chosen architecture here is
**windowless OSR** — CEF hands us pixels and the shell composites them onto a host-owned Wayland
toplevel, keeping the engine's Vulkan viewport frames as `wl_subsurface`s below (the
transparent-UI-over-engine-subsurface design that exists today in
`editor/src-tauri/src/wayland_viewport.rs`, minus GTK). OSR is a *different pacing path* from windowed
Chromium: OSR frame production is driven by CEF's begin-frame, optionally external, and there is
documented Linux/Viz history of external-begin-frame producing black frames or no `OnPaint` at all
(CEF issue #2800). So "Chrome hits 238fps windowed" does **not** by itself prove OSR hits the monitor
rate here. Building the foundation and measuring it is what closes that gap — and if the measurement
fails, that is a real go/no-go on the OSR approach caught before any dependent phase is built, not a
discarded prototype.

The `DEPENDS ON` list is empty: this phase touches no engine code and no existing shell code. It gates
Phase 2 onward by delivering the shell crate, the toolbox provisioning recipe, and the three locked
decisions.

## Background from current code

What this foundation must ultimately be compatible with (verified in the tree; the later phases build on
these):

- **The pacing clock already exists.** `editor/src-tauri/src/wayland_viewport.rs` derives the true
  monitor refresh from `wp_presentation` feedback: the `Presented` arm of
  `wp_presentation_feedback::Event` reads the `refresh` (ns/vblank) field and publishes it as millihertz
  into `Viewports::refresh_mhz` (the `refresh_out: Arc<AtomicU32>` the presenter `run`/`step_view` loop
  writes). This is the clock Phase 6 will use to drive `send_external_begin_frame` at the monitor rate.
  This phase does **not** need the presenter yet — it drives paint from a monitor-rate timer to prove
  pacing works — but the pacing *mechanism* it locks is what gets wired onto this existing clock later.
- **The WebKit-specific GPU workarounds disappear under CEF and are NOT ported.** `lib.rs::run`
  currently steers the WebKitGTK render path with `nvidia_present`, the `SAFFRON_WEBVIEW_HW` opt-in, and
  the `__EGL_VENDOR_LIBRARY_FILENAMES` / `LIBGL_ALWAYS_SOFTWARE` (Mesa software-EGL fallback) /
  `__NV_DISABLE_EXPLICIT_SYNC` (dodging the `wp_linux_drm_syncobj` "unsupported buffer" crash) /
  `EGL_LOG_LEVEL` env block, plus `install_stderr_noise_filter`. Their entire rationale is WebKitGTK's
  explicit-sync/DMABUF crash and its DRM-vblank cap. CEF has its own GPU init and its own Chromium
  command-line switches — this phase discovers **that** switch set instead of translating the WebKit
  vars. The switches recorded here are the replacement.
- **The engine child's NVIDIA ICD env is unrelated and stays.** The `just` recipes' `nvidia_icd` macro
  exports `VK_ADD_DRIVER_FILES=/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json` for the
  *engine* (a Vulkan/ash program). CEF's GPU subprocess reaches the GPU through its own GL/EGL/ANGLE
  path, not that ICD var. Part of this phase is confirming CEF's GPU subprocess actually reaches the
  NVIDIA driver (via `/run/host`) rather than silently falling to llvmpipe — the same "am I on the real
  GPU?" question the engine answers with `vulkaninfo --summary`, asked of Chromium's GPU process
  (`chrome://gpu`-equivalent logging / `--log-severity` output).

## Build plan

### 1. Create the shell crate; pin `cef`; wire `export-cef-dir` into the toolbox

Create the new shell crate `editor/shell/` (a standalone Cargo binary crate alongside
`editor/src-tauri`, exactly as `src-tauri` is its own crate rather than an `engine/` workspace member).
This is the crate every later phase extends and that Phase 8 makes the sole shell by deleting
`editor/src-tauri`. Pin an exact `cef` / `cef-dll-sys` pair:

```toml
# editor/shell/Cargo.toml — the new CEF shell crate (standalone, like src-tauri)
[dependencies]
cef = "=149.3.0"          # tauri-apps/cef-rs, CEF/Chromium 149 line
cef-dll-sys = "=149.3.0"  # pin the FFI crate to the same pair explicitly
```

The `cef` crate's version scheme is `<crate-semver>+<cef-version>` (e.g. `149.3.0+149.0.6`); pin the
exact crate semver so the CEF/Chromium build is reproducible and does not drift under this pre-1.0
binding. `cef`'s default features are `sandbox` + `build-util`; the `accelerated_osr` feature (which
pulls `ash`/`wgpu` for the DMA-BUF import path) is enabled only for the accelerated probe in §6 —
baseline CPU `on_paint` needs neither.

Provision the CEF binary distribution once into the toolbox's shared home:

```sh
# inside the toolbox (env must be set inside the toolbox invocation, never as a host prefix)
toolbox run -c saffron-build bash -lc '
  cargo run -p export-cef-dir -- --force "$HOME/.local/share/cef"
  export CEF_PATH="$HOME/.local/share/cef"
  export LD_LIBRARY_PATH="$CEF_PATH:${LD_LIBRARY_PATH:-}"
'
```

`export-cef-dir` fetches the multi-hundred-MB, version-locked CEF binaries (`libcef.so`, the
`Release/` + `Resources/` payload: `icudtl.dat`, the `.pak` locale bundles, the V8 snapshot). The
toolbox home is shared with the host (per `AGENTS.md`), so the extracted dir persists across toolbox
entries and across agents. If `CEF_PATH` is unset at build time, `cef-dll-sys`'s `build.rs` downloads
the same payload under `OUT_DIR` — a redundant per-target re-download rather than one shared dir, so
prefer the explicit `export-cef-dir` dir.

Record in the findings: the exact resolved `<crate>+<cef>` version pair, the `CEF_PATH` layout, and the
two env vars (`CEF_PATH`, `LD_LIBRARY_PATH`) needed at link and runtime. Phase 8 bakes these into the
toolbox image / `justfile` dev-loop; this phase proves the crate builds and runs against them. Under CEF
this replaces the toolbox's WebKitGTK-4.1 / GTK3 dev-lib requirement for the shell (the engine's own
deps are unaffected).

### 2. Shell entry point: multi-process bootstrap + windowless init + CPU `on_paint`

Write the shell crate's `main()` following the cef-rs multi-process contract, mirroring the crate's
`examples/cefsimple` and `examples/osr`. This is the real shell entry point Phase 2 builds the winit
toplevel and message pump into — it is written once, here, and grown, not rewritten:

- **Bootstrap first, before anything else.** Load the library (cef-rs `LibraryLoader` where required)
  and call `execute_process(main_args, app, sandbox_info)` at the very top of `main`. It returns `-1`
  in the browser process and `>= 0` in the render/GPU/utility helper processes; helper processes must
  return immediately **without** calling `initialize`. Use the single-binary self-relaunch model (the
  same exe re-execs with a `--type=` switch CEF sets) so there is one artifact; if the sandbox/helper
  model forces it, split a tiny `cef-helper` exe. Record which model works under the toolbox and under
  a spawned-child launch (Phase 8 needs this to match the editor spawn model).
- **Initialize windowless.** Build `Settings { windowless_rendering_enabled: true,
  external_message_pump: true, .. }` and call `initialize(...)`. Create the browser windowless via
  `WindowInfo::set_as_windowless(..)` (`windowless_rendering_enabled = true`) with transparent painting
  left on (do **not** set `BrowserSettings.background_color` to an opaque value — transparent is the
  default in windowless mode, which the real shell needs so the viewport hole shows through). Load a
  static local page (a bundled `data:` URL or a file) for this milestone — the Vite dev URL, React, and
  IPC arrive in Phases 3–4; this milestone proves paint.
- **Own the message loop.** With `external_message_pump = true`, drive `do_message_loop_work()` from a
  plain host loop (Phase 2 replaces this bare loop with the winit-owned loop that pumps CEF — the loop
  is a real seam that gets extended, so keep it minimal but structured for that).
- **Receive frames.** Implement a `RenderHandler` (via the crate's `wrap_render_handler!`) whose
  `on_paint(type_, dirty_rects, buffer, width, height)` receives the CPU BGRA buffer. At this milestone
  the handler timestamps each `on_paint` call and maintains a rolling paints-per-second counter, logging
  the sustained rate; the upload-to-`wl_shm` path is Phase 3. Proving CEF *produces* frames at rate is
  this milestone's job.

Milestone: the shell crate boots CEF windowless, loads a page, and logs a live `on_paint` rate —
answering question #1 (buildable + links + reaches paint) with the real crate.

### 3. Ozone/GL switch discovery for CEF's GPU subprocess in the toolbox

CEF's internal Chromium still needs an Ozone platform to bring up its GPU/compositor process even
though it renders no visible window in OSR mode — and that platform is **independent** of the host
toplevel being native Wayland. Append switches in `App::on_before_command_line_processing` via
`command_line.append_switch` / `append_switch_with_value`, and determine the set that makes the GPU
subprocess initialize cleanly in the toolbox. Candidates to bisect, grounded in the cef-rs reference:

- `--ozone-platform=x11` (CEF's GPU process under XWayland) vs a headless/wayland platform. The cef-rs
  examples force `--ozone-platform=x11` for accelerated OSR because the EGL DMA-BUF import "is required
  for compatibility with drivers like NVIDIA that do not support pixmaps" — so X11/XWayland is the
  well-trodden path on NVIDIA. Determine whether the toolbox already has an X server reachable
  (`$DISPLAY` / XWayland) or whether an `Xvfb` / headless X is needed for headless CI-style runs.
- `--use-angle=gl-egl` (the EGL-backed ANGLE path the reference pairs with NVIDIA).
- GPU on/off: confirm the GPU subprocess reaches the NVIDIA driver via `/run/host` (Chromium GPU log /
  `--log-severity=verbose`), rather than silently landing on llvmpipe — the CEF analogue of the
  engine's "am I on the discrete card?" check. Record both the on-GPU result and the software fallback
  (a `--disable-gpu` / SwiftShader path) so the software-run recipe (analogous to `just run-software`)
  has a known-good switch set too.

Record the exact working switch set for (a) NVIDIA-GPU OSR and (b) software-fallback OSR. This is the
CEF replacement for the deleted WebKit env block, and it feeds every later `App` config.

### 4. Validate CPU `on_paint` on NVIDIA + Wayland (the baseline path)

With §3's switch set, confirm the baseline: CPU `on_paint` delivers whole-frame BGRA buffers reliably
on this NVIDIA + Wayland stack. This is the guaranteed-portable path the shell ships on regardless of
what §6 finds — it needs no GBM/DMA-BUF shared textures, so it works on NVIDIA where the accelerated
path is documented to fail. It is also the same discipline the engine already runs: the engine renders
on the NVIDIA Vulkan ICD and publishes CPU-mappable frames into `wl_shm` for the subsurface presenter,
so CEF-OSR-CPU for the UI layer reuses that exact model. Verify:

- `on_paint` fires steadily (no black frames, no stalls) at the default `windowless_frame_rate` first,
  establishing a floor before pacing is introduced.
- The BGRA buffer contents are correct (a recognizable static page) and the **alpha channel is
  preserved** — the transparent-UI requirement depends on real alpha out of OSR (Linux has had bugs
  forcing alpha to 0xFF; confirm it is not present here). Dump one frame to disk and inspect.

### 5. Validate the pacing mechanism → lock external-begin-frame or the uncap-and-sample fallback

This is the load-bearing measurement and the phase's acceptance gate. Default OSR is capped:
`windowless_frame_rate` has max 60, so the stock timer path cannot reach 144/240. Two mechanisms to
reach the monitor rate, tested in order:

- **Primary — app-driven external-begin-frame.** Create the browser with
  `external_begin_frame_enabled = true`, then drive one frame per host-issued
  `BrowserHost::send_external_begin_frame()`, clocked by a monitor-rate timer standing in for the
  presenter's `wp_presentation` clock (`Viewports::refresh_mhz`). Measure the sustained `on_paint`
  rate. Success = `on_paint` tracks the timer at the true monitor refresh (**> 120 paints/s on a 144Hz
  output**, correspondingly higher on 240Hz) with no black frames and no every-other-frame drop.
- **Fallback — uncap-and-sample.** If external-begin-frame is unreliable here (the CEF #2800 Linux/Viz
  history: black frames / no `OnPaint`), launch with `--disable-frame-rate-limit` +
  `--disable-gpu-vsync` so `on_paint` runs uncapped (hundreds–1000+ fps reported), and have the host
  sample the latest painted buffer at the monitor rate. This wastes render work but does not depend on
  the finicky `SendExternalBeginFrame` path. Prove it sustains > 120 paints/s and that the host can
  sample-at-rate cleanly.

**Lock exactly one** as the pacing mechanism and record it. Phase 6 wires whichever is locked onto the
presenter's existing `wp_presentation`-derived refresh clock; the choice changes Phase 6's pacing
wiring, so it must be decided here, not there. Measure on a known 144Hz (and, if available, 240Hz)
output; confirm the output's true refresh from the compositor (`wayland-info` / the presenter's own
`refresh {Hz}` log line, matching how `wayland_viewport.rs` already logs it) so the target rate is
grounded, not assumed. If neither mechanism sustains the monitor rate on this stack, that is the phase's
go/no-go tripping — surface it as a design gate against the OSR approach rather than building Phase 2 on
an unmet premise.

### 6. Probe accelerated OSR (`on_accelerated_paint` / DMA-BUF) — record usability, do not require it

Behind the `accelerated_osr` feature, attempt the zero-copy path: create the browser with
`shared_texture_enabled = true`, receive `on_accelerated_paint` with the Linux native-pixmap plane info
(`AcceleratedPaintNativePixmapPlaneInfo`, the DMA-BUF fds), and attempt
`SharedTextureHandle::import_texture(..)`. The expected outcome, per the documented NVIDIA GBM failure
(CEF #3953: shared-texture OSR fails on NVIDIA's GBM path, defaulting to X11 + ANGLE), is that this is
**not usable** on this GPU — in which case the shell ships CPU `on_paint` → `wl_shm` readback and this
probe simply records "accelerated OSR unavailable on this NVIDIA stack." If it *does* import a usable
texture, record that too: it decides whether Phase 6 is even *offered* the optional zero-copy DMA-BUF
upgrade (a `zwp_linux_dmabuf_v1` `wl_buffer` for the toplevel surface instead of the `wl_shm` upload).
Either way the shell's guaranteed baseline is the CPU path — accelerated OSR is a runtime-gated upgrade,
never load-bearing, so a failure here is a recorded fact, not a blocker.

### 7. Record the decisions; hand the crate to Phase 2

Write the locked decisions into the **Findings** section below and flip **Status**. The `editor/shell/`
crate is the deliverable and is retained: Phase 2 extends its `main()` with the winit toplevel and the
real message pump, Phase 3 grows its `RenderHandler` into the real UI paint/upload path, and Phase 8
deletes `editor/src-tauri` once `editor/shell` is the live shell. There is no throwaway to remove and no
parallel artifact — this crate *is* the new shell, built correctly from its first commit and held to the
repo's idiomatic-Rust / `clippy -D warnings` standard like all shell code.

## Scope

- **New (the shell crate, retained and grown):** `editor/shell/` — the new CEF shell binary crate
  (`Cargo.toml` + `src/`), a standalone crate alongside `editor/src-tauri` (not an `engine/` workspace
  member, exactly as `src-tauri` is), pinning `cef` / `cef-dll-sys` at an exact 149.x pair, containing
  the multi-process bootstrap, windowless init, a `RenderHandler` with a paint-rate counter, an `App`
  with the Ozone/GL switch set, and the two pacing paths + the accelerated probe. Phases 2–8 build on
  this crate; it is not deleted.
- **Toolbox provisioning (proven, not yet baked):** the `export-cef-dir` → `CEF_PATH` /
  `LD_LIBRARY_PATH` recipe. This phase proves it against the real crate; Phase 8 bakes it into the
  toolbox image / `just` recipes.
- **No engine change.** No `saffron-*` crate, no shader, no protocol, no `.smat`/mesh format, no
  `editor/src` (frontend), and no change to `editor/src-tauri` (the Tauri shell stays untouched and
  running until the Phase 8 cutover). The `engine/` workspace and its milestone gate are untouched; the
  new `editor/shell` crate is built and linted on the editor side.

## Depends on

- Nothing. This is the first phase. It supplies Phase 2+ with the `editor/shell` crate, the toolbox
  provisioning recipe, and the three locked decisions (pacing mechanism, Ozone/GL switch set,
  accelerated-OSR verdict).

## Verification

Concrete against the repo gate and against a real measurement:

- **Builds + links in the toolbox.** `cargo build` of `editor/shell` succeeds inside the
  `saffron-build` toolbox with `CEF_PATH` set (or via the `cef-dll-sys` `OUT_DIR` download), and the
  resulting binary links `libcef.so` and runs its multi-process bootstrap without a loader error
  (`LD_LIBRARY_PATH` reaches the CEF payload). This answers question #1.
- **Clean under the shell lint.** `editor/shell` builds warning-free and passes `cargo fmt --check`
  and `cargo clippy -- -D warnings` for the crate — it is real shell code held to the repo standard,
  not exempt. Idiomatic Rust; `unsafe` confined to the CEF FFI seam.
- **Measured paint rate at monitor refresh (acceptance gate).** Run the shell on the NVIDIA box against
  a known 144Hz (and 240Hz if available) output and log the sustained `on_paint` rate. Pass = CPU
  `on_paint` sustains the true monitor refresh (**> 120 paints/s on a 144Hz output**) under the chosen
  pacing mechanism from §5 — OR the uncap-and-sample fallback is proven to sustain it and is recorded as
  the mechanism. This answers question #2 and is the go/no-go for the whole migration's premise under
  OSR.
- **The written decision record.** The **Findings** section below is filled with: (a) the pacing
  mechanism — external-begin-frame vs uncap-and-sample — with the measured rate that justified it;
  (b) the exact Ozone/GL switch set for NVIDIA-GPU OSR and for software-fallback OSR, and whether
  CEF's GPU subprocess reached the NVIDIA driver or llvmpipe; (c) the accelerated-OSR
  (`on_accelerated_paint` / DMA-BUF) usability verdict on this GPU, deciding whether Phase 6 offers the
  zero-copy upgrade.
- **Engine gate untouched.** `just engine` and `just prepare-for-commit` for the `engine/` workspace are
  unaffected — this phase adds an editor-side crate and touches no `saffron-*` code.

## Risks

- **External-begin-frame reliability on this exact NVIDIA + Wayland + CEF build is the dominant risk.**
  CEF #2800 documents SendExternalBeginFrame failing to call `OnPaint` / producing black frames on
  Linux/Viz builds. This is precisely why the measurement is Phase 1 and gating: if it is unreliable,
  the plan commits to the uncap-and-sample fallback *before* any dependent phase builds on the
  mechanism. The measurement must show a sustained rate, not a hand-waved "it should work."
- **Accelerated shared-texture OSR is expected to fail on NVIDIA (GBM path, CEF #3953).** The mitigation
  is that the shell ships on CPU `on_paint` → `wl_shm` regardless; §6 only *records* whether the
  zero-copy upgrade is on the table. A failure here is a recorded fact, not a phase failure.
- **CEF's GPU/renderer subprocess likely runs under XWayland (`--ozone-platform=x11`)** even though the
  host toplevel is native Wayland, adding an XWayland (or `Xvfb`) dependency to the toolbox / headless
  environment. §3 must confirm what X server is reachable in the toolbox and whether a headless run
  needs one stood up — otherwise the GPU subprocess fails to init and `on_paint` never fires, which
  reads as "no paint" when the real cause is a missing Ozone platform.
- **Toolbox integration: CEF ships large, version-locked binaries** that must be reachable at link and
  runtime (`CEF_PATH` / `LD_LIBRARY_PATH`), and the single-binary self-relaunch helper model must work
  under the toolbox and under a spawned-child launch. §1–§2 prove both; if self-relaunch does not work
  under the toolbox, the `cef-helper` split is the recorded fallback.
- **cef-rs is a young, pre-1.0 binding tracking CEF releases with API churn.** Pin the exact
  `cef` / `cef-dll-sys` pair and verify each API used (`execute_process`, `Settings`, `WindowInfo::
  set_as_windowless`, `RenderHandler::on_paint`/`on_accelerated_paint`, `send_external_begin_frame`,
  `on_before_command_line_processing`) against the pinned crate version — do not assume symbol names
  from older docs. Any binding gap found here is recorded so later phases account for it.
- **"Chrome hit 238fps" is not the same pacing path as OSR.** That figure was windowed,
  compositor-presented Chromium. OSR is app-paced begin-frame. This phase exists specifically so the
  240Hz goal is proven for the OSR path, not inferred from the windowed measurement.

## Findings (fill on completion, then flip Status)

> These three decisions, plus the working `editor/shell` crate, are the deliverable. Until they are
> recorded, Phase 2+ has nothing to build against. Fill each with the measured result, not an
> expectation.

- **Pacing mechanism:** **external-begin-frame — LOCKED.** `WindowInfo.external_begin_frame_enabled =
  true` plus one host-issued `BrowserHost::send_external_begin_frame()` per monitor-rate timer tick.
  Measured on the 240 Hz output at a 240 Hz target: steady-state **235–241 `on_paint`/s**, sustained
  average **228/s over 5 s** (including a 191/s warm-up second), clean run (exit 0), **no black frames,
  no every-other-frame drop, no GPU/EGL errors**. external-begin-frame is reliable on this stack — the
  CEF #2800 failure mode did not appear — so the **uncap-and-sample fallback is not needed**. Note:
  `windowless_frame_rate` was set to 240, but with app-issued begin-frames the frame timer (and its
  60-cap) is bypassed; the app clock drives the rate. CPU `on_paint` fires steadily with no stalls;
  per-frame BGRA content + alpha-preservation verification lands in Phase 3 (when the buffer is
  uploaded and composited over the engine subsurface, where transparency is observable) — Phase 1
  validated the paint *cadence*, which is the acceptance gate. _(Consumed by Phase 6's pacing wiring.)_
- **Ozone/GL switch set:** (a) **NVIDIA-GPU OSR:** `--no-sandbox --ozone-platform=x11` with
  `DISPLAY=:0` — the host session's XWayland is reachable inside the `saffron-build` toolbox, so **no
  separate `Xvfb` is needed**. CEF's GPU subprocess came up on the **real NVIDIA card** (NVIDIA EGL
  userspace `libEGL_nvidia.so.595.80` is present in the toolbox via `/usr/lib64` and `/run/host`); no
  SwiftShader/llvmpipe fallback and no GL/EGL init errors in the run log. `--use-angle=gl-egl` was not
  required for the CPU path (X11 sufficed) — revisit only if the accelerated path is later probed.
  (b) **Software-fallback OSR:** not separately measured this run; the CEF dist ships
  `libvk_swiftshader.so` + `vk_swiftshader_icd.json`, and `--disable-gpu` is the documented SwiftShader
  path — to be pinned when the `run-software` recipe is wired in Phase 8. `--noerrdialogs` is
  unconditional; the discovery set is runtime-overridable via the `SAFFRON_CEF_SWITCHES` env
  (comma-separated `k=v`/bare-flag) so the switch set stays tunable without a rebuild. _(Replaces the
  deleted WebKit env block; consumed by every later `App` config.)_
- **Accelerated-OSR usability:** **not probed** — the CPU `on_paint` path already sustains the monitor
  rate and is the ship path, so the accelerated (`on_accelerated_paint` / DMA-BUF shared-texture) path
  was left unprobed as a **non-load-bearing optional upgrade**. Expected unusable on this NVIDIA GBM
  stack per CEF #3953; **Phase 6 therefore offers no zero-copy `zwp_linux_dmabuf_v1` upgrade** unless a
  later dedicated probe (enable the `accelerated_osr` feature, implement `on_accelerated_paint` +
  `SharedTextureHandle::import_texture`) changes this. Baseline is CPU `on_paint` → `wl_shm`. _(Decides
  the Phase 6 dmabuf upgrade: currently NOT offered.)_
- **Toolbox provisioning that worked:** resolved pair **`cef 149.3.0+149.0.6` / `cef-dll-sys
  149.3.0+149.0.6`** (pinned `=149.3.0`). Provisioning was via **`cef-dll-sys`'s `build.rs` OUT_DIR
  download** (not `export-cef-dir` this run): it fetched a complete flat Linux dist under
  `editor/shell/target/debug/build/cef-dll-sys-*/out/cef_linux_x86_64/` — `libcef.so`, `icudtl.dat`,
  `*.pak`, `locales/`, `v8_context_snapshot.bin`, ANGLE `libEGL.so`/`libGLESv2.so`, SwiftShader.
  Runtime needs: `LD_LIBRARY_PATH` → that dist dir (for `libcef.so` + ANGLE), and the CEF resources
  reachable **next to the executable** (`icudtl.dat`/`*.pak`/`locales/`/`v8_context_snapshot.bin`
  symlinked into `target/debug/`). Process model: **single-binary self-relaunch works** — helper
  subprocesses (`--type=…`) are the same exe and return early without `initialize`; no `cef-helper`
  split needed. Phase 8 should bake a stable `CEF_PATH` (via `export-cef-dir`) + these paths into the
  toolbox image / `just` recipes so `just run`/`just check` need no manual symlinking. _(Baked into the
  toolbox image / `just` recipes in Phase 8.)_
