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
#   4b. draw-path tripwire       no retired CPU draw-list / meshlet / raster-toggle symbol
#   4c. front-door assertion     every Pipelines::request_* PSO has a caller outside test code
#   5. present-only smoke        SAFFRON_EXIT_AFTER_FRAMES=5 + validation-clean log grep
#   6. control-schema contract   check-control-schema/check.ts against the live host
#   7. project startup smoke     check-projects/check.sh against the live host
#   8. performance budgets       bench-foliage-phase1/check.ts vs this device's recorded ceilings
#   9. e2e                       tsc --noEmit over every TypeScript program, then the bun suite
#                                against the host
#  10. frontend                  editor/ bun run build + bun test
#  11. lint                      cargo fmt --check + cargo clippy --workspace -- -D warnings +
#                                oxfmt --check + oxlint --deny-warnings over every .ts/.tsx
#
# A step whose prerequisite this environment lacks DEFERS with a reason instead of failing.
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENGINE="$REPO/engine"
cd "$REPO"
. "$REPO/tools/gpu-driver.sh"
fail=0
declare -a results=()
deferred=()

step() { echo; echo "=== $* ==="; }

fail() { echo "FAILED: $*" >&2; fail=1; }

defer() { echo "DEFERRED: $*"; deferred+=("$*"); }

pass_step() { results+=("PASS  $1"); }
fail_step() { results+=("FAIL  $1"); fail "$2"; }
defer_step() { results+=("DEFER $1"); defer "$2"; }

RUST_HOST="${SAFFRON_ANIMA_BIN:-$ENGINE/target/debug/saffron-host}"
RUST_SA="${SAFFRON_SA_BIN:-$ENGINE/target/debug/sa}"

# Probed once and cached; steps 5-8 gate on it. A false answer is a failure, never a defer:
# the build step produced the binary, so a host that will not answer ping is a regression.
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

# The CEF editor shell is a standalone crate outside the engine workspace; its build.rs links the
# version-locked libcef, so an absent provisioning defers.
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

# Every line of a Rust tree that ships, as `file:line: text`: the test-only files (`tests.rs`, a
# `tests/` directory) are dropped whole, and a `#[cfg(test)]` attribute hides the one line it
# annotates when that line is a complete item (a `mod tests;` declaration, a test-only `use`) and
# the rest of the file otherwise — which is where the convention puts an inline test module. The
# steps below ask what production reaches, so a fixture's line must never answer for it. Build
# output is excluded under both spellings: a private `CARGO_TARGET_DIR=engine/target-<name>` is the
# convention for building off the shared cache, and its generated sources are not this tree's.
production_lines() {
  find "$1" -name '*.rs' -not -path '*/target/*' -not -path '*/target-*/*' -print0 | xargs -0 awk '
    FNR == 1 { intest = (FILENAME ~ /\/tests\// || FILENAME ~ /tests\.rs$/); pending = 0 }
    pending { pending = 0; if ($0 ~ /;[ \t]*$/) next; intest = 1; next }
    /^[ \t]*#\[cfg\(test\)\]/ { pending = 1; next }
    !intest { print FILENAME ":" FNR ": " $0 }
  '
}

step "4. self-test-removal assertion (no runtime run*SelfTest / SAFFRON_SELFTEST / fn *self_test)"
# Any of the three patterns outside a `#[cfg(test)]` module is a runtime self-test and fails. A
# commented-out mention is prose, not a self-test.
selftest_hits="$(
  production_lines "$ENGINE" |
    grep -E 'run[A-Za-z]+SelfTest|SAFFRON_SELFTEST|fn [A-Za-z0-9_]*self_test' |
    grep -vE ':[0-9]+:[[:space:]]*//' || true
)"
if [ -z "$selftest_hits" ]; then
  pass_step "4. self-test-removal assertion"
else
  echo "$selftest_hits" >&2
  fail_step "4. self-test-removal assertion" "a runtime self-test survives outside #[cfg(test)] (see above)"
fi

step "4b. draw-path tripwire (one production draw path, no retired symbol survives)"
# The persistent GPU scene's visibility traversal binned into counted-indirect executor draws is
# the ONE production draw path. Each pattern names something the engine must not contain, matched
# by class rather than by spelling: a `DrawList` type or a `<verb>_draw_list` function (a CPU draw
# gather, a batcher, or a per-pass recorder of either), a per-instance meshlet loop, the
# `SAFFRON_MESH_SHADER` raster toggle (the mesh executor is selected by the device's capabilities,
# with `set-mesh-executor` to switch a running host), and any executor depth PSO or shader beside
# the übershader's own depth pre-pass.
# `crate::draw_list` — the module holding the frame's deformation state, resolved material
# vocabulary, and render counters — is the one bare occurrence the class patterns deliberately do
# not match. A hit in engine source or in the shader tree fails the gate.
tripwire_hits="$(
  grep -rnE 'DrawItem|DrawBatch|[A-Za-z]*DrawList|[a-z]+_draw_list|SAFFRON_MESH_SHADER|MeshletRaster|record_meshlet_draws|scene_executor_depth' \
    "$ENGINE/crates" --include='*.rs' 2>/dev/null || true
  # The shader pair by filename as well as by reference: a resurrected entry point arrives as a
  # new source file, and its name is the only thing a content grep would miss.
  find "$ENGINE/assets/shaders" -name 'scene_executor_depth*' -print 2>/dev/null || true
  grep -rnE 'scene_executor_depth' "$ENGINE/assets/shaders" 2>/dev/null || true
)"
if [ -z "$tripwire_hits" ]; then
  pass_step "4b. draw-path tripwire"
