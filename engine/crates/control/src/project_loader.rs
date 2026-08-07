//! The non-blocking project loader: a once-per-frame state machine that brings a project up
//! without stalling the host main loop.
//!
//! A load is seeded through [`SceneEditContext::project_load_inbox`]. Each frame
//! [`ProjectLoader::advance`] takes one bounded step — spawn the off-thread [`ProjectDocWorker`]
//! (read, parse, cold catalog scan), install the returned doc on the main thread, then stream GPU
//! residency a few assets per frame — so between steps the loop keeps draining the control socket
//! and publishing frames. `Loading` therefore has real duration, and the dispatch gate discards
//! non-allow-listed commands for its whole span rather than queuing them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use saffron_assets::{
    AssetServer, LoadInput, LoadedDoc, NewProject, ProjectDocWorker, ProjectSidecar, WarmItem,
};
use saffron_core::Uuid;
use saffron_scene::{Entity, MaterialSet, Mesh, Scene, ScriptInputState, seed_starter_scene};
use saffron_sceneedit::{
    BootStage, ProjectLoadRequest, ProjectPhase, SceneEditContext, debug_overlays_from_json,
};

use crate::commands_asset::RendererProjectHost;
use crate::registry::ControlRenderer;

/// The per-frame residency time budget: warm assets until this much wall time has elapsed this
/// frame, then yield so the loop drains the control socket (progress polls + `cancel-load`) before
/// the next batch. Small assets pack many into one frame; a single large decode/upload advances the
/// bar one asset at a time — so `Loading assets n/m` moves smoothly instead of jumping in blocks.
const RESIDENCY_FRAME_BUDGET: Duration = Duration::from_millis(6);

/// The GPU residency prefetch: the scene-referenced warm items, and the running resident
/// count for the determinate `Assets` progress.
struct Residency {
    queue: VecDeque<WarmItem>,
    total: u32,
    done: u32,
    started: Instant,
}

/// The loader's state. `Idle` waits for an inbox request; `Parsing` runs the off-thread doc
/// worker; `Streaming` uploads residency a few assets per frame.
enum LoaderState {
    Idle,
    Parsing(ProjectDocWorker),
    Streaming(Residency),
}

/// The once-per-frame project loader, owned by the [`ControlContext`](crate::ControlContext).
pub struct ProjectLoader {
    state: LoaderState,
    load_started: Option<Instant>,
}

impl Default for ProjectLoader {
    fn default() -> Self {
        Self {
            state: LoaderState::Idle,
            load_started: None,
        }
    }
}

impl ProjectLoader {
    /// Advances the load by one bounded step against the live borrows. Returns `true` when it did
    /// work that changed the rendered/reported state (the host requests a redraw), so a static
    /// idle loader leaves the GPU quiet.
    pub fn advance(
        &mut self,
        renderer: &mut dyn ControlRenderer,
        scene_edit: &mut SceneEditContext,
        assets: &mut AssetServer,
    ) -> bool {
        match &mut self.state {
            LoaderState::Idle => self.advance_idle(renderer, scene_edit),
            LoaderState::Parsing(_) => self.advance_parsing(renderer, scene_edit, assets),
            LoaderState::Streaming(_) => self.advance_streaming(renderer, scene_edit, assets),
        }
    }

    /// `Idle`: pick up a cancel (no-op) or an inbox request → spawn the doc worker.
    fn advance_idle(
        &mut self,
        renderer: &mut dyn ControlRenderer,
        scene_edit: &mut SceneEditContext,
    ) -> bool {
        if scene_edit.project_cancel {
            scene_edit.project_cancel = false;
            return false;
        }
        let Some(request) = scene_edit.project_load_inbox.take() else {
            return false;
        };
        let input = match request {
            ProjectLoadRequest::Open(path) => LoadInput::Open(path),
            ProjectLoadRequest::Reload => LoadInput::Open(scene_edit.project_path.clone()),
            ProjectLoadRequest::New(spec) => LoadInput::New(NewProject {
                name: spec.name,
                display_name: spec.display_name,
                root: spec.root,
            }),
        };
        scene_edit.project_phase = ProjectPhase::Loading;
        set_stage(scene_edit, BootStage::Manifest, 0, 0, "Reading project", "");
        let defs = renderer.sa_lua_defs();
        self.load_started = Some(Instant::now());
        self.state = LoaderState::Parsing(ProjectDocWorker::spawn(input, defs));
        true
    }

