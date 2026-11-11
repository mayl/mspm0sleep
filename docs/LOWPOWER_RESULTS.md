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

Captures: `tools/energytrace/captures/*_upstream.{bin,png}`, `rom_bsl_standby.bin`.

## What this does and does not show

Verified:

- The low-power executor works: the chip enters deep sleep (PWR-AP status
  reads `0x00a00003`, AHB-AP power-gated, the same signature as the ROM's
  STANDBY) and the time driver wakes it on schedule (LED pulses every 2 s in
  `lp_blinky_upstream`; `lp_idle` wakes every 5 s).
- Relative to idling in plain SLEEP, deep sleep removes about 0.98 mA.
- The idle floor is **identical to the pulse** (350 pulses/s) to TI's own ROM
  code sitting in STANDBY on the same board. Whatever sets that floor is not
  embassy.

Not verified:

- **The MCU's own STANDBY current.** 165 uA is the whole target rail as the
  board is currently jumpered. It cannot be the chip (datasheet STANDBY is
  around 1 uA), and it is the same with two unrelated programs. 3.3 V / 20 kohm
  is 165 uA, which would fit a 10 k thermistor divider across the rail; that is
  a guess until jumpers are pulled. To test: flash `lp_idle`, remove jumpers
  one at a time, and run `NOSWD=1 tools/measure.sh - <name> 10` after each.
- **GPIO wake from deep sleep.** `lp_blinky` has a task waiting on S2 (PA14);
  nobody has pressed it.
- **STOP-level guards.** Nothing here holds a `WakeGuard`, so only the
  no-guard (STANDBY1) path was exercised.
- **Accuracy at the low end.** One pulse/s is about 0.5 uA and the open-load
  offset is about 16 pulses/s (8 uA), so a true 1 uA floor would sit inside the
  calibration offset. Resolving it needs long captures and a stable offset.

## Corrections to earlier conclusions

- All currents recorded before 2026-11-11 used `I = pulses/s * 1e6 / 10186`
  and are **low by a factor of about 5.0**. 10186 is the calibration duration
  in sample ticks, not a scale constant. The June "245 uA busy loop" is
  1.2 mA; the June "36 uA STANDBY floor" is about 180 uA.
- The June diagnosis for mspm0sleep-6ht (HAL leaves MFCLK on, keeping SYSOSC
  alive) was never confirmed by measurement and is not supported: upstream
  `init()` still enables MFCLK, and its idle floor equals the ROM's STANDBY.
