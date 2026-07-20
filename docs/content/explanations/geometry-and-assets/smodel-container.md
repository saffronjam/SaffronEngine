+++
title = '.smodel container'
weight = 11
+++

# .smodel container

A `.smodel` is one self-contained file per imported model. It bakes the meshes, materials,
textures, and animation clips of a [glTF](https://github.com/KhronosGroup/glTF) or OBJ import
into a single binary container, together with the node hierarchy that arranges them. The file
carries its own identity and the recipe for rebuilding itself from its source.

One file per model is what lets the filesystem be the source of truth for the
[asset catalog](../asset-server-and-catalog/). A large import brings dozens of textures;
scattered across loose files, a forgotten project save turns them into orphans the catalog never
knew about. A container is its own record, so a scan rediscovers it whether or not the project
was saved.

## Layout

The file opens with a fixed 64-byte `SMDL` header, followed by a chunk table of 32-byte
`TocEntry` records at offset 64, then the payload chunks, each on a 16-byte boundary. The
framing follows the same discipline as [.smesh](../smesh-format/): a fixed magic, a version
gate, 64-bit offsets, and a total-length check on read.

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct SModelHeader {            // 64 bytes, little-endian
    pub magic: [u8; 4],              // b"SMDL"
    pub container_version: u32,      // the byte-framing version
    pub schema_version: u32,         // the metadata-chunk schema version
    pub flags: u32,
    pub toc_count: u32,
    pub reserved0: u32,
    pub toc_offset: u64,
    pub meta_offset: u64,            // the META chunk, placed first
    pub meta_length: u64,
    pub total_length: u64,           // validated against the file size
    pub reserved: [u32; 2],
}

#[repr(u32)]
pub enum ChunkKind { Meta, Mesh, Texture, Material, Animation, Thumbnail }
// on-disk fourcc tags:  META  MESH  STEX      SMAT      SANM       THMB
```

Each payload chunk wraps an existing format rather than inventing one: a `MESH` chunk is a
`.smesh` byte image, a `SANM` chunk a `.sanim` image, an `SMAT` chunk the material's `.smat`
JSON, and an `STEX` chunk the texture's encoded bytes with its colorspace in the chunk flags.
Those images use self-relative offsets, so `load_mesh_from_bytes` reads a chunk slice exactly as
it reads a whole file. The same framing carries the `.smatx` material container: one `.smat`
chunk plus a texture chunk per map.

The header and table are reinterpreted with safe [bytemuck](https://docs.rs/bytemuck) casts over
`#[repr(C)]` Pod structs, so the crate's `#![deny(unsafe_code)]` holds. `read_container`
validates that the chunk table is in bounds, that every payload sits inside the file, and that
no two payloads overlap: it sorts the payload ranges by offset and rejects any range starting
before the previous one ends.

Concretely, a container with an 8-byte META, a 40-byte mesh, and a 33-byte texture frames as:
the table ends at 160 (64 + 3 × 32), META sits at 160, the mesh at 176 (the next 16-byte
boundary), the texture at 224, and `total_length` is 257 — the unaligned end of the last
payload.

## The metadata chunk

The `META` chunk is JSON, and `write_container` places it first after the table, recording its
span in `meta_offset`/`meta_length`. A prefix read is therefore enough for a catalog entry:
`read_container_metadata` decodes the header plus that one chunk and parses no payload.
`ContainerMetadata` carries the model's id, name, and source format, the `Import` recipe, the
flat `SubAsset` list, a per-material factor summary, the node hierarchy, an optional skin and
morph block, and the extract/remap table.

```jsonc
{
  "import":    { "importerVersion": 1, "options": { "axis": "y-up", "scale": 1.0 },
                 "sourceHash": "9455546962", "sourcePath": "raw/town.glb" },
  "materials": [ { "baseColor": [1, 1, 1, 1], "metallic": 0.0, "roughness": 1.0, "subId": "13" } ],
  "model":     { "id": "4242", "name": "town", "sourceFormat": "gltf" },
  "nodes":     [ { "mesh": "11", "name": "root", "parent": -1,
                   "r": [1, 0, 0, 0], "s": [1, 1, 1], "t": [0, 0, 0] } ],
  "remap":     {},
  "schema":    2,
  "subAssets": [ { "chunk": 1, "contentHash": 4211066488, "name": "town_mesh",
                   "subId": "11", "type": "mesh" } ]
}
```

The encoding is deterministic: `encode_container_metadata` serializes every object's keys in
sorted order, compact, so the same metadata always produces the same bytes. `skin` and `morph`
appear only when the model has them. Each sub-asset record adds type extras (a texture's
colorspace, a clip's duration and track count) plus an
[FNV-1a](https://en.wikipedia.org/wiki/Fowler%E2%80%93Noll%E2%80%93Vo_hash_function) content
hash of its baked chunk bytes, the key for the content-addressed thumbnail cache.

Sub-assets are addressed by stable 64-bit ids, serialized as decimal strings. `sub_id_for`
derives each id with FNV-1a over the source file stem, the sub-asset kind, the source name, and
a duplicate-disambiguation index, folded above the id range below 1024 that the engine reserves.
The same tuple always hashes to the same id, so re-baking an unchanged source resolves every
sub-asset to its prior identity.

## Scanning and the catalog cache

The filesystem is the source of truth. `scan_assets` walks `assets/` in sorted order and
rebuilds the catalog from disk: each `.smodel` (and `.smatx`) contributes one parent row plus a
row per sub-asset through the prefix read, an engine-written standalone file identifies by its
uuid filename stem, and a foreign file (a raw `.png` dropped in) takes its id from a
[`.smeta` sidecar](../asset-server-and-catalog/#the-smeta-sidecar) minted on first sight. A
never-saved import can therefore never become an orphan.

Editable metadata (a name, folder, or colorspace) lives in that same co-located sidecar, written
eagerly on import and rename so it survives a cold scan without a project save. A regenerable
`assets/.cache/catalog.json` is a latency shortcut keyed by a signature of the tree: on a match,
`load_catalog` reuses the cached rows and skips every prefix read. Deleting it is always safe;
the next load is a cold scan that yields the identical catalog.

## Instantiate, extract, reimport

Import produces an asset and spawns nothing. `instantiate_model` expands the stored node
hierarchy into [scene entities](../../scene-and-ecs/ecs-architecture/) on demand, so one asset
becomes any number of instances. The spawned components hold soft `(model_id, sub_id)`
references that `resolve_mesh` / `resolve_texture` follow at draw time, and the root is tagged
`ModelInstance`; a change to the container flows to every instance without re-instantiation.

`extract_sub_asset` slices one chunk out to a standalone file, keeping its sub-id, and writes a
`remap` entry into the META so resolution prefers the external file. That is the path for
editing or sharing one embedded material or texture. The embedded chunk stays behind as the
fallback (with a logged warning) if the external file goes missing, and `clear-extraction`
reverts the whole operation.

`reimport_model` re-bakes the container from the stored source path and options. It compares a
content hash of the source bytes (`hash_file_fnv`, never an mtime) against the stored
`import.sourceHash` and skips when the hash and the importer version are both unchanged. Because
sub-ids are stable, the result diffs by id into updated, added, and removed-from-source sets,
and remap entries for surviving sub-assets are preserved: an extracted edit outlives a reimport.

```sh
sa model-info town         # sub-assets, byte sizes, and the import recipe
sa reimport-model town
# { "updated": 9, "added": 0, "removedFromSource": 0, "skipped": false }
sa reimport-model town     # same source bytes: a content-addressed skip
# { "updated": 0, "added": 0, "removedFromSource": 0, "skipped": true }
```

> [!NOTE]
> `.smodel` has its own magic and version; `.smesh` and `.sanim` bytes embed unchanged as chunk
> payloads. Importing the *same* source file twice mints two model ids whose source-derived
> sub-ids collide in the catalog — the path for updating a model from its source is
> `reimport-model`, not a second import.

## In the code

| What | File | Symbols |
|---|---|---|
| Header + table + framing | `geometry/src/smodel.rs` | `SModelHeader`, `TocEntry`, `ChunkKind`, `write_container`, `read_container`, `ContainerReader` |
| Metadata + prefix read | `assets/src/model.rs` | `ContainerMetadata`, `SubAsset`, `encode_container_metadata`, `read_container_metadata`, `ByteSource`, `chunk_source_for` |
| Stable sub-ids | `geometry/src/sub_id.rs` | `sub_id_for` |
| Bake + import | `assets/src/import.rs` | `bake_model`, `import_model`, `catalog_rows_for_container`, `hash_file_fnv` |
| Chunk-slice load | `assets/src/load.rs`; `geometry/src/smesh.rs` | `resolve_mesh`, `resolve_texture`, `load_mesh_from_bytes` |
| Instantiate | `assets/src/spawn.rs` | `instantiate_model`, `spawn_model`, `ModelInstance` |
| Scan + cache | `assets/src/scan.rs` | `scan_assets`, `load_catalog`, `reconcile_catalog_from_disk` |
| Extract + reimport | `assets/src/manage.rs` | `extract_sub_asset`, `clear_extraction`, `reimport_model` |

## Related

- [.smesh format](../smesh-format/) — the mesh image embedded as a `MESH` chunk
- [.sanim format](../sanim-format/) — the clip image embedded as a `SANM` chunk
- [Asset server and catalog](../asset-server-and-catalog/) — the scan-derived, UUID-keyed catalog
- [Import pipeline](../import-pipeline/) — the translate → bake path that writes the container
- [Model import](../gltf-and-obj-import/) — how glTF/OBJ sources translate into the baked forms
- [Clean unused assets](../../../how-to/clean-unused-assets/) — the deliberate cleanup workflow
- [Asset editor](../../ui-and-editor/asset-editor/) — reads the node hierarchy, skin, and clips back out of this container
