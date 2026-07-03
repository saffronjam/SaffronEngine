# Phase 2 — Non-blocking staged project load

**Status:** COMPLETED

## As-built (the shipped architecture)

The shipped design keeps the goal — the host main loop never stalls during a load — with a
lower-risk decomposition than the original two-worker proposal below:

- **Off-thread doc worker** (`engine/crates/assets/src/project_load.rs`, `ProjectDocWorker`): runs
  the heavy, blocking CPU/IO — `read_to_string` + `parse_json` + version-gate (`Manifest`), then the
  cold catalog disk reconcile (`Catalog`, determinate `done`/`total`) — building a **fresh owned**
  `AssetCatalog`. The cold scan is factored into `reconcile_catalog_from_disk` /
  `resolve_catalog_from_disk` (`scan.rs`), free of any `AssetServer` borrow, so both the worker and
  the sync path call one copy.
- **Main-thread install** (`engine/crates/control/src/project_loader.rs`, `install_doc`): one bounded
  step — `wait_gpu_idle` → `clear_asset_caches` → swap scene/catalog → `scene_from_json` (needs the
  main-thread `ComponentRegistry`, so it stays here) → apply render settings + sidecar. The
  idle-before-clear UAF discipline lives here now.
- **Stepped main-thread residency**: instead of a second GPU worker thread + shared `GpuQueue` +
  unsafe `Send` wrapper, the loader warms up to `RESIDENCY_PER_FRAME` scene-referenced meshes/textures
  per frame through the existing `with_gpu_uploader` seam (`AssetServer::warm_asset` →
  `load_mesh_asset`/`load_texture_asset`). Bounded per frame, so the loop drains + publishes between
  steps; determinate `Assets` `n/m` from the scene reference set (`scene_residency_ids`).
- **Orchestrator** lives in `saffron-control` (`ProjectLoader`, owned by `ControlContext`), advanced
  each frame from `HostLayer::on_update` via `ControlContext::advance_project_load` — not on
  `HostLayer` — because the install reuses the control-side `RendererProjectHost` + scene helpers.
- **No renderer warmup hooks / no `Skybox`/`Accel` driving**: those `BootStage` variants exist (the
  Phase-3 DTO mirrors the full enum) but are not driven; the sky panorama warms as part of `Assets`
  (`scene_residency_ids` includes `Environment.sky_texture`). `Ready` follows residency drain.
- **Sync methods retained for `saffron-player`**: `AssetServer::load_project` / `create_project` /
  `create_scratch_project` stay for the standalone exported game (it boots blocking — no control
  plane to keep responsive). The **host** has exactly one bring-up path (the loader); this is not a
  duplicate path for the same flow.
- **Teardown**: the doc worker is pure CPU/IO (no GPU handbacks), so it is safe to abandon on exit —
  no `LoadWorkerJoined` teardown step was needed (unlike the thumbnail worker).
- **Render is suppressed while `phase == Loading`** (`HostLayer` sets `RedrawController::set_suppressed`).
  `on_update` runs every loop iteration regardless of whether a frame renders, so the control socket
  drains and the loader advances without any continuous-render reason — and *not* rendering across the
  install means no frame ever draws against a half-swapped scene / cleared caches (which wedged
  `end_frame` on the llvmpipe software path). The `Loading → Ready` flip requests a redraw, so the
  loaded scene draws once the load settles. The viewport is parked behind the loading screen the whole
  time, so nothing visible is lost.
- **A fresh (New) project installs with `Scene::default()`, not `scene_from_json`** — its off-thread
  doc carries an empty `{}` scene, which `scene_from_json` rejects as versionless (matching the old
  `create_project`, which never deserialized). `install_doc` only deserializes a non-empty scene doc.

Validated live: during a `dev`-project load, `get-project` stayed answered (socket responsive) while
`add-entity` returned `busy-loading` (discarded, not queued), then flipped to `loaded:true` +
`add-entity` succeeding once residency drained.

The original design write-up follows for reference.

---


## Goal

