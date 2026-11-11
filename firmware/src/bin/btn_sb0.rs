//! Button-wake test: idle cap Standby0, FASTWAKE false. See `mspm0sleep_firmware::button_count`.
//! Build without the time driver: --no-default-features --features lp
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use mspm0sleep_firmware::{Cap, button_count};

#[embassy_executor::main(executor = "embassy_mspm0::executor::Executor", entry = "cortex_m_rt::entry")]
async fn main(_spawner: Spawner) -> ! {
    button_count(Cap::Standby0, false).await
}
