//! `project.json` save / load / create and the path/name helpers.
//!
//! The project document bundles the asset catalog, the asset folders, the scene
//! (via `saffron-scene`'s `scene_to_json`), and a `renderSettings` block, plus the
//! optional `editorCamera` / `debugOverlays` blocks. The camera + overlay blocks
//! belong to `saffron-sceneedit`, so they ride through here as opaque
//! [`serde_json::Value`]s round-tripped to the caller (the host) — this module never
//! owns or interprets them.
//!
//! # The load order is load-bearing (the UAF guard)
//!
//! [`AssetServer::load_project`] keeps an exact ordered sequence: parse → version-gate →
//! `wait_gpu_idle` → clear the worker queue + the GPU caches → set the asset root → ensure the
//! script `src/` + library → load the catalog from the doc → reconcile against disk (the filesystem
//! is the source of truth, a cold scan on a cache miss) → apply render settings → pull
//! camera/overlays → `scene_from_json`. The GPU must be idle before the caches' `Arc`s drop,
//! because an in-flight frame may still reference an `Arc<GpuTexture>`.
//!
//! Save/load reach the renderer only through the [`ProjectHost`] trait — `wait_gpu_idle`, the
//! `renderSettings` serde, and `apply_render_settings`.

use std::path::{Path, PathBuf};

use saffron_json::{Value, dump_json, json_string_or, json_u64_or, parse_json};
use saffron_scene::{AssetCatalog, ComponentRegistry, Scene, seed_starter_scene};

use crate::AssetServer;
use crate::catalog::{
    catalog_folders_from_json, catalog_folders_to_json, catalog_from_json, catalog_to_json,
};
use crate::error::{Error, Result};

/// The unified project document version. A `project.json` declaring any other version is
/// a typed [`Error::BadProjectVersion`], not a silent best-effort load.
pub const PROJECT_VERSION: i64 = 1;

/// The renderer-touching operations [`AssetServer::save_project`] / [`AssetServer::load_project`]
/// drive, behind a trait so this crate stays decoupled from the live renderer.
///
/// The host implements it over its `Renderer` (`wait_gpu_idle` → `device.wait_idle`,
/// the serde over the renderer's getters/setters, the RT toggles gated on device
/// support). Tests implement a recording stub to assert the load-order discipline
/// without a Vulkan device.
pub trait ProjectHost {
    /// Blocks until the GPU has finished every in-flight frame. Called by `load_project`
    /// / `create_project` **before** the asset caches are cleared, so dropping a cached
    /// `Arc<GpuTexture>` never frees a resource a frame still reads.
    fn wait_gpu_idle(&mut self);

    /// Serializes the renderer's settings as the project-file `renderSettings` block.
    fn render_settings_to_json(&self) -> Value;

    /// Applies a saved `renderSettings` block; missing fields keep the current value and
    /// the RT toggles apply only where the device supports ray tracing.
    fn apply_render_settings(&mut self, settings: &Value);
}

/// The opaque editor-camera + debug-overlay blocks that ride through `save_project` /
/// `load_project` (the `editorCamera` / `debugOverlays` blocks).
///
/// They belong to `saffron-sceneedit`, so this crate never owns or interprets them — it
/// writes each to the doc only when it is a JSON object on save, and hands them back
/// (or JSON null when absent) on load. Pairing them in one carrier keeps the I/O
/// signatures tight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSidecar {
    /// The opaque editor-camera block (a `saffron-sceneedit` payload).
    pub editor_camera: Value,
    /// The opaque debug-overlays block (a `saffron-sceneedit` payload).
    pub debug_overlays: Value,
    /// The opaque asset-store enablement block (enabled connector ids + non-secret
    /// per-connector config; never a credential). Owned by the editor.
    pub stores: Value,
}

/// The spec for [`AssetServer::create_project`].
///
/// `root` empty resolves to `<userdata>/<name>`; `display_name` empty falls back to
/// [`default_display_name`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewProject {
    /// The short project name (must pass [`valid_project_name`]).
    pub name: String,
    /// The human display name, or empty to derive from `name`.
    pub display_name: String,
    /// The project root directory, or empty to place it under the userdata root.
    pub root: String,
}

/// The active project's identity + paths. The host owns one and
/// passes `&mut` it into create/load so it is updated in place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectInfo {
    /// Whether a project is currently loaded.
    pub loaded: bool,
    /// The project root directory (parent of `project.json`).
    pub root: String,
    /// The absolute / selection path of `project.json`.
    pub path: String,
    /// The short project name (lowercase/digit/`-`, the directory name under userdata).
    pub name: String,
    /// The human display name.
    pub display_name: String,
}

/// The app-data root: `$SAFFRON_APPDATA_DIR` when set and non-empty, else `appdata`.
#[must_use]
pub fn app_data_root() -> String {
    match std::env::var("SAFFRON_APPDATA_DIR") {
        Ok(value) if !value.is_empty() => value,
        _ => "appdata".to_string(),
    }
}

