#!/usr/bin/env python3
"""Summarise a blinky tap-test capture: flashes that are off the firmware's
0.5 s / 2.0 s timer grid, with their phase within the second.

usage: taps.py capture.bin
"""
import sys
from floor import decode, RATE

recs = decode(open(sys.argv[1], "rb").read())
bw = RATE // 50                                   # 20 ms
bins = [0] * (recs[-1][0] // bw + 1)
for t, p in recs:
    bins[t // bw] += p
on = [i for i, b in enumerate(bins) if b > 8]
st = [i * bw / RATE for i in on if i - 1 not in on]
off = [(b, b - a) for a, b in zip(st, st[1:]) if min(abs(b - a - 2.0), abs(b - a - 0.5)) > 0.04]
print("%.0f s, %d flashes, %d off-grid" % (recs[-1][0] / RATE, len(st), len(off)))
for t, g in off:
    print("  t=%6.2f s  (%.2f s after previous)  phase within second: .%02d" % (t, g, round(t % 1 * 100) % 100))
