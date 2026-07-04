+++
title = 'The .smodel container'
weight = 11
+++

# The .smodel container

A `.smodel` is one self-contained file per imported model. It bakes the meshes, materials,
textures, animations, and the node hierarchy of a glTF or OBJ import into a single binary
container, so an import writes exactly one file instead of scattering dozens of loose ones. The
file carries its own identity and a recipe for rebuilding itself, which lets the filesystem be
the source of truth for the asset catalog.

Importing Sponza touches around fifty textures. Scattered across loose
`textures/<uuid>.<ext>` files plus loose `.smat` material files, a forgotten save
turns them into dead orphans the catalog never knew about. One container closes that gap: it is
the record, so a scan rediscovers it whether or not the project was saved.

## Layout

A fixed 64-byte `SMDL` header, a 32-byte-stride chunk table, the front-loaded metadata chunk,
then the payload chunks (16-byte aligned). It mirrors the `.smesh` discipline — fixed magic, a
version gate, 64-bit offsets, a total-length check on read.

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct SModelHeader {            // 64 bytes, little-endian
    pub magic: [u8; 4],              // b"SMDL"
    pub container_version: u32,      // CONTAINER_FORMAT_VERSION
    pub schema_version: u32,         // metadata-chunk schema version
    pub flags: u32,
    pub toc_count: u32,
    pub reserved0: u32,
    pub toc_offset: u64,
    pub meta_offset: u64,            // the META chunk, placed first so a prefix read reaches it
    pub meta_length: u64,
    pub total_length: u64,           // validated against the file size
    pub reserved: [u32; 2],
}

#[repr(u32)]
pub enum ChunkKind { Meta, Mesh, Texture, Material, Animation, Thumbnail }  // fourcc tags
```

Each payload chunk is wrapped, never reinvented: a `MESH` chunk is a `.smesh` image, a `SANM`
chunk is a `.sanim` image, an `SMAT` chunk is the material's `.smat` JSON, and an `STEX` chunk
is the texture's encoded bytes with its colorspace in the chunk flags. Because those images use
self-relative offsets, the same `load_mesh_from_bytes` reads a chunk slice exactly as it reads
a whole file. The header/TOC are reinterpreted with safe `bytemuck` over `#[repr(C)]` Pod
structs, so the crate's `#![deny(unsafe_code)]` holds. `read_container` validates the
chunk-table-in-bounds and no-overlap invariants (sort payload ranges by offset, reject any that
start before the previous ends) plus `total_length` against the file size.

## The metadata chunk

The `META` chunk is JSON, placed first after the table. A *prefix read* — the 64-byte header
plus that one chunk — is enough to build a catalog entry without touching a single payload
byte. `ContainerMetadata` carries the model id and name, the `Import` recipe, the flat list of
`SubAsset`s, a material factor summary, the node hierarchy, an optional skin, and the
extract/remap table.

```jsonc
{
  "model":  { "id": "<decimal-uuid>", "name": "town", "sourceFormat": "gltf" },
  "import": { "sourcePath": "raw/town.glb", "sourceHash": "<content hash>", "options": { … } },
  "subAssets": [ { "subId": "<uuid>", "type": "mesh", "name": "town_mesh", "chunk": 1 }, … ],
  "nodes": [ … ], "skin": { … }, "remap": { }
}
```

Every reference is by a stable 64-bit `Uuid`, never a path or an array index. A sub-asset's id
is derived from a source key (`sub_id_for(model_key, kind, source_name, dup_index)`), so a
reimport that reorders meshes still resolves the same sub-id. The `nodes`/`skin` block is
exactly the rig payload the instantiate path expands.

## Identity, scanning, and the cache

The filesystem is the source of truth. `scan_assets` walks `assets/`, prefix-reads every
`.smodel` into one `Model` row plus a row per sub-asset (via `read_container_metadata`), and
identifies engine-written standalone files by their uuid filename. Each standalone file's editable
metadata (name / folder / colorspace) lives in a co-located [`.smeta` sidecar](../asset-server-and-catalog/#the-smeta-sidecar),
written eagerly on import and rename so it survives a cold scan without a save; a foreign file
dropped in (a raw `.png`) additionally takes its stable id from that sidecar. `load_project`
reconciles the loaded catalog against the scan, so a never-saved import can never become an orphan. A
regenerable `assets/.cache/catalog.json` is a latency shortcut keyed by a signature of the tree
— delete it and a cold scan (`load_catalog`) yields the identical catalog.

## Instantiate, extract, reimport

Import produces an asset and does **not** spawn. `instantiate_model` expands the stored node
hierarchy into `hecs` entities on demand, so one asset becomes many instances (or none). The
entities hold soft `(model_id, sub_id)` references resolved at draw time, so changes to the
container flow through without re-instantiation.

`extract_sub_asset` slices a chunk to a standalone file keeping its sub-id and writes a `remap`
entry, so resolution prefers the external file — the workflow for editing or sharing one
embedded material. `reimport_model` skips when the source bytes are unchanged
(content-addressed: `hash_file_fnv` versus the stored `import.source_hash`), otherwise re-bakes
with the *stored* options, diffs by sub-id, and never clobbers a remapped (extracted)
sub-asset.

## In the code

| What | File | Symbols |
|---|---|---|
| Header + table + framing | `geometry/src/smodel.rs` | `SModelHeader`, `TocEntry`, `ChunkKind`, `write_container`, `read_container` |
| Metadata + prefix read | `assets/src/model.rs` | `ContainerMetadata`, `SubAsset`, `read_container_metadata`, `encode_container_metadata` |
| Bake | `assets/src/import.rs` | `bake_model`, `import_model` |
| Chunk-slice load | `assets/src/load.rs`; `geometry/src/smesh.rs` | `resolve_mesh`, `load_mesh_from_bytes` |
| Instantiate | `assets/src/spawn.rs` | `instantiate_model`, `ModelInstance` |
| Scan + cache | `assets/src/scan.rs` | `scan_assets`, `load_catalog`, `read_smeta` |
| Extract + reimport | `assets/src/manage.rs` | `extract_sub_asset`, `reimport_model` |

> [!NOTE]
> `.smodel` is its own magic and version; `.smesh` and `.sanim` are reused verbatim as chunk
> payloads. Re-importing the *same* source as a second model collides on the source-derived
> sub-ids — the path for updating a model from its source is `reimport-model`, not a second
> import.

## Related

- [.smesh format](../smesh-format/) — the mesh image embedded as a `MESH` chunk
- [.sanim format](../sanim-format/) — the clip image embedded as a `SANM` chunk
- [Asset server & catalog](../asset-server-and-catalog/) — the scan-derived, UUID-keyed catalog
- [Import pipeline](../import-pipeline/) — translate → bake, and why import no longer spawns
- [Clean unused assets](../../../how-to/clean-unused-assets/) — the deliberate cleanup workflow
- [Asset editor](../../ui-and-editor/asset-editor/) — reads the node hierarchy + skin + clips back out of this container
