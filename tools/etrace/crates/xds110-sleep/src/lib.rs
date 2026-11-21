//! Debug access to a TI MSPM0 that is asleep, through the XDS110 probe of a
//! LaunchPad.
//!
//! In STOP and STANDBY the chip power-gates the AHB-AP, the access port a
//! debugger reads memory through: a generic SWD tool (probe-rs 0.32, for
//! one) finds a valid debug port and then gets a FAULT on its first memory
//! access. Bit 3 of `DPREC0` in the vendor PWR-AP (FORCEACTIVE) keeps the
//! chip out of low power, so [`wake`] sets it and a debugger can attach.
//! That bit and a pending debug power-up request also keep the chip awake
//! afterwards, so [`release`] takes both back, and pulses NRST, before a
//! current measurement means anything.
//!
//! ```no_run
//! # fn main() -> Result<(), xds110_sleep::Error> {
//! let probe = xds110_sleep::probe::resolve(Some("0451:bef3:ML130001"))?;
//! let mut dap = xds110_sleep::Dap::open(&probe)?;
//! if xds110_sleep::status(&mut dap)?.asleep {
//!     xds110_sleep::wake(&mut dap)?;
//! }
//! drop(dap); // the probe is free for the debugger now; the chip stays awake
//! // ... attach, debug, detach ...
//! xds110_sleep::release(&mut xds110_sleep::Dap::open(&probe)?)?; // resets the chip
//! # Ok(())
//! # }
//! ```
//!
//! # What gets written
//!
//! [`status`], [`wake`] and [`release`] write DP `ABORT`, `SELECT` and
//! `CTRL/STAT`, the PWR-AP's `DPREC0`, and through the AHB-AP nothing but
//! `DHCSR` (only `release`, to withdraw a halt request); `release` also
//! drives NRST. [`wake`] never halts or resets the chip. [`catch`],
//! [`wake_or_catch`], [`dump`], [`run_to_hardfault`] and the register reads
//! ([`core_register`], [`halted_registers`]) are for a halted core and also
//! write `DEMCR` and `DCRSR`. Nothing in this crate writes flash,
//! NONMAIN or the DSSM mailbox, so nothing here can erase a chip or lock it.
//! The tests run the operations against a simulated chip and check this.
//!
//! # Limits
//!
//! - Linux only: probes are found in sysfs and CMSIS-DAP goes through
//!   `/dev/hidraw*`, which the user must be allowed to open. Elsewhere
//!   [`probe::list`] is empty and [`Dap::open`] fails with
//!   [`Error::Unsupported`]. Other transports can implement [`Transport`].
//! - One user of a probe at a time. Opening a [`Dap`] and dropping it sends
//!   `DAP_Connect` and `DAP_Disconnect`, which would cut off a debugger that
//!   is using the same probe. Finish here before a debugger attaches, and
//!   come back after it has detached.
//! - The probe now and then fails the first command after it was used by
//!   another program; [`retry`] covers that.
//! - Tested on LP-MSPM0L1306 and LP-MSPM0C1104.
//!
//! The evidence for the register bits is in `docs/UNWEDGE.md` of the
//! repository this crate lives in.

#![warn(missing_docs)]

use std::time::{Duration, Instant};

pub mod dap;
mod error;
#[cfg(target_os = "linux")]
mod hidraw;
pub mod probe;

pub use dap::{Dap, Transport};
pub use error::{Error, Result};
#[cfg(target_os = "linux")]
pub use hidraw::Hidraw;
pub use probe::Probe;

/// Access port number of the PWR-AP (IDR 0x002e0002).
pub const PWR_AP: u8 = 4;
/// The PWR-AP register that holds [`FORCEACTIVE`].
pub const DPREC0: u8 = 0x00;
/// DPREC0 bit that keeps the chip out of low power.
pub const FORCEACTIVE: u32 = 1 << 3;
/// DPREC0 bits 13..16: with these set the core waits in reset after NRST is released.
const WAIT_IN_RESET: u32 = 0x1e000;

