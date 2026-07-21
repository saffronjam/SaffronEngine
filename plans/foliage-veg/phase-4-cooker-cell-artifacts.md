# Phase 4 — Incremental cooker and immutable artifacts

**Status:** IN PROGRESS

**Depends on:** Phases 2–3

This phase turns authored plant/biome/map sources into canonical content-addressed artifacts. It
normalizes imported plant families, cooks spatial cells incrementally, tracks exact provenance and
dependencies, and publishes atomically. Rendering-specific cluster/voxel pages are completed in
Phase 6, but `.splant` is already the sole source and `.svegcell` already has its final sectioned
ownership model.

## Plant source normalization

- [x] Add one `.splant` import/recook route in `saffron-assets`. Source recipes may reference current
  glTF/OBJ-imported `.smodel`/`.smesh`/`.smat` assets or standard USD/Houdini/SpeedTree-originated
  exports; no source-specific runtime component is created.
- [x] Normalize units, axes, handedness, origin/pivot, scale, winding, tangents, UVs, material slots,
  and semantic plant parts.
- [x] Validate bounds, crown/root footprints, leaf orientation and coverage, structural skeleton/
  spines and weights, collision/breakage/nav proxies, variations, phenotype/life-state compatibility,
  source license/attribution, and missing semantic mappings.
- [x] Keep derived mesh/material/atlas/skeleton/collision data private to `.splant` compilation.
  Imported referenced assets remain legitimate source assets, but there is one compiled family
  interface and one recook route.
- [x] Emit visible reimport conflicts when topology/semantic targets disappear; never silently drop
  manual plant overrides.

## Content-addressed cook graph

- [x] Build dependency nodes for source assets, material/coverage schema, biome IR, map chunks,
  surface tiles/provider revisions, hierarchy/global stages, cell halos, compiler/schema versions,
  and platform profile.
- [x] Hash canonical bytes, not file timestamps. A source edit invalidates exactly intersecting
  downstream cells plus declared support halos/ancestor dependencies.
- [x] Execute jobs in parallel with cancellation and deterministic results. Publish output under its
  final content hash atomically only after validation/checksum succeeds.
- [x] Record work estimates, actual time, peak memory, input/output sizes, dependencies, cache hits,
  and rejection totals for CLI/editor inspection.

## Base manifest

Define a canonical vegetation-world manifest containing:

- world/map UUID and schema/compiler/evaluator/numeric versions;
- exact graph/plant/surface/map dependency hashes and world seed namespaces;
- point-column schema hashes and plant table;
- `WorldCellKey → cell content hash` entries by hierarchy level;
- cell bounds, neighbours/halos, species/macro/micro counts, memory/work estimates, and integrity
  checksums; and
- simulation/save compatibility version.

Saves and future network sessions bind to the complete manifest identity. Matching seed alone is
insufficient.

## Sectioned `.svegcell`

Use a TOC/content-addressed subchunk layout so facets can reside independently:

- canonical macro point/state columns;
- micro density/attribute tiles;
- expanded/compact provenance and rejection diagnostics;
- surface attachment and dependency tables;
- render references/bounds (payload pages arrive in Phase 6);
- collision broadphase/proxy derivation inputs;
- navigation obstacle/cost contributions;
- ecology boundary summaries/checkpoint seed data; and
- section hashes, codecs, sizes, alignment, and version.

The cell is immutable derived base data. It contains no authored brush truth and no player mutation.
Unknown/corrupt/version-incompatible sections fail with typed errors; no best-effort reinterpretation.

## Tiled `.svegmap` storage

- [x] Store the logical manifest separately from sparse quantized field tiles, anchor/override chunks,
  graph instances, and layer metadata.
- [x] Address chunks by map/layer/tile key and content hash; writing one brush transaction touches
  only intersecting chunks and the manifest root.
- [x] Persist quantized field results. Gesture replay is optional editor metadata and never required
  to reproduce map truth.
- [x] Provide transactional multi-cell/map writes with canonical lock/order and rollback on failure.

## Commands, tests, and editor asset routing

- [x] Add `vegetation-cook`, `vegetation-cook-status`, `vegetation-cell-inspect`,
  `vegetation-manifest`, and plant validate/recook commands through the central command table so `sa`
  gets them without bespoke CLI code.
- [x] Route Plant/Biome/VegetationMap assets into asset-editor placeholders with validation,
  provenance, dependencies, and cook statistics; visual rendering arrives later.
- [x] Add canonical byte fixtures, corrupt/truncated/version rejection, cache deletion/rebuild,
  cancellation races, dependency invalidation, negative-coordinate cells, and reimport conflict tests.

## Acceptance

- [x] Repeated full/incremental cooks produce identical manifests and cell bytes under different
  worker counts and schedules.
- [x] Editing one bounded field/source invalidates only its dependency region and declared halos.
- [x] Canceled/superseded jobs cannot publish; readers never observe a partial generation.
- [x] Deleting every `.svegcell`/`.splantc` cache artifact and recooking preserves authored bytes and
  returns the identical manifest.
- [x] No `.svegcell` or `.splantc` appears in the asset catalog or project references.
- [x] Imported plants from different source families normalize to the same `PlantFamily` contract.
- [ ] Cook commands, schemas, e2e fixtures, standard gate, and cooking docs are green.

## NO-LEGACY gate

There is one cooker and one artifact ownership model. Runtime generation later invokes the same cell
evaluator and artifact writer; it is a scheduling/residency mode, not a second procedural system.
