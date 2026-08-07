//! The off-thread project doc loader — the CPU/IO half of a non-blocking project load.
//!
//! [`ProjectDocWorker`] runs the heavy, blocking part of bringing a project up on its own
//! thread so the host main loop keeps draining the control socket and publishing frames: reading
//! and parsing `project.json`, then the cold catalog disk reconcile (walking + hashing every
//! asset file — the dominant, variable cost). It builds a **fresh, owned** [`AssetCatalog`] and
//! returns a [`LoadedDoc`] the main thread installs (idle + swap + `scene_from_json`), since the
//! scene deserialize needs the main-thread `ComponentRegistry` and the GPU swap must be idle.
//!
//! The host has exactly one project bring-up path: bootstrap and every lifecycle command seed a
//! [`ProjectDocWorker`], never a synchronous load. The synchronous `AssetServer::load_project` /
//! `create_project` remain for `saffron-player` (the standalone exported game boots blocking — it
//! has no control plane to keep responsive), and both share the catalog scan via
//! [`resolve_catalog_from_disk`](crate::scan::resolve_catalog_from_disk).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use saffron_json::{Value, json_u64_or, parse_json};
use saffron_scene::AssetCatalog;

use crate::catalog::{catalog_folders_from_json, catalog_from_json};
use crate::error::{Error, Result};
use crate::project::{
    NewProject, PROJECT_VERSION, ProjectInfo, ProjectSidecar, default_display_name,
    ensure_script_library, ensure_script_src, project_info_from_path, project_json_path,
    project_userdata_root, valid_project_name,
};
use crate::scan::resolve_catalog_from_disk;

/// The doc-worker stage, reported through [`DocProgress`]. The host maps it onto the wider
/// `saffron_sceneedit::BootStage` (which this crate cannot name — it sits below `saffron-sceneedit`
/// in the DAG). Only the two off-thread stages live here; `Scene`/`Install`/`Assets` are driven by
/// the main-thread loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocStage {
    /// Reading + parsing `project.json`.
    #[default]
    Manifest,
    /// Reconciling the asset catalog against disk (the cold scan; determinate `done`/`total`).
    Catalog,
}

/// The doc worker's per-frame progress snapshot, cloned under the lock by the loader.
#[derive(Debug, Clone, Default)]
pub struct DocProgress {
    /// The current off-thread stage.
    pub stage: DocStage,
    /// Files scanned so far (`Catalog` stage).
    pub done: u32,
    /// Total files to scan (`Catalog` stage; `0` while indeterminate).
    pub total: u32,
    /// The file/asset currently being scanned, or empty.
    pub current_item: String,
}

/// The staged result of the doc phase, installed on the main thread by the loader.
pub struct LoadedDoc {
    /// The scene document (deserialized on the main thread via the `ComponentRegistry`).
    pub scene_json: Value,
    /// The freshly reconciled catalog, swapped in at install.
    pub catalog: AssetCatalog,
    /// The resolved project identity + paths.
    pub project_info: ProjectInfo,
    /// The opaque editor camera / overlays / stores blocks for `saffron-sceneedit` to apply.
    pub sidecar: ProjectSidecar,
    /// The saved `renderSettings` block (applied to the live renderer at install), or `None`.
    pub render_settings: Option<Value>,
    /// The project's `assets/` root, set on the live `AssetServer` at install.
    pub asset_root: PathBuf,
    /// A freshly created project must save its `project.json` at install (it needs the live
    /// renderer's `renderSettings`, unavailable off-thread).
    pub save_after_install: bool,
}

/// What the doc worker loads: an existing project (by selection path/name) or a fresh one.
pub enum LoadInput {
    /// Open the project at `project.json` resolved from the selection.
    Open(String),
    /// Create a fresh, empty project from the spec.
    New(NewProject),
}

/// The off-thread project doc worker: a [`JoinHandle`] yielding the [`LoadedDoc`], plus a shared
/// [`DocProgress`] the loader polls each frame.
pub struct ProjectDocWorker {
    handle: Option<JoinHandle<Result<LoadedDoc>>>,
    progress: Arc<Mutex<DocProgress>>,
}

impl ProjectDocWorker {
    /// Spawns the worker for `input`, passing the host-supplied `sa_lua_defs` for `library/sa.lua`.
    #[must_use]
    pub fn spawn(input: LoadInput, sa_lua_defs: String) -> Self {
        let progress = Arc::new(Mutex::new(DocProgress::default()));
        let worker_progress = Arc::clone(&progress);
        let handle = std::thread::Builder::new()
            .name("project-doc-worker".to_owned())
            .spawn(move || run(input, &sa_lua_defs, &worker_progress))
            .expect("spawn project doc worker");
        Self {
            handle: Some(handle),
            progress,
        }
    }

    /// The current progress snapshot (cloned under the lock).
    #[must_use]
    pub fn progress_snapshot(&self) -> DocProgress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Whether the worker thread has finished (its result is ready to join).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Joins the worker, returning its [`LoadedDoc`] result. A panicked worker maps to an error.
    pub fn join(mut self) -> Result<LoadedDoc> {
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err(Error::Io("project doc worker panicked".to_owned()))),
            None => Err(Error::Io("project doc worker already joined".to_owned())),
        }
    }
}

