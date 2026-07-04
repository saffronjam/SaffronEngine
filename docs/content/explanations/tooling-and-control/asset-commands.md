+++
title = 'Asset commands'
weight = 5
+++

# Asset commands

The asset commands are the control-plane verbs that manage a project's assets: importing models and
textures, browsing and organizing the catalog, binding assets onto entities, and saving or loading the
project. They act on both the `AssetServer` — the catalog and its GPU caches — and the scene. The
`screenshot` and `quit` commands sit alongside them to complete a scriptable session.

## Import and catalog

| Command | Params | Effect |
|---|---|---|
| `import-model` | `{path}` | Imports + bakes a model into the catalog as a `.smodel` container. Returns the model `{id, name, …}`. |
| `instantiate-model` | `{asset, name?}` | Spawns an entity from a catalog model (selected). Returns `{id, name}`. |
| `import-texture` | `{path}` | Imports an image into the asset dir; returns `{texture: id}` to assign later. |
| `list-assets` | — | Returns every catalog entry as `{id, name, type, path, folder?}` plus `folders`. |
| `scan-assets` | — | Rescans the `assets/` dir and reconciles the catalog from disk. |
| `rename-asset` | `{asset, name}` | Renames a catalog entry (selected by id or current name). |
| `create-asset-folder` | `{folder}` | Creates a project-saved virtual folder. |
| `rename-asset-folder` | `{folder, name}` | Renames a virtual folder and updates assets assigned to it. |
| `delete-asset-folder` | `{folder}` | Deletes a virtual folder and moves assigned assets back to root. |
| `move-asset` | `{asset, folder?}` | Moves an asset into a virtual folder, or back to root when `folder` is omitted. |
| `asset-usages` | `{asset}` | Lists scene/environment slots that reference an asset. |
| `delete-asset` | `{asset}` | Deletes the catalog entry and imported file, clears usages, and returns what was cleared. |
| `assign-asset` | `{entity, slot, asset}` | Sets the entity's mesh, or writes a texture into its material slot 0's overrides. |

`import-model` bakes the source into a `.smodel` container in the catalog; `instantiate-model` is the
command that spawns — it resolves the catalog model and creates a selected entity carrying it.
`import-texture` adds to the catalog alone; the result is attached later with `assign-asset` or by
writing the texture id into a `MaterialSet` slot override with `set-component-field`. `assign-asset`
takes `slot` (one of `mesh`, `albedo`, `metallicRoughness`, `normal`, `occlusion`, `emissive`,
`height`), resolves the asset by id or name, and — for a texture slot — writes the id as an override
on the entity's material slot 0 (the packed ORM means `metallicRoughness` and `occlusion` share
`ormTexture`); `mesh` writes `Mesh.mesh`, adding the component if the entity lacks it.

Folders are catalog metadata, not filesystem directories. They are saved next to the asset list so
empty folders survive a reload. Renaming a folder updates the folder list and each catalog entry
assigned to the old name. Deleting a folder only removes that virtual folder; assigned assets move
back to root. `delete-asset` clears the scene references (mesh, material textures, sky texture) before
removing the entry and cache records.

