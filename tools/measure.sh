#!/usr/bin/env bash
# Flash a firmware (or leave what's there with FW=-), release all debug holds,
# and capture EnergyTrace current.   usage: tools/measure.sh <fw-bin|-> <name> [secs]
# Writes tools/energytrace/captures/<name>.bin (raw stream), .json (calibration)
# and .png, and prints the statistics. Look at a capture with
#   tools/etrace/target/release/etrace view tools/energytrace/captures/<name>.bin
# NOSWD=1 skips every step that needs SWD/NRST (for captures taken with the
# LaunchPad's isolation jumpers pulled).
# A wrapper around `etrace measure` that adds this bench's hub power cycle and
# paths. Run inside `nix develop`.
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
FW=$1; NAME=$2; SECS=${3:-10}
CAP="$ROOT/tools/energytrace/captures"
(cd "$ROOT/tools/etrace" && cargo build --release --quiet)
"$ROOT/tools/unwedge/replug.sh"                  # clear any stalled ICDI state
ARGS=()
if [ "$FW" != "-" ]; then
  case "$FW" in /*) IMG="$FW" ;; *) IMG="$ROOT/firmware/target/thumbv6m-none-eabi/release/$FW" ;; esac
  ARGS+=(--flash "$IMG")
elif [ -n "${NOSWD:-}" ]; then
  ARGS+=(--no-swd)
fi
# --settle: past the firmware's 3 s attach window
BOARD="${BOARD:-l1306}" "$ROOT/tools/etrace/target/release/etrace" measure "$CAP/$NAME.bin" \
  --secs "$SECS" --settle "${SETTLE:-5}" --png "${ARGS[@]}"
