#!/usr/bin/env python3
"""
Reference FFT analysis of a saved EnergyTrace RAW capture.

This is the ground-truth Python implementation that motivated and validates the
Rust CIC decimator (mspm0sleep-89q) and the planned built-in FFT view
(mspm0sleep-ni3). It re-implements the EtDecoder byte state machine and the
multi-stage CIC in numpy so the spectral content of a capture can be inspected
without rebuilding the Rust binary, and so the Rust FFT port has a tested
reference to match.

Two panels are rendered:
  (top)    FFT of the full-rate per-window pulse stream (Nyquist = SAMPLE_RATE/2)
           — shows the unaliased Σ-Δ DC/DC limit-cycle tone (~1.52 kHz on
             LP-MSPM0L1306) and the Σ-Δ shaped quantisation noise.
  (bottom) FFT of the CIC-decimated current series (Nyquist = SAMPLE_RATE/2R)
           — what the user-facing time-domain plot is built from, including any
             tones folded into baseband by decimation.

Run via the project's nix dev shell + an ad-hoc python+numpy+matplotlib shell:

    nix shell --impure --expr \\
      '(import <nixpkgs> {}).python3.withPackages(ps: with ps; [ numpy matplotlib scipy ])' \\
      --command python3 tools/energytrace/fft_analysis.py \\
      tools/energytrace/captures/blinky_calib.bin --out /tmp/blinky_cic_fft.png

Defaults match repro-cli (SAMPLE_RATE=10000, R=10, ORDER=3, CAL2=10186, CAL1=0).
"""
from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt


# Probe calibration defaults — match repro-cli's DEFAULT_CAL2 and the
# et_calibrate_works memory (LP-MSPM0L1306 reads cal2 ≈ 10186 live).
DEFAULT_CAL2 = 10186.0
DEFAULT_CAL1 = 0.0
# Known DC/DC Σ-Δ limit-cycle fundamental on LP-MSPM0L1306 — annotated only.
SD_TONE_HZ = 1520.0


def decode_pulses(path: Path) -> np.ndarray:
    """Replay the EtDecoder state machine and return a uniform per-window pulse
    vector (zero-filled across any counter jumps), matching EtDecoder.feed in
    repro-cli/src/main.rs. Skips the 8-byte URB timestamp header."""
    data = path.read_bytes()[8:]
    n = len(data)
    pulses: list[int] = []
    prev_b1: int | None = None
    i = 0
    while i + 4 <= n:
        if data[i] != 0x70:
            # 0x5c window-wrap marker or stray byte; advance one to realign.
            i += 1
            continue
        b1 = data[i + 1]
        b2 = data[i + 2]
        if prev_b1 is None:
            step = 1
        else:
            d = (b1 - prev_b1) & 0xff
            step = 256 if d == 0 else d
        if step > 1:
            pulses.extend([0] * (step - 1))
        pulses.append(b2)
        prev_b1 = b1
        i += 4
    return np.asarray(pulses, dtype=np.float64)


def cic_decimate(x: np.ndarray, order: int, rate: int) -> np.ndarray:
    """Vectorised equivalent of the Rust Cic: N cumsum integrators @ input rate,
    decimate by R, N diff combs @ output rate (M=1). Returns the boxcar-
    equivalent per-bin pulse SUM so dividing by bin_s gives pulses/sec."""
    y = x.astype(np.float64).copy()
    for _ in range(order):
        y = np.cumsum(y)
    y = y[rate - 1::rate]
    for _ in range(order):
        y = np.diff(y, prepend=0.0)
    return y / (rate ** (order - 1))


def bin_sum_to_ua(bin_pulse_sum: np.ndarray, fs_in: float, rate: int,
                  cal1: float, cal2: float) -> np.ndarray:
    bin_s = rate / fs_in
    pps = bin_pulse_sum / bin_s
    current_na = pps * 1e6 / cal2 - cal1
    return current_na / 1000.0


def full_rate_to_ua(pulses: np.ndarray, fs_in: float,
                    cal1: float, cal2: float) -> np.ndarray:
    pps = pulses * fs_in
    return (pps * 1e6 / cal2 - cal1) / 1000.0


def fft_db(x: np.ndarray, fs: float) -> tuple[np.ndarray, np.ndarray]:
    """Single-sided amplitude spectrum (dB µA peak), Hann-windowed to suppress
    square-wave-edge leakage."""
    x = x - x.mean()
    w = np.hanning(len(x))
    X = np.fft.rfft(x * w)
    f = np.fft.rfftfreq(len(x), d=1.0 / fs)
    mag = np.abs(X) * 2.0 / w.sum()
    mag[mag < 1e-12] = 1e-12
    return f, 20.0 * np.log10(mag)


