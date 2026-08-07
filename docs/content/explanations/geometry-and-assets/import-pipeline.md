+++
title = 'Import pipeline'
weight = 7
+++

# Import pipeline

The import pipeline is the write side of the asset system: it turns an external file into a
project asset. Importing a model bakes the source into one [`.smodel` container](../smodel-container/)
under the project's asset directory and adds the catalog rows it contributes. The bake touches
no GPU and spawns nothing; placing the asset in a scene is a separate step.

Resolution, turning a cataloged id back into a live `Arc`, is the read side and belongs to the
[asset server](../asset-server-and-catalog/). This page covers import alone.

## Importing a model

`AssetServer::import_model` (the `import-model` command) is the full chain from a source file
to a stored asset:

```rust
let graph = translate_model(path)?;                          // parse glTF/OBJ → ImportedModel
let bake = self.bake_model(&graph, options, path, Uuid(0))?; // write one .smodel (0 mints a fresh id)
for row in &bake.rows {
    self.register_imported_asset(row.clone());               // catalog + mutation journal
}
```

The steps run in order:

1. `translate_model` parses [glTF/OBJ](../gltf-and-obj-import/) into an `ImportedModel`: a node
   forest, materials, clips, and optional skin and morph payloads.
2. `bake_model` writes `assets/models/<uuid>.smodel`: one [`.smesh`](../smesh-format/) `MESH`
   chunk per mesh-bearing node, an `SMAT` chunk per material, an `STEX` chunk per texture, a
   [`.sanim`](../sanim-format/) `SANM` chunk per clip, and a front-loaded `META` chunk.
3. `catalog_rows_for_container` adds the container's rows: one `Model` row plus one per
   embedded sub-asset, linked back by container id and chunk index.

