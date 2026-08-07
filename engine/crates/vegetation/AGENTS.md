# saffron-vegetation — vegetation value contracts

The authored and persistent value contracts for the whole vegetation system: the three authored
asset documents, the point schema, the biome and botanical graph IRs, the evaluator, the cooked
artifact containers, the runtime cell store, and the ecology rules. Its `lib.rs` states the scope
rule that governs everything here:

> This crate owns authored and persistent vegetation value contracts. It performs no asset-server
> I/O, rendering, physics, scene-ECS mutation, control transport, or editor work. Those systems
> consume these formats without becoming alternate sources of vegetation truth.

Concepts are documented in `docs/content/explanations/` — [vegetation-assets], [botanical-graph],
[biome-graph-evaluation], [vegetation-cooking], [point-interchange] under `geometry-and-assets/`,
and [vegetation-state], [ecology-catchup] under `scene-and-ecs/`. This file is the rules, not the
concepts: what breaks, and what must move together.

## Layout

Thirty-four modules, all declared private in `lib.rs`. Sixteen are directory modules whose
`mod.rs` fixes the module's public surface; the other eighteen are single files.

| Cluster | Modules |
|---|---|
| Authored documents | `asset/` (the three documents as values), `codec/` (their binary form + schema hashes), `point.rs` (the 25-column macro-point schema), `layer.rs` (map layer algebra + provenance) |
| Biome graph | `graph/` (typed IR, compilation, authority flow), `graph_gpu/` (resident program ABI + Rust reference interpreter) |
| Evaluation | `evaluator/` (the largest module, 34 files: reference + parallel evaluation, preflight, surface queries), `merge.rs` (map-level composition in GUID order) |
| Botanical graph | `botanical/` (the IR, `BotanicalElementId`, integer trig in `shape.rs`), `botanical_edit/` (manual edits + orphan diagnostics), `botanical_compile/` (grown assembly → meshes/parts/spines) |
| Plant normalization | `plant_compile/` (format-erased imported families), `virtual_hierarchy.rs` (adapters onto `saffron-geometry`) |
| Derived artifacts | `artifact/` (`.svegcell` / `.splantc` containers plus the KTX2 texture payload in `texture.rs`), `cell_facet.rs` (the typed facet payloads inside a `.svegcell`), `manifest.rs` (the immutable world manifest), `cook/` (`ContentHash`, `CookVersionSet`, the cook graph), `cook_work.rs` (the distributed work-item manifest) |
| Runtime + persistence | `runtime_world/` (`VegetationWorld`, generations, residency, queries), `mutation/` (typed persistent mutations), `state_codec/` (state + save containers) |
| Network session | `network/` (base-manifest handshake, per-cell facet interest, checkpoint fingerprints, late join) |
| Ecology | `ecology.rs` (the clock and state), `ecology_region.rs` (dependency-region closure plus the `CellSpatialIndex` grid the closure and halo lookup query), `ecology_tick/` (`advance_cell`, the pure rules), `season.rs` |
| Interchange | `interchange.rs` (Houdini/JSON points), `interchange_usd.rs` (USDA `PointInstancer` text form) |
| Identity | `identity.rs` (`derive_procedural_plant_id`, collision table), `hash.rs` (the one SHA-256) |
| Internal only | `binary.rs` (big-endian primitive codec), `canonical.rs` (`CanonicalSink`), `memory.rs` (checked allocation), `error.rs` |

Tests are `#[cfg(test)]` modules beside the code they cover — inline at the bottom of a single-file
module, and a `tests.rs` or `tests/` submodule inside a directory module. Golden values are inline
literals: this crate does not use the repo's `fixtures/golden/` mechanism and has no on-disk
fixtures. `tests/operator_coverage/` is the only integration test — one `main.rs` binary over
`contracts`, `evaluation`, `scheduling`, `surface`, and shared `fixtures` modules.

## Rules that are easy to break

