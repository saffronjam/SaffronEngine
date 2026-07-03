+++
title = 'Asset catalog'
weight = 6
+++

# Asset catalog

An asset catalog is a named registry that maps stable UUIDs to files on disk, decoupling
the assets a project references from where those files live. Components reference an asset
by `Uuid`; the catalog resolves that id to a name, a type, and a relative path.

The catalog gives every asset an identity that survives renaming and moving its file. A
scene stores a `Uuid` rather than a path, so editing the asset's location or display name
never breaks the reference. Around the catalog sits the asset server, which turns a `Uuid`
into a live GPU resource.

## The server

`AssetServer` is the project's single owner of assets: the catalog plus three UUID-keyed
GPU caches, so entities sharing an asset upload it once.

```rust
pub struct AssetServer {
    pub root: PathBuf,
    pub catalog: AssetCatalog,            // id -> {name, type, path, ...}
    pub mesh_by_uuid: AssetCache<GpuMesh>,        // GPU cache
    pub texture_by_uuid: AssetCache<GpuTexture>,  // GPU cache
    pub model_by_uuid: AssetCache<ModelAsset>,    // opened .smodel containers
    // editor-camera gizmo visual + the optional thumbnail worker
}
```

The catalog is the source of truth; the three maps are caches over it. The catalog records
that asset 42 is a model at `models/42.smodel`; a cache records that 42 is already uploaded
and holds its `Arc`. `AssetServer::new` creates the asset root with its `models/`,
`textures/`, and `materials/` subdirectories. The catalog is normally populated by
[loading a project](../project-serialization/) or a scan.

## The catalog

The catalog is a named registry, not a filesystem view. Each entry pairs a human-facing name
the user can rename with a separate on-disk path:

```rust
pub enum AssetType { Mesh, Texture, Other, Animation, Material, Model }

pub struct AssetEntry {
    pub id: Uuid,
    pub name: String,        // UTF-8, renameable, user-facing
    pub asset_type: AssetType,
    pub path: String,        // relative to the asset root
    pub container: Uuid,     // 0 = standalone; else the owning .smodel
    pub chunk: i32,          // TOC chunk index inside the container (-1 = standalone)
    pub colorspace: Colorspace,
    pub content_hash: u64,   // FNV of the baked content; the thumbnail cache key
    // + folder, hdr/linear flags, animation duration/tracks, rigged
}

pub struct AssetCatalog {
    pub entries: Vec<AssetEntry>,
    pub by_id: HashMap<u64, usize>,  // id -> index into entries
}
```

`AssetCatalog::put` inserts or replaces by id and keeps `by_id` in sync; `find` resolves an
id to an `Option<&AssetEntry>`; `rename` renames in place; `unique_name` appends ` (2)`,
` (3)`, … so two imports of `cube.gltf` get distinct names. `AssetCatalog` lives in
`saffron-scene`, not `saffron-assets`, so the inspector can read it without depending on the
renderer; the asset layer hands the scene a shared read-only handle (`Option<Arc<AssetCatalog>>`).

`AssetEntry` carries container linkage — `container` (the owning [`.smodel`](../smodel-container/))
and `chunk` — so one row can be the model and another a mesh/material/texture embedded inside
it, resolved by `(container, sub-id)`. `colorspace` records how a texture's bytes are
interpreted, recovered from a chunk flag or a `.smeta`.

## The filesystem is the source of truth

The catalog is **derived from a scan**, not authored in `project.json`. `scan_assets` walks
`assets/`, prefix-reads every `.smodel` into a `Model` row plus a row per sub-asset, and
identifies engine-written standalone files by their uuid filename. A foreign file with no
identity in its bytes (a raw `.png` dropped in) gets a `.smeta` sidecar holding its id and
colorspace, minted on first sight. `load_project` reconciles the loaded catalog against the
scan, so an import you never saved is rediscovered rather than orphaned. `load_catalog` is the
fast path: `assets/.cache/catalog.json` memoizes the scan keyed by a signature of the tree.
It is a latency shortcut only — delete it and a cold scan rebuilds an identical catalog.

