#!/usr/bin/env bash
# The part of tools/edge_all.sh after the matrix and the plain width sweep, for a run that was cut short.
#   BOARD=... TARGET=... tools/edge_rest.sh <label>     appends to captures/edge_<label>.log
set -uo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
L=$1; T="$ROOT/tools"; LOG="$T/energytrace/captures/edge_$L.log"
{
  echo "== $L resumed: board ${BOARD:-l1306}, $("$T/unwedge/board.py" elf), $(date -Is)"
  "$T/flash.sh" "$("$T/unwedge/board.py" elf)"
  echo "== width sweep, wait_for_high pattern (firmware sets FASTWAKE: 0)"; STRATS=2 FW=0 WIDTHS=9:153:1 python3 "$T/edge_window.py" 50
  echo "== bounce";  FW=0 python3 "$T/edge_bounce.py"
  echo "== hold";    python3 "$T/edge_hold.py" 5
  echo "== hold, no reads in between"; QUIET=1 python3 "$T/edge_hold.py" 5
  echo "== done $(date -Is)"
} 2>&1 | tee -a "$LOG"
