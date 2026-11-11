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
  it.** See "GPIO wake" below.

Not verified:

- **The MCU-only STANDBY current to better than about 0.5 uA.** With J15 out
  the rail reads 1.4 to 1.9 uA, but one pulse/s is 0.49 uA and the open-load
  offset is itself a 16-or-17-count measurement. Separating the MCU from the
  remaining jumpered circuits, or resolving 1.0 from 1.5 uA, needs a longer or
  repeated calibration (mspm0sleep-av2).
- **STOP-level guards.** Nothing here holds a `WakeGuard`, so only the
  no-guard (STANDBY1) path was exercised.

## GPIO wake needs FASTWAKE (upstream gap)

`firmware/src/lib.rs::blinky` flashes the LED immediately when the button task
sees a falling edge on S2 (PA14), so a detected press appears off the LED's
timer grid. Presses were made by hand during the captures.

| Firmware | Presses (as reported) | Presses registered | When they registered |
|---|---|---|---|
| `lp_blinky` (upstream defaults, idle STANDBY1), 90 s, `tap_default.bin` | 15 to 20 short taps, then 3 long holds | 8 | only at timer wakes: five at t = 16.40, 41.38, 48.38, 56.38, 78.38 s (all on a 1.000 s raster), three coincident with an LED-timer wake |
| `lp_blinky_fastwake` (plus `GPIOA.FASTWAKE` bit 14), 45 s, `tap_fastwake.bin` | about 5 short taps requested | 13 flashes off the grid | at arbitrary phases (x.96, .74, .60, .28, .84, .66, .44, .34, .62, .48 ...), including pairs 0.18 to 0.36 s apart |

Reading:

- With upstream defaults the GPIO edge does **not** wake the chip. A press is
  noticed only if the button is still down at the next timer wake. The 1 s
  raster is the time driver itself: it interrupts every 2^15 ticks of the
  32768 Hz counter (overflow and half-overflow, `time_driver/tim.rs`), so the
  chip wakes once a second regardless of pending timers. Roughly 5 of 15 to 20
  short taps happened to span such an instant; the long holds always do.
- With FASTWAKE set, presses register asynchronously. The close pairs are
  switch bounce or the release edge getting past the firmware's 50 ms
  debounce, i.e. our firmware, not embassy.
- Idle floor is the same in both (about 20 pulses/s): FASTWAKE costs nothing
  measurable, and the once-a-second time-driver wake is already inside the
  1.4 to 1.9 uA figure.

In upstream `embassy-mspm0/src/gpio.rs` nothing writes `FASTWAKE`, and
"wakeup enable" is a TODO (line 202). Workarounds today: set the bit through
the PAC as `lp_blinky_fastwake` does, or hold a `WakeGuard` to cap idle at a
shallower mode (`lp_blinky_standby0`, `lp_blinky_stop2`: built, not yet
tested). That the missed edges are due to the edge detector being unclocked in
STANDBY1 is an inference; the reference manual has not been checked.

History of this section: an earlier version claimed GPIO wake was verified
from a capture where the blink rate changed; that capture could not
distinguish a GPIO wake from a press noticed at a timer wake. A first
"baseline" tap run was also void because no taps were made during it. The
table above is from the rerun.

## Corrections to earlier conclusions

- All currents recorded before 2026-11-11 used `I = pulses/s * 1e6 / 10186`
  and are **low by a factor of about 5.0**. 10186 is the calibration duration
  in sample ticks, not a scale constant. The June "245 uA busy loop" is
  1.2 mA; the June "36 uA STANDBY floor" is about 180 uA.
- The June diagnosis for mspm0sleep-6ht (HAL leaves MFCLK on, keeping SYSOSC
  alive) was never confirmed by measurement and is not supported: upstream
  `init()` still enables MFCLK, and its idle floor equals the ROM's STANDBY.
