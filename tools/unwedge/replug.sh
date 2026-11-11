#!/usr/bin/env bash
# Re-enumerate the LaunchPad's XDS110 via the hub (clears a stalled ICDI
# interface). NOTE: only the USB2 half of the port is switchable with the
# current udev rule, so this does NOT remove power from the board.
set -euo pipefail
LOC=${HUB_LOC:-1-2.3}; PORT=${HUB_PORT:-3}
uhubctl -l "$LOC" -p "$PORT" -a off >/dev/null
sleep "${1:-3}"
uhubctl -l "$LOC" -p "$PORT" -a on >/dev/null
for _ in $(seq 40); do sleep 0.25; lsusb -d 0451:bef3 >/dev/null && break; done
sleep 1.5
