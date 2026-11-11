//! As lp_blinky, with GPIOA.FASTWAKE set for the button pin.
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use mspm0sleep_firmware::{ButtonWake, blinky};

#[cfg_attr(feature = "lp", embassy_executor::main(executor = "embassy_mspm0::executor::Executor", entry = "cortex_m_rt::entry"))]
#[cfg_attr(feature = "wfi", embassy_executor::main)]
async fn main(spawner: Spawner) -> ! {
    blinky(spawner, ButtonWake::FastWake).await
}
