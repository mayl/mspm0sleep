# Upstream embassy low-power on LP-MSPM0L1306: verification and current

Measured 2026-11-11 (bead mspm0sleep-9wr). Embassy at upstream `5b8b806ce`,
unmodified. Firmware in `firmware/`, captured with `tools/measure.sh`.

## What upstream provides

`embassy-mspm0` now has a `low-power` feature and its own executor
(`embassy_mspm0::executor::Executor`, enabled by `executor-thread`). On idle it
enters the deepest mode no `WakeGuard` forbids; with no guards held that is
STANDBY1. The time driver must be on TIMG0 or TIMG1 (the only timers clocked in
STANDBY); `time-driver-any` is rejected at compile time. There is no upstream
example using it, so `firmware/` is ours:

    #[embassy_executor::main(executor = "embassy_mspm0::executor::Executor",
                             entry = "cortex_m_rt::entry")]

with `embassy-mspm0` features `executor-thread`, `time-driver-timg0` and
**without** `embassy-executor`'s `platform-cortex-m` / `executor-thread`.

## Results

Calibrated with TI's two-point method (2200 ohm reference load vs open; see
`tools/energytrace/THEIA_VERIFICATION.md`). Calibration repeated on every
capture: 493.2 to 493.7 nA per pulse/s, open-load offset 15.7 to 16.7 pulses/s.

| Firmware | State | Pulses/s | Current |
|---|---|---|---|
| `busy` | RUN, 32 MHz, spinning | 3420 | 1.68 mA |
| `lp_idle --features wfi` | generic executor, idle in SLEEP (WFI) | 2320 | 1.14 mA |
| `lp_idle` | upstream low-power executor, idle | 350 | 165 uA |
| (blank flash) | TI ROM bootloader after its 10 s timeout, STANDBY | 350 | 165 uA |
| `lp_blinky` | as `lp_idle`, plus LED 100 ms every 2 s | 350 between pulses | 165 uA floor, 371 uA mean |
| `lp_idle`, **J15 removed** | upstream low-power executor, idle | 19.6 | **1.4 to 1.9 uA** |

Captures: `tools/energytrace/captures/*_upstream.{bin,png}`, `rom_bsl_standby.bin`,
`lp_idle_noJ15.{bin,png}`.

### The 165 uA was the thermistor

