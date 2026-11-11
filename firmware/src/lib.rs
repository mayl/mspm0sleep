#![no_std]
//! Shared bits for the LP-MSPM0L1306 measurement firmwares.

use core::sync::atomic::{AtomicU32, Ordering};

use defmt_rtt as _;
use panic_probe as _;

/// Stay in RUN, CPU spinning, for ~3 s after reset before any sleep is allowed.
///
/// Deep sleep makes the SWD port hard to reach; a firmware that sleeps
/// immediately and never wakes leaves only a narrow race window for a debugger
/// to attach after reset. Every firmware here calls this first so that
/// "reset (or power-cycle), then attach within 3 s" always works.
/// See docs/UNWEDGE.md.
pub fn attach_window() {
    // SYSOSC = 32 MHz out of reset; asm::delay counts CPU cycles.
    cortex_m::asm::delay(3 * 32_000_000);
}

/// How `blinky` arranges for the button (a GPIO edge) to be seen while idle.
#[cfg(feature = "timer")]
#[derive(Clone, Copy, PartialEq)]
pub enum ButtonWake {
    /// Whatever upstream embassy does by default (idle in STANDBY1).
    Default,
    /// Additionally set the port's FASTWAKE enable bit for the button pin.
    FastWake,
    /// Hold a `WakeGuard` so idle is capped at STANDBY0 (ULPCLK = LFCLK keeps running).
    Standby0,
    /// Hold a `WakeGuard` so idle is capped at STOP2.
    Stop2,
}

#[cfg(feature = "timer")]
static PRESS: embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

#[cfg(feature = "timer")]
#[embassy_executor::task]
async fn button_task(mut s2: embassy_mspm0::gpio::Input<'static>, mut rx: embassy_mspm0::gpio::Input<'static>) {
    use embassy_futures::select::{Either, select};
    let bump = |c: &AtomicU32| c.store(c.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    loop {
        // S2, or PA9 driven from the XDS110 backchannel UART (tools/unwedge/uart_pulse.py).
        match select(s2.wait_for_falling_edge(), rx.wait_for_falling_edge()).await {
            Either::First(()) => bump(&BTN_STATS.s2),
            Either::Second(()) => bump(&BTN_STATS.uart),
        }
        PRESS.signal(());
        // Debounce with the time driver: the chip sleeps here, possibly with the pin still low.
        embassy_time::Timer::after_millis(50).await;
    }
}

/// LED1 (PA0, active low) flashes 100 ms every 2 s. A press of S2 (PA14)
/// flashes it **immediately** (off the 2 s grid) and toggles a fast mode
/// (every 0.5 s). If presses are only noticed when the timer wakes the chip,
/// the flash stays on the grid and a short tap between flashes is lost.
#[cfg(feature = "timer")]
pub async fn blinky(spawner: embassy_executor::Spawner, wake: ButtonWake) -> ! {
    use embassy_futures::select::{Either, select};
    use embassy_mspm0::gpio::{Input, Level, Output, Pull};
    use embassy_mspm0::sysctl::{SleepLevel, WakeGuard};
    use embassy_time::Timer;

    attach_window();
    let p = embassy_mspm0::init(embassy_mspm0::Config::default());
    let mut led = Output::new(p.PA0, Level::High); // off
    let s2 = Input::new(p.PA14, Pull::Up);
    let rx = Input::new(p.PA9, Pull::Up);
    // Same encoding as BTN_CFG, plus bit 4 = "timer-based blinky firmware".
    BTN_STATS.mode.store(
        0x10 | match wake {
            ButtonWake::Default => 0x000,
            ButtonWake::FastWake => 0x001,
            ButtonWake::Standby0 => 0x100,
            ButtonWake::Stop2 => 0x200,
        },
        Ordering::Relaxed,
    );
    BTN_STATS.state.store(1, Ordering::Relaxed);
    BTN_STATS.magic.store(0xB07C_0DE5, Ordering::Relaxed);
    let _guard = match wake {
        ButtonWake::Default => None,
        ButtonWake::FastWake => {
            embassy_mspm0::pac::GPIOA.fastwake().modify(|w| {
                w.set_din(14, true);
                w.set_din(9, true);
            });
            None
        }
        // A guard at a level forbids that level and everything deeper.
        ButtonWake::Standby0 => Some(WakeGuard::new(SleepLevel::Standby1)),
        ButtonWake::Stop2 => Some(WakeGuard::new(SleepLevel::Standby0)),
    };
    spawner.spawn(button_task(s2, rx).unwrap());

    let mut fast = false;
    loop {
        led.set_low();
        Timer::after_millis(100).await;
        led.set_high();
        let gap = if fast { 400 } else { 1900 };
        if let Either::Second(()) = select(Timer::after_millis(gap), PRESS.wait()).await {
            fast = !fast;
            defmt::info!("S2 pressed, fast={}", fast);
        }
    }
}

// ---------------------------------------------------------------------------
// Edge-wake test (no time driver): see docs/LOWPOWER_RESULTS.md
// ---------------------------------------------------------------------------

/// Read over SWD by `tools/unwedge/unwedge.py peek` (symbol `BTN_STATS`).
#[repr(C)]
pub struct BtnStats {
    /// 0xB07C0DE5 once `edge_test` has started.
    pub magic: AtomicU32,
    /// The config word in effect (see [`BTN_CFG`]).
    pub mode: AtomicU32,
    /// Falling edges registered on S2 (PA14).
    pub s2: AtomicU32,
    /// 1 = idle, waiting for an edge; 2 = awake, handling one.
    pub state: AtomicU32,
    /// Falling edges registered on PA9 (driven by the XDS110 backchannel UART TXD).
    pub uart: AtomicU32,
}

#[used]
#[unsafe(no_mangle)]
pub static BTN_STATS: BtnStats = BtnStats {
    magic: AtomicU32::new(0),
    mode: AtomicU32::new(0),
    s2: AtomicU32::new(0),
    state: AtomicU32::new(0),
    uart: AtomicU32::new(0),
};

/// Test configuration, written over SWD and kept across an NRST reset
/// (`.uninit` is not zeroed by the runtime): `[0xC0F16A11, word]`.
///
/// word bits 11:8  idle cap: 0 none (STANDBY1), 1 STANDBY0, 2 STOP2, 3 STOP0, 4 WFI only
///      bit 0      set GPIOA.FASTWAKE for both test pins
///      bit 1      go back to sleep immediately, without waiting for the pin to go high
///      bits 31:16 (with bit 1) stay awake this many x 10 us after the edge before re-arming
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".uninit.BTN_CFG")]
pub static mut BTN_CFG: [core::mem::MaybeUninit<u32>; 2] = [core::mem::MaybeUninit::uninit(); 2];

