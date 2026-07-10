# packager — Bun + TypeScript distributable builder

Builds the editor into a per-OS distributable. A single entry point (`index.ts`) parses args with
[cac](https://github.com/cacjs/cac), dispatches to a target, and renders a
[@clack/prompts](https://github.com/bombshell-dev/clack) console UI (framed intro/outro, a spinner per
step, a boxed result). The `just package` recipe is a thin caller — all logic lives here, in TypeScript.

It runs **under Bun inside the `saffron-build` toolbox** (Bun, cargo, curl, appimagetool all live there).
User-facing usage is in `README.md`; this file is the agent contract.

## Layout

```
index.ts          arg parsing (cac) + target dispatch + clack intro/outro/error handling
targets/linux.ts  the AppImage pipeline: build host/shaders/frontend/shell → CEF gate → stage AppDir → appimagetool
lib/cef.ts        CEF runtime integrity gate (verify + heal a truncated cef-dll-sys extraction)
lib/paths.ts      repo paths, derived from import.meta.dir (never cwd)
lib/ui.ts         step(): the clack per-stage spinner wrapper
assets/<os>/      static files copied into the bundle (linux: AppRun, .desktop, icon)
```

## Workflow

```sh
just package linux          # from repo root: full pipeline → build/dist/Saffron_Anima-x86_64.AppImage
just package                # interactive: clack prompts for the target
cd packager && bun run index.ts linux   # inside the toolbox, for iterating on the packager itself
```

`bun run index.ts <target>` runs directly (no build/transpile step). Deps are pinned in `package.json`
+ `bun.lock`; `node_modules/` is gitignored. Output lands under the gitignored `build/`.

## Rules that are easy to break

- **This is Bun, not Node.** It leans on Bun APIs — `Bun.$` (the shell), `import.meta.dir`, `Bun.which`,
  `Bun.file(...).exists()`. Do not rewrite these to `child_process`/`__dirname`; run it with `bun`, never `node`.
- **File operations use `node:fs/promises`, not shell `cp`.** `Bun.$` does not invoke `/bin/sh`, so
  `cp -aL` / `install -m755` flags are unreliable through it. Copy with
  `cp(src, dst, { recursive, dereference: true, preserveTimestamps: true })` (that reproduces `cp -aL`),
  `chmod(dst, 0o755)` for executables, `symlink()` for `.DirIcon`. Reserve `Bun.$` for **running tools**
  (cargo, bun, appimagetool, curl).
- **Every pipeline stage goes through `step()`** (`lib/ui.ts`) so it gets a spinner and a uniform ✔/✖.
  Failures propagate as thrown errors; `index.ts` surfaces them via `log.error` (Bun.$ errors carry
  `.stderr`) + `cancel()`. Never swallow a `catch` — the user must see why a step failed.
- **The CEF heal-gate is mandatory and mirrors the dev path.** `verifyCefRuntime` (`lib/cef.ts`) checks
  the CEF resource *content sizes* (an interrupted `cef-dll-sys` extraction leaves 0-byte icudtl.dat, which
  it never re-provisions on its own) and, on corruption, purges + rebuilds once. The justfile's `cef_gate`
  macro is the same check for `just run`; keep the two consistent. Do **not** replace this with "pick a
  non-empty dir" — heal at the source.
- **The AppDir layout is a contract.** `usr/bin` colocates `saffron-editor-shell`, `saffron-host`, and the
  CEF runtime (CEF resolves `libcef.so` + its packs beside the shell binary); engine data goes in
  `usr/share/saffron-anima/assets` (`{models,fonts,icons,shaders}` as direct children), the built UI in
  `usr/share/saffron-anima/ui`. `AppRun` sets `SAFFRON_ANIMA_BIN`, `SAFFRON_ASSET_DIR`, `SAFFRON_SHADER_DIR`,
  `SAFFRON_UI_DIR` and lets the system Vulkan loader find the GPU. **`SAFFRON_ASSET_DIR` must point at the
  `assets/` dir itself** — the engine joins relative paths (`models/editor-camera.glb`, `fonts/…`) straight
  onto it (`engine_asset_path`, `engine/crates/assets/src/load.rs`), so it must be the dir that *directly*
  holds `models/fonts/icons/shaders`, not their parent; `SAFFRON_SHADER_DIR` is `$SAFFRON_ASSET_DIR/shaders`.
  This mirrors the dev "beside-the-binary" `target/<profile>/` layout, where those four are siblings. The
  shell serves the bundled UI over its `saffron-app://` scheme (no dev server in a package); `SAFFRON_UI_DIR`
  points it at `ui/`. Keep AppRun and that env name in sync with the shell.
- **A new OS target is `targets/<os>.ts` + a branch in `index.ts`.** Add the target to the `TARGETS` tuple
  and dispatch to it; put its static files under `assets/<os>/`. Keep the not-yet-implemented branches
  printing a clack warning, not a silent no-op.
- **Keep the justfile recipe a thin caller.** Packaging logic belongs here in TypeScript, never back in a
  bash recipe. Do not resurrect a top-level `packaging/` folder — it was replaced by this one.

## Open gaps

- `targets/windows.ts` / `targets/macos.ts` are unbuilt (macOS also needs helper apps + MoltenVK).
- Not yet wired into `just lint` / `just format` (those cover the editor).