const AP_IDR: u8 = 0xfc;

/// Debug Halting Control and Status Register: the only memory address
/// [`release`] writes, and [`status`] and [`wake`] write none.
pub const DHCSR: u32 = 0xe000_edf0;
const DCRSR: u32 = 0xe000_edf4;
const DCRDR: u32 = 0xe000_edf8;
const DEMCR: u32 = 0xe000_edfc;
/// The key every write to [`DHCSR`] carries; written alone it withdraws a halt request.
pub const DHCSR_KEY: u32 = 0xa05f_0000;
const C_DEBUGEN: u32 = 1;
const C_HALT: u32 = 2;
const S_HALT: u32 = 1 << 17;
const VC_CORERESET: u32 = 1;
const VC_HARDERR: u32 = 1 << 10;
const TRCENA: u32 = 1 << 24;

const SYSCTL: u32 = 0x400a_f000;
const NONMAIN: u32 = 0x41c0_0000;

/// IDR of the AHB-AP, or `None` while it is power-gated.
fn ahb_ap(d: &mut Dap) -> Result<Option<u32>> {
    d.unless_fault(|d| d.ap_read(0, AP_IDR))
}

/// Read core register `n` of a halted core (13 SP, 14 LR, 15 PC, 16 xPSR),
/// through `DCRSR` and `DCRDR`.
pub fn core_register(d: &mut Dap, n: u32) -> Result<u32> {
    d.mem_write32(DCRSR, n)?;
    d.pause(1);
    d.mem_read32(DCRDR)
}

/// Run `op` on the debug port of `probe`, up to three times: the probe
/// occasionally fails its first CMSIS-DAP command after another program used
/// it. Returns the last error if none of the attempts worked.
///
/// Only for operations that are safe to repeat, as [`status`], [`wake`] and
/// [`release`] are.
pub fn retry<T>(probe: &Probe, mut op: impl FnMut(&mut Dap) -> Result<T>) -> Result<T> {
    let mut attempt = 0;
    loop {
        match Dap::open(probe).and_then(|mut d| op(&mut d)) {
            Ok(v) => return Ok(v),
            Err(e @ (Error::PermissionDenied { .. } | Error::NoHidraw { .. } | Error::Unsupported)) => return Err(e),
            Err(e) if attempt == 2 => return Err(e),
            Err(_) => {}
        }
        attempt += 1;
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// What [`status`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// The debug port's ID register.
    pub dpidr: u32,
    /// DP CTRL/STAT as found, before this check asked for power itself.
    pub ctrl_stat_before: u32,
    /// DP CTRL/STAT with debug and system power-up requested.
    pub ctrl_stat: u32,
    /// IDR of access ports 0 to 4; `None` where the read faulted. On an
    /// MSPM0: AHB-AP, CFG-AP, SEC-AP, an ET-AP on some chips, PWR-AP.
    pub ap_idr: [Option<u32>; 5],
    /// PWR-AP DPREC0.
    pub dprec0: u32,
    /// The AHB-AP is power-gated: the chip is in STOP or STANDBY.
    pub asleep: bool,
}

impl Status {
    /// FORCEACTIVE is set: [`wake`], here or in another tool, is keeping the chip awake.
    pub fn force_active(&self) -> bool {
        self.dprec0 & FORCEACTIVE != 0
    }

    /// A debug power-up request was pending before the check: a debugger is
    /// attached, or [`wake`] was used and not released.
    pub fn debug_powered(&self) -> bool {
        self.ctrl_stat_before & dap::POWER_UP_REQUEST != 0
    }
}

/// Read-only: the debug port, the access ports and whether the chip is
/// asleep. Reading the access ports takes a debug power-up request; it is
/// taken back afterwards unless one was there already.
pub fn status(d: &mut Dap) -> Result<Status> {
    let dpidr = d.connect_unpowered()?;
    let ctrl_stat_before = d.ctrl_stat()?;
    d.connect()?;
    let ctrl_stat = d.ctrl_stat()?;
    let mut ap_idr = [None; 5];
    for (ap, idr) in ap_idr.iter_mut().enumerate() {
        *idr = d.unless_fault(|d| d.ap_read(ap as u8, AP_IDR))?;
    }
    let dprec0 = d.ap_read(PWR_AP, DPREC0)?;
    let asleep = ahb_ap(d)?.is_none();
    let status = Status {
        dpidr,
        ctrl_stat_before,
        ctrl_stat,
        ap_idr,
        dprec0,
        asleep,
    };
    if !status.debug_powered() {
        d.power_down()?;
    }
    Ok(status)
}

/// What [`wake`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Woken {
    /// PWR-AP DPREC0 as found.
    pub dprec0_before: u32,
    /// PWR-AP DPREC0 once the chip answered.
    pub dprec0_after: u32,
    /// IDR of the AHB-AP, which answers now.
    pub ahb_ap_idr: u32,
    /// Only [`wake_or_catch`] sets this: FORCEACTIVE was not enough, the
    /// chip was reset and is now halted at its reset vector.
    pub caught_after_reset: bool,
}

