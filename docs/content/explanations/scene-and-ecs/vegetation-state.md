+++
title = 'Vegetation state'
weight = 9
+++

# Vegetation state

Vegetation state gives authored, procedural, and runtime plants one authority. Stable identities,
immutable cell generations, and manifest-bound deltas let rendering, simulation, editing, and
gameplay read the same effective plant state without owning it.

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

## Runtime cell generations

`VegetationWorld` binds to one exact [cooked manifest](../../geometry-and-assets/vegetation-cooking/)
and owns the reduced persistent state for that generation. Each known cell has a
`GenerationSlot<VegetationCellGeneration>`. A reader receives an `Arc` snapshot, so publication can
replace the slot while an older reader finishes safely.

A cell load begins with a `GenerationToken` containing the cell, source revision, and monotonic cell
generation. Staging verifies the artifact hash, cell and payload identities, platform profile, and
complete section directory against the manifest. It decodes only the requested facets, applies the
persistent cell delta, builds the macro bounds index, and returns a private generation.

`GenerationSlot::try_publish` swaps that complete value only while the token remains current. A
source update, cancellation, or newer load invalidates the token, so late work returns without
changing the published cell. Existing `Arc` snapshots keep the prior generation alive until their
readers release them.

This trace shows a handle crossing a state change:

```text
source revision 12 requests Physics for cell (0, 0, 0)
begin_load                  -> generation token 4
publish_staged              -> cell generation 4
find_plant                  -> handle { plant: 01ab..., generation: 4 }
confirmed Tombstone         -> cell generation 5
resolve_handle(old handle)  -> StaleGeneration { expected: 4, current: 5 }
```

The public handle carries `PlantId` and `VegetationCellGenerationId`. Its private `PlantSlot` is only
a row and generation tag. Row indices can change when the effective columns are rebuilt; stale rows
therefore cannot escape through an API.

## Facet residency

A [spatial source](../spatial-world/) requests cell facets through velocity prediction, hierarchy
levels, and separate load and cleanup radii. The cleanup radius supplies hysteresis. Multiple
sources contribute reference counts to the same cell-facet pair, and removing one source leaves the
other claims intact.

Logical facets map to these cooked sections:

| Facet | Cell sections |
|---|---|
| Render | Macro points, micro fields, render references, render bounds |
| Physics | Macro points, collision inputs — consumed by [vegetation collision residency](../../physics/vegetation-collision/) during play |
| Simulation | Macro points, micro fields, ecology boundary, ecology checkpoint |
| Editing | Macro points, provenance, rejection diagnostics, surface attachments and dependencies |
| Navigation | Macro points, navigation contributions |
| Network | Macro points, ecology boundary, ecology checkpoint |

Every logical facet includes macro points, which keeps stable plant identity available to its
adapter. `VegetationResidencyBudgets` sets an independent decoded-byte ceiling for each facet.
Admission visits cells by source priority and stable cell order. Demand beyond a ceiling remains
visible in requested-byte accounting without publishing an oversized generation.

`VegetationResidencyReport` separates requested and resident byte counts and lists coalesced missing
facets. Releasing demand republishes a generation containing only retained sections. Quantized micro
tiles store density, typed attribute channels, and a reconstruction seed; individual blades are
cosmetic and do not become runtime records. Persistent disturbance masks remain per-tile state and
are overlaid whenever a generation is rebuilt.

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

## Runtime queries

Macro queries use a cell-local bounds tree built from the effective `PlantPointColumns`. Bounds,
radius, ray, and nearest queries scan every CPU-resident macro generation, independent of render or
collision visibility. Results have stable ordering by `PlantId`, or by distance then `PlantId` where
distance matters.

`VegetationQueryFilter` can select families, require all listed family tags, and restrict lifecycle
or interaction policy. A result contains `PlantId`, generation-tagged handle, exact position,
conservative bounds, family tags, biological state, and interaction policy. Provenance is present
only when the editing facet supplies its table.

The ray query intersects conservative vegetation bounds. It is not a physics raycast and does not
claim that a render-only or simulation-only plant has a collision body. Physics queries remain
limited to collision-resident objects.

## Biological time

Biological age is its own axis, counted in fixed ecology ticks by a clock that only moves forward.
The calendar and time of day are presentation inputs: rewinding them previews a different season,
and it cannot un-grow a tree, revive a dead one, or re-emit an event.

Three independent guards hold that asymmetry, none of them a caller convention. The clock refuses a
target behind itself, and a cell may only step to its successor tick, so a skipped tick cannot drop
a generation and a repeat cannot double-apply one. The reducer rejects an `ecology_tick` that
regresses. Events come from committed transitions, and a replayed transaction commits nothing.

Persisted ecology state carries the rule-set version it was produced under, world time, and one
boundary summary per cell — the plant count, canopy, health, moisture and fuel a neighbouring cell
needs without reading its plants. A snapshot decoded under a different rule-set version is a loud
mismatch rather than a silent re-simulation. [Ecology ticks and catch-up](../ecology-catchup/)
covers how those summaries let unloaded ground come forward.

## Typed transitions

A committed mutation is where a vegetation change becomes observable. The reducer emits one typed
transition per committed record — `Damaged`, `Harvested`, `Burned`, `Removed`, `Planted`,
`Regrew`, `LifecycleChanged`, `Ignited`, `Extinguished`, `Wetted`, `StateReplaced`, `Moved`,
`Disturbed` — carrying the
transaction, the cell, and the plant the record named. Scripts, VFX, audio, quests, fire, and
navigation dirtying all read the same stream, so no consumer needs to diff state to notice a
change.

