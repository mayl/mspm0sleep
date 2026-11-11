//! As lp_blinky, with idle capped at STOP2 by a WakeGuard.
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use mspm0sleep_firmware::{ButtonWake, blinky};

#[cfg_attr(feature = "lp", embassy_executor::main(executor = "embassy_mspm0::executor::Executor", entry = "cortex_m_rt::entry"))]
#[cfg_attr(feature = "wfi", embassy_executor::main)]
async fn main(spawner: Spawner) -> ! {
    blinky(spawner, ButtonWake::Stop2).await
}
