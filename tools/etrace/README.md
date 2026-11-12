# etrace

Current measurement with the XDS110 debug probe on a TI LaunchPad, without
Code Composer Studio. Watch the current live as a terminal oscilloscope, or
record it headless and get statistics.

```
 etrace  LIVE   XDS110  10000 ticks/s  488.9 nA per pulse/s, offset 15.7 pulses/s
5 mA   │
       │                                  ⢀⠤⡄                                         ⢠⢤
3.75 mA│                                  ⡇ ⡇                                         ⢸⢸
       │                                  ⡇ ⡇                                         ⡇ ⡇
2.5 mA │                                  ⡇ ⢸                                         ⡇ ⡇
       │                                 ⢸  ⢸                                         ⡇ ⡇
1.25 mA│                                 ⢸  ⢸                                        ⢸  ⢱
       │                                 ⢸  ⠘⡄                                       ⢸  ⢸
0      │⡠⡠⡠⡠⣀⢄⢄⢄⢄⢄⢄⢄⢄⢄⠤⡠⡠⡠⡠⡠⡠⡠⡠⡠⣀⢄⢄⢄⢄⢄⢄⢄⢄⢼   ⡧⡠⡠⡠⡠⡠⡠⣀⢄⢄⢄⢄⢄⢄⢄⢄⢄⠤⡠⡠⡠⡠⡠⡠⡠⡠⡠⣀⢄⢄⢄⢄⡠⡠⡠⡠⡠⣀⠤⡠⡸  ⠸⣀⢄⢄⠤⡠⡠⡠⡠⡠⡠⡠⡠⡠⣀⢄⢄⢄
       └──────────────────────────────────────────────────────────────────────────────────────────────
   0.8 s                                           3.3 s                                        5.8 s
 now 2.1 µA (1 s)   window: mean 168 µA  min 0.00 µA  max 4.128 mA  2.77 mJ   total: 5.8 s  mean 216 µA
 span 5 s  24 ms per point  one pulse = 20.4 µA   Y auto linear
 space pause  +/- span  ←/→ pan  ↑/↓ Y zoom  a auto Y  g log  s smooth  f fit  r reset totals  q quit
```

(An MSPM0L1306 asleep at 2 µA, waking to flash an LED every 2 s.)

## What you need

- A LaunchPad whose on-board XDS110 supports EnergyTrace. Tested on the
  LP-MSPM0L1306 (probe hardware `0x28`, firmware 3.0.0.34). Hardware `0x2d`
  uses the same calibration load and should work; other versions are refused.
- Permission to open the probe's USB device (`0451:bef3`), for example a udev
  rule with `MODE="0666"`.
- `libusb-1.0` and `pkg-config` to build.

## Build and run

```bash
# with Nix, from the repository root
nix run .#etrace -- live

# or with cargo (inside `nix develop`, or anywhere libusb-1.0 is installed)
cd tools/etrace && cargo build --release
./target/release/etrace live
```

## Commands

| Command | What it does |
|---|---|
| `etrace live` | Live oscilloscope. `--save run.bin` records at the same time. |
| `etrace view run.bin` | The same oscilloscope over a saved capture. |
| `etrace capture run.bin --secs 20` | Headless capture, then one line of statistics. Without `--secs` it runs until Ctrl-C. `--png`, `--svg` write a plot next to it. |
| `etrace stats a.bin b.bin` | Statistics for saved captures. `--json` for scripts. |
| `etrace export run.bin --csv out.csv` | CSV, SVG or PNG from a capture. `--bin-ms` sets the time resolution. |
| `etrace cal` | Run the probe's calibration and print it. |

`etrace view` and `etrace live` take `--span SECONDS` and `--log` to set the
starting view, and `--dump 120x30` to print one frame as text instead of
opening the display (for logs, or for a quick look over a dumb terminal).
`etrace live --replay run.bin` plays a capture back in real time.

A statistics line looks like this:

```
run.bin: 20.0 s  mean 217 µA  |  100 ms windows: min 1.9 µA  p10 1.9 µA  median 1.9 µA  max 3.338 mA  (step 4.9 µA)  |  floor 20 pulses/s  |  14306.3 µJ
```