`import-texture`, `rename-asset`, and `move-asset` persist an asset's name / folder / colorspace to a
co-located [`.smeta` sidecar](../../geometry-and-assets/asset-server-and-catalog/#the-smeta-sidecar)
the moment they run, so those edits survive a cold catalog scan even if you never save the project —
`delete-asset` removes the sidecar alongside the file.

## Thumbnails and previews

| Command | Params | Effect |
|---|---|---|
| `get-thumbnail` | `{asset, size=128}` | Renders a small preview of a catalog asset; returns the PNG as base64. |
| `view-asset` | `{asset, size=512}` | Same as `get-thumbnail` at a larger default size, for a full-asset look. |
| `thumbnail-cache` | `{action: stats\|clear}` | Inspects (`{entries, bytes}`) or empties the project's thumbnail disk cache. |

Both resolve the `asset` by id or name and return `{format: "png", width, height, base64, pending}`:
the encoded image bytes inline in the JSON result, so a remote UI can show a preview without sharing a
filesystem. The asset's type selects the path. A **mesh** is drawn as a framed 3D render through the
renderer's `render_mesh_thumbnail`, the same preview the Assets panel tiles use. A **texture** is the
image itself, GPU-downscaled to fit the requested size before being read back, so the cost is bounded
by `size`, not the source resolution; an HDR texture is tonemapped on the way out. The command reaches
the renderer through the `ControlRenderer::with_thumbnail_gpu` seam, which hands a transient
`ThumbnailGpu` to `request_thumbnail`.

A generated PNG is written to a persistent **disk cache** at `<projectRoot>/cache/thumbnails/`, keyed
on the asset uuid, the requested size, and a stamp of the source. A hit reads the PNG straight off
disk — no GPU work — so a warm start is disk reads, not regeneration. Edits self-invalidate through
the stamp; `delete-asset` purges an asset's cached PNGs, and `thumbnail-cache clear` empties the dir on
demand.

A cold miss does the real work — decode, upload, render, readback — on the asset crate's
**thumbnail worker thread**, not the per-frame control drain, since a 4k HDR is ~1 s of it. The
command enqueues a job and replies `pending: true` immediately; the worker writes the PNG to the disk
cache and the client retries (with backoff) until the retry is a cache hit. Uploaded textures/meshes
are handed back to the main thread and folded into the in-session caches. The worker is the asset
crate's sole cross-thread site.

## Save and load

| Command | Params | Effect |
|---|---|---|
| `save-scene` | `{path}` | Writes the scene (entities + components) to `path`. |
| `load-scene` | `{path}` | Reads a scene file; clears selection. |
| `save-project` | `{path?}` | Writes the asset catalog + scene + render settings for the active project. |
| `load-project` | `{path?}` | Reads catalog + scene; clears selection. |
| `open-project` | `{path}` | Opens a project by path. |
| `new-project` | `{name}` | Creates a fresh project. |

The project commands hold the catalog and scene together, which is what `load-project` needs so that
mesh and texture UUIDs in the scene resolve against the catalog it just loaded. A project load idles
the GPU and clears the thumbnail-worker queue before swapping the caches, so dropping a cached GPU
resource never frees one a frame still reads.

## Session control

| Command | Params | Effect |
|---|---|---|
| `screenshot` | `{target: viewport\|window, path}` | Writes a PNG. `viewport` is captured immediately; `window` is deferred to end-of-frame. |
| `quit` | — | Sets the window's should-close flag, ending the run loop. |

`screenshot` reports `pending`: `false` for a viewport grab, done synchronously, and `true` for a
window grab, written when the current frame presents. The [capture](../screenshots-and-capture/) path
has its own page.

## In the code

| What | File | Symbols |
|---|---|---|
| Registration | `engine/crates/control/src/commands_asset.rs` | `register_asset_commands` |
| Import + spawn | `engine/crates/control/src/commands_asset.rs` | the `import-model`, `instantiate-model`, `import-texture` rows; `AssetServer::instantiate_model` |
| Catalog | `engine/crates/control/src/commands_asset.rs` | the `list-assets`, `scan-assets`, `rename-asset`, folder, `move-asset`, `asset-usages`, `delete-asset`, `assign-asset` rows; `AssetSlotDto` |
| Thumbnails | `engine/crates/control/src/commands_asset.rs`, `engine/crates/assets/src/lib.rs` | `get-thumbnail`/`view-asset`/`thumbnail-cache`; `request_thumbnail`, `ThumbnailWorker`, the disk-cache helpers |
| Project IO | `engine/crates/control/src/commands_asset.rs` | the `save-project`/`load-project`, `save-scene`/`load-scene`, `open-project`/`new-project` rows |
| Capture + quit | `engine/crates/control/src/commands_asset.rs` | the `screenshot` and `quit` rows; `ControlRenderer::capture_viewport`, `request_window_capture` |

## Related
- [Capture](../screenshots-and-capture/) — the PNG capture path behind `screenshot` and the thumbnail readback
- [Shared types](../shared-types/) — the base64-PNG result shape and the wire contract
- [Scene commands](../scene-commands/) — `set-component-field` is the other way to set albedo
- [Geometry & assets](../../geometry-and-assets/) — import and the asset catalog
