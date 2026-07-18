+++
title = 'Asset catalog'
weight = 6
+++

# Asset catalog

Anima components identify assets with stable [`Uuid`](../scene-serialization/) values. The asset catalog resolves each ID to the metadata needed by loaders, tools, and the editor: a display name, asset kind, project-relative path, and type-specific details.

The catalog is project data rather than entity data. `AssetServer` owns the live catalog and the GPU caches keyed by the same IDs. A `Scene` can hold an `Option<Arc<AssetCatalog>>` for read-only lookup, but the ECS world does not own or serialize catalog entries.

## Catalog rows

`AssetCatalog` stores ordered `entries`, user-created `folders`, and a `by_id` index. Each `AssetEntry` has an ID, name, `AssetType`, folder, and path below the project's `assets/` directory.

The asset type is one of `Mesh`, `Texture`, `Other`, `Animation`, `Material`, `Model`, or `Lut`. Extra fields describe only the kinds that need them:

| Asset kind | Additional catalog data |
|---|---|
| Texture | HDR and linear flags, colorspace, semantic role |
| Animation | Duration and animated-track count |
| Model sub-asset | Container ID and chunk index |
| Rigged model | Rigged flag |
| Store import | Author, source, license, and attribution requirement |
| Thumbnail source | Content hash |

A model container and its embedded meshes, materials, textures, and clips each have catalog IDs. Sub-asset rows share the container path and use `container` plus `chunk` to locate their payload inside the `.smodel` file.

## Stable lookup

`AssetCatalog::find` performs an indexed ID lookup. `put` replaces an existing row in place or appends a new row and records its index. `rename`, `set_attribution`, and `set_content_hash` update a row when the ID exists. `unique_name` adds ` (2)`, ` (3)`, and higher suffixes when a display name is already in use.

```rust
use saffron_core::Uuid;
use saffron_scene::{AssetCatalog, AssetEntry, AssetType};

let mut catalog = AssetCatalog::default();
let mesh_id = Uuid::new();
catalog.put(AssetEntry {
    id: mesh_id,
    name: catalog.unique_name("Crate"),
    asset_type: AssetType::Mesh,
    path: format!("meshes/{}.smesh", mesh_id.value()),
    ..AssetEntry::default()
});

assert_eq!(catalog.find(mesh_id).map(|row| row.name.as_str()), Some("Crate"));
```

Names are labels, not identities. Two source files may have the same stem, while component references remain unambiguous because they store IDs.

## Persistence and recovery

`project.json` stores catalog rows in `assets` and folders in `assetFolders`. The JSON reader ignores unknown keys, defaults omitted optional fields, and skips rows with an ID of zero.

The `assets/` tree remains sufficient to rebuild the catalog. Engine-authored files encode identity in their filename or container metadata; foreign files receive a neighboring `.smeta` sidecar. A load reuses `assets/.cache/catalog.json` only when its asset signature matches. Otherwise, Anima scans the directory and writes a fresh cache.

This split gives each layer one job:

| Layer | Responsibility |
|---|---|
| Scene component | Stores an asset ID. |
| `AssetCatalog` | Maps the ID to project metadata. |
| `AssetServer` | Owns the catalog and resolves CPU/GPU resources. |
| Control plane | Returns catalog DTOs to the editor and applies asset operations. |
| React editor | Presents names, folders, previews, and pickers. |

## Source map

| What | File | Symbols |
|---|---|---|
| Catalog types and lookup helpers | `engine/crates/scene/src/environment.rs` | `AssetCatalog`, `AssetEntry`, `AssetType` |
| Optional scene handle | `engine/crates/scene/src/scene.rs` | `Scene::catalog` |
| Live owner and resource caches | `engine/crates/assets/src/lib.rs` | `AssetServer` |
| Project JSON shape | `engine/crates/assets/src/catalog.rs` | `catalog_to_json`, `catalog_from_json` |
| Disk reconciliation and cache | `engine/crates/assets/src/scan.rs` | `reconcile_catalog_from_disk`, `resolve_catalog_from_disk` |
| Editor-facing catalog commands | `engine/crates/control/src/commands_asset.rs` | `list-assets`, `rename-asset`, `move-asset` |

## Related

- [Asset server and catalog](../../geometry-and-assets/asset-server-and-catalog/)
- [Project serialization](../../geometry-and-assets/project-serialization/)
- [Asset pickers and drag-drop](../../ui-and-editor/asset-pickers-and-drag-drop/)
- [Built-in components](../built-in-components/)