/// Count falling edges on S2 (PA14) and on PA9, toggling LED1 (PA0) on each.
///
/// Built without a time driver, so a GPIO edge is the only thing that can wake
/// the chip. By default the CPU then spins, awake, until both pins are high
/// again: one pulse is exactly one count and the chip never re-enters sleep
/// with a pin still asserted. Config bit 1 removes that wait.
pub async fn edge_test() -> ! {
    use embassy_futures::select::{Either, select};
    use embassy_mspm0::gpio::{Input, Level, Output, Pull};
    use embassy_mspm0::sysctl::{SleepLevel, WakeGuard};

    const MS: u32 = 32_000; // CPU cycles per millisecond at 32 MHz

    attach_window();
    let p = embassy_mspm0::init(embassy_mspm0::Config::default());
    let cfg = unsafe {
        let c = core::ptr::addr_of!(BTN_CFG) as *const u32;
        if c.read_volatile() == 0xC0F1_6A11 { c.add(1).read_volatile() } else { 0 }
    };
    let (cap, fastwake, resleep) = ((cfg >> 8) & 0xF, cfg & 1 != 0, cfg & 2 != 0);

    let mut led = Output::new(p.PA0, Level::High); // active low: off
    let mut s2 = Input::new(p.PA14, Pull::Up);
    let mut rx = Input::new(p.PA9, Pull::Up);
    if fastwake {
        embassy_mspm0::pac::GPIOA.fastwake().modify(|w| {
            w.set_din(14, true);
            w.set_din(9, true);
        });
    }
    // A guard at a level forbids that level and everything deeper.
    let _guard = match cap {
        1 => Some(WakeGuard::new(SleepLevel::Standby1)),
        2 => Some(WakeGuard::new(SleepLevel::Standby0)),
        3 => Some(WakeGuard::new(SleepLevel::Stop1)),
        4 => Some(WakeGuard::new(SleepLevel::Stop0)),
        _ => None,
    };
    BTN_STATS.mode.store(cfg, Ordering::Relaxed);
    BTN_STATS.magic.store(0xB07C_0DE5, Ordering::Relaxed);

    let bump = |c: &AtomicU32| c.store(c.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    loop {
        BTN_STATS.state.store(1, Ordering::Relaxed);
        let which = select(s2.wait_for_falling_edge(), rx.wait_for_falling_edge()).await;
        BTN_STATS.state.store(2, Ordering::Relaxed);
        match which {
            Either::First(()) => bump(&BTN_STATS.s2),
            Either::Second(()) => bump(&BTN_STATS.uart),
        }
        led.toggle();
        if !resleep {
            cortex_m::asm::delay(20 * MS);
            while s2.is_low() || rx.is_low() {}
            cortex_m::asm::delay(20 * MS);
        } else {
            // Stay awake for (cfg >> 16) x 10 us after the edge before re-arming.
            cortex_m::asm::delay((cfg >> 16) * (MS / 100));
        }
    }
}
