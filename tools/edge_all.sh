#!/usr/bin/env bash
# Run every GPIO edge-wake test against one edge_test build on one board.
#   BOARD=l1306|c1104 TARGET=<cargo target dir under firmware/> tools/edge_all.sh <label> [window-FW]
# Results: tools/energytrace/captures/edge_matrix_<label>.json and edge_<label>.log.
# window-FW: 0 (default) leaves FASTWAKE to the HAL in the width sweep; 1 lets the firmware set it,
# which is the only way to see anything on an embassy that never sets it.
# Run inside `nix develop`.
set -uo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
L=$1; WFW=${2:-0}; T="$ROOT/tools"; LOG="$T/energytrace/captures/edge_$L.log"
{
  echo "== $L: board ${BOARD:-l1306}, $("$T/unwedge/board.py" elf), $(date -Is)"
  "$T/flash.sh" "$("$T/unwedge/board.py" elf)"
  echo "== matrix";  OUT="edge_matrix_$L.json" python3 "$T/edge_matrix.py" 20
  echo "== width sweep (firmware sets FASTWAKE: $WFW)"; STRATS=0,2 FW=$WFW WIDTHS=9:153:1 python3 "$T/edge_window.py" 50
  echo "== bounce";  FW=0 python3 "$T/edge_bounce.py"
  echo "== hold";    QUIET=1 python3 "$T/edge_hold.py" 5
  echo "== done $(date -Is)"
} 2>&1 | tee "$LOG"
