# MSPM0 Sleep Mode Support - Implementation Plan

## Target
MSPM0L1306 (first), then generalize to full MSPM0 family.

## Documentation References

### Primary Hardware Documentation

| Document | Location | Key Sections | What It Contains |
|----------|----------|--------------|------------------|
| **Technical Reference Manual (TRM)** | `/tmp/slau847f.pdf` (SLAU847F) | Section 2.4.2 (p.~265): Operating Mode Selection | How to enter SLEEP/STOP/STANDBY/SHUTDOWN - the canonical reference for register sequences |
| | | Section 2.4.3 (p.~267): Asynchronous Fast Clock Requests | How peripherals temporarily boost clocks in STOP/STANDBY |
| | | Section 2.6 (p.~275): SYSCTL_TYPEA Registers | Full register bitfield descriptions |
| | | Table 2-10: Policy Bit Configuration | Combines all mode entry settings in one table |
| | | Table 2-29: SYSOSCCFG fields (offset 0x1100) | DISABLE(10), DISABLESTOP(9), USE4MHZSTOP(8), FREQ(1:0), FASTCPUEVENT(17), BLOCKASYNCALL(16) |
| | | Table 2-30: MCLKCFG fields (offset 0x1104) | USELFCLK(20), STOPCLKSTBY(21), USEMFTICK(12), MDIV(3:0), MCLKDEADCHK(22) |
| | | Table 2-33: PMODECFG fields (offset 0x1140) | DSLEEP(1:0): 00=STOP, 01=STANDBY, 10=SHUTDOWN; SYSSRAMONSTOP(5) |
| | | Section 2.9.3-2.9.8 (p.~421-422): Quick Start | Recommended configurations for each mode |
| **Power Optimization App Note** | `/tmp/slaae72.pdf` (SLAAE72) | Section 2: Low-Power Features in PMCU | Current consumption figures, power domain behavior |
| | | Section 3: Software Coding Strategies | GPIO config, clock reduction, non-blocking patterns |
| | | Table 2-1: Supported Functionality by Operating Mode | What runs in each mode (SYSOSC, CPUCLK, ULPCLK, etc.) |
| **Datasheet** | https://www.ti.com/document-viewer/mspm0l1306/datasheet | Power section | Exact current numbers, wake latency (not in TRM/app note) |

### Embassy Code References

| File | What To Learn From It |
|------|----------------------|
| `embassy/embassy-stm32/src/low_power.rs` | Full low-power flow: `sleep()` -> `configure_pwr()` -> `WFI` -> `on_wakeup()`. Time driver pause/resume integration. Stop mode refcounting. |
| `embassy/embassy-stm32/src/executor.rs` | Custom executor that calls `low_power::sleep()` in poll loop. `__pender` implementation. Thread + interrupt executor patterns. |
| `embassy/embassy-stm32/src/rcc/mod.rs` | Stop mode tracking: `get_stop_mode()`, `increment_stop_refcount()`, `StopMode` enum, `RccInfo.stop_mode` field. How peripherals declare they block stop. |
| `embassy/embassy-executor/src/platform/cortex_m.rs` | Generic WFE/SEV executor. Standard pattern that HAL-specific executors replace. |
| `embassy/embassy-rp/src/executor.rs` | Another custom executor reference (multicore-aware). Shows `__pender` with `sev` instruction. |
| `embassy/embassy-mspm0/src/lib.rs` | Current HAL init: `Config` struct, `init()` function, SYSCTL setup, clock config. Where to add low-power config options. |
| `embassy/embassy-mspm0/src/time_driver.rs` | Timer-based time driver using LFCLK. Needs `pause_time()` / `resume_time()` for STOP mode (Phase 2). |
| `embassy/embassy-mspm0/Cargo.toml` | Features: add `low-power`, `executor-thread`, `executor-interrupt`. Existing chip feature structure. |
| `embassy/examples/mspm0l1306/` | Existing examples: `blinky.rs`, `button.rs`, `adc.rs`, `uart.rs`, `i2c.rs`, etc. New `sleep_demo.rs` goes here. |
| `embassy/tests/mspm0/` | Hardware test infrastructure using `teleprobe-meta`. Only targets G3507/G3519 currently. |

## Phase 1a: Basic SLEEP Mode + Custom Executor (High Priority)

### What SLEEP Mode Does
- CPU clock (CPUCLK) halted
- Everything else identical to RUN mode
- Any NVIC interrupt wakes immediately
- Entry: clear `SCR.SLEEPDEEP`, execute `WFI`

### Files to Create/Modify

1. **`embassy/embassy-mspm0/src/low_power.rs`** (NEW)
   - `unsafe fn sleep(cs: CriticalSection)` - enters SLEEP mode via WFI
   - `fn configure_sleep()` - clears SLEEPDEEP in SCR
   - Keep simple: no STOP mode yet, no peripheral tracking

