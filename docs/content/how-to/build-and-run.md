+++
title = 'Build and run'
weight = 1
math = false
+++

# Build and run

Build the Rust engine host and the CEF editor shell, then run both.

The editor is a CEF/React app that drives the engine over the control socket. The engine is the Rust `saffron-host` binary — a headless, present-only viewport host built from the Cargo workspace in `engine/`. `just` is the task runner; its recipes auto-enter the `saffron-build` toolbox when run from a host shell, so the same `just engine` works on the Silverblue host or inside the container. The home directory is shared into the toolbox, which carries the Rust toolchain, the Vulkan SDK, SDL3, Slang, and the host's `bun` on PATH.

## Build the engine host

`just engine` builds the workspace and compiles the shaders next to the host binary. The shell spawns that binary on launch, so build it first.

```sh
just engine
```

That runs `cargo build --workspace` then `cargo run -p xtask -- shaders` inside the toolbox. To use the host toolchain directly instead of the container, set `SAFFRON_NO_TOOLBOX=true`:

```sh
SAFFRON_NO_TOOLBOX=true just engine
```

You can also drive the underlying commands by hand:

```sh
toolbox run -c saffron-build bash -lc '
  cd /var/home/saffronjam/repos/saffron-anima/engine
  cargo build --workspace
  cargo run -p xtask -- shaders'
```

To run the host on its own — useful for a headless check or for driving it from the `sa` CLI without the editor — use `just run-engine`, which loads a default content project so the viewport shows a scene:

```sh
just run-engine
```

## Run the CEF editor shell

`just run` starts the editor, which spawns the `saffron-host` binary as a native child and composites its frames under the webview. Build the host first.

```sh
just run
```

The editor resolves its engine binary from `SAFFRON_ANIMA_BIN`, defaulting to `engine/target/debug/saffron-host`. The dev launch needs a Wayland session because the viewport presents on a `wl_subsurface`; use a real desktop session.

To build and typecheck the frontend on its own, `just editor` runs `bun run build`, which regenerates `editor/src/protocol/` from the [control schemas](../../explanations/tooling-and-control/shared-types/) via `xtask gen-protocol` and then runs `tsc` + `vite build`.

## Verify

- **Engine host alone**: the viewport presents the scene; drive it with the `sa` CLI over its control socket.
- **CEF editor shell**: the shell opens with the Hierarchy / tabbed Inspector·Environment·Stats / Assets / Viewport dock; a "Preparing renderer…" overlay clears once the embedded scene attaches.
- **Headless check**: with no display attached, `just run-engine-headless` boots the host under a private headless `weston`, bounded to a few frames:
  ```sh
  just run-engine-headless 5
  ```
  It sets `SAFFRON_EXIT_AFTER_FRAMES=5` and a per-run `SAFFRON_CONTROL_SOCK`. `SAFFRON_EXIT_AFTER_FRAMES=N` exits after `N` frames; the offscreen image is captured over the control plane via the screenshot command (`capture_viewport`).

> [!NOTE]
> The CEF editor shell is the only editor. Undo/redo, multi-viewport, and native Wayland are non-goals for now.

## In the code

| What | File | Symbols |
|---|---|---|
| Task runner + toolbox auto-enter | `justfile` | `engine`, `run`, `run-engine-headless` |
| Host entry point | `engine/crates/host/src/main.rs` | `main`, `saffron_host::run_host` |
| The loop + frame limit | `engine/crates/app/src/lib.rs` | `run`, `frame_limit_from_env` |
| Shader + protocol build steps | `engine/xtask/src/main.rs` | `run_shaders`, `run_gen_protocol` |
| Viewport screenshot | `engine/crates/rendering/src/renderer.rs` | `Renderer::capture_viewport` |
| Frontend scripts | `editor/package.json` | `dev`, `check`, `gen:protocol` |

## Related

- [Editor shell and the viewport bridge](../../explanations/ui-and-editor/editor-shell-and-viewport-bridge/) — how the editor drives the host
- [Main loop](../../explanations/app-lifecycle-and-window/main-loop-and-run/)
- [Headless runs and capture](../../explanations/app-lifecycle-and-window/headless-and-capture/)
</content>
</invoke>