    /// `Parsing`: mirror the worker's progress; on completion install the doc (Ok) or fail (Err).
    fn advance_parsing(
        &mut self,
        renderer: &mut dyn ControlRenderer,
        scene_edit: &mut SceneEditContext,
        assets: &mut AssetServer,
    ) -> bool {
        let LoaderState::Parsing(worker) = &mut self.state else {
            return false;
        };
        if scene_edit.project_cancel {
            scene_edit.project_cancel = false;
            self.state = LoaderState::Idle;
            cancel_reset(scene_edit);
            return true;
        }
        if !worker.is_finished() {
            let p = worker.progress_snapshot();
            let (stage, label) = doc_stage_label(p.stage, p.done, p.total);
            set_stage(scene_edit, stage, p.done, p.total, &label, &p.current_item);
            return true;
        }
        let LoaderState::Parsing(worker) = std::mem::replace(&mut self.state, LoaderState::Idle)
        else {
            return false;
        };
        match worker.join() {
            Ok(doc) => {
                let residency = install_doc(renderer, scene_edit, assets, doc);
                self.state = LoaderState::Streaming(residency);
                true
            }
            Err(err) => {
                fail(scene_edit, &err.to_string());
                true
            }
        }
    }

    /// `Streaming`: warm up to [`RESIDENCY_PER_FRAME`] assets; finish → `Ready`.
    fn advance_streaming(
        &mut self,
        renderer: &mut dyn ControlRenderer,
        scene_edit: &mut SceneEditContext,
        assets: &mut AssetServer,
    ) -> bool {
        let LoaderState::Streaming(res) = &mut self.state else {
            return false;
        };
        if scene_edit.project_cancel {
            scene_edit.project_cancel = false;
            self.state = LoaderState::Idle;
            cancel_reset(scene_edit);
            return true;
        }

        // Warm assets until the frame budget is spent (or the queue drains), then yield. The name
        // of the last-warmed asset rides along as the progress `current_item`.
        let start = Instant::now();
        let mut current = String::new();
        renderer.with_gpu_uploader(&mut |gpu| {
            while let Some(item) = res.queue.pop_front() {
                assets.warm(gpu, &item);
                res.done += 1;
                current = warm_item_name(assets, &item);
                if start.elapsed() >= RESIDENCY_FRAME_BUDGET {
                    break;
                }
            }
        });

        if res.queue.is_empty() {
            let total = res.total;
            tracing::info!(
                "project ready — {total} assets resident in {} ms (load total {} ms)",
                res.started.elapsed().as_millis(),
                self.load_started
                    .take()
                    .map_or(0, |t| t.elapsed().as_millis())
            );
            // The load's slow first renders are about to happen; drop the load-transition frames
            // from the perf HUD so it grades steady state, not the cold-pipeline warm-up.
            renderer.reset_frame_telemetry();
            set_stage(scene_edit, BootStage::Ready, total, total, "Ready", "");
            scene_edit.project_phase = ProjectPhase::Ready;
            self.state = LoaderState::Idle;
        } else {
            let (done, total) = (res.done, res.total);
            set_stage(
                scene_edit,
                BootStage::Assets,
                done,
                total,
                &format!("Loading assets {done}/{total}"),
                &current,
            );
        }
        true
    }
}

