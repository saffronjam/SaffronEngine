+++
title = 'Asset commands'
weight = 5
+++

# Asset commands

Asset commands expose the project catalog, import pipeline, material assets, preview scenes, and
project lifecycle through the control plane. Most selectors accept either a decimal UUID or an exact
catalog name. Commands that need project storage reject the request until a project is ready.

## Command families

The asset control module contains several related surfaces rather than one flat import API:

| Family | Representative commands | Purpose |
|---|---|---|
| Project | `get-project`, `new-project`, `open-project`, `save-project`, `reload-project` | Project identity, persistence, and lifecycle |
| Load progress | `project-status`, `cancel-load` | Poll or cancel asynchronous project loading |
| Import | `import-model`, `import-texture`, `import-lut`, `import-vegetation-asset` | Add native and baked assets |
| Model containers | `model-info`, `get-asset-model`, `extract-subasset`, `clear-extraction` | Inspect and manage embedded model data |
| Scene placement | `instantiate-model`, `asset-placement`, `assign-asset` | Create entities and bind catalog assets |
| Catalog | `list-assets`, `scan-assets`, `probe-asset`, `asset-references` | Browse metadata and dependency edges |
| Vegetation | `vegetation-cook`, `vegetation-manifest`, `plant-recook` | Cook and inspect plant, biome, and map data |
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

`import-vegetation-asset` reads an authored `.splant`, `.sbiome`, or complete `.svegmap` package.
It preserves the asset's stable identity so references between separately imported families, biomes,
and maps remain valid. `vegetation-asset-summary` returns the native typed summary and ordered map
layers used by the editor workspace. Each summary includes validation, source attribution, exact
dependency identities, and attributable cook statistics. A map summary also reports its dirty
layers: those whose authored chunks differ from what the current manifest consumed (every layer
when no manifest exists), meaning a recook would change the cooked output.

`plant-validate` resolves the retained source recipe through the plant compiler without publishing.
Its result contains structured diagnostics, observed source hashes, license provenance, reimport
conflicts, and the exact dependencies used for the cook key. `plant-recook` runs the same compiler,
publishes a validated `.splantc`, and accepts changed source hashes only after publication succeeds.

`vegetation-map-layer-commit` edits the authored map's ordered layers as one optimistic
transaction: complete replacement rows (each with a bumped per-layer revision) plus removals,
validated against the root generation the editor captured — a concurrent edit rejects instead of
interleaving. The committed root publishes last, so a reader never sees a partial transaction.

`vegetation-map-chunk-read` and `vegetation-map-chunk-commit` carry a brush gesture's
read-modify-write over the authored chunks. The read resolves logical keys (layer + tile + kind)
through the map root's inventory and returns the root generation with every present chunk — an
absent key contributes no row. The commit replaces the gesture's touched field tiles and anchor
chunks across cells atomically under that captured generation, through the same optimistic
transaction as the layer commit.

`vegetation-cook` queues the single staged world-cooking route for an entire map, explicit bounds, or
an explicit cell set. `vegetation-cook-status` reports monotonic progress and terminal output;
`vegetation-cancel-cook` requests cooperative cancellation. A newer cook for the same map supersedes
the older queued or running job.

`vegetation-rejections` reads one cooked cell's rejected candidates: each row carries the sampled
world position, the rejection reason, and the sampler ordinal — the rejection facet stores every
candidate's exact position, so diagnostics can place rejections in the world.

`vegetation-topology-diff` compares two cooked manifests per cell: plants added, removed, and
moved between the identities (unchanged cell artifacts skip by hash), plus unresolved authored
overrides — anchors, pins, and transform/state overrides whose referenced plant is absent from the
newer manifest. The editor's Review action drives it across the two most recent cooks.

`vegetation-manifest` reads the current generation or an exact immutable manifest identity.
`vegetation-cell-inspect` validates one cell through that manifest and returns its header, content
identity, counts, and complete section table. These inspection commands do not make generated
`.splantc` or `.svegcell` files catalog assets.

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
material, environment-sky, and vegetation-map assignments. `asset-references` builds the wider asset
dependency graph, including container, material, plant, biome, and map relationships. It reports both
directions plus recursive footprint.

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
overrides, shader compilation, and project-wide cooking. A material's studio-sphere render is a
thumbnail request like any other asset's — `get-thumbnail` answers with an inline PNG, or `pending`
while the host converges the tile, so the caller repolls. See [native materials](../../materials-and-pipelines/native-materials/) and
[node-graph code generation](../../materials-and-pipelines/node-graph-codegen/) for the data and
shader paths behind these commands.

## Interactive previews and thumbnails

`enter-asset-preview` builds an isolated scene and switches the renderer to the `assetPreview` view.
Models use their full entity forest; materials and ordinary textures use a furnished sphere; HDRIs
light a three-sphere environment rig. Built-in primitives use their reserved mesh IDs. A plant
family compiles through its retained recipe (content-addressed, so an unchanged family reuses its
artifact) and previews its renderable form with the family's material slots. The command stores the
authored camera, selection, overlay, and exposure so `exit-asset-preview` can restore them.

`get-thumbnail` and `view-asset` share `request_thumbnail`, with default sizes of 128 and 512 pixels.
A cache hit returns an inline base64 PNG. A miss returns `pending: true` and enqueues a preview render;
the host drains up to two jobs per update through the main forward+ graph, writes their PNGs, and the
client polls again.

A plant family renders its compiled form on the studio floor, like a mesh or model tile. Biome and
vegetation-map misses rasterize their canonical vector type icons immediately; they use the same
content-addressed cache and return a completed PNG in the first reply.

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
| Vegetation cook commands | `control/src/commands_vegetation.rs` | `register_vegetation_commands` |
| Vegetation cook jobs | `control/src/vegetation_cook_jobs.rs` | `VegetationCookJobs` |
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
- [Vegetation assets](../../geometry-and-assets/vegetation-assets/) — native plant, biome, and map packages.
