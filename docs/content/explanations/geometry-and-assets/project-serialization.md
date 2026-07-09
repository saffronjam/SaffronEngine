+++
title = 'Project files'
weight = 9
+++

# Project files

A project is a folder with one `project.json` and a project-local `assets/` directory. A
scene refers to meshes and textures by UUID; the catalog in `project.json` maps those UUIDs
to files under the project's asset root. Keeping both in one project folder means the editor
can copy or archive a project without depending on engine-bundled runtime assets.

During local development, app data lives under a userdata root, with user projects under
`<userdata>/<project-name>/`. Packaged builds swap the base directory behind the same
`app_data_root` / `project_userdata_root` helpers.

```text
<userdata>/<project-name>/
  project.json
  assets/
    models/
    textures/
    materials/
  src/            Lua scripts (Script component slot paths resolve here)
  cache/          regenerable scan + thumbnail caches
```

The `src/` folder is ensured on create and on load (with a starter script when absent) by
`ensure_script_src`; its contents are plain files, never catalog entries — see
[Script components](../../scripting/script-components-and-runtime/).

The project name is the stable folder-safe id (validated by `valid_project_name`).
`display_name` is stored separately in the project file and is what the editor shows users.

## How it works

`AssetServer::save_project` serializes one JSON document: a version, the project name, the
display name, the asset catalog, the scene, the renderer settings, and (when present) the
editor camera and debug overlays. The renderer-touching parts go through a `ProjectHost`
trait, so the asset crate stays decoupled from the live renderer: the host implements
`render_settings_to_json` / `apply_render_settings` over its `Renderer`, and `wait_gpu_idle`
over `device.wait_idle`. `load_project` reverses this, after first idling the GPU so the
previous project's resources release safely.

## One document

```json
{
  "version": 1,
  "name": "sample-project",
  "displayName": "Sample Project",
  "assets": [
    {
      "id": 3862017159553017004,
      "name": "cube",
      "type": "model",
      "path": "models/3862017159553017004.smodel"
    }
  ],
  "scene": { "version": 2, "entities": [] },
  "renderSettings": { "aa": "msaa4", "exposureEv": 0.0, "clustered": true, "shadows": true },
  "editorCamera": { "position": { "x": 3.0, "y": 2.5, "z": 4.0 }, "yaw": -37.0, "pitch": -29.0, "fov": 45.0 },
  "debugOverlays": { "bounds": false, "sceneAabb": false, "lightVolumes": false, "grid": true }
}
```

`catalog_to_json` serializes every `AssetEntry`; the type is written as a string
(`asset_type_name`), so the file stays readable and stable across enum reordering. The scene
half is the registry-driven `Scene::scene_to_json`. The `version` field is checked on load; a
mismatch is a typed `Error::BadProjectVersion` rather than a best-effort parse.

Two things are deliberately not saved: the GPU caches and the absolute asset root. The catalog
stores paths relative to `<project-root>/assets`, and the root is set when the project opens.

## Render settings, camera, and overlays ride along

`renderSettings` captures the renderer state the editor's render panel drives — the
[AA mode](../../anti-aliasing/aa-modes/), tonemap exposure, and the feature toggles
(clustered, depth prepass, shadows, IBL, SSAO, contact shadows, SSGI, DDGI, RT shadows,
ReSTIR) — so a project reopens looking the way it was saved. Missing fields keep their current
value, and the RT toggles only apply on a device that reports ray-tracing support.

The [editor camera](../../ui-and-editor/editor-camera/) and the
[debug overlays](../../ui-and-editor/debug-visualization/) ride along as opaque `editorCamera`
/ `debugOverlays` blocks carried in a `ProjectSidecar`. They belong to `saffron-sceneedit`, so
the asset crate never owns or interprets them — it writes each only when it is a JSON object on
save and hands them back (or JSON null when absent) on load; callers in the control commands
and the host startup path apply them.

## Loading replaces both, after a device idle

```rust
host.wait_gpu_idle();              // every in-flight frame finished
self.clear_asset_caches();         // drop the cached Arc<GpuMesh>/Arc<GpuTexture>
self.set_asset_root(/* <root>/assets */);
catalog_from_json(&mut self.catalog, doc.get("assets") ...);
self.load_catalog();               // reconcile against the disk scan
scene.scene_from_json(reg, &scene_doc)?;
```

The ordering matters. The GPU caches hold `Arc`s to meshes and textures the old project
uploaded; loading a new one must drop them. Dropping the last `Arc` frees a `GpuMesh`, which
frees Vulkan buffers a frame in flight may still reference. So `load_project` calls
`wait_gpu_idle` first, then clears the caches, then swaps the catalog and scene. With the
caches empty, the new scene's UUIDs re-resolve from the new catalog on the next `render_scene`,
[uploading lazily](../asset-server-and-catalog/) as they are first drawn. The load also
reconciles the doc's catalog against the disk scan (`load_catalog`), so a never-saved import is
rediscovered and a deleted file's row is dropped.

## Startup and commands

The editor shell owns startup project choice. If `SAFFRON_PROJECT` is set, the engine opens
(or creates) that project immediately — a project name under userdata, a project directory, or
a direct `project.json` path. `SAFFRON_SCRATCH_PROJECT` creates a per-shell scratch project
without showing the startup modal.

The control plane exposes project-aware commands (in `commands_asset.rs`):

- `get-project` returns the active project state.
- `new-project` creates and opens a project.
- `open-project` opens an existing project folder or file.
- `save-project` saves the active project when no path is passed.
- `load-project` loads a project from an explicit `project.json` path.

## Project-local assets and the path fixup

Imported models are baked under `assets/models/<uuid>.smodel`; imported textures are copied
under `assets/textures/<uuid>.<ext>`. `import-model`, `import-texture`, and the cube/model
entity preset require an active project so imports cannot write into the engine's bundled asset
directory. A standalone-mesh row whose file is absent under its recorded path but begins
`meshes/` is retried under `assets/models/`, so a catalog path written with the old `meshes/`
prefix still resolves.

## In the code

| What | File | Symbols |
|---|---|---|
| Save / load / create | `assets/src/project.rs` | `save_project`, `load_project`, `create_project`, `PROJECT_VERSION` |
| Renderer seam | `assets/src/project.rs` | `ProjectHost`, `ProjectSidecar` |
| Path + name helpers | `assets/src/project.rs` | `project_json_path`, `valid_project_name`, `app_data_root`, `project_userdata_root`, `ensure_script_src` |
| Catalog ↔ JSON | `assets/src/catalog.rs`; `assets/src/names.rs` | `catalog_to_json`, `catalog_from_json`, `asset_type_name` |
| Scene half | `scene/src/document.rs` | `Scene::scene_to_json`, `Scene::scene_from_json` |
| Project commands | `control/src/commands_asset.rs` | `get-project`, `new-project`, `open-project`, `save-project`, `load-project` |

> [!WARNING]
> `load_project` must `wait_gpu_idle` before clearing the caches. Clearing drops the last
> `Arc` to in-flight GPU meshes/textures, freeing their Vulkan buffers; doing that while a
> frame still uses them is a use-after-free. The idle is the ordering guarantee.

## Related

- [Asset catalog](../asset-server-and-catalog/) — what gets serialized
- [Import pipeline](../import-pipeline/) — fills the catalog this persists
- [Scene serialization](../../scene-and-ecs/scene-serialization/) — the `scene_to_json` half
- [Asset commands](../../tooling-and-control/asset-commands/) — `save-project`/`load-project` over the CLI