- **Value contracts only — no I/O, no device, no ECS.** There is no `std::fs`, no `ash`, no `hecs`,
  and no socket in this crate, and adding one moves truth to the wrong layer. The filesystem for
  these formats is `saffron-assets` (`vegetation_store/` writes the content-addressed store;
  `vegetation_cooker/` and `plant_cook/` drive cooking). The crate re-exports
  `saffron_material::*` rather than restating surface, coverage, thin-sheet, or opacity-micromap
  vocabulary — `saffron-material` owns that, and a second definition here is a second truth.
- **`lib.rs` glob-re-exports 28 of the 34 modules, so `pub` publishes instantly and silently.** A new
  helper marked `pub` becomes crate API the moment you save, with no `lib.rs` edit to review. New
  internals are `pub(crate)`. `binary`, `canonical`, `memory`, and `state_codec` are internal in
  full (`state_codec` publishes only inherent impls on already-exported types); `error`, `hash`, and
  `season` re-export selected names only. (`saffron-spatial` uses explicit named re-exports and is
  the better pattern — but this crate's shape is the one you have to work with.)
- **No float is hashed, cooked, persisted, or fed to a rule.** This is narrower and more useful than
  "no floats": floats do exist, at the system boundaries. `evaluator/` takes them from surface
  providers, `interchange.rs` / `interchange_usd.rs` from DCC files, `plant_compile/mesh.rs` from
  imported meshes, and `runtime_world/` hands them out on the ephemeral ray/nearest query surface.
  Every one of those quantizes at the boundary (`DecisionScalar::from_f64`, `snorm16`,
  `quantize_orientation`, `SignedUnit::from_f64`) and the quantized value is what travels inward.
  `season.rs` reads one `f32` latitude, for a hemisphere sign test only — it never reaches
  arithmetic. No other module contains a float at all, and **no lint enforces this** —
  `clippy::float_arithmetic` is off — so it holds only if you keep it.
- **Trigonometry is integer, never libm, and there are two routines.** `turn_sin_cos` in
  `botanical/shape.rs` is a 17-entry quarter-turn table, and its doc comment is the reason: "A table
  rather than `f64`: an authored plant must grow the same on every target, and a libm difference of
  one bit would move a branch." `botanical/grow.rs`, `botanical_compile/`, and `botanical_edit/` all
  call it. `cordic_sin_cos` in `evaluator/math.rs` is a 16-iteration Q30 CORDIC feeding
  `yaw_quaternion_q15` (the cooked orientation column) and the random-direction path in
  `evaluator/generate.rs`. Both are swept and pinned by digest — `turn_table_sweep_is_byte_pinned`,
  `cordic_sweep_is_byte_pinned`, `cooked_yaw_orientation_sweep_is_byte_pinned` — so a target that
  answers one bit differently fails rather than cooks. Nothing here calls `f64::sin`.
- **Every map is a `BTreeMap`/`BTreeSet`.** `grep -rn "HashMap\|HashSet" engine/crates/vegetation/src`
  returns nothing, and must keep returning nothing — iteration order reaches published bytes. There
  is no `rand::` and no `SystemTime` either. `Instant::now()` appears only in deadline and
  cancellation guards under `evaluator/`, where timing can abort a result but never alter one.
- **Identity is derived from ancestry, never from a counter or a slot.** `BotanicalElementId::child`
  mixes the producing node with the parent and the ordinal, which is what lets a manual edit survive
  a parameter change — an edit addresses *that* element, not the seventh thing generated. Changing
  `mix` orphans every authored edit in every document. Never mix in a value that changes on save: a
  variation's source id derives from its index alone, because folding in the catalog-assigned family
  id went stale the moment the catalog assigned a different one. Graft ids reserve the top two bits
  so an author cannot collide with a derived identity.
- **`derive_procedural_plant_id` is a fixed nine-field preimage.** Domain string, namespace byte,
  map, layer guid, node address, node semantic revision, candidate, ancestor, seed namespace, owner
  cell bytes, family — appended in that order at those widths. Reordering, rewidening, or bumping a
  node's semantic revision when its meaning did not change re-keys plants that should have been
  stable. The golden digest is pinned in `identity.rs`; if it moves, you changed identity.
- **Every packed tile grid linearizes with Z fastest and X slowest**: `(x * dims[1] + y) * dims[2] + z`,
  written by `evaluator/math.rs::tile_index` and inverted by `tile_sample_position`. It governs the
  authored `AuthoredFieldTile::values` payload *and* the cooked `MicroFieldTile::density` payload,
  which `saffron-assets` copies to the GPU verbatim. Three readers decode it and all three must
  agree: `EvaluationFieldTile::sample_index`, `runtime_world/query.rs::query_micro_ray`, and
  `microTexel` in `engine/assets/shaders/scene_micro_common.slang`. Square dimensions turn a
  transposed decode into a symmetric relabel that no square-dims fixture can see, so a test that
  pins this order uses non-square dimensions
  (`micro_density_texels_linearize_with_z_fastest`,
  `micro_ray_reads_a_non_square_tile_in_the_canonical_texel_order`).
- **Thirty-six domain-separated hash preimages exist**, each a `saffron-anima/…/vN` string next to
  the encoder it protects. A domain string, a field order, and a field width are all part of the
  contract. Version the string in the same change as the encoder.
- **A format change moves four things together:** the writer, the reader, the version constant, and
  the schema-hash domain string. The codec tests corrupt the version byte and the first schema byte
  precisely to prove both are load-bearing. Magic strings do not carry the version (`SPLANT01` holds
  `PLANT_ASSET_VERSION = 7`) — except `SVEGMAN4` and `SVCGPH04`, where the digit is part of the
  magic and both move at once.
- **No migrations, ever.** A noncurrent graph, node, asset, artifact, or state version is rejected
  with a typed error. There is no best-effort reinterpretation of an unknown or corrupt section.
  Bump the schema identity, break the callers, and regenerate the goldens in the same change.
- **Artifact bytes are reproducible, which pins the zstd framing.** `.svegcell` / `.splantc` sections
  encode at `ZSTD_COMPRESSION_LEVEL = 10` and `ZSTD_WINDOW_LOG = 27` with checksum on, dictid off,
  content size on, and long-distance matching off; a section stores `Zstd` only when it is actually
  smaller, otherwise `Raw`. Changing a knob — or the zstd version — changes every artifact hash.
  Sections are written sorted by kind and duplicates are a hard error.
- **Simulation ownership is runtime-only state, like bulk suppression.** `plant_authority` and
  `authority_epoch` on `VegetationWorld` record who last claimed *where a plant is* or *whether it
  exists*; `claims_simulation_ownership` in `runtime_world/state.rs` is the closed list of mutations
  that claim, and everything else is biology. Neither is persisted and neither survives
  `replace_persistent_state`, which clears the map and bumps the epoch — that is how a save load, a
  snapshot import, and a network join are all noticed by one test.
- **The promotion write-back's velocity reaches readers through the generation, not the delta.**
  `CellPersistentOverlay::from_state` lifts each plant's `promotion_origin` velocity into the
  published generation so `VegetationPlantSnapshot` carries it; a build path that skips the overlay
  publishes a cell whose plants have silently come to rest.
- **`mutation_tag` is a fixed discriminant table.** Persisted ordering is
  `(cell bytes, mutation_tag, plant id, idempotency key)`, so reordering the `VegetationMutation`
  variants without preserving their tags reorders every persisted tail. Add new variants at the end
  with new tags.
- **`CookVersionSet::current()` participates in every cook identity.** Bump `numeric` when
  `DecisionScalar`, `UnitInterval`, `div_round_ties_even`, `turn_sin_cos`, or `cordic_sin_cos`
  semantics change; bump `evaluator` when evaluation results change; `validate()` rejects a zero in
  any field.
- **`ECOLOGY_SIMULATION_VERSION` mismatch is a hard error, never a silent re-simulation.** Its doc
  comment carries the trigger: bump it whenever a rule, a coefficient, an evaluation order, or a
  numeric convention changes. State simulated under old rules must not quietly advance under new
  ones.
- **A catch-up round computes in parallel and commits in canonical region order.** `advance_ecology`
  spreads the pure per-region rule evaluation across `EcologyCatchUpBudget::workers` and then commits
  the results in the order the regions were given, so the worker count reaches how long a call takes
  and nothing else. `catch_up_is_identical_across_worker_counts` compares `canonical_bytes()` at one
  worker against three over an eight-region world: three shards of eight join out of index order, so
  the result reordering is load-bearing at those numbers and a commit that followed completion order
  fails outright. It also asserts `EcologyCatchUpReport::workers`, which `compute_region_ticks`
  reports from the branch it actually took — otherwise a call that silently ran serial would pass a
  bytes-only comparison. The budget is spent a round at a time, so one region cannot starve another
  out of a call.
- **The dependency-region partition is memoized on `ecology_ground_revision`, not rebuilt per read.**
  Building it is a closure over every planted cell in the world, and both the per-frame catch-up poll
  and the per-cell `simulation_facet_is_settled` reader need it. The revision moves when a generation
  is published or unloaded and when a mutation changes which cells carry plants; `publish_staged`
  takes `&mut self` for exactly that reason. Adding a publication path that skips the bump leaves the
  partition — and the residency it records — stale.
- **The point schema hash covers every column.** `point_schema_hash` folds each of the 25
  `POINT_SCHEMA_COLUMNS` by id, element type, and name. Adding, removing, renaming, retyping, or
  reordering a column invalidates every `.svegcell` macro-point section and every persisted point.
- **CPU cost tracks macro rows, never micro blade count.** `MacroBvh` is built over macro row bounds
  alone and the micro field is a quantized tile, so a cell reconstructing fifty thousand blades costs
  the same bytes and the same traversal as one reconstructing fifty. Every `runtime_world/query.rs`
  entry point drains its traversal into `VegetationQueryCost`, which is what makes that testable —
  a query that walked the tiles and discarded them returns identical plants and would otherwise go
  unnoticed. `cpu_bytes_and_query_work_track_macro_rows_never_micro_blade_count` compares two worlds
  that differ only in density.
- **`cell_facet.rs` has no direct tests.** It is covered only through an evaluator test that decodes
  the canonical encoder output. Know that before changing a facet decoder.
- **`saffron-wind` is the determinism exception, and it is not a dependency here.** That crate is
  entirely `f32`/`f64` and calls `sin`/`cos`/`powf`; it gives same-binary reproducibility, not the
  cross-target bit-exactness this crate is built on. A wind value must never become a cooked,
  hashed, or persisted vegetation input.

## Formats

Authored documents live in the project and are versioned by the user. Derived artifacts live in the
content-addressed store at `<project>/cache/vegetation/`, beside `assets/` rather than inside it,
and are disposable: deleting the cache loses no authored and no persistent runtime state. That holds
because persistent state has a separate durable root, `<project>/state/vegetation/` — the `.svegstate`
baseline a map ships is the one derived-looking file no cook reproduces. It binds to one generation
and a recook publishes another, so it rebases onto the incoming base rather than being dropped
(`VegetationState::rebase`): the identity moves, every delta crosses, and a delta the new base has no
ground for is inert rather than deleted.

| Extension | Magic | Version const | Owner |
|---|---|---|---|
| `.splant` | `SPLANT01` | `PLANT_ASSET_VERSION` | `codec/plant.rs` |
| `.sbiome` | `SBIOME01` | `BIOME_ASSET_VERSION` | `codec/biome.rs` |
| `.svegmap` | `SVEGMAP1` | `VEGETATION_MAP_VERSION` | `codec/map.rs` |
| `.svegmap` chunk | `SVEGCH01` | `VEGETATION_MAP_CHUNK_VERSION` | `codec/map.rs` |
| `.svegcell` | `SVEGCEL1` | `VEGETATION_CELL_ARTIFACT_VERSION` | `artifact/cell.rs` |
| `.splantc` | `SPLANTC2` | `PLANT_COMPILED_ARTIFACT_VERSION` | `artifact/plant.rs` |
| `.svegmanifest` | `SVEGMAN4` | `VEGETATION_BASE_MANIFEST_VERSION` | `manifest.rs` |
| `.svegcook` | `SVCGPH04` | `CookVersionSet` | `cook/graph.rs` |
| `.svegstate` | `SVEGST01` / `SVEGSV01` | `STATE_VERSION` / `SAVE_VERSION` | `state_codec/` |
| cook work plan | `SVCWRK01` / `SVCWPL01` / `SVCWCP01` | — | `cook_work.rs` |

`.svegmapc` is the compiled map, named and written by `saffron-assets`
(`vegetation/map_package.rs`).
`.svegcell` and `.splantc` are never catalog assets and cannot be imported or edited.

Binary conventions: big-endian throughout, except the GPU program word stream in
`graph_gpu/program.rs` and the KTX2 texture container in `artifact/texture.rs`, both of which are
little-endian because the consumer reading them is. `bool` is a `u8` that rejects any byte other than 0 or 1. Every reader ends
with `complete()`, so trailing bytes are an error. Lengths are `u64` in `binary.rs` but `u32` in
`codec/stream.rs::Writer` — do not mix the two writers. JSON embedded in binary goes through
`dump_json_sorted`, never `dump_json`.

## Vocabulary

| Term | Meaning |
|---|---|
| family | One `.splant`: a normalized plant species. Its source is exactly one of an imported recipe or a native botanical graph. |
| variation | One authored individual of a family (`seed`, `age`, `name`). Age scales lengths and radii continuously and changes nothing else. |
| phenotype | An appearance of a variation, by `PhenotypeRole` (Healthy, Harvested, Damaged, Burned, Dead, Flowering, Fruiting, Senescent, Wet). Never inferred from the active mesh. |
| combination | A cooked `(variation, phenotype)` pair, resolving to the bitmask of assembly uses that draw. |
| lifecycle | Typed `PlantLifecycle` (Seed … Removed). Distinct from phenotype and from calendar time. |
| element / axis / frame / shell | A grown botanical part; a grown spine or limb; an attachment site on an axis; a swept cross-section surface. |
| graft | Substitution of an external hero mesh for one placement, keeping that element's identity and frame. |
| cell | `WorldCellKey` — the storage, dependency, compilation, residency, and simulation unit. Never a visual-quality unit: cell-wide culling and cell-wide LOD are forbidden. |
| facet | An independently resident demand class on a cell (Render, Physics, Simulation, Editing, Navigation, NetworkInterest). |
| halo | The immutable neighbour region a partitioned job reads. Halo evaluation never changes ownership. |
| layer / chunk | An ordered element of the map algebra; a sparse addressable piece of a `.svegmap` so one stroke rewrites only what it touched. |
| provenance | The interned lineage handle from map through layer, graph, node, and candidate to the plant. |
| generation | A published immutable cell payload. Any change republishes with a new id; consumers diff by id. |
| manifest / baseline | The immutable world identity that saves bind to; the persistent state a map boots into, one per map, naming the generation it was reduced against. |
| region | The transitive dependency closure an ecology tick needs resident before it may run. |

## What this system is not

These are design decisions, not omissions: there is no terminal billboard or octahedral impostor, no
artist-authored foliage LOD chain, no material-WPO wind path, no CPU foliage draw loop, no
terrain-only grass system, and no one-hecs-entity-per-decorative-plant model. Micro grass is
cosmetic — individual blades never reach saves, collision, navigation, ecology, or gameplay queries.

[vegetation-assets]: ../../../docs/content/explanations/geometry-and-assets/vegetation-assets.md
[botanical-graph]: ../../../docs/content/explanations/geometry-and-assets/botanical-graph.md
[biome-graph-evaluation]: ../../../docs/content/explanations/geometry-and-assets/biome-graph-evaluation.md
[vegetation-cooking]: ../../../docs/content/explanations/geometry-and-assets/vegetation-cooking.md
[point-interchange]: ../../../docs/content/explanations/geometry-and-assets/point-interchange.md
[vegetation-state]: ../../../docs/content/explanations/scene-and-ecs/vegetation-state.md
[ecology-catchup]: ../../../docs/content/explanations/scene-and-ecs/ecology-catchup.md
