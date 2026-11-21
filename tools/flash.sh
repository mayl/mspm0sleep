#!/usr/bin/env bash
# Flash an ELF (or a raw .bin at address 0) to the LaunchPad chosen by BOARD
# (l1306 by default, or c1104), coping with a sleeping target, then drop every
# debug hold and reset so it runs untouched. A wrapper around `etrace flash`.
# usage: tools/flash.sh <path-to-elf-or-bin>      (run inside `nix develop`)
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
(cd "$ROOT/tools/etrace" && cargo build --release --quiet)
BOARD="${BOARD:-l1306}" exec "$ROOT/tools/etrace/target/release/etrace" flash "$1"