The mean is over the whole capture. The percentiles are over 100 ms windows
(`--bin-ms`), so `p10` and `median` describe the quiescent level even when the
target wakes up now and then.

## Oscilloscope keys

| Key | |
|---|---|
| `space` | pause and resume (data keeps arriving while paused) |
| `+` `-` | shorter or longer time span, 100 ms to 2 h |
| `←` `→` | pan; with Shift, by a whole span. Panning to the newest data resumes |
| `Home` `End` | oldest and newest data |
| `↑` `↓` | zoom the current axis in and out; `a` returns to auto-ranging |
| `g` | logarithmic current axis (0.1 µA at the bottom) |
| `s` | smoothing: average each point over 4, 16 or 64 points |
| `f` | fit the whole trace |
| `r` | restart the totals |
| `q` | quit |

The footer shows the mean over the last second, the mean, extremes and energy
of the window on screen, and the totals since the start or the last `r`. To
measure one event, pause, then zoom and pan until the window brackets it.

## Reading the numbers

The probe powers the target through a DC/DC converter that delivers charge in
fixed pulses, and reports each pulse with a time stamp. Current is pulse rate
times a scale, minus an offset, both from a calibration the probe runs at
start-up (against a built-in 2200 Ω load and against no load).

That has consequences worth knowing:

- **Resolution trades against time.** One pulse is about 0.49 µA·s. In a 1 s
  window the step is 0.49 µA; in 100 ms, 4.9 µA; in 1 ms, 490 µA. The footer
  shows the step for the current zoom ("one pulse = ..."). Milliamp events are
  sharp at millisecond resolution; microamp levels need long windows or
  smoothing (`s`).
- **The offset limits accuracy at the bottom.** With nothing connected the
  probe still counts about 16 pulses/s (8 µA), which is subtracted. It varies
  by about 1 pulse/s between calibrations, so readings near 1 µA are good to
  roughly ±0.5 µA. Readings never go below zero.
- **The scale was checked against one known load**: a 20 kΩ divider across
  3.3 V (165 µA) reads 164.7 µA.
- **What is measured is everything on the target's supply rail**, not just the
  microcontroller. On the LP-MSPM0L1306 the thermistor divider (jumper J15)
  alone draws 165 µA.
- **A debugger connection keeps the chip awake.** After flashing, release the
  debug hold (in this repository: `tools/unwedge/unwedge.py release`) or
  power-cycle the board before trusting low readings.

## If it does not work

- **"the probe did not answer"**: the probe's command interface has stalled,
  usually after a debugger used it. Unplug and replug the LaunchPad (here:
  `tools/unwedge/replug.sh`).
- **"no permission to open the XDS110"**: add the udev rule.
- **"in use by another program"**: close CCS, UniFlash, or another `etrace`.

## Files

A capture is two files:

- `run.bin`: the bytes the probe sent, unmodified. Format in `src/decode.rs`.
- `run.json`: calibration, sample rate, probe versions and start time.

Captures without a `.json` (older ones in `tools/energytrace/captures/`) are
read with a nominal calibration, or with `--cal NA_PER_PPS,X0_PPS`.

## As a library

The crate is also a library: `etrace::probe::Session` (open, calibrate, start,
read, stop), `etrace::decode::Decoder` (bytes to events), `etrace::trace::Trace`
(pulses on a millisecond grid, statistics), `etrace::scope::Scope` (the display).

## Tests

```bash
cargo test
```

Unit tests cover the decoder, the trace store and the display (rendered to an
off-screen buffer). `tests/captures.rs` runs the decoder over captures recorded
from a real board, including a deep-sleep one and one with a damaged stream.

## Where the protocol comes from

Reverse-engineered from TI's `libenergytracestandalone.so` (CCS Theia 1.5.1)
and checked on hardware; see `../energytrace/THEIA_VERIFICATION.md`. Things
still not understood are listed there, among them why the calibration command
reports about 10186 ticks whatever sample rate is set, and returns after half
a second.
