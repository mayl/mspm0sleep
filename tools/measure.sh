#!/usr/bin/env bash
# Flash a firmware (or leave what's there with FW=-), release all debug holds,
# and capture EnergyTrace current.   usage: tools/measure.sh <fw-bin|-> <name> [secs]
# Writes tools/energytrace/captures/<name>.bin (raw stream), .json (calibration)
# and .png, and prints the statistics. Look at a capture with
#   tools/etrace/target/release/etrace view tools/energytrace/captures/<name>.bin
# NOSWD=1 skips every step that needs SWD/NRST (for captures taken with the
# LaunchPad's isolation jumpers pulled).
# Run inside `nix develop`.
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
FW=$1; NAME=$2; SECS=${3:-10}
U="$ROOT/tools/unwedge"; CAP="$ROOT/tools/energytrace/captures"
ETRACE="$ROOT/tools/etrace/target/release/etrace"
[ -x "$ETRACE" ] || (cd "$ROOT/tools/etrace" && cargo build --release)
"$U/replug.sh"                                   # clear any stalled ICDI state
if [ "$FW" != "-" ]; then
  case "$FW" in /*) IMG="$FW" ;; *) IMG="$ROOT/firmware/target/thumbv6m-none-eabi/release/$FW" ;; esac
  "$ROOT/tools/flash.sh" "$IMG" >/dev/null
fi
[ -n "${NOSWD:-}" ] || "$U/unwedge.py" release
sleep "${SETTLE:-5}"                             # past the firmware's 3 s attach window
"$ETRACE" capture "$CAP/$NAME.bin" --secs "$SECS" --png 2>/dev/null
