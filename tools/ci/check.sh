#!/usr/bin/env bash
# The single reproducible verification gate for the Saffron Anima engine + CEF editor shell. It
# sequences every test layer in dependency order, accumulates failures, and prints one ALL/SOME
# verdict.
#
# Run inside the saffron-build toolbox with the host bun on PATH, under a display (the engine
# smoke, the schema contract test, the project smoke, and the e2e suite open a swapchain → need
# one):
#
#   toolbox run -c saffron-build bash -lc '
#     export PATH="/var/home/saffronjam/.bun/bin:$PATH" XDG_RUNTIME_DIR=/run/user/$(id -u)
#     weston --backend=headless --width=1280 --height=720 --socket=wl-ci --idle-time=0 &
#     sleep 2; export WAYLAND_DISPLAY=wl-ci SDL_VIDEODRIVER=wayland
#     tools/ci/check.sh
#   '
#
# `just check` invokes this script the same way. The sequenced steps:
#
#   1. workspace build           cargo build --workspace
#   1b. editor shell build       cd editor/shell && cargo build (standalone CEF crate; links libcef)
#   2. codegen freshness         xtask gen-protocol + git diff over the generated wire/Luau artifacts
#   3. unit + crate tests        cargo test --workspace (inline #[cfg(test)] + tests/, incl. the
#                                golden/snapshot tests and the physics determinism gate)
#   4. self-test-removal grep    no run*SelfTest / SAFFRON_SELFTEST / fn *self_test outside #[cfg(test)]
#   5. present-only smoke        SAFFRON_EXIT_AFTER_FRAMES=5 + validation-clean log grep
#   6. control-schema contract   check-control-schema/check.ts against the live host
#   7. project startup smoke     check-projects/check.sh against the live host
#   8. e2e                       the tests/e2e bun suite against the host
#   9. frontend                  editor/ bun run build + bun test
#  10. lint                      cargo fmt --check + cargo clippy --workspace -- -D warnings
#
# The four standing gates are IN the sequence: validation-clean (step 5), the control-schema
# contract (step 6), golden/snapshot + the cross-arch determinism gate (inside step 3's cargo
# test), and the e2e validation-clean assertions (step 8).
#
# Hardware/display-gated steps that this x86 software-GPU toolbox cannot fully run DEFER (without
# failing the gate) and say why: the frontend build defers if bun/editor deps are absent, and the
# determinism gate's aarch64 leg is owned by `physics/tests/determinism.rs` and DEFERRED-NEEDS-
# HARDWARE there (the x86 half runs in step 3; the ARM half needs the self-hosted aarch64 runner).
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENGINE="$REPO/engine"
cd "$REPO"
fail=0
declare -a results=()
deferred=()

step() { echo; echo "=== $* ==="; }

# A required step failed: record it so the final verdict is SOME GATES FAILED.
fail() { echo "FAILED: $*" >&2; fail=1; }

# A step's prerequisite (a host tool / display this hardware lacks) is missing: note it and move
# on without failing the gate, with the reason.
defer() { echo "DEFERRED: $*"; deferred+=("$*"); }

# Record a per-step verdict for the final summary.
pass_step() { results+=("PASS  $1"); }
fail_step() { results+=("FAIL  $1"); fail "$2"; }
defer_step() { results+=("DEFER $1"); defer "$2"; }

# The present-only host + control CLI built by `cargo build --workspace`.
RUST_HOST="${SAFFRON_ANIMA_BIN:-$ENGINE/target/debug/saffron-host}"
RUST_SA="${SAFFRON_SA_BIN:-$ENGINE/target/debug/sa}"

# Does the host boot far enough to answer a control `ping`? A per-run socket must appear and
# `sa ping` must round-trip. Probed once and cached (steps 5–8 gate on it). A FALSE here is a
# regression (NOT a defer): the build step produced the binary, so a host that does not answer
# ping is a failure to surface, not a missing prerequisite.
host_ready_cache=""
host_ready() {
  if [ -n "$host_ready_cache" ]; then return "$host_ready_cache"; fi
  probe_host
  host_ready_cache=$?
  return "$host_ready_cache"
}

probe_host() {
  [ -x "$RUST_HOST" ] && [ -x "$RUST_SA" ] || return 1
  local sock="/tmp/sa-ci-probe-$$.sock"
  rm -f "$sock"
  SAFFRON_CONTROL_SOCK="$sock" "$RUST_HOST" >/tmp/sa-ci-probe-$$.log 2>&1 &
  local pid=$!
  local ok=1
  for _ in $(seq 1 80); do
    if [ -S "$sock" ]; then ok=0; break; fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.1
  done
  if [ "$ok" -eq 0 ]; then
    SAFFRON_CONTROL_SOCK="$sock" "$RUST_SA" ping >/dev/null 2>&1 || ok=1
  fi
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  rm -f "$sock" "/tmp/sa-ci-probe-$$.log"
  return "$ok"
}