/// Force the chip active so that the AHB-AP answers and a debugger can
/// attach. The firmware keeps running: nothing is halted or reset. The hold
/// stays after the [`Dap`] is dropped and until [`release`].
///
/// Fails with [`Error::NotResponding`] if the chip does not answer within 50
/// polls (half a second).
pub fn wake(d: &mut Dap) -> Result<Woken> {
    d.connect()?;
    let dprec0_before = d.ap_read(PWR_AP, DPREC0)?;
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)?;
    for _ in 0..50 {
        if let Some(ahb_ap_idr) = ahb_ap(d)? {
            return Ok(Woken {
                dprec0_before,
                dprec0_after: d.ap_read(PWR_AP, DPREC0)?,
                ahb_ap_idr,
                caught_after_reset: false,
            });
        }
        d.pause(1);
    }
    Err(Error::NotResponding { dprec0_before })
}

/// [`wake`], and if the chip does not answer, [`catch`] it: **that resets
/// the chip and halts it** at its reset vector. For getting a debugger onto a
/// firmware that crashes at boot and resets in a loop.
pub fn wake_or_catch(d: &mut Dap) -> Result<Woken> {
    let dprec0_before = match wake(d) {
        Err(Error::NotResponding { dprec0_before }) => dprec0_before,
        other => return other,
    };
    catch(d)?;
    let ahb_ap_idr = ahb_ap(d)?.ok_or(Error::NotResponding { dprec0_before })?;
    Ok(Woken {
        dprec0_before,
        dprec0_after: d.ap_read(PWR_AP, DPREC0)?,
        ahb_ap_idr,
        caught_after_reset: true,
    })
}

/// What [`release`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Released {
    /// The core's debug register could be reached and a halt request
    /// withdrawn. If not, the reset still clears the debug state.
    pub halt_withdrawn: bool,
}

/// Undo [`wake`] and whatever a debugger left: withdraw a halt request, drop
/// FORCEACTIVE and the debug power-up request, then pulse NRST. **The
/// firmware restarts.**
pub fn release(d: &mut Dap) -> Result<Released> {
    d.connect()?;
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)?; // make sure the core is reachable...
    let mut halt_withdrawn = false;
    for _ in 0..50 {
        // ...so that a debugger's halt request can be withdrawn
        if d.unless_fault(|d| d.mem_write32(DHCSR, DHCSR_KEY))?.is_some() {
            halt_withdrawn = true;
            break;
        }
        d.pause(2);
    }
    d.ap_write(PWR_AP, DPREC0, 0)?;
    d.power_down()?;
    pulse_nrst(d)?;
    Ok(Released { halt_withdrawn })
}

/// Pulse NRST for five ticks (50 ms). Shorter than 1 s, so the chip takes it
/// as a BOOTRST and not as a POR. Use [`reset`] on a fresh [`Dap`].
pub fn pulse_nrst(d: &mut Dap) -> Result<()> {
    d.nrst(false)?;
    d.pause(5);
    d.nrst(true)
}

