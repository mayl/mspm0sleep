#!/usr/bin/env python3
"""Summarise a repro-cli RAW_OUT capture: mean current and the quiescent floor.

Record grammar (THEIA_VERIFICATION.md, DecoderStateMachineMSP432):
  0x34 <rate u16>          config record; the next 10 pulse records are dropped
  0x5c                     heartbeat at each tick-counter wrap
  0x70 <tick u8> <pulses u16 LE>   pulse record; one tick = 1/rate s
Current uses TI's two-point fit: I = (pulses_per_s - x0) * k, with k (nA per
pulse/s) and x0 (open-load pulses/s) from the calibration repro-cli prints.

usage: floor.py capture.bin [k_nA_per_pps x0_pps]
The floor is reported as low percentiles of 100 ms-binned current so brief
wake-ups / LED pulses don't contaminate it.
"""
import sys

RATE = 10000


def decode(buf):
    i, last, t, out, discard = 0, None, 0, [], 10   # out: (tick_index, pulses)
    while i + 4 <= len(buf):
        if buf[i] == 0x34:
            discard = 10
            i += 3
            continue
        if buf[i] != 0x70:
            i += 1
            continue
        ctr = buf[i + 1]
        pulses = buf[i + 2] | (buf[i + 3] << 8)
        t += 1 if last is None else ((ctr - last) & 0xFF) or 256
        last = ctr
        if discard:
            discard -= 1
            pulses = 0
        out.append((t, pulses))
        i += 4
    return out


def main():
    path = sys.argv[1]
    k = float(sys.argv[2]) if len(sys.argv) > 2 else 1e6 / 10186.0
    x0 = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0
    recs = decode(open(path, "rb").read())
    total_t = recs[-1][0]
    total_p = sum(p for _, p in recs)
    ua = lambda pulses, ticks: max(0.0, (pulses / (ticks / RATE) - x0) * k) / 1000
    binw = RATE // 10                            # 100 ms
    bins = [0] * (total_t // binw + 1)
    for t, p in recs:
        bins[t // binw] += p
    cur = sorted(ua(b, binw) for b in bins[1:-1])  # drop partial edge bins
    pct = lambda q: cur[min(len(cur) - 1, int(q * len(cur)))]
    print("%-26s %5.1f s  mean %8.1f uA | 100 ms bins: min %7.1f  p10 %7.1f  median %7.1f  max %8.1f uA | floor %.0f pulses/s" % (
        path.split("/")[-1], total_t / RATE, ua(total_p, total_t), cur[0], pct(0.10), pct(0.5), cur[-1],
        sorted(bins[1:-1])[len(bins) // 10] * 10))


if __name__ == "__main__":
    main()
