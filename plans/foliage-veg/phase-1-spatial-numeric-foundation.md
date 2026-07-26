# Phase 1 — Shared spatial and numeric foundation

**Status:** IN PROGRESS

**Depends on:** nothing

This phase fixes the cross-cutting coordinate, cell, surface, residency, and deterministic-numeric
contracts before vegetation assets or tools exist. It adds a leaf `saffron-spatial` crate used by
vegetation and later adopted by terrain and large-world streaming. It does not add a foliage-private
grid or camera-owned world model.

## Outcome

- A hierarchical `WorldCellKey` identifies logical world regions independently of origin rebasing,
  camera order, or a specific feature.
- `WorldPosition` separates global integer cell coordinates from high-precision cell-local values.
- `SurfaceField` exposes geometry hits and typed scalar/vector fields over meshes now and terrain,
  splines, water, roads, and voxels later.
- `SpatialSource` and facet-based residency express independent render, physics, simulation, edit,
  network-interest, and navigation demand with hysteresis and prediction.
- `GenerationToken` prevents canceled/superseded async work from publishing stale data.
- Canonical fixed/quantized arithmetic, sorting, hashing, and counter-based RNG primitives are
  specified and implemented identically in Rust and Slang where GPU execution is permitted.
- Representative baseline fixtures and counters record Anima's own current CPU/GPU/memory costs.

## Fixed contracts

### Coordinates and cells

- [x] Add `engine/crates/spatial/` with typed errors and no dependency on scene, assets, vegetation,
  rendering, Jolt, editor, or networking.
- [x] Define a signed 64-bit global cell coordinate, hierarchy level, and canonical Morton/key byte
  encoding. Parent/child/neighbour/ancestor operations must be exact and checked for overflow.
- [x] Define `WorldPosition` as `(WorldCellKey at base level, quantized local position)`, plus explicit
  conversion to/from floating render-relative space. Origin rebasing never changes serialized identity.
- [x] Specify local-position quantum, exact rounding mode, tie behavior at cell faces, endianness,
  range, saturation/overflow rejection, and negative-coordinate floor division.
- [x] Make one canonical owner rule: a point belongs to the half-open cell containing its quantized
  position; halo evaluation never changes ownership.
- [x] Define hierarchy levels by exact powers of two. Coarse outputs are produced once at their owner
  level and referenced by descendants; descendants cannot regenerate the same logical object.

### Canonical numerics and randomness

- [x] Add fixed/normalized numeric types for every decision that can affect macro acceptance,
  competition, identity, lifecycle state, or persistence.
- [x] Specify curve interpolation, division/rounding, comparison/tie-breaking, NaN/Inf rejection,
  checked overflow, canonical sort keys, and byte serialization.
- [x] Implement a pinned Philox/Random123-style counter generator. Its key vocabulary includes map,
  stable node GUID, node semantic revision, cell, candidate/ancestor, species, and random channel.
- [x] Keep whole-graph/content hashes out of random stream keys; they invalidate cache but must not
  reshuffle unrelated nodes.
- [x] Add Rust/Slang golden vectors for every RNG lane and numeric primitive. A GPU implementation is
  usable for authoritative work only when byte-equivalence is proven on every supported platform.

### Surface and field contract

- [x] Define `SurfaceProviderId`, stable primitive/surface attachment identity, provider revision,
  and dirty-world-bounds notifications.
- [x] Define arbitrary-direction ray/project/nearest queries returning position, normal, tangent
  frame, UV/projection data, stable attachment, weighted material/surface tags, and provider revision.
- [x] Define typed scalar/vector channels and derivatives for altitude, slope, curvature, concavity,
  drainage, moisture, temperature, precipitation, sunlight/shade, exposure, water distance/depth,
  signed blockers, spline distance, and user fields.
- [x] Provide bounds/cardinality/availability queries so graph planning can predict work and report
  missing data before dispatch.
- [x] Require canonical quantized surface tiles for cross-machine runtime-authoritative generation.
  Arbitrary floating mesh intersections may feed offline cook only until quantized.
