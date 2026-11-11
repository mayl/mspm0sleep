//! Functional check for deep-sleep wake sources: LED1 (PA0, active low)
//! flashes 100 ms every 2 s from the time driver, and S2 (PA14) toggles a
//! "fast mode" (flash every 500 ms) via a GPIO edge. Between events the
//! executor idles (STANDBY1 with `lp`, SLEEP with `wfi`).
#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_mspm0::Config;
use embassy_mspm0::gpio::{Input, Level, Output, Pull};
use embassy_time::Timer;
use mspm0sleep_firmware::attach_window;

static FAST: AtomicBool = AtomicBool::new(false);

#[embassy_executor::task]
async fn button(mut s2: Input<'static>) {
    loop {
        s2.wait_for_falling_edge().await;
        let fast = !FAST.load(Ordering::Relaxed);
        FAST.store(fast, Ordering::Relaxed);
        defmt::info!("S2 pressed, fast={}", fast);
        Timer::after_millis(50).await; // debounce
    }
}

#[cfg_attr(feature = "lp", embassy_executor::main(executor = "embassy_mspm0::executor::Executor", entry = "cortex_m_rt::entry"))]
#[cfg_attr(feature = "wfi", embassy_executor::main)]
async fn main(spawner: Spawner) -> ! {
    attach_window();
    let p = embassy_mspm0::init(Config::default());
    let mut led = Output::new(p.PA0, Level::High); // off
    spawner.spawn(button(Input::new(p.PA14, Pull::Up)).unwrap());

    loop {
        led.set_low();
        Timer::after_millis(100).await;
        led.set_high();
        Timer::after_millis(if FAST.load(Ordering::Relaxed) { 400 } else { 1900 }).await;
    }
}
