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
    spin_us(3_000_000);
}

/// Busy-wait with the CPU awake. `cortex_m::asm::delay(n)` takes 3 cycles per
/// count on Cortex-M0+ (measured: a nominal 3 s came out as 9.0 s), and SYSOSC
/// is 32 MHz, so one microsecond is 32/3 counts.
pub fn spin_us(us: u32) {
    cortex_m::asm::delay(((us as u64 * 32) / 3) as u32);
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
    /// Times the task was woken by any GPIO event (includes rising edges in the
    /// strategies that arm them).
    pub wakes: AtomicU32,
    /// Default mode only: loop iterations PA9 stayed low after the last edge
    /// (a relative measure of pulse width; calibrate against a known pulse).
    pub low_loops: AtomicU32,
}

#[used]
#[unsafe(no_mangle)]
pub static BTN_STATS: BtnStats = BtnStats {
    magic: AtomicU32::new(0),
    mode: AtomicU32::new(0),
    s2: AtomicU32::new(0),
    state: AtomicU32::new(0),
    uart: AtomicU32::new(0),
    wakes: AtomicU32::new(0),
    low_loops: AtomicU32::new(0),
};

/// Test configuration, written over SWD and kept across an NRST reset
/// (`.uninit` is not zeroed by the runtime): `[0xC0F16A11, word]`.
///
/// word bits 11:8  idle cap: 0 none (STANDBY1), 1 STANDBY0, 2 STOP2, 3 STOP0, 4 WFI only
///      bit 0      set GPIOA.FASTWAKE for both test pins
///      bit 1      go back to sleep immediately, without waiting for the pin to go high
///      bits 31:16 (with bit 1) stay awake this many x 10 us (true time) after the edge before re-arming
///      bits 15:12 (with bit 1) re-arm strategy for PA9:
///                 0 plain `wait_for_falling_edge`
///                 1 read DIN, clear the interrupt, read DIN and RIS, then as 0
///                 2 if the pin is low, sleep until it is high (rising edge), then as 0
///                 3 arm both edges; count the wake only if the pin is then low
///                 4 arm-then-check: arm the edge, then read the pin, and sleep only
///                   if the level agrees with the armed edge (falling+high, rising+low)
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
    let strategy = (cfg >> 12) & 0xF;
    if resleep && strategy == 4 {
        // Arm-then-check on PA9 only. Invariant on every sleep: the armed edge
        // and the pin level agree, so no transition can fall between a level
        // check and the arming of the interrupt.
        use core::future::{Future, poll_fn};
        use core::pin::pin;
        use core::task::Poll;
        let g = embassy_mspm0::pac::GPIOA;
        let high = || g.din31_0().read().0 & (1 << 9) != 0;
        let latched = || g.cpu_int().ris().read().0 & (1 << 9) != 0;
        loop {
            {
                BTN_STATS.state.store(1, Ordering::Relaxed);
                let mut f = pin!(rx.wait_for_falling_edge());
                // First poll arms: polarity, clear, pending check, unmask.
                let armed = poll_fn(|cx| Poll::Ready(f.as_mut().poll(cx))).await;
                if armed.is_ready() || high() || latched() {
                    f.await; // armed for falling with the pin high, or the edge is already latched
                    BTN_STATS.state.store(2, Ordering::Relaxed);
                    bump(&BTN_STATS.wakes);
                    bump(&BTN_STATS.uart);
                    led.toggle();
                    spin_us((cfg >> 16) * 10);
                    continue;
                }
                // Pin was already low when we armed: a falling edge cannot come until it rises.
            }
            let mut r = pin!(rx.wait_for_rising_edge());
            let armed = poll_fn(|cx| Poll::Ready(r.as_mut().poll(cx))).await;
            if armed.is_pending() && !high() {
                BTN_STATS.state.store(3, Ordering::Relaxed);
                r.await; // armed for rising with the pin low
                bump(&BTN_STATS.wakes);
            }
        }
    }
    loop {
        if resleep && strategy == 1 {
            let g = embassy_mspm0::pac::GPIOA;
            let _ = g.din31_0().read();
            g.cpu_int().iclr().write(|w| w.set_dio(9, true));
            let _ = g.din31_0().read();
            let _ = g.cpu_int().ris().read();
        }
        if resleep && strategy == 2 && rx.is_low() {
            BTN_STATS.state.store(3, Ordering::Relaxed);
            rx.wait_for_high().await;
            bump(&BTN_STATS.wakes);
        }
        BTN_STATS.state.store(1, Ordering::Relaxed);
        let which = if resleep && strategy == 3 {
            select(s2.wait_for_falling_edge(), rx.wait_for_any_edge()).await
        } else {
            select(s2.wait_for_falling_edge(), rx.wait_for_falling_edge()).await
        };
        BTN_STATS.state.store(2, Ordering::Relaxed);
        bump(&BTN_STATS.wakes);
        match which {
            Either::First(()) => bump(&BTN_STATS.s2),
            Either::Second(()) => {
                if !(resleep && strategy == 3) || rx.is_low() {
                    bump(&BTN_STATS.uart)
                }
            }
        }
        led.toggle();
        if !resleep {
            let mut n = 0u32;
            while rx.is_low() {
                n += 1;
            }
            BTN_STATS.low_loops.store(n, Ordering::Relaxed);
            spin_us(20_000);
            while s2.is_low() || rx.is_low() {}
            spin_us(20_000);
        } else {
            // Stay awake for (cfg >> 16) x 10 us after the edge before re-arming.
            spin_us((cfg >> 16) * 10);
        }
    }
}
