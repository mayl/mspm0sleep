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
sees a falling edge on S2 (PA14), so a detected press shows up off the 2 s
timer grid. Short taps (press and release between flashes), 45 s captures:

| Firmware | Difference | Flashes off the timer grid |
|---|---|---|
| `lp_blinky` | upstream defaults, idle in STANDBY1 | none: 22 flashes, all exactly 2.00 s apart (`tap_default.bin`) |
| `lp_blinky_fastwake` | plus `GPIOA.FASTWAKE` bit 14 | many: flashes within the gap after each tap, rate toggles (`tap_fastwake.bin`) |

Idle floor is the same in both (about 20 pulses/s), so FASTWAKE costs nothing
measurable here.

With defaults, a press is only registered if the button is still held when the
time driver next wakes the chip (observed by hand: "hold it until the LED comes
on"). That fits the edge detector having no clock in STANDBY1. An earlier
version of this document claimed GPIO wake was verified from a capture where
the blink rate changed; that capture could not distinguish a GPIO wake from a
press noticed at the next timer wake, and the claim was wrong.

In upstream `embassy-mspm0/src/gpio.rs` nothing writes `FASTWAKE`, and
"wakeup enable" is a TODO (line 202). Workarounds today: set the bit through
the PAC as `lp_blinky_fastwake` does, or hold a `WakeGuard` to cap idle at a
shallower mode (`lp_blinky_standby0`, `lp_blinky_stop2`: built, not yet
tested). The tap tests rely on a person pressing the button; the baseline run
has no independent record that taps occurred.

## Corrections to earlier conclusions

- All currents recorded before 2026-11-11 used `I = pulses/s * 1e6 / 10186`
  and are **low by a factor of about 5.0**. 10186 is the calibration duration
  in sample ticks, not a scale constant. The June "245 uA busy loop" is
  1.2 mA; the June "36 uA STANDBY floor" is about 180 uA.
- The June diagnosis for mspm0sleep-6ht (HAL leaves MFCLK on, keeping SYSOSC
  alive) was never confirmed by measurement and is not supported: upstream
  `init()` still enables MFCLK, and its idle floor equals the ROM's STANDBY.
