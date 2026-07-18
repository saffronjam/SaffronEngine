+++
title = 'Asset commands'
weight = 5
+++

# Asset commands

Asset commands expose the project catalog, import pipeline, material assets, preview scenes, and
project lifecycle through the control plane. Most selectors accept either a decimal UUID or an exact
catalog name. Commands that need project storage reject the request until a project is ready.

## Command families

`register_asset_commands` contains several related surfaces rather than one flat import API:

| Family | Representative commands | Purpose |
|---|---|---|
| Project | `get-project`, `new-project`, `open-project`, `save-project`, `reload-project` | Project identity, persistence, and lifecycle |
| Load progress | `project-status`, `cancel-load` | Poll or cancel asynchronous project loading |
| Import | `import-model`, `import-texture`, `import-lut`, `reimport-model` | Add or refresh baked assets |
| Model containers | `model-info`, `get-asset-model`, `extract-subasset`, `clear-extraction` | Inspect and manage embedded model data |
| Scene placement | `instantiate-model`, `asset-placement`, `assign-asset` | Create entities and bind catalog assets |
| Catalog | `list-assets`, `scan-assets`, `probe-asset`, `asset-references` | Browse metadata and dependency edges |
| Organization | `rename-asset`, `move-asset`, `create-asset-folder` | Maintain names and virtual folders |
| Cleanup | `asset-usages`, `clean-assets`, `delete-unused`, `delete-asset` | Find references and remove data |
| Materials | `material-create`, `material-update`, `material-set-graph`, `material-cook` | Author, assign, preview, and compile materials |
| Preview | `enter-asset-preview`, `exit-asset-preview`, `get-thumbnail`, `view-asset` | Interactive and tile-sized asset views |
| Output | `save-scene`, `load-scene`, `export-app`, `screenshot`, `quit` | Scene files, app export, capture, and shutdown |

For example, importing a model and placing an instance are separate operations:

```sh
sa import-model ./assets-source/Robot.glb
sa instantiate-model Robot
sa model-info Robot
```

`import-model` bakes a `.smodel` container and adds its model and embedded sub-assets to the catalog.
`instantiate-model` resolves that catalog entry, creates its entity forest in the active scene,
selects the root, and increments the scene version. `asset-placement` uses a three-phase
preview/commit/clear protocol for drag placement; its temporary subtree carries `PreviewGhost` and
does not serialize.

`import-texture` accepts optional colorspace and semantic-role hints, then uploads through the
renderer-owned `GpuUploader`. `import-lut` imports a `.cube` creative look. Model reimport preserves
the container identity while reporting updated, added, removed-from-source, and skipped sub-assets.
An embedded sub-asset can be promoted to a standalone file with `extract-subasset` and returned to
container ownership with `clear-extraction`.

## Catalog durability and references

Catalog folders are virtual paths stored as metadata. Renaming a folder rewrites that prefix for its
descendant folders and assigned assets. Deleting a folder removes its descendant folder rows and
moves affected assets to the catalog root. `rename-asset`, `move-asset`, and folder operations write
`.smeta` sidecars for the affected standalone assets, so a filesystem scan can recover those names
and folders.

`scan-assets` idles the GPU, clears asset caches, reconciles the catalog with the assets directory,
and writes the catalog cache. `probe-asset` returns file size and type-specific counts such as mesh
vertices and triangles. `model-info` reads container import metadata and sub-asset sizes, while
`get-asset-model` returns the capabilities, bone tree, and animation clips used by the asset editor.

Two reference queries answer different questions. `asset-usages` scans the active scene for mesh,
material, and environment-sky assignments. `asset-references` builds the wider asset dependency graph,
including container and material relationships, and reports both directions plus recursive footprint.

`clean-assets` analyzes that graph without deleting anything. `delete-unused` accepts explicit IDs
from the report and requires `confirm: true`; it deletes only entries still classified as unused.
`delete-asset` clears direct scene usages, removes the catalog row and owned file, drops its `.smeta`
sidecar, and invalidates relevant caches. Content-addressed thumbnail PNGs remain available for other
assets with identical content and age out through cache eviction.