/// The per-user project root: `<appDataRoot>/userdata`.
#[must_use]
pub fn project_userdata_root() -> String {
    Path::new(&app_data_root())
        .join("userdata")
        .to_string_lossy()
        .into_owned()
}

/// Whether `name` is a legal project directory name.
///
/// Non-empty, at most 63 bytes, lowercase ASCII letters / digits / `-`, and the first
/// and last characters are a lowercase letter or digit (no leading/trailing `-`).
#[must_use]
pub fn valid_project_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 63 {
        return false;
    }
    let is_lower_digit = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let bytes = name.as_bytes();
    if !is_lower_digit(bytes[0]) || !is_lower_digit(bytes[bytes.len() - 1]) {
        return false;
    }
    bytes.iter().all(|&c| is_lower_digit(c) || c == b'-')
}

/// A display name derived from a project name: `-` becomes a space and each word's first
/// letter upper-cases (`my-cool-game` →
/// `My Cool Game`). An empty name yields `Untitled Project`.
#[must_use]
pub fn default_display_name(name: &str) -> String {
    if name.is_empty() {
        return "Untitled Project".to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for c in name.chars() {
        if c == '-' {
            out.push(' ');
            capitalize = true;
        } else if capitalize && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            out.push(c);
            capitalize = false;
        }
    }
    out
}

/// Resolves a `project.json` path from a selection.
///
/// A valid project *name* resolves to `<userdata>/<name>/project.json`; a path already
/// ending in `project.json` is used verbatim; any other path is treated as a project
/// root and gets `/project.json` appended.
#[must_use]
pub fn project_json_path(selection: &str) -> PathBuf {
    if valid_project_name(selection) {
        return Path::new(&project_userdata_root())
            .join(selection)
            .join("project.json");
    }
    let path = Path::new(selection);
    if path
        .file_name()
        .map(|f| f == "project.json")
        .unwrap_or(false)
    {
        return path.to_path_buf();
    }
    path.join("project.json")
}

/// Builds a [`ProjectInfo`] from a resolved `project.json` path and its parsed document.
///
/// The root is the file's parent (`.` when empty); the name falls back to the root's
/// directory name (then `project`) when the document carries no valid `name`; the display
/// name falls back to [`default_display_name`].
#[must_use]
pub fn project_info_from_path(path: &Path, doc: &Value) -> ProjectInfo {
    let root = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let fallback_name = root
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "project".to_string());

    let mut name = json_string_or(doc, "name", fallback_name.clone());
    if !valid_project_name(&name) {
        name = if valid_project_name(&fallback_name) {
            fallback_name
        } else {
            "project".to_string()
        };
    }
    let display_name = json_string_or(doc, "displayName", default_display_name(&name));

    ProjectInfo {
        loaded: true,
        root: root.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        name,
        display_name,
    }
}

/// The starter Lua script written into a fresh project's `src/example.lua`. Written only
/// when absent, so a user copy is never clobbered.
pub const STARTER_SCRIPT: &str = r#"-- example.lua: attach to an entity's Script component, then press Play.
-- Orbits the entity in the x/y plane around where it was authored.
---@class Example : sa.ScriptSelf
local Example = {}

Example.properties = {
  speed = 1.0,  -- radians/second, editable in the Inspector
  radius = 2.0,
}

function Example:on_create()
  -- Center one radius left of the authored spot, so the orbit starts on the entity.
  self.center = self.entity:get_position() - sa.vec3(self.radius, 0, 0)
  self.angle = 0
end

function Example:on_update(dt)
  self.angle = self.angle + self.speed * dt
  local r = self.radius
  self.entity:set_position(self.center + sa.vec3(math.cos(self.angle) * r, math.sin(self.angle) * r, 0))
end

return Example
"#;

/// The project `.luarc.json` pointing LuaLS at `library/`, declaring the `sa` global, and
/// disabling the libs the runtime VM sandboxes out. Written only-when-absent so a user
/// copy is never clobbered.
pub const LUARC_JSON: &str = r#"{
  "runtime.version": "Lua 5.4",
  "workspace.library": ["library"],
  "diagnostics.globals": ["sa"],
  "runtime.builtin": { "io": "disable", "os": "disable", "debug": "disable", "package": "disable" }
}
"#;

/// Ensures `<root>/src/` exists and seeds `src/example.lua` when absent.
///
/// Idempotent: the folder is ensured on create *and* on load (a pre-existing project gains
/// it on open), the example only when it does not already exist. A directory-creation
/// failure is logged and skipped — the real I/O error surfaces later if a script needs it.
pub fn ensure_script_src(root: &Path) {
    let src = root.join("src");
    if let Err(err) = std::fs::create_dir_all(&src) {
        tracing::warn!("project src/ not created: {err}");
        return;
    }
    let example = src.join("example.lua");
    if !example.exists()
        && let Err(err) = std::fs::write(&example, STARTER_SCRIPT)
    {
        tracing::warn!("project example.lua not written: {err}");
    }
}

