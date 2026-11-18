#!/usr/bin/env bash
# Power-cycle the LaunchPad (probe + target) via its hub port. Also clears a
# stalled XDS110 ICDI interface. Needs uhubctl access to BOTH halves of the
# USB3 hub (2109:2817 and 2109:0817), otherwise VBUS stays up: docs/UNWEDGE.md.
set -euo pipefail
# The hub port is this bench's wiring: L1306 on port 3, C1104 on port 2.
LOC=${HUB_LOC:-1-2.3}; case "${BOARD:-l1306}" in c1104) P=2 ;; *) P=3 ;; esac; PORT=${HUB_PORT:-$P}
uhubctl -l "$LOC" -p "$PORT" -a off >/dev/null
sleep "${1:-3}"
uhubctl -l "$LOC" -p "$PORT" -a on >/dev/null
for _ in $(seq 40); do sleep 0.25; lsusb -d 0451:bef3 >/dev/null && break; done
sleep 1.5
