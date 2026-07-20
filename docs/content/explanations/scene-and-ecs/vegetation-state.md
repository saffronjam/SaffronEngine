+++
title = 'Vegetation state'
weight = 9
+++

# Vegetation state

Vegetation state gives authored, procedural, and runtime plants one identity and one persistent
mutation model. Stable IDs, schema-hashed point columns, ordered layers, and manifest-bound deltas
keep edits and saves independent from render resources.

## Scene binding and ownership

A scene contains at most one `VegetationField` component. It stores the active `.svegmap` UUID and an
enabled flag. Local biomes and edited regions are layers inside that map, so adding a second field to
another entity fails with a singleton-component error.

```json
{
  "VegetationField": {
    "map": "4103",
    "enabled": true
  }
}
```

The component is part of the built-in registry and scene document codec. Asset usage and deletion
analysis treat `map` as a catalog reference.

## Plant identity

`PlantId` is an opaque 128-bit value written as 32 lowercase hexadecimal digits. The high bits
separate procedural, explicit authored, and authority-issued runtime namespaces. JSON and generated
TypeScript keep the value as a string, so JavaScript never rounds it as a number.

Procedural IDs hash a domain tag plus the map, layer and node GUIDs, node semantic revision,
candidate and ancestor keys, seed namespace, canonical owner cell, and plant family. The hash uses
[SHA-256](https://csrc.nist.gov/pubs/fips/180-4/upd1/final), with pinned big-endian encodings. Whole-graph
hashes and accepted-array positions are absent from the preimage, so an unrelated node edit does not
renumber retained plants.

Every cooked result passes through `PlantIdCollisionTable`. A duplicate 128-bit value is a hard
error. Seed or topology edits can compare the old and new accepted ID sets through
`identity_conflicts`, which lists invalid pins and overrides explicitly.

## Point columns and layers

`PlantPointColumns` is the canonical CPU structure of arrays. Its fixed vocabulary includes
identity, owner and local position, orientation, scale, bounds, family and phenotype data, candidate
lineage, biological state, policy, provenance, and surface attachment. Registered extension columns
use typed numeric IDs and packed row strides.

The column descriptor list has a pinned schema hash. GPU layouts are projections of this table, not
another point definition. Render-origin changes affect only temporary relative coordinates; the
world position, owner cell, point bytes, and plant ID stay unchanged.

Map inputs use one ordered `VegetationLayer` algebra. Each layer carries a stable ID, coordinate
space, bounds, dependency set, order, revision, and lock or mute state. Operators cover fields,
species weights, density, masks, volumes, splines, anchors, pins, overrides, and blockers.

## Persistent mutations

Persistent state has a fixed precedence:

```text
authored sources -> cooked base -> confirmed persistent delta -> transient prediction or cosmetics
```

`reduce_mutations` is the only reducer for field patches, additions, removals, overrides, planting,
damage, moisture and fuel, lifecycle changes, harvest, burn, regrowth, promoted state, and disturbance
masks. Each record carries a cell, transaction, authority, logical tick, idempotency key, and optional
base revision.

The reducer sorts transactions canonically, verifies exact replays, and rejects reused IDs with
different contents. It evaluates a multi-cell transaction on a cloned candidate state and publishes
only after every cell precondition succeeds. Cell revisions and changed-cell output follow canonical
cell order.

Three envelopes keep transport policy separate from mutation meaning. `EditorJournalEnvelope` holds
gesture preimages and inverses. `SaveStateEnvelope` binds a compact snapshot and tail to one exact
base manifest. `NetworkMutationEnvelope` adds transport sequence and an optional snapshot while using
the same records and reducer.

Biological age advances through monotonic `ecology_tick` values. Phenology remains a separate closed
value, so changing scene calendar appearance does not reverse age or mortality. Weather and wind do
not alter persistent placement unless a mutation writes durable state.

## In the code

| What | File | Symbols |
|---|---|---|
| Plant IDs and collision audit | `vegetation/src/identity.rs` | `PlantId`, `ProceduralPlantIdentity`, `PlantIdCollisionTable` |
| Point schema and lifecycle | `vegetation/src/point.rs` | `PlantPointColumns`, `POINT_SCHEMA_COLUMNS`, `PlantLifecycle` |
| Layer algebra and provenance | `vegetation/src/layer.rs` | `VegetationLayer`, `VegetationLayerOperator`, `ProvenanceTable` |
| Persistent reducer and envelopes | `vegetation/src/mutation.rs` | `VegetationState`, `VegetationMutation`, `reduce_mutations` |
| Scene singleton | `scene/src/component.rs`, `scene.rs` | `VegetationField`, `Scene::add_component` |
| Generated wire DTOs | `protocol/src/vegetation_dto.rs`, `xtask/src/protocol/ts.rs` | `PlantId`, `VegetationMutationDto`, `emit_sa_types` |

## Related

- [Vegetation assets](../../geometry-and-assets/vegetation-assets/) — authored family, biome, and map ownership
- [Spatial world](../spatial-world/) — exact coordinates, owner cells, fields, and deterministic numerics
- [Scene serialization](../scene-serialization/) — registry-driven component persistence
- [Shared control types](../../tooling-and-control/shared-types/) — generated Rust, TypeScript, and OpenRPC contracts
