+++
title = 'Build and run'
weight = 1
math = false
+++

# Build and run

Build the Rust workspace and shaders, run a bounded host smoke test, then launch the CEF editor.

## Prerequisites

- Run every command from the repository root.
- Install `just` and Bun.
- On Linux, create the `saffron-build` toolbox. The recipes enter it automatically and use its Rust, Vulkan, SDL3, and Slang tools.
- On macOS, install the Rust 1.96 toolchain selected by `rust-toolchain.toml` and a Vulkan loader with MoltenVK.
- Use a Wayland desktop session on Linux when launching the editor.

On macOS, provision the pinned CEF 149.0.6 distribution once:

```sh
cargo install export-cef-dir
export-cef-dir --version 149.0.6 "$HOME/.local/share/cef/149.0.6"
```

The second command creates `$HOME/.local/share/cef/149.0.6/Chromium Embedded Framework.framework`.

## Steps

1. Confirm that the repository recipes are available:

   ```sh
   just help
   # Available recipes:
   #     engine
   #     editor
   #     run
   #     run-engine-headless frames="5"
   ```

2. Build the Cargo workspace and compile the Slang shaders:

   ```sh
   just engine
   # Finished `dev` profile ...
   # xtask shaders: <compiled> compiled, <cached> up to date, ... -> .../target/debug/shaders
   ```

   Check the runtime artifacts:

   ```sh
   test -x engine/target/debug/saffron-host \
     && test -x engine/target/debug/sa \
     && test -s engine/target/debug/shaders/triangle.spv \
     && echo 'engine artifacts OK'
   # engine artifacts OK
   ```

3. Run the host for five offscreen frames. This smoke test uses a no-surface renderer and exits on its own:

   ```sh
   just run-engine-headless 5 && echo 'headless host OK'
   # ... vulkan ready - gpu '<device>' (<type>)
   # headless host OK
   ```

4. Generate the editor protocol types, typecheck TypeScript, and build the Vite frontend:

   ```sh
   just editor
   # ... built in <time>
   ```

   A successful command leaves `editor/dist/index.html`:

   ```sh
   test -s editor/dist/index.html && echo 'editor frontend OK'
   # editor frontend OK
   ```

5. Launch the complete editor:

   ```sh
   just run
   ```

   The recipe rebuilds `saffron-host` and its shaders, builds the CEF shell, starts Vite on `127.0.0.1:1420`, and launches the native shell. The shell then starts the host as its child.

6. Keep `just run` open and verify the control socket from a second terminal:

   ```sh
   just sa ping
   # pong  engine=SaffronAnima  version=<version>  pid=<pid>
   ```

## Verify

The editor window should show the **Hierarchy**, **Inspector**, **Assets**, and **Viewport** panels. The **Preparing renderer...** overlay disappears after the host attaches, and `just sa ping` returns exit status 0.

Run the full project gate when the smoke checks pass:

```sh
just check
# ...
# ALL GATES PASSED
```

`just check` runs the workspace build, shader pipeline, tests, present-only smoke, schema contract, project checks, end-to-end suite, frontend build, and lint gate.

## Related

- [Build environment](../../explanations/architecture-and-conventions/build-environment/) — toolbox entry, task recipes, and the project gate
- [Shader compilation](../../explanations/architecture-and-conventions/shader-compilation/) — Slang inputs and runtime SPIR-V outputs
- [Editor shell and viewport bridge](../../explanations/ui-and-editor/editor-shell-and-viewport-bridge/) — the shell, host child, and CEF composition
- [Headless runs and capture](../../explanations/app-lifecycle-and-window/headless-and-capture/) — offscreen host mode and bounded frames
