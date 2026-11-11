#!/usr/bin/env bash
# Flash an ELF (or a raw .bin at address 0) to the LP-MSPM0L1306, coping with a
# sleeping target, then drop every debug hold and reset so it runs untouched.
# usage: tools/flash.sh <path-to-elf-or-bin>      (run inside `nix develop`)
set -euo pipefail
ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
U="$ROOT/tools/unwedge"; IMG=("$1")
case "$1" in *.bin) IMG+=(--binary-format bin --base-address 0) ;; esac
for _ in 1 2 3; do                                # the probe occasionally fails its first CMSIS-DAP command
  "$U/unwedge.py" wake >/dev/null
  if probe-rs download --chip MSPM0L1306 --protocol swd "${IMG[@]}" 2>&1 | grep -q Finished; then
    "$U/unwedge.py" release
    exit 0
  fi
  sleep 1
done
echo "flash failed" >&2; exit 1
