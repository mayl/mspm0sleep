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
re-arming. "Immediate re-arm" re-arms and sleeps within microseconds of the
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

### The STANDBY1 + FASTWAKE caveat

In that one mode, later edges are lost if the interrupt is re-armed too soon
after the wake while the pin is still low. After the first edge the firmware is
deaf; reading the counter over SWD (which forces the chip active for a moment)
lets exactly one more edge through each time.

| Pulse | Awake time before re-arm | Pin at re-arm | Counted |
|---|---|---|---|
| 30 ms | 0, 10, 30, 50, 100, 300 us, 1, 2, 3, 5, 7 ms | low | 1/20 each |
| 30 ms | 10 ms | low | 20/20 |
| 180 ms | 50 ms | low | 20/20 |
| 833 us | 0 | low | 1/20 |
| 833 us | 1, 5, 10 ms | high | 20/20 each |

So it takes both: the pin still asserted at re-arm **and** less than roughly
8 to 10 ms awake since the edge. Either waiting for release or staying awake
10 ms avoids it. STANDBY0 + FASTWAKE does not show it (20/20 with immediate
re-arm, 30 ms and 180 ms pulses). The mechanism is not known; upstream's
low-power notes mention erratum `GPIO_ERR_01` in similar words ("if the pin is
still asserted when the chip goes back to sleep no further edge is detected"),
and the errata sheet has not been checked.

### Results: timer-based firmware (`lp_blinky*`), 20 pulses at irregular intervals

The realistic case: time driver running, a 50 ms timer debounce after each
edge, so the interrupt is re-armed 50 ms after the edge. `tools/edge_blinky.py`.

| Idle mode | FASTWAKE | 833 us | 30 ms | 82 ms | 180 ms |
|---|---|---|---|---|---|
| STANDBY1 (default) | off | 0/20 | 4/20 | | |
| STANDBY1 (default) | on | 20/20 | 20/20 | 20/20 | 20/20 |
| STANDBY0 | off | 20/20 | 20/20 | 20/20 | 20/20 |
| STOP2 | off | 20/20 | 20/20 | | |

The 4/20 are pulses that happened to span a timer wake: with defaults, an edge
is only noticed if the pin is still low when the time driver next wakes the
chip. That is the "hold the button until the LED comes on" behaviour seen by
hand.

### Consequences

- Upstream `embassy-mspm0/src/gpio.rs` never writes `FASTWAKE` ("wakeup
  enable" is a TODO at line 202), so `Input::wait_for_*_edge` cannot wake a
  low-power-executor application from its default idle mode.
- Workarounds today: set the bit through the PAC
  (`pac::GPIOA.fastwake().modify(|w| w.set_din(n, true))`, needs
  `unstable-pac`), or hold a `WakeGuard::new(SleepLevel::Standby1)` to idle in
  STANDBY0 instead. On this board the idle current is the same either way
  within the measurement's resolution (about 20 pulses/s).
- With FASTWAKE in STANDBY1, do not re-arm an edge wait within about 10 ms of
  the wake while the pin may still be asserted.

Not tested: rising edges, level waits, pins other than PA9/PA14, whether a
timer wake (as opposed to a debug force-active) clears the deaf state, and the
power cost of FASTWAKE beyond "no change at 0.5 uA resolution".

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