/// Ensures `<root>/library/` exists, (re)writes `library/sa.lua` with the supplied LuaLS
/// type-def text, and seeds `.luarc.json` when absent.
///
/// `sa.lua` is an engine-owned generated artifact describing the live `sa` API, so it is
/// rewritten every open to track the engine version — the host supplies the text (it owns
/// the binding surface). `.luarc.json` holds editable LuaLS settings, so it is written
/// only-when-absent and a user copy is never clobbered.
pub fn ensure_script_library(root: &Path, sa_lua_defs: &str) {
    let library = root.join("library");
    if let Err(err) = std::fs::create_dir_all(&library) {
        tracing::warn!("project library/ not created: {err}");
        return;
    }
    if let Err(err) = std::fs::write(library.join("sa.lua"), sa_lua_defs) {
        tracing::warn!("project sa.lua not written: {err}");
    }
    let luarc = root.join(".luarc.json");
    if !luarc.exists()
        && let Err(err) = std::fs::write(&luarc, LUARC_JSON)
    {
        tracing::warn!("project .luarc.json not written: {err}");
    }
}

/// The file stem as a Lua identifier for the boilerplate's class table: non-identifier
/// characters become `_`, the first letter upper-cases,
/// and a leading digit gets a `Script` prefix (`turret-2` → `Turret_2`).
fn script_class_name(stem: &str) -> String {
    let mut name: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        name.insert_str(0, "Script");
    }
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => name,
    }
}

/// Creates `<root>/src/<name>.lua` with the class-table boilerplate the runtime expects.
///
/// A `.lua` suffix is appended when missing; subfolders are allowed, `..` is not. Returns
/// the `src/`-relative path a `ScriptSlot` stores.
///
/// # Errors
///
/// [`Error::Io`] when the name is invalid (empty, contains `..`, or is absolute), the file
/// already exists, or the directory / file cannot be written.
pub fn create_project_script(root: &str, name: &str) -> Result<String> {
    if name.is_empty() || name.contains("..") || name.starts_with('/') {
        return Err(Error::Io(format!("invalid script name '{name}'")));
    }
    let name = if name.ends_with(".lua") {
        name.to_string()
    } else {
        format!("{name}.lua")
    };
    let file = Path::new(root).join("src").join(&name);
    if file.exists() {
        return Err(Error::Io(format!("'{name}' already exists")));
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| Error::Io(format!("cannot create '{}': {err}", parent.display())))?;
    }
    let class_name = script_class_name(
        file.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default(),
    );
    let body = format!(
        "local {0} = {{}}\n\n{0}.properties = {{\n  -- speed = 1.0, -- declared fields show up in the Inspector\n}}\n\nfunction {0}.on_create(self)\nend\n\nfunction {0}.on_update(self, dt)\nend\n\nreturn {0}\n",
        class_name
    );
    std::fs::write(&file, body)
        .map_err(|err| Error::Io(format!("cannot write '{}': {err}", file.display())))?;
    Ok(name)
}

