+++
title = 'Architecture & conventions'
weight = 19
bookCollapseSection = true
+++

# Architecture & conventions

The engine's architecture and conventions are the structure and rules that hold the codebase
together. They cover the Cargo workspace and crate DAG, the toolbox build, the shader pipeline,
and the Rust house style the whole codebase follows.

## Pages

| Page | Covers | Code |
|---|---|---|
| `rust-house-style` | plain structs + methods, traits as itables, errors as values, `clippy -D warnings` | `crates/core/`, `crates/app/` |
| `cargo-workspace` | the Cargo workspace, crates vs modules, the single pin list, `pub use` surfaces | `engine/Cargo.toml`; `crates/core/src/lib.rs` |
| `module-partitions` | how one crate splits into module files behind a curated `pub use` root | `crates/rendering/src/lib.rs` |
| `module-dag` | the crate dependency DAG, why `saffron-host` sits on top | `engine/Cargo.toml`; `crates/host/` |
| `build-environment` | the `saffron-build` toolbox, `just` auto-enter, `SAFFRON_NO_TOOLBOX`, the gate's recorded performance budgets, the repo-wide TypeScript workspace and style arrangement | `justfile`; `tools/ci/check.sh`; `package.json`, `.oxlintrc.json`, `tsconfig.json` |
| `shader-compilation` | Slang → SPIR-V via `cargo run -p xtask -- shaders` | `engine/xtask/src/shaders.rs`; `engine/assets/shaders/` |
| `dependencies` | Cargo deps pinned once in `[workspace.dependencies]`, FFI/unsafe seams | `engine/Cargo.toml` |
