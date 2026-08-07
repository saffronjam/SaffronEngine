# saffron-spatial — deterministic world vocabulary

World coordinates, canonical integer numerics, counter-based randomness, surface fields, plant
identity, and facet residency. Every determinism rule in the vegetation system is written in this
crate's types, so a change here reaches every cooked artifact and every persisted plant.

Its `lib.rs` states the scope rule:

> This crate is a leaf foundation. It knows no scene, renderer, physics world, asset server, editor,
> or network transport. Systems publish providers and sources through these value-level contracts
> instead of defining feature-specific grids or coordinate policies.

## Layout

| Module | Owns |
|---|---|
| `numeric` | The canonical scalar types and the one rounding rule |
| `coordinate` | `WorldCellKey`, `WorldPosition`, `QuantizedLocalPosition`, `WorldBounds`, the grid constants |
| `random` | `philox4x32_10`, `RandomStream`, `RandomDomain` |
| `plant_identity` | `PlantId`, `PlantIdNamespace` |
| `residency` | `ResidencyFacet`, `ResidencyManager`, `GenerationToken`, `SpatialSource`, `ResidencySnapshot::admission_order` |
| `surface` | The `SurfaceField` provider contract and its field/query vocabulary |

Dependencies are `glam` and `thiserror` — nothing from Saffron. Keep it that way: a Saffron
dependency here inverts the DAG and every crate below it.

## Rules that are easy to break

- **`lib.rs` uses explicit named re-exports, not globs.** Publishing a type requires editing
  `lib.rs`, which is deliberate and reviewable. Do not switch it to `pub use module::*`.
- **The scale asymmetry is the easiest mistake in the subsystem.** `DecisionScalar` is
  `FixedI32<16>`, so one is `from_bits(65_536)`. `UnitInterval::ONE` is `u16::MAX`, which is
  **65_535** — `from_bits(65_536)` does not exist, and its `lerp` divides by `u16::MAX`. `SignedUnit`
  is symmetric ±32767 and rejects `i16::MIN`. Mixing the two scales silently shifts a value by one
  part in 65k, which survives every test that does not compare bytes.
- **`div_round_ties_even` is the one rounding rule.** Everything that quantizes uses ties-to-even
  through `i128` intermediates. A stray `as` cast or a `round()` introduces a different rule at one
  site and the artifact hashes diverge from there.
- **`FixedI32` arithmetic returns `Result`.** `checked_add/sub/mul/div` and `lerp` fail on overflow
  rather than wrapping, and `scale_i128` asserts `FRACTION_BITS <= 30` at compile time. Propagate
  the error; do not `unwrap` an overflow away.
- **`CanonicalF32` is the sanctioned float, for non-authoritative sort keys only.** It is
  finite-only, normalizes `-0.0`, and orders by `total_cmp`. It is not a licence to carry floats
  into a decision — see the boundary rule in `engine/crates/vegetation/AGENTS.md`.
- **`QuantizedOrientation` is canonical XYZW.** USD's `quatf`/`quath` is WXYZ; convert at that seam,
  in `saffron-vegetation`'s `interchange_usd.rs`, never by redefining the lane order here. `new()`
  rejects a quaternion whose length differs from unit by more than a small tolerance.
- **`DecisionCurve` requires strictly ascending unique abscissae and does not sort.** The caller
  supplies canonical order; a curve built from an unsorted list is an error, not a silent re-sort.
- **The grid constants fix every cooked artifact's identity.** `LOCAL_FRACTION_BITS = 12` gives
  `LOCAL_TICKS_PER_METER = 4096`, and `BASE_CELL_TICKS = 64 * LOCAL_TICKS_PER_METER`, with
  `MAX_HIERARCHY_LEVEL = 62`. Changing any of them re-keys every cell and invalidates every cooked
  artifact and persisted plant in every project.
- **Cell ownership is half-open on the max face.** A point exactly on a face belongs to the higher
  cell, exactly once. `WorldCellKey::canonical_bytes()` is a 25-byte level prefix plus a
  Morton-interleaved zigzag of the three axes, big-endian so byte order preserves locality. It feeds
  plant identity, so its encoding is frozen.
- **Randomness is counter-based, so results never depend on draw order.** `RandomStream::sample(i)`
  is random-access; there is no "next". `RandomDomain` has a fixed nine-field vocabulary folded in a
  fixed order, and perturbing any one field must separate the stream — there is a test for exactly
  that. Never key a stream on a whole-graph or whole-content hash: that invalidates caches *and*
  reshuffles unrelated nodes. Never introduce `rand::`.
- **`PlantId` is opaque, 128 bits, three namespaces, lowercase hex.** The top two bits of byte 0 are
  the namespace tag; tag `3` is unassigned and rejected. `FromStr` rejects uppercase and any length
  other than 32. On the wire and in the editor it is a **string** — never a JavaScript number.
- **Identity is never a slot.** No instance-buffer index, Jolt `BodyID`, hecs handle, accepted-array
  ordinal, or render LOD is ever plant identity. Slots are ephemeral and generation-tagged.
- **Every map is a `BTreeMap`/`BTreeSet`.** `grep -rn "HashMap\|HashSet" engine/crates/spatial/src`
  returns nothing and must keep doing so.
- **Facets are independent demand classes, refcounted per source.** A cell may be resident for
  Render and not for Physics. A facet nobody claims is a structurally dead feature — a source that
  fails to claim Physics means collision batches can never have rows, and nothing else reports the
  problem.
- **`GenerationToken` is the staleness guard.** Publication rejects a token that is no longer
  current, which is what prevents an async job from publishing against a superseded generation.

## Tests

Inline `#[cfg(test)] mod tests` per module; no `tests/` directory, no dev-dependencies, no on-disk
fixtures. Determinism is asserted with pinned literals (the Philox zero vector against the published
Random123 value, the stream sample goldens, the cell-key boundary encodings) and with invariance
tests that reverse or reshuffle inputs and compare canonical bytes.
