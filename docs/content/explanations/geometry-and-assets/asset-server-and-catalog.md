+++
title = 'Asset server and catalog'
weight = 6
+++

# Asset server and catalog

An asset catalog is a named registry that maps stable ids to files on disk, so what a project
references is decoupled from where the bytes live. A component stores a `Uuid`; the catalog
resolves it to a name, a type, and a relative path, and renaming or moving the file never breaks
the reference. Around the catalog sits the asset server, which turns an id into a live GPU
resource.

## The server

`AssetServer` owns the project's assets: the catalog plus id-keyed caches, so entities sharing an
asset load and upload it once.

```rust
pub struct AssetServer {
    pub root: PathBuf,                               // the project's assets/ dir
    pub catalog: AssetCatalog,                       // id -> {name, type, path, ...}
    pub mesh_by_uuid: AssetCache<GpuMesh>,           // uploaded meshes
    pub mesh_bvh_by_uuid: AssetCache<MeshBvh>,       // lazily built ray-pick BVHs
    pub texture_by_uuid: AssetCache<GpuTexture>,     // uploaded textures
    pub model_by_uuid: AssetCache<ModelAsset>,       // opened .smodel containers
    pub material_by_uuid: AssetCache<MaterialAsset>, // parent-resolved .smat state
    // + the codegen-shader memo, the preview-render queue, the thumbnail cache root
}
```

The catalog is the source of truth; every map is a cache over it. The catalog records that asset
42 is a mesh at `models/42.smesh`; the mesh cache records that 42 is uploaded and holds its
`Arc<GpuMesh>`. `AssetServer::new` seeds the root's `models/`, `textures/`, `materials/`, and
`environments/` subdirectories, and [loading a project](../project-serialization/) populates the
catalog from a disk scan.

A project switch drops every cache through `clear_asset_caches`, and its caller idles the GPU
first: an in-flight frame may still reference a cached `Arc<GpuTexture>`, so the last `Arc` must
drop only under an idle device.

## The catalog

Each entry pairs a human-facing, renameable name with a separate on-disk path:

```rust
pub enum AssetType {
    Mesh, Texture, Other, Animation, Material, Model, Lut, Environment,
}

pub struct AssetEntry {
    pub id: Uuid,
    pub name: String,          // UTF-8, renameable, user-facing
    pub asset_type: AssetType,
    pub path: String,          // relative to the asset root
    pub container: Uuid,       // 0 = standalone; else the owning container's id
    pub chunk: i32,            // TOC chunk index inside it (-1 = standalone)
    pub colorspace: Colorspace,
    pub role: TextureRole,     // albedo/normal/.../hdri: routes the preview
    pub content_hash: u64,     // FNV-1a of the baked bytes; the thumbnail key
    pub attribution: Option<Attribution>, // store source + license, if imported
    // + folder, hdr/linear flags, animation duration/tracks, rigged
}

pub struct AssetCatalog {
    pub entries: Vec<AssetEntry>,
    pub folders: Vec<String>,
    pub by_id: HashMap<u64, usize>,  // id -> index into entries
}
```

`AssetCatalog::put` inserts or replaces by id and keeps `by_id` in sync; `find` resolves an id to
an `Option<&AssetEntry>`; `rename` renames in place; `unique_name` appends ` (2)`, ` (3)`, … so
two imports of `cube.gltf` get distinct names. The type lives in `saffron-scene`, not
`saffron-assets`, so the inspector reads it without depending on the renderer; the asset layer
hands the scene a shared read-only handle (see
[asset catalog in the scene](../../scene-and-ecs/asset-catalog-in-scene/)).

`container` and `chunk` link a sub-asset to its owning [`.smodel`](../smodel-container/): one row
is the model, and sibling rows are the meshes, materials, and textures embedded inside it. A
texture's `role` is inferred from filename tokens at import, or supplied by a store connector, and
routes its preview — a map renders on a lit sphere in its slot, an HDRI as an environment (see the
[asset editor](../../ui-and-editor/asset-editor/)).

An environment profile is a standalone `.senv` asset containing one complete scene environment.
Its catalog identity lets the Environment panel browse, rename, move, delete, and update it through
the same asset-management surface as other project data.

## The filesystem is the source of truth

The catalog is derived from a scan, not authored by hand. `reconcile_catalog_from_disk` walks
`assets/` in sorted order, skipping `.cache/`. A `.smodel` container (or a `.smatx`
texture-embedding material) contributes a parent row plus one row per embedded sub-asset from a
prefix read of its metadata; an engine-written standalone file is recognized by its decimal-uuid
filename stem; a foreign file identifies through its `.smeta` sidecar. A deleted file's row
drops, and an import you never saved is rediscovered.

