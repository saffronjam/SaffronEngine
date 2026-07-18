+++
title = 'Serialization'
weight = 5
+++

# Serialization

Scene serialization converts the ECS world into a JSON value without depending on `hecs` storage
introspection. A component registry supplies the per-type operations, while the scene document owns
entity identity, environment state, component display order, and cross-entity relinking.

## Document shape

`scene_to_json` returns a value that can be written as a standalone scene or embedded under the
`scene` key in `project.json`. The scene schema version is `4`:

```json
{
  "version": 4,
  "environment": {},
  "entities": [
    {
      "id": "1024",
      "components": {
        "Name": { "name": "Cube" },
        "Transform": {
          "translation": { "x": 1.0, "y": 2.0, "z": 3.0 },
          "rotation": { "x": 0.0, "y": 0.0, "z": 0.0 },
          "scale": { "x": 1.0, "y": 1.0, "z": 1.0 }
        },
        "Relationship": { "parent": "0" }
      },
      "componentOrder": ["Name", "Transform"]
    }
  ]
}
```

Entity IDs and component UUID fields are decimal strings. A `u64` can exceed JavaScript's safe
integer range, so the reader accepts either strings or numbers but the writer always emits strings.
The entity's `IdComponent` is represented by the top-level `id` field.

`componentOrder` preserves the Inspector's authored component order. Before writing, the registry
reconciles it with the components actually present: removed names drop out, duplicates collapse, and
newly present components append in canonical registry order. `Relationship` and `Bone` do not appear
in this display order even though their durable data can serialize.

## Registry dispatch

`ComponentRegistry::serialize_entity` walks its rows and calls the stored `has` and `serialize`
function pointers. The result is a JSON object keyed by registered component name. Each built-in row
dispatches to that type's `SceneSerialize::to_json` implementation.

```rust
for traits in &self.rows {
    if (traits.has)(scene, entity) {
        components.insert(
            traits.name.to_string(),
            (traits.serialize)(scene, entity),
        );
    }
}
```

Runtime data has no registry row. `WorldTransform`, `PoseOverride`, `MorphWeightOverride`, and
`ComponentOrder` therefore stay out of the component object. `Relationship` and `SkinnedMesh` do
have rows, but their `SceneSerialize` implementations emit only durable UUID data and omit resolved
handle caches.

Asset-placement previews are another explicit exclusion. `scene_to_json` skips every entity tagged
with `PreviewGhost`, so saving during a drag does not persist the temporary model subtree.

## Loading a scene

`scene_from_json` validates the root, schema version, entity array, and entity IDs before rebuilding
the world. It clears the ECS, creates each entity with `spawn_with_id`, then asks the registry to
deserialize every named component. The stored UUID survives even though the live `hecs` handle does
not.

An unknown component name logs a warning and is skipped. A malformed body for a known component
returns `Error::Deserialize` with that component's name. Versions outside the accepted `1..=4`
range, a missing entity array, and an entry without an ID are errors.

Cross-entity references resolve after every entity has been created. `relink_hierarchy` maps parent
and skin-joint UUIDs to live handles, which permits a child to appear before its parent in the JSON
array. It also supplies root relationships where absent and sanitizes invalid parent links. See
[scene hierarchy](../scene-hierarchy/) for those invariants.

The reader supplies defaults for fields omitted by an accepted document. A missing environment uses
the default environment, an absent relationship makes the entity a root, and an absent
`componentOrder` derives the canonical registry order.

## File boundary

`write_scene` passes `scene_to_json` through `dump_json_sorted` and writes two-space-indented JSON;
every object key is sorted recursively. `read_scene` reads the file with `parse_json` and hands the
value to `scene_from_json`.

Project saving uses the same `scene_to_json` value but embeds it in the wider project document. The
[project serialization](../../geometry-and-assets/project-serialization/) page covers the catalog,
render settings, and editor sidecars around that scene block.

## In the code

| What | File | Symbols |
|---|---|---|
| Whole-scene conversion | `scene/src/document.rs` | `scene_to_json`, `scene_from_json`, `SCENE_VERSION` |
| Standalone file I/O | `scene/src/document.rs` | `write_scene`, `read_scene` |
| Registry dispatch | `scene/src/registry.rs` | `serialize_entity`, `deserialize_entity`, `component_order` |
| Component bodies | `scene/src/serde.rs` | `SceneSerialize` implementations |
| UUID JSON encoding | `json/src/lib.rs` | `uuid_to_json`, `json_u64_or`, `WireUuid` |
| Project embedding | `assets/src/project.rs` | `save_project`, `load_project` |

## Related

- [Component registry](../component-registry/) — registry rows and per-type operations.
- [Scene hierarchy](../scene-hierarchy/) — durable parent UUIDs and runtime handle caches.
- [Project serialization](../../geometry-and-assets/project-serialization/) — the surrounding project document.
- [JSON gateway](../../core-and-conventions/json-gateway/) — parsing, dumping, and typed readers.
