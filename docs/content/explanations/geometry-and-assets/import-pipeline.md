+++
title = 'Import pipeline'
weight = 7
+++

# Import pipeline

The import pipeline is the write side of the asset system: it turns an external file into a
project asset. Importing a model bakes the source into one [`.smodel` container](../smodel-container/)
under the project's asset directory and adds the catalog rows it contributes. It does not touch
the GPU and does not spawn anything; placing the asset in a scene is a separate step.

Resolution — turning a cached id back into an `Arc` — is the read side. This page covers
import alone.

## Importing a model

`AssetServer::import_model` is the full chain from a source file to a stored asset:

```rust
let graph = translate_model(path)?;                       // parse glTF/OBJ → ImportedModel
let bake = self.bake_model(&graph, options, path, Uuid(0))?;  // write one .smodel (0 mints a fresh id)
// bake.rows are added to self.catalog (the container + its sub-assets)
```

The steps run in order:

1. Parse the source through the [importer](../gltf-and-obj-import/) into an `ImportedModel`.
2. `bake_model` writes one `assets/models/<uuid>.smodel`: the mesh as a [`.smesh`](../smesh-format/)
   `MESH` chunk, each material as an `SMAT` chunk, each texture as an `STEX` chunk (colorspace in
   the chunk flags), each animation clip as a [`.sanim`](../sanim-format/) `SANM` chunk, and a
   front-loaded `META` chunk holding the node hierarchy, skin, and the deterministic reimport
   recipe.
3. `catalog_rows_for_model` produces the catalog rows the container contributes: one `Model`
   row plus one row per embedded sub-asset, each linked back to the container by id and chunk
   index.

The on-disk asset is the single `.smodel`. The source glTF/OBJ is read once and never
referenced again, and nothing loose lands beside the container.

## Placing a model

Import populates the catalog; it never spawns. `AssetServer::instantiate_model` (the
`instantiate-model` command) reconstructs a `ModelSpawnInput` from the container's `META` and
expands the stored hierarchy into entities: `spawn_model` builds the mesh entity, dispatching
to `spawn_skinned_model` for a rig (with its bone entities, joints, and a stopped
`AnimationPlayer`), and `apply_imported_materials` attaches a `MaterialSet` whose slots reference the
baked `.smat` chunks — one slot per material. The root carries a `ModelInstance` component naming its
source asset. One `.smodel` instantiates into many independent entity trees, so the
`add-entity cube` preset is just an instantiate of the built-in cube model.

## Importing a texture

A standalone texture import is its own path: `import_texture` reads a file into bytes and calls
`register_texture_bytes`, which decodes to confirm a valid image, uploads via the
[`GpuUploader`](../gpu-mesh-upload/) seam, then writes the original encoded bytes to a loose
`textures/<uuid>.<ext>` and adds a `Texture` catalog row:

```rust
let decoded = decode_image_from_memory(&encoded)?;
let texture = gpu.upload_texture(&decoded.rgba, decoded.width, decoded.height, /* srgb */ true)?;
// write `encoded` (not the decoded pixels) under textures/<uuid>.<ext>, then catalog.put(...)
```

The disk copy is the encoded PNG/JPG, so reloading it re-runs [the decode](../image-decoding/)
rather than storing bulky raw RGBA. The `srgb = true` argument matches albedo being authored in
sRGB. A model's textures, by contrast, ride inside the `.smodel` as `STEX` chunks rather than
loose files. (`register_hdr_texture_bytes` is the parallel float path for `.hdr` panoramas.)

## Deduplication

A fresh `import_model` mints a new model id, so importing `cube.gltf` twice writes two `.smodel`
containers and two catalog entries (`cube`, `cube (2)`). Within a container the sub-asset ids
are stable (`sub_id_for`, keyed by source name), and `reimport_model` reuses the model id and
skips a byte-identical source by its content hash (`hash_file_fnv` versus the stored
`import.source_hash`, plus the importer version). Otherwise there is no cross-import content
dedup; GPU-side sharing happens at resolve time, where entities referencing the same sub-id
share one upload through the cache.

## In the code

| What | File | Symbols |
|---|---|---|
| Parse → import graph | `geometry/src/translate.rs` | `translate_model` |
| Bake + import a model | `assets/src/import.rs` | `bake_model`, `import_model` |
| Catalog rows a container contributes | `assets/src/import.rs` | `catalog_rows_for_model` |
| Reimport (content-hash skip) | `assets/src/manage.rs` | `reimport_model`, `hash_file_fnv` |
| Texture import | `assets/src/scan.rs` | `import_texture`, `register_texture_bytes`, `register_hdr_texture_bytes` |
| Place a model in the scene | `assets/src/spawn.rs` | `instantiate_model`, `spawn_model`, `spawn_skinned_model` |
| Stable sub-ids | `geometry/src/sub_id.rs` | `sub_id_for` |

## Related

- [Model import](../gltf-and-obj-import/) — the parse step
- [The .smodel container](../smodel-container/) — the one-file asset the bake produces
- [.smesh format](../smesh-format/) — the mesh chunk format
- [Image decoding](../image-decoding/) — the texture decode
- [Asset catalog](../asset-server-and-catalog/) — the read side
- [Project files](../project-serialization/) — persisting the catalog the import filled
