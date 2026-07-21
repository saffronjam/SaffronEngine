+++
title = 'Spatial world'
weight = 8
+++

# Spatial world

The spatial world gives every large-scale system one exact coordinate, cell, surface, residency, and
deterministic-numeric vocabulary. Serialized identity stays stable when rendering moves its local
origin, and vegetation, terrain, streaming, physics, editing, navigation, and network interest can
refer to the same region without inventing feature-specific grids.

## Exact positions and hierarchical cells

A `WorldPosition` contains a level-zero `WorldCellKey` and a quantized local position. A level-zero
cell is a half-open 64-metre cube. Local coordinates use 4096 ticks per metre, giving a quantum of
about 0.244 mm. Converting a floating point location uses round-to-nearest, ties-to-even; cell
ownership then uses Euclidean floor division, including below zero.

The half-open owner rule has one answer at every face. A point on a cell's maximum face belongs to
the neighbouring cell, while halo samples never change ownership. This matters at negative
coordinates, where truncation toward zero would assign a boundary to the wrong cell.

`WorldCellKey` levels span exact powers of two from level 0 through level 62. Parent, child,
ancestor, and neighbour operations reject overflow. The canonical 25-byte key stores the level
followed by a big-endian, 192-bit [Morton ordering](https://en.wikipedia.org/wiki/Z-order_curve) of
the ZigZag-encoded signed axes. Decimal wire fields preserve the complete integer domain without
passing 64- or 128-bit values through a JavaScript number.

```rust
let position = WorldPosition::from_world_meters(DVec3::new(-0.0002, 5.0, 64.0))?;
let owner = position.cell();
let coarse = owner.ancestor(4)?;
```

Rendering converts a `WorldPosition` relative to an explicit render origin. Moving that origin
changes only the small floating point value sent to render-side code; the position's global ticks,
owner key, canonical bytes, and serialized identity remain unchanged.

## Deterministic decisions

Persistent decisions use checked fixed-point and normalized numeric types. Division and curve
interpolation use ties-to-even rounding, comparisons use canonical integer bits, non-finite floats
are rejected, and overflow is an error. Floating point remains appropriate for rendering and
offline geometric evaluation, but it does not decide a persistent macro identity by accident.

Randomness is random-access rather than iteration-driven. `RandomDomain` separates streams by map,
stable node GUID, node semantic revision, owner cell, candidate and ancestor identity, species, and
named channel. `RandomStream` folds that vocabulary into
[Philox4x32-10](https://random123.com/), so job order and worker count cannot advance shared state or
reshuffle another node. A whole-graph content hash invalidates cached work but is deliberately not a
random key.

The matching Slang implementation is authoritative only on a platform that passes the Rust/GPU
byte-golden test. The renderer requires 64-bit shader integers because the shared fixed arithmetic
and random-access sample counter depend on them.

## Surfaces and fields

`SurfaceField` is the query contract for geometry and environmental data. Every provider publishes a
stable provider ID, revision, exact bounds, primitive count, maximum weighted tags per hit, and
capabilities before work is dispatched. The query boundary rejects a hit that exceeds the declared
tag maximum.
Ray, directional projection, and nearest queries return an exact world position, geometric tangent
frame, UV or projection coordinates, weighted tags, revision, and a stable primitive attachment with
canonical barycentrics when the provider can preserve it.

The initial static-mesh provider uses the mesh's cached BVH for arbitrary-direction queries. It
handles non-uniform affine scale in world metric and reports authoritative attachments. A skinned
mesh reports the deformed surface shown in the viewport, but explicitly disables nearest queries,
authoritative attachments, and authoritative fields. It never substitutes a bind-pose attachment.

Field channels cover altitude, slope, curvature, concavity, drainage, moisture, temperature,
precipitation, sunlight, exposure, water distance and depth, signed blockers, spline distance, and
stable user channels. Availability and cardinality are planning queries. Runtime-authoritative field
decisions require canonical quantized tiles; arbitrary floating point mesh intersections remain
suitable for editor queries and offline cooking. After a provider edit, attachment reprojection
either resolves against the declared revision or returns an orphan. It never chooses a different
primitive silently.

## Facet residency and publication

A `SpatialSource` requests independently reference-counted render, physics, simulation, editing,
navigation, and network facets. Its exact position, velocity prediction, per-level load and cleanup
radii, priority, and stable ID determine claims. Cleanup radii cannot be smaller than load radii,
which gives each source explicit hysteresis. Multiple sources add references to the same facet-cell
pair instead of taking ownership away from one another.

Async work carries `GenerationToken { cell, source_revision, generation }`. Beginning or cancelling a
generation invalidates older tickets. `GenerationSlot` publishes one complete `Arc` under a lock, so
readers see the complete old value or complete new value. The deterministic priority queue changes
latency only; result bytes derive from canonical inputs, not scheduling order.

## Inspecting the spatial world

The control plane exposes the same DTOs to the editor and `sa`:

```sh
sa -o json spatial-cell --world '{"x":-0.0002,"y":5,"z":64}' --level 4
sa -o json spatial-providers
sa -o json spatial-sample --provider 42 --channel altitude --position '{"x":0,"y":10,"z":0}'
sa -o json spatial-residency
```

`spatial-cell` reports exact decimal global ticks, the level-zero owner, the requested ancestor, and
canonical key bytes. Provider and residency listings are sorted by stable identity, making their
JSON useful in tests and diagnostic diffs.

## In the code

| What | File | Symbols |
|---|---|---|
| Coordinates and cells | `engine/crates/spatial/src/coordinate.rs` | `WorldCellKey`, `WorldPosition`, `WorldBounds` |
| Canonical numerics and randomness | `engine/crates/spatial/src/numeric.rs`, `random.rs` | `DecisionScalar`, `RandomDomain`, `RandomStream` |
| Surface contract | `engine/crates/spatial/src/surface.rs` | `SurfaceField`, `SurfaceHit`, `SurfaceAttachment` |
| Residency and publication | `engine/crates/spatial/src/residency.rs` | `SpatialSource`, `ResidencyManager`, `GenerationSlot` |
| Static mesh provider and scene queries | `engine/crates/assets/src/mesh_surface.rs`, `render_scene.rs` | `StaticMeshSurfaceProvider`, `query_scene_surface_ray` |
| GPU numeric goldens | `engine/assets/shaders/spatial_numeric.slang`, `spatial_numeric_test.slang` | `spatialRandomSample`, `computeMain` |
| Control diagnostics | `engine/crates/control/src/commands_scene.rs` | `register_scene_commands`, `spatial-cell`, `spatial-sample` |

## Related

- [Picking](../picking/)
- [Performance telemetry](../../frame-and-render-graph/performance-telemetry/)
- [Shared control types](../../tooling-and-control/shared-types/)