`load_catalog` is the fast path: `assets/.cache/catalog.json` memoizes the scan, keyed by a
signature — an [FNV-1a](https://datatracker.ietf.org/doc/html/rfc9923) fold over every file's
`(path, mtime, size)`, stat-only. A matching signature reuses the cached rows and skips every
container read; anything else falls back to the cold scan and rewrites the cache. The cache is a
latency shortcut only: delete it and the next load rebuilds an identical catalog.

Both are scriptable from a shell:

```sh
sa scan-assets           # reconcile the live catalog against disk
# { "added": 2, "removed": 0 }
sa list-assets           # type, name, id per row
#   model     helmet        4462050936982726040
#   mesh      helmet_mesh   4462050936982726041
```

### The `.smeta` sidecar

A file's bytes carry its geometry, not its display name. The editable metadata — `name`,
`folder`, and a texture's `colorspace` and `role` — lives beside the file in a `<path>.smeta`
JSON sidecar, written eagerly: an import mints it, and `rename-asset` / `move-asset` rewrite it,
so the metadata is durable the instant it changes, with no project save.

After the walk, `apply_sidecar_overrides` overlays each sidecar onto the row whose id it names.
The sidecar wins over the `project.json` seed, which is only as fresh as the last save, so a
never-saved rename survives a restart and a linear normal map cannot rescan as sRGB. A foreign
file with no identity in its own bytes (a raw `.png` dropped into `assets/`) also takes its
stable id from the sidecar, minted on first sight.

The id check keeps a container's sidecar from bleeding onto an embedded sub-asset that shares its
path. An embedded sub-asset has no file of its own, so `write_asset_sidecar` skips it; its name
stays in the container metadata until the sub-asset is extracted.

The scan also records each row's `content_hash`, an FNV-1a fold of the asset's baked bytes. An
embedded sub-asset's hash is baked into the container metadata, so the scan recovers it without
reading chunk data; a standalone file is hashed during the cold scan, which runs exactly when
some file changed.

## Resolving an id to a GPU resource

`load_mesh_asset` and `load_texture_asset` resolve on demand: check the cache, then route by the
catalog row. An engine id in the reserved range (below 1024) has no catalog row: a
[built-in primitive](../built-in-primitives/)'s id generates and uploads its geometry on first
use. An embedded sub-asset (`container != 0`) routes through `resolve_mesh` / `resolve_texture`,
which read the owning container's chunk; a standalone file reads its own path. A texture's upload
colorspace comes from an explicit `.smeta` value, else from its `hdr`/`linear` provenance flags.

The cache means many entities referencing one mesh trigger one load and one
[upload](../gpu-mesh-upload/); the rest are map hits. `render_scene` calls the resolvers once per
entity per frame, so the hit path stays a hash lookup.

## Negative caching

```rust
pub type AssetCache<T> = HashMap<u64, Option<Arc<T>>>;
```

Two distinct facts live in that shape. A **present** key holding `None` is a negative-cache
marker: a load that failed and is not retried. An **absent** key was never attempted, and only an
absent key triggers a load. `resolve_cached` packages the get-or-load-or-mark contract as a
helper; the mesh, texture, and model loaders apply the same discipline inline — return a present
entry verbatim, insert the outcome of a miss, including a `None`.

Because `render_scene` runs every frame, this keeps a missing or corrupt asset from re-reading
the disk many times a second. A texture id with no catalog row warns once, negative-caches, and
the draw path substitutes the default-white slot.

## The thumbnail cache

Thumbnails are content-addressed and app-level: a tile lives at
`<appDataRoot>/thumbnail-cache/v<version>-<contentHash>-<size>.png`, keyed on the asset's baked
content rather than its id or a file stat. Identical content resolves to one file shared across
projects; a touch that bumps a file's mtime without changing bytes still hits; only a real
content change mints a new key.

Bumping `THUMBNAIL_CACHE_VERSION` changes the filename prefix, so a change to how tiles render
orphans every old entry in one stroke; the orphans age out through the size-cap eviction.
Materials are the exception to content addressing: their key hashes the *resolved* material
state, so editing a parent reflows every instance's tile without touching the child `.smat`.

`request_thumbnail` checks the cache before loading anything: a mesh, texture, or model reads its
`content_hash` straight from the in-memory catalog, so a hit never opens the container it would
otherwise decode. A miss enqueues a `PreviewRenderJob` and replies `pending`; the host drains the
queue in `on_update`, renders the tile through the main forward+ graph on an offscreen view,
writes the PNG, and the editor's next poll hits it. A row whose stored hash is `0` derives one
from the gathered bytes, backfills the catalog, and persists the catalog cache, so the next
request takes the cheap path.

The shared cache is bounded to 1 GiB: a write that pushes past the cap deletes the oldest files
(by mtime) until the directory is back under 80 % of it.

## In the code

| What | File | Symbols |
|---|---|---|
| The server | `assets/src/lib.rs` | `AssetServer`, `AssetServer::new`, `clear_asset_caches` |
| Catalog types | `scene/src/environment.rs` | `AssetCatalog`, `AssetEntry`, `AssetType`, `Colorspace`, `TextureRole` |
| Catalog ops | `scene/src/environment.rs` | `AssetCatalog::put`, `find`, `rename`, `unique_name` |
| Cache shape | `assets/src/cache.rs` | `AssetCache`, `resolve_cached` |
| Resolve + cache | `assets/src/load.rs` | `load_mesh_asset`, `load_texture_asset`, `resolve_mesh`, `resolve_texture` |
| Scan, sidecar, catalog cache | `assets/src/scan.rs` | `reconcile_catalog_from_disk`, `load_catalog`, `apply_sidecar_overrides`, `write_asset_sidecar`, `asset_signature` |
| Role inference | `assets/src/scan.rs` | `infer_texture_role`, `detect_material_role`, `colorspace_for_role_explicit` |
| Thumbnail cache | `assets/src/thumbnail.rs` | `request_thumbnail`, `THUMBNAIL_CACHE_VERSION`, `write_thumbnail_cache` |

## Related

- [The .smodel container](../smodel-container/) — the scanned, self-describing model file
- [Import pipeline](../import-pipeline/) — how entries get into the catalog
- [Project files](../project-serialization/) — how the catalog persists
- [Built-in primitives](../built-in-primitives/) — the reserved ids that bypass the catalog
- [Draw list](../draw-list/) — the per-frame consumer of resolved meshes
- [Asset catalog in the scene](../../scene-and-ecs/asset-catalog-in-scene/) — why the type lives in `saffron-scene`
- [Asset commands](../../tooling-and-control/asset-commands/) — driving the catalog from the CLI
- [Assets panel and thumbnails](../../ui-and-editor/assets-panel-and-thumbnails/) — the editor surface over the catalog