/// Pulse NRST and nothing else: no debug power-up is requested.
pub fn reset(d: &mut Dap) -> Result<()> {
    d.connect_unpowered()?;
    pulse_nrst(d)
}

/// Core and sleep state read by [`dump`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dump {
    /// How the chip was woken.
    pub woken: Woken,
    /// DHCSR after the halt request.
    pub dhcsr: u32,
    /// Program counter.
    pub pc: u32,
    /// Link register.
    pub lr: u32,
    /// Stack pointer.
    pub sp: u32,
    /// SYSCTL and core registers that decide how the chip sleeps, by name.
    pub registers: Vec<(&'static str, u32)>,
    /// The first two words of main flash: initial SP and reset vector.
    pub vectors: [u32; 2],
    /// The first three words of NONMAIN.
    pub nonmain: [u32; 3],
}

impl Dump {
    /// Main flash is erased, so the boot ROM runs the bootstrap loader, which goes to STANDBY after 10 s.
    pub fn flash_blank(&self) -> bool {
        self.vectors[0] == 0xffff_ffff
    }
}

/// Wake the chip ([`wake_or_catch`]), **halt it**, and read where it is and
/// how it is set up to sleep. Leaves the core halted; [`release`] lets it go.
pub fn dump(d: &mut Dap) -> Result<Dump> {
    let woken = wake_or_catch(d)?;
    d.mem_write32(DHCSR, DHCSR_KEY | C_HALT | C_DEBUGEN)?;
    d.pause(5);
    let dhcsr = d.mem_read32(DHCSR)?;
    let (pc, lr, sp) = (core_register(d, 15)?, core_register(d, 14)?, core_register(d, 13)?);
    let mut registers = Vec::new();
    for (name, addr) in [
        ("SYSOSCCFG", SYSCTL + 0x1100),
        ("MCLKCFG", SYSCTL + 0x1104),
        ("GENCLKEN", SYSCTL + 0x113c),
        ("PMODECFG", SYSCTL + 0x1140),
        ("CLKSTATUS", SYSCTL + 0x1204),
        ("SCB.SCR", 0xe000_ed10),
        ("VTOR", 0xe000_ed08),
    ] {
        registers.push((name, d.mem_read32(addr)?));
    }
    Ok(Dump {
        woken,
        dhcsr,
        pc,
        lr,
        sp,
        registers,
        vectors: [d.mem_read32(0)?, d.mem_read32(4)?],
        nonmain: [
            d.mem_read32(NONMAIN)?,
            d.mem_read32(NONMAIN + 4)?,
            d.mem_read32(NONMAIN + 8)?,
        ],
    })
}

/// One change seen by [`watch`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchEvent {
    /// Time since the reset.
    pub at: Duration,
    /// Whether the AHB-AP answers.
    pub reachable: bool,
    /// PWR-AP DPREC0.
    pub dprec0: u32,
}

/// Pulse NRST, then poll for `duration` and report every change in whether
/// the chip is reachable: how long a firmware stays awake after reset.
pub fn watch(d: &mut Dap, duration: Duration, mut on_change: impl FnMut(WatchEvent)) -> Result<()> {
    pulse_nrst(d)?;
    let started = Instant::now();
    let mut last = None;
    while started.elapsed() < duration {
        d.connect()?;
        let state = (ahb_ap(d)?.is_some(), d.ap_read(PWR_AP, DPREC0)?);
        if last != Some(state) {
            on_change(WatchEvent {
                at: started.elapsed(),
                reachable: state.0,
                dprec0: state.1,
            });
            last = Some(state);
        }
        d.power_down()?;
        d.pause(20);
    }
    Ok(())
}