2. **`embassy/embassy-mspm0/src/executor.rs`** (NEW)
   - Modeled after `embassy-stm32/src/executor.rs`
   - `__pender`: sets atomic flag for thread mode, pends NVIC for interrupt mode
   - `Executor::run()`: poll -> if no work -> `crate::low_power::sleep(cs)`
   - Must support both `executor-thread` and `executor-interrupt` features

3. **`embassy/embassy-mspm0/src/lib.rs`** (MODIFY)
   - Add `#[cfg(feature = "low-power")]` mod declaration for `low_power`
   - Add `#[cfg(feature = "executor-thread")]` mod declaration for `executor`
   - Keep backward compatible: existing `init()` unchanged

4. **`embassy/embassy-mspm0/Cargo.toml`** (MODIFY)
   - Add feature: `low-power = []`
   - Add feature: `_executor = ["dep:embassy-executor", "low-power"]`
   - Add feature: `executor-thread = ["_executor"]`
   - Add feature: `executor-interrupt = ["_executor"]`
   - Ensure `embassy-executor` is an optional dependency

5. **`embassy/examples/mspm0l1306/Cargo.toml`** (MODIFY for demo)
   - Add example showing `executor = "embassy_mspm0::executor::Executor"` usage
   - Keep existing examples using generic executor unchanged

### Registers Used (Phase 1a)
- **SCR** (CPU register, via `cortex_m::peripheral::SCB`):
  - Bit 2: `SLEEPDEEP` - clear for SLEEP mode, set for STOP/STANDBY
  - Use `SCB::clear_sleepdeep()` / `SCB::set_sleepdeep()`

### What This Gives Us
- ~200 µA idle current (vs. ~mA active) with zero wake latency
- Works with any interrupt
- No peripheral state concerns (PD1 stays fully on)
- Foundation for Phase 2 STOP mode

### Testing Phase 1a
- [ ] Build `embassy-mspm0` without new features (regression)
- [ ] Build `embassy-mspm0` with `low-power` feature
- [ ] Build `embassy-mspm0` with `executor-thread` feature
- [ ] Build all existing `examples/mspm0l1306` without changes
- [ ] Create `sleep_demo.rs` - compile and flash to LaunchPad
- [ ] Verify LED blink timing is correct (timer accuracy)
- [ ] Verify button interrupt wakes immediately
- [ ] Verify RTT/defmt logging continues to work

---

## Phase 1b: Compile-Time Regression Tests

Run these commands to verify no breakage:

```bash
cd /home/larry/projects/claude/mspm0sleep/embassy

# Core HAL builds
cargo check -p embassy-mspm0 --features mspm0l1306rhb
cargo check -p embassy-mspm0 --features mspm0l1306rhb,low-power
cargo check -p embassy-mspm0 --features mspm0l1306rhb,executor-thread

# All L-series
cargo check -p embassy-mspm0 --features mspm0l2228pn
cargo check -p embassy-mspm0 --features mspm0l1106rhb

# Other families (regression)
cargo check -p embassy-mspm0 --features mspm0c1104dgs20
cargo check -p embassy-mspm0 --features mspm0g3507pm

# Examples
cd examples/mspm0l1306 && cargo build
cd examples/mspm0c1104 && cargo build
cd examples/mspm0g3507 && cargo build
```

---

## Phase 1c: Sleep Demo Example

Create `embassy/examples/mspm0l1306/src/bin/sleep_demo.rs`:

```rust
#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_mspm0::gpio::{Input, Level, Output, Pull};
use embassy_mspm0::{Config, bind_interrupts, peripherals};
use embassy_time::Timer;
use {defmt_rtt as _, panic_halt as _};

#[embassy_executor::main(executor = "embassy_mspm0::executor::Executor")]
async fn main(_spawner: Spawner) -> ! {
    info!("Sleep demo starting!");
    let p = embassy_mspm0::init(Config::default());

    let mut led = Output::new(p.PA0, Level::Low);
    led.set_inversion(true);
    let button = Input::new(p.PA18, Pull::Up);  // Adjust for LaunchPad pin

    loop {
        led.set_high();
        Timer::after_millis(100).await;
        led.set_low();

        // Sleep until next timer event (or button interrupt)
        Timer::after_millis(900).await;

        if button.is_low() {
            info!("Button pressed - woke from sleep!");
        }
    }
}
```

Hardware verification checklist:
- [ ] LED blinks 100ms on, 900ms off
- [ ] Pressing button logs message
- [ ] Power consumption drops in idle vs. busy-wait equivalent
- [ ] No lockups after 1 hour of continuous operation

---

