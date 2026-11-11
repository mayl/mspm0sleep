//! RUN-mode reference load: HAL init, then spin forever. No executor.
#![no_std]
#![no_main]

use embassy_mspm0::Config;
use mspm0sleep_firmware as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    let _p = embassy_mspm0::init(Config::default());
    loop {
        cortex_m::asm::nop();
    }
}