Move the whole project load off the main loop so `App::step_frame` (`engine/crates/app/src/lib.rs`,
`App::step_frame`) drains the control socket and publishes a frame **every iteration** throughout an
open / new / reload / bootstrap. Introduce a one-shot `ProjectDocWorker` (CPU/IO), a persistent
`AssetLoadWorker` (GPU decode + upload, sibling of `ThumbnailWorker`), and a host-owned `ProjectLoader`
state machine advanced one bounded step per frame from `HostLayer::on_update`
(`engine/crates/host/src/layer.rs`). This gives `ProjectPhase::Loading` (from Phase 1) real duration, so
the Phase-1 dispatch gate now fires. Bootstrap and every lifecycle command route through one loader
seam.

**NO-LEGACY:** the synchronous `AssetServer::load_project` (`engine/crates/assets/src/project.rs`,
`AssetServer::load_project`) and `AssetServer::create_project` bodies are decomposed into the doc-worker
logic and the main-thread install step; their public synchronous signatures are removed. The inline
handler load in `load_project_into` / `open-project` / `new-project` / `reload-project` is deleted. No
synchronous bring-up path survives on the handler or bootstrap path — grep proves it.

## Why (shared context)

Today `AssetServer::load_project` runs the entire load — file read, JSON parse, `wait_gpu_idle`,
`clear_asset_caches`, cold catalog disk scan, thumbnail sweep, `scene_from_json` — synchronously inside
the command handler (`load_project_into`, `engine/crates/control/src/commands_asset.rs`), which itself
runs inside the once-per-frame control drain. GPU residency then smears lazily over the first
`render_scene` frames. Both stall the single main loop: `step_frame` cannot reach frame publish
(`HostLayer::on_ui` → `publish_pipelined_view`) until `on_update` returns, so during a thick load no
frame publishes and no socket byte drains — the viewport freezes and every control round-trip stalls,
with queued commands replaying in a burst afterward.

This phase is where the "responsive at all times" guarantee is actually delivered.

## Dependencies

- **Phase 1** (`plans/loading-screen/phase-1-project-load-state-machine.md`) must land first: it adds
  the `ProjectPhase` enum on `SceneEditContext`, the dispatch-level command gate + `is_loading_safe_command`
  allow-list, and the typed `Error::Busy` / `code: "busy-loading"` wire error. The gate is dormant until
  this phase gives `Loading` a real duration.
- Phase 3 (`phase-3-progress-protocol-and-boot-stages.md`) exposes the `ProjectLoadProgress` written
  here over the wire and adds the `cancel-load` / `project-status` commands. Phase 4 consumes them.

---

## 1. Engine types added to `saffron-sceneedit`

State homes (from the blueprint): `SceneEditContext` (`engine/crates/sceneedit/src/context.rs`,
`SceneEditContext`) owns the authoritative phase, the per-frame progress snapshot, the command inbox,
and the cancel flag. It is `&mut`-borrowed into `EngineContext.scene_edit`, so both the dispatch gate
and the (Phase-3) status command read it with zero new plumbing, and the host loop writes it.

### 1.1 `BootStage` (next to `ProjectPhase` from Phase 1)

Keep this in `saffron-sceneedit` (no protocol dep). Phase 3 mirrors it to a `BootStageDto`.

```rust
/// The ordered boot stages within `ProjectPhase::Loading`. Reported through `ProjectLoadProgress`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BootStage {
    #[default]
    Manifest,   // read + parse project.json                  (indeterminate)
    Catalog,    // reconcile catalog vs disk (cold scan)       (done/total over scanned files)
    Scene,      // scene_from_json                             (indeterminate)
    Install,    // main-thread: wait_gpu_idle + swap + enqueue (indeterminate, one frame)
    Assets,     // residency: mesh + texture uploads           (done/total = resident/total)
    Skybox,     // sky panorama resident + IBL env bake        (indeterminate, warmup)
    Accel,      // RT TLAS / first-frame priming               (indeterminate, warmup)
    Ready,
    Failed,
}
```

### 1.2 Per-frame progress snapshot + request inbox + cancel flag

```rust
/// The per-frame progress snapshot the loader writes and (Phase 3) `project-status` reads.
#[derive(Debug, Clone, Default)]
pub struct ProjectLoadProgress {
    pub stage: BootStage,
    pub done: u32,
    pub total: u32,      // 0 == indeterminate
    pub label: String,   // human ("Scanning assets", "Loading meshes 12/40")
    pub current_item: String,
    pub error: String,   // empty unless stage == Failed
    pub version: u64,    // monotonic; the editor dedups on it
}

/// One request handed from a command handler / bootstrap to the loader.
pub enum ProjectLoadRequest {
    Open(String),      // project path
    New(NewProjectSpec),
    Reload,            // re-open the current project_path
}
```

