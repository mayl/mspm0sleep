#!/usr/bin/env bash
# Power-cycle the LaunchPad (probe + target) via its hub port. Also clears a
# stalled XDS110 ICDI interface. Needs uhubctl access to BOTH halves of the
# USB3 hub (2109:2817 and 2109:0817), otherwise VBUS stays up: docs/UNWEDGE.md.
set -euo pipefail
LOC=${HUB_LOC:-1-2.3}; PORT=${HUB_PORT:-3}
uhubctl -l "$LOC" -p "$PORT" -a off >/dev/null
sleep "${1:-3}"
uhubctl -l "$LOC" -p "$PORT" -a on >/dev/null
for _ in $(seq 40); do sleep 0.25; lsusb -d 0451:bef3 >/dev/null && break; done
sleep 1.5
