# Saffron Anima task runner. Toolbox-bound recipes auto-enter the `saffron-build` container;
# set SAFFRON_NO_TOOLBOX=true to run on the host. Set env vars inside recipes, never as a
# host-side `ENV=… just …` prefix (it won't cross the toolbox boundary).

set shell := ["bash", "-uc"]
# Expose recipe args as $@/$1… so `{{reenter}}` can forward them across the toolbox re-exec.
set positional-arguments

repo := justfile_directory()
engine := repo / "engine"
editor := repo / "editor"
docs := repo / "docs"

toolbox := "saffron-build"
bun_bin := "/var/home/saffronjam/.bun/bin"
engine_bin := engine / "target/debug/saffron-host"
e2e_test_args := "--timeout 30000 --max-concurrency 4"

# Put the toolchain on PATH: on Linux re-exec the recipe inside the toolbox (unless already in it
# or SAFFRON_NO_TOOLBOX) then add bun; on macOS there is no toolbox, so use the host's rustup
# toolchain (which honors rust-toolchain.toml — ahead of any Homebrew cargo) plus the host's bun.
reenter := '''
    if [ "$(uname)" = "Darwin" ]; then
      export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:$PATH"
    else
      if [ ! -f /run/.toolboxenv ] && [ -z "${SAFFRON_NO_TOOLBOX:-}" ]; then
        command -v toolbox >/dev/null || { echo "toolbox not found — install it, or run inside the saffron-build container" >&2; exit 1; }
        exec toolbox run -c saffron-build bash -lc 'export PATH="''' + bun_bin + ''':$PATH"; exec just --justfile "''' + justfile() + '''" "$@"' _ "$RECIPE" "$@"
      fi
      export PATH="''' + bun_bin + ''':$PATH"
    fi
'''

# Point the Vulkan loader at the host GPU driver: on Linux add the host's NVIDIA ICD (Mesa/llvmpipe
# stays the fallback); on macOS name MoltenVK's ICD plus Homebrew's validation-layer manifest and
# dynamic-library directory.
gpu_driver := '''
    if [ "$(uname)" = "Darwin" ]; then
      for icd in /opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json /usr/local/etc/vulkan/icd.d/MoltenVK_icd.json; do
        [ -f "$icd" ] && export VK_ICD_FILENAMES="$icd" && break
      done
      if [ -z "${VK_LAYER_PATH:-}" ]; then
        for prefix in /opt/homebrew /usr/local; do
          layer_manifest="$prefix/opt/vulkan-validationlayers/share/vulkan/explicit_layer.d"
          layer_library="$prefix/opt/vulkan-validationlayers/lib"
          if [ -d "$layer_manifest" ] && [ -d "$layer_library" ]; then
            export VK_LAYER_PATH="$layer_manifest"
            export DYLD_FALLBACK_LIBRARY_PATH="$layer_library${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
            break
          fi
        done
      fi
    else
      NVIDIA_ICD="$(ls /run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json /usr/share/vulkan/icd.d/nvidia_icd.x86_64.json 2>/dev/null | head -n1 || true)"
      [ -n "$NVIDIA_ICD" ] && export VK_ADD_DRIVER_FILES="$NVIDIA_ICD"
    fi
'''