def detect_blink_fundamental(cur: np.ndarray, fs: float) -> float | None:
    """Coarse blink-frequency estimate via the autocorrelation peak in the
    plausible blink range [0.5, 20] Hz. Returns None if no clear peak."""
    x = cur - cur.mean()
    if x.std() < 1e-9:
        return None
    c = np.correlate(x, x, mode="full")[len(x) - 1:]
    lo = max(1, int(fs / 20.0))
    hi = min(len(c) - 1, int(fs / 0.5))
    if hi <= lo + 1:
        return None
    peak = lo + int(np.argmax(c[lo:hi]))
    return fs / peak if peak > 0 else None


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    p.add_argument("capture", type=Path, help="RAW_OUT .bin capture path")
    p.add_argument("--sample-rate", type=float, default=10_000.0,
                   help="Window rate the probe was configured for (default 10000 Hz)")
    p.add_argument("--rate", "-R", type=int, default=10,
                   help="CIC decimation factor (default 10 → 1 kHz output)")
    p.add_argument("--order", "-N", type=int, default=3,
                   help="CIC order, sinc^N (default 3)")
    p.add_argument("--cal1", type=float, default=DEFAULT_CAL1)
    p.add_argument("--cal2", type=float, default=DEFAULT_CAL2)
    p.add_argument("--out", type=Path, default=Path("/tmp/et_fft.png"),
                   help="Output PNG path (default /tmp/et_fft.png)")
    args = p.parse_args()

    pulses = decode_pulses(args.capture)
    elapsed_s = len(pulses) / args.sample_rate
    print(f"  capture           : {args.capture}")
    print(f"  decoded windows   : {len(pulses)}  ({elapsed_s:.3f} s)")
    print(f"  total pulses      : {int(pulses.sum())}")

    cur_full = full_rate_to_ua(pulses, args.sample_rate, args.cal1, args.cal2)
    binned = cic_decimate(pulses, args.order, args.rate)
    cur_dec = bin_sum_to_ua(binned, args.sample_rate, args.rate, args.cal1, args.cal2)
    fs_dec = args.sample_rate / args.rate
    print(f"  decimated samples : {len(cur_dec)}  @ {fs_dec:.1f} Hz "
          f"(sinc^{args.order}, R={args.rate})")
    print(f"  mean full / dec   : {cur_full.mean():.2f} / {cur_dec.mean():.2f} µA")

    f_full, db_full = fft_db(cur_full, args.sample_rate)
    f_dec, db_dec = fft_db(cur_dec, fs_dec)

    fig, (a1, a2) = plt.subplots(2, 1, figsize=(12, 7))

    a1.semilogx(f_full[1:], db_full[1:], lw=0.6, color="C0")
    a1.set_xlim(1, args.sample_rate / 2)
    a1.set_ylim(-60, 80)
    a1.set_title(
        f"Full-rate per-window current — FFT  "
        f"(fs = {args.sample_rate:.0f} Hz, Nyquist {args.sample_rate/2:.0f} Hz)"
    )
    a1.set_xlabel("Frequency [Hz]")
    a1.set_ylabel("Amplitude [dB µA]")
    a1.grid(True, which="both", alpha=0.3)

    a2.semilogx(f_dec[1:], db_dec[1:], lw=0.7, color="C3")
    a2.set_xlim(1, fs_dec / 2)
    a2.set_ylim(-60, 80)
    a2.set_title(
        f"CIC-decimated current (sinc^{args.order}, R={args.rate}) — FFT  "
        f"(fs = {fs_dec:.0f} Hz, Nyquist {fs_dec/2:.0f} Hz)"
    )
    a2.set_xlabel("Frequency [Hz]")
    a2.set_ylabel("Amplitude [dB µA]")
    a2.grid(True, which="both", alpha=0.3)

    # Annotate the load fundamental if a clear periodicity is present.
    f_blink = detect_blink_fundamental(cur_dec, fs_dec)
    if f_blink is not None:
        for n in range(1, 20):
            for ax in (a1, a2):
                if n * f_blink < ax.get_xlim()[1]:
                    ax.axvline(n * f_blink, color="k", alpha=0.08, lw=1)
        a1.annotate(
            f"load f₀ ≈ {f_blink:.2f} Hz (+ harmonics)",
            xy=(f_blink, 60), xytext=(2, 70),
            fontsize=9, color="k", alpha=0.6,
        )

    # Σ-Δ limit-cycle tone (full-rate) and its decimation alias.
    if SD_TONE_HZ < args.sample_rate / 2:
        a1.axvline(SD_TONE_HZ, color="g", alpha=0.5, lw=1.5)
        a1.annotate(
            f"Σ-Δ tone ≈ {SD_TONE_HZ:.0f} Hz",
            xy=(SD_TONE_HZ, 50), xytext=(SD_TONE_HZ * 1.05, 60),
            fontsize=9, color="g",
        )
    # Folded image of an out-of-band tone in an fs_dec-sampled spectrum.
    aliased = abs(((SD_TONE_HZ + fs_dec / 2) % fs_dec) - fs_dec / 2)
    if 0 < aliased < fs_dec / 2:
        a2.axvline(aliased, color="g", alpha=0.5, lw=1.5)
        a2.annotate(
            f"Σ-Δ aliased to {aliased:.0f} Hz",
            xy=(aliased, 30), xytext=(aliased * 0.4, 50),
            fontsize=9, color="g",
        )

    plt.tight_layout()
    plt.savefig(args.out, dpi=120)
    print(f"  wrote {args.out}")


if __name__ == "__main__":
    main()
