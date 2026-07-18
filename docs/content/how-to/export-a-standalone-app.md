+++
title = 'Export a standalone app'
weight = 11
math = false
+++

# Export a standalone app

Export the loaded project as a Linux x86_64 folder containing `saffron-player`, the saved project,
runtime data, and pre-baked material shaders.

## Prerequisites

- A Linux x86_64 build environment with `just`, the `saffron-build` toolbox, and `jq`.
- A running host that `sa` can reach.
- A loaded project with a primary camera.
- A new or empty destination directory. Export copies into the destination without removing stale files.

The target machine needs a Vulkan-capable driver and the window-system libraries required by the
player. The export bundles `libc++.so.1` and `libc++abi.so.1` beside the binary.

## Steps

1. Build the workspace and engine shaders so `saffron-player` and `shaders/` exist beside the host
   binary:

   ```sh
   just engine
   ```

   Expected output ends with a successful workspace build and shader compilation. Keep the host and
   player in the same Cargo target directory when starting the engine.

2. Save the active project:

   ```sh
   sa -o json save-project
   ```

   Export copies the `project.json` on disk, so this step is required after scene or project changes.

3. Export to the destination directory:

   ```sh
   sa -o json export "$HOME/MyApp" \
     --title "My App" \
     --width 1280 \
     --height 720 \
     > /tmp/saffron-export.json
   ```

   Omitted options use title `Saffron App`, size `1280x720`, windowed mode, and VSync enabled. Add
   `--fullscreen` or `--no-vsync` to write those values to `app.json`.

4. Inspect the export result:

   ```sh
   jq . /tmp/saffron-export.json
   ```

   A complete export reports the destination and an empty warning list:

   ```json
   {
     "path": "/home/user/MyApp",
     "warnings": []
   }
   ```

   Resolve every warning before distributing the folder. Warnings identify failed material shader
   bakes or missing player, shader directory, or C++ runtime libraries.

5. Check the staged files:

   ```sh
   APP_DIR="$HOME/MyApp"
   test -x "$APP_DIR/saffron-player"
   test -f "$APP_DIR/app.json"
   test -f "$APP_DIR/project.json"
   test -f "$APP_DIR/libc++.so.1"
   test -f "$APP_DIR/libc++abi.so.1"
   test -d "$APP_DIR/assets"
   test -d "$APP_DIR/shaders"
   ```

   A project with scripts also contains `src/`.

6. Confirm the runtime manifest:

   ```sh
   jq '{title, width, height, fullscreen, vsync}' "$APP_DIR/app.json"
   ```

   For the command above, expect:

   ```json
   {
     "title": "My App",
     "width": 1280,
     "height": 720,
     "fullscreen": false,
     "vsync": true
   }
   ```

7. Check the binary's dynamic library resolution:

   ```sh
   ldd "$APP_DIR/saffron-player"
   ```

   The output must not contain `not found`. The bundled C++ libraries should resolve from the export
   directory through the player's `$ORIGIN` runtime path.

## Verify

1. Run the exported application without the editor or host:

   ```sh
   "$APP_DIR/saffron-player"
   ```

2. Confirm that the configured window opens and the primary camera renders the scene.

3. Exercise animation, physics, keyboard and mouse input, and Luau behavior used by the project.

4. Close the player and confirm it exits without Vulkan validation or asset-load errors in the log.

The player applies the manifest title, width, and height. A requested fullscreen mode is logged but the
window remains windowed. `vsync: false` is also logged while presentation continues in FIFO mode.

## Export from the editor

1. Save the project.
2. Open the project menu and choose **Export App**.
3. Enter the app title, choose a parent directory, and set the window dimensions and options.
4. Select **Export**.
5. Resolve every `Export warning` notification, then perform the file and runtime checks above.

The editor creates a sanitized child folder below the chosen parent directory. The CLI uses the exact
destination passed to `sa export`.

## What export stages

Material node graphs that require generated shader code are compiled to mesh SPIR-V in the project
assets before copying. Factor-only materials and graphs that lower entirely to material parameters do
not require generated shaders. The player loads the staged SPIR-V and does not invoke `slangc`.

The output includes the full project asset directory, saved `project.json`, optional `src/`, engine
`shaders/`, player binary, C++ runtime libraries, and `app.json`. The exporter does not remove unused
catalog assets. `saffron-player` loads the project beside its executable and advances the shared
`RuntimeSession` for animation, physics, contacts, and scripts.

## In the code

| What | File | Symbols |
|---|---|---|
| Export cook and staging | `control/src/commands_asset.rs` | `export_app`, `find_runtime_lib`, `copy_file`, `copy_dir_recursive` |
| Typed CLI command | `sa/src/main.rs` | `Subcmd::Export`, `export` |
| App manifest and result | `protocol/src/dto.rs` | `AppManifest`, `ExportAppParams`, `ExportAppResult` |
| Standalone startup | `player/src/main.rs` | `main`, `resolve_project_dir`, `load_manifest`, `PlayerLayer` |
| Shared simulation | `runtime/src/session.rs` | `RuntimeSession`, `start`, `advance`, `stop_scripts` |
| Export dialog | `editor/src/app/ExportModal.tsx` | `ExportModal`, `sanitizeFolderName` |

## Related

- [Drive the editor from the CLI](../drive-the-editor-from-the-cli/)
- [Node-graph code generation](../../explanations/materials-and-pipelines/node-graph-codegen/)
- [Script components and the play runtime](../../explanations/scripting/script-components-and-runtime/)
