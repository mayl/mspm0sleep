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

static PRESS: embassy_sync::signal::Signal<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, ()> =
    embassy_sync::signal::Signal::new();

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
