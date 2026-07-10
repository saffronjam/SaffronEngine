# Saffron Anima — engine (Rust)

The Rust engine: a Cargo workspace ([`Cargo.toml`](Cargo.toml), edition 2024) whose member
crates live under [`crates/`](crates/), with the [`xtask`](xtask/) helper for build tasks. The
workspace builds **`saffron-host`** — the present-only viewport host that renders the scene plus a
native gizmo overlay offscreen, publishes frames into shared memory, and serves the JSON-over-unix-socket
control plane. The editor (the CEF/React app in [`../editor/`](../editor/)) spawns it, presenting
its frames on Wayland subsurfaces below its transparent UI, and points `SAFFRON_ANIMA_BIN` at
this binary.

## Crates

The workspace is a DAG of `saffron-*` crates (leaves first), wrapped around the `saffron-host`
binary:

- `saffron-core`, `saffron-signal`, `saffron-json`, `saffron-geometry` — foundations.
- `saffron-window` — winit window + typed event signals.
- `saffron-scene` — hecs ECS + JSON project format (the ECS is wrapped, never named outside this crate).
- `saffron-animation` — pose/clip types, samplers, the animation-player runtime.
- `saffron-physics` + `saffron-physics-sys` — the Jolt wrapper (vendored Jolt via `cxx`).
- `saffron-script` — per-entity Luau scripting (`mlua`).
- `saffron-rendering` — the Vulkan renderer (`ash` + `vk-mem`) and the render graph.
- `saffron-assets` — glTF/OBJ import, asset catalog, materials.
- `saffron-sceneedit` — editor-side scene operations.
- `saffron-control`, `saffron-control-client`, `saffron-protocol` — the control plane, its client,
  and the wire-contract types fed to `@saffron/protocol`.
- `saffron-app` — the `App`/`Layer` lifecycle and the deferred `submit` render seam.
- `saffron-host` — the `saffron-host` binary that wires the above together.
- `sa` — the `sa` control CLI (JSON over the unix socket).
- `saffron-e2e`, `saffron-test-support` — test harness support.
- `xtask` — build tasks: `cargo run -p xtask -- shaders` compiles the Slang shaders to SPIR-V,
  `xtask gen-protocol` regenerates `@saffron/protocol` from the control DTOs.

Third-party versions are pinned once in `[workspace.dependencies]`; member crates pull each via
`dep.workspace = true`.

## Build

Everything runs in the `saffron-build` toolbox (Rust/cargo + Vulkan SDK + SDL3 + Slang); `just`
recipes auto-enter it from the host, and `SAFFRON_NO_TOOLBOX=true` skips that to use the host
toolchain.

```sh
cargo build --workspace            # build the whole workspace
cargo run -p xtask -- shaders      # compile shaders to SPIR-V
cargo test --workspace             # tests
cargo fmt && cargo clippy -- -D warnings
./target/debug/saffron-host        # the present-only viewport host
```

The editor end-to-end suite is `bun test` in [`../tests/e2e/`](../tests/e2e/). The reproducible gate
is [`../tools/ci/check.sh`](../tools/ci/check.sh): engine build + shaders → headless smoke →
control-schema contract → frontend `bun run build`.