/// Writes a fresh progress snapshot, bumping the monotonic version so the editor poll dedups.
fn set_stage(
    scene_edit: &mut SceneEditContext,
    stage: BootStage,
    done: u32,
    total: u32,
    label: &str,
    item: &str,
) {
    let version = scene_edit.project_load.version.wrapping_add(1);
    scene_edit.project_load = saffron_sceneedit::ProjectLoadProgress {
        stage,
        done,
        total,
        label: label.to_owned(),
        current_item: item.to_owned(),
        error: String::new(),
        version,
    };
}

/// Resets progress + phase to `Unloaded` after a cancel.
fn cancel_reset(scene_edit: &mut SceneEditContext) {
    scene_edit.project_phase = ProjectPhase::Unloaded;
    set_stage(scene_edit, BootStage::Manifest, 0, 0, "", "");
}

/// Sets the terminal `Failed` state carrying the message.
fn fail(scene_edit: &mut SceneEditContext, message: &str) {
    scene_edit.project_phase = ProjectPhase::Failed;
    let version = scene_edit.project_load.version.wrapping_add(1);
    scene_edit.project_load = saffron_sceneedit::ProjectLoadProgress {
        stage: BootStage::Failed,
        done: 0,
        total: 0,
        label: "Load failed".to_owned(),
        current_item: String::new(),
        error: message.to_owned(),
        version,
    };
}

/// Maps the assets-crate `DocStage` onto the wider `BootStage` + a human label.
fn doc_stage_label(stage: saffron_assets::DocStage, done: u32, total: u32) -> (BootStage, String) {
    match stage {
        saffron_assets::DocStage::Manifest => (BootStage::Manifest, "Reading project".to_owned()),
        saffron_assets::DocStage::Catalog => (
            BootStage::Catalog,
            format!("Scanning assets {done}/{total}"),
        ),
    }
}

/// The main-thread install: idle the GPU, swap in the loaded scene + catalog, apply render
/// settings + sidecar, and (for a fresh project) save `project.json`. Returns the residency
/// prefetch for the `Streaming` stage. Leaves the phase `Loading` — the loader flips it to
/// `Ready` only once residency drains.
fn install_doc(
    renderer: &mut dyn ControlRenderer,
    scene_edit: &mut SceneEditContext,
    assets: &mut AssetServer,
    doc: LoadedDoc,
) -> Residency {
    set_stage(scene_edit, BootStage::Scene, 0, 0, "Loading scene", "");

    // Idle-before-clear: the GPU must be quiet before the caches' `Arc`s drop (an in-flight frame
    // may still read one) — the load-order UAF guard.
    let idle_started = Instant::now();
    renderer.wait_gpu_idle();
    assets.clear_asset_caches();
    assets.replace_catalog(doc.catalog);
    assets.set_asset_root(&doc.asset_root);
    let idle_ms = idle_started.elapsed().as_millis();

    set_stage(
        scene_edit,
        BootStage::Install,
        0,
        0,
        "Installing project",
        "",
    );
    // A freshly created project carries an empty scene doc (`{}`); seed the shared starter
    // scene (a framed camera and a sun) rather than running `scene_from_json`, which rejects a
    // versionless object — the save below persists the seed into the new project. An opened
    // project's saved scene block always carries a version and is deserialized as-is, so a
    // deliberately-emptied project is never re-seeded.
    let scene_started = Instant::now();
    let mut scene = Scene::default();
    if doc
        .scene_json
        .as_object()
        .is_some_and(|obj| !obj.is_empty())
    {
        if let Err(err) = scene.scene_from_json(&scene_edit.registry, &doc.scene_json) {
            tracing::error!("scene load: {err}");
        }
    } else if doc.save_after_install {
        seed_starter_scene(&mut scene);
    }
    scene_edit.scene = scene;
    tracing::info!(
        "project installed — gpu idle + cache swap {idle_ms} ms, scene {} ms",
        scene_started.elapsed().as_millis()
    );

    if let Some(settings) = &doc.render_settings {
        renderer.apply_render_settings(settings);
        // Rebind the persisted lens-dirt mask: the renderer's `apply_render_settings` applies every
        // bloom field but the mask asset, which only the asset catalog here can resolve to a live
        // texture. A `0`/absent id (or a dangling one) clears it back to the white fallback.
        let dirt_id = settings
            .get("bloomDirtTexture")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let mut resolved = None;
        if dirt_id != 0 {
            let assets = &mut *assets;
            let core_id = Uuid(dirt_id);
            renderer.with_gpu_uploader(&mut |gpu| {
                resolved = assets.load_texture_asset(gpu, core_id);
            });
        }
        renderer.set_bloom_dirt_texture(dirt_id, resolved);
    }

    // Project identity + sidecar (phase stays `Loading`; the loader owns the flip to `Ready`).
    scene_edit.project_root = doc.project_info.root.clone();
    scene_edit.project_path = doc.project_info.path.clone();
    scene_edit.project_name = doc.project_info.name.clone();
    scene_edit.project_display_name = doc.project_info.display_name.clone();
    scene_edit.scene_path = doc.project_info.path.clone();
    scene_edit.camera.from_json(&doc.sidecar.editor_camera);
    debug_overlays_from_json(&mut scene_edit.debug_overlays, &doc.sidecar.debug_overlays);
    scene_edit.stores = doc.sidecar.stores.clone();
    scene_edit.scene_version += 1;
    scene_edit.script_input = ScriptInputState::default();
    scene_edit.set_selection(Entity::NULL);

    if doc.save_after_install {
        let host = RendererProjectHost { renderer };
        let path = doc.project_info.path.clone();
        if let Err(err) = assets.save_project(
            &host,
            &scene_edit.registry,
            &mut scene_edit.scene,
            &doc.project_info,
            &path,
            &ProjectSidecar::default(),
        ) {
            tracing::error!("save new project: {err}");
        }
    }

    let queue = scene_residency_items(&mut scene_edit.scene);
    let total = queue.len() as u32;
    set_stage(
        scene_edit,
        BootStage::Assets,
        0,
        total,
        &format!("Loading assets 0/{total}"),
        "",
    );
    Residency {
        queue: queue.into_iter().collect(),
        total,
        done: 0,
        started: Instant::now(),
    }
}