J15 ("T.PWR") permanently powers the on-board thermistor divider: R2 10.0 kohm
from 3V3 to RT1 (TMP6131, 10 kohm) to ground, i.e. 3.3 V / 20 kohm = 165 uA
(SLAU869E section 2.3.7.1 and Table 2-1, which lists "J15: OFF to remove power to
thermistor circuitry" for low-power measurement). With J15 pulled and nothing
else changed, the idle stream falls from 350 to 19.6 pulses/s, against an
open-load calibration offset of 15.7 to 16.7 pulses/s. Two 60 s captures give
1.9 uA and 1.4 uA; the difference between them is entirely the calibration
offset landing on 16 or 17 counts, so read this as about 1.6 uA +/- 0.5.

That figure is still the whole target rail with the J101 isolation block, the
backchannel UART jumpers (J16/J17), S1 (J11) and the light sensor (J4 to J6)
installed, so the MCU itself is at or below it. The datasheet's STANDBY figure
is about 1 uA.

## What this does and does not show

Verified:

- The low-power executor works: the chip enters deep sleep (PWR-AP status
  reads `0x00a00003`, AHB-AP power-gated, the same signature as the ROM's
  STANDBY) and the time driver wakes it on schedule (LED pulses every 2 s in
  `lp_blinky_upstream`; `lp_idle` wakes every 5 s).
- Relative to idling in plain SLEEP, deep sleep removes about 0.98 mA.
- With the board as shipped, the idle floor is **identical to the pulse**
  (350 pulses/s) to TI's own ROM code sitting in STANDBY, so it is not embassy;
  it is the thermistor (below).
- With J15 removed the target rail idles at about 1.6 uA, consistent with the
  chip being in STANDBY.
- **GPIO edges do NOT wake the chip from the default idle state (STANDBY1)
  unless the port's FASTWAKE bit is set for the pin, and upstream never sets
  it.** See "GPIO wake from deep sleep" below.

Not verified:

- **The MCU-only STANDBY current to better than about 0.5 uA.** With J15 out
  the rail reads 1.4 to 1.9 uA, but one pulse/s is 0.49 uA and the open-load
  offset is itself a 16-or-17-count measurement. Separating the MCU from the
  remaining jumpered circuits, or resolving 1.0 from 1.5 uA, needs a longer or
  repeated calibration (mspm0sleep-av2).
- **STOP-level guards.** Nothing here holds a `WakeGuard`, so only the
  no-guard (STANDBY1) path was exercised.

## GPIO wake from deep sleep (upstream gap)

Summary: with upstream defaults a GPIO edge does **not** wake the chip from
STANDBY1, the mode the executor idles in. Setting the port's `FASTWAKE` bit for
the pin fixes that, with one caveat (below). STOP0/STOP2/STANDBY0 wake without
it for pulses longer than about one 32 kHz clock period.

### Method

`firmware/src/bin/edge_test.rs` (`edge_test` in `lib.rs`) is built **without
the time driver**, so a GPIO edge is the only thing that can wake the chip
(the time driver otherwise interrupts once a second: overflow and half-overflow
of its 32768 Hz counter). It counts falling edges on S2 (PA14) and on PA9 and
toggles LED1. The idle cap (via `WakeGuard`), FASTWAKE and re-arm behaviour are
set by a config word written over SWD into `.uninit` RAM, then a reset.

Edges on PA9 come from the XDS110 backchannel UART (`tools/unwedge/uart_pulse.py`):
the line idles high, `0xFF` gives one low pulse of 1 bit time, `0x00` one of
9 bit times. In the WFI control mode this is exact (20/20 at every width).
Counters are read with `unwedge.py peek`, which does not halt or reset; each
cell starts from a reset and is read once, after its pulses.
`tools/edge_matrix.py` runs the matrix; raw results in
`tools/energytrace/captures/edge_matrix.json`.

### Results: timer-free firmware, 20 pulses per cell

By default the firmware stays awake until the pin is high again before
re-arming (so it always sleeps armed for falling with the pin high). "Immediate re-arm" re-arms and sleeps within microseconds of the
edge, while the pin is still low.

| Idle mode | FASTWAKE | 8.7 us | 833 us | 30 ms | 30 ms, immediate re-arm |
|---|---|---|---|---|---|
| WFI (control) | off | 20/20 | 20/20 | 20/20 | 20/20 |
| WFI (control) | on | 20/20 | 20/20 | 20/20 | 20/20 |
| STOP0 | off | 20/20 | 20/20 | 20/20 | 20/20 |
| STOP0 | on | 20/20 | 20/20 | 20/20 | 20/20 |
| STOP2 | off | 5/20 | 20/20 | 20/20 | 20/20 |
| STOP2 | on | 20/20 | 20/20 | 20/20 | 20/20 |
| STANDBY0 | off | 4/20 | 20/20 | 20/20 | 20/20 |
| STANDBY0 | on | 20/20 | 20/20 | 20/20 | 20/20 |
| STANDBY1 (default) | off | **0/20** | **0/20** | **0/20** | **0/20** |
| STANDBY1 (default) | on | 20/20 | 20/20 | 20/20 | **1/20** |

Manual presses of S2 agree: STANDBY1 without FASTWAKE 0 of 10 (LED never lit,
counter 0, `RIS` 0 with `IMASK` 1: the edge was never latched); STANDBY1 with
FASTWAKE 10 of 10; STANDBY0 without FASTWAKE toggled on all 10 (seen, counter
not read); WFI control read 12 for about 10 presses (bounce or a miscount).

Reading:

- **STANDBY1 without FASTWAKE sees nothing**, at any pulse width.
- **STOP2 and STANDBY0 without FASTWAKE** catch pulses of 833 us and longer but
  only about a quarter of 8.7 us ones. That is what sampling at 32768 Hz would
  give (8.7 us / 30.5 us = 28%, i.e. 5.7 of 20 expected; 4 and 5 seen), so edge
  detection there is evidently clocked by LFCLK. With FASTWAKE all are caught.
- **STOP0** catches everything.

### The STANDBY1 + FASTWAKE rule: armed edge and pin level must agree at sleep

In that one mode, later edges are lost **if and only if the chip goes to sleep
armed for a falling edge while the pin is low**. It has nothing to do with how
long the chip was awake. Plain `wait_for_falling_edge`, re-armed a true time
after the edge, pulse widths verified (see "Measurement corrections"):

| Pulse | Re-armed after | Pin at re-arm | Counted |
|---|---|---|---|
| 30 ms | 0, 1, 10, 20, 25 ms | low | 1/20 each |
| 30 ms | 35, 45, 55, 65, 80 ms | high | 20/20 each |
| 60 ms | 0, 1, 10, 20, 25, 35, 45, 55 ms | low | 1/20 each |
| 60 ms | 65, 80 ms | high | 20/20 each |

Once deaf, reading the counter over SWD (which forces the chip active for a
moment) lets exactly one more edge through. The mirror image holds too: stuck
armed for *rising* with the pin already high, the firmware stayed deaf through
49 further pulses (next table). STANDBY0 + FASTWAKE does not show it (20/20
with immediate re-arm).

This is TI erratum **GPIO_ERR_01**, "GPIO wakeup edges can be lost in STANDBY
mode" (SLAZ741E, MSPM0L110x/L13xx, silicon Rev C and D; same text in SLAZ742H
for the G parts), checked 2026-11-12:

