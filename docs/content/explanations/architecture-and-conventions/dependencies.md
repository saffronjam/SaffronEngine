+++
title = 'Dependencies'
weight = 7
+++

# Dependencies

A third-party dependency is a crate the engine consumes but does not author. Cargo builds every
one from source against a single `Cargo.lock`, and the versions live in one list:
`[workspace.dependencies]` in `engine/Cargo.toml`. Member crates inherit each pin through
[Cargo's dependency inheritance](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-dependencies-table),
so a version is written once and never drifts between the crates that use it.

## One pinned set

The workspace manifest declares each external crate with its version and default feature set. A
member manifest pulls a dependency with `dep.workspace = true` and never restates the number:

```toml
# engine/Cargo.toml
[workspace.dependencies]
ash = "=0.38"          # Vulkan bindings, pinned exactly
vk-mem = "0.4"         # VMA allocator
winit = "0.30"         # windowing
hecs = "0.11"          # the ECS, wrapped by saffron-scene
glam = "0.30"          # math
mlua = { version = "0.11", features = ["luau", "vendored"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = { version = "1.0", features = ["preserve_order"] }
thiserror = "2"
```

```toml
# crates/rendering/Cargo.toml
[dependencies]
ash.workspace = true
rustix = { workspace = true, features = ["shm", "mm", "time"] }
```

Features layer per crate on top of the shared version. `rustix` is one pin, but
`saffron-rendering` adds `shm`/`mm` for the shared-memory frame transport, `saffron-control` adds
`net`/`event`/`fs` for the control socket, and `saffron-host` adds `process` for its parent-death
watch. `saffron-geometry` enables glam's `bytemuck` feature the same way, so math types cast
zero-copy into GPU struct layout; glam's column-major matrices pair with the
`-matrix-layout-column-major` flag in [shader compilation](../shader-compilation/), and a CPU
transform reaches the shader unchanged.

Several pins encode a decision, not just a number. `ash = "=0.38"` is pinned exactly to hold the Vulkan
binding surface still. `serde_json`'s `preserve_order` keeps map keys in insertion order, so
control results emit keys in DTO field order and the generated protocol artifacts reproduce
byte-for-byte. `serde_with`'s `schemars_1` feature carries its adapters' schema impls into the
draft 2020-12 schemas the control DTOs declare, and `mlua`'s `vendored` feature builds the Luau VM
from bundled sources instead of a system library.

One version sits outside the list: `cc = "1"`, a `[build-dependencies]` entry local to
`saffron-physics-sys`, whose build script is the only consumer.

## The major dependencies

| Area | Crate(s) | Role |
|---|---|---|
| Vulkan | `ash`, `ash-window`, `raw-window-handle`, `vk-mem` | the `vk::` API surface, surface creation, VMA allocation |
| Windowing | `winit` | the OS window + event loop |
| ECS | `hecs` | the world behind `saffron-scene` |
| Math | `glam` | vectors/matrices, `bytemuck`-castable to GPU layout |
| Scripting | `mlua` | the vendored Luau VM behind `saffron-script` |
| Serde stack | `serde`, `serde_json`, `serde_with`, `schemars`, `ts-rs` | JSON, the wire DTOs, schema + TypeScript codegen |
| Physics FFI | `cxx`, `cxx-build`, `cc` | the vendored Jolt 5.3.0 bridge in `saffron-physics-sys` |
| Import / images | `gltf`, `tobj`, `image`, `resvg`/`usvg`/`tiny-skia` | glTF/OBJ import, texture decode, SVG icon raster |
| Syscalls / CLI | `rustix`, `clap`, `clap_complete`, `walkdir` | shm/socket/process syscalls, the `sa` CLI + shell completions, the asset-catalog scan |
| GPU casts / blobs | `bytemuck`, `base64` | struct→bytes for upload, control-protocol blobs |
| Logging | `tracing`, `tracing-subscriber`, `time`, `nu-ansi-term` | the event/span API; `saffron-log`'s subscriber, timestamps, ANSI palette |
| Errors | `thiserror`, `anyhow` | typed library errors; `anyhow` in tooling |
| Benchmarks | `criterion` | dev-only harness for the `saffron-scene` ECS bench |

## Wrapped choices

The big subsystem picks hide behind one engine crate each, so swapping one is a single-crate
decision plus a one-line pin change. [hecs](https://github.com/Ralith/hecs) is named only inside
`saffron-scene`, which wraps it behind `Scene` and `Entity`; no other crate spells `hecs::`.
[Luau](https://luau.org/) lives behind `saffron-script`, and
[Jolt](https://github.com/jrouwe/JoltPhysics) behind `saffron-physics-sys` plus the safe
`saffron-physics` API above it.

[ash](https://github.com/ash-rs/ash) and
[VMA](https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator) (via `vk-mem`) stay
inside `saffron-rendering` the same way: consumers see `Device`, `Buffer`, and `Image` wrappers,
not raw `vk::` handles.

## FFI and unsafe

`unsafe_code = "deny"` holds workspace-wide through `[workspace.lints]`. Three crates opt back in
with a crate-root `#![allow(unsafe_code)]`, each at a foreign boundary a third-party dependency
crosses:

- `saffron-rendering` calls `ash`'s raw Vulkan entry points.
- `saffron-physics-sys` bridges vendored Jolt 5.3.0 C++ through a
  [`#[cxx::bridge]`](https://cxx.rs/); its `build.rs` fetches the checksum-verified release
  tarball (`ensure_vendored_jolt`) and compiles it with `cxx-build` + `cc`.
- `saffron-host` owns the shared-memory frame publisher's raw mapping writes.

Each keeps the `unsafe` confined to its seam and exposes a safe API; the lint gate and the
`// SAFETY:` comment rule are covered in [Rust house style](../rust-house-style/).

## In the code

| What | File | Symbols |
|---|---|---|
| The pin list | `engine/Cargo.toml` | `[workspace.dependencies]`, `[workspace.lints]` |
| A crate pulling pins + features | `crates/rendering/Cargo.toml` | `ash.workspace = true`, `rustix = { workspace = true, features = [...] }` |
| The ECS wrap | `crates/scene/src/scene.rs` | `Scene`, `Entity` |
| The Jolt FFI sys crate | `crates/physics-sys/Cargo.toml`, `build.rs` | `cxx.workspace = true`, `[build-dependencies]`, `ensure_vendored_jolt` |
| The unsafe opt-ins | `crates/{rendering,physics-sys,host}/src/lib.rs` | `#![allow(unsafe_code)]` |

## Related

- [Cargo workspace and crate model](../cargo-workspace/) — the workspace the pins live in
- [Build environment](../build-environment/) — the toolbox `cargo` that resolves them
- [Rust house style](../rust-house-style/) — the lint gate behind the unsafe seams
- [Shader compilation](../shader-compilation/) — the `slangc` half of the toolchain
