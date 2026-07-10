# packager

Builds the editor into a per-OS distributable. A single Bun + TypeScript entry point (`index.ts`) with
a [@clack/prompts](https://github.com/bombshell-dev/clack) UI, invoked by the `just package` recipe.

## Usage

```sh
just package linux     # build an AppImage → build/dist/
just package           # interactive: pick a target
```

| Target | Format | Status |
|--------|--------|--------|
| `linux` | AppImage (single self-contained file) | built by `targets/linux.ts` |
| `windows` | installer `.exe` / `.msi` | not yet implemented |
| `macos` | `.app` in a notarized `.dmg` (needs helper apps + MoltenVK) | not yet implemented |

## Layout

```
index.ts            arg parsing (cac) + target dispatch + clack intro/outro
targets/linux.ts    the AppImage pipeline: build → CEF gate → stage AppDir → appimagetool
lib/cef.ts          CEF runtime integrity gate (heal on a truncated cef-dll-sys extraction)
lib/paths.ts        repo paths
lib/ui.ts           the clack per-step spinner helper
assets/linux/       AppRun, .desktop, icon copied into the AppDir
```

The Linux AppDir colocates `saffron-editor-shell`, `saffron-host`, and the CEF runtime (`libcef.so` +
resource packs, which CEF resolves beside the shell binary) in `usr/bin`; engine data (models, fonts,
icons, compiled shaders) in `usr/share/saffron-anima`; and the built React UI in
`usr/share/saffron-anima/ui`. `AppRun` sets `SAFFRON_ANIMA_BIN`, `SAFFRON_ASSET_DIR`,
`SAFFRON_SHADER_DIR`, and `SAFFRON_UI_DIR` to those paths and lets the system Vulkan loader find the
GPU driver. The shell serves the bundled UI over its `saffron-app://` scheme (there is no dev server in
a package); `SAFFRON_UI_DIR` tells it where the `ui/` dir is.

New OS targets are added as `targets/<os>.ts`.