# Build the CEF shell and verify its staged runtime, leaving $shell_bin at the binary to exec.
# Linux: cef-dll-sys stages the runtime next to the binary; an interrupted extraction leaves 0-byte
# icudtl.dat/*.pak, which cef-dll-sys never repairs on its own (it only downloads when the dir is
# absent) and CEF then aborts at startup with "Couldn't mmap icu data file" — on a truncated
# resource, purge cef-dll-sys and rebuild once, failing loudly if it recurs. macOS: CEF loads from
# the framework inside the `.app` bundle; the shell builds against a pre-provisioned CEF
# distribution at $CEF_PATH (version pinned to the `cef` crate in editor/shell/Cargo.toml — without
# it, cef-dll-sys downloads a full distribution per target dir), the bundle is assembled once, and
# later builds refresh only the binaries inside it (the framework re-copy is the slow path).
# Requires cwd = editor/shell and $cef_profile (debug|release; the macOS bundle path is debug-only).
cef_gate := '''
    if [ "$(uname)" = "Darwin" ]; then
      [ "${cef_profile:-debug}" = debug ] || { echo "cef: the macOS bundle path builds debug only" >&2; exit 1; }
      export CEF_PATH="${CEF_PATH:-$HOME/.local/share/cef/149.0.6}"
      if [ ! -d "$CEF_PATH/Chromium Embedded Framework.framework" ]; then
        echo "cef: no CEF distribution at $CEF_PATH — provision it once:" >&2
        echo "  cargo install export-cef-dir && export-cef-dir --version 149.0.6 '$CEF_PATH'" >&2
        exit 1
      fi
      cargo build
      app="target/bundle/saffron-editor-shell.app"
      if [ -x "$app/Contents/MacOS/saffron-editor-shell" ]; then
        # Replace, never rewrite in place: the kernel caches code signatures per inode and
        # SIGKILLs an exec of a binary whose inode was modified after validation.
        rm -f "$app/Contents/MacOS/saffron-editor-shell"
        cp target/debug/saffron-editor-shell "$app/Contents/MacOS/saffron-editor-shell"
        for helper_app in "$app/Contents/Frameworks/"*.app; do
          helper="$(basename "$helper_app" .app)"
          rm -f "$helper_app/Contents/MacOS/$helper"
          cp target/debug/saffron-editor-shell-helper "$helper_app/Contents/MacOS/$helper"
        done
      else
        cargo run --bin bundle
      fi
      shell_bin="$PWD/$app/Contents/MacOS/saffron-editor-shell"
    else
      if [ "${cef_profile:-debug}" = release ]; then cargo build --release; else cargo build; fi
      cef_dir="target/${cef_profile:-debug}"
      _cef_intact() {
        for f in icudtl.dat resources.pak v8_context_snapshot.bin chrome_100_percent.pak; do
          [ -s "$cef_dir/$f" ] || return 1
        done
        [ "$(stat -Lc%s "$cef_dir/icudtl.dat")" -ge 1000000 ]
      }
      if ! _cef_intact; then
        echo "cef: runtime resources in $cef_dir are truncated (interrupted extraction) — re-provisioning cef-dll-sys" >&2
        rm -f "$cef_dir"/icudtl.dat "$cef_dir"/*.pak "$cef_dir"/v8_context_snapshot.bin
        cargo clean -p cef-dll-sys
        if [ "${cef_profile:-debug}" = release ]; then cargo build --release; else cargo build; fi
        _cef_intact || { echo "cef: re-provision left $cef_dir still truncated — aborting" >&2; exit 1; }
        echo "cef: re-provisioned intact CEF resources in $cef_dir" >&2
      fi
      export LD_LIBRARY_PATH="$PWD/$cef_dir:${LD_LIBRARY_PATH:-}"
      shell_bin="$PWD/$cef_dir/saffron-editor-shell"
    fi
'''

# The per-platform Chromium switch set the shell forwards via SAFFRON_CEF_SWITCHES (comma-separated
# `k=v` / bare flags). Linux brings CEF's GPU process up over Ozone/X11; macOS runs the default
# GPU-composited OSR and mocks the Chromium keychain so a dev run never prompts for keychain access.
cef_switches := '''
    if [ "$(uname)" = "Darwin" ]; then
      export SAFFRON_CEF_SWITCHES="use-mock-keychain"
    else
      export SAFFRON_CEF_SWITCHES="ozone-platform=x11"
    fi
'''

