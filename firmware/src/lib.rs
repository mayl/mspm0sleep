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
