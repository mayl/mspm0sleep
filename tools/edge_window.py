#!/usr/bin/env python3
"""Look for the check-then-arm window in the edge_test re-arm strategies.

STANDBY1 + FASTWAKE, interrupt re-armed immediately after each falling edge.
Pulse width is swept finely so the pin's return to high lands at different
points in the firmware's handling. A strategy with a window between reading
the pin and arming the interrupt should lose pulses at some width.

usage: edge_window.py [N=50]
"""
import os, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools", "unwedge"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import unwedge, uart_pulse
from edge_matrix_common import with_dap

ELF = os.path.join(ROOT, "firmware/target-notimer/thumbv6m-none-eabi/release/edge_test")
N = int(sys.argv[1]) if len(sys.argv) > 1 else 50
NAMES = {2: "check pin, then arm rising (wait_for_high)", 3: "arm both edges", 4: "arm, then check pin"}
WIDTHS_US = [9, 12, 15, 18] + list(range(20, 81, 2)) + list(range(85, 201, 5)) + [250, 300, 400, 500, 833]


def boot(word):
    with_dap(unwedge.set_cfg, ELF, word)
    t0 = time.time()
    while True:
        time.sleep(1.0)
        r = with_dap(unwedge.peek, ELF, True)
        if (r["ok"] and r["mode"] == word) or time.time() - t0 > 30:
            return r


for strat in (2, 4, 3):
    word = 0x003 | (strat << 12)
    boot(word)
    time.sleep(1.5)
    prev = with_dap(unwedge.peek, ELF, True)
    bad = []
    for w in WIDTHS_US:
        baud = round(1e6 / w)
        fd = uart_pulse.open_uart(baud)
        time.sleep(0.05)
        for _ in range(N):
            os.write(fd, b"\xff")
            time.sleep(0.02)
        os.close(fd)
        time.sleep(0.1)
        r = with_dap(unwedge.peek, ELF, True)
        got, wakes = r["uart"] - prev["uart"], r["wakes"] - prev["wakes"]
        if got != N or r["state"] != 1:
            bad.append("%d us: %d/%d counted, %d wakes, state %d" % (w, got, N, wakes, r["state"]))
        prev = r
        if r["state"] != 1:                         # stuck waiting for an edge that already happened: restart
            boot(word); time.sleep(1.5); prev = with_dap(unwedge.peek, ELF, True)
    total = len(WIDTHS_US)
    print("%-45s %d widths x %d pulses: %d widths fully counted" % (NAMES[strat], total, N, total - len(bad)), flush=True)
    for b in bad:
        print("     " + b, flush=True)