# Pick a default content project (most-recent with meshes) for the editor-less run-engine; a preset SAFFRON_PROJECT wins.
default_project := '''
    export SAFFRON_APPDATA_DIR="''' + repo + '''/appdata"
    if [ -z "${SAFFRON_PROJECT:-}" ]; then
      SAFFRON_PROJECT="$(python3 - "$SAFFRON_APPDATA_DIR" <<'PY'
import json, os, sys, glob
appdata = sys.argv[1]
def has_meshes(pj):
    try:
        with open(pj) as f: doc = json.load(f)
    except Exception: return False
    ents = doc.get("scene", {}).get("entities", [])
    return any("Mesh" in e.get("components", {}) for e in ents)
recents = os.path.join(appdata, "recent-projects.json")
ordered = []
try:
    with open(recents) as f: ordered = [p.get("path") for p in json.load(f).get("projects", [])]
except Exception: ordered = []
extra = sorted(glob.glob(os.path.join(appdata, "userdata", "*", "project.json")),
               key=lambda p: os.path.getmtime(p), reverse=True)
for path in [p for p in ordered if p] + extra:
    if path and os.path.exists(path) and has_meshes(path):
        print(path); break
PY
)"
      [ -n "$SAFFRON_PROJECT" ] && export SAFFRON_PROJECT && echo "run-engine: default project $SAFFRON_PROJECT"
    fi
'''

[private]
default:
    @just --list

# list the available recipes
help:
    @just --list

# count tracked Rust and TypeScript source lines
count-code:
    cloc --vcs=git --include-lang=Rust,TypeScript "{{repo}}"

# init the theme submodule and serve the docs site
run-docs:
    git -C "{{repo}}" submodule update --init --depth 1 docs/themes/hugo-book
    cd "{{docs}}" && hugo server

# fetch on-demand external sources (pinned + checksummed Jolt) into the gitignored cache
fetch-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=fetch-deps; {{reenter}}
    cd "{{engine}}"
    touch crates/physics-sys/build.rs
    cargo build -p saffron-physics-sys --quiet

# remove the on-demand source cache so the next build re-fetches + re-verifies it
clean-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=clean-deps; {{reenter}}
    rm -rf "{{engine}}/crates/physics-sys/vendor"

# build the Rust workspace + compile shaders next to the host binary
engine:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=engine; {{reenter}}
    cd "{{engine}}"
    cargo build --workspace
    cargo run -p xtask -- shaders

# compile shaders only (*.slang -> SPIR-V + copy assets), no workspace build
shaders:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=shaders; {{reenter}}
    cd "{{engine}}"
    cargo run -p xtask -- shaders

# gen @saffron/protocol + tsc + vite build of the frontend
editor:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=editor; {{reenter}}
    cd "{{editor}}" && bun run build

# control-schema contract test (live `sa` control output vs schemas/control)
schema: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=schema; {{reenter}}
    {{gpu_driver}}
    cd "{{repo}}/tools/check-control-schema" && bun run check.ts

# end-to-end tests driving a headless engine over the control plane (bun test)
e2e: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e; {{reenter}}
    {{gpu_driver}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    # A generous per-test/hook timeout: boots wait for the non-blocking project load to reach
    # `ready`, and on the llvmpipe fallback the first content renders are slow, so the 5s default is
    # too tight for the heavier setup hooks (boot + multiple loads + import).
    cd "{{repo}}/tests/e2e" && bun test {{e2e_test_args}}

# run one e2e file by name (`.test.ts` appended if omitted): `just e2e-file rendering`
e2e-file NAME: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e-file; {{reenter}}
    {{gpu_driver}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    name="{{NAME}}"
    case "$name" in *.test.ts) ;; *) name="$name.test.ts";; esac
    cd "{{repo}}/tests/e2e" && bun test {{e2e_test_args}} "$name"

