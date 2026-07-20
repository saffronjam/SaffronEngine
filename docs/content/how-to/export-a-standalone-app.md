+++
title = 'Export a standalone app'
weight = 11
math = false
+++

# Export a standalone app

Export the loaded project as a platform-native application: a flat `saffron-player` directory on
Linux or a `.app` bundle on macOS. The result contains the saved project, runtime data, pre-baked
material shaders, and the platform runtime libraries the player needs.

## Prerequisites

- A working build environment for the current platform. Follow [Build and run](../build-and-run/)
  first.
- A running host that `sa` can reach.
- A loaded project with a primary camera.
- `jq` for inspecting the command result and app manifest.
- A new or empty destination. Export copies files into the destination without removing stale files.

Linux exports target x86_64 and bundle `libc++.so.1` and `libc++abi.so.1`. The target machine still
needs a Vulkan-capable driver and the window-system libraries used by the player.

macOS exports target the build machine's architecture. The build environment must provide
[MoltenVK](https://github.com/KhronosGroup/MoltenVK/blob/main/Docs/MoltenVK_Runtime_UserGuide.md).
The exporter copies its dynamic library into `Contents/Frameworks`, includes the license, and ad-hoc
signs the bundle. The player loads that library directly, without a Homebrew Vulkan loader or ICD
manifest at runtime.

## Steps

1. Build the workspace and engine shaders:

   ```sh
   just engine
   ```

   Expected output ends with a successful workspace build and shader compilation. Keep the host and
   player in the same Cargo target directory when starting the engine.

2. Save the active project:

   ```sh
   sa -o json save-project
   ```

   Export copies the `project.json` on disk, so save after changing the scene or project settings.

3. Export to the destination:

   ```sh
   sa -o json export "$HOME/MyApp" \
     --title "My App" \
     --width 1280 \
     --height 720 \
     > /tmp/saffron-export.json
   ```

   Omitted options use title `Saffron App`, size `1280x720`, windowed mode, and VSync enabled. Add
   `--fullscreen` or `--no-vsync` to write those values to `app.json`. On macOS, export appends
   `.app` when the destination does not already have that extension.

4. Inspect the result:

   ```sh
   jq . /tmp/saffron-export.json
   ```

   A complete Linux export reports a flat directory:

   ```json
   {
     "path": "/home/user/MyApp",
     "warnings": []
   }
   ```

   A complete macOS export reports the application bundle:

   ```json
   {
     "path": "/Users/user/MyApp.app",
     "warnings": []
   }
   ```

   Resolve every warning before distributing the app. Warnings identify failed material shader
   bakes or missing runtime files.

5. Check the staged files for the current platform.

   On Linux:

   ```sh
   APP_ROOT="$HOME/MyApp"
   APP_RESOURCES="$APP_ROOT"
   test -x "$APP_ROOT/saffron-player"
   test -f "$APP_ROOT/libc++.so.1"
   test -f "$APP_ROOT/libc++abi.so.1"
   test -d "$APP_RESOURCES/assets"
   test -d "$APP_RESOURCES/shaders"
   ldd "$APP_ROOT/saffron-player"
   ```

   The `ldd` output must not contain `not found`. The bundled C++ libraries resolve from the export
   directory through the player's `$ORIGIN` runtime path.

   On macOS:

   ```sh
   APP_ROOT="$HOME/MyApp.app"
   APP_RESOURCES="$APP_ROOT/Contents/Resources"
   test -x "$APP_ROOT/Contents/MacOS/saffron-player"
   test -f "$APP_ROOT/Contents/Frameworks/libMoltenVK.dylib"
   test -f "$APP_RESOURCES/licenses/MoltenVK-LICENSE.txt"
   test -d "$APP_RESOURCES/assets"
   test -d "$APP_RESOURCES/shaders"
   plutil -lint "$APP_ROOT/Contents/Info.plist"
   codesign --verify --deep --strict --verbose=2 "$APP_ROOT"
   otool -L "$APP_ROOT/Contents/Frameworks/libMoltenVK.dylib"
   ```

   `plutil` and `codesign` must succeed. The MoltenVK dependency list must not contain a missing or
   build-tree-only library path.

   A project with scripts also contains `$APP_RESOURCES/src`.

6. Confirm the runtime manifest:

   ```sh
   test -f "$APP_RESOURCES/app.json"
   test -f "$APP_RESOURCES/project.json"
   jq '{title, width, height, fullscreen, vsync}' "$APP_RESOURCES/app.json"
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

## Verify

1. Run the exported application without the editor, host, or Vulkan environment overrides.

   On Linux:

   ```sh
   "$APP_ROOT/saffron-player"
   ```

   On macOS:

   ```sh
   "$APP_ROOT/Contents/MacOS/saffron-player"
   ```

   You can also launch the macOS bundle with `open "$APP_ROOT"`.

2. Confirm that the configured window opens and the primary camera renders the scene.

3. Exercise the animation, physics, keyboard and mouse input, and Luau behavior used by the project.

4. Close the player and confirm it exits without Vulkan validation or asset-load errors in the log.

The player applies the manifest title, width, and height. The standalone window backend opens a
window and uses FIFO presentation regardless of the `fullscreen` and `vsync` values.

## Export from the editor

1. Save the project.
2. Open the project menu and choose **Export App**.
3. Enter the app title, choose a parent directory, and set the window dimensions and options.
4. Select **Export**.
5. Resolve every `Export warning` notification, then perform the platform checks above.

The editor creates a sanitized child name below the chosen parent: a directory on Linux or a `.app`
bundle on macOS. The CLI uses the exact destination passed to `sa export`, adding `.app` on macOS
when needed.

## What export stages

Material node graphs that require generated shader code are compiled to mesh SPIR-V in the project
assets before copying. Factor-only materials and graphs that lower entirely to material parameters do
not require generated shaders. The player loads the staged SPIR-V and does not invoke `slangc`.

Both layouts contain `project.json`, `app.json`, `assets/`, optional `src/`, and engine `shaders/`.
Linux keeps these beside `saffron-player` and the two C++ runtime libraries. macOS places data in
`Contents/Resources`, the player in `Contents/MacOS`, and MoltenVK in `Contents/Frameworks`. The
exporter does not remove unused catalog assets.

`saffron-player` resolves the staged project from its platform layout and advances the shared
`RuntimeSession` for animation, physics, contacts, and scripts.

## In the code

| What | File | Symbols |
|---|---|---|
| Export cook and staging | `control/src/commands_asset.rs` | `export_app`, `ExportLayout`, `stage_platform_runtime` |
| Typed CLI command | `sa/src/main.rs` | `Subcmd::Export`, `export` |
| App manifest and result | `protocol/src/dto.rs` | `AppManifest`, `ExportAppParams`, `ExportAppResult` |
| Standalone startup | `player/src/main.rs` | `main`, `resolve_project_dir`, `platform_project_dir`, `load_manifest` |
| Bundled Vulkan loading | `rendering/src/device.rs` | `load_entry` |
| Shared simulation | `runtime/src/session.rs` | `RuntimeSession`, `start`, `advance`, `stop_scripts` |
| Export dialog | `editor/src/app/ExportModal.tsx` | `ExportModal`, `sanitizeFolderName` |

## Related

- [Drive the editor from the CLI](../drive-the-editor-from-the-cli/)
- [Node-graph code generation](../../explanations/materials-and-pipelines/node-graph-codegen/)
- [Script components and the play runtime](../../explanations/scripting/script-components-and-runtime/)
