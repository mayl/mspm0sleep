#!/usr/bin/env python3
"""Edge-wake test against the timer-based blinky firmwares (the realistic case:
time driver running, 50 ms timer debounce, chip re-sleeps while the pin is low).

For each lp_blinky* build: flash, wait until idle, send N pulses on PA9 at
irregular intervals, read the counter once.   usage: edge_blinky.py [N=20]
"""
import os, random, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools", "unwedge"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import unwedge, uart_pulse
from edge_matrix_common import with_dap

N = int(sys.argv[1]) if len(sys.argv) > 1 else 20
OUT = os.path.join(ROOT, "firmware/target/thumbv6m-none-eabi/release")
BUILDS = [("lp_blinky", "STANDBY1 (default)", "off"), ("lp_blinky_fastwake", "STANDBY1 (default)", "on"),
          ("lp_blinky_standby0", "STANDBY0", "off"), ("lp_blinky_stop2", "STOP2", "off")]
SHAPES = [("833 us", 1200, 0xFF), ("30 ms", 300, 0x00), ("60 ms", 150, 0x00)]  # 150 baud is the lowest accurate rate
random.seed(1)
rows = []
for name, mode, fw in BUILDS:
    elf = os.path.join(OUT, name)
    cells = []
    for label, baud, byte in SHAPES:
        subprocess.run([os.path.join(ROOT, "tools/flash.sh"), elf], check=True, stdout=subprocess.DEVNULL)
        t0 = time.time()
        while True:
            time.sleep(1.0)
            r = with_dap(unwedge.peek, elf, True)
            if r["ok"] or time.time() - t0 > 30:
                break
        time.sleep(2.0)
        fd = uart_pulse.open_uart(baud)
        time.sleep(0.3)
        for _ in range(N):
            os.write(fd, bytes([byte]))
            time.sleep(random.uniform(0.35, 0.95))     # irregular: not locked to the 1 s time-driver wake
        os.close(fd)
        time.sleep(0.3)
        r = with_dap(unwedge.peek, elf, True)
        cells.append("%d/%d" % (r["uart"], N) if r["ok"] else "boot?")
        print("%-20s fastwake=%-3s %-7s -> %s" % (mode, fw, label, cells[-1]), flush=True)
    rows.append("| %s | %s | %s |" % (mode, fw, " | ".join(cells)))
print("\n| Idle mode | FASTWAKE | " + " | ".join(s[0] for s in SHAPES) + " |\n|---|---|" + "---|" * len(SHAPES))
print("\n".join(rows))
