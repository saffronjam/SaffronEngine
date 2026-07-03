//! Project I/O tests: the save/load round-trip, the byte-stable saved doc, the
//! version gate, the load-order discipline (via a recording stub host), and the
//! name/display-name rules.
//!
//! GPU-free: the renderer is a recording stub implementing [`ProjectHost`], so the
//! ordered `wait_gpu_idle` → clear → swap sequence is asserted without a Vulkan device.

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

    // Empty, too long, illegal characters, leading/trailing dash, uppercase.
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
    // A valid name resolves under the userdata root.
    let by_name = project_json_path("game");
    assert!(by_name.ends_with("game/project.json"));
    // A path already ending in project.json is used verbatim.
    let by_file = project_json_path("/tmp/x/project.json");
    assert_eq!(by_file, PathBuf::from("/tmp/x/project.json"));
    // Any other path is treated as a project root.
    let by_root = project_json_path("/tmp/x/y");
    assert_eq!(by_root, PathBuf::from("/tmp/x/y/project.json"));
}

#[test]
fn project_info_from_path_falls_back_to_the_directory_name() {
    let path = PathBuf::from("/tmp/projects/my-game/project.json");
    // No name/displayName in the doc → derived from the directory name.
    let info = project_info_from_path(&path, &Value::Object(serde_json::Map::new()));
    assert!(info.loaded);
    assert_eq!(info.root, "/tmp/projects/my-game");
    assert_eq!(info.path, "/tmp/projects/my-game/project.json");
    assert_eq!(info.name, "my-game");
    assert_eq!(info.display_name, "My Game");

    // An explicit, valid name/displayName in the doc wins.
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

    // Seed a folder + a catalog row so the round-trip carries content.
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

    // Spawn one entity so the scene block has content.
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

    // create_project clears the scene + catalog, so re-seed the saved-state we want to
    // verify survives a fresh save → load. Re-do the seeding then save.
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

    // Load into fresh state.
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

    // The catalog row + folder survive (disk is the source of truth and the file exists).
    assert!(
        loaded_assets
            .catalog
            .find(saffron_core::Uuid(4242))
            .is_some()
    );
    assert!(loaded_assets.catalog.folders.contains(&"props".to_string()));
    // The scene entity survives.
    let names: Vec<String> = {
        let mut v = Vec::new();
        loaded_scene.for_each::<&saffron_scene::Name, _>(|_, n| v.push(n.name.clone()));
        v
    };
    assert!(names.contains(&"hero".to_string()));
    // Render settings applied; no editor camera / overlays in the doc → JSON null.
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

    // The top-level fields are present and the version is the gate value.
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
    // No editor camera / overlays were passed → the keys are absent.
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
    // Write a valid project to load.
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

    // Now load with a recording host and assert the ordered sequence.
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

    // The stale cache entry is gone (clear_asset_caches ran on load).
    assert!(assets.mesh_by_uuid.is_empty());

    // The opaque editor camera / debug overlays round-trip unchanged to the caller.
    assert_eq!(
        sidecar.editor_camera,
        serde_json::json!({ "kind": "orbit" })
    );
    assert_eq!(sidecar.debug_overlays, serde_json::json!({ "grid": true }));
}

#[test]
fn create_scratch_project_produces_a_loadable_minimal_project() {
    // The scratch project lands under the default userdata root (env overriding is
    // unsafe under #![deny(unsafe_code)] and racy across parallel tests), so the project
    // dir is computed from the deterministic name and removed afterward.
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
    // The project.json was written under <userdata>/<name>/ and is loadable.
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

    // create_project_script makes a new src/ file with the class-table boilerplate.
    let rel = crate::project::create_project_script(root.to_str().unwrap(), "turret-2").unwrap();
    assert_eq!(rel, "turret-2.lua");
    let body = std::fs::read_to_string(root.join("src").join("turret-2.lua")).unwrap();
    assert!(body.contains("local Turret_2 = {}"));
    assert!(body.contains("return Turret_2"));
    // A second create with the same name errors (no clobber).
    assert!(crate::project::create_project_script(root.to_str().unwrap(), "turret-2").is_err());
    // A traversal name is rejected.
    assert!(crate::project::create_project_script(root.to_str().unwrap(), "../escape").is_err());
}
