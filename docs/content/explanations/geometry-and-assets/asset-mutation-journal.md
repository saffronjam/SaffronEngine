+++
title = 'Asset mutation journal'
weight = 7
+++

# Asset mutation journal

The asset mutation journal is the ordered change stream between the authoritative catalog and
disposable render-derived state. The [persistent GPU scene](../../frame-and-render-graph/persistent-gpu-scene/),
a residency manager, or a preview cache applies changes since its cursor instead of rescanning
every asset. The catalog and authored files remain the truth.

## One mutation surface

`AssetServer` keeps its catalog and loaded caches private. Import, reimport, authored edits, metadata
edits, unload, deletion, disk-scan reconciliation, project replacement, and vegetation asset writes
all use its typed methods. Each successful operation updates the catalog or caches and appends the
matching journal record as one operation.

```rust
let start = assets.asset_journal_cursor();
assets.register_imported_asset(entry);

match assets.read_asset_journal(start) {
    AssetJournalRead::Delta { mutations, next } => {
        apply_asset_changes(&mut gpu_scene, &mut residency, &mutations);
        cursor = next;
    }
    AssetJournalRead::SnapshotRequired { next } => {
        rebuild_from_catalog(assets.catalog());
        cursor = next;
    }
}
```

Code that only reads catalog state uses `catalog()`. There is no mutable catalog accessor, so a
write cannot bypass cache invalidation or publication.

## Revisions and snapshots

Each record receives a monotonic `AssetRevision` and addresses either one stable UUID and asset type
or the complete catalog. `AssetMutationKind` distinguishes import, reimport, edit, unload, delete,
and catalog replacement.

The journal is bounded. A consumer whose cursor is older than retained history receives
`SnapshotRequired`. `asset_catalog_snapshot` captures a cloned catalog and its exact cursor together,
so a derived mirror can rebuild without guessing which revisions the snapshot includes.

## Precise invalidation

`AssetInvalidations` tells render consumers which derived records are stale:

| Bit | Derived state |
|---|---|
| `PROTOTYPE` | mesh, model, plant, animation, or instance prototype data |
| `MATERIAL` | resolved materials and their dependency state |
| `TEXTURE` | decoded images and descriptor bindings |
| `PAGE` | streamed geometry or vegetation pages |

A rename or folder move publishes an edit with no render invalidation. A texture content edit
invalidates both texture and material state. Mesh/model changes invalidate prototype and page data;
plant changes also invalidate materials. A catalog replacement invalidates every class.

Unload and deletion clear loaded representations before they publish. Reimport preserves stable
identity, clears derived state, and publishes one reimport record. Disk reconciliation emits ordered
deletes, imports, and reimports after atomically installing the scanned catalog.

## In the code

| What | File | Symbols |
|---|---|---|
| Journal vocabulary | `assets/src/journal.rs` | `AssetRevision`, `AssetMutation`, `AssetInvalidations` |
| Cursor, snapshot, and canonical writes | `assets/src/lib.rs` | `asset_journal_cursor`, `read_asset_journal`, `asset_catalog_snapshot`, catalog mutation methods |
| Disk reconciliation | `assets/src/scan.rs` | `load_catalog`, `replace_scanned_catalog` |
| Vegetation asset writes | `assets/src/vegetation.rs`, `vegetation_store.rs` | plant, biome, and vegetation-map save/update/delete methods |

## Related

- [Asset server and catalog](../asset-server-and-catalog/) — ownership, loading, and caches
- [Import pipeline](../import-pipeline/) — new assets and stable reimport identity
- [Project files](../project-serialization/) — catalog persistence and replacement
- [Scene mutation journal](../../scene-and-ecs/scene-mutation-journal/) — entity and component deltas
