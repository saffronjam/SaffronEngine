#!/usr/bin/env bash
# Repeats the control-schema contract run to expose an intermittent GPU fault.
#
# One green run says nothing about a fault that appears half the time. This runs the contract test
# N times against a fresh host each time, counting the two symptoms the run can produce — a lost
# device and a watchdog report naming a submission still in flight — and fails if any run trips
# either or exits non-zero. Each run's output is kept until the loop ends, and a tripped run's log
# is printed.
#
#   tools/ci/stress-schema.sh [runs]      # `just stress-schema [runs]` wraps it in the toolbox
set -uo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENGINE="$REPO/engine"
cd "$REPO"
. "$REPO/tools/gpu-driver.sh"

RUNS="${1:-20}"
case "$RUNS" in
  '' | *[!0-9]*) echo "usage: stress-schema.sh [runs]" >&2; exit 2 ;;
esac
[ "$RUNS" -gt 0 ] || { echo "usage: stress-schema.sh [runs]" >&2; exit 2; }

RUST_HOST="${SAFFRON_ANIMA_BIN:-$ENGINE/target/debug/saffron-host}"
RUST_SA="${SAFFRON_SA_BIN:-$ENGINE/target/debug/sa}"
if [ ! -x "$RUST_HOST" ]; then
  echo "no host binary at $RUST_HOST — build it first (just engine)" >&2
  exit 2
fi
command -v bun >/dev/null || { echo "bun is not on PATH" >&2; exit 2; }

logs="$(mktemp -d /tmp/saffron-stress-schema.XXXXXX)"
trap 'rm -rf "$logs"' EXIT

failed=0
losses=0
hangs=0
for run in $(seq 1 "$RUNS"); do
  log="$logs/run-$run.log"
  run_ok=0
  (
    cd "$REPO/tools/check-control-schema" &&
      SAFFRON_ANIMA_BIN="$RUST_HOST" SAFFRON_SA_BIN="$RUST_SA" \
        SAFFRON_CONTROL_SOCK="/tmp/saffron-stress-$$-$run.sock" bun run check.ts
  ) >"$log" 2>&1 || run_ok=1
  run_losses="$(grep -c 'ERROR_DEVICE_LOST' "$log")"
  run_hangs="$(grep -c 'has been in flight' "$log")"
  losses=$((losses + run_losses))
  hangs=$((hangs + run_hangs))
  if [ "$run_ok" -ne 0 ] || [ "$run_losses" -ne 0 ] || [ "$run_hangs" -ne 0 ]; then
    failed=$((failed + 1))
    echo "=== run $run/$RUNS FAILED (exit $run_ok, $run_losses device loss, $run_hangs hang report) ==="
    cat "$log"
  else
    echo "run $run/$RUNS ok"
  fi
done

echo
echo "=== stress summary ==="
echo "  runs                 $RUNS"
echo "  failed runs          $failed"
echo "  ERROR_DEVICE_LOST    $losses"
echo "  watchdog in-flight   $hangs"
if [ "$failed" -eq 0 ]; then
  echo "ALL $RUNS CONTRACT RUNS CLEAN"
  exit 0
fi
echo "$failed OF $RUNS CONTRACT RUNS TRIPPED"
exit 1