impl AssetServer {
    /// Saves the whole project (catalog + folders + scene + render settings + the optional
    /// editor camera / debug overlays) to one JSON file.
    ///
    /// `target` falls back to `project.path` when empty. The opaque [`ProjectSidecar`]
    /// blocks are written only when they are JSON objects — the host passes them through
    /// unchanged from `saffron-sceneedit`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when there is no active project path, the parent directory cannot be
    /// created, or the file cannot be written.
    pub fn save_project(
        &self,
        host: &dyn ProjectHost,
        reg: &ComponentRegistry,
        scene: &mut Scene,
        project: &ProjectInfo,
        path: &str,
        sidecar: &ProjectSidecar,
    ) -> Result<()> {
        let target = if path.is_empty() {
            project.path.as_str()
        } else {
            path
        };
        if target.is_empty() {
            return Err(Error::Io("no active project path".to_string()));
        }

        let mut doc = serde_json::Map::new();
        doc.insert("version".to_string(), Value::from(PROJECT_VERSION));
        doc.insert("name".to_string(), Value::String(project.name.clone()));
        doc.insert(
            "displayName".to_string(),
            Value::String(project.display_name.clone()),
        );
        doc.insert("assets".to_string(), catalog_to_json(&self.catalog));
        doc.insert(
            "assetFolders".to_string(),
            catalog_folders_to_json(&self.catalog),
        );
        doc.insert("scene".to_string(), scene.scene_to_json(reg));
        doc.insert("renderSettings".to_string(), host.render_settings_to_json());
        if sidecar.editor_camera.is_object() {
            doc.insert("editorCamera".to_string(), sidecar.editor_camera.clone());
        }
        if sidecar.debug_overlays.is_object() {
            doc.insert("debugOverlays".to_string(), sidecar.debug_overlays.clone());
        }
        if sidecar.stores.is_object() {
            doc.insert("stores".to_string(), sidecar.stores.clone());
        }

        let target_path = Path::new(target);
        if let Some(parent) = target_path.parent()
            && !parent.as_os_str().is_empty()
        {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(target_path, dump_json(&Value::Object(doc), 2))
            .map_err(|err| Error::Io(format!("write failed for '{target}': {err}")))
    }

    /// Loads a project file: replaces the catalog + scene, after idling the GPU and
    /// clearing the GPU caches so stale `Arc`s drop and assets re-resolve.
    ///
    /// `sa_lua_defs` is the LuaLS type-def text the host supplies for `library/sa.lua`.
    /// The saved [`ProjectSidecar`] blocks (each JSON null when absent) are returned to the
    /// caller for `saffron-sceneedit` to apply.
    ///
    /// The load order is load-bearing — see the module docs.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be read; [`Error::Json`] when it is not valid
    /// JSON or not an object; [`Error::BadProjectVersion`] when the `version` is not
    /// [`PROJECT_VERSION`]; a scene-load error otherwise.
    pub fn load_project(
        &mut self,
        host: &mut dyn ProjectHost,
        reg: &ComponentRegistry,
        scene: &mut Scene,
        project: &mut ProjectInfo,
        selection: &str,
        sa_lua_defs: &str,
    ) -> Result<ProjectSidecar> {
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

        host.wait_gpu_idle();
        self.clear_asset_caches();
        *project = project_info_from_path(&path, &doc);
        self.set_asset_root(Path::new(&project.root).join("assets"));
        ensure_script_src(Path::new(&project.root));
        ensure_script_library(Path::new(&project.root), sa_lua_defs);

        let empty_array = Value::Array(Vec::new());
        let mut loaded_catalog = AssetCatalog::default();
        catalog_from_json(
            &mut loaded_catalog,
            doc.get("assets").unwrap_or(&empty_array),
        );
        catalog_folders_from_json(
            &mut loaded_catalog,
            doc.get("assetFolders").unwrap_or(&empty_array),
        );
        self.replace_catalog(loaded_catalog);
        // The filesystem is the source of truth: reconcile the doc's catalog against disk via
        // the regenerable cache (a cold scan on a cache miss), so a never-saved import is
        // rediscovered and a deleted file's row is dropped.
        match self.load_catalog() {
            Ok(scan) => {
                if !scan.added.is_empty() || !scan.removed.is_empty() {
                    tracing::info!(
                        "scan: reconciled catalog with disk (+{} -{})",
                        scan.added.len(),
                        scan.removed.len()
                    );
                }
            }
            Err(err) => tracing::warn!("scan: {err}"),
        }

        if let Some(settings) = doc.get("renderSettings") {
            host.apply_render_settings(settings);
        }
        let sidecar = ProjectSidecar {
            editor_camera: doc.get("editorCamera").cloned().unwrap_or(Value::Null),
            debug_overlays: doc.get("debugOverlays").cloned().unwrap_or(Value::Null),
            stores: doc.get("stores").cloned().unwrap_or(Value::Null),
        };

        let scene_doc = doc
            .get("scene")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        scene.scene_from_json(reg, &scene_doc)?;
        Ok(sidecar)
    }

    /// Creates a fresh project: resets the scene to the shared starter scene (a framed camera
    /// and a sun) + clears the catalog, idles + clears the GPU caches, sets the asset root,
    /// ensures the script `src/` + library, then saves `project.json`.
    ///
    /// `spec.root` empty resolves the root to `<userdata>/<name>`; `spec.display_name`
    /// empty falls back to [`default_display_name`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidProjectName`] when `spec.name` fails [`valid_project_name`]; the
    /// [`AssetServer::save_project`] errors otherwise.
    pub fn create_project(
        &mut self,
        host: &mut dyn ProjectHost,
        reg: &ComponentRegistry,
        scene: &mut Scene,
        project: &mut ProjectInfo,
        spec: &NewProject,
        sa_lua_defs: &str,
    ) -> Result<()> {
        if !valid_project_name(&spec.name) {
            return Err(Error::InvalidProjectName(spec.name.clone()));
        }
        let root = if spec.root.is_empty() {
            Path::new(&project_userdata_root()).join(&spec.name)
        } else {
            PathBuf::from(&spec.root)
        };
        let next = ProjectInfo {
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

        host.wait_gpu_idle();
        *scene = Scene::default();
        seed_starter_scene(scene);
        self.clear_asset_caches();
        self.replace_catalog(AssetCatalog::default());
        self.set_asset_root(root.join("assets"));
        ensure_script_src(&root);
        ensure_script_library(&root, sa_lua_defs);
        *project = next;
        self.save_project(
            host,
            reg,
            scene,
            project,
            &project.path.clone(),
            &ProjectSidecar::default(),
        )
    }

    /// Creates an auto-named scratch project keyed to the current working directory + the
    /// `$SAFFRON_CONTROL_SOCK`: a deterministic per-shell project so a host launched
    /// without a project still has a loadable one.
    ///
    /// # Errors
    ///
    /// The [`AssetServer::create_project`] errors.
    pub fn create_scratch_project(
        &mut self,
        host: &mut dyn ProjectHost,
        reg: &ComponentRegistry,
        scene: &mut Scene,
        project: &mut ProjectInfo,
        sa_lua_defs: &str,
    ) -> Result<()> {
        let socket = std::env::var("SAFFRON_CONTROL_SOCK").unwrap_or_default();
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let suffix = scratch_suffix(&format!("{cwd}{socket}"));
        let name = format!("scratch-{}", &suffix[..suffix.len().min(12)]);
        let spec = NewProject {
            name,
            display_name: "Scratch Project".to_string(),
            root: String::new(),
        };
        self.create_project(host, reg, scene, project, &spec, sa_lua_defs)
    }
}

/// The deterministic per-shell scratch project name, keyed to the current working directory + the
/// `$SAFFRON_CONTROL_SOCK` (`scratch-<fnv>`), so a host launched without a project resolves the
/// same scratch project each run.
#[must_use]
pub fn scratch_project_name() -> String {
    let socket = std::env::var("SAFFRON_CONTROL_SOCK").unwrap_or_default();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let suffix = scratch_suffix(&format!("{cwd}{socket}"));
    format!("scratch-{}", &suffix[..suffix.len().min(12)])
}

/// An FNV-1a fold of `key` as a decimal string, for the scratch project name suffix.
///
/// FNV-1a is deterministic, giving a stable per-`(cwd, socket)` suffix.
fn scratch_suffix(key: &str) -> String {
    const FNV_OFFSET: u64 = 1469598103934665603;
    const FNV_PRIME: u64 = 1099511628211;
    let mut hash = FNV_OFFSET;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash.to_string()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use saffron_json::{Value, parse_json};
    use saffron_scene::{ComponentRegistry, Scene, register_builtin_components};

    use crate::project::{
        NewProject, ProjectHost, ProjectInfo, ProjectSidecar, default_display_name,
        project_info_from_path, project_json_path, valid_project_name,
    };
    use crate::{AssetServer, PROJECT_VERSION};

    /// A unique scratch dir under the system temp, removed and recreated per test, so two
    /// tests never collide on the asset root.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "saffron-assets-project-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A recording stub host: it records the sequence of renderer-touching calls so the load
    /// order can be asserted, and applies no real render settings.
    #[derive(Default)]
    struct RecordingHost {
        calls: Rc<RefCell<Vec<String>>>,
    }

    impl ProjectHost for RecordingHost {
        fn wait_gpu_idle(&mut self) {
            self.calls.borrow_mut().push("wait_gpu_idle".to_string());
        }

        fn render_settings_to_json(&self) -> Value {
            serde_json::json!({ "aa": "taa", "exposureEv": 1.5, "shadows": true })
        }

        fn apply_render_settings(&mut self, settings: &Value) {
            self.calls
                .borrow_mut()
                .push(format!("apply_render_settings:{}", settings.is_object()));
        }
    }

    /// A no-op host that fails the test if any clearing/idle call lands out of order — used by
    /// the round-trip tests where only the I/O outcome matters.
    fn plain_host() -> RecordingHost {
        RecordingHost::default()
    }

    fn builtin_reg() -> ComponentRegistry {
        register_builtin_components()
    }

    #[test]
    fn valid_project_name_reproduces_the_cpp_rules() {
        assert!(valid_project_name("game"));
        assert!(valid_project_name("my-cool-game"));
        assert!(valid_project_name("a"));
        assert!(valid_project_name("level2"));
        assert!(valid_project_name("9lives"));

        assert!(!valid_project_name(""));
        assert!(!valid_project_name(&"a".repeat(64)));
        assert!(valid_project_name(&"a".repeat(63)));
        assert!(!valid_project_name("-game"));
        assert!(!valid_project_name("game-"));
        assert!(!valid_project_name("My-Game"));
        assert!(!valid_project_name("my_game"));
        assert!(!valid_project_name("my game"));
        assert!(!valid_project_name("a/b"));
    }

    #[test]
    fn default_display_name_capitalizes_on_dash() {
        assert_eq!(default_display_name(""), "Untitled Project");
        assert_eq!(default_display_name("game"), "Game");
        assert_eq!(default_display_name("my-cool-game"), "My Cool Game");
        assert_eq!(default_display_name("level2"), "Level2");
        // A leading digit is not upper-cased (it cannot be); the next word still capitalizes.
        assert_eq!(default_display_name("2nd-try"), "2nd Try");
    }

    #[test]
    fn project_json_path_resolves_name_root_and_file() {
        let by_name = project_json_path("game");
        assert!(by_name.ends_with("game/project.json"));
        let by_file = project_json_path("/tmp/x/project.json");
        assert_eq!(by_file, PathBuf::from("/tmp/x/project.json"));
        let by_root = project_json_path("/tmp/x/y");
        assert_eq!(by_root, PathBuf::from("/tmp/x/y/project.json"));
    }

    #[test]
    fn project_info_from_path_falls_back_to_the_directory_name() {
        let path = PathBuf::from("/tmp/projects/my-game/project.json");
        let info = project_info_from_path(&path, &Value::Object(serde_json::Map::new()));
        assert!(info.loaded);
        assert_eq!(info.root, "/tmp/projects/my-game");
        assert_eq!(info.path, "/tmp/projects/my-game/project.json");
        assert_eq!(info.name, "my-game");
        assert_eq!(info.display_name, "My Game");

        let doc = serde_json::json!({ "name": "other", "displayName": "Custom" });
        let info = project_info_from_path(&path, &doc);
        assert_eq!(info.name, "other");
        assert_eq!(info.display_name, "Custom");
    }

    #[test]
    fn create_then_load_round_trips_catalog_folders_scene_and_render_settings() {
        let root = scratch("roundtrip").join("game");
        let assets_root = root.join("assets");
        let mut assets = AssetServer::new(&assets_root);
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut info = ProjectInfo::default();
        let mut host = plain_host();

        assets.catalog.folders.push("props".to_string());
        assets.catalog.put(saffron_scene::AssetEntry {
            id: saffron_core::Uuid(4242),
            name: "loose-mat".to_string(),
            asset_type: saffron_scene::AssetType::Material,
            path: "materials/4242.smat".to_string(),
            ..saffron_scene::AssetEntry::default()
        });
        // A real material file on disk so the disk reconcile keeps the row (the cold scan is
        // the source of truth — a row with no file would be dropped).
        let mat_path = assets_root.join("materials").join("4242.smat");
        std::fs::create_dir_all(mat_path.parent().unwrap()).unwrap();
        std::fs::write(&mat_path, b"{}").unwrap();

        let _ = scene.create_entity("hero");

        assets
            .create_project(
                &mut host,
                &reg,
                &mut scene,
                &mut info,
                &NewProject {
                    name: "game".to_string(),
                    display_name: String::new(),
                    root: root.to_string_lossy().into_owned(),
                },
                "---@meta\n",
            )
            .unwrap();

        // create_project clears the scene + catalog, so re-seed before saving.
        assets.catalog.folders.push("props".to_string());
        assets.catalog.put(saffron_scene::AssetEntry {
            id: saffron_core::Uuid(4242),
            name: "loose-mat".to_string(),
            asset_type: saffron_scene::AssetType::Material,
            path: "materials/4242.smat".to_string(),
            ..saffron_scene::AssetEntry::default()
        });
        let _ = scene.create_entity("hero");
        assets
            .save_project(
                &host,
                &reg,
                &mut scene,
                &info,
                &info.path.clone(),
                &ProjectSidecar::default(),
            )
            .unwrap();

        let mut loaded_assets = AssetServer::new(scratch("roundtrip-load").join("assets"));
        let mut loaded_scene = Scene::default();
        let mut loaded_info = ProjectInfo::default();
        let mut load_host = plain_host();
        let sidecar = loaded_assets
            .load_project(
                &mut load_host,
                &reg,
                &mut loaded_scene,
                &mut loaded_info,
                &info.path,
                "---@meta\n",
            )
            .unwrap();

        assert!(
            loaded_assets
                .catalog
                .find(saffron_core::Uuid(4242))
                .is_some()
        );
        assert!(loaded_assets.catalog.folders.contains(&"props".to_string()));
        let names: Vec<String> = {
            let mut v = Vec::new();
            loaded_scene.for_each::<&saffron_scene::Name, _>(|_, n| v.push(n.name.clone()));
            v
        };
        assert!(names.contains(&"hero".to_string()));
        assert!(sidecar.editor_camera.is_null());
        assert!(sidecar.debug_overlays.is_null());
        assert_eq!(loaded_info.name, "game");
        assert_eq!(loaded_info.display_name, "Game");
    }

    #[test]
    fn saved_doc_is_byte_stable_with_decimal_string_ids() {
        let root = scratch("bytes").join("game");
        let mut assets = AssetServer::new(root.join("assets"));
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut host = plain_host();

        assets.catalog.put(saffron_scene::AssetEntry {
            id: saffron_core::Uuid(9007199254740993), // > 2^53, must serialize as a string
            name: "big".to_string(),
            asset_type: saffron_scene::AssetType::Mesh,
            path: "models/big.smodel".to_string(),
            ..saffron_scene::AssetEntry::default()
        });

        let info = ProjectInfo {
            loaded: true,
            root: root.to_string_lossy().into_owned(),
            path: root.join("project.json").to_string_lossy().into_owned(),
            name: "game".to_string(),
            display_name: "Game".to_string(),
        };
        assets
            .save_project(
                &host,
                &reg,
                &mut scene,
                &info,
                &info.path,
                &ProjectSidecar::default(),
            )
            .unwrap();
        let _ = &mut host;

        let text = std::fs::read_to_string(&info.path).unwrap();

        // The id crosses as a decimal STRING, never a JSON number (the frozen wire rule).
        assert!(text.contains("\"9007199254740993\""));
        assert!(!text.contains("9007199254740993,"));
        assert!(!text.contains(": 9007199254740993"));

        let doc = parse_json(&text).unwrap();
        assert_eq!(
            doc.get("version").and_then(Value::as_i64),
            Some(PROJECT_VERSION)
        );
        assert_eq!(doc.get("name").and_then(Value::as_str), Some("game"));
        assert!(doc.get("assets").is_some());
        assert!(doc.get("assetFolders").is_some());
        assert!(doc.get("scene").is_some());
        assert!(doc.get("renderSettings").is_some());
        assert!(doc.get("editorCamera").is_none());
        assert!(doc.get("debugOverlays").is_none());

        // Byte-stable: a second save of identical state produces identical bytes.
        assets
            .save_project(
                &host,
                &reg,
                &mut scene,
                &info,
                &info.path,
                &ProjectSidecar::default(),
            )
            .unwrap();
        let text2 = std::fs::read_to_string(&info.path).unwrap();
        assert_eq!(text, text2, "the saved doc is byte-stable across saves");
    }

    #[test]
    fn bad_project_version_is_a_typed_error() {
        let root = scratch("badversion").join("game");
        let path = root.join("project.json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!({ "version": 99, "scene": {} })).unwrap(),
        )
        .unwrap();

        let mut assets = AssetServer::new(root.join("assets"));
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut info = ProjectInfo::default();
        let mut host = plain_host();

        let err = assets
            .load_project(
                &mut host,
                &reg,
                &mut scene,
                &mut info,
                path.to_str().unwrap(),
                "---@meta\n",
            )
            .unwrap_err();
        match err {
            crate::Error::BadProjectVersion { found, expected } => {
                assert_eq!(found, 99);
                assert_eq!(expected, PROJECT_VERSION);
            }
            other => panic!("expected BadProjectVersion, got {other:?}"),
        }
    }

    #[test]
    fn load_idles_and_clears_caches_before_swapping_the_catalog() {
        let root = scratch("order").join("game");
        let assets_root = root.join("assets");
        let writer = AssetServer::new(&assets_root);
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut info = ProjectInfo {
            loaded: true,
            root: root.to_string_lossy().into_owned(),
            path: root.join("project.json").to_string_lossy().into_owned(),
            name: "game".to_string(),
            display_name: "Game".to_string(),
        };
        let write_host = plain_host();
        writer
            .save_project(
                &write_host,
                &reg,
                &mut scene,
                &info,
                &info.path.clone(),
                &ProjectSidecar {
                    editor_camera: serde_json::json!({ "kind": "orbit" }),
                    debug_overlays: serde_json::json!({ "grid": true }),
                    stores: serde_json::json!({ "enabled": ["polyhaven"] }),
                },
            )
            .unwrap();

        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut host = RecordingHost {
            calls: Rc::clone(&calls),
        };
        let mut assets = AssetServer::new(scratch("order-load").join("assets"));
        // Seed a stale negative-cache marker so we can prove the caches are cleared on load.
        assets.mesh_by_uuid.insert(7, None);

        let mut loaded_scene = Scene::default();
        let path = info.path.clone();
        let sidecar = assets
            .load_project(
                &mut host,
                &reg,
                &mut loaded_scene,
                &mut info,
                &path,
                "---@meta\n",
            )
            .unwrap();

        // wait_gpu_idle is the FIRST renderer-touching call, before apply_render_settings.
        let recorded = calls.borrow().clone();
        assert_eq!(
            recorded.first().map(String::as_str),
            Some("wait_gpu_idle"),
            "wait_gpu_idle is recorded first (the idle-before-clear guard)"
        );
        assert!(
            recorded.iter().any(|c| c == "apply_render_settings:true"),
            "render settings applied after the catalog swap"
        );
        let idle_idx = recorded.iter().position(|c| c == "wait_gpu_idle").unwrap();
        let apply_idx = recorded
            .iter()
            .position(|c| c == "apply_render_settings:true")
            .unwrap();
        assert!(idle_idx < apply_idx, "idle precedes apply");

        assert!(assets.mesh_by_uuid.is_empty());

        assert_eq!(
            sidecar.editor_camera,
            serde_json::json!({ "kind": "orbit" })
        );
        assert_eq!(sidecar.debug_overlays, serde_json::json!({ "grid": true }));
    }

    #[test]
    fn stores_block_round_trips_through_save_and_load() {
        // The per-project enabled-connector set persists into project.json's `stores` block and
        // comes back unchanged on load (credentials live editor-side; only enablement is host state).
        let root = scratch("stores").join("game");
        let writer = AssetServer::new(root.join("assets"));
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let info = ProjectInfo {
            loaded: true,
            root: root.to_string_lossy().into_owned(),
            path: root.join("project.json").to_string_lossy().into_owned(),
            name: "game".to_string(),
            display_name: "Game".to_string(),
        };
        let write_host = plain_host();
        let enabled = serde_json::json!({ "enabled": ["polyhaven", "poly-pizza"] });
        writer
            .save_project(
                &write_host,
                &reg,
                &mut scene,
                &info,
                &info.path.clone(),
                &ProjectSidecar {
                    stores: enabled.clone(),
                    ..ProjectSidecar::default()
                },
            )
            .unwrap();

        let text = std::fs::read_to_string(&info.path).unwrap();
        let doc = parse_json(&text).unwrap();
        assert_eq!(doc.get("stores"), Some(&enabled));

        let mut loaded_assets = AssetServer::new(scratch("stores-load").join("assets"));
        let mut loaded_scene = Scene::default();
        let mut loaded_info = ProjectInfo::default();
        let mut load_host = plain_host();
        let sidecar = loaded_assets
            .load_project(
                &mut load_host,
                &reg,
                &mut loaded_scene,
                &mut loaded_info,
                &info.path,
                "---@meta\n",
            )
            .unwrap();
        assert_eq!(sidecar.stores, enabled);
    }

    #[test]
    fn create_scratch_project_produces_a_loadable_minimal_project() {
        // The scratch project lands under the default userdata root, so its dir is computed
        // from the deterministic name and removed afterward.
        let mut assets = AssetServer::new(scratch("scratch-project").join("assets"));
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut info = ProjectInfo::default();
        let mut host = plain_host();

        assets
            .create_scratch_project(&mut host, &reg, &mut scene, &mut info, "---@meta\n")
            .unwrap();

        assert!(info.loaded);
        assert!(info.name.starts_with("scratch-"));
        assert_eq!(info.display_name, "Scratch Project");
        assert!(valid_project_name(&info.name), "the auto name is valid");
        let project_root = PathBuf::from(&info.root);
        assert!(std::path::Path::new(&info.path).exists());

        let mut load_assets = AssetServer::new(scratch("scratch-project-load").join("assets"));
        let mut load_scene = Scene::default();
        let mut load_info = ProjectInfo::default();
        let mut load_host = plain_host();
        load_assets
            .load_project(
                &mut load_host,
                &reg,
                &mut load_scene,
                &mut load_info,
                &info.path,
                "---@meta\n",
            )
            .expect("the scratch project loads");
        assert_eq!(load_info.name, info.name);

        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn create_project_rejects_an_invalid_name() {
        let mut assets = AssetServer::new(scratch("badname").join("assets"));
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut info = ProjectInfo::default();
        let mut host = plain_host();

        let err = assets
            .create_project(
                &mut host,
                &reg,
                &mut scene,
                &mut info,
                &NewProject {
                    name: "Bad Name!".to_string(),
                    ..NewProject::default()
                },
                "---@meta\n",
            )
            .unwrap_err();
        assert!(matches!(err, crate::Error::InvalidProjectName(_)));
    }

    #[test]
    fn create_project_writes_the_script_scaffold() {
        let root = scratch("scaffold").join("game");
        let mut assets = AssetServer::new(root.join("assets"));
        let reg = builtin_reg();
        let mut scene = Scene::default();
        let mut info = ProjectInfo::default();
        let mut host = plain_host();

        assets
            .create_project(
                &mut host,
                &reg,
                &mut scene,
                &mut info,
                &NewProject {
                    name: "game".to_string(),
                    display_name: String::new(),
                    root: root.to_string_lossy().into_owned(),
                },
                "---@meta\n-- defs\n",
            )
            .unwrap();

        assert!(root.join("src").join("example.lua").is_file());
        assert!(root.join("library").join("sa.lua").is_file());
        assert!(root.join(".luarc.json").is_file());
        let defs = std::fs::read_to_string(root.join("library").join("sa.lua")).unwrap();
        assert_eq!(defs, "---@meta\n-- defs\n");

        let rel =
            crate::project::create_project_script(root.to_str().unwrap(), "turret-2").unwrap();
        assert_eq!(rel, "turret-2.lua");
        let body = std::fs::read_to_string(root.join("src").join("turret-2.lua")).unwrap();
        assert!(body.contains("local Turret_2 = {}"));
        assert!(body.contains("return Turret_2"));
        assert!(crate::project::create_project_script(root.to_str().unwrap(), "turret-2").is_err());
        assert!(
            crate::project::create_project_script(root.to_str().unwrap(), "../escape").is_err()
        );
    }
}
