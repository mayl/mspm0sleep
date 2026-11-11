#!/usr/bin/env bash
# GPIO edge-wake test helper (firmware/src/lib.rs::edge_test, built with no time driver).
#   tools/button_test.sh build            build + flash edge_test
#   tools/button_test.sh cfg <word>       set mode (see BTN_CFG in lib.rs) and reset, then show state
#   tools/button_test.sh read             read counters over SWD (no halt, no reset)
# Run inside `nix develop`. The automated matrix is tools/edge_matrix.py.
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
ELF="$ROOT/firmware/target-notimer/thumbv6m-none-eabi/release/edge_test"
U="$ROOT/tools/unwedge/unwedge.py"
case "${1:-}" in
  build) (cd "$ROOT/firmware" && cargo build --release --no-default-features --features lp --target-dir target-notimer)
         "$ROOT/tools/flash.sh" "$ELF" ;;
  cfg)   "$U" cfg "$ELF" "$2"; sleep 12; "$U" peek "$ELF" ;;
  read)  "$U" peek "$ELF" ;;
  *) sed -n 2,6p "$0"; exit 1 ;;
esac