`META` holds the node hierarchy, the skin, the morph target names, and the reimport recipe: the
source path, an [FNV-1a](https://datatracker.ietf.org/doc/html/rfc9923) content hash of its
bytes, the importer version, and the `ImportOptions` verbatim, so a reimport replays the
recorded options rather than the current defaults. Texture chunks carry their colorspace in the
chunk flags; `ImportOptions::colorspace_for` marks albedo and emissive maps sRGB and every data
map linear.

The on-disk asset is the single `.smodel`; nothing loose lands beside it. The disk scan builds
rows through the same `catalog_rows_for_container`, so a freshly baked container and a
rediscovered one yield identical rows.

## Placing a model

Import populates the catalog; it never spawns. `AssetServer::instantiate_model` (the
`instantiate-model` command) reconstructs a `ModelSpawnInput` from the container's `META` and
hands it to `spawn_model`, which dispatches on shape:

- **A skinned import** spawns the rigged path (`spawn_skinned_model`): one entity per glTF
  node, `Bone` tags on the joints, a `SkinnedMesh` on the mesh node listing the joints in glTF
  order, and a stopped `AnimationPlayer` for the first clip.
- **A single identity root** collapses to one entity carrying the mesh and material table.
- **Any other forest** spawns one entity per node under a container root
  (`spawn_node_forest`), each mesh-bearing node carrying its node-local mesh.

`apply_imported_materials` attaches a `MaterialSet` whose slots reference the baked `.smat`
chunks by sub-id, with no inline copy, so editing an embedded material propagates to every
instance. The root carries a `ModelInstance` component naming its source asset; one `.smodel`
instantiates into many independent entity trees.

```sh
sa import-model /path/to/robot.gltf   # → { "id": "…", "name": "robot", "type": "model" }
sa instantiate-model robot            # expand the stored hierarchy into entities
```

## Importing a texture

A standalone texture import is its own path. `import_texture` (the `import-texture` command)
reads the file and resolves a colorspace: an explicit override wins, a role hint (albedo,
normal, …) derives one, and with neither an `.hdr` extension routes to the float path while
everything else uploads as sRGB. `register_texture_bytes` then does the work:

```rust
let decoded = decode_image_from_memory(encoded)?;
let texture = gpu.upload_texture(&decoded.rgba, decoded.width, decoded.height, srgb)?;
// write `encoded` (not the decoded pixels) to textures/<uuid>.<ext>, then add the catalog row
```

It decodes through [image decoding](../image-decoding/), uploads via the
[`GpuUploader`](../gpu-mesh-upload/) seam, writes the original encoded bytes to a loose
`textures/<uuid>.<ext>`, adds a `Texture` catalog row with a `.smeta` sidecar, and seeds the
GPU texture cache with the fresh upload. The disk copy is the encoded PNG/JPG, so a reload
re-runs the decode instead of storing bulky raw RGBA. `register_hdr_texture_bytes` is the
parallel float path for `.hdr` panoramas.

A model's textures ride inside its `.smodel` as `STEX` chunks rather than loose files. Material
sets imported from the [Asset Store](../../asset-store-and-connectors/connector-framework/)
follow the same container discipline: `bake_material_container` writes the role-tagged maps
plus their material document into one `assets/materials/<uuid>.smatx`, the material as the
container parent and each texture a hidden sub-row.

## Sub-id stability and reimport

Within a container, sub-asset ids come from `sub_id_for`: an FNV-1a fold over the model key
(the source file stem), the sub-asset kind, the source name, and a duplicate index. The same
tuple always yields the same id, so a re-bake of the same source resolves every sub-asset to
its prior identity. A drifting hash would silently orphan every baked sub-asset; a golden test
pins the exact value.

`reimport_model` (the `reimport-model` command) re-bakes a container from its stored source and
options, reusing the model id. A byte-identical source is a content-addressed skip:
`hash_file_fnv` over the current source bytes must differ from the stored `import.source_hash`
(or the importer version must have bumped) for a re-bake to run. A changed source produces a
`ReimportDelta` diffed by stable sub-id — updated, added, and removed-from-source, the last
kept and reported rather than silently dropped. Live instances resolve by sub-id, so they pick
up the new bytes without re-instantiation.

A fresh `import_model` always mints a new model id, so importing the same file twice writes two
containers. There is no cross-import content dedup; GPU-side sharing happens at resolve time,
where entities referencing the same sub-id share one upload through the cache.

## In the code

| What | File | Symbols |
|---|---|---|
| Parse dispatch | `geometry/src/translate.rs` | `translate_model` |
| Bake + import a model | `assets/src/import.rs` | `bake_model`, `import_model`, `ImportOptions` |
| Rows a container contributes | `assets/src/import.rs` | `catalog_rows_for_container` |
| Catalog publication | `assets/src/lib.rs` | `register_imported_asset`, `register_reimported_asset` |
| Content hashes | `assets/src/import.rs` | `hash_file_fnv`, `hash_bytes_fnv` |
| Reimport (content-hash skip) | `assets/src/manage.rs` | `reimport_model`, `ReimportDelta` |
| Texture import | `assets/src/scan.rs` | `import_texture`, `register_texture_bytes`, `register_hdr_texture_bytes` |
| Material container bake | `assets/src/import.rs` | `bake_material_container` |
| Place a model in the scene | `assets/src/spawn.rs` | `instantiate_model`, `spawn_model`, `apply_imported_materials` |
| Stable sub-ids | `geometry/src/sub_id.rs` | `sub_id_for` |

## Related

- [Model import](../gltf-and-obj-import/) — the parse step
- [The .smodel container](../smodel-container/) — the one-file asset the bake produces
- [.smesh format](../smesh-format/) — the mesh chunk format
- [Image decoding](../image-decoding/) — the texture decode
- [Asset catalog](../asset-server-and-catalog/) — the read side
- [Asset mutation journal](../asset-mutation-journal/) — publication and derived-state invalidation
- [Project files](../project-serialization/) — persisting the catalog the import filled
