//! GPIO edge-wake test. See `mspm0sleep_firmware::edge_test`.
//! Build without the time driver: --no-default-features --features lp
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use mspm0sleep_firmware::edge_test;

#[embassy_executor::main(executor = "embassy_mspm0::executor::Executor", entry = "cortex_m_rt::entry")]
async fn main(_spawner: Spawner) -> ! {
    edge_test().await
}
