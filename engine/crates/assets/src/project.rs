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
//! [`AssetServer::load_project`] keeps an exact ordered sequence:
//! parse → version-gate → `wait_gpu_idle` → clear the worker queue + the GPU
//! caches → set the asset root → ensure the script `src/` + library → load the catalog
//! from the doc → reconcile against disk (the filesystem is the source of truth, a cold
//! scan on a cache miss) → apply render settings →
//! pull camera/overlays → `scene_from_json`. The idle-before-clear is the use-after-free
//! guard: the GPU must be idle before the caches' `Arc`s drop, because an in-flight frame
//! may still reference an `Arc<GpuTexture>` and dropping it under the GPU is a runtime
//! UAF that `Drop` ordering alone cannot catch.
//!
//! # The renderer seam
//!
//! Save/load reach the renderer only through the [`ProjectHost`] trait — `wait_gpu_idle`,
//! the `renderSettings` serde, and `apply_render_settings`. The host implements it over
//! the live `Renderer`; tests drive a recording stub, so the ordered sequence is asserted
//! without a Vulkan device.

use std::path::{Path, PathBuf};

use saffron_json::{Value, dump_json, json_string_or, json_u64_or, parse_json};
use saffron_scene::{ComponentRegistry, Scene};

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
    if !example.exists() {
        if let Err(err) = std::fs::write(&example, STARTER_SCRIPT) {
            tracing::warn!("project example.lua not written: {err}");
        }
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
    if !luarc.exists() {
        if let Err(err) = std::fs::write(&luarc, LUARC_JSON) {
            tracing::warn!("project .luarc.json not written: {err}");
        }
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
        if let Some(parent) = target_path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
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
        catalog_from_json(&mut self.catalog, doc.get("assets").unwrap_or(&empty_array));
        catalog_folders_from_json(
            &mut self.catalog,
            doc.get("assetFolders").unwrap_or(&empty_array),
        );
        // The filesystem is the source of truth: reconcile the doc's catalog against disk
        // via the regenerable cache (a cold scan on a cache miss), so a never-saved import
        // is rediscovered and a deleted file's row is dropped. The doc names just loaded
        // seed the scan's name preservation. The caches were just cleared, so no GPU patch.
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

    /// Creates a fresh, empty project: resets the scene + catalog, idles + clears the GPU
    /// caches, sets the asset root, ensures the script `src/` + library, then saves
    /// `project.json`.
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
        self.catalog.entries.clear();
        self.catalog.folders.clear();
        self.catalog.by_id.clear();
        self.clear_asset_caches();
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
#[path = "project_tests.rs"]
mod tests;
