# saffron-vegetation-gpu — the graph compute adapter

The Vulkan adapter for **biome graph evaluation**, plus the evidence that GPU evaluation matches the
Rust reference byte-for-byte. It is a pure adapter: `saffron-rendering` on one side
(`ComputeDispatch`, `ComputeBuffer`, `Device`, `ShaderArtifactContract`), `saffron-vegetation` on the
other (`GraphGpuProgram`, `GraphGpuInvocationBatch`, `GpuQualificationRegistry`). Those two plus
serde, sha2, and thiserror are the whole dependency set.

**This is not the vegetation draw path.** Plants are drawn by `saffron-rendering` through the
persistent GPU scene, fed by `saffron-assets` (`plant_render.rs`, `gpu_scene_mirror.rs`). The
MoltenVK indirect-draw workaround people come looking for is there too — `ExecutorDrawInputs`'
`draw_bound` in `rendering/src/visibility.rs` — not here. This crate only runs graph programs.

| File | Owns |
|---|---|
| `executor.rs` | `VulkanGraphComputeExecutor`, the only implementation of `saffron_vegetation::GraphComputeExecutor` |
| `conformance.rs` | `ComputeConformanceEvidence` — the profile, spatial-numeric, graph-program, and validation record |
| `tests/conformance_records.rs` | Holds every checked-in record in `benchmarks/foliage-veg/` to the current corpus, ABI, and shader input |
| `tests/baseline_records.rs` | Holds the phase-1 performance records in `benchmarks/foliage-veg/` to their own derivation, device identity, and absence rules |
| `lib.rs` | The crate `Error` enum and re-exports |

Both `tests/*_records.rs` files are this crate's custody of `benchmarks/foliage-veg/`. Neither needs
a GPU, and both fail rather than skip when a checked-in record stops describing this tree.

The GPU half of the contract is `engine/assets/shaders/vegetation_graph.slang`, which imports
`spatial_numeric.slang`. Concept documentation is
`docs/content/explanations/geometry-and-assets/biome-graph-evaluation.md`.

## Rules that are easy to break

- **Construction is fail-closed and must stay that way.** `VulkanGraphComputeExecutor::new` rejects
  a non-physical device, then qualifies the *entire* canonical corpus against the Rust reference
  under `QUALIFICATION_TIMEOUT` before returning. An executor that exists has already proven
  byte-equality for every operator. Do not add a "skip qualification" path, a lazy mode, or a
  partial corpus — the value of the type is that holding one is proof. Evidence is minted per
  operator, so the corpus has to drive each operator's *branches*, not just its opcode: adding a
  combine operation, a ramp form, or a curve shape without a corpus invocation licenses a path no
  run ever executed. `corpus_drives_every_dual_domain_operator_branch` enforces what is enumerable.
- **The shader contract is a frozen const declaring its own source closure.**
  `VEGETATION_GRAPH_ARTIFACT` names the module, the source, the SPIR-V, and every source file that
  contributes. The closure is declared, not discovered, so adding an `import` to the shader without
  adding the file here produces an artifact identity that does not change when the source does.
- **The ABI moves in lockstep, in one change:** `GRAPH_GPU_ABI_VERSION` and the ABI descriptor
  string in `saffron-vegetation`'s `graph_gpu.rs`, `graph_gpu_abi_hash()`, the magic and layout
  constants mirrored in `vegetation_graph.slang`, `qualification_corpus_hash()`, and the evidence
  fields in `conformance.rs`. Changing the shader alone leaves the hashes claiming the old layout.
- **Never take a blocking lock on the dispatcher.** The dispatch loop polls `try_lock` and
  `park_timeout`s against a deadline, which is what keeps `GraphCancellationToken` responsive. A
  plain `lock()` makes cancellation a lie and can wedge the frame.
- **Output decoding is strict, never saturating.** Non-canonical tags and out-of-range data are
  rejected. A decoder that clamps turns a GPU disagreement — exactly what this crate exists to
  catch — into a plausible wrong answer.
- **Behaviour keys on feature bits, never on vendor identity.** `vendor_id`, `device_id`,
  `driver_id`, the device UUIDs, and `is_molten_vk()` are recorded *into the evidence* and read by
  nothing that chooses a code path. Selection keys on advertised features and limits.
- **A GPU result may only affect authoritative macro state after conformance passes** on the target
  platform. Cross-vendor floating-point output is not assumed bit-identical; the evidence record is
  what licenses the GPU path, per platform.

## Running conformance

```sh
cargo run -p saffron-vegetation-gpu --example compute_conformance
```

It emits the evidence JSON checked into `benchmarks/foliage-veg/`, one file per validated platform.
A record certifies exactly one corpus, so moving the corpus, the ABI, or the shader source closure
means re-running the recipe on every platform that carries a record, in the same change —
`tests/conformance_records.rs` fails on a record left behind.
`tests/vegetation_graph.rs` is the real-device integration test, and it is the only evidence the
device path works. It fails rather than skips when no adapter is reachable or the one it gets is
software, so source `tools/gpu-driver.sh` before `cargo test` — a green run without it would be a
run that proved nothing.