Add fields to `SceneEditContext` (with `Default`):

- `pub project_load: ProjectLoadProgress`
- `pub project_load_inbox: Option<ProjectLoadRequest>` (default `None`)
- `pub project_cancel: bool` (default `false`)

`NewProjectSpec` is whatever the current `new-project` handler already parses from its params DTO; move
its definition to `saffron-sceneedit` (or re-export it) so `ProjectLoadRequest` can own it without a
control-crate dependency inversion. Confirm which crate defines the new-project params today and place
`NewProjectSpec` at or below `saffron-sceneedit` in the DAG.

Every loader transition below bumps `project_load.version`.

---

## 2. The doc worker — new module `engine/crates/assets/src/project_load.rs`

One-shot per load; pure CPU/IO, `Send`. It requires `ComponentRegistry` to be `Send + Sync` (it is fn
pointers + type metadata; add the bounds or wrap the shared handle in `Arc` — verify and make it so in
`saffron-scene`, `ComponentRegistry`).

```rust
/// The staged result of the doc phase, installed on the main thread.
pub struct LoadedDoc {
    pub scene: Scene,
    pub catalog: AssetCatalog,
    pub project_info: ProjectInfo,
    pub sidecar: ProjectSidecar,
    pub render_settings: Option<serde_json::Value>,
    pub residency_jobs: Vec<LoadJob>,   // meshes/textures/sky, owned ByteSources
}

/// Progress the doc worker publishes as it moves read → scan → scene.
#[derive(Default)]
pub struct DocProgress {
    pub stage: BootStage,
    pub done: u32,
    pub total: u32,
    pub current_item: String,
}

pub struct ProjectDocWorker {
    handle: Option<JoinHandle<Result<LoadedDoc>>>,
    progress: Arc<Mutex<DocProgress>>,
}
```

The worker body reuses the exact ordered logic of today's `AssetServer::load_project`
(`engine/crates/assets/src/project.rs`, `AssetServer::load_project`) **minus** the two main-thread-only
steps (`wait_gpu_idle`, `clear_asset_caches`) and minus the in-place mutation of the live catalog /
scene — it builds into **fresh owned** `Scene` + `AssetCatalog`:

1. `read_to_string` + JSON parse + version gate → set `DocProgress.stage = Manifest`.
2. `catalog_from_json` + `catalog_folders_from_json` into a fresh `AssetCatalog`; then the disk
   reconcile (`engine/crates/assets/src/scan.rs`, `scan_assets` / `load_catalog`) against that fresh
   catalog → `stage = Catalog`. Instrument the walk to set `total` after enumeration and increment
   `done` per file — this is the heavy, variable cost, and the reason the worker exists. Write the
   catalog cache exactly as today.
3. `scene_from_json(&registry, &scene_doc)` into a fresh `Scene` → `stage = Scene`.
4. Walk the fresh scene's `Mesh` / `SkinnedMesh` / `Material` / `Environment.sky_texture` references
   (reuse `scan_asset_references`, `engine/crates/control/src/commands_asset.rs`, `scan_asset_references`
   — move it to `saffron-assets` if the control crate is above the worker in the DAG so both call one
   copy, per NO-LEGACY) against the fresh catalog, resolving each to a `LoadJob` with an **owned**
   `ByteSource` (standalone path, or container-chunk bytes copied out via `load_model_asset` — the
   `ThumbnailContent` path-vs-bytes split). Meshes carry `Some(sdf_bake)` inputs; textures carry their
   colorspace.

`ensure_script_src` / `ensure_script_library` (small mkdir + write) stay in the worker (pure IO). The
worker returns `Err` on any failure; the loader maps it to `Failed`.

`ProjectDocWorker::spawn(inputs) -> Self` spawns the thread and stores the `JoinHandle` + shared
`Arc<Mutex<DocProgress>>`. `progress_snapshot(&self) -> DocProgress` clones under the lock; the loader
copies it into `SceneEditContext::project_load` each frame while `Parsing`.