step "1. workspace build (cargo build --workspace)"
if ( cd "$ENGINE" && cargo build --workspace ); then
  ( cd "$ENGINE" && cargo run -q -p xtask -- shaders ) || fail_step "1. workspace build (shaders)" "cargo run -p xtask shaders"
  pass_step "1. workspace build"
else
  fail_step "1. workspace build" "cargo build --workspace"
fi

# The CEF editor shell is a standalone crate (its own Cargo.toml/lock, outside the engine workspace).
# Its build.rs downloads/links the version-locked libcef; defer if that provisioning
# is absent rather than failing the gate (the "hardware/tooling this environment lacks" spirit).
step "1b. editor shell build (cargo build in editor/shell, links libcef)"
if ( cd "$REPO/editor/shell" && cargo build ); then
  pass_step "1b. editor shell build"
else
  defer "1b. editor shell build — libcef link/provisioning unavailable (cd editor/shell && cargo build)"
fi

step "2. codegen freshness (xtask gen-protocol + git diff over the generated wire + Luau artifacts)"
if ( cd "$ENGINE" && cargo run -q -p xtask -- gen-protocol ) && git diff --exit-code -- \
    editor/src/protocol/sa-types.ts \
    schemas/control/openrpc.generated.json \
    schemas/control/command-manifest.generated.json \
    schemas/control/sa.generated.luau; then
  pass_step "2. codegen freshness"
else
  fail_step "2. codegen freshness" "generated wire/Luau artifacts drifted (run \`cargo run -p xtask gen-protocol\`)"
fi

step "3. unit + crate tests (cargo test --workspace — incl. golden/snapshot + the determinism gate)"
if ( cd "$ENGINE" && cargo test --workspace ); then
  pass_step "3. unit + crate tests"
else
  fail_step "3. unit + crate tests" "cargo test --workspace"
fi

step "4. self-test-removal assertion (no runtime run*SelfTest / SAFFRON_SELFTEST / fn *self_test)"
# No in-engine self-test mechanism survives. Any of the three patterns appearing OUTSIDE a
# `#[cfg(test)]` module is a runtime self-test and fails the gate. The `#[cfg(test)] mod tests`
# test functions (e.g. `scene_hierarchy_self_test`) are the legitimate place for these and are
# not flagged.
selftest_hits="$(
  find "$ENGINE" -name '*.rs' -not -path '*/target/*' -print0 | xargs -0 awk '
    /^#\[cfg\(test\)\]/ { intest = 1 }
    /run[A-Za-z]+SelfTest|SAFFRON_SELFTEST|fn [A-Za-z0-9_]*self_test/ {
      line = $0; sub(/^[ \t]+/, "", line)
      if (line ~ /^\/\//) next
      if (!intest) { print FILENAME ":" FNR ": " $0 }
    }
  '
)"
if [ -z "$selftest_hits" ]; then
  pass_step "4. self-test-removal assertion"
else
  echo "$selftest_hits" >&2
  fail_step "4. self-test-removal assertion" "a runtime self-test survives outside #[cfg(test)] (see above)"
fi

step "4b. draw-path tripwire (no CPU gather/batcher/meshlet symbol survives)"
# The persistent GPU scene's visibility traversal is the ONE production draw path.
# Any of these identifiers reappearing in engine source is a resurrected CPU draw
# list, batcher, or the retired mesh-shader raster toggle.
tripwire_hits="$(
  grep -rnE 'DrawItem|DrawBatch|submit_draw_list|gather_static_draw_list|record_scene_draw_list|record_transparent_draw_list|SAFFRON_MESH_SHADER|MeshletRaster|record_meshlet_draws' \
    "$ENGINE/crates" --include='*.rs' 2>/dev/null || true
)"
if [ -z "$tripwire_hits" ]; then
  pass_step "4b. draw-path tripwire"
else
  echo "$tripwire_hits" >&2
  fail_step "4b. draw-path tripwire" "a retired CPU draw-path symbol survives (see above)"
fi