Delivery is cursor-based over a ring of the last `VEGETATION_EVENT_RING_CAP` transitions, exactly
like physics contacts: each consumer keeps its own sequence number, and a cursor older than the
retained tail is told it overflowed rather than handed a gap. Only confirmed commits reach the
ring — a transient prediction and an idempotent replay both emit nothing, so a transition is
observed exactly once.

```sh
sa vegetation-drain-events
#   #7      damaged             plant=40aabbccddeeff00112233445566778899
#   #8      disturbed           cell-wide
#   high=8  oldest=1  overflowed=no  (2 events)
```

Cosmetic response stays out of this stream. Bend prediction lives in the GPU interaction field and
is never persisted; only confirmed crush, clear, and damage state becomes a `DisturbanceMask` or a
plant delta, and only those emit a transition.

## Persistent mutations

Persistent state has a fixed precedence:

```text
authored sources -> cooked base -> confirmed persistent delta -> transient prediction or cosmetics
```

`VegetationWorld::apply_confirmed_mutations` is the persistent write boundary. It clones the state,
calls `reduce_mutations`, rebuilds every changed resident cell privately, and publishes the candidate
only after all work succeeds. `apply_prediction` uses the same reducer for a transient overlay above
confirmed state. Rejecting a prediction republishes the remaining overlay, while confirmation sends
that transaction through the persistent write boundary. Snapshots contain confirmed state only.

`reduce_mutations` handles field patches, additions, removals, overrides, planting, damage, moisture
and fuel, lifecycle changes, harvest, burn, ignition and extinguishing, regrowth, promoted state,
and disturbance masks. Each
record carries a cell, transaction, authority, logical tick, idempotency key, and optional base
revision.

The reducer sorts transactions canonically, verifies exact replays, and rejects reused IDs with
different contents. It evaluates a multi-cell transaction on a cloned candidate state and publishes
only after every cell precondition succeeds. Cell revisions and changed-cell output follow canonical
cell order.

The `vegetation-mutate` control command is the wire form of the same boundary: a batch of typed
`VegetationMutationRecordDto` records (each header plus one mutation) decodes into the reducer's
exact vocabulary and applies through `apply_confirmed_mutations`. Editing a generated plant from
the editor writes an override, pin, or anchor mutation this way — never a transform into cooked
cell bytes.

```sh
sa -o json vegetation-mutate '{"records":[{"header":{...},"mutation":{"kind":"tombstone","plant":"…"}}]}'
```

## Snapshot and tail persistence

`SaveStateEnvelope` contains a canonical reduced snapshot followed by a canonically ordered mutation
tail. Reading the envelope applies that tail through `reduce_mutations`; compaction performs the same
reduction and emits the result as a new snapshot with an empty tail. Duplicate tail transactions are
accepted only when their canonical signatures match, so replay and compaction produce the same
state.

The binding includes the exact manifest and cook-graph identities, the schema, compiler, evaluator,
numeric, and simulation contract versions, and a hash of every named seed namespace. Every field
must equal the expected runtime binding. A mismatch is an error rather than an inferred migration.

Snapshot and save containers carry format magic, a schema identity, payload length, payload hash,
and a final commit marker. Their decoders reject truncation, trailing data, corrupt payloads,
non-canonical ordering, and bytes that do not reproduce under canonical encoding.

`EditorJournalEnvelope` keeps gesture preimages and inverse operations separate from runtime save
state. `NetworkMutationEnvelope` adds transport sequence and an optional authoritative snapshot.
Both reuse the same mutation records instead of defining another state transition model.

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
| Typed transitions and delivery | `vegetation/src/mutation.rs`, `runtime_world.rs` | `VegetationTransitionKind`, `VegetationEvent`, `drain_events` |
| Biological clock and checkpoints | `vegetation/src/ecology.rs` | `EcologyClock`, `EcologyState`, `EcologyCellSummary`, `checkpoint_identity` |
| Combustible state a fire system reads | `vegetation/src/runtime_world.rs` | `combustion_sample`, `VegetationCombustionSample`, `PlantFlags::IGNITED` |
| Runtime generations and queries | `vegetation/src/runtime_world.rs` | `VegetationWorld`, `VegetationCellGeneration`, `VegetationPlantHandle` |
| Strict snapshot codecs | `vegetation/src/state_codec.rs` | `VegetationState::from_canonical_bytes`, `SaveStateEnvelope::from_canonical_bytes` |
| Facet demand and publication | `spatial/src/residency.rs` | `ResidencyManager`, `GenerationToken`, `GenerationSlot` |
| Scene singleton | `scene/src/component.rs`, `scene.rs` | `VegetationField`, `Scene::add_component` |
| Generated wire DTOs | `protocol/src/vegetation_dto.rs`, `xtask/src/protocol/ts.rs` | `PlantId`, `VegetationMutationDto`, `emit_sa_types` |

## Related

- [Vegetation assets](../../geometry-and-assets/vegetation-assets/) — authored family, biome, and map ownership
- [Vegetation cooking](../../geometry-and-assets/vegetation-cooking/) — immutable cell artifacts and manifest identities
- [Spatial world](../spatial-world/) — exact coordinates, predictive sources, and generation publication
- [Scene serialization](../scene-serialization/) — registry-driven component persistence
- [Shared control types](../../tooling-and-control/shared-types/) — generated Rust, TypeScript, and OpenRPC contracts
