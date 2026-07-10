# packaging

Per-OS distributables for the editor, driven by `just package <target>`.

| Target | Format | Status |
|--------|--------|--------|
| `linux` | AppImage (single self-contained file) | built by `just package linux` → `build/dist/` |
| `windows` | installer `.exe` / `.msi` | not yet implemented |
| `macos` | `.app` in a notarized `.dmg` (needs helper apps + MoltenVK) | not yet implemented |

## Linux AppImage

`just package linux` builds the release artifacts (host, shell, shaders, frontend), stages an AppDir
under `build/appimage/`, and runs `appimagetool` (fetched into `build/tools/` if not on `PATH`) to
emit `build/dist/Saffron_Anima-x86_64.AppImage`.

The AppDir colocates `saffron-editor-shell`, `saffron-host`, and the CEF runtime (`libcef.so` + the
resource packs, which CEF resolves beside the shell binary) in `usr/bin`; engine data (models, fonts,
icons, compiled shaders) in `usr/share/saffron-anima`; and the built React UI in
`usr/share/saffron-anima/ui`. `AppRun` sets `SAFFRON_ANIMA_BIN`, `SAFFRON_ASSET_DIR`, and
`SAFFRON_SHADER_DIR` to those paths and lets the system Vulkan loader find the GPU driver.

**Open gap:** loading the bundled UI needs the `app://` prod scheme, which is not wired yet, so the
AppImage launches (shell + host + viewport) but shows a placeholder until that lands. See `AppRun` for
the `SAFFRON_DEV_URL` test hook.
