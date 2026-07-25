+++
title = 'Scene mutation journal'
weight = 2
+++

# Scene mutation journal

The scene mutation journal is an ordered change stream for derived copies of ECS state. A renderer
mirror or inspection cache can apply the small set of changes since its last cursor instead of
walking every entity, while the `Scene` remains the authority.

## Revisions and cursors

Every tracked change receives a `SceneRevision`. The revision increases across entity creation and
destruction as well as component add, update, and removal. Each record includes the live `Entity`
handle and its durable `Uuid`, so a destruction record remains identifiable after the handle becomes
stale.

A consumer takes a snapshot and retains the cursor returned at the same scene revision. Subsequent
reads return all newer records in revision order:

```rust
let mut cursor = scene.journal_cursor();
build_render_snapshot(&scene);

match scene.read_journal(cursor) {
    SceneJournalRead::Delta { mutations, next } => {
        apply_render_changes(&scene, &mutations);
        cursor = next;
    }
    SceneJournalRead::SnapshotRequired { next } => {
        rebuild_render_snapshot(&scene);
        cursor = next;
    }
}
```

The journal has bounded storage. A cursor older than the retained window receives
`SnapshotRequired`, which makes loss explicit and gives the replacement snapshot an exact cursor.
Consumers never infer correctness from a partial tail.

## Tracked component access

All ECS storage is private to `Scene`. `add_component`, `remove_component`, and
`with_component_mut` append their mutation after the operation succeeds. `for_each` inspects the
query's unique borrows and records an update for each mutable component type on each matched entity.
Shared query members do not create records.

`IdComponent` is the exception to mutable access: an entity's UUID is immutable. Snapshot loading
uses `spawn_with_id`, and changing identity requires destroying one entity and creating another, so
journal consumers never lose the stable key behind a live handle.

This makes physics, scripts, hierarchy work, document loading, and editor operations use the same
tracking seam. The journal is conservative: a mutable callback records an update even when it writes
the same value. Derived consumers can use component and transform revisions to avoid unnecessary
uploads.

## Transform propagation

Each entity tracks content, local-transform, hierarchy, current-world, and previous-world revisions.
`update_world_transforms` compares the local and hierarchy revisions plus its parent's published
world revision. A clean subtree reuses its cached matrix. A parent change reaches each descendant
once through the roots-first hierarchy walk.

When composition produces a different matrix, the scene publishes a `WorldTransform` update and
moves the prior matrix and revision into `SceneWorldTransformState::previous`. Motion-vector and
deformation consumers can therefore upload exact current/previous pairs without a second transform
history store.

## In the code

| What | File | Symbols |
|---|---|---|
| Revisions, cursors, and records | `scene/src/journal.rs` | `SceneRevision`, `SceneJournalCursor`, `SceneMutation` |
| Journal reads and tracked component access | `scene/src/scene.rs` | `read_journal`, `with_component_mut`, `for_each` |
| Incremental world propagation | `scene/src/hierarchy.rs` | `update_world_transforms`, `write_subtree` |
| Current and previous transform state | `scene/src/journal.rs` | `SceneEntityRevisions`, `SceneWorldTransformState` |

## Related

- [ECS architecture](../ecs-architecture/) — the wrapped storage and component access surface.
- [Scene hierarchy](../scene-hierarchy/) — parent links and roots-first transform composition.
- [Scene serialization](../scene-serialization/) — durable UUID ownership and document loading.
- [Persistent GPU scene](../../frame-and-render-graph/persistent-gpu-scene/) — the render mirror consuming this journal.
