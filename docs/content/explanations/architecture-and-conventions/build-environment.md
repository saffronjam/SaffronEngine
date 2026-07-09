+++
title = 'Build environment'
weight = 5
+++

# Build environment

The build environment is a single container that holds the entire Rust toolchain plus the Vulkan
SDK and the Slang shader compiler. The host runs no compiler, so building, testing, and running all
happen inside that container — and the `just` recipes auto-enter it, so the same command works from
a host shell or from inside the container.

A toolbox is a Fedora development container with the home directory shared host-side. It isolates
the toolchain from an immutable host while leaving project files editable from either side.

## The toolbox

The dev machine is Fedora **Silverblue**, ostree-booted, with home under `/var/home`. It ships no
Rust toolchain or Vulkan SDK on the host. Everything builds inside the **`saffron-build`**
container. The home directory is shared host-to-toolbox, so files edited on the host are visible
inside immediately.

The container carries `cargo` + `rustc` (the workspace pins `rust-version = "1.85"`, `edition =
"2024"`), the Vulkan headers / loader / validation layers / tools, and the prebuilt `slangc` under
`~/.cache/saffron-slang/`. The GPU inside the toolbox is **llvmpipe**, Mesa's software Vulkan,
sufficient for correctness and validation; hardware acceleration needs the NVIDIA ICD (the `just
run` recipes wire it in) or `mesa-vulkan-drivers` installed in the container.

## Driving the build with `just`

The `justfile` at the repo root drives the flow through `cargo`, the `xtask` helper, and `bun`.
Toolbox-bound recipes **auto-enter** `saffron-build` when run from the host, so a plain `just lint`
behaves identically from a host shell or inside the container:

```sh
just engine    # cargo build --workspace, then cargo run -p xtask -- shaders
just test      # cargo test --workspace
just lint      # cargo fmt --check + cargo clippy --workspace -- -D warnings + editor oxlint
just run       # build the host, compile shaders, start the CEF editor shell
just check     # the full reproducible gate
```

Under the hood, each toolbox recipe re-execs the same recipe inside the container when it is not
already there (it checks `/run/.toolboxenv`). Because of that boundary, a host-side `ENV=… just …`
would no-op — the variable never crosses into the container. Set the variable inside the recipe, or
use a recipe argument.

The engine build is a workspace `cargo build`; running it directly is just as valid:

```sh
toolbox run -c saffron-build bash -lc '
  cd engine
  cargo build --workspace
  cargo run -p xtask -- shaders        # compile shaders + copy runtime assets
  ./target/debug/saffron-host          # the present-only viewport host
'
```

## Opting out of the toolbox

Set `SAFFRON_NO_TOOLBOX=true` to skip the auto-enter and run a recipe directly on the host. This
trusts the host to provide `cargo`, `bun`, and the Vulkan/Slang tooling itself:

```sh
SAFFRON_NO_TOOLBOX=true just test
```

It is the escape hatch for a host that already has the toolchain (CI on a provisioned runner, or a
non-Silverblue dev box). On the standard Silverblue host it will fail at the first missing tool —
which is the point: the toolbox is the default for a reason.

## Headless and bounded runs

For headless or automated verification, bound the run so it exits on its own. The host honours
`SAFFRON_EXIT_AFTER_FRAMES`:

```sh
SAFFRON_EXIT_AFTER_FRAMES=5 ./target/debug/saffron-host
```

`just run-engine-headless 5` wraps this with the native-viewport driver set, so no window or
compositor is needed.

> [!NOTE]
> The `cargo build` profile optimizes *dependencies* even in a debug engine build
> (`[profile.dev.package."*"] opt-level = 3` in `engine/Cargo.toml`), so glam, ash inlining, and
> the vendored Jolt run at speed while engine crates stay at `opt-level = 0` for fast incremental
> rebuilds.

## In the code

| What | File | Symbols |
|---|---|---|
| Recipes + toolbox auto-enter | `justfile` | `engine`, `test`, `lint`, `run`, `check`, the re-enter prelude |
| Toolbox opt-out | `justfile` | `SAFFRON_NO_TOOLBOX` |
| Toolchain + profile knobs | `engine/Cargo.toml` | `[workspace.package]`, `[profile.dev]`, `[profile.dev.package."*"]` |
| Shader + asset step | `engine/xtask/src/shaders.rs` | `Config::resolve`, `run` |

## Related
- [The Cargo workspace and crate model](../cargo-workspace/) — what `cargo build --workspace` builds
- [Shader compilation](../shader-compilation/) — where `slangc` runs (the `xtask` step)
- [Dependencies](../dependencies/) — the pins the toolbox `cargo` resolves
