# Saffron Anima

A from-scratch **Vulkan** renderer / **Rust** game engine. The workspace (`engine/`, a Cargo
workspace) builds **`saffron-host`**, a *present-only viewport host*: it renders the scene plus a
native gizmo overlay offscreen, publishes frames into shared memory, and serves the control plane —
**no UI panels of its own**. The **editor is the Tauri/React/TypeScript app in `editor/`**; it spawns
the host, presents its frames on a Wayland subsurface below the transparent webview (UI composites
over the live viewport), and drives every operation over a JSON-over-unix-socket control plane. The
engine keeps the API *shape* that works — an `App`/`Layer` lifecycle, a deferred `submit(closure)`
render seam, a frame graph, a hecs scene, signal/slot events.

## Conventions (not optional)

- **ALWAYS the modern, correct approach — cost is never the deciding factor.** When two
  implementations differ in correctness or modernity, choose the modern, technically-right one
  *regardless of how large the coding task is*. Never propose or pick a cheaper, simpler, or
  partial alternative *because* it is less work, smaller, or faster to land — effort is not a
  reason to compromise on the right design. Surface the correct option as the recommendation and
  build it; mention a lesser fallback only if explicitly asked. (Scope/phasing may still be
  discussed, but the destination is always the correct modern design.)
- **NO LEGACY. NO COMPAT SHIMS. EVER.** This is a clean-slate codebase (`main` is an intentional orphan
  fresh start — there is nothing on disk, in the field, or downstream to be backward-compatible with).
  There is exactly **one** way to do each thing, and one code path for it. When a change would break an
  existing flow, command, file format, component, or test: **break it, then rebuild that flow on the new
  design — in the same change.** This is absolute and overrides any instinct toward caution:
  - **Never** keep an old code path alive "for back-compat" or "so callers don't break".
  - **Never** add a second command / function / format / field that duplicates an existing one's purpose
    just to avoid disturbing its callers. Replace the old one and update every caller.
  - **Never** defer a cutover by leaving the superseded path running next to the new one ("additive for
    now, retire later" is forbidden — *now* is when you retire it).
  - If you ever catch the thought *"I won't do X because it would break Y"* — that is the signal to **do X
    and fix Y**, not to preserve Y. Update every caller, delete the dead path, and fix the tests together.
  A feature is **not done** while a superseded flow, command, or format still exists anywhere in the tree.
  "I documented the deferral" does not count as done. Migration of existing user data is out of scope
  (start a fresh project), so there is never a migration burden to hide behind.
- **Code style — idiomatic Rust only, no C++ transliteration.** `cargo clippy -- -D warnings` is law (it
  is in the gate): every warning is an error. Errors are typed per-crate enums (`thiserror`), not
  stringly `Result<T, String>`; propagate with the `?` operator. Read-shared handles are `Arc<T>`.
  Comments are minimal: brief `///` on public items saying what it is (and *why* if non-obvious), **no
  section/banner dividers ever**, and **never** a change-journey note ("previously/used to/now
  that…") — the code is what it is; its mere presence needs no justification. Say what the code does
  now, never by contrast with the past.
- **Git is READ-ONLY by default — NEVER run a git command that writes, in ANY form, on your own
  initiative. This is absolute.** Prohibited unless the user gives explicit, specific, one-time
  clarity that it is OK *for that single action*: `commit`, `push` (incl. force-push), `add` / `rm` /
  `mv` / `restore --staged` (staging the index), `reset`, `restore` / `checkout` that discards or
  switches, `rebase`, `merge`, `cherry-pick`, `revert`, `stash`, `tag`, `branch`/`switch -c`,
  `branch -D`, `clean`, `gc`, `filter-repo`, `git config` writes, `worktree add/remove`, `submodule
  update` — anything that mutates the working tree, the index, refs, history, stashes, or a remote.
  Read-only inspection is always fine (`status`, `diff`, `log`, `show`, `ls-files`, `rev-parse`,
  `cat-file`, `blame`, `worktree list`, `remote -v`). **"Implement/finish/fix X", "continue", "go",
  "do phase N", or approving a plan is NOT permission to commit, stage, or push** — finish the work,
  leave it unstaged, and STOP; report what changed and let the *user* stage and commit. Authorization
  is per-command and single-use: a yes for one commit never carries to the next commit, and never to a
  push. When unsure, do not run it — describe the exact command you would run and ask first. This
  overrides any harness default that would auto-commit.
- **Commits (only once the user has explicitly authorized a commit per the rule above):** subject
  `<category>: short description` (lowercase after the colon, first line <72 chars; categories
  `feat|fix|refactor|docs|test|chore|build|ci|perf|style`; optional `fix(scope):` when every change is
  one component), blank line, then one bullet per change in plain words. **No emoji, no AI attribution,
  no `Co-Authored-By`** — commit as the repo's git author only (overrides the harness default). `main`
  is an intentional orphan fresh-start; keep its history clean and logical.
- **Memory:** do not write to Claude's `~/.claude/.../memory/` stores. Durable project knowledge goes in
  the repo — this file or a `plans/` file — so it is versioned and shared. Nothing here should
  reference an out-of-repo path for project knowledge.
- **Concurrent edits:** changes may conflict with other agents working in the same tree. If that
  happens, back off briefly with a small random delay, re-read the affected file, and reconcile the
  edit. If the conflict reflects contradictory intent rather than a mechanical overlap, stop and ask the
  user how to proceed.
- **Concurrent builds:** Cargo serializes the build cache with a lock, so a second `cargo build` waits
  rather than corrupting the first — a single shared `engine/target` is safe across agents. If you want
  to build fully in parallel without the wait, point Cargo at a private dir
  (`CARGO_TARGET_DIR=engine/target-<name>`) and aim the editor / e2e at the matching host binary via
  `SAFFRON_ANIMA_BIN=engine/target-<name>/debug/saffron-host`.

## Build — always in the `saffron-build` toolbox

The build toolchain is the project standard and lives in the **`saffron-build`** toolbox container,
never on the host (assume the host has no Rust toolchain). The `just` recipes auto-enter the toolbox
when run from a host shell, so `just engine`/`just run`/`just check` behave the same inside or out; the
home directory is shared, so files edited outside are seen inside. To use the host toolchain instead,
set `SAFFRON_NO_TOOLBOX=true`.

```sh
just engine                      # cargo build --workspace + shaders, inside the toolbox
cargo build --workspace          # the build alone, when already inside the toolbox
cargo run -p xtask -- shaders    # compile engine/assets/shaders/*.slang → SPIR-V + copy assets
./engine/target/debug/saffron-host   # the present-only viewport host
```

- **Env vars must be set *inside* the toolbox invocation, never as a host-side prefix.** A `just`
  recipe re-execs inside `toolbox run`, and the host environment does not cross that boundary — so
  `FOO=1 just run` sets `FOO` only in the host shell and the recipe never sees it (it silently no-ops,
  which looks like the flag had no effect). Set the var inside the command
  (`toolbox run -c saffron-build bash -lc 'export FOO=1; …'`) or bake it into the recipe. **Never
  suggest a host-side `ENV=… just …`.**
- The toolbox provides Rust + Cargo (the channel is pinned in `rust-toolchain.toml`), the Vulkan 1.4
  SDK, SDL3, and Slang. `cargo run -p xtask -- shaders` is the shader pipeline; `cargo run -p xtask --
  gen-protocol` regenerates the editor-facing protocol artifacts from the `saffron-protocol` DTOs.
- **The real GPU is available inside the toolbox** — the host's NVIDIA card enumerates fine (e.g. a
  discrete RTX). The toolbox reaches the host's Vulkan ICD through the `/run/host` mount; the driver is
  **not** in the toolbox's own `/usr`. The `just run*` recipes add it via the `nvidia_icd` macro, which
  resolves and exports:
  `VK_ADD_DRIVER_FILES=/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json`
  (falling back to `/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json`, then to llvmpipe if neither
  exists). **Do not hand-roll this path in an ad-hoc script** — it is *not* `/usr/share/…/nvidia_icd.json`
  (wrong location *and* the filename carries the `.x86_64` suffix), and getting it wrong silently drops
  you to llvmpipe, which reads as "no GPU here" when in fact the card is right there. Reuse
  `just run-engine-headless` / `just run-engine`, or copy the `nvidia_icd` macro from the `justfile`
  verbatim. Confirm with `vulkaninfo --summary` (it lists the NVIDIA device) or the host's
  `vulkan ready — gpu 'NVIDIA …' (discrete)` log line. Mesa llvmpipe is the fallback and is fine for
  correctness/validation (just slow); `just run-software` forces it.

### Headless runs & the verification gate

- `SAFFRON_EXIT_AFTER_FRAMES=N ./engine/target/debug/saffron-host` exits after N frames.
- **No display?** Run a headless compositor in the toolbox, then point the window backend at it:
  `weston --backend=headless --width=1280 --height=720 --socket=wl-x --idle-time=0 &`, then
  `export WAYLAND_DISPLAY=wl-x`. Use a unique `--socket` + `SAFFRON_CONTROL_SOCK` per run, and capture
  the exit code to a file *before* any `pkill` (the toolbox wrapper surfaces the pkill signal, not the
  real exit code). `just run-engine-headless [frames]` wraps this.
- **Want the NVIDIA GPU in a headless/ad-hoc run?** It is available (see the GPU note above). Export
  `VK_ADD_DRIVER_FILES=/run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json` before launching the
  host, or just use the `just run-engine-headless` recipe which already does it. A bespoke script that
  probes the toolbox-local `/usr/share/vulkan/icd.d/` (Mesa-only) or the un-suffixed `nvidia_icd.json`
  will fall to llvmpipe — that is a harness bug, not an absent GPU.
- The reproducible gate is `tools/ci/check.sh`: workspace build + shaders → present-only smoke →
  control-schema contract test → frontend bun build. `just check` wraps it once the toolbox/bun/display
  are set up (also `just engine|editor|schema|test|e2e`). There is intentionally no GitHub-hosted CI (a
  stock runner can't reproduce the toolbox); `.github/workflows/ci.yml` targets a self-hosted runner.
- `just e2e` runs the `tests/e2e` suite — TypeScript on `bun test` that boots a headless host and
  drives it over the control plane (typed via `@saffron/protocol`), asserting responses and a
  validation-clean log. It is the language-appropriate place for engine behaviour tests: the wire is
  JSON, so the driver need not be Rust.
- Convenience recipes (all auto-enter the toolbox; `just help` lists them): `just run` starts the
  editor, which spawns the host; `just run-engine` starts only the present-only host; `just run-docs`
  serves the Hugo site. `just format` runs `cargo fmt` over the workspace and oxfmt over the editor
  TypeScript; `just lint` runs `cargo fmt --check` + `cargo clippy --workspace -- -D warnings` + oxlint
  (`editor/.oxlintrc.json`); `just prepare-for-commit` does format then lint.

### The editor (Tauri/React)

With `bun` on PATH inside the toolbox:

```sh
cd editor && bun install && bun run check && bun run tauri dev
```

`bun run check` regenerates `@saffron/protocol` (via `xtask gen-protocol`) from the `saffron-protocol`
DTOs and typechecks; `bun run format` (oxfmt) and `bun run lint` (oxlint) cover style. `tauri dev`
spawns `engine/target/debug/saffron-host` (override with `SAFFRON_ANIMA_BIN`) and needs a Wayland
session for the subsurface presenter.

## Architecture

- **Lifecycle:** a client fills `AppConfig` (window config + `on_create`/`on_exit`) and calls `run`,
  which owns the main loop: poll events → `on_update` → `begin_frame` → `on_render` (submit GPU work) →
  `on_ui` → `begin_frame_graph` (cull + scene passes) → `on_render_graph` (app passes) → `end_frame`
  (derive barriers, execute, present). `run` calls `wait_gpu_idle` before any teardown.
- **Layer = trait of optional hooks** (`on_attach/on_update/on_render/on_ui/on_render_graph/on_detach`,
  all defaulted); a layer is pushed onto the `App` and the loop dispatches each hook through it.
- **Render seam:** `Renderer::submit(|cmd| { … })` records a closure into the current frame.
- **Render graph:** each pass *declares* its resource usage (`ColorWrite`, `SampledRead`,
  `StorageImageRwCompute`, …) + attachments; the graph derives every barrier and layout transition and
  records each pass body. No pass writes a barrier by hand; apps add passes via `on_render_graph`.
- **Resources:** Vulkan via the `ash` bindings (`vk::*`) — every call returns a `Result`, checked on
  the spot. VMA via `vk-mem` for allocation. Data-plane resources are RAII wrappers held as `Arc<T>`,
  freed before the device (teardown: the client drops its handles in `on_exit`, `run` calls
  `wait_gpu_idle` first, so nothing outlives the allocator).
- **Events:** `SubscriberList<Args>` signal/slot (handler returns `true` to stop propagation); the
  window exposes typed signals (`on_resize`, `on_key_pressed`, …).
- **Errors:** fallible functions return a per-crate `Result<T, Error>` over a `thiserror` enum; no
  panics on expected failure paths in engine code.

## Crates (Cargo workspace under `engine/`)

Members are `crates/*` plus `xtask`; every crate is named `saffron-<area>` (the two binaries are
`saffron-host`, the present-only editor host, and `saffron-player`, the exported game). The inter-crate
DAG, leaves first:

```
saffron-core
saffron-log
saffron-signal      → saffron-core
saffron-json        → saffron-core
saffron-window      → {saffron-core, saffron-signal}
saffron-geometry    → saffron-core
saffron-scene       → {saffron-core, saffron-json}                       hecs-backed ECS
saffron-animation   → {saffron-core, saffron-geometry, saffron-scene}
saffron-physics-sys → (cxx-built vendored Jolt 5.3.0)
saffron-physics     → {saffron-core, saffron-geometry, saffron-scene, saffron-animation, saffron-physics-sys}
saffron-script      → {saffron-core, saffron-scene}                      Luau via mlua (vendored)
saffron-rendering   → {saffron-core, saffron-window, saffron-geometry}   ash + vk-mem
saffron-assets      → {saffron-core, saffron-json, saffron-geometry, saffron-rendering, saffron-scene}
saffron-sceneedit   → {saffron-core, saffron-signal, saffron-scene, saffron-json}
saffron-runtime     → {saffron-core, saffron-scene, saffron-assets, saffron-animation, saffron-script, saffron-physics}   shared play-mode sim spine
saffron-protocol    → saffron-core                                       wire DTOs (serde + schemars + ts-rs)
saffron-control     → {saffron-core, saffron-geometry, saffron-json, saffron-window, saffron-rendering, saffron-scene, saffron-sceneedit, saffron-assets, saffron-physics, saffron-protocol}
saffron-app         → {saffron-core, saffron-window, saffron-rendering}
saffron-host        → {saffron-core, saffron-log, saffron-app, saffron-window, saffron-rendering, saffron-sceneedit, saffron-runtime, saffron-control, saffron-scene, saffron-geometry, saffron-animation, saffron-physics, saffron-script, saffron-assets, saffron-signal, saffron-protocol}   (the present-only host exe)
saffron-player      → {saffron-core, saffron-log, saffron-app, saffron-runtime, saffron-rendering, saffron-window, saffron-scene, saffron-assets, saffron-protocol}   (the exported-game exe)
saffron-control-client → saffron-protocol                               unix-socket client (no engine dep)
sa                  → {saffron-protocol, saffron-control-client}         the control CLI (clap)
```

- `saffron-test-support` is a dev-dependency crate of shared test helpers; `saffron-e2e` carries the
  `tests/e2e` driver; `xtask` is the build-task runner (shaders, protocol codegen).
- There is no engine UI toolkit: the in-viewport gizmo is a **native overlay** (`OverlayVertex` /
  `submit_overlay` in `saffron-rendering`; `build_scene_edit_overlay` in `saffron-host`), and the full
  editor UI is the React/Tauri frontend.

## Layout

```
engine/Cargo.toml       the Cargo workspace (members: crates/*, xtask)
engine/crates/<crate>/  one crate per entry above (core, rendering, host, sceneedit, …)
engine/crates/host/     the saffron-host present-only viewport binary
engine/xtask/           the build-task runner: `cargo run -p xtask -- {shaders,gen-protocol}`
engine/assets/          shaders (*.slang → SPIR-V via xtask), models, fonts, icons (copied next to the exe)
editor/                 Tauri/React/TS editor — src/ (React + Zustand + typed control client), src-tauri/ (Rust bridge)
schemas/control/        DTO-first wire contract → @saffron/protocol: hand-authored envelope.schema.json + generated openrpc/command-manifest JSON (from the saffron-protocol DTOs via xtask gen-protocol)
tools/ci/, tools/check-control-schema/, tools/check-projects/   the reproducible gate, the live-vs-schema contract test, the project-feature smoke
tests/e2e/              end-to-end tests (bun) driving a headless host over the control plane
docs/                   Hugo (hugo-book) docs site — per-concept explanations + how-to/reference/tutorials
plans/                  phased, dependency-ordered plans for future expansions
justfile                the task runner (build/run/test/lint/format/check); auto-enters the toolbox
```

The `sa` control CLI is the `sa` crate (`engine/crates/sa`); the protocol codegen is `xtask
gen-protocol` over `saffron-protocol`.

## Stack

| Area | Choice | Notes |
|------|--------|-------|
| Language / toolchain | Rust (channel pinned in `rust-toolchain.toml`), edition 2024 | `cargo clippy -- -D warnings`, `unsafe_code = deny` workspace-wide (except the FFI seams) |
| Build | Cargo workspace + `xtask` | versions pinned once in `[workspace.dependencies]` |
| Vulkan | `ash` 0.38 `vk::*`, target 1.4 | dynamic rendering + sync2; `ash-window` + `raw-window-handle` |
| Allocation | `vk-mem` 0.4 | VMA bindings |
| Window / ECS / math | `winit` 0.30, `hecs` 0.11, `glam` 0.30 | hecs wrapped behind `saffron-scene` |
| Scripting | `mlua` (Luau, vendored) | behind `saffron-script` |
| Physics | vendored Jolt 5.3.0 via `cxx` | `saffron-physics-sys` (FFI) + `saffron-physics` |
| Shaders | Slang | `slangc -target spirv`, run by `xtask shaders` |
| Serialization | `serde` + `serde_json` (`preserve_order`) + `serde_with` + `schemars` + `ts-rs` | scene/project save/load; wire DTOs + codegen |
| Import / images | `gltf`, `tobj`, `image`, `resvg`/`usvg`/`tiny-skia` | glTF/OBJ → `.smesh`; texture decode; SVG icons |
| Errors | `thiserror` (per-crate enums), `anyhow` at edges | `bytemuck` for GPU struct casts |
| Editor | Tauri 2 + React 19 + Vite + shadcn/ui + Tailwind v4, Bun | |

## Keep current (part of "done")

- **Milestone gate:** after each feature — and at each phase boundary of a larger task, not only at
  the very end — run `just engine` then `just prepare-for-commit` (format + lint) and fix every warning
  your change raises. The point is a clean testing ground at intervals (a green `cargo build` a plain
  `just run` picks up), not one big reconciliation at the end. This composes with the concurrent rules
  above: if the build or lint fails *only* because of another agent's in-flight changes (see
  **Concurrent edits** / **Concurrent builds**), assume it will land soon — leave it, note it, and move
  on. **Never** fix another agent's parallel work to make the gate pass. When unsure whether a failure
  is yours, gate your own changes in isolation via a private `CARGO_TARGET_DIR`, and it is fine to defer
  the shared build until the tree settles.
- **`sa` CLI:** a feature that adds engine state worth driving/inspecting gets a matching control
  command (one registration in `saffron-control`), so the running editor stays scriptable and visually
  debuggable from a shell.
- **`docs/`:** a change that adds/alters an engine concept updates the matching explanation page under
  `docs/content/` and its hub `_index.md` row, in the same change.

## Docs site

Hugo (hugo-book theme, organised by Diátaxis). Needs **Hugo extended** (it compiles SCSS); the theme is
a git submodule.

```sh
git submodule update --init --depth 1 docs/themes/hugo-book
cd docs && hugo server   # preview at http://localhost:1313/saffron-anima/
```

Page conventions: one concept per page; TOML front matter (start from an archetype); **title** is a
short sentence-case noun phrase, and the front-matter `title` must equal the body `# H1` (the theme
doesn't render the title). Lead with the concept and why, not "file X does Y"; put code pointers in a
slim `What | File | Symbols` table (symbols, not line numbers). Math via KaTeX (`$…$`, `math = true`),
diagrams via ` ```mermaid `, callouts via GitHub alerts. Voice plain and direct — run prose through the
`humanizer` pass. Theme overrides live in `docs/assets/_custom.scss` + `docs/layouts/_partials/docs/inject/head.html`.

## Plans (`plans/`)

Phased, dependency-ordered implementation plans for scoped-but-unbuilt expansions; each subfolder is one
feature area with a `README.md` index + numbered `phase-N-*.md` files, grounded in current code. Each
plan carries a `**Status:**` line (`NOT STARTED`/`IN PROGRESS`/`COMPLETED`); mark it `COMPLETED` when
done, and delete a plan file only *after* it is `COMPLETED`. **Check `plans/` first** when implementing
a feature — follow and update a matching plan rather than starting cold.

## Status

- **Built** (per-concept reference is `docs/`): the full forward+ PBR pipeline — clustered lighting, IBL,
  shadows (directional/spot/point/contact/ray-traced), DDGI + voxel GI + SSGI + ReSTIR, GTAO, TAA, motion
  vectors, tonemap, MSAA/FXAA; bindless + instanced rendering with an übershader/PSO cache; the render
  graph; hecs scene + registry-driven JSON project format with scene-graph parenting (a `Relationship`
  component, parent-composed world transforms, and a `set-parent` reparent command); glTF/OBJ import +
  asset catalog; a native
  material system (`.smat` PBR assets + params buffer, importer, asset-level instances/overrides,
  thumbnails; entities bind materials through one `MaterialSet` component whose submesh-indexed slots
  *reference* a `.smat` asset plus a sparse per-object override map — no inline per-entity PBR blob) with
  a node-graph editor (React Flow model → Slang codegen for preview and scene entities); skeletal animation
  behind `saffron-animation` (glTF clip import, an animation-player runtime with transitions/blending, a
  compute-skinning prepass feeding motion vectors + skinned-BLAS rebuild, foot IK, a native skeleton
  overlay, animation control commands, and the editor timeline panel); the control plane + `sa` CLI; the
  Tauri editor (with editor-only per-tab undo/redo reconstructed from inverse control calls, and an
  in-editor Asset Store that imports models/textures/HDRIs/materials from external providers — Poly Haven,
  ambientCG, Poly Pizza, Sketchfab — over an editor-local connector backend with OS-keyring credentials
  and OAuth loopback); per-entity Luau scripting (behind `saffron-script`: ScriptComponent slots,
  script-declared fields + overrides, Inspector UI, project `src/` scaffold); physics behind
  `saffron-physics` (Jolt vendored, cross-platform-deterministic; a per-play world on the play edge;
  rigidbody/collider split components with five shapes + materials + auto-fit; object-layer matrix +
  sensors/triggers + a contact-event ring to scripts; kinematic bone-following; a `CharacterVirtual`
  controller; raycast/shapecast queries + a Luau `sa.raycast`; and a motor-driven ragdoll routed through
  the pose-buffer override/weight blend layer — passive, active, and partial, with import auto-fit).
- **Not yet:** transient render-graph resources (graph-created images + aliasing) + async compute;
  GPU-driven culling (MDI / mesh shaders); hardware GPU in the toolbox.