- [x] Specify attachment reprojection/orphan policy after provider edits; never silently attach a
  manual plant to a different primitive.

### Residency and jobs

- [x] Define `ResidencyFacet` independently for render, physics, simulation, editing, navigation,
  and network interest. One cell can hold any subset with reference counts from multiple sources.
- [x] Define `SpatialSource` position, velocity/prediction, per-level radii, facet mask, priority,
  load/cleanup hysteresis, and stable source identity.
- [x] Add cancelable job tickets with `GenerationToken { cell, source_revision, generation }`.
  Publication rejects any token that is no longer current.
- [x] Require atomic publication: readers observe the previous complete generation or the next one,
  never partially replaced arrays.
- [x] Provide deterministic priority queues whose scheduling changes latency, not output.

## Initial mesh provider

Implement the first `SurfaceField` adapter over current static meshes and their cached BVHs. Replace
the narrow `SceneSurfaceHit { entity, point, distance }` return shape in
`engine/crates/assets/src/render_scene.rs` with the shared hit vocabulary, including geometric normal,
tangent/UV when present, material/surface tags, provider/primitive identity, and revision. `pick_entity`
remains a consumer of this shared query; it is not a second ray-triangle implementation.

The provider must support arbitrary projection direction and render-relative conversion. Skinned
surfaces can remain non-authoritative placement targets until they expose stable deformation-aware
attachments, but that limitation must be explicit in capabilities rather than silently falling back
to bind pose.

## Baseline fixtures and telemetry

- [x] Add small seam/negative-coordinate/origin-rebase fixtures and a large heterogeneous forest
  specification used by later phases.
- [x] Capture current CPU scene-gather, draw count, `InstanceData` upload, shadow, RT, and memory
  metrics before renderer changes.
- [x] Define quality invariants: no silhouette/coverage/transmission/shadow/GI discontinuity;
  performance budgets are recorded from Anima measurements rather than copied vendor claims.
- [x] Add `sa` read-only spatial inspection commands for cell key conversion, provider listing, field
  sampling, and source/residency status. Register commands/fixtures/DTOs once in `saffron-protocol`.

## Acceptance

- [x] Cell encode/decode, parent/child, face ownership, negative coordinates, and origin rebasing pass
  exhaustive boundary/property tests.
- [ ] RNG and fixed numeric goldens are byte-identical in Rust and Slang on NVIDIA, AMD, and MoltenVK.
  (HARDWARE-GATED, and the gate is the only thing holding it. This machine enumerates exactly one
  Vulkan device — `Apple M4` through `MoltenVK`, api 1.4.334 — so the NVIDIA and AMD arms cannot be run
  here at all, and no amount of code closes them. What IS verified on MoltenVK: the dual-domain
  qualification corpus covers every declared operator with resident programs, the ABI and corpus hashes
  are pinned, branching and terminal masks preserve exact semantics, and only complete
  program/profile/artifact evidence is admitted — `saffron-vegetation` `graph_gpu` 7/7. Recorded rather
  than claimed: a box that says three vendors is not closed by one.)
- [x] Shuffled job order, worker count, source order, and cancellation cannot change published bytes.
- [x] A late result with an old generation token is discarded in a named race test.
- [x] Mesh surface queries return the same hit, normal, tags, and attachment after render-origin
  rebasing.
- [x] `saffron-spatial` has no forbidden dependency edge.
- [x] `just engine`, `just prepare-for-commit`, `just schema`, `just test`, and `just e2e` are green.
- [x] Add/update the spatial-world docs page and hub row using the docs-page skill during implementation.

## Platform conformance

- MoltenVK on Apple M4: Rust/Slang numeric goldens pass on the physical GPU.
- NVIDIA: pending access to a physical supported GPU.
- AMD: pending access to a physical supported GPU.

## NO-LEGACY gate

There is one cell key, coordinate policy, residency-source vocabulary, and surface query contract.
Any current placement/picking ray helper migrated here is deleted or reduced to a thin caller in this
phase. Future terrain and large-world work must consume these types rather than introduce another grid.
