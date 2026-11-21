//! Debug access to an MSPM0 that is asleep, and handing the chip back.
//!
//! In STOP and STANDBY the AHB-AP, the access port debuggers read memory
//! through, is power-gated: a generic SWD tool finds a valid debug port and
//! then gets a FAULT on its first memory access. Bit 3 of `DPREC0` in the
//! vendor PWR-AP (FORCEACTIVE) keeps the chip out of low power. That bit and a
//! pending debug power-up request also keep the chip awake afterwards, so both
//! have to be withdrawn before a current measurement means anything.
//!
//! Background and the evidence for the register bits: `docs/UNWEDGE.md`.

use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::board::Probe;
use crate::dap::{Dap, DapError};
use crate::Result;

/// The PWR-AP (IDR 0x002e0002) and its one register of interest.
const PWR_AP: u8 = 4;
const DPREC0: u8 = 0x00;
const FORCEACTIVE: u32 = 1 << 3;
/// DPREC0 bits 13..16: with these set the core waits in reset after NRST is released.
const WAIT_IN_RESET: u32 = 0x1e000;

const AP_IDR: u8 = 0xfc;

const DHCSR: u32 = 0xe000_edf0;
const DCRSR: u32 = 0xe000_edf4;
const DCRDR: u32 = 0xe000_edf8;
const DEMCR: u32 = 0xe000_edfc;
const DHCSR_KEY: u32 = 0xa05f_0000;
const C_DEBUGEN: u32 = 1;
const C_HALT: u32 = 2;
const S_HALT: u32 = 1 << 17;
const VC_CORERESET: u32 = 1;
const VC_HARDERR: u32 = 1 << 10;
const TRCENA: u32 = 1 << 24;

const SYSCTL: u32 = 0x400a_f000;
const NONMAIN: u32 = 0x41c0_0000;

const MS: Duration = Duration::from_millis(1);