For a **New** request the worker builds an empty `LoadedDoc` in-worker (empty scene, minimal catalog,
default sidecar/info, empty `residency_jobs`) using the `create_project` scaffolding logic, so the New
path shares the identical seam and produces the same `LoadedDoc` type.

---

## 3. The residency worker — new module `engine/crates/assets/src/asset_load_worker.rs`

Mirror `engine/crates/assets/src/thumbnail.rs` (`ThumbnailWorker`, `worker_loop`, `WorkerState`).

```rust
pub enum LoadJob {
    Mesh { id: Uuid, source: ByteSource, sdf: SdfBakeInputs },
    Texture { id: Uuid, source: ByteSource, space: Colorspace },
}

#[derive(Default)]
pub struct LoadState {
    queue: VecDeque<LoadJob>,
    in_flight: HashSet<Uuid>,
    failed: HashSet<Uuid>,
    mesh_handback: Vec<(Uuid, Arc<GpuMesh>)>,
    texture_handback: Vec<(Uuid, Arc<GpuTexture>)>,
    stop: bool,
    total: u32,     // enqueued distinct jobs
    resident: u32,  // handed back so far
}

pub struct AssetLoadWorker {
    handle: Option<JoinHandle<()>>,
    shared: Arc<(Mutex<LoadState>, Condvar)>,
}
```

Worker loop body (mirror `worker_loop` in `thumbnail.rs`): pop a `LoadJob` → `source.read()` →
`load_mesh_from_bytes` / `decode_image_from_memory[_hdr]` (CPU decode) →
`gpu.upload_mesh(..., Some(sdf))` / `gpu.upload_texture(...)` (GPU) → push `(id, Arc)` to the matching
handback; on a decode/upload error insert the `id` into `failed`. Dedup enqueue by `id` via `in_flight`.
The loop waits on the `Condvar` when the queue is empty and exits when `stop` is set (the same pattern
`ThumbnailWorker` uses).

### 3.1 GPU seam — `WorkerUploaderGpu` in `engine/crates/host/src/control_renderer.rs`

Reuse the worker-safe `GpuUploader` trait (`engine/crates/assets/src/gpu.rs`, `GpuUploader`). Add
`WorkerUploaderGpu`, a copy of `WorkerThumbnailGpu` (`control_renderer.rs`, `WorkerThumbnailGpu`) **minus**
the `ThumbnailRenderer`: it holds the renderer's `Arc<Device>` + `Arc<Descriptors>` + its own
`Uploader` + `skinning_enabled`, is `unsafe impl Send`, and reuses the existing `GpuUploader` impl block
(`control_renderer.rs`, `impl GpuUploader for WorkerThumbnailGpu`) — factor the shared `upload_mesh` /
`upload_texture` body so both worker seams call one implementation rather than a duplicated copy.

### 3.2 CRITICAL correctness — one shared submit queue (highest priority)

`GpuQueue` wraps the raw `vk::Queue` in an `Arc<Mutex<…>>`; only submitters sharing **one** `Arc<Mutex>`
serialize. Today `HostLayer::ensure_uploader` (`layer.rs`, `ensure_uploader`) and
`HostLayer::start_thumbnail_worker` (`layer.rs`, `start_thumbnail_worker`) each mint a **separate**
`GpuQueue::new(...)` around the same raw queue — they are NOT mutually excluded. Adding a third submitter
(the load worker) makes this load-bearing.

Thread **one** shared `GpuQueue` `Arc` through: the host uploader (`ensure_uploader`), the thumbnail
worker's `Uploader` (`start_thumbnail_worker`), and the new load worker's `Uploader`. Verify the
frame-loop present path does not concurrently submit on the same raw `vk::Queue` without that mutex. Do
this share **explicitly in this phase** — do not defer it.

### 3.3 SDF bake on the worker

`upload_mesh` runs a GPU jump-flood compute bake when `sdf = Some(..)`
(`engine/crates/assets/src/upload.rs`, the `upload_mesh` bake path). The thumbnail worker only ever
uploads `sdf = None`, so worker + bake is currently unexercised. Confirm:

- the bake uses the `Uploader`'s own command pool (not a frame pool);
- the `assets/cache` SDF sidecar read/write is concurrency-safe against any sync path;
- it does not race frame-loop compute — it is serialized by the shared `GpuQueue` mutex from §3.2.

