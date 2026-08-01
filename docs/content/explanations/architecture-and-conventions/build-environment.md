+++
title = 'Build environment'
weight = 5
+++

# Build environment

The toolchain lives in one pinned environment, not on whatever machine runs the command. On Linux
that environment is the **`saffron-build`** container; on macOS it is the host rustup toolchain
plus MoltenVK. The [`just`](https://just.systems/man/en/) recipes select the right one, so
`just engine` behaves the same from any shell.

## The `saffron-build` toolbox

A toolbox is a [Toolbx](https://containertoolbx.org/) container: a Podman-based development
environment that shares the home directory with the host and reaches the host filesystem at
`/run/host`. The Linux host is assumed to carry no compiler of its own; `cargo`, the Vulkan SDK,
and `slangc` all live inside `saffron-build`. Because home is the same directory, a file edited on
the host is visible inside immediately.

The container ships `rustc`/`cargo` 1.96.0 as Fedora packages, and `rust-toolchain.toml` pins the
same `channel = "1.96.0"` (with the `rustfmt` and `clippy` components) so a
[rustup-managed host](https://rust-lang.github.io/rustup/overrides.html) resolves the identical
toolchain. The workspace itself declares `rust-version = "1.85"` as its minimum supported Rust and
builds with `edition = "2024"`.

`xtask` resolves `slangc` in a fixed order: a `PATH` lookup, then `SAFFRON_SLANG_DIR/bin`, then
the toolbox cache at `~/.cache/saffron-slang/slang/bin`, which holds the pinned Slang 2026.10. A
missing `slangc` is a hard error (`find_slangc` bails); the build never fetches a prebuilt on its
own.

## Driving the build with `just`

The `justfile` at the repo root drives everything through `cargo`, the `xtask` helper, and `bun`:

```sh
just engine    # cargo build --workspace + cargo run -p xtask -- shaders
just test      # cargo test --workspace
just lint      # cargo fmt --check + cargo clippy --workspace -- -D warnings + editor oxlint
just run       # build the host, compile shaders, start the CEF editor shell
just e2e       # the tests/e2e bun suite against a headless host
just check     # the full reproducible gate
```

Every toolbox-bound recipe opens with the `reenter` prelude. On Linux it checks for
`/run/.toolboxenv`; outside the container it re-execs the same recipe via
`toolbox run -c saffron-build`, forwarding the recipe's positional arguments. On macOS there is no
toolbox: the prelude puts `~/.cargo/bin` and `~/.bun/bin` on `PATH`, and the host rustup toolchain
honors `rust-toolchain.toml`.

That re-exec is why a host-side `FOO=1 just run` silently does nothing on Linux: the variable
stays in the host shell and never crosses into the container. Set variables inside the recipe, or
inside an explicit `toolbox run`:

```sh
toolbox run -c saffron-build bash -lc '
  cd engine
  cargo build --workspace
  cargo run -p xtask -- shaders   # compile shaders + copy runtime assets
  ./target/debug/saffron-host     # the present-only viewport host
'
```

Setting `SAFFRON_NO_TOOLBOX` to any non-empty value skips the re-exec and runs the recipe directly
on the host. This is the escape hatch for a machine that provides `cargo`, `bun`, and the
Vulkan/Slang tooling itself, such as a provisioned CI runner.

## GPU selection

The container's own `/usr` carries only Mesa, so a Vulkan app inside it falls back to
[llvmpipe](https://docs.mesa3d.org/drivers/llvmpipe.html), Mesa's software renderer (its software
Vulkan device enumerates under the same name). The host's NVIDIA driver stays reachable through
the `/run/host` mount. The `gpu_driver` prelude locates `nvidia_icd.x86_64.json` there (or under
`/usr/share/vulkan/icd.d/`) and exports it via
[`VK_ADD_DRIVER_FILES`](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md),
which adds a driver to the Vulkan loader's search without replacing the standard paths.

`just run`, `just run-engine`, `just run-engine-headless`, `just e2e`, and `just capture` all
start with that prelude, so they run on the hardware GPU when the manifest exists and on llvmpipe
otherwise. `just run-software` and `just run-engine-software` omit it to force llvmpipe, which is
correct but slow and fine for validation work. On macOS the same prelude exports
`VK_ICD_FILENAMES` pointing at the ICD of [MoltenVK](https://github.com/KhronosGroup/MoltenVK),
the Vulkan-on-Metal implementation Homebrew installs. It also locates Homebrew's validation-layer
manifest and dynamic library so debug runs execute the same validation-clean gate as Linux.

## Headless and bounded runs

Two environment variables make the host suitable for automation. `SAFFRON_EXIT_AFTER_FRAMES`
bounds a run: `frame_limit_from_env` parses it as a strict `u64` (unset, `0`, or garbage means no
limit) and the main loop exits after that many frames. `SAFFRON_EDITOR_NATIVE_VIEWPORT` switches
the host to `HostMode::Headless`: no window, an offscreen no-surface device, frames published over
shared memory. Together they give a compositor-free smoke run:

```sh
just run-engine-headless 5   # build, compile shaders, render 5 frames offscreen, exit
```

The recipe also wires the GPU prelude and a per-run control socket, so parallel runs do not
collide. `tools/ci/check.sh`, the gate `just check` wraps, boots the host the same bounded way for
its validation-clean smoke step.

## Performance budgets in the gate

`benchmarks/foliage-veg/phase-1-<device>.json` records what one fixed scene costs on one named
device: the scene-gather and frame-time distributions, draw and shadow submission, exact instance
traffic, and retained mesh memory. Each record derives acceptance ceilings from its own
steady-state p95, and the gate grades a fresh measurement against them. A recorded number nothing
reads back is a note, not a budget.

The comparison is per device, never per vendor. `check.ts` re-measures the fixture through the same
`measureBaseline` the recording recipe uses, finds the record whose `platform.gpu` matches this
machine, and fails when a live p95 or counter exceeds that record's ceiling. A machine with no
record of its own defers, as does one that lands on the software rasterizer or serves no GPU
timestamps — so a checkout on unmeasured hardware reports a deferral instead of borrowing a
threshold measured elsewhere. `just bench-foliage-check` runs the step alone.

A ceiling gets no extra allowance at comparison time, because the 25% headroom is already in the
derivation. What a breach must do instead is reproduce: the step measures a second time and fails
only if the same leg goes over twice, which rejects a sample contaminated by other load without
ever moving the threshold. A companion `cargo test` holds the records themselves to that
derivation, so a hand-edited ceiling fails the gate on a machine with no GPU at all.

## Build profiles

A debug build optimizes its dependencies:
[`[profile.dev.package."*"]`](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides)
sets `opt-level = 3` for every non-workspace crate, so glam, ash, and the vendored Jolt run at
full speed while engine crates stay at `opt-level = 0` for fast incremental rebuilds. The release
profile keeps `debug = true` and `panic = "unwind"`, because the FFI seams must unwind cleanly
across the Rust/C++ boundary.

## In the code

| What | File | Symbols |
|---|---|---|
| Recipe preludes + opt-out | `justfile` | `reenter`, `gpu_driver`, `SAFFRON_NO_TOOLBOX` |
| Toolchain pin | `rust-toolchain.toml` | `channel`, `components` |
| MSRV + profile knobs | `engine/Cargo.toml` | `rust-version`, `[profile.dev]`, `[profile.dev.package."*"]`, `[profile.release]` |
| `slangc` resolution + shader step | `engine/xtask/src/shaders.rs` | `Config::resolve`, `find_slangc`, `run` |
| Bounded + headless run | `crates/app/src/lib.rs` | `frame_limit_from_env`, `HostMode` |
| The reproducible gate | `tools/ci/check.sh` | `probe_host`, `pass_step`, `defer_step` |
| Baseline measurement + grading | `tools/bench-foliage-phase1/` | `measureBaseline`, `deriveBudgets`, `requireComparable`, `grade` |
| The records and their derivation test | `benchmarks/foliage-veg/`, `crates/vegetation-gpu/tests/baseline_records.rs` | `phase-1-<device>.json`, `ceiling`, `class_from_file_name` |

## Related

- [Cargo workspace and crate model](../cargo-workspace/) — what `cargo build --workspace` builds
- [Shader compilation](../shader-compilation/) — what `cargo run -p xtask -- shaders` does with the resolved `slangc`
- [Dependencies](../dependencies/) — the pins the toolbox `cargo` resolves
- [Performance telemetry](../../frame-and-render-graph/performance-telemetry/) — the counters and distributions a baseline record samples
