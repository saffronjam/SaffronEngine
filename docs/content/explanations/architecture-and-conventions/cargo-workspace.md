+++
title = 'Cargo workspace and crate model'
weight = 2
+++

# Cargo workspace and crate model

The engine is one [Cargo workspace](https://doc.rust-lang.org/cargo/reference/workspaces.html)
rooted at `engine/Cargo.toml`: every crate under `engine/crates/`, plus the `xtask` task runner,
shares a single `Cargo.lock`, one resolver, and one pin list for third-party versions. Each engine
area is its own crate (`saffron-core`, `saffron-rendering`, `saffron-scene`, …), so the crate
boundary is the real boundary between areas.

The workspace is the unit the build operates on. `cargo build --workspace` builds every member and
`cargo test --workspace` tests every member. The dependency edges between members make the
architecture mechanical: a crate can only call into the crates its `Cargo.toml` lists (see
[the crate DAG](../module-dag/)).

## One workspace, one pin list

The root `engine/Cargo.toml` declares the members and pins every external dependency once under
`[workspace.dependencies]`. Member crates pull each dependency with `dep.workspace = true`, so a
version is written in exactly one place and never drifts across crates:

```toml
[workspace]
resolver = "3"
members = ["crates/*", "xtask"]

[workspace.dependencies]
ash = "=0.38"          # Vulkan, pinned exactly
glam = "0.30"          # math
hecs = "0.11"          # the ECS, wrapped by saffron-scene
serde = { version = "1.0", features = ["derive"] }
thiserror = "2"
```

A member adds crate-local detail where it needs to, without restating the version:
`saffron-rendering` writes `rustix = { workspace = true, features = ["shm", "mm", "time"] }` to
layer shared-memory features onto the workspace pin. Sibling engine crates are path dependencies
(`saffron-core = { path = "../core" }`), never version-pinned, because they live in the same tree.

`[workspace.package]` shares `edition = "2024"`, `rust-version`, `version`, and license metadata
across every crate, and `[workspace.lints]` holds the single lint policy (`unsafe_code = "deny"`,
`clippy::all = "warn"`) that each member inherits with `[lints] workspace = true`. The profiles
are workspace-wide too: `[profile.dev.package."*"]` compiles third-party dependencies at
`opt-level = 3` even in a debug build, so glam, Jolt, and ash run at speed while engine code keeps
its debug info.

## Libraries and executables

Most members are library crates. Four members build executables:

| Member | Binary | Role |
|---|---|---|
| `saffron-host` | `saffron-host` | The present-only viewport host (also a library, so tests can link it) |
| `saffron-player` | `saffron-player` | The exported game |
| `sa` | `sa` | The control-plane CLI |
| `xtask` | `xtask` | Build tasks: `shaders`, `gen-protocol` |

Everything else exports a library API that other members consume through `use`.

## A crate's public surface

Each library crate has a `lib.rs` root that names its internal modules and re-exports the types
that form its public API. `saffron-core`, the root of the crate DAG, is the smallest complete
example:

```rust
// crates/core/src/lib.rs
mod base64;
mod blend;
mod error;
mod height;
mod time;
mod uuid;

pub use base64::base64_encode;
pub use blend::BlendMode;
pub use error::{Error, Result};
pub use height::HeightMode;
pub use time::TimeSpan;
pub use uuid::Uuid;

pub type Ref<T> = Arc<T>;
```

Consumers write `use saffron_core::{Result, Ref};` and see exactly what `lib.rs` re-exports,
nothing else. A type that is `pub` inside a private `mod` stays unreachable until `lib.rs`
re-exports it, which is how a crate keeps its internal files private while presenting one curated
surface.

## Crate vs module

A **crate** is the architectural unit: it has a `Cargo.toml`, a dependency list, and a compilation
boundary. A **module** (`mod foo;` → `foo.rs`) is an organizational unit inside one crate — see
[how a crate organizes its modules](../module-partitions/). The dependency DAG that holds the
engine together is a graph of crates, not modules.

| Concept | Spelled | Boundary |
|---|---|---|
| Crate | `engine/crates/<area>/` with a `Cargo.toml` | What a crate may depend on |
| Module | `mod name;` inside a crate | File-level organization within a crate |
| Re-export | `pub use` in `lib.rs` | The crate's public API |

## In the code

| What | File | Symbols |
|---|---|---|
| Workspace + pin list | `engine/Cargo.toml` | `[workspace]`, `members`, `[workspace.dependencies]` |
| Shared package metadata + lints + profiles | `engine/Cargo.toml` | `[workspace.package]`, `[workspace.lints]`, `[profile.dev.package."*"]` |
| A crate manifest pulling pins | `crates/rendering/Cargo.toml` | `ash.workspace = true`, `saffron-core = { path = ... }` |
| A crate's public surface | `crates/core/src/lib.rs` | `pub use`, `pub type Ref<T>` |
| A lib + bin member | `crates/host/Cargo.toml` | `[lib]`, `[[bin]]` |

## Related
- [How a crate organizes its modules](../module-partitions/) — files inside one crate
- [The crate DAG](../module-dag/) — how the crates depend on each other
- [Build environment](../build-environment/) — the toolbox that runs `cargo`
- [Dependencies](../dependencies/) — the pins under `[workspace.dependencies]`