---

## 4. `AssetServer` additions — `engine/crates/assets/src/lib.rs`

Beside `thumbnail_worker` (`AssetServer.thumbnail_worker`):

- Field `pub load_worker: Option<AssetLoadWorker>`.
- `start_asset_load_worker(&mut self, gpu: Box<dyn GpuUploader + Send>)` — mirror
  `start_thumbnail_worker`.
- `enqueue_residency(&mut self, jobs: Vec<LoadJob>)` — set `total`, dedup by `id`, wake the `Condvar`.
- `drain_load_completions(&mut self) -> bool` — fold `mesh_handback` / `texture_handback` into
  `mesh_by_uuid` / `texture_by_uuid`, bump `resident`, clear the handback vecs; return whether anything
  changed (the loop requests a redraw when it did).
- `load_progress(&self) -> (u32, u32, u32)` = `(resident, failed, total)`.
- `clear_load_queue(&mut self)` — abandon `queue` + `in_flight` + `failed` + any un-drained handbacks;
  reset `total` / `resident`. Call it from `clear_asset_caches` right after `clear_thumbnail_queue`.
- `stop_asset_load_worker(&mut self)` — set `stop`, notify, join the handle (mirror the thumbnail stop).

`clear_asset_caches` (`engine/crates/assets/src/lib.rs`, `AssetServer::clear_asset_caches`) calls
`self.clear_load_queue()` so a project switch abandons in-flight residency under the already-idle GPU —
the same UAF-safe discipline `clear_thumbnail_queue` follows.

---

## 5. Resolve-or-skip on the draw path — `engine/crates/assets/src/load.rs`

The lazy resolve in `render_scene` must distinguish **not-yet-resident** (transient — no marker, skip
this frame) from **worker-reported failure** (permanent `None`). Today `resolve_mesh` /
`resolve_texture` (`load.rs`, `AssetServer::resolve_mesh` / `AssetServer::resolve_texture`) insert a
negative cache marker on a cold miss — for async that would permanently skip a pending asset.

Change: on a cache miss, check the load worker's `failed` set:

- if `id` is in `failed` → return `None` **and** mark negative (permanent skip);
- otherwise return `None` **without** a marker — it is streaming (a missing mesh already skips this
  frame; a missing texture already falls back to default-white).

Since this phase prefetches the whole residency set at the install step, the draw path no longer
*drives* enqueue — it only reads the filling caches. Remove any cold-miss enqueue-on-resolve code paths
that this makes dead (NO-LEGACY).

---

## 6. The orchestrator — `ProjectLoader` on `HostLayer` (`engine/crates/host/src/layer.rs`)

```rust
enum LoaderState {
    Idle,
    Parsing(ProjectDocWorker),
    Streaming,
    Warming,
}

pub struct ProjectLoader {
    state: LoaderState,
}
```

Ownership: `HostLayer` owns `project_loader: ProjectLoader` (default `Idle`). The residency
`AssetLoadWorker` is started in `HostLayer::on_attach` alongside `start_thumbnail_worker` (the
`self.start_thumbnail_worker(renderer)` call site in `layer.rs`), building a `WorkerUploaderGpu` on the
shared `GpuQueue` from §3.2.

`ProjectLoader::advance(&mut self, renderer, scene_edit, assets, app)` is called each frame in
`HostLayer::on_update` (`layer.rs`, `HostLayer::on_update`) **right after** `poll_control` and the
existing `self.assets.drain_thumbnail_completions()` call, and **before** the session/play spine. It
performs **one bounded step per call**:

1. **`Idle`:**
   - If `scene_edit.project_cancel` is set → take-and-clear it, no-op.
   - Else if `scene_edit.project_load_inbox.take()` is `Some(req)` → set
     `scene_edit.project_phase = ProjectPhase::Loading`, bump `project_load.version`, spawn a
     `ProjectDocWorker` (Open / New / Reload map to the right worker inputs; New builds an empty
     `LoadedDoc` in-worker per §2), transition to `Parsing`.

