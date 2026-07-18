+++
title = 'Project files'
weight = 9
+++

# Project files

A project is one folder: a `project.json` document plus everything it references — asset files
under `assets/`, Lua scripts under `src/`. Scene components name meshes, textures, and materials
by asset id (a 64-bit `Uuid`); the catalog block inside `project.json` maps each id to a file
under the project's asset root. Copying or archiving the folder moves the whole project, since
nothing in it depends on the engine's bundled assets.

## Layout

```text
<root>/
  project.json      the one document: catalog + scene + render settings (+ editor blocks)
  assets/
    models/         baked .smodel containers, extracted .smesh / .sanim
    textures/       imported images, textures/<uuid>.<ext>
    materials/      .smat / .smatx material assets
    .cache/         regenerable catalog scan cache (catalog.json)
  src/              Lua scripts; Script component slot paths resolve here
  library/sa.lua    generated LuaLS type defs for the live `sa` API
  .luarc.json       LuaLS settings, seeded once and never overwritten
```

`ensure_script_src` creates `src/` on create *and* on load, seeding `src/example.lua` only when
absent. `ensure_script_library` rewrites `library/sa.lua` on every open, since it is an
engine-generated artifact for the [Lua Language Server](https://luals.github.io/) and must track
the engine; `.luarc.json` holds user-editable settings and is written only when missing. Script
files are plain
files, never catalog entries; see [Script components](../../scripting/script-components-and-runtime/).

Thumbnails are not project files. The thumbnail cache is app-level
(`<appDataRoot>/thumbnail-cache/`), content-addressed, and shared across projects, so the catalog
scan and project save never see it.

## Names and roots

The project name is the folder-safe id, validated by `valid_project_name`: non-empty, at most 63
bytes, lowercase ASCII letters, digits, and `-`, with no leading or trailing hyphen. The stored
`displayName` is what the editor shows; when empty it derives from the name (`my-cool-game` →
`My Cool Game`). User projects default to `<appDataRoot>/userdata/<name>/`, where the app-data
root is `$SAFFRON_APPDATA_DIR` when set and `appdata` under the working directory otherwise.

## One document

`AssetServer::save_project` writes a single pretty-printed JSON file, keys in write order:

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
      "path": "models/3862017159553017004.smodel",
      "folder": "",
      "hdr": false,
      "linear": false
    }
  ],
  "assetFolders": [],
  "scene": { "version": 4, "environment": {}, "entities": [] },
  "renderSettings": {
    "aa": "taa", "exposureEv": 0.0, "clustered": true, "depthPrepass": true,
    "shadows": true, "ibl": true, "quality": "high", "tonemap": "agx",
    "ddgi": true, "gdf": true, "skyOcclusion": true, "rtShadows": false, "restir": false
  },
  "editorCamera": { "position": { "x": 3.0, "y": 2.5, "z": 4.0 }, "yaw": -37.0, "pitch": -29.0, "fov": 45.0 },
  "debugOverlays": { "bounds": false, "sceneAabb": false, "lightVolumes": false, "grid": true, "colliders": false }
}
```

`catalog_to_json` writes one row per `AssetEntry`. Every row carries
`id`/`name`/`type`/`path`/`folder`/`hdr`/`linear`; the container linkage (`container`/`chunk`),
texture `colorspace` and `role`, animation `duration`/`tracks`, the `rigged` flag, store
`attribution`, and `contentHash` appear only when non-default, so a standalone row stays minimal.
The type is a string via `asset_type_name`, readable and stable across enum reordering.
`assetFolders` persists the user-created catalog folders as a string array.

The `scene` block (shown empty above) is the registry-driven `Scene::scene_to_json` document —
its own `version` and migrations are described in
[scene serialization](../../scene-and-ecs/scene-serialization/). The top-level `version` is gated
on load: any value other than `PROJECT_VERSION` is a typed `Error::BadProjectVersion`, never a
best-effort parse. The reader is otherwise lenient; unknown keys are ignored and missing keys
take defaults.

Catalog paths are relative to `<root>/assets`, and the absolute root is set when the project
opens. GPU caches are not saved; the new scene's ids re-resolve against the new catalog and
[upload lazily](../asset-server-and-catalog/) as they are first drawn.

## Render settings and the sidecar blocks

`renderSettings` snapshots the renderer state the editor's render panel drives: the
[AA mode](../../anti-aliasing/aa-modes/), exposure, the
[quality tier](../../screen-space-and-post/render-quality-tiers/), the tonemap operator, and the
feature toggles (clustered, depth prepass, shadows, IBL, DDGI, GDF, sky occlusion, RT shadows,
ReSTIR). On load each field is a patch: a missing or mistyped field keeps the current value, and
the RT toggles apply only on a device that reports ray-tracing support.

The [editor camera](../../ui-and-editor/editor-camera/), the
[debug overlays](../../ui-and-editor/debug-visualization/), and the enabled
[asset-store connectors](../../asset-store-and-connectors/connector-framework/) travel as opaque
`editorCamera` / `debugOverlays` / `stores` blocks in a `ProjectSidecar`. They belong to
`saffron-sceneedit` and the editor, so the asset crate never interprets them: each is written only
when it is a JSON object and handed back (or JSON null) on load for the caller to apply.

## Save and load through a seam

The renderer-touching steps go through one trait, `ProjectHost`: `wait_gpu_idle`,
`render_settings_to_json`, and `apply_render_settings`. It keeps `saffron-assets` decoupled from
the live renderer. The control plane implements it as `RendererProjectHost` over the real `Renderer`;
tests implement a recording stub and assert the ordering without a Vulkan device.

`AssetServer::load_project` keeps a strict order: parse and version-gate, `wait_gpu_idle`, clear
the GPU asset caches, set the asset root, ensure `src/` + `library/`, rebuild the catalog from the
doc, reconcile it against the disk scan, apply render settings, then `scene_from_json`. The
reconcile (`load_catalog`) treats the filesystem as the source of truth, so a never-saved import
is rediscovered and a deleted file's row is dropped.

> [!WARNING]
> The GPU idle must come before the cache clear. The caches hold the last `Arc` to uploaded
> `GpuMesh`/`GpuTexture` resources; dropping one while an in-flight frame reads its Vulkan
> buffers is a use-after-free. The idle is the ordering guarantee.

This synchronous path boots `saffron-player`, where blocking is correct because there is no
control plane to keep responsive. The host instead brings projects up through the non-blocking
per-frame loader described in [project loading](../../tooling-and-control/project-loading/): an
off-thread `ProjectDocWorker` runs the parse and disk scan, and a bounded main-thread install step
applies the same idle → clear → swap discipline.

## Startup and commands

`bootstrap_project_from_env` seeds the loader from the environment the editor sets. `SAFFRON_PROJECT`
opens the named project, or creates it when the name is valid and no such project exists;
`SAFFRON_SCRATCH_PROJECT` creates a deterministic per-shell scratch project, named `scratch-<hash>`
by an [FNV-1a](https://www.rfc-editor.org/rfc/rfc9923) fold of the working directory and control
socket. With neither set, a `project.json` in the working directory is opened; otherwise the host
waits for the editor's project picker.

| Command | Does |
|---|---|
| `get-project` | returns the active project identity (name, paths, display name) |
| `project-status` | returns the live load phase, boot stage, and progress |
| `cancel-load` | aborts the in-flight project load |
| `new-project` | creates and opens a project (seeds the loader, returns a status snapshot) |
| `open-project` | opens by name, project root, or `project.json` path (via the loader) |
| `load-project` | opens like `open-project`, defaulting to `./project.json` |
| `reload-project` | closes and re-opens the active project |
| `save-project` | writes the document synchronously; the path defaults to the active project |
| `create-script` | writes a boilerplate `.lua` under the project `src/` |

`save-project` on a host with no active project adopts the target path as the project identity,
deriving the name from the parent directory. The load-side commands answer immediately with a
`ProjectStatusDto` and refuse to run during play mode or an asset preview.

## Project-local assets and the path fixup

`import-model` bakes into `assets/models/<uuid>.smodel` and `import-texture` copies into
`assets/textures/<uuid>.<ext>`; both require a loaded project, so an import can never write into
the engine's bundled asset directory. A standalone-mesh row whose file is absent under its
recorded `meshes/` path is retried under `assets/models/`, where the importer writes baked
`.smesh` siblings (`standalone_mesh_path`).

## In the code

| What | File | Symbols |
|---|---|---|
| Save / load / create | `assets/src/project.rs` | `AssetServer::save_project`, `AssetServer::load_project`, `AssetServer::create_project`, `PROJECT_VERSION` |
| Renderer seam + sidecar | `assets/src/project.rs` | `ProjectHost`, `ProjectSidecar` |
| Names + roots | `assets/src/project.rs` | `valid_project_name`, `default_display_name`, `project_json_path`, `app_data_root`, `project_userdata_root` |
| Script scaffold | `assets/src/project.rs` | `ensure_script_src`, `ensure_script_library`, `create_project_script` |
| Scratch projects | `assets/src/project.rs` | `scratch_project_name`, `AssetServer::create_scratch_project` |
| Catalog ↔ JSON | `assets/src/catalog.rs`; `assets/src/names.rs` | `catalog_to_json`, `catalog_from_json`, `catalog_folders_to_json`, `asset_type_name` |
| Render settings block | `rendering/src/render_settings.rs` | `Renderer::render_settings_to_json`, `Renderer::apply_render_settings` |
| Scene half | `scene/src/document.rs` | `Scene::scene_to_json`, `Scene::scene_from_json`, `SCENE_VERSION` |
| Bootstrap + commands | `control/src/commands_asset.rs` | `bootstrap_project_from_env`, `register_asset_commands`, `RendererProjectHost` |
| Path fixup | `assets/src/load.rs` | `standalone_mesh_path` |

## Related

- [Asset catalog](../asset-server-and-catalog/) — the caches and catalog this document persists
- [Project loading](../../tooling-and-control/project-loading/) — the host's non-blocking bring-up of this file
- [Scene serialization](../../scene-and-ecs/scene-serialization/) — the `scene` block's format and migrations
- [Import pipeline](../import-pipeline/) — fills `assets/` and the catalog
- [Asset commands](../../tooling-and-control/asset-commands/) — the project commands over the CLI
