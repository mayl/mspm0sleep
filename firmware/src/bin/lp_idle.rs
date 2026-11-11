//! Sleep-floor measurement with the executor: one task that wakes every 5 s
//! and does nothing. No GPIO driven. With the `lp` feature the executor idles
//! in STANDBY1 (TIMG0 on LFCLK is the wake source); with `wfi` it idles in SLEEP.
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_mspm0::Config;
use embassy_time::Timer;
use mspm0sleep_firmware::attach_window;

#[cfg_attr(feature = "lp", embassy_executor::main(executor = "embassy_mspm0::executor::Executor", entry = "cortex_m_rt::entry"))]
#[cfg_attr(feature = "wfi", embassy_executor::main)]
async fn main(_spawner: Spawner) -> ! {
    attach_window();
    let _p = embassy_mspm0::init(Config::default());
    let mut n = 0u32;
    loop {
        Timer::after_secs(5).await;
        n += 1;
        defmt::info!("wake {}", n);
    }
}
