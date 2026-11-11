#!/usr/bin/env bash
# Button-wake test helper (firmware/src/lib.rs::button_count, built with no time driver).
#   tools/button_test.sh build              build all btn_* variants
#   tools/button_test.sh flash <variant>    flash btn_<variant>, reset, show initial state
#   tools/button_test.sh read  <variant>    read the press counter over SWD (no halt, no reset)
# variants: wfi stop0 stop2 stop2_fw sb0 sb0_fw sb1 sb1_fw     (run inside `nix develop`)
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
OUT="$ROOT/firmware/target-notimer/thumbv6m-none-eabi/release"
case "${1:-}" in
  build) cd "$ROOT/firmware" && cargo build --release --no-default-features --features lp --target-dir target-notimer ;;
  flash) "$ROOT/tools/flash.sh" "$OUT/btn_$2"; sleep 8; "$ROOT/tools/unwedge/unwedge.py" peek "$OUT/btn_$2" ;;
  read)  "$ROOT/tools/unwedge/unwedge.py" peek "$OUT/btn_$2" ;;
  *) sed -n 2,6p "$0"; exit 1 ;;
esac
