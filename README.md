<h1 align="center">Saffron Anima</h1>

<p align="center">
  A from-scratch Vulkan renderer and Rust game engine, driven by a CEF/React editor.
</p>

<p align="center">
  <a href="https://github.com/saffronjam/saffron-anima/actions/workflows/ci.yml"><img src="https://github.com/saffronjam/saffron-anima/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <img src="https://img.shields.io/badge/Rust-2024-CE412B.svg?logo=rust&logoColor=white" alt="Rust 2024" />
  <img src="https://img.shields.io/badge/Vulkan-1.4-A41E22.svg?logo=vulkan&logoColor=white" alt="Vulkan 1.4" />
  <img src="https://img.shields.io/badge/CEF-149-4285F4.svg?logo=googlechrome&logoColor=white" alt="CEF 149" />
</p>

---

A Cargo workspace (`engine/`) that builds its own present-only viewport host (`saffron-host`). The editor is a separate CEF/React app — a Rust shell (`editor/shell`) that renders the React UI through CEF windowless OSR, presents the host's offscreen frames on Wayland subsurfaces below its transparent UI, and drives it over a JSON-over-socket control plane.

## Features

- Vulkan 1.4 via `ash`, dynamic rendering + sync2, vk-mem.
- A render graph: passes declare resource usage, the graph derives every barrier.
- Clustered-forward PBR, IBL, shadows (incl. contact + ray-traced).
- DDGI, voxel GI, SSGI, ReSTIR; GTAO, TAA, motion vectors, tonemap; MSAA + FXAA.
- Bindless textures + instanced draws; an übershader with a keyed PSO cache.
- hecs ECS, registry-driven JSON scene/project format, glTF + OBJ import.
- A unix-socket control plane and `sa` CLI that script the running editor.

## Build & run

`just` is the task runner. Recipes auto-enter the `saffron-build` toolbox when run from the host (set `SAFFRON_NO_TOOLBOX=true` to use the host toolchain instead); the toolbox provides Rust/cargo, the Vulkan SDK, SDL3, and Slang. See [`AGENTS.md`](AGENTS.md) for the full toolchain notes.

```sh
just engine   # cargo build --workspace + compile shaders
just run      # start the editor, which spawns the host
```

Under the hood the engine builds with `cargo build --workspace` (from `engine/`); shaders compile via `cargo run -p xtask -- shaders`.

Other convenience recipes (`just --list` for the full set): `just run-engine` boots only the present-only host, `just test` runs the workspace tests, `just e2e` drives a headless engine over the control plane, `just run-docs` serves the docs site, and `just format` / `just lint` cover style (`cargo fmt` + `cargo clippy -- -D warnings` for Rust, oxfmt + oxlint for the editor TypeScript). `just check` runs the reproducible gate (engine build + shaders → headless smoke → control-schema contract → frontend bun build).

## More

- Concept-by-concept docs: [`docs/content/overview.md`](docs/content/overview.md) (Hugo site).
- Toolchain, architecture, conventions, code style: [`AGENTS.md`](AGENTS.md).
</content>
</invoke>