> Case 1: STANDBY0 wakeup - IF the MCU is set into STANDBY/STOP/SLEEP mode with
> an IO in "wake" state AND one sets the IO back to the "non-wake" state for
> < 3 LFCLK cycles and THEN asserts it again, the next wakeup edge will not be
> detected.
> Case 2: STANDBY1 wakeup - IF a GPIO edge is used to wakeup AND the GPIO pulse
> is still active when the device returns to STANDBY1, THEN the device will not
> detect any subsequent wakeup edges.
>
> Workaround, case 1: make sure that the GPIO is de-asserted while the device
> is in active mode, OR ensure the wakeup pulse is longer than 3 LFCLK cycles.
> Case 2: set GPIO wakeup edge to both falling and rising edges, OR ensure the
> GPIO wakeup pulse is not active before entering STANDBY1.

Case 2 is the rule measured above; arm-then-check (below) is its second
workaround done without a race, and "arm both edges" is its first. The
erratum does not mention the mirror case (armed rising, pin already high),
which behaved the same here.

### Re-arm strategies

STANDBY1 + FASTWAKE, re-armed immediately after each falling edge.

| Strategy before sleeping again | 833 us, 30 ms (20 each) | Width sweep, 9 to 833 us, 64 widths x 50 pulses |
|---|---|---|
| plain `wait_for_falling_edge` | 1/20 | not run |
| read `DIN`, clear, read `DIN` and `RIS`, then arm (extra reads *before* arming) | 1/20 | not run |
| check then arm: if the pin is low, `wait_for_high`, then arm falling | 20/20 | **62 of 64**: at 74 and 76 us, 1/50 and stuck asleep waiting for a rise that had already happened |
| arm both edges, count the wake if the pin is then low | 20/20 | 35 of 64: every pulse wakes the chip, but below about 70 us the pin is high again before it can be read, so the edge is misclassified (0/50 counted, 50 wakes) |
| **arm then check**: arm the edge, read the pin, sleep only if the level agrees with the armed edge | 20/20 | **64 of 64** (3200 pulses) |

So:

- The window in check-then-arm is real and was hit. Upstream's
  `wait_for_high` / `wait_for_low` have this order: they read the level, and
  only then does `wait_inner` set the polarity and **clear** any latched edge
  (`gpio.rs` lines 297 to 356). A transition between the read and the clear is
  latched and then wiped. That race exists without any sleep mode; in
  STANDBY1 + FASTWAKE the resulting state is also deaf.
- Arm-then-check has no such window: after the clear, any transition sets
  `RIS`, so reading the level afterwards is safe. For a falling wait the
  procedure is: arm falling, read the pin; if it is high, sleep; if it is low
  and nothing is latched, it was low before arming, so arm rising, read the
  pin, and sleep only if it is still low. `edge_test` strategy 4.
- Arming both edges wakes reliably but cannot tell direction for pulses
  shorter than the wake latency (about 70 us here).

Wake latency from STANDBY1 to the firmware reading the pin is about 70 us
(from the two thresholds above); in WFI it is about 55 us.

### Results: timer-based firmware (`lp_blinky*`), 20 pulses at irregular intervals

The realistic case: time driver running, a 50 ms timer debounce after each
edge, then plain `wait_for_falling_edge`. `tools/edge_blinky.py`.

| Idle mode | FASTWAKE | 833 us | 3.1 ms | 22.7 ms | 30 ms | 60 ms |
|---|---|---|---|---|---|---|
| STANDBY1 (default) | off | 0/20 | | | 2/20, 4/20 | 4/20 |
| STANDBY1 (default) | on | 20/20 | 20/20 | 20/20 | 20/20 | 20/20 |
| STANDBY0 | off | 20/20 | 20/20 | 20/20 | 20/20 | 20/20 |
| STOP2 | off | 20/20 | | | 20/20 | |

With defaults, the few that register are pulses that span a timer wake: an
edge is only noticed if the pin is still low when the time driver next wakes
the chip. That is the "hold the button until the LED comes on" behaviour seen
by hand.