2. **`Parsing(worker)`:**
   - Copy `worker.progress_snapshot()` into `scene_edit.project_load` (stage / done / total; derive a
     `label` from the stage + `current_item`).
   - If `scene_edit.project_cancel` → take-and-clear, drop the handle (its `Result` is discarded on
     join), go `Idle`, set phase `Unloaded`, reset `project_load`.
   - If `worker.handle.is_finished()` → `join()`:
     - **`Ok(doc)`** → the **install step** (main thread, one bounded step):
       `renderer.wait_gpu_idle()` → `assets.clear_asset_caches()` → swap `scene_edit.scene = doc.scene`,
       `assets.catalog = doc.catalog` → `assets.set_asset_root(...)` → apply `doc.render_settings`
       (the existing `apply_render_settings` path) → apply project info + sidecar via
       `apply_loaded_project` refactored to take a `LoadedDoc` (§7) → `assets.enqueue_residency(
       doc.residency_jobs)` → set `project_load.stage = Assets` → transition to `Streaming`.
       `wait_gpu_idle` is bounded by frames-in-flight (a few ms) — the existing reload/teardown
       discipline, acceptable for one step.
     - **`Err(e)`** → phase `Failed`, `project_load.error = e.to_string()`, `stage = Failed`, `Idle`.

3. **`Streaming`:**
   - `let changed = assets.drain_load_completions();`
   - update `project_load` from `assets.load_progress()`: `done/total = resident/total`,
     `stage = Assets`, `label = "Loading assets N/M"`, `current_item`.
   - if `changed` → `app.redraw.request_redraw()`.
   - on cancel → take-and-clear, `assets.clear_load_queue()`, phase `Unloaded`, reset, `Idle`.
   - when `resident + failed == total` → `stage = Skybox`, transition to `Warming`.

4. **`Warming`:**
   - poll `renderer.warmup_state()` (§6.1): while `env_bake` set `stage = Skybox`; else while `accel`
     set `stage = Accel`; when both clear → phase `Ready`, `stage = Ready`,
     `app.redraw.request_redraw()` (seed the completion paint — the same call `on_attach` uses), `Idle`.

**Every transition bumps `scene_edit.project_load.version`.**

### 6.1 Renderer hook — `warmup_state` on `ControlRenderer`

Add to the `ControlRenderer` trait (`engine/crates/control/src/registry.rs`, `trait ControlRenderer`)
and its host impl:

```rust
fn warmup_state(&self) -> WarmupState;

pub struct WarmupState {
    pub env_bake: bool,   // IBL env bake pending
    pub accel: bool,      // RT TLAS / first-frame priming pending
}
```

Back `env_bake` with the renderer's IBL re-bake gate (`engine/crates/rendering/src/ibl.rs`,
`Ibl::rebake_pending`) and `accel` with the TLAS build-pending flag. This lets `Warming` hold on real
GPU completion, not a fixed frame count.

### 6.2 Reactive hold — keep rendering while loading

Fold a project-loading reason into `HostLayer::render_activity_reasons` (`layer.rs`,
`render_activity_reasons`), gated on
`scene_edit.project_phase == ProjectPhase::Loading || assets.load_progress().2 > assets.load_progress().0`
(i.e. residency still pending), so the `RedrawController` holds continuous render while the load streams
and idles once `Ready`.

---

## 7. Route bootstrap + commands through the loader seam (delete the synchronous path)

- **Bootstrap** — `bootstrap_project_from_env` (`engine/crates/control/src/commands_asset.rs`,
  `bootstrap_project_from_env`), called from `HostLayer::bootstrap_project` (`layer.rs`,
  `bootstrap_project`) in `on_attach`: instead of calling `load_project` inline, set
  `scene_edit.project_load_inbox = Some(ProjectLoadRequest::Open(path))` (or `New`), set phase
  `Loading`, and request one redraw. The loop's `advance` picks it up on frame one; startup no longer
  blocks the first frame.

