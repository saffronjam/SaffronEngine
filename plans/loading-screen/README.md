# Project loading (non-blocking load + loading screen)

**Status:** COMPLETED

Make `saffron-host` stay responsive while a thick project loads (control drain + frame publish every
iteration), gate/discard commands that make no sense mid-load with a typed reply, expose pollable
load progress backed by a concrete boot-stage list, and turn the editor's project picker into a
loading bar.

## Why

Today the entire project load runs **synchronously inside the once-per-frame control drain**, and GPU
residency smears lazily across the first rendered frames. `AssetServer::load_project`
(`engine/crates/assets/src/project.rs`) does the file read, JSON parse, `wait_gpu_idle`,
`clear_asset_caches`, cold catalog disk scan, thumbnail sweep, and `scene_from_json` in one call —
reached from the `load-project` handler (`load_project_into` in
`engine/crates/control/src/commands_asset.rs`) which itself runs inside the control server's per-frame
drain. Because the host main loop (`App::step_frame` in `engine/crates/app/src/lib.rs`) cannot reach
frame publish (`HostLayer::on_ui` → `publish_pipelined_view` in `engine/crates/host/src/layer.rs`)
until `on_update` returns, a thick load **freezes the viewport and stalls every control round-trip**
until it finishes — and commands that arrived meanwhile are queued and replay in a burst, rather than
being discarded as nonsensical mid-load.

This plan moves the CPU **doc phase** and the GPU **residency phase** off the main thread, advances one
bounded step per frame from `HostLayer::on_update` (mirroring the existing `ThumbnailWorker` drained at
each frame in `engine/crates/host/src/layer.rs`), introduces a project-load **phase** state machine
that discards non-sensical commands with a typed `busy-loading` reply instead of queuing them, surfaces
the load state as a pollable progress snapshot, and turns the editor picker into a grounded loading bar.

## Goal

- **Responsive at all times.** During any open/new/reload/bootstrap, `step_frame` drains the socket and
  publishes a frame every iteration — no multi-second gap, the shm seqlock keeps advancing.
- **Project-load states that discard, not queue.** A `ProjectPhase { Unloaded, Loading, Ready, Failed }`
  machine on `SceneEditContext` drives a dispatch-level gate: while `Loading`, any command outside a
  tiny allow-list is **discarded** with `{ ok:false, code:"busy-loading" }` — never queued for replay.
- **Grounded progress bar in the editor picker.** An ordered `BootStage` list backed by a per-frame
  progress snapshot is exposed over a pollable `project-status` command; the editor's
  `ProjectStartupModal` (`editor/src/app/ProjectStartupModal.tsx`) becomes a picker + loading-view split
  that shows a real progress bar and stage label, driven by polling rather than a blocking promise.

## Phases (dependency-ordered)

| Phase | File | Summary | Depends on |
|-------|------|---------|------------|
| 1 | `phase-1-project-load-state-machine.md` | `ProjectPhase` on `SceneEditContext` (replaces the `project_loaded` bool), the dispatch-level command gate + allow-list, the typed `Error::Busy` / `code:"busy-loading"` wire error, the envelope `code` field, and the editor drop-on-busy hook. | — |
| 2 | `phase-2-non-blocking-load.md` | The off-thread `ProjectDocWorker` (read/parse/cold catalog scan) + main-thread install + stepped GPU residency, driven by the control-owned `ProjectLoader` state machine one bounded step per frame; cancel; and routing bootstrap + open/new/reload through one loader seam. (As-built: no second GPU worker — see the phase-2 header.) | Phase 1 |
| 3 | `phase-3-progress-protocol-and-boot-stages.md` | The ordered `BootStage` list, the `ProjectStatusDto` + `project-status` / `cancel-load` control commands, protocol codegen, `sa` CLI formatter, and the docs page. | Phase 2 |
| 4 | `phase-4-editor-loading-screen.md` | `ProjectStartupModal` → picker + loading-view split, the `useProjectLoadPoll` hook, the `projectLoad` store slice, and its relation to `LoadingOverlay`. | Phases 3, 1 |

The chain is strict: Phase 1's gate is dormant until Phase 2 gives `Loading` real duration; Phase 3
exposes the truth Phase 2 produces; Phase 4 consumes Phase 3's wire surface.

## Grounding (key current-code entry points)

| What | File | Symbols |
|------|------|---------|
| Host main loop that must keep draining + publishing | `engine/crates/app/src/lib.rs` | `App::run`, `App::step_frame`, `on_update` dispatch |
| Frame publish reached only after `on_update` returns | `engine/crates/host/src/layer.rs` | `HostLayer::on_update`, `HostLayer::on_ui` → `publish_pipelined_view` |
| Control server drained once per frame (non-blocking) | `engine/crates/control/src/server.rs` | drain loop |
| Control context polled once per frame from the host loop | `engine/crates/control/src/context.rs` | poll |
| Command dispatch + where the gate lands | `engine/crates/control/src/registry.rs` | `CommandRegistry::dispatch`, `is_read_only_command` |
| Synchronous project lifecycle to be decomposed | `engine/crates/control/src/commands_asset.rs` | `load_project_into`, `open-project`/`new-project` handlers, `apply_project_info`, `require_project_loaded`, `bootstrap_project_from_env` |
| Synchronous load body moved off-thread | `engine/crates/assets/src/project.rs` | `AssetServer::load_project`, `create_project` |
| Prior-art off-thread worker drained each frame | `engine/crates/assets/src/thumbnail.rs` | `ThumbnailWorker`, `worker_loop` |
| Authoritative load state home (phase + progress + inbox) | `engine/crates/sceneedit/src/context.rs` | `SceneEditContext`, `project_loaded` (→ `project_phase`) |
| Editor picker that becomes the loading view | `editor/src/app/ProjectStartupModal.tsx` | picker JSX, `complete()`, `handleProjectLoaded` |

## Ground rules

- **One bring-up path.** Bootstrap and open/new/reload all seed the same `ProjectLoader`. No synchronous
  `AssetServer::load_project` / `create_project` caller survives on the handler or bootstrap path
  (NO-LEGACY: the old synchronous flow is replaced, not kept alongside).
- **The dispatch gate DISCARDS.** Non-allow-listed commands during `Loading` return
  `{ ok:false, code:"busy-loading" }` — they are never queued for replay.
- **Two distinct axes.** Engine-process lifecycle (`engineStatus.phase` / `LoadingOverlay`) and
  project load (`projectLoad` / the modal loading view) stay separate; they share only visual
  vocabulary, never a merged state.
- **Each phase ends green.** `just engine` + `just prepare-for-commit` (format + clippy `-D warnings`),
  protocol regenerated where a wire type changed, `just e2e`. The two responsiveness proofs that define
  "done": during a headless thick load, `sa ping` replies within one `IDLE_POLL_INTERVAL` throughout,
  and the shm seqlock sequence keeps advancing.

Detail lives in the phase files; this page is the index.
