//! Minimal CMSIS-DAP v1 SWD client.
//!
//! Gives raw DP and AP register access, which is what it takes to reach
//! MSPM0's vendor access ports (the PWR-AP above all) that generic ARM
//! tooling ignores. Every command is one 64-byte report out and one back,
//! and every `DAP_Transfer` carries a single transfer.

use std::io;
use std::time::{Duration, Instant};

use crate::probe::Probe;
use crate::{Error, Result};

/// A CMSIS-DAP v1 link: 64-byte reports.
///
/// The real one is [`crate::Hidraw`]. Tests can put a simulated chip behind
/// this trait and check what the operations of this crate send.
pub trait Transport: Send {
    /// Send `report` (at most 64 bytes; the rest is zero) and return the
    /// probe's response report.
    fn command(&mut self, report: &[u8]) -> io::Result<[u8; 64]>;
}

/// CMSIS-DAP command bytes this client uses.
pub mod command {
    /// Connect in a mode: byte 1 is 1 for SWD.
    pub const DAP_CONNECT: u8 = 0x02;
    /// Disconnect from the target.
    pub const DAP_DISCONNECT: u8 = 0x03;
    /// Idle cycles, WAIT retries and match retries for transfers.
    pub const DAP_TRANSFER_CONFIGURE: u8 = 0x04;
    /// Read or write DP and AP registers: index, count, then per transfer a request byte and, for a write, a word.
    pub const DAP_TRANSFER: u8 = 0x05;
    /// Drive and read the SWCLK, SWDIO and nRESET pins: value, select mask, wait in microseconds.
    pub const DAP_SWJ_PINS: u8 = 0x10;
    /// The SWD clock frequency in Hz.
    pub const DAP_SWJ_CLOCK: u8 = 0x11;
    /// Clock out a bit sequence on SWDIO: bit count, then the bits.
    pub const DAP_SWJ_SEQUENCE: u8 = 0x12;
    /// Turnaround and data phase of the SWD protocol.
    pub const DAP_SWD_CONFIGURE: u8 = 0x13;
}
use command::*;

/// DP register addresses.
const DP_ABORT: u8 = 0x0;
const DP_CTRL_STAT: u8 = 0x4;
const DP_SELECT: u8 = 0x8;

/// CSYSPWRUPREQ | CDBGPWRUPREQ in DP CTRL/STAT.
pub const POWER_UP_REQUEST: u32 = 0x5000_0000;

/// One SWD transfer, as passed to the callback set with [`Dap::set_trace`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    /// The CMSIS-DAP transfer request byte: bit 0 AP (not DP), bit 1 read, bits 2..3 the address.
    pub request: u8,
    /// The word written, for a write.
    pub data: Option<u32>,
    /// How many transfers the probe completed (1 if all went well).
    pub count: u8,
    /// The acknowledge: 1 OK, 2 WAIT, 4 FAULT.
    pub ack: u8,
    /// The four data bytes of the response, whatever the outcome.
    pub response: [u8; 4],
}

/// Callback for [`Dap::set_trace`].
pub type Tracer = Box<dyn FnMut(&Transfer) + Send>;

/// An open CMSIS-DAP connection, in SWD mode. Dropping it sends `DAP_Disconnect`.
pub struct Dap {
    dev: Box<dyn Transport>,
    select: Option<u32>,
    tick: Duration,
    trace: Option<Tracer>,
}

impl Dap {
    /// Open the CMSIS-DAP interface of `probe` (Linux: its hidraw node).
    pub fn open(probe: &Probe) -> Result<Dap> {
        #[cfg(target_os = "linux")]
        {
            Dap::new(Box::new(crate::hidraw::Hidraw::open(probe)?))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = probe;
            Err(Error::Unsupported)
        }
    }

    /// Put the probe behind `dev` in SWD mode at 1 MHz.
    pub fn new(dev: Box<dyn Transport>) -> Result<Dap> {
        let mut dap = Dap {
            dev,
            select: None,
            tick: Duration::from_millis(10),
            trace: None,
        };
        dap.cmd(&[DAP_CONNECT, 0x01])?; // SWD
        let mut clock = vec![DAP_SWJ_CLOCK];
        clock.extend_from_slice(&1_000_000u32.to_le_bytes());
        dap.cmd(&clock)?;
        dap.cmd(&[DAP_TRANSFER_CONFIGURE, 0, 64, 0, 0, 0])?; // no idle cycles, 64 WAIT retries
        dap.cmd(&[DAP_SWD_CONFIGURE, 0])?;
        Ok(dap)
    }

