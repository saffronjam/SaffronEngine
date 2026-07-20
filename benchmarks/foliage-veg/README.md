# Foliage and vegetation baselines

These records measure Anima itself on named hardware. They are regression references for the
foliage and vegetation planset, not performance claims borrowed from another engine.

Run the Phase 1 fixture against a freshly built host:

```sh
cargo build --manifest-path engine/Cargo.toml -p saffron-host
bun tools/bench-foliage-phase1/measure.ts
```

The fixture creates 512 deterministically arranged built-in meshes, one sun, one spot light, and one
point light at a 1280 × 720 viewport. It records a 32-frame timestamp capture, the scene-gather and
frame-time distributions, draw and shadow counts, exact `InstanceData` traffic, active RT instances,
retained mesh-query memory, device capabilities, and validation errors. A platform without hardware
ray tracing records that absence rather than pretending to exercise the RT path.

Each baseline carries budgets derived from its own steady-state p95 with 25% headroom and a small
absolute noise floor. A later implementation compares against the record for the same hardware class.
Cross-device comparisons use separate records because CPU, GPU, driver, and Vulkan translation costs
are not interchangeable.

Run the shared Rust/Slang conformance corpus on the selected physical Vulkan device:

```sh
just compute-conformance
```

The command regenerates the exact shader artifacts first, executes the Phase 1 spatial-numeric
goldens and the Phase 3 resident graph-program corpus, and writes machine-readable JSON to standard
output. The record binds every result to the physical device and driver UUIDs, resident-program ABI,
corpus and reference hashes, complete shader-manifest identity, and validation-layer counts. Software
Vulkan devices are rejected because they do not qualify a supported hardware profile.

`compute-conformance-apple-m4-moltenvk.json` is the physical Apple M4 record. Its 32 spatial words
include the full-width probability primitive, and its four resident programs qualify all seven
dual-domain operators against the same canonical Rust output with zero validation issues.
