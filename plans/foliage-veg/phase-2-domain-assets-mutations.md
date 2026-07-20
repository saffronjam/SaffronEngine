# Phase 2 — Vegetation domain, assets, identity, and mutations

**Status:** COMPLETED

**Depends on:** Phase 1

This phase defines all authored truth and persistent state semantics before a brush or simulation can
modify a plant. It adds a data-only `saffron-vegetation` crate, three vegetation-specific logical
catalog asset types, the canonical point schema, stable identity namespaces, one world component,
one layer algebra, and one mutation reducer. Rendering and Jolt remain downstream adapters.

## Crate and dependency boundary

- [x] Add `engine/crates/vegetation/` depending on `saffron-core`, `saffron-json`,
  `saffron-geometry`, and `saffron-spatial` only.
- [x] Keep asset-server I/O, GPU resources, Jolt, scene ECS, control, and host concerns out of the
  crate. `saffron-assets`/`saffron-runtime` integrate its pure formats and reducers later.
- [x] Add typed `Error` variants for format, identity, schema, mutation, manifest, and numeric
  failures; no stringly core results.

## Three vegetation-specific logical assets

### `.splant`

Define `PlantFamilySource` as exactly one union:

- imported-family recipe: source files/assets, reimport settings, semantic part mapping, units/axes,
  licensing/provenance; or
- embedded native botanical graph.

Both produce one normalized family payload. Intrinsic plant data includes semantic trunk/branch/root/
frond/leaf/flower/fruit/blade parts; physical dimensions and crown/root footprints; material slots;
structural skeleton/spines; stiffness/drag/bend/flutter/damage response; phenotype/life-state variants;
collision/breakage/nav proxies; interaction policy defaults; and optional habitat-preference defaults.
Biome-local density, spacing, community weights, and competition remain outside `.splant`.

### `.sbiome`

A `.sbiome` is either a root biome or a reusable typed graph module through one declared role and
parameter interface. It owns palette weights, density, clustering, suitability bindings, spacing,
competition, companion/child relations, succession, graph policy, and seed namespaces. Subgraphs are
ordinary `.sbiome` references with cycle rejection and bounded recursion; no fourth graph asset type.

### `.svegmap`

One logical catalog asset manifest owns sparse internal authored chunks: tiled scalar/vector/species
fields, blockers, volumes, splines, local biome instances/parameters, explicit plants, pins, authored
transform/state overrides, and layer metadata. A one-stroke edit rewrites only touched chunks. The map
never owns player/runtime save state or compiled cells.

Add `Plant`, `Biome`, and `VegetationMap` to `AssetType`/`AssetTypeDto` and every frozen extension,
scan, naming, content-hash, rename/move/delete, thumbnail, selector, store import, and editor routing
map. Add `.splant`, `.sbiome`, and `.svegmap` version/schema readers and canonical writers. Generated
`.splantc`/`.svegcell` files stay outside `AssetType` and cannot be imported or edited.

## Stable identity and point schema

- [x] Add opaque `PlantId([u8; 16])` with a canonical lowercase hexadecimal wire/string form; it is
  never a JavaScript number.
- [x] Pin hash algorithm, byte layout, namespace tag, endian encoding, and collision audit.
- [x] Use three namespaces: deterministic cooked procedural IDs valid for an exact base manifest;
  stored GUIDs for authored explicit plants; authority-issued IDs for runtime plants.
- [x] Derive procedural IDs from map/layer/node GUID, node semantic revision, candidate/ancestor,
  seed namespace, and canonical owner cell—not accepted-array order or whole graph hash.
- [x] Add a cooker collision table and hard error on any duplicate 128-bit ID.

Define one schema-hashed, typed columnar point vocabulary. Required columns are:

- plant ID, owner cell, quantized cell-local position, orientation, scale, and conservative bounds;
- plant family, variation, phenotype/life-state, and representation-class IDs;
- deterministic key/candidate identity and optional parent/colony plant ID;
- age, health, moisture, fuel, phenology, authored/runtime flags, and interaction policy;
- compact provenance handle; and
- stable surface attachment/provider/primitive identity plus projection data.

Extensible columns use registered typed IDs and packed column layouts, never per-point string maps.
CPU cells are canonical; GPU layouts are generated projections of this schema.

## Provenance and layer algebra

Every candidate records compact lineage:

```text
map → layer → biome/subgraph → node → sampler candidate → plant/variation
```

Accepted and rejected debug tables expand those handles for inspection and invalidation.

