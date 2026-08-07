# Foliage and vegetation baselines

These records measure Anima itself on named hardware. They are regression references for the
foliage and vegetation planset, not performance claims borrowed from another engine.

Run the Phase 1 fixture, naming the record for the device it measures:

```sh
just bench-foliage-phase1 benchmarks/foliage-veg/phase-1-nvidia-rtx-3070-ti.json
```

The recipe builds the host, compiles the shaders, and points the Vulkan loader at this platform's
driver before it measures — a run that skips that step lands on the software rasterizer and records
a different machine than the filename claims.

The fixture creates 512 deterministically arranged built-in meshes, one sun, one spot light, and one
point light at a 1280 × 720 viewport. It records a 32-frame timestamp capture, the scene-gather and
frame-time distributions, draw and shadow counts, exact `InstanceData` traffic, active RT instances,
retained mesh-query memory, device-local memory occupancy against the driver's budget, device
capabilities, and validation errors. A platform without hardware ray tracing records that absence
rather than pretending to exercise the RT path.

Each baseline carries budgets derived from its own steady-state p95 with 25% headroom and a small
absolute noise floor. Cross-device comparisons use separate records because CPU, GPU, driver, and
Vulkan translation costs are not interchangeable.

Two checks read those budgets back.

`tests/baseline_records.rs` in `saffron-vegetation-gpu` runs inside `cargo test --workspace` and
needs no GPU. It re-derives every threshold from the observations beside it, holds each record's
platform block to the device its filename names, and rejects a threshold shared by two devices. A
zeroed capability column stays an absence: the budget key set is exhaustive, so an absent RT or
memory leg cannot arrive as a ceiling of zero.

```sh
just bench-foliage-check
```

That measures this machine and grades it against the record captured on this exact device, and is
the same step `tools/ci/check.sh` runs as step 8. Hardware with no record of its own defers rather
than borrowing another class's ceiling, as does a software rasterizer or a device that serves no
GPU timestamps. Only the p95 legs and the exact counters are graded — a record's p99 sits an order
of magnitude above its p95, because the capture window includes pipeline compilation.

Nothing is added to a ceiling at comparison time. The derivation already carries the 25% headroom,
and a second allowance would compound into a threshold only a doubling could breach. A leg that
goes over is measured again instead, and fails only when the breach reproduces.

`phase-1-nvidia-rtx-3070-ti.json` is the discrete-GPU record: hardware ray tracing on, 513 TLAS
instances, and a live GPU-memory reading. `phase-1-apple-m4-moltenvk.json` is the MoltenVK record;
it predates the memory telemetry and carries zeros for it, so re-run the recipe on an Apple device
to replace it.

Run the shared Rust/Slang conformance corpus on the selected physical Vulkan device:

```sh
just compute-conformance
```

The command regenerates the exact shader artifacts first, executes the Phase 1 spatial-numeric
goldens and the Phase 3 resident graph-program corpus, and writes machine-readable JSON to standard
output through `saffron-vegetation-gpu`, the sole adapter between vegetation programs and generic
renderer compute. The record binds every result to the physical device and driver UUIDs,
resident-program ABI, corpus and reference hashes, complete shader-manifest identity, and
validation-layer counts. Software Vulkan devices are rejected because they do not qualify a
supported hardware profile.

`compute-conformance-nvidia-rtx-3070-ti.json` is the physical NVIDIA record. Its 32 spatial words
include the full-width probability primitive, and its seven resident programs drive every dual-domain
operator branch — each combine operation, the constant ramp, the single-point curve, and the
value-free terminal form — against the same canonical Rust output with zero validation issues.

A record is evidence for one corpus. `every_conformance_record_binds_the_current_corpus_and_abi` in
`saffron-vegetation-gpu` fails as soon as a record's corpus, ABI, reference, or operator set drifts
from the tree, so every supported platform re-runs `just compute-conformance` in the change that
moves any of them. MoltenVK has no current record; run the recipe on an Apple device to add one.