## Phase 2: STOP Mode + Time Driver Integration (Medium Priority)

### What STOP Mode Does
- CPU, PD1 (SRAM, flash, high-speed peripherals) disabled but retention maintained
- PD0 peripherals remain active on ULPCLK
- SYSOSC behavior configurable: keep running (STOP0), gear to 4MHz (STOP1), or disable (STOP2)
- Wake: any NVIC interrupt from active PD0 peripheral
- SYSCTL auto-re-enables SYSOSC on wake

### Registers Used (Phase 2)
- **PMODECFG @ 0x1140**: `DSLEEP = 0b00` for STOP
- **SYSOSCCFG @ 0x1100**: `DISABLESTOP = 1` to disable SYSOSC in STOP (STOP2 behavior)
- **MCLKCFG @ 0x1104**: `USELFCLK` for running from 32kHz
- **SCR** (CPU): `SLEEPDEEP = 1` for deep sleep

### Time Driver Considerations
- MSPM0 time driver uses LFCLK @ 32.768 kHz (via `time_driver.rs`)
- LFCLK stays active in STOP mode (all STOP variants)
- Timer continues counting during STOP - **no special pause/resume needed for SLEEP**
- For STOP: may need to verify alarm setup before sleep, handle edge case where alarm fires during sleep entry

### Files to Modify
1. **`embassy/embassy-mspm0/src/low_power.rs`** (EXTEND)
   - Add `unsafe fn stop_sleep(cs)` for STOP mode
   - Configure PMODECFG.DSLEEP = 0b00 before sleep
   - Set SCR.SLEEPDEEP
   - On wakeup: verify timer alarm hasn't been missed

2. **`embassy/embassy-mspm0/src/time_driver.rs`** (MODIFY)
   - Add `fn ensure_alarm_before_sleep()` - verify next alarm is set
   - The LFCLK-based timer naturally continues; mostly just bookkeeping

3. **`embassy/embassy-mspm0/src/lib.rs`** (MODIFY)
   - Add config option: `enable_stop_mode: bool`
   - Add config option: `enable_debug_during_sleep: bool` (for STANDBY later)

### Testing Phase 2
- [ ] STOP mode demo: verify ~50 µA idle current
- [ ] Timer accuracy test: 1-second blinks for 60 seconds, compare to wall clock
- [ ] Wake latency: button press to LED response time
- [ ] Verify DMA transfer completes after STOP wake (future: Phase 3)
- [ ] Long-run stability: 24-hour continuous operation

---

## Phase 3: Peripheral-Aware Deep Sleep (Future)

### What This Adds
- Track which peripherals need PD1 (DMA transfers, flash writes)
- Only enter STOP when safe, otherwise fall back to SLEEP
- Analogous to STM32's `StopMode` refcounting

### MSPM0-Specific Considerations
- SYSCTL auto-disables PD1 peripherals in STOP; they retain config
- Main concern: active DMA transfers (DMA is in PD1)
- GPIO, ADC conversion logic are in PD0 - safe in STOP
- UART/SPI/I2C are PD1 - will be disabled but retain config

### No Plan Yet
This needs more analysis of which MSPM0 peripherals actually wake from STOP and what state they need.

---

## Hardware Test Setup (MSPM0L1306 LaunchPad)

### Pins
- **PA0**: LED1 (already used in blinky example, active low)
- **PA18**: S2 button (verify on LaunchPad schematics)
- **SWD**: PA13/PA14 for debug + RTT

### Power Measurement
- Remove JP5 (if present) to isolate MCU power
- Use ammeter in series, or:
- Use LaunchPad's built-in EnergyTrace if using CCS
- Expected readings:
  - Active (32MHz): ~2-3 mA
  - SLEEP (4MHz): ~200 µA
  - STOP (32kHz): ~50 µA
  - STANDBY: ~1-2 µA

### Debug Notes
- Debug connection may be lost in STOP mode if not configured
- Add `enable_debug_during_sleep: bool` config (like STM32)
- May need to keep SYSOSC running in STOP for debug (STOP0)
- `defmt-rtt` should continue working in SLEEP mode

---

## Key Design Decisions

1. **SLEEP as default, STOP as opt-in**: SLEEP is safe (no state loss, any interrupt wakes). STOP requires more care.
2. **No STANDBY in executor loop**: STANDBY is too aggressive for transparent async sleeping. Explicit API only.
3. **Custom executor is opt-in**: Existing examples continue using `embassy-executor`'s generic WFE executor.
4. **LFCLK time driver is an advantage**: Unlike STM32, no separate low-power timer needed. Timer keeps ticking in STOP.
5. **WFI not WFE**: NVIC interrupt model is cleaner for MSPM0. SEV/WFE not needed since `__pender` uses NVIC pending.
