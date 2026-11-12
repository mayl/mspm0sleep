# etrace

Current measurement with the XDS110 debug probe on a TI LaunchPad, without
Code Composer Studio. Watch the current live as a terminal oscilloscope, or
record it headless and get statistics.

```
 etrace  ■ STOPPED   tools/energytrace/captures/lp_blinky_gpiofix.bin
╭ Current ────────────────────────────────────────────────────────────────────── 8 s · 44 ms per point ╮
│5 mA   │                                                                                              │
│       │            ⣀                      ⢀                       ⣀                      ⢀           │
│       │            ⣿                      ⢸                       ⣿                      ⢸           │
│3.75 mA│            ⣿                      ⡸⡀                      ⣿                      ⣿           │
│       │            ⣿                      ⡇⡇                      ⣿                      ⡟⡄          │
│       │            ⣿                      ⡇⡇                     ⢀⢿                      ⡇⡇          │
│2.5 mA │           ⢠⠃⡇                     ⡇⡇                     ⢸⠘⡄                     ⡇⡇          │
│       │           ⢸ ⡇                     ⡇⡇                     ⢸ ⡇                     ⡇⡇          │
│       │           ⢸ ⡇                    ⢠⠃⢇                     ⢸ ⡇                    ⢸ ⡇          │
│1.25 mA│           ⢸ ⡇                    ⢸ ⢸                     ⢸ ⡇                    ⢸ ⢱          │
│       │           ⢸ ⢇                    ⢸ ⢸                     ⡸ ⡇                    ⢸ ⢸          │
│0      │⠤⢄⠤⠤⠤⠤⠤⠤⡠⠤⠤⠎⠒⠸⠤⠤⠤⠤⠤⡠⠤⠤⠤⠤⠤⡠⠤⠤⠤⠤⠤⡠⠤⠤⠼⠒⠸⢄⠤⠤⠤⠤⠤⠤⡠⠤⠤⠤⠤⢄⠤⠤⠤⠤⠤⠤⡠⠤⠇⠒⠣⠤⡠⠤⠤⠤⠤⠤⡠⠤⠤⠤⠤⠤⡠⠤⠤⠤⠤⠤⡠⠼⠒⠸⠤⠤⠤⠤⡠⠤⠤⠤⠤⡀│
│       └──────────────────────────────────────────────────────────────────────────────────────────────│
│   5.0 s                                            9.0 s                                       13.0 s│
╰──────────────────────────────────────────────────────────────────────────────────────────────────────╯
  █▃▃▃▃▃▃▄▃▃█▃▄▃▃▃▃▃▄▃█▃▃▃▄▃▃▃▃▃█▄▃▃▃▃▃▄▃▃█▃▄▃▃▃▃▃▃▄█▃▃▃▃▃▄▃▃▃█▃▃▃▃▃▃▄▃▃█▃▃▃▃▄▃▃▃▃█▃▃▃▃▃▄▃▃▃█▃▄▃▃▃▃▃▃▃
╭ Window ──────────────────╮╭ Capture ───────────╮╭ View ────────────────╮╭ Calibration ───────────────╮
│mean 212 µA               ││mean 217 µA         ││span 8 s              ││scale 493.5 nA/pps          │
│min 0.00 µA  max 4.187 mA ││time 20.0 s         ││1 pulse = 11.2 µA     ││offset 16.2 pps             │
│5.58 mJ in 8.0 s          ││energy 14.31 mJ     ││Y auto · linear       ││nominal (none saved)        │
╰──────────────────────────╯╰────────────────────╯╰──────────────────────╯╰────────────────────────────╯
 +/- span   ←/→ pan   ↑/↓ Y zoom   a auto Y   g log   s smooth   f fit   ? help   q quit
```

(`etrace view` on a capture of an MSPM0L1306 asleep at 2 µA, waking to flash an LED every 2 s.
In a terminal the trace is green, the window mean a yellow line, and the part of the overview strip
that is on screen is highlighted.)

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
| `?` | key reference |
| `q` | quit |

Under the trace, a one-row strip shows everything recorded so far, on a
logarithmic scale, with the part on screen highlighted. The cards below it
show the mean over the last second (live only), the mean, extremes and energy
of the window on screen, the totals since the start or the last `r`, the view
settings, and the calibration in use. To measure one event, pause, then zoom
and pan until the window brackets it.

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