else
  echo "$tripwire_hits" >&2
  fail_step "4b. draw-path tripwire" "a retired CPU draw-path symbol survives (see above)"
fi

step "4c. render-path front-door assertion (no PSO exists only for a test)"
# A `Pipelines::request_*` front door whose only callers are fixtures is a second render path
# built to make one test render, next to the production path it duplicates. A door with no
# production caller fails the gate: either wire it into a pass or delete it with its shader.
frontdoor_corpus="$(mktemp)"
production_lines "$ENGINE/crates" >"$frontdoor_corpus"
frontdoor_hits=""
for door in $(grep -hoE 'pub fn request_[a-z0-9_]+' "$ENGINE"/crates/rendering/src/pipelines/*.rs |
  sed 's/pub fn //' | sort -u); do
  if ! grep -qE "(\.|Pipelines::)$door\(" "$frontdoor_corpus"; then
    frontdoor_hits="${frontdoor_hits}Pipelines::$door has no caller outside test code
"
  fi
done
rm -f "$frontdoor_corpus"
if [ -z "$frontdoor_hits" ]; then
  pass_step "4c. render-path front-door assertion"
else
  printf '%s' "$frontdoor_hits" >&2
  fail_step "4c. render-path front-door assertion" "a PSO front door is reachable only from tests (see above)"
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
  # The grep IS the validation-clean gate: a wrong barrier or layout mismatch corrupts no wire
  # byte and throws nothing, so a dirty log is its only automated detector.
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

step "8. performance budgets (re-measure the phase-1 fixture against this device's record)"
# Runs ahead of the e2e suite so the measurement is taken on a machine this gate has not yet
# loaded. It grades only the device it is running on: a checkout on hardware with no record in
# benchmarks/foliage-veg/ defers rather than borrowing another class's ceiling.
if ! command -v bun >/dev/null; then
  defer_step "8. performance budgets" "performance budgets — bun not on PATH (add /var/home/saffronjam/.bun/bin)"
elif ! host_ready; then
  fail_step "8. performance budgets" "the Rust host did not boot + answer ping (see probe log)"
else
  budget_log="/tmp/sa-ci-budget-$$.log"
  ( cd "$REPO" && SAFFRON_ANIMA_BIN="$RUST_HOST" bun tools/bench-foliage-phase1/check.ts ) 2>&1 |
    tee "$budget_log"
  budget_status=${PIPESTATUS[0]}
  case "$budget_status" in
    0) pass_step "8. performance budgets" ;;
    2) defer_step "8. performance budgets" \
      "performance budgets — $(grep -m1 '^DEFER: ' "$budget_log" | cut -d' ' -f2-)" ;;
    *) fail_step "8. performance budgets" "phase-1 budgets (see the graded legs above)" ;;
  esac
  rm -f "$budget_log"
fi

step "9. e2e (tsc --noEmit over every TypeScript program, then the tests/e2e bun suite against the Rust host)"
if ! command -v bun >/dev/null; then
  defer_step "9. e2e" "e2e — bun not on PATH (add /var/home/saffronjam/.bun/bin)"
else
  rm -f /tmp/saffron-e2e-*.sock 2>/dev/null || true
  # `bun test` strips types without checking them, so the suite's assertions only stay bound to the
  # generated @saffron/protocol types while `tsc` runs over them here. It needs no host, so it runs
  # ahead of the boot probe and reports a type error even when nothing can execute. The same run
  # covers tools/, packager/, and the editor app — every .ts/.tsx in the tree belongs to one of the
  # two programs.
  if ! ( cd "$REPO" && bun install --frozen-lockfile ); then
    fail_step "9. e2e" "workspace dependency install (bun install --frozen-lockfile)"
  elif ! ( cd "$REPO" && bun run typecheck ); then
    fail_step "9. e2e" "TypeScript typecheck"
  elif ! host_ready; then
    fail_step "9. e2e" "the Rust host did not boot + answer ping (see probe log)"
  elif ( cd "$REPO/tests/e2e" && SAFFRON_ANIMA_BIN="$RUST_HOST" bun test --timeout 30000 --max-concurrency 4 ); then
    pass_step "9. e2e"
  else
    fail_step "9. e2e" "e2e suite"
  fi
fi

step "10. frontend: gen @saffron/protocol + tsc --noEmit + vite build + unit tests"
if ! command -v bun >/dev/null; then
  defer_step "10. frontend" "frontend build — bun not on PATH (add /var/home/saffronjam/.bun/bin)"
elif [ ! -x "$REPO/node_modules/.bin/tsc" ]; then
  defer_step "10. frontend" "frontend build — workspace deps not installed (run \`bun install\`)"
else
  if ( cd "$REPO/editor" && bun run build && bun test ); then
    pass_step "10. frontend"
  else
    fail_step "10. frontend" "frontend build/tests"
  fi
fi

step "11. lint (cargo fmt --check + cargo clippy -D warnings + oxfmt --check + oxlint over all TypeScript)"
lint_ok=0
( cd "$ENGINE" && cargo fmt --check ) || lint_ok=1
( cd "$ENGINE" && cargo clippy --workspace -- -D warnings ) || lint_ok=1
if ! command -v bun >/dev/null; then
  defer "11. lint (TypeScript half) — bun not on PATH (add /var/home/saffronjam/.bun/bin)"
else
  ( cd "$REPO" && bun install --frozen-lockfile && bun run format:check ) || lint_ok=1
  ( cd "$REPO" && bun run lint ) || lint_ok=1
fi
if [ "$lint_ok" -eq 0 ]; then pass_step "11. lint"; else fail_step "11. lint" "cargo fmt --check / cargo clippy --workspace -- -D warnings / oxfmt --check / oxlint"; fi

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