/// IDR of the AHB-AP, or `None` while it is power-gated.
fn ahb_ap(d: &mut Dap) -> Result<Option<u32>> {
    match d.ap_read(0, AP_IDR) {
        Ok(idr) => Ok(Some(idr)),
        Err(DapError::Fault(_)) => {
            d.clear_errors()?;
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// Run `op`, turning a FAULT into `None` (with the sticky flags cleared).
fn unless_fault<T>(d: &mut Dap, op: impl FnOnce(&mut Dap) -> crate::dap::Result<T>) -> Result<Option<T>> {
    match op(d) {
        Ok(v) => Ok(Some(v)),
        Err(DapError::Fault(_)) => {
            d.clear_errors()?;
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

fn core_register(d: &mut Dap, n: u32) -> Result<u32> {
    d.mem_write32(DCRSR, n)?;
    sleep(10 * MS);
    Ok(d.mem_read32(DCRDR)?)
}

/// Read-only: the debug port, the access ports and whether the chip is asleep.
pub fn status(d: &mut Dap) -> Result<()> {
    let idr = d.connect_unpowered()?;
    // Reading the access ports takes a debug power-up request. Take it back afterwards unless
    // one was there already (a debugger is attached, or `wake` was used).
    let was_powered = d.ctrl_stat()? & 0x5000_0000 != 0;
    d.connect()?;
    println!("DPIDR            {idr:#010x}");
    println!("CTRL/STAT        {:#010x}", d.ctrl_stat()?);
    for ap in 0..5 {
        match unless_fault(d, |d| d.ap_read(ap, AP_IDR))? {
            Some(idr) => println!("AP{ap} IDR          {idr:#010x}"),
            None => println!("AP{ap} IDR          FAULT"),
        }
    }
    let v = d.ap_read(PWR_AP, DPREC0)?;
    println!(
        "PWR-AP DPREC0    {v:#010x}  (FORCEACTIVE {})",
        if v & FORCEACTIVE != 0 { "set" } else { "clear" }
    );
    println!(
        "=> AHB-AP {}",
        match ahb_ap(d)? {
            Some(_) => "reachable",
            None => "power-gated: chip is in STOP/STANDBY; use `wake`",
        }
    );
    if !was_powered {
        d.power_down()?;
    }
    Ok(())
}

/// Force the chip active so that the AHB-AP answers. The hold stays after this
/// program exits, so a debugger can attach; undo it with [`release`].
pub fn wake(d: &mut Dap) -> Result<()> {
    d.connect()?;
    let before = d.ap_read(PWR_AP, DPREC0)?;
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)?;
    let mut idr = None;
    for _ in 0..50 {
        idr = ahb_ap(d)?;
        if idr.is_some() {
            break;
        }
        sleep(10 * MS);
    }
    if idr.is_none() {
        // Seen on a firmware that faults at boot and resets in a loop: catch it right after reset.
        catch(d)?;
        idr = ahb_ap(d)?;
    }
    let idr = idr.ok_or("the AHB-AP does not answer, even right after reset")?;
    println!(
        "DPREC0 {before:#010x} -> {:#010x}, AHB-AP IDR {idr:#010x}: awake",
        d.ap_read(PWR_AP, DPREC0)?
    );
    Ok(())
}

/// Wake and halt the chip, and print where it is and how it is set up to sleep.
/// Leaves the core halted.
pub fn dump(d: &mut Dap) -> Result<()> {
    wake(d)?;
    d.mem_write32(DHCSR, DHCSR_KEY | C_HALT | C_DEBUGEN)?;
    sleep(50 * MS);
    println!("{:<16} {:#010x}", "DHCSR", d.mem_read32(DHCSR)?);
    for (n, name) in [(15, "PC"), (14, "LR"), (13, "SP")] {
        println!("{name:<16} {:#010x}", core_register(d, n)?);
    }
    for (name, addr) in [
        ("SYSOSCCFG", SYSCTL + 0x1100),
        ("MCLKCFG", SYSCTL + 0x1104),
        ("GENCLKEN", SYSCTL + 0x113c),
        ("PMODECFG", SYSCTL + 0x1140),
        ("CLKSTATUS", SYSCTL + 0x1204),
        ("SCB.SCR", 0xe000_ed10),
        ("VTOR", 0xe000_ed08),
    ] {
        println!("{name:<16} {:#010x}", d.mem_read32(addr)?);
    }
    let flash = [d.mem_read32(0)?, d.mem_read32(4)?];
    println!(
        "{:<16} {:#010x} {:#010x}{}",
        "flash[0],[4]",
        flash[0],
        flash[1],
        if flash[0] == 0xffff_ffff {
            "  <- BLANK: ROM BSL will run"
        } else {
            ""
        }
    );
    let nonmain: Vec<String> = (0..3)
        .map(|i| d.mem_read32(NONMAIN + 4 * i).map(|v| format!("{v:08x}")))
        .collect::<crate::dap::Result<_>>()?;
    println!("{:<16} {}", "NONMAIN[0..3]", nonmain.join(" "));
    println!("(core left halted; run `release` when done)");
    Ok(())
}

/// Pulse NRST. Shorter than 1 s, so the chip takes it as a BOOTRST and not as a POR.
pub fn pulse_nrst(d: &mut Dap) -> Result<()> {
    d.nrst(false)?;
    sleep(50 * MS);
    d.nrst(true)?;
    Ok(())
}

/// Pulse NRST and nothing else.
pub fn reset(d: &mut Dap) -> Result<()> {
    d.connect_unpowered()?;
    pulse_nrst(d)?;
    println!("NRST pulsed");
    Ok(())
}

/// Undo [`wake`] and whatever a debugger left: resume the core, drop
/// FORCEACTIVE and the debug power-up request, then reset.
pub fn release(d: &mut Dap) -> Result<()> {
    d.connect()?;
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)?; // make sure the core is reachable...
    let mut withdrawn = false;
    for _ in 0..50 {
        // ...so that a debugger's halt request can be withdrawn
        if unless_fault(d, |d| d.mem_write32(DHCSR, DHCSR_KEY))?.is_some() {
            withdrawn = true;
            break;
        }
        sleep(20 * MS);
    }
    if !withdrawn {
        println!("warning: could not reach DHCSR; relying on the reset to clear debug state");
    }
    d.ap_write(PWR_AP, DPREC0, 0)?;
    d.power_down()?;
    pulse_nrst(d)?;
    println!("released and reset");
    Ok(())
}

/// After an NRST pulse, report how long the AHB-AP stays reachable.
pub fn watch(d: &mut Dap, secs: f64) -> Result<()> {
    pulse_nrst(d)?;
    let started = Instant::now();
    let mut last = None;
    while started.elapsed().as_secs_f64() < secs {
        d.connect()?;
        let reachable = ahb_ap(d)?.is_some();
        let dprec0 = d.ap_read(PWR_AP, DPREC0)?;
        if last != Some((reachable, dprec0)) {
            println!(
                "t={:5.1}s  AHB-AP {:<9} DPREC0={dprec0:#010x}",
                started.elapsed().as_secs_f64(),
                if reachable { "reachable" } else { "FAULT" }
            );
            last = Some((reachable, dprec0));
        }
        d.power_down()?;
        sleep(200 * MS);
    }
    Ok(())
}

/// Catch the chip right after reset and halt it at its reset vector.
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
        sleep(50 * MS);
        unless_fault(d, |d| d.ap_write(PWR_AP, DPREC0, WAIT_IN_RESET | FORCEACTIVE))?;
        d.nrst(true)?;
        let released = Instant::now();
        while released.elapsed() < 500 * MS {
            let halted = unless_fault(d, |d| {
                d.ap_write(PWR_AP, DPREC0, WAIT_IN_RESET | FORCEACTIVE)?;
                // The halt request stays pending while the core waits in reset.
                d.mem_write32(DHCSR, DHCSR_KEY | C_HALT | C_DEBUGEN)?;
                d.mem_write32(DEMCR, TRCENA | VC_HARDERR | VC_CORERESET)?;
                d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)?; // let the core out of reset
                for _ in 0..50 {
                    if d.mem_read32(DHCSR)? & S_HALT != 0 {
                        return Ok(true);
                    }
                    sleep(10 * MS);
                }
                Ok(false)
            })?;
            if halted == Some(true) {
                return Ok(());
            }
        }
    }
    Err("could not halt the chip after reset".into())
}

/// [`catch`], then print where the core stopped. With `trace`, run on for up
/// to 6 s and stop at a HardFault.
pub fn catch_report(d: &mut Dap, trace: bool) -> Result<()> {
    catch(d)?;
    println!(
        "halted: PC {:#x} SP {:#x}",
        core_register(d, 15)?,
        core_register(d, 13)?
    );
    if !trace {
        return Ok(());
    }
    d.mem_write32(DEMCR, TRCENA | VC_HARDERR)?;
    d.mem_write32(DHCSR, DHCSR_KEY | C_DEBUGEN)?;
    for i in 1..=60 {
        sleep(100 * MS);
        let t = i as f64 / 10.0;
        let Some(dhcsr) = unless_fault(d, |d| d.mem_read32(DHCSR))? else {
            println!("t={t:.1} lost access");
            return Ok(());
        };
        if dhcsr & S_HALT != 0 {
            let sp = core_register(d, 13)?;
            println!(
                "t={t:.1} halted: PC {:#x} LR {:#x} SP {sp:#x} xPSR {:#x}",
                core_register(d, 15)?,
                core_register(d, 14)?,
                core_register(d, 16)?
            );
            let stack: Vec<String> = (0..12)
                .map(|k| d.mem_read32(sp + 4 * k).map(|v| format!("{v:08x}")))
                .collect::<crate::dap::Result<_>>()?;
            println!("stack: {}", stack.join(" "));
            return Ok(());
        }
    }
    d.mem_write32(DHCSR, DHCSR_KEY | C_HALT | C_DEBUGEN)?;
    println!(
        "still running after 6 s: PC {:#x} SP {:#x}",
        core_register(d, 15)?,
        core_register(d, 13)?
    );
    Ok(())
}

/// Write an image to the chip's flash with probe-rs, whether or not the chip
/// is asleep, then [`release`] it so that it runs untouched.
///
/// `image` is an ELF file, or a raw binary for address 0 if its name ends in `.bin`.
pub fn flash(probe: &Probe, chip: &str, image: &Path, verbose: bool) -> Result<()> {
    if !image.is_file() {
        return Err(format!("{}: no such file", image.display()).into());
    }
    let mut last = String::new();
    // The probe occasionally fails its first CMSIS-DAP command, hence the retries.
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_secs(1));
        }
        // probe-rs 0.32 cannot attach to a sleeping MSPM0, so wake it first. The
        // connection is closed again before probe-rs runs: it uses the same probe.
        if let Err(e) = Dap::open(probe, verbose)
            .map_err(Into::into)
            .and_then(|mut d| wake(&mut d))
        {
            last = e.to_string();
            continue;
        }
        let mut cmd = Command::new("probe-rs");
        cmd.args(["download", "--chip", chip, "--protocol", "swd", "--probe"])
            .arg(probe.probe_rs_selector())
            .arg(image);
        if image.extension().is_some_and(|e| e == "bin") {
            cmd.args(["--binary-format", "bin", "--base-address", "0"]);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("could not run probe-rs: {e} (it is in the Nix dev shell)"))?;
        let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        if verbose {
            eprint!("{text}");
        }
        if text.contains("Finished") {
            return release(&mut Dap::open(probe, verbose)?);
        }
        last = text
            .trim()
            .lines()
            .last()
            .unwrap_or("probe-rs printed nothing")
            .to_string();
    }
    Err(format!("flash failed: {last}").into())
}