/// Catch the chip right after reset and **halt it** at its reset vector.
///
/// For a firmware that faults at boot: the chip then resets in a loop (a
/// HardFault inside HardFault locks the core up, which resets the chip), is
/// awake only a few ms per turn, and FORCEACTIVE does not hold. What works:
/// hold NRST low, set FORCEACTIVE together with the wait-in-reset bits,
/// release NRST, request a halt with the reset vector caught, then drop the
/// wait-in-reset bits. Found on an MSPM0C1104 linked with too much RAM.
///
/// Leaves the core halted with FORCEACTIVE set, ready for a debugger.
pub fn catch(d: &mut Dap) -> Result<()> {
    for _ in 0..10 {
        d.connect()?;
        d.nrst(false)?;
        d.pause(5);
        d.unless_fault(|d| d.ap_write(PWR_AP, DPREC0, WAIT_IN_RESET | FORCEACTIVE))?;
        d.nrst(true)?;
        let released = Instant::now();
        while released.elapsed() < Duration::from_millis(500) {
            let halted = d.unless_fault(|d| {
                d.ap_write(PWR_AP, DPREC0, WAIT_IN_RESET | FORCEACTIVE)?;
                // The halt request stays pending while the core waits in reset.
                d.mem_write32(DHCSR, DHCSR_KEY | C_HALT | C_DEBUGEN)?;
                d.mem_write32(DEMCR, TRCENA | VC_HARDERR | VC_CORERESET)?;
                d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)?; // let the core out of reset
                for _ in 0..50 {
                    if d.mem_read32(DHCSR)? & S_HALT != 0 {
                        return Ok(true);
                    }
                    d.pause(1);
                }
                Ok(false)
            })?;
            if halted == Some(true) {
                return Ok(());
            }
        }
    }
    Err(Error::CatchFailed)
}

/// Core registers of a halted chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Halted {
    /// Program counter.
    pub pc: u32,
    /// Link register.
    pub lr: u32,
    /// Stack pointer.
    pub sp: u32,
    /// Program status register.
    pub xpsr: u32,
    /// Twelve words from the stack pointer up: at a HardFault the first eight are the exception frame.
    pub stack: Vec<u32>,
}

/// Read the registers of a core that is halted (after [`catch`], say).
pub fn halted_registers(d: &mut Dap) -> Result<Halted> {
    let (pc, lr, sp, xpsr) = (
        core_register(d, 15)?,
        core_register(d, 14)?,
        core_register(d, 13)?,
        core_register(d, 16)?,
    );
    Ok(Halted {
        pc,
        lr,
        sp,
        xpsr,
        stack: (0..12).map(|k| d.mem_read32(sp + 4 * k)).collect::<Result<_>>()?,
    })
}

/// How [`run_to_hardfault`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The core halted, at a HardFault unless a debugger set something else up.
    Halted {
        /// Time since the core was let go.
        after: Duration,
        /// Where it stopped.
        registers: Halted,
    },
    /// Debug access was lost: the chip reset or went to sleep.
    LostAccess {
        /// Time since the core was let go.
        after: Duration,
    },
    /// Still running when the time was up; halted now.
    StillRunning {
        /// Where it was halted.
        registers: Halted,
    },
}

/// Let a chip halted by [`catch`] run for up to `limit`, stopping at a HardFault.
pub fn run_to_hardfault(d: &mut Dap, limit: Duration) -> Result<RunOutcome> {
    d.mem_write32(DEMCR, TRCENA | VC_HARDERR)?;
    d.mem_write32(DHCSR, DHCSR_KEY | C_DEBUGEN)?;
    let started = Instant::now();
    while started.elapsed() < limit {
        d.pause(10);
        let Some(dhcsr) = d.unless_fault(|d| d.mem_read32(DHCSR))? else {
            return Ok(RunOutcome::LostAccess {
                after: started.elapsed(),
            });
        };
        if dhcsr & S_HALT != 0 {
            return Ok(RunOutcome::Halted {
                after: started.elapsed(),
                registers: halted_registers(d)?,
            });
        }
    }
    d.mem_write32(DHCSR, DHCSR_KEY | C_HALT | C_DEBUGEN)?;
    Ok(RunOutcome::StillRunning {
        registers: halted_registers(d)?,
    })
}

#[cfg(test)]
mod sim;
