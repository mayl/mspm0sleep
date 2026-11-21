//! Minimal CMSIS-DAP v1 SWD client for the XDS110, over its HID interface.
//!
//! Gives raw DP and AP register access, which is what it takes to reach
//! MSPM0's vendor access ports (the PWR-AP above all) that generic ARM tooling
//! ignores. The HID interface is used through the kernel's hidraw node, so the
//! EnergyTrace interfaces are left alone and nothing has to be detached.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::board::Probe;

/// A failed debug-port operation.
#[derive(Debug)]
pub enum DapError {
    /// The target answered FAULT: on an MSPM0, mostly an access port that is power-gated.
    Fault(String),
    Other(String),
}

impl From<DapError> for crate::Error {
    fn from(e: DapError) -> Self {
        match e {
            DapError::Fault(s) | DapError::Other(s) => crate::Error(s),
        }
    }
}

impl From<std::io::Error> for DapError {
    fn from(e: std::io::Error) -> Self {
        DapError::Other(format!("CMSIS-DAP: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, DapError>;

const DAP_CONNECT: u8 = 0x02;
const DAP_DISCONNECT: u8 = 0x03;
const DAP_TRANSFER_CONFIGURE: u8 = 0x04;
const DAP_TRANSFER: u8 = 0x05;
const DAP_SWJ_PINS: u8 = 0x10;
const DAP_SWJ_CLOCK: u8 = 0x11;
const DAP_SWJ_SEQUENCE: u8 = 0x12;
const DAP_SWD_CONFIGURE: u8 = 0x13;

/// DP register addresses.
const DP_ABORT: u8 = 0x0;
const DP_CTRL_STAT: u8 = 0x4;
const DP_SELECT: u8 = 0x8;

/// The hidraw node of a probe's CMSIS-DAP interface.
fn find_hidraw(probe: &Probe) -> Result<PathBuf> {
    let mut nodes: Vec<_> = fs::read_dir("/sys/class/hidraw")
        .map_err(|e| DapError::Other(format!("/sys/class/hidraw: {e}")))?
        .flatten()
        .collect();
    nodes.sort_by_key(|e| e.file_name());
    let id = format!("HID_ID=0003:{:08X}:{:08X}", crate::board::TI_VID, probe.pid);
    let uniq = format!("HID_UNIQ={}", probe.serial);
    for node in nodes {
        let Ok(uevent) = fs::read_to_string(node.path().join("device/uevent")) else {
            continue;
        };
        if uevent.lines().any(|l| l == id) && uevent.lines().any(|l| l == uniq) {
            return Ok(PathBuf::from("/dev").join(node.file_name()));
        }
    }
    Err(DapError::Other(format!(
        "no CMSIS-DAP hidraw node for the XDS110 with serial {}",
        probe.serial
    )))
}

/// An open CMSIS-DAP connection, in SWD mode.
pub struct Dap {
    dev: File,
    select: Option<u32>,
    verbose: bool,
}

impl Dap {
    pub fn open(probe: &Probe, verbose: bool) -> Result<Dap> {
        let path = find_hidraw(probe)?;
        let dev = OpenOptions::new().read(true).write(true).open(&path).map_err(|e| {
            DapError::Other(match e.kind() {
                std::io::ErrorKind::PermissionDenied => format!(
                    "no permission to open {} (add a udev rule for the XDS110's hidraw node, or run as root)",
                    path.display()
                ),
                _ => format!("{}: {e}", path.display()),
            })
        })?;
        let mut dap = Dap {
            dev,
            select: None,
            verbose,
        };
        dap.cmd(&[DAP_CONNECT, 0x01])?; // SWD
        let mut clock = vec![DAP_SWJ_CLOCK];
        clock.extend_from_slice(&1_000_000u32.to_le_bytes());
        dap.cmd(&clock)?;
        dap.cmd(&[DAP_TRANSFER_CONFIGURE, 0, 64, 0, 0, 0])?; // no idle cycles, 64 WAIT retries
        dap.cmd(&[DAP_SWD_CONFIGURE, 0])?;
        Ok(dap)
    }

    fn cmd(&mut self, data: &[u8]) -> Result<[u8; 64]> {
        let mut tx = [0u8; 65]; // report ID 0, then the 64-byte report
        tx[1..1 + data.len()].copy_from_slice(data);
        self.dev.write_all(&tx)?;
        let mut rx = [0u8; 64];
        let n = self.dev.read(&mut rx)?;
        if n == 0 || rx[0] != data[0] {
            return Err(DapError::Other(format!(
                "CMSIS-DAP: bad response {:02x?} to command {:#04x} (is another program using the probe?)",
                &rx[..4],
                data[0]
            )));
        }
        Ok(rx)
    }

    // --- wire level ---

    fn swj_sequence(&mut self, bits: u8, value: u64) -> Result<()> {
        let mut tx = vec![DAP_SWJ_SEQUENCE, bits];
        tx.extend_from_slice(&value.to_le_bytes()[..bits.div_ceil(8) as usize]);
        self.cmd(&tx).map(drop)
    }

    fn line_reset(&mut self) -> Result<()> {
        self.swj_sequence(51, (1 << 51) - 1)?;
        self.swj_sequence(16, 0xe79e)?; // JTAG-to-SWD
        self.swj_sequence(51, (1 << 51) - 1)?;
        self.swj_sequence(8, 0)?;
        self.select = None;
        Ok(())
    }

    /// Drive NRST: `false` holds the chip in reset.
    pub fn nrst(&mut self, high: bool) -> Result<()> {
        // DAP_SWJ_Pins: output value, pin select (bit 7 is nRESET), wait in us
        self.cmd(&[DAP_SWJ_PINS, if high { 0x80 } else { 0 }, 0x80, 0, 0, 0, 0])
            .map(drop)
    }

    fn transfer(&mut self, request: u8, data: Option<u32>) -> Result<u32> {
        let mut tx = vec![DAP_TRANSFER, 0, 1, request];
        if let Some(data) = data {
            tx.extend_from_slice(&data.to_le_bytes());
        }
        let rx = self.cmd(&tx)?;
        let (count, ack) = (rx[1], rx[2] & 7);
        if self.verbose {
            eprintln!(
                "   transfer req={request:#04x} data={data:x?} -> count={count} ack={ack} {:02x?}",
                &rx[3..7]
            );
        }
        if ack == 4 {
            return Err(DapError::Fault(format!("SWD FAULT (request {request:#04x})")));
        }
        if ack != 1 || count != 1 {
            return Err(DapError::Other(format!(
                "SWD transfer failed: ack {:#x}, count {count} (request {request:#04x})",
                rx[2]
            )));
        }
        Ok(u32::from_le_bytes([rx[3], rx[4], rx[5], rx[6]]))
    }

    // --- DP and AP registers ---

    pub fn dp_read(&mut self, addr: u8) -> Result<u32> {
        self.transfer(0x02 | (addr & 0xc), None)
    }

    pub fn dp_write(&mut self, addr: u8, value: u32) -> Result<()> {
        self.transfer(addr & 0xc, Some(value)).map(drop)
    }

    fn select(&mut self, ap: u8, addr: u8) -> Result<()> {
        let select = (ap as u32) << 24 | (addr & 0xf0) as u32;
        if self.select != Some(select) {
            self.dp_write(DP_SELECT, select)?;
            self.select = Some(select);
        }
        Ok(())
    }

    pub fn ap_read(&mut self, ap: u8, addr: u8) -> Result<u32> {
        self.select(ap, addr)?;
        self.transfer(0x03 | (addr & 0xc), None)
    }

    pub fn ap_write(&mut self, ap: u8, addr: u8, value: u32) -> Result<()> {
        self.select(ap, addr)?;
        self.transfer(0x01 | (addr & 0xc), Some(value)).map(drop)
    }

    /// Clear the sticky error flags a FAULT leaves behind.
    pub fn clear_errors(&mut self) -> Result<()> {
        self.dp_write(DP_ABORT, 0x1e)
    }

    /// Line reset and read DPIDR, without asking for debug power.
    pub fn connect_unpowered(&mut self) -> Result<u32> {
        self.line_reset()?;
        let idr = self.dp_read(0x0)?;
        self.clear_errors()?;
        Ok(idr)
    }

    /// Line reset, read DPIDR and request debug and system power-up.
    pub fn connect(&mut self) -> Result<u32> {
        let idr = self.connect_unpowered()?;
        self.dp_write(DP_CTRL_STAT, 0x5000_0000)?; // CSYSPWRUPREQ | CDBGPWRUPREQ
        let started = Instant::now();
        loop {
            let stat = self.dp_read(DP_CTRL_STAT)?;
            if stat >> 28 == 0xf {
                return Ok(idr);
            }
            if started.elapsed() > Duration::from_secs(1) {
                return Err(DapError::Other(format!(
                    "debug power-up not acknowledged: CTRL/STAT = {stat:#010x}"
                )));
            }
        }
    }

    /// Withdraw the power-up request, so the debug domain no longer keeps the chip from sleeping.
    pub fn power_down(&mut self) -> Result<()> {
        self.dp_write(DP_CTRL_STAT, 0)
    }

    pub fn ctrl_stat(&mut self) -> Result<u32> {
        self.dp_read(DP_CTRL_STAT)
    }

    // --- memory, through the AHB-AP (AP 0) ---

    pub fn mem_read32(&mut self, addr: u32) -> Result<u32> {
        self.ap_write(0, 0x00, 0x2300_0052)?; // CSW: 32-bit, no auto-increment
        self.ap_write(0, 0x04, addr)?;
        self.ap_read(0, 0x0c)
    }

    pub fn mem_write32(&mut self, addr: u32, value: u32) -> Result<()> {
        self.ap_write(0, 0x00, 0x2300_0052)?;
        self.ap_write(0, 0x04, addr)?;
        self.ap_write(0, 0x0c, value)
    }
}

impl Drop for Dap {
    fn drop(&mut self) {
        let _ = self.cmd(&[DAP_DISCONNECT]);
    }
}