    /// The unit of every pause the operations of this crate make while they
    /// wait for the chip: 10 ms unless changed. A poll waits one tick, the
    /// NRST pulse lasts five. Zero makes tests against a simulated chip instant.
    pub fn set_tick(&mut self, tick: Duration) {
        self.tick = tick;
    }

    /// Call `trace` after every SWD transfer (for a verbose mode), or stop with `None`.
    pub fn set_trace(&mut self, trace: Option<Tracer>) {
        self.trace = trace;
    }

    /// Wait `ticks` ticks; see [`Dap::set_tick`].
    pub(crate) fn pause(&self, ticks: u32) {
        if !self.tick.is_zero() {
            std::thread::sleep(self.tick * ticks);
        }
    }

    fn cmd(&mut self, report: &[u8]) -> Result<[u8; 64]> {
        let rx = self
            .dev
            .command(report)
            .map_err(|e| io::Error::new(e.kind(), format!("CMSIS-DAP: {e}")))?;
        if rx[0] != report[0] {
            return Err(Error::BadResponse {
                command: report[0],
                response: [rx[0], rx[1], rx[2], rx[3]],
            });
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
        let response = [rx[3], rx[4], rx[5], rx[6]];
        if let Some(trace) = &mut self.trace {
            trace(&Transfer {
                request,
                data,
                count,
                ack,
                response,
            });
        }
        if ack == 4 {
            return Err(Error::Fault { request });
        }
        if ack != 1 || count != 1 {
            return Err(Error::Transfer {
                ack: rx[2],
                count,
                request,
            });
        }
        Ok(u32::from_le_bytes(response))
    }

    // --- DP and AP registers ---

    /// Read the DP register at `addr` (0x0, 0x4, 0x8 or 0xc).
    pub fn dp_read(&mut self, addr: u8) -> Result<u32> {
        self.transfer(0x02 | (addr & 0xc), None)
    }

    /// Write the DP register at `addr`.
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

    /// Read register `addr` of access port `ap`.
    pub fn ap_read(&mut self, ap: u8, addr: u8) -> Result<u32> {
        self.select(ap, addr)?;
        self.transfer(0x03 | (addr & 0xc), None)
    }

    /// Write register `addr` of access port `ap`.
    pub fn ap_write(&mut self, ap: u8, addr: u8, value: u32) -> Result<()> {
        self.select(ap, addr)?;
        self.transfer(0x01 | (addr & 0xc), Some(value)).map(drop)
    }

    /// Clear the sticky error flags a FAULT leaves behind.
    pub fn clear_errors(&mut self) -> Result<()> {
        self.dp_write(DP_ABORT, 0x1e)
    }

    /// Run `op`, turning a FAULT into `None` (with the sticky flags cleared).
    pub fn unless_fault<T>(&mut self, op: impl FnOnce(&mut Dap) -> Result<T>) -> Result<Option<T>> {
        match op(self) {
            Ok(v) => Ok(Some(v)),
            Err(Error::Fault { .. }) => {
                self.clear_errors()?;
                Ok(None)
            }
            Err(e) => Err(e),
        }
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
        self.dp_write(DP_CTRL_STAT, POWER_UP_REQUEST)?;
        let started = Instant::now();
        loop {
            let ctrl_stat = self.dp_read(DP_CTRL_STAT)?;
            if ctrl_stat >> 28 == 0xf {
                return Ok(idr);
            }
            if started.elapsed() > Duration::from_secs(1) {
                return Err(Error::PowerUpTimeout { ctrl_stat });
            }
        }
    }

    /// Withdraw the power-up request, so the debug domain no longer keeps the chip from sleeping.
    pub fn power_down(&mut self) -> Result<()> {
        self.dp_write(DP_CTRL_STAT, 0)
    }

    /// Read DP CTRL/STAT.
    pub fn ctrl_stat(&mut self) -> Result<u32> {
        self.dp_read(DP_CTRL_STAT)
    }

    // --- memory, through the AHB-AP (AP 0) ---

    /// Read a word of target memory.
    pub fn mem_read32(&mut self, addr: u32) -> Result<u32> {
        self.ap_write(0, 0x00, 0x2300_0052)?; // CSW: 32-bit, no auto-increment
        self.ap_write(0, 0x04, addr)?;
        self.ap_read(0, 0x0c)
    }

    /// Write a word of target memory.
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