Define one ordered layer algebra. Each layer carries stable ID, coordinate space, bounds, blend/
operator semantics, dependency set, deterministic order, lock/mute state, and revision. Operators cover
scalar/vector fields, species weights, density, include/exclude, volumes, splines, anchors, pins,
transform/state overrides, and blockers. Editing a generated plant creates a typed override; it never
mutates a compiled cell. Durable field truth is the quantized tile result. Optional brush gesture
history is non-authoritative tooling metadata, not a second evaluation source.

## Lifecycle and environment boundary

- [x] Define typed plant lifecycle states and transitions for seed/sprout/juvenile/mature/senescent/
  dead/stump/removed plus species-declared harvested/damaged/burn phenotypes.
- [x] Keep monotonic biological `ecology_tick` separate from calendar/phenology time. Moving the
  editor calendar backwards changes appearance, not age or mortality.
- [x] Keep authored ecological baseline fields separate from dynamic Environment signals. Wind,
  current season, wetness, and weather appearance do not recook placement unless an explicit
  simulation mutation changes persistent state.

## One mutation reducer, distinct envelopes

Define `VegetationMutation` variants for field-tile patches, anchors/additions, tombstones/removal,
transform/state overrides, planting, damage, moisture/fuel, lifecycle transition, harvest, burn,
regrow, promotion-origin state, and disturbance masks. Each mutation carries cell, target,
transaction ID, authority, logical tick, idempotency key, and base/precondition revision where needed.

Implement one deterministic reducer and state precedence:

```text
authored sources → cooked base → confirmed persistent runtime delta → transient prediction/cosmetic
```

Use distinct envelopes over the same mutation/reducer:

- editor journal: gesture grouping plus inverse/preimage for undo/redo;
- save state: compact canonical snapshot plus tail against an exact manifest; and
- future network: sequenced idempotent operations plus snapshots.

Do not pretend these are one physical append-only log; their retention and ordering needs differ.
Cross-cell edits acquire cells in canonical key order and publish one transaction atomically.

## Scene, protocol, and material contract

- [x] Add exactly one serialized scene-level `VegetationField { map: Uuid, enabled: bool }` component.
  Local areas are layers inside the referenced map; multiple competing field components are rejected.
- [x] Register it in `register_builtin_components`, `BUILTIN_COMPONENT_NAMES`, `COMPONENT_NAMES`,
  schema fragments, inventories, fixtures, inspector order, create/inspect/remove paths, and scene serde.
- [x] Add generated DTOs for `PlantId`, asset summaries, points/provenance, layers, mutations,
  manifests, and the field component. Rust remains the DTO source; regenerate TypeScript.
- [x] Extend `.smat` with one `surfaceModel` selector and `ThinSheetFoliage` parameters: front/back
  albedo response, thickness, absorption/transmission color, roughness, normal behavior, coverage
  source, and energy-conservation constraints.
- [x] Pin coverage-preserving mip metadata, spatial hash inputs, alpha classification, voxel material
  moments, and optional OMM derivation fields before plant geometry is cooked.

## Acceptance

- [x] All three asset kinds round-trip canonical bytes and participate in every asset-management route.
- [x] `.svegmap` edits touch only intersecting chunks; whole-map rewrite tests fail.
- [x] Plant IDs round-trip Rust/JSON/generated TypeScript as opaque strings and survive origin rebasing.
- [x] Unrelated graph edits do not change deterministic IDs; seed/topology edits produce an explicit
  conflict/diff report for affected pins/overrides.
- [x] Mutation snapshot+tail compaction reduces to the same bytes as the full sequence under shuffled
  idempotent duplicates.
- [x] Cache deletion cannot remove any map, plant, biome, or persistent state bytes.
- [x] No dependency cycle or rendering/Jolt dependency enters `saffron-vegetation`.
- [x] Protocol/schema/editor checks and the standard milestone gate are green.
- [x] Add the plant assets and vegetation state docs pages plus hub rows.

## Verification

- `cargo test -p saffron-vegetation -p saffron-assets -p saffron-control -p saffron-protocol -p xtask`
- `cd editor && bun run check`
- `just engine`, `just prepare-for-commit`, `just schema`, `just test`, and `just e2e`
- `cd docs && hugo --gc`, the docs link checker, and the docs style checker
- `cargo tree -p saffron-vegetation --edges normal`
- `git diff --check`

## NO-LEGACY gate

There is one plant family asset, one biome graph asset, one tiled world map asset, one point schema,
one reducer, and one scene field. No generated `.smesh` copies become separately authored plant
truth, and no source format survives as a runtime renderer path.
