#!/usr/bin/env python3
"""List LED-sized current pulses in a capture: start time and gap to the previous one.

usage: pulses.py capture.bin [threshold_pulses_per_50ms]
Used to see whether events (e.g. a button press) show up immediately or only
on the firmware's timer grid.
"""
import sys
from floor import decode, RATE

recs = decode(open(sys.argv[1], "rb").read())
thr = int(sys.argv[2]) if len(sys.argv) > 2 else 20
bw = RATE // 20                                   # 50 ms
bins = [0] * (recs[-1][0] // bw + 1)
for t, p in recs:
    bins[t // bw] += p
on = [i for i, b in enumerate(bins) if b > thr]
starts = [i * bw / RATE for i in on if i - 1 not in on]
print("%d pulses over %.1f s" % (len(starts), recs[-1][0] / RATE))
prev = None
for s in starts:
    print("  t=%6.2f s%s" % (s, "" if prev is None else "   gap %.2f s" % (s - prev)))
    prev = s