## Assignment and materials

`assign-asset` writes a mesh reference or a texture override on material slot 0. The texture slots
are `albedo`, `metallic-roughness`, `normal`, `occlusion`, `emissive`, and `height`; metallic-roughness
and occlusion both target the packed `ormTexture` field. Passing `0` clears the assignment.

The `material-*` commands operate on native `.smat` assets. They cover creation, folder import,
catalog listing, parameter updates, entity assignment, graph replacement, instances, typed
overrides, shader compilation, and project-wide cooking. `preview-render` renders one material to an
inline PNG. See [native materials](../../materials-and-pipelines/native-materials/) and
[node-graph code generation](../../materials-and-pipelines/node-graph-codegen/) for the data and
shader paths behind these commands.

## Interactive previews and thumbnails

`enter-asset-preview` builds an isolated scene and switches the renderer to the `assetPreview` view.
Models use their full entity forest; materials and ordinary textures use a furnished sphere; HDRIs
light a three-sphere environment rig. Built-in primitives use their reserved mesh IDs. The command
stores the authored camera, selection, overlay, and exposure so `exit-asset-preview` can restore them.

`get-thumbnail` and `view-asset` share `request_thumbnail`, with default sizes of 128 and 512 pixels.
A cache hit returns an inline base64 PNG. A miss returns `pending: true` and enqueues a preview render;
the host drains up to two jobs per update through the main forward+ graph, writes their PNGs, and the
client polls again.

The cache is app-wide at `<appDataRoot>/thumbnail-cache`. Its key combines cache version, resolved
content hash, and requested size, so identical content can share a tile across assets and projects.
Material keys include resolved parameter state. The cache evicts oldest files after exceeding 1 GiB
until usage falls to 80 percent. `thumbnail-cache` reports its entry/byte totals or clears it.

## Project and session boundaries

`new-project`, `open-project`, `load-project`, and `reload-project` seed the non-blocking
[project loader](../project-loading/) and return its status. They require Edit mode and an exited
asset preview. `save-project` writes the catalog, scene, renderer settings, editor camera, debug
overlays, and enabled store connectors to the chosen project path.

`save-scene` and `load-scene` operate on the scene document alone. `export-app` cooks graph materials
and stages the player, project data, scripts, shaders, manifest, and required runtime libraries in an
application folder. `screenshot` and `quit` complete scriptable sessions; the
[capture page](../screenshots-and-capture/) covers viewport and window readback timing.

## In the code

| What | File | Symbols |
|---|---|---|
| Command registration | `control/src/commands_asset.rs` | `register_asset_commands`, `resolve_asset` |
| Model and dependency management | `assets/src/manage.rs` | `reimport_model`, `extract_sub_asset`, `build_dependency_graph`, `analyze_clean` |
| Import pipeline | `assets/src/import.rs` | `AssetServer::import_model`, `AssetServer::import_texture` |
| Interactive preview | `control/src/commands_asset.rs` | `enter_asset_preview`, `install_preview_scene`, `PreviewSubject` |
| Thumbnail request | `assets/src/thumbnail.rs` | `request_thumbnail`, `PreviewRenderJob`, `write_thumbnail_cache` |
| Thumbnail render drain | `host/src/layer.rs` | `drive_preview_render_queue` |
| Project load requests | `sceneedit/src/context.rs` | `ProjectLoadRequest`, `ProjectLoadProgress` |

## Related

- [Project loading](../project-loading/) — worker preparation, installation, and progress polling.
- [Capture](../screenshots-and-capture/) — screenshot targets and frame-boundary readback.
- [Shared types](../shared-types/) — command DTOs and UUID wire encoding.
- [Asset editor](../../ui-and-editor/asset-editor/) — the UI built on model and preview commands.
- [Asset server and catalog](../../geometry-and-assets/asset-server-and-catalog/) — catalog storage and sidecars.
