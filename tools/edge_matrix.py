#!/usr/bin/env python3
"""Automated GPIO edge-wake matrix for the edge_test firmware.

For every idle mode x FASTWAKE setting x pulse shape: set the mode over SWD,
reset, wait until the firmware is idle, send N pulses on PA9 from the XDS110
backchannel UART, then read the counter once. Each cell starts from a fresh
reset and is read exactly once, after the pulses, so the readout (which
briefly forces the chip active) cannot influence the result.

usage: edge_matrix.py [N=20]      (edge_test must already be flashed)
"""
import json, os, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools", "unwedge"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import unwedge, uart_pulse
from edge_matrix_common import with_dap

ELF = os.path.join(ROOT, "firmware/target-notimer/thumbv6m-none-eabi/release/edge_test")
N = int(sys.argv[1]) if len(sys.argv) > 1 else 20
CAPS = [(4, "WFI (control)"), (3, "STOP0"), (2, "STOP2"), (1, "STANDBY0"), (0, "STANDBY1 (default)")]
# (label, baud, byte, resleep): 0xFF -> low for 1 bit time; 0x00 -> low for 9 bit times
SHAPES = [("8.7 us", 115200, 0xFF, 0), ("833 us", 1200, 0xFF, 0), ("30 ms", 300, 0x00, 0),
          ("30 ms, re-sleep while low", 300, 0x00, 2)]


def run_cell(word, baud, byte):
    with_dap(unwedge.set_cfg, ELF, word)
    t0 = time.time()
    while True:                                    # wait for the firmware to come up in this mode
        time.sleep(1.0)
        r = with_dap(unwedge.peek, ELF, True)
        if r["ok"] and r["mode"] == word and r["state"] == 1:
            break
        if time.time() - t0 > 30:
            return None
    time.sleep(1.5)                                # let it settle into its idle mode, untouched
    uart_pulse.pulses(N, baud, byte, 0.15)
    time.sleep(0.3)
    return with_dap(unwedge.peek, ELF, True)


rows, results = [], []
for cap, capname in CAPS:
    for fw in (0, 1):
        cells = []
        for label, baud, byte, resleep in SHAPES:
            word = (cap << 8) | fw | resleep
            r = run_cell(word, baud, byte)
            got = None if r is None else r["uart"]
            results.append(dict(cap=capname, fastwake=bool(fw), shape=label, sent=N, counted=got,
                                asleep=None if r is None else r["gated"],
                                ris9=None if r is None else (r["RIS"] >> 9) & 1))
            cells.append("boot?" if got is None else "%d/%d" % (got, N))
            print("%-20s fastwake=%d  %-26s -> %s%s" % (capname, fw, label, cells[-1],
                  "" if r is None else "   (AHB-AP %s after)" % ("gated" if r["gated"] else "reachable")), flush=True)
        rows.append("| %s | %s | %s |" % (capname, "on" if fw else "off", " | ".join(cells)))

print("\n| Idle mode | FASTWAKE | " + " | ".join(s[0] for s in SHAPES) + " |")
print("|---|---|" + "---|" * len(SHAPES))
print("\n".join(rows))
json.dump(results, open(os.path.join(ROOT, "tools/energytrace/captures/edge_matrix.json"), "w"), indent=1)
