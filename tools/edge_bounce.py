#!/usr/bin/env python3
"""Bounce stress for the edge_test firmware: does a burst of fast transitions
leave the chip deaf to later edges?

For each idle mode: boot with immediate re-arm (the firmware goes back to sleep
at once, whatever the pin level), send bursts of 0x55 bytes on PA9 (each byte is
five low pulses one bit time long, one bit time apart), then send N clean,
well-separated 833 us pulses and check that exactly N more edges are counted.
The bursts themselves are not expected to be counted one for one.

usage: edge_bounce.py [N=20]
env:   CAPS=0,1   idle modes (bits 11:8 of BTN_CFG; 0 STANDBY1, 1 STANDBY0)
       FW=0       1: the firmware sets FASTWAKE itself; 0: leave it to the HAL
"""
import os, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools", "unwedge"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import unwedge, uart_pulse
from edge_matrix_common import with_dap

ELF = os.path.join(ROOT, "firmware/target-notimer/thumbv6m-none-eabi/release/edge_test")
N = int(sys.argv[1]) if len(sys.argv) > 1 else 20
CAPS = [int(x) for x in os.environ.get("CAPS", "0,1").split(",")]
FW = int(os.environ.get("FW", "0"))
NAMES = {0: "STANDBY1", 1: "STANDBY0", 2: "STOP2", 3: "STOP0", 4: "WFI"}
BIT_US = [9, 20, 30, 45, 60, 75, 90, 120, 200, 500]   # 3 LFCLK cycles = 92 us


def peek():
    return with_dap(unwedge.peek, ELF, True)


for cap in CAPS:
    word = (cap << 8) | 0x002 | FW
    with_dap(unwedge.set_cfg, ELF, word)
    t0 = time.time()
    while True:
        time.sleep(1.0)
        r = peek()
        if (r["ok"] and r["mode"] == word) or time.time() - t0 > 30:
            break
    time.sleep(1.5)
    for bit in BIT_US:
        before = peek()
        fd = uart_pulse.open_uart(round(1e6 / bit))
        time.sleep(0.05)
        for _ in range(20):
            os.write(fd, b"\x55")
            time.sleep(0.05)
        os.close(fd)
        time.sleep(0.2)
        mid = peek()
        uart_pulse.pulses(N, 1200, 0xFF, 0.15)
        time.sleep(0.3)
        after = peek()
        got = after["uart"] - mid["uart"]
        print("%-9s bursts of 5 x %3d us: %3d of 100 burst edges counted; then %d/%d clean pulses%s"
              % (NAMES[cap], bit, mid["uart"] - before["uart"], got, N, "" if got == N else "   <-- LOST"), flush=True)
