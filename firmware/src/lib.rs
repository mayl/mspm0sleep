#![no_std]
//! Shared bits for the LP-MSPM0L1306 measurement firmwares.

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
async fn button_task(mut s2: embassy_mspm0::gpio::Input<'static>) {
    loop {
        s2.wait_for_falling_edge().await;
        PRESS.signal(());
        embassy_time::Timer::after_millis(50).await; // debounce
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
    let _guard = match wake {
        ButtonWake::Default => None,
        ButtonWake::FastWake => {
            embassy_mspm0::pac::GPIOA.fastwake().modify(|w| w.set_din(14, true));
            None
        }
        // A guard at a level forbids that level and everything deeper.
        ButtonWake::Standby0 => Some(WakeGuard::new(SleepLevel::Standby1)),
        ButtonWake::Stop2 => Some(WakeGuard::new(SleepLevel::Standby0)),
    };
    spawner.spawn(button_task(s2).unwrap());

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
// Button-wake test (no time driver): see docs/LOWPOWER_RESULTS.md
// ---------------------------------------------------------------------------

use core::sync::atomic::{AtomicU32, Ordering};

/// Read over SWD by `tools/unwedge/unwedge.py peek` (symbol `BTN_STATS`).
#[repr(C)]
pub struct BtnStats {
    /// 0xB07C0DE5 once `button_count` has started.
    pub magic: AtomicU32,
    /// `(cap << 8) | fastwake`, cap: 0 none (STANDBY1), 1 STANDBY0, 2 STOP2, 3 STOP0, 4 no deep sleep.
    pub mode: AtomicU32,
    /// Presses registered (falling edges that completed `wait_for_falling_edge`).
    pub presses: AtomicU32,
    /// 1 = idle, waiting for an edge; 2 = awake, waiting for the button to be released.
    pub state: AtomicU32,
}

#[used]
#[unsafe(no_mangle)]
pub static BTN_STATS: BtnStats = BtnStats {
    magic: AtomicU32::new(0),
    mode: AtomicU32::new(0),
    presses: AtomicU32::new(0),
    state: AtomicU32::new(0),
};

/// Deepest idle mode the executor may use during the button test.
#[derive(Clone, Copy)]
pub enum Cap {
    /// No guard: the executor idles in STANDBY1 (upstream default).
    Standby1 = 0,
    Standby0 = 1,
    Stop2 = 2,
    Stop0 = 3,
    /// All deep sleep blocked: idle is a plain WFI. Control case.
    Wfi = 4,
}

/// Count presses of S2 (PA14) and toggle LED1 (PA0) on each one.
///
/// Built without a time driver, so a GPIO edge is the only thing that can wake
/// the chip. Debounce uses no sleep at all: after an edge the CPU spins until
/// the button is released, so one physical press is exactly one count and the
/// chip never goes back to sleep with the pin still asserted.
pub async fn button_count(cap: Cap, fastwake: bool) -> ! {
    use embassy_mspm0::gpio::{Input, Level, Output, Pull};
    use embassy_mspm0::sysctl::{SleepLevel, WakeGuard};

    const MS: u32 = 32_000; // CPU cycles per millisecond at 32 MHz

    attach_window();
    let p = embassy_mspm0::init(embassy_mspm0::Config::default());
    let mut led = Output::new(p.PA0, Level::High); // active low: off
    let mut s2 = Input::new(p.PA14, Pull::Up);
    if fastwake {
        embassy_mspm0::pac::GPIOA.fastwake().modify(|w| w.set_din(14, true));
    }
    // A guard at a level forbids that level and everything deeper.
    let _guard = match cap {
        Cap::Standby1 => None,
        Cap::Standby0 => Some(WakeGuard::new(SleepLevel::Standby1)),
        Cap::Stop2 => Some(WakeGuard::new(SleepLevel::Standby0)),
        Cap::Stop0 => Some(WakeGuard::new(SleepLevel::Stop1)),
        Cap::Wfi => Some(WakeGuard::new(SleepLevel::Stop0)),
    };
    BTN_STATS.mode.store(((cap as u32) << 8) | fastwake as u32, Ordering::Relaxed);
    BTN_STATS.magic.store(0xB07C_0DE5, Ordering::Relaxed);

    loop {
        BTN_STATS.state.store(1, Ordering::Relaxed);
        s2.wait_for_falling_edge().await;
        BTN_STATS.state.store(2, Ordering::Relaxed);
        BTN_STATS.presses.store(BTN_STATS.presses.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        led.toggle();
        cortex_m::asm::delay(20 * MS);
        while s2.is_low() {}
        cortex_m::asm::delay(20 * MS);
    }
}
