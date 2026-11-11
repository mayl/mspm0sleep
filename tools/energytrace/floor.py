#!/usr/bin/env python3
"""Summarise a repro-cli RAW_OUT capture: mean current and the quiescent floor.

Record grammar (see REVERSE_ENGINEERING.md): [0x70, window_ctr, pulses, flags],
with a lone 0x5c byte at each window-counter wrap. Each window is 100 us.
The floor is reported as low percentiles of 100 ms-binned current so brief
wake-ups / LED pulses don't contaminate it.
"""
import sys

def decode(buf):
    i, last, t, out = 0, None, 0, []          # out: (window_index, pulses)
    while i + 4 <= len(buf):
        if buf[i] != 0x70:
            i += 1
            continue
        ctr, pulses = buf[i + 1], buf[i + 2]
        t += 1 if last is None else ((ctr - last) & 0xFF) or 256
        last = ctr
        out.append((t, pulses))
        i += 4
    return out

def main():
    path = sys.argv[1]
    cal2 = float(sys.argv[2]) if len(sys.argv) > 2 else 10186.0
    recs = decode(open(path, "rb").read())
    total_w = recs[-1][0]
    total_p = sum(p for _, p in recs)
    ua = lambda pulses, windows: pulses / (windows * 1e-4) * 1e6 / cal2 / 1000
    binw = 1000                                 # 100 ms
    bins = [0] * (total_w // binw + 1)
    for t, p in recs:
        bins[t // binw] += p
    cur = sorted(ua(b, binw) for b in bins[1:-1])  # drop partial edge bins
    pct = lambda q: cur[min(len(cur) - 1, int(q * len(cur)))]
    print("%-28s %6.1f s  mean %9.2f uA   floor(100ms bins): min %8.2f  p10 %8.2f  median %8.2f uA   max %9.1f uA" % (
        path.split("/")[-1], total_w * 1e-4, ua(total_p, total_w), cur[0], pct(0.10), pct(0.5), cur[-1]))

if __name__ == "__main__":
    main()
