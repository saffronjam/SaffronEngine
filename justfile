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

# Re-exec the recipe inside the toolbox (unless already in it or SAFFRON_NO_TOOLBOX), then put bun on PATH.
reenter := '''
    if [ ! -f /run/.toolboxenv ] && [ -z "${SAFFRON_NO_TOOLBOX:-}" ]; then
      command -v toolbox >/dev/null || { echo "toolbox not found — install it, or run inside the saffron-build container" >&2; exit 1; }
      exec toolbox run -c saffron-build bash -lc 'export PATH="''' + bun_bin + ''':$PATH"; exec just --justfile "''' + justfile() + '''" "$@"' _ "$RECIPE" "$@"
    fi
    export PATH="''' + bun_bin + ''':$PATH"
'''

# Add the host's NVIDIA Vulkan ICD to the loader search (Mesa/llvmpipe stays the fallback).
nvidia_icd := '''
    NVIDIA_ICD="$(ls /run/host/usr/share/vulkan/icd.d/nvidia_icd.x86_64.json /usr/share/vulkan/icd.d/nvidia_icd.x86_64.json 2>/dev/null | head -n1 || true)"
    [ -n "$NVIDIA_ICD" ] && export VK_ADD_DRIVER_FILES="$NVIDIA_ICD"
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
schema:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=schema; {{reenter}}
    cd "{{repo}}/tools/check-control-schema" && bun run check.ts

# end-to-end tests driving a headless engine over the control plane (bun test)
e2e: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e; {{reenter}}
    {{nvidia_icd}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    # A generous per-test/hook timeout: boots wait for the non-blocking project load to reach
    # `ready`, and on the llvmpipe fallback the first content renders are slow, so the 5s default is
    # too tight for the heavier setup hooks (boot + multiple loads + import).
    cd "{{repo}}/tests/e2e" && bun test --timeout 30000

# run one e2e file by name (`.test.ts` appended if omitted): `just e2e-file rendering`
e2e-file NAME: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e-file; {{reenter}}
    {{nvidia_icd}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    name="{{NAME}}"
    case "$name" in *.test.ts) ;; *) name="$name.test.ts";; esac
    cd "{{repo}}/tests/e2e" && bun test --timeout 30000 "$name"

# run the e2e files matching a filename glob: `just e2e-glob 'material_*'`
e2e-glob PATTERN: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e-glob; {{reenter}}
    {{nvidia_icd}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    cd "{{repo}}/tests/e2e" && bun test --timeout 30000 "{{PATTERN}}"

# fast representative e2e subset (<1 min): one scene, one play, one physics, one skinned proof
e2e-smoke: engine
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=e2e-smoke; {{reenter}}
    {{nvidia_icd}}
    rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
    cd "{{repo}}/tests/e2e" && bun test --timeout 30000 \
      rendering.test.ts play.test.ts physics-falling-box.test.ts skinned-rt.test.ts

# run the Rust workspace unit + integration tests
test:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=test; {{reenter}}
    cd "{{engine}}" && cargo test --workspace

# start the Tauri editor (it spawns the engine host as a native child)
run:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{nvidia_icd}}
    export SAFFRON_WEBVIEW_HW=1
    export SAFFRON_ANIMA_BIN="{{engine_bin}}"
    cd "{{editor}}" && bun run tauri dev

# like `run`, but with the editor's developer mode pre-enabled
run-debug:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-debug; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{nvidia_icd}}
    export SAFFRON_WEBVIEW_HW=1 VITE_SAFFRON_DEV_MODE=1
    export SAFFRON_ANIMA_BIN="{{engine_bin}}"
    cd "{{editor}}" && bun run tauri dev

# run the editor on the llvmpipe software GPU (no NVIDIA ICD)
run-software:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-software; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    export SAFFRON_ANIMA_BIN="{{engine_bin}}"
    cd "{{editor}}" && bun run tauri dev

# start only the present-only host (loads a default content project so it shows a scene)
run-engine:
    #!/usr/bin/env bash
    set -euo pipefail
    RECIPE=run-engine; {{reenter}}
    cd "{{engine}}"
    cargo build --bin saffron-host
    cargo run -p xtask -- shaders
    {{nvidia_icd}}
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
    {{nvidia_icd}}
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
    {{nvidia_icd}}
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