/// Sets the shared progress under the lock.
fn set_progress(
    shared: &Arc<Mutex<DocProgress>>,
    stage: DocStage,
    done: u32,
    total: u32,
    item: &str,
) {
    if let Ok(mut p) = shared.lock() {
        p.stage = stage;
        p.done = done;
        p.total = total;
        p.current_item = item.to_owned();
    }
}

/// The worker body: dispatch to the open / new loaders.
fn run(
    input: LoadInput,
    sa_lua_defs: &str,
    progress: &Arc<Mutex<DocProgress>>,
) -> Result<LoadedDoc> {
    match input {
        LoadInput::Open(selection) => load_open(&selection, sa_lua_defs, progress),
        LoadInput::New(spec) => load_new(&spec, sa_lua_defs),
    }
}

/// Loads an existing project's doc off-thread: read + parse + version-gate (`Manifest`), then the
/// catalog reconcile (`Catalog`). The GPU idle, cache clear, and `scene_from_json` stay on the main
/// thread.
fn load_open(
    selection: &str,
    sa_lua_defs: &str,
    progress: &Arc<Mutex<DocProgress>>,
) -> Result<LoadedDoc> {
    set_progress(progress, DocStage::Manifest, 0, 0, "");
    let manifest_started = std::time::Instant::now();
    let path = project_json_path(selection);
    let text = std::fs::read_to_string(&path)
        .map_err(|err| Error::Io(format!("cannot open '{}': {err}", path.display())))?;
    let doc = parse_json(&text)?;
    if !doc.is_object() {
        return Err(Error::Json(saffron_json::Error::Parse(format!(
            "'{}': not a JSON object",
            path.display()
        ))));
    }
    let version = i64::try_from(json_u64_or(&doc, "version", 0)).unwrap_or(i64::MAX);
    if version != PROJECT_VERSION {
        return Err(Error::BadProjectVersion {
            found: version,
            expected: PROJECT_VERSION,
        });
    }

    let project_info = project_info_from_path(&path, &doc);
    let root = PathBuf::from(&project_info.root);
    ensure_script_src(&root);
    ensure_script_library(&root, sa_lua_defs);

    let empty_array = Value::Array(Vec::new());
    let mut seed = AssetCatalog::default();
    catalog_from_json(&mut seed, doc.get("assets").unwrap_or(&empty_array));
    catalog_folders_from_json(&mut seed, doc.get("assetFolders").unwrap_or(&empty_array));

    let manifest_ms = manifest_started.elapsed().as_millis();
    let catalog_started = std::time::Instant::now();
    let asset_root = root.join("assets");
    let (catalog, _delta) =
        resolve_catalog_from_disk(&asset_root, &seed, &mut |done, total, item| {
            set_progress(progress, DocStage::Catalog, done, total, item);
        });
    tracing::info!(
        "project doc ready — manifest {manifest_ms} ms, catalog {} ms ({} assets)",
        catalog_started.elapsed().as_millis(),
        catalog.entries.len()
    );

    let sidecar = ProjectSidecar {
        editor_camera: doc.get("editorCamera").cloned().unwrap_or(Value::Null),
        debug_overlays: doc.get("debugOverlays").cloned().unwrap_or(Value::Null),
        stores: doc.get("stores").cloned().unwrap_or(Value::Null),
    };
    let scene_json = doc
        .get("scene")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    Ok(LoadedDoc {
        scene_json,
        catalog,
        project_info,
        sidecar,
        render_settings: doc.get("renderSettings").cloned(),
        asset_root,
        save_after_install: false,
    })
}

/// Builds a fresh, empty project's doc off-thread. The `project.json` save happens at install (it
/// needs the live renderer's `renderSettings`), so this only ensures the script scaffold + the
/// identity; the install swaps in the empty scene/catalog and saves.
fn load_new(spec: &NewProject, sa_lua_defs: &str) -> Result<LoadedDoc> {
    if !valid_project_name(&spec.name) {
        return Err(Error::InvalidProjectName(spec.name.clone()));
    }
    let root = if spec.root.is_empty() {
        PathBuf::from(project_userdata_root()).join(&spec.name)
    } else {
        PathBuf::from(&spec.root)
    };
    let project_info = ProjectInfo {
        loaded: true,
        root: root.to_string_lossy().into_owned(),
        path: root.join("project.json").to_string_lossy().into_owned(),
        name: spec.name.clone(),
        display_name: if spec.display_name.is_empty() {
            default_display_name(&spec.name)
        } else {
            spec.display_name.clone()
        },
    };
    ensure_script_src(&root);
    ensure_script_library(&root, sa_lua_defs);

    Ok(LoadedDoc {
        scene_json: Value::Object(serde_json::Map::new()),
        catalog: AssetCatalog::default(),
        project_info,
        sidecar: ProjectSidecar::default(),
        render_settings: None,
        asset_root: root.join("assets"),
        save_after_install: true,
    })
}