/// The distinct warm items the scene references (the residency prefetch set): every
/// `Mesh.mesh`, every `MaterialSet` slot (whose resolve loads the `.smat` and each texture it
/// binds through its canonical role), and the environment sky panorama — so the draw path
/// after `Ready` finds every scene-referenced resource already resident.
fn scene_residency_items(scene: &mut Scene) -> Vec<WarmItem> {
    let mut items: Vec<WarmItem> = Vec::new();
    let mut push = |item: WarmItem| {
        if !items.contains(&item) {
            items.push(item);
        }
    };
    scene.for_each::<&Mesh, _>(|_, mesh| {
        if mesh.mesh.value() != 0 {
            push(WarmItem::Mesh(mesh.mesh));
        }
    });
    scene.for_each::<&MaterialSet, _>(|_, set| {
        for slot in &set.slots {
            // A default-material slot with no overrides binds no textures.
            let overridden = slot.overrides.as_object().is_some_and(|o| !o.is_empty());
            if slot.material.value() != 0 || overridden {
                push(WarmItem::MaterialSlot {
                    material: slot.material,
                    overrides: slot.overrides.clone(),
                });
            }
        }
    });
    let sky = scene.environment.sky_texture;
    if sky.value() != 0 {
        push(WarmItem::Texture(sky));
    }
    items
}

/// The progress `current_item` label for a warm item: the catalog name of the mesh, texture,
/// or referenced material.
fn warm_item_name(assets: &AssetServer, item: &WarmItem) -> String {
    let id = match item {
        WarmItem::Mesh(id) | WarmItem::Texture(id) => *id,
        WarmItem::MaterialSlot { material, .. } => *material,
    };
    assets
        .catalog()
        .find(id)
        .map_or_else(String::new, |entry| entry.name.clone())
}