The scan also records each row's `content_hash` — an FNV fold of the asset's baked bytes.
An embedded sub-asset's hash is baked into the `.smodel` META, so the scan recovers it for
free; a standalone file is hashed on the cold scan (which runs exactly when a file changed).

## The thumbnail cache

Thumbnails are **content-addressed** and **app-level**: a PNG lives at
`<appDataRoot>/thumbnail-cache/<contentHash>-<size>.png`, keyed on the asset's baked content
rather than its uuid or a file stat. The consequences fall out of the key. A cache entry
survives a project switch and is shared across projects (identical content resolves to one
file); a bare touch that bumps a file's mtime without changing bytes still hits; and only a
real content change mints a new key and regenerates. Materials are the exception: their key is
a hash of the *resolved* material state, so editing a parent reflows every instance's
thumbnail without touching the child `.smat`.

`request_thumbnail` checks the cache **before** loading anything: a mesh/texture/model reads
its `content_hash` straight from the catalog (in memory) and looks up the file, so a hit never
opens the 50 MB container it would otherwise decode on the main thread. Only a miss builds the
full job. A legacy row with no stored hash self-heals — it derives the hash from the gathered
bytes, backfills the catalog, and persists it, so the next boot takes the cheap path. The
shared cache is bounded: a write that pushes it over its size cap evicts the oldest files.

## Resolving an id to a GPU resource

`load_mesh_asset` and `load_texture_asset` are the resolve-on-demand front doors. Both share
one code path through `resolve_cached`: check the cache; on an absent key, look the id up in
the catalog, load and upload the file, and cache the outcome.

```rust
// resolve_cached(cache, id.value(), || { ... look up + load + upload ... })
//  - a present key returns its cached Option<Arc<T>> verbatim (a live Arc or a cached None)
//  - an absent key runs the loader once and caches its result
```

The cache means many entities referencing the same mesh trigger one load and one
[upload](../gpu-mesh-upload/); the rest are map hits. `render_scene` calls these resolvers
once per entity per frame, so the caching keeps that cheap. `resolve_mesh` / `resolve_texture`
are the cache-only lookups the draw loop uses when the upload has already happened.

## Negative caching

A GPU cache is an `AssetCache<T> = HashMap<u64, Option<Arc<T>>>`, and the inner `Option` is
load-bearing: a **present** key holding `None` is a *negative-cache marker* — a load that
failed and is not retried — distinct from an **absent** key that was never attempted.
`resolve_cached` is the single place that honors this:

```rust
if let Some(cached) = cache.get(&key) {
    return cached.clone();   // a present None is returned verbatim, not retried
}
```

On the next frame the lookup hits that cached `None` and returns it immediately, without
re-reading the broken file or re-warning. Because `render_scene` runs every frame, this keeps
a missing or corrupt asset from flooding the log and re-hitting the disk many times a second.

## In the code

| What | File | Symbols |
|---|---|---|
| The server | `assets/src/lib.rs` | `AssetServer`, `AssetServer::new` |
| Catalog type | `scene/src/environment.rs` | `AssetCatalog`, `AssetEntry`, `AssetType`, `Colorspace` |
| Catalog ops | `scene/src/environment.rs` | `AssetCatalog::put`, `find`, `rename`, `unique_name` |
| The cache shape | `assets/src/cache.rs` | `AssetCache`, `resolve_cached` |
| Resolve + cache | `assets/src/load.rs` | `load_mesh_asset`, `load_texture_asset`, `resolve_mesh`, `resolve_texture` |
| Scan + sidecar + cache | `assets/src/scan.rs` | `scan_assets`, `load_catalog`, `read_smeta` |
| Thumbnail cache | `assets/src/thumbnail.rs` | `request_thumbnail`, `thumbnail_content_cache_path`, `evict_thumbnail_cache` |

## Related

- [The .smodel container](../smodel-container/) — the scanned, self-describing model file
- [Import pipeline](../import-pipeline/) — how entries get into the catalog
- [Project files](../project-serialization/) — how the catalog persists
- [Draw list](../draw-list/) — calls the resolvers per entity per frame
- [Asset catalog in the scene](../../scene-and-ecs/asset-catalog-in-scene/) — why it lives in `saffron-scene`
- [Asset commands](../../tooling-and-control/asset-commands/) — driving the catalog from the CLI