# run the e2e files matching a filename glob: `just e2e-glob 'material_*'`
e2e-glob PATTERN: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e-glob; {{reenter}}
    {{gpu_driver}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    cd "{{repo}}/tests/e2e" && bun test {{e2e_test_args}} "{{PATTERN}}"

# fast representative e2e subset (<1 min): one scene, one play, one physics, one skinned proof
e2e-smoke: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e-smoke; {{reenter}}
    {{gpu_driver}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    cd "{{repo}}/tests/e2e" && bun test {{e2e_test_args}} \
      rendering.test.ts play.test.ts physics-falling-box.test.ts skinned-rt.test.ts

# run the Rust workspace unit + integration tests
test:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=test; {{reenter}}
    cd "{{engine}}" && cargo test --workspace

# start the editor: build the engine host + the CEF shell, verify CEF's staged runtime, start Vite,
# then launch the shell pointed at it (the shell spawns the host as a child).
# `just run inspect` additionally opens Chrome DevTools remote debugging on :9222 (Console, Network,
# Performance tracing) — then open http://localhost:9222 in Chrome and click the page.
run mode="":
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{gpu_driver}}
    export SAFFRON_ANIMA_BIN="{{engine_bin}}"
    export SAFFRON_APPDATA_DIR="{{repo}}/appdata"
    # The shell is a standalone crate (its own target dir), outside the engine workspace.
    cd "{{editor}}/shell"
    cef_profile=debug; {{cef_gate}}
    {{cef_switches}}
    # `just run inspect` also exposes Chrome DevTools over remote debugging. `remote-allow-origins`
    # is mandatory on Chromium 149 or the DevTools websocket is refused.
    if [ "{{mode}}" = "inspect" ]; then
      export SAFFRON_CEF_SWITCHES="${SAFFRON_CEF_SWITCHES},remote-debugging-port=9222,remote-allow-origins=*"
      echo "[run] remote debugging: Chrome -> chrome://inspect -> Configure -> add localhost:9222 -> inspect the editor page."
    fi
    # Start Vite in the background, wait for it, run the shell; Vite dies with the recipe.
    cd "{{editor}}"
    bun run dev >/tmp/saffron-vite.log 2>&1 &
    trap 'kill %1 2>/dev/null || true' EXIT
    for _ in $(seq 1 100); do curl -sf http://127.0.0.1:1420 >/dev/null 2>&1 && break; sleep 0.1; done
    export SAFFRON_DEV_URL="http://127.0.0.1:1420"
    exec "$shell_bin"

# like `run`, but with the editor's developer mode pre-enabled
run-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-debug; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{gpu_driver}}
    export SAFFRON_ANIMA_BIN="{{engine_bin}}" VITE_SAFFRON_DEV_MODE=1
    export SAFFRON_APPDATA_DIR="{{repo}}/appdata"
    cd "{{editor}}/shell"
    cef_profile=debug; {{cef_gate}}
    {{cef_switches}}
    cd "{{editor}}"
    bun run dev >/tmp/saffron-vite.log 2>&1 &
    trap 'kill %1 2>/dev/null || true' EXIT
    for _ in $(seq 1 100); do curl -sf http://127.0.0.1:1420 >/dev/null 2>&1 && break; sleep 0.1; done
    export SAFFRON_DEV_URL="http://127.0.0.1:1420"
    exec "$shell_bin"

# run the editor with CEF's GPU process on software (`disable-gpu`) and, on Linux, the engine on the
# llvmpipe software GPU (no NVIDIA ICD). macOS has no software Vulkan ICD, so the engine stays on
# MoltenVK there and only the CEF half goes software.
run-software:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-software; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    if [ "$(uname)" = "Darwin" ]; then
      {{gpu_driver}}
    fi
    export SAFFRON_ANIMA_BIN="{{engine_bin}}"
    export SAFFRON_APPDATA_DIR="{{repo}}/appdata"
    cd "{{editor}}/shell"
    cef_profile=debug; {{cef_gate}}
    {{cef_switches}}
    export SAFFRON_CEF_SWITCHES="${SAFFRON_CEF_SWITCHES},disable-gpu"
    cd "{{editor}}"
    bun run dev >/tmp/saffron-vite.log 2>&1 &
    trap 'kill %1 2>/dev/null || true' EXIT
    for _ in $(seq 1 100); do curl -sf http://127.0.0.1:1420 >/dev/null 2>&1 && break; sleep 0.1; done
    export SAFFRON_DEV_URL="http://127.0.0.1:1420"
    exec "$shell_bin"