- **Commands** — `open-project`, `new-project`, and `load-project` / `reload-project` via
  `load_project_into` (`commands_asset.rs`, `load_project_into`): keep the pre-flight guards
  (play-state / preview / path validation), then set
  `scene_edit.project_load_inbox = Some(req)`, phase `Loading`, and return an **immediate ack**. In this
  phase return a minimal `{}` ack; Phase 3 changes the return type to the initial `ProjectStatusDto`
  snapshot (do not build a dual sync+async return — that is Phase 3's cutover). Delete the synchronous
  `load_project` / `create_project` call from the handler body.

- **Decompose** `AssetServer::load_project` / `AssetServer::create_project`: their logic moves into the
  doc-worker body (§2) and the main-thread install step (§6.2). Remove their public synchronous
  signatures — no caller invokes them synchronously anymore.

- **`apply_loaded_project`** (`commands_asset.rs`, `apply_loaded_project`) is refactored to install from
  a `&LoadedDoc` on the main thread (called by the loader's install step), not from a synchronous load
  return. It still sets `project_phase = Ready` via `apply_project_info` on success (the Phase-1
  hookup).

---

## 8. Cancel

The `scene_edit.project_cancel` flag (set by the `cancel-load` command added in Phase 3; the field lands
here) is read by `advance`:

- `Parsing` → drop the doc handle (its result is discarded on join);
- `Streaming` / `Warming` → `assets.clear_load_queue()`;
- all → phase `Unloaded`, `project_load` reset, `Idle`, version bumped.

Because a switch/cancel path has already idled the GPU (or the residency handbacks are dropped under the
idle at the next `clear_asset_caches`), dropping un-drained `Arc`s is UAF-safe — the same discipline as
`clear_thumbnail_queue`.

---

## 9. Teardown ordering

Add `TeardownStep::LoadWorkerJoined` to the `TeardownStep` enum (`layer.rs`, `enum TeardownStep`) and
push it in `HostLayer::teardown_recording` (`layer.rs`, `teardown_recording`) sequenced **first** —
before `wait_gpu_idle` / device drop — joining the residency `AssetLoadWorker` and, if a doc worker is
in flight, joining it too. This mirrors how `TeardownStep::WorkerJoined` pins the thumbnail worker.
Update the ordering test `teardown_unsubscribes_and_drops_in_order` (in `layer.rs`) in the same change so
the expected sequence lists `LoadWorkerJoined` first.

---

## 10. `sa` CLI + docs

- The pollable status surface + `cancel-load` command land in **Phase 3**; this phase adds no new
  control command. `sa get-project` still reports `loaded` (now driven by `project_ready()`).
- Docs: the project-load lifecycle concept page is authored in **Phase 3** (which adds the wire surface).
  This phase is engine-internal plumbing.

---

## NO-LEGACY checklist (grep-verify at the phase boundary)

- Zero synchronous callers of `AssetServer::load_project` / `AssetServer::create_project` remain; their
  public signatures are gone.
- Zero inline handler loads: `open-project` / `new-project` / `load_project_into` / `bootstrap_project_from_env`
  only seed the loader inbox.
- No cold-miss enqueue-on-resolve path survives in `load.rs`; the draw path only reads the caches.
- Exactly one shared `GpuQueue` `Arc` feeds the host uploader + thumbnail worker + load worker (grep the
  `GpuQueue::new` call sites — there is one construction, threaded to all three).
- `scan_asset_references` has a single definition (moved to `saffron-assets` if the crate ordering
  required it; no duplicate copy).

## Milestone gate (run at this phase boundary, do not defer)

- `just engine` then `just prepare-for-commit` clean — `clippy -D warnings`, `cargo fmt`.
- `just e2e` green (see the responsiveness test below).
- Leave all changes **unstaged**; the user stages/commits (git is read-only by default).

## Verification — the two proofs that define "done"

During a headless **thick** project load, both must hold; if either regresses, the load is still
stalling the main loop and the phase is not done:

1. **Socket stays responsive.** Boot with `just run-engine-headless` a thick test project while a driver
   sends `sa ping` every ~50 ms across the whole load — every `ping` replies within one
   `IDLE_POLL_INTERVAL` (no multi-second gap), proving the drain never stalls.
2. **Frames keep publishing.** Confirm the shared-memory seqlock sequence advances throughout the load
   (the present-only host keeps publishing frames), not just before and after.

Plus:

- `just e2e`: a test opens a project, immediately polls the allow-listed `get-project` repeatedly and
  observes `loaded:false` → (streaming) → `true`, while a non-allow-listed command (e.g. `add-entity`)
  returns `{ code: "busy-loading" }` during `Loading`. Validation-clean log.
- Grep: no `project_loaded`, no synchronous `load_project` / `create_project` caller remains.
