#!/usr/bin/env bash
# Project feature smoke tests. Requires an existing display because the host opens a
# Vulkan swapchain; tools/ci/check.sh runs this under the same compositor as the engine
# and schema checks. The host + `sa` binaries default to the Rust workspace build; override
# SAFFRON_ANIMA_BIN / SAFFRON_SA_BIN to point at a different build.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENGINE="${SAFFRON_ANIMA_BIN:-$REPO/engine/target/debug/saffron-host}"
SA="${SAFFRON_SA_BIN:-$REPO/engine/target/debug/sa}"
APPDATA="$(mktemp -d /tmp/saffron-projects.XXXXXX)"
PNG="$(mktemp /tmp/saffron-projects-texture.XXXXXX.png)"
ENGINE_PID=""
SOCK=""

# Wait up to ~5s for the host to exit, then force-kill it. Under llvmpipe the host can trip a
# known VMA "allocations not freed" assertion at device teardown and stall, so an unbounded
# `wait` would hang the gate — the control assertions above already prove the engine ran.
reap_engine() {
  [ -n "$ENGINE_PID" ] || return 0
  for _ in $(seq 1 50); do
    kill -0 "$ENGINE_PID" 2>/dev/null || { ENGINE_PID=""; return 0; }
    sleep 0.1
  done
  kill -9 "$ENGINE_PID" 2>/dev/null || true
  wait "$ENGINE_PID" 2>/dev/null || true
  ENGINE_PID=""
}

cleanup() {
  if [ -n "$ENGINE_PID" ] && kill -0 "$ENGINE_PID" 2>/dev/null; then
    SAFFRON_CONTROL_SOCK="$SOCK" "$SA" quit >/dev/null 2>&1 || true
    reap_engine
  fi
  rm -rf "$APPDATA" "$PNG" "$SOCK"
  rm -f \
    /tmp/saffron-projects-engine-$$.log \
    /tmp/saffron-projects-status-$$.json \
    /tmp/saffron-projects-invalid-$$.json \
    /tmp/saffron-projects-model-$$.json \
    /tmp/saffron-projects-texture-$$.json \
    /tmp/saffron-projects-save-$$.json \
    /tmp/saffron-projects-get-$$.json \
    /tmp/saffron-projects-list-$$.json \
    /tmp/saffron-projects-thumb-$$.json
}
trap cleanup EXIT

printf "%s" "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mP4z8AAAAMBAQD3A0FDAAAAAElFTkSuQmCC" | base64 -d > "$PNG"

start_engine() {
  local name="$1"
  SOCK="/tmp/saffron-projects-$name-$$.sock"
  rm -f "$SOCK"
  SAFFRON_CONTROL_SOCK="$SOCK" \
    SAFFRON_APPDATA_DIR="$APPDATA" \
    SAFFRON_PROJECT="$name" \
    "$ENGINE" >/tmp/saffron-projects-engine-$$.log 2>&1 &
  ENGINE_PID=$!
  for _ in $(seq 1 80); do
    if [ -S "$SOCK" ]; then
      if SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json project-status \
        >/tmp/saffron-projects-status-$$.json 2>/dev/null; then
        if grep -q '"phase": "ready"' /tmp/saffron-projects-status-$$.json; then
          return 0
        fi
        if grep -q '"phase": "failed"' /tmp/saffron-projects-status-$$.json; then
          cat /tmp/saffron-projects-status-$$.json >&2
          cat /tmp/saffron-projects-engine-$$.log >&2
          return 1
        fi
      fi
    fi
    if ! kill -0 "$ENGINE_PID" 2>/dev/null; then
      cat /tmp/saffron-projects-engine-$$.log >&2
      return 1
    fi
    sleep 0.1
  done
  cat /tmp/saffron-projects-engine-$$.log >&2
  return 1
}

stop_engine() {
  SAFFRON_CONTROL_SOCK="$SOCK" "$SA" quit >/dev/null
  # The control assertions above already prove the engine ran and answered; the device-teardown
  # exit is tolerated (and bounded by reap_engine) because llvmpipe trips a known VMA
  # "allocations not freed" assertion at exit.
  reap_engine
}

start_engine asset-test

if SAFFRON_CONTROL_SOCK="$SOCK" "$SA" new-project Bad_Name >/tmp/saffron-projects-invalid-$$.json 2>&1; then
  echo "invalid project name was accepted" >&2
  exit 1
fi

# import-model bakes the glTF into one .smodel container (mesh + materials + textures as chunks);
# import-texture imports a standalone image as a loose texture asset.
SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json import-model "$REPO/engine/assets/models/cube.gltf" >/tmp/saffron-projects-model-$$.json
SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json import-texture "$PNG" >/tmp/saffron-projects-texture-$$.json
SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json save-project >/tmp/saffron-projects-save-$$.json

MODEL_ID="$(grep '"id"' /tmp/saffron-projects-model-$$.json | head -1 | tr -dc '0-9')"
test -n "$MODEL_ID"

PROJECT="$APPDATA/userdata/asset-test/project.json"
test -f "$PROJECT"
grep -q '"name": "asset-test"' "$PROJECT"
grep -q '"displayName": "Asset Test"' "$PROJECT"
grep -q '"type": "model"' "$PROJECT"
find "$APPDATA/userdata/asset-test/assets/models" -name '*.smodel' -print -quit | grep -q .
find "$APPDATA/userdata/asset-test/assets/textures" -type f -print -quit | grep -q .

# A fresh project is scaffolded for VS Code: the LuaLS def file + .luarc.json pointing at it.
test -f "$APPDATA/userdata/asset-test/library/sa.lua"
grep -q '@class sa.Entity' "$APPDATA/userdata/asset-test/library/sa.lua"
test -f "$APPDATA/userdata/asset-test/.luarc.json"
grep -q '"library"' "$APPDATA/userdata/asset-test/.luarc.json"

stop_engine

# Restart: loadProject reads the saved project.json and the filesystem scan rediscovers the .smodel,
# so the model asset (and its renderable thumbnail) survive the round-trip.
start_engine asset-test
SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json get-project >/tmp/saffron-projects-get-$$.json
grep -q '"loaded": true' /tmp/saffron-projects-get-$$.json
SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json list-assets >/tmp/saffron-projects-list-$$.json
grep -q "$MODEL_ID" /tmp/saffron-projects-list-$$.json
SAFFRON_CONTROL_SOCK="$SOCK" "$SA" --output=json get-thumbnail "$MODEL_ID" 32 >/tmp/saffron-projects-thumb-$$.json
grep -q '"base64"' /tmp/saffron-projects-thumb-$$.json
stop_engine

echo "project checks passed"