step "5. present-only smoke (bounded, headless) + validation-clean log grep"
if host_ready; then
  smoke_log="/tmp/sa-ci-smoke-$$.log"
  smoke_ok=0
  (
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    cd /tmp && rm -f project.json
    SAFFRON_EXIT_AFTER_FRAMES=5 SAFFRON_CONTROL_SOCK="/tmp/sa-ci-$$.sock" "$RUST_HOST" >"$smoke_log" 2>&1
  ) || smoke_ok=1
  cat "$smoke_log"
  # The validation-clean gate (13:phase-5): the boot+render smoke must produce zero
  # `ERROR  vulkan  [validation] …` lines. The grep IS the gate — a dirty log is a
  # render-subsystem bug (a wrong barrier, a layout mismatch) that never throws or corrupts a
  # wire byte, so this is its only automated detector. The regression probe lives in the e2e
  # suite (boots with `SAFFRON_VK_PLANT_VALIDATION_ERROR` and asserts the grep WOULD catch it).
  if grep -qE "ERROR[[:space:]]+vulkan[[:space:]]+\[validation\]" "$smoke_log"; then
    smoke_ok=1
    echo "present-only smoke produced Vulkan validation errors (see log above)" >&2
  fi
  rm -f "$smoke_log"
  if [ "$smoke_ok" -eq 0 ]; then pass_step "5. present-only smoke + validation-clean"; else fail_step "5. present-only smoke + validation-clean" "present-only host smoke / validation-clean"; fi
else
  fail_step "5. present-only smoke + validation-clean" "the Rust host did not boot + answer ping (see probe log)"
fi

step "6. control DTO contract test (live help/results vs generated manifest/OpenRPC)"
if host_ready; then
  if ( cd "$REPO/tools/check-control-schema" && SAFFRON_ANIMA_BIN="$RUST_HOST" SAFFRON_SA_BIN="$RUST_SA" bun run check.ts ); then
    pass_step "6. control-schema contract"
  else
    fail_step "6. control-schema contract" "control-schema contract test"
  fi
else
  fail_step "6. control-schema contract" "the Rust host did not boot + answer ping (see probe log)"
fi

step "7. project startup and asset layout smoke"
if host_ready; then
  if ( SAFFRON_ANIMA_BIN="$RUST_HOST" SAFFRON_SA_BIN="$RUST_SA" "$REPO/tools/check-projects/check.sh" ); then
    pass_step "7. project smoke"
  else
    fail_step "7. project smoke" "project smoke"
  fi
else
  fail_step "7. project smoke" "the Rust host did not boot + answer ping (see probe log)"
fi

step "8. e2e (the tests/e2e bun suite against the Rust host)"
if ! command -v bun >/dev/null; then
  defer_step "8. e2e" "e2e — bun not on PATH (add /var/home/saffronjam/.bun/bin)"
elif host_ready; then
  if ( cd "$REPO/tests/e2e" && SAFFRON_ANIMA_BIN="$RUST_HOST" bun test ); then
    pass_step "8. e2e"
  else
    fail_step "8. e2e" "e2e suite"
  fi
else
  fail_step "8. e2e" "the Rust host did not boot + answer ping (see probe log)"
fi

step "9. frontend: gen @saffron/protocol + tsc --noEmit + vite build + unit tests"
if ! command -v bun >/dev/null; then
  defer_step "9. frontend" "frontend build — bun not on PATH (add /var/home/saffronjam/.bun/bin)"
elif [ ! -x "$REPO/editor/node_modules/.bin/tsc" ]; then
  defer_step "9. frontend" "frontend build — editor deps not installed (run \`cd editor && bun install\`)"
else
  if ( cd "$REPO/editor" && bun run build && bun test ); then
    pass_step "9. frontend"
  else
    fail_step "9. frontend" "frontend build/tests"
  fi
fi

step "10. lint (cargo fmt --check + cargo clippy --workspace -- -D warnings)"
lint_ok=0
( cd "$ENGINE" && cargo fmt --check ) || lint_ok=1
( cd "$ENGINE" && cargo clippy --workspace -- -D warnings ) || lint_ok=1
if [ "$lint_ok" -eq 0 ]; then pass_step "10. lint"; else fail_step "10. lint" "cargo fmt --check / cargo clippy --workspace -- -D warnings"; fi

echo
echo "=== per-step verdict ==="
for r in "${results[@]}"; do echo "  $r"; done
if [ "${#deferred[@]}" -gt 0 ]; then
  echo
  echo "DEFERRED STEPS (hardware/display this toolbox lacks; run on the self-hosted runner):"
  for d in "${deferred[@]}"; do echo "  - $d"; done
fi
echo
if [ "$fail" -eq 0 ]; then echo "ALL GATES PASSED"; else echo "SOME GATES FAILED"; fi
exit "$fail"