# start only the present-only host (loads a default content project so it shows a scene)
run-engine:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-engine; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{gpu_driver}}
    {{default_project}}
    exec "{{engine_bin}}"

# the present-only host forced onto llvmpipe (no NVIDIA ICD)
run-engine-software:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-engine-software; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{default_project}}
    exec "{{engine_bin}}"

# boot the host headless on the GPU for a bounded number of frames (renders offscreen, no compositor)
run-engine-headless frames="5":
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-engine-headless; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{gpu_driver}}
    export SAFFRON_EDITOR_NATIVE_VIEWPORT=1
    SAFFRON_EXIT_AFTER_FRAMES={{frames}} SAFFRON_CONTROL_SOCK="/tmp/sa-just-$$.sock" "{{engine_bin}}"

# screenshot the viewport + print per-pass GPU timings from a headless GPU boot (positional path arg)
capture out="engine/target/capture.png":
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=capture; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host --bin sa
    cargo run -p xtask -- shaders
    {{gpu_driver}}
    {{default_project}}
    export SAFFRON_EDITOR_NATIVE_VIEWPORT=1
    sock="/tmp/sa-capture-$$.sock"; log="/tmp/sa-capture-$$.log"
    export SAFFRON_CONTROL_SOCK="$sock"
    case "{{out}}" in /*) out="{{out}}";; *) out="{{repo}}/{{out}}";; esac
    mkdir -p "$(dirname "$out")"
    "{{engine_bin}}" >"$log" 2>&1 &
    host=$!
    trap 'kill "$host" 2>/dev/null || true; rm -f "$sock"' EXIT
    for _ in $(seq 1 80); do [ -S "$sock" ] && break; sleep 0.25; done
    [ -S "$sock" ] || { echo "capture: host never opened $sock"; tail -20 "$log"; exit 1; }
    SA="{{engine}}/target/debug/sa"
    "$SA" screenshot --target viewport --path "$out" >/dev/null
    # per-pass GPU timings: arm timestamps, then nudge the camera so the reactive loop renders a
    # fresh burst in timestamps mode (a static scene idles and would otherwise report nothing).
    "$SA" profiler.set-mode --mode timestamps >/dev/null
    "$SA" set-camera --fov 44 >/dev/null; "$SA" set-camera --fov 45 >/dev/null
    sleep 1
    echo "--- pass timings ---"
    "$SA" pass-timings
    "$SA" quit >/dev/null 2>&1 || true
    echo "capture: wrote $out"

# package the editor as a distributable: `just package linux` builds an AppImage; no arg prompts for a
# target. The Bun + clack packager under packager/ drives the pipeline. Output lands in build/dist/.
package target="":
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=package; {{reenter}}
    cd "{{repo}}/packager"
    bun install --silent
    exec bun run index.ts {{target}}

# the host-runnable control CLI; `just sa ping`, `just sa help`
sa *args:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=sa; {{reenter}}
    cd "{{engine}}" && cargo run --bin sa -- {{args}}

# cargo fmt the Rust workspace + oxfmt the editor TypeScript
format:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=format; {{reenter}}
    cd "{{engine}}" && cargo fmt
    cd "{{editor}}" && bun run format

# cargo fmt --check + clippy (deny warnings) on the workspace + oxlint the editor
lint:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=lint; {{reenter}}
    cd "{{engine}}" && cargo fmt --check
    cd "{{engine}}" && cargo clippy --workspace -- -D warnings
    cd "{{editor}}" && bun run lint

# format everything, then lint
prepare-for-commit:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=prepare-for-commit; {{reenter}}
    just --justfile "{{justfile()}}" format
    just --justfile "{{justfile()}}" lint

# the full reproducible gate (engine build + shaders, smoke, schema, frontend)
check:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=check; {{reenter}}
    "{{repo}}/tools/ci/check.sh"