The 60 ms column re-arms at 50 ms with the pin still low, i.e. the deaf
condition, yet scores 20/20 with FASTWAKE. In this firmware a timer always
fires 100 ms after each press (LED off), after the pin has gone high, and the
chip then re-enters sleep in a consistent state. So a later wake evidently
clears the deaf state. An application with no timer due soon would stay deaf
until the time driver's next tick (up to 1 s); that case was not produced
here because the probe cannot make pulses longer than 60 ms.

### Consequences

- Upstream `embassy-mspm0/src/gpio.rs` never writes `FASTWAKE` ("wakeup
  enable" is a TODO at line 202), so `Input::wait_for_*_edge` cannot wake a
  low-power-executor application from its default idle mode.
- Workarounds today: set the bit through the PAC
  (`pac::GPIOA.fastwake().modify(|w| w.set_din(n, true))`, needs
  `unstable-pac`), or hold a `WakeGuard::new(SleepLevel::Standby1)` to idle in
  STANDBY0 instead. On this board the idle current is the same either way
  within the measurement's resolution (about 20 pulses/s).
- With FASTWAKE in STANDBY1, never sleep armed for an edge the pin's current
  level rules out. Arm first, then read the pin (arm-then-check).
- `wait_for_high` / `wait_for_low` should check the level after arming, not
  before, regardless of sleep mode.

Not tested: pins other than PA9/PA14, the power cost of FASTWAKE beyond "no
change at 0.5 uA resolution", pulses longer than 60 ms, and the deaf state in
a timer-based application with no timer due.

### Upstream status (checked 2026-11-12)

Reported as [embassy-rs/embassy#7513](https://github.com/embassy-rs/embassy/issues/7513).

- `embassy-mspm0` on upstream `main` (5e697db33) is identical to the commit
  tested here (5b8b806ce); no existing issue or PR covered this.
- The closed PR #6580 had a GPIO part (`EdgeArm`: sets `FASTWAKE`, always
  programs both edges and decides direction by sampling `DIN` in the interrupt
  handler). It was left out when the work was split into #6646, but the doc
  comment in `low_power/mod.rs` saying the STANDBY1 half of `GPIO_ERR_01` "is
  handled" was kept.
- Open PR #7326 (GPIO interrupt bindings) rewrites `gpio.rs` without adding
  `FASTWAKE` or changing the `wait_for_high` / `wait_for_low` order; a fix
  would have to be coordinated with it.

### Measurement corrections made along the way

- `cortex_m::asm::delay(n)` takes 3 CPU cycles per count on this core
  (a nominal 3 s measured 9.0 s). Firmware delays were 3x longer than labelled
  until `spin_us` fixed it. An earlier version of this section gave a table of
  "awake time before re-arm" in those units and concluded that staying awake
  about 10 ms avoided the deaf state; that was an artifact (the "10 ms" was
  30 ms, the end of the pulse) and is replaced by the table above.
- The XDS110 UART is accurate down to 150 baud (`0x00` = 60.05 ms measured by
  the firmware) and wrong below it: 110, 75 and 50 baud gave 3.1, 41.4 and
  22.7 ms instead of 82, 120 and 180 ms. Cells earlier labelled "82 ms" and
  "180 ms" were 3.1 ms and 22.7 ms and are relabelled.
- The default-mode matrix cells are unaffected: they wait for the pin to go
  high, whatever the delay scale.

### Earlier, wrong readings of this (kept for the record)

1. A capture where the blink rate changed after button presses was reported as
   "GPIO wake verified". It could not distinguish a GPIO wake from a press
   noticed at the next timer wake.
2. A "baseline" tap capture was read as "taps ignored" when no taps had been
   made during it.
3. A STANDBY0 blink capture was read as "worked a few times, then went dead".
   The presses had most likely all been made in its first seconds; the
   automated runs show STANDBY0 registering 20/20.

The counter-based test above replaced all three.

## Corrections to earlier conclusions

- All currents recorded before 2026-11-11 used `I = pulses/s * 1e6 / 10186`
  and are **low by a factor of about 5.0**. 10186 is the calibration duration
  in sample ticks, not a scale constant. The June "245 uA busy loop" is
  1.2 mA; the June "36 uA STANDBY floor" is about 180 uA.
- The June diagnosis for mspm0sleep-6ht (HAL leaves MFCLK on, keeping SYSOSC
  alive) was never confirmed by measurement and is not supported: upstream
  `init()` still enables MFCLK, and its idle floor equals the ROM's STANDBY.
