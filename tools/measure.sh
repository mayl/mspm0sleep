#!/usr/bin/env bash
# Flash a firmware (or leave what's there with FW=-), release all debug holds,
# and capture EnergyTrace current.   usage: tools/measure.sh <fw-bin|-> <name> [secs]
# NOSWD=1 skips every step that needs SWD/NRST (for captures taken with the
# LaunchPad's isolation jumpers pulled); the target is then only restarted by
# the probe's own ET_DCDC_RestartMCU at the start of the capture.
# Run inside `nix develop`.
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
FW=$1; NAME=$2; SECS=${3:-10}
U="$ROOT/tools/unwedge"; CAP="$ROOT/tools/energytrace/captures"
"$U/replug.sh"                                   # clear any stalled ICDI state
if [ "$FW" != "-" ]; then
  case "$FW" in /*) IMG="$FW" ;; *) IMG="$ROOT/firmware/target/thumbv6m-none-eabi/release/$FW" ;; esac
  "$ROOT/tools/flash.sh" "$IMG" >/dev/null
fi
[ -n "${NOSWD:-}" ] || "$U/unwedge.py" release
sleep "${SETTLE:-5}"                             # past the firmware's 3 s attach window
LOG="/tmp/et_$NAME.log"
DURATION_SECS=$SECS RAW_OUT="$CAP/$NAME.bin" PLOT_OUT="$CAP/$NAME.png" \
  "$ROOT/tools/energytrace/repro-cli/target/release/repro-cli" > "$LOG" 2>&1 || { tail -5 "$LOG"; exit 1; }
# repro-cli prints "CAL nA_per_pps=<k> x0_pps=<x0>" from its two-point calibration.
read -r K X0 < <(sed -n 's/^CAL nA_per_pps=\([0-9.]*\) x0_pps=\([0-9.]*\)/\1 \2/p' "$LOG")
"$ROOT/tools/energytrace/floor.py" "$CAP/$NAME.bin" "$K" "$X0"
