//! USB transport and the EnergyTrace command sequence for the XDS110.
//!
//! Commands go to interface 2 (bulk out 0x02, bulk in 0x83) in TI's ICDI framing:
//!
//! ```text
//! request:   0x2a  len:u16  cmd  payload...        len counts cmd + payload
//! response:  0x2a  len:u16  status:i32  payload... len counts status + payload
//! ```
//!
//! Measurement data arrives unframed on interface 6 (bulk in 0x87); see [`crate::decode`].
//!
//! The sequence is the one TI's `EnergyTrace_MSP432` class uses: connect, read
//! the version, set range and digital mode, set the sample rate, calibrate
//! against the reference load and against no load, start. Sending the
//! calibration command out of that order stalls the probe's command interface
//! until it is power-cycled.

use std::time::{Duration, Instant};

use rusb::{Context, DeviceHandle, UsbContext};

use crate::cal::{Calibration, Meta};
use crate::{Error, Result};

const TI_VID: u16 = 0x0451;
const XDS110_PIDS: &[u16] = &[0xbef3, 0xbef4, 0x1cbe, 0x029e, 0x029f, 0x02a5];

const CMD_IFACE: u8 = 2;
const EP_CMD_OUT: u8 = 0x02;
const EP_CMD_IN: u8 = 0x83;
const DATA_IFACE: u8 = 6;
const EP_DATA_IN: u8 = 0x87;

const SYNC: u8 = 0x2a;
const CMD_XDS_VERSION: u8 = 0x03;
const CMD_ET_SETUP: u8 = 0x1d;
const CMD_ET_CALIBRATE: u8 = 0x1e;
const CMD_ET_START: u8 = 0x1f;
const CMD_ET_STOP: u8 = 0x20;
const CMD_ET_CLEANUP: u8 = 0x21;
const CMD_XDS_CONNECT_ET: u8 = 0x28;
const CMD_ET_SETUP_RANGE: u8 = 0x30;
const CMD_ET_SETUP_DIG: u8 = 0x31;
/// CMSIS-DAP `DAP_Disconnect`, sent bare (no ICDI framing) on the same endpoints.
const DAP_DISCONNECT: u8 = 0x03;

const CMD_TIMEOUT: Duration = Duration::from_millis(4000);

/// Time base of the tick count `ET_Calibrate` returns, in ticks per second.
///
/// It is not the configured sample rate: the probe reports the same reading
/// (about 10186 ticks) whatever rate was set, while the stream's ticks do
/// follow the rate. Taking the calibration ticks as 100 us gives a scale that
/// reads a known 165 uA load (the LP-MSPM0L1306 thermistor divider, 20 kohm
/// across 3.3 V) as 164.7 uA. The command itself returns after about 0.5 s,
/// which is not understood.
const CAL_TICKS_PER_S: f64 = 10_000.0;

/// Probe hardware versions whose reference load is 2200 ohm switched by selector 1.
const SUPPORTED_HARDWARE: &[u16] = &[0x28, 0x2d];

impl From<rusb::Error> for Error {
    fn from(e: rusb::Error) -> Self {
        Error(match e {
            rusb::Error::Access => {
                "no permission to open the XDS110 (add a udev rule for USB ID 0451:bef3, or run as root)".into()
            }
            rusb::Error::Busy => "the XDS110 is in use by another program (CCS, UniFlash, another etrace?)".into(),
            rusb::Error::NoDevice => "the XDS110 was disconnected".into(),
            other => format!("USB error: {other}"),
        })
    }
}

/// An open XDS110 with both interfaces claimed.
struct Usb {
    handle: DeviceHandle<Context>,
    verbose: bool,
}

impl Usb {
    fn open(verbose: bool) -> Result<Usb> {
        let ctx = Context::new()?;
        let device = ctx
            .devices()?
            .iter()
            .find(|d| {
                d.device_descriptor()
                    .is_ok_and(|desc| desc.vendor_id() == TI_VID && XDS110_PIDS.contains(&desc.product_id()))
            })
            .ok_or("no XDS110 probe found on USB")?;
        // The handle is never reset: on this probe a USB reset drops it off the bus until it is replugged.
        let handle = device.open()?;
        for iface in [CMD_IFACE, DATA_IFACE] {
            if handle.kernel_driver_active(iface).unwrap_or(false) {
                let _ = handle.detach_kernel_driver(iface);
            }
            handle.claim_interface(iface)?;
        }
        let usb = Usb { handle, verbose };
        usb.drain(EP_CMD_IN);
        usb.drain(EP_DATA_IN);
        Ok(usb)
    }

    /// Discard whatever a previous session left queued on an IN endpoint.
    fn drain(&self, ep: u8) {
        let mut tmp = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_millis(300);
        let mut total = 0;
        while Instant::now() < deadline && total < 64 * 1024 {
            match self.handle.read_bulk(ep, &mut tmp, Duration::from_millis(50)) {
                Ok(n) if n > 0 => total += n,
                _ => break,
            }
        }
        if self.verbose && total > 0 {
            eprintln!("drained {total} stale bytes from endpoint {ep:#04x}");
        }
    }

    /// A debugger that used the probe over CMSIS-DAP (probe-rs, for one) leaves
    /// it "connected", and it then ignores the commands below. Disconnect first.
    fn dap_disconnect(&self) {
        if self
            .handle
            .write_bulk(EP_CMD_OUT, &[DAP_DISCONNECT], CMD_TIMEOUT)
            .is_ok()
        {
            let mut rx = [0u8; 64];
            let _ = self.handle.read_bulk(EP_CMD_IN, &mut rx, Duration::from_millis(300));
        }
        self.drain(EP_CMD_IN);
    }

    /// Send one command and return the response payload (after the status word).
    fn execute(&self, name: &str, cmd: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let mut tx = vec![SYNC];
        tx.extend_from_slice(&((1 + payload.len()) as u16).to_le_bytes());
        tx.push(cmd);
        tx.extend_from_slice(payload);
        let sent = Instant::now();
        self.handle.write_bulk(EP_CMD_OUT, &tx, CMD_TIMEOUT)?;

        let mut rx = [0u8; 4352];
        let n = match self.handle.read_bulk(EP_CMD_IN, &mut rx, CMD_TIMEOUT) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => {
                return Err(format!(
                    "{name}: the probe did not answer. Its command interface is probably stalled; \
                     unplug and replug the LaunchPad (or power-cycle its USB port) and try again."
                )
                .into())
            }
            Err(e) => return Err(e.into()),
        };
        if self.verbose {
            eprintln!(
                "{name}: tx {tx:02x?}  rx {:02x?}  ({} ms)",
                &rx[..n.min(24)],
                sent.elapsed().as_millis()
            );
        }
        if n < 7 || rx[0] != SYNC {
            return Err(format!("{name}: malformed response ({n} bytes: {:02x?})", &rx[..n.min(16)]).into());
        }
        let len = u16::from_le_bytes([rx[1], rx[2]]) as usize;
        if len + 3 != n {
            return Err(format!("{name}: response length field says {len} bytes, got {}", n - 3).into());
        }
        let status = i32::from_le_bytes([rx[3], rx[4], rx[5], rx[6]]);
        if status != 0 {
            return Err(format!("{name}: probe returned status {status}").into());
        }
        Ok(rx[7..n].to_vec())
    }
}

/// What the probe reported about itself.
#[derive(Clone, Copy, Debug)]
pub struct ProbeInfo {
    pub firmware: u32,
    pub hardware: u16,
}

impl ProbeInfo {
    /// Firmware version as TI prints it, e.g. `3.0.0.34`.
    pub fn firmware_string(&self) -> String {
        let b = self.firmware.to_be_bytes();
        format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
    }
}

/// A configured and calibrated EnergyTrace session.
pub struct Session {
    usb: Usb,
    pub info: ProbeInfo,
    pub rate: u32,
    pub cal: Calibration,
    /// Raw calibration readings (pulses, ticks): reference load, then open.
    pub cal_loaded: (u32, u32),
    pub cal_open: (u32, u32),
    started: bool,
    opened: Instant,
}

impl Session {
    /// Open the probe, configure it for `rate` ticks per second and calibrate.
    /// Takes a second or two.
    pub fn open(rate: u32, verbose: bool) -> Result<Session> {
        let usb = Usb::open(verbose)?;
        usb.dap_disconnect();
        usb.execute("XDS_ConnectET", CMD_XDS_CONNECT_ET, &[])?;

        let v = usb.execute("XDS_Version", CMD_XDS_VERSION, &[])?;
        if v.len() < 6 {
            return Err("XDS_Version: short response".into());
        }
        let info = ProbeInfo {
            firmware: u32::from_le_bytes([v[0], v[1], v[2], v[3]]),
            hardware: u16::from_le_bytes([v[4], v[5]]),
        };
        if !SUPPORTED_HARDWARE.contains(&info.hardware) {
            return Err(format!(
                "probe hardware version {:#06x} is not supported: its calibration loads differ from the \
                 2200 ohm reference this tool assumes (supported: 0x28, 0x2d)",
                info.hardware
            )
            .into());
        }

        usb.execute("ET_Setup_Range", CMD_ET_SETUP_RANGE, &[0])?;
        usb.execute("ET_Setup_Dig", CMD_ET_SETUP_DIG, &[0])?;
        // mode 0 (analog), digital mode 0, sample rate, digital enable 0
        let mut setup = vec![0u8, 0];
        setup.extend_from_slice(&rate.to_le_bytes());
        setup.push(0);
        usb.execute("ET_Setup", CMD_ET_SETUP, &setup)?;

        // The argument selects the load: 1 = 2200 ohm reference, 0 = none.
        let calibrate = |selector: u16| -> Result<(u32, u32)> {
            let r = usb.execute("ET_Calibrate", CMD_ET_CALIBRATE, &selector.to_le_bytes())?;
            if r.len() < 8 {
                return Err("ET_Calibrate: short response".into());
            }
            let pulses = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
            let ticks = u32::from_le_bytes([r[4], r[5], r[6], r[7]]);
            if ticks == 0 {
                return Err("ET_Calibrate: probe reported zero elapsed time".into());
            }
            Ok((pulses, ticks))
        };
        let cal_loaded = calibrate(1)?;
        let cal_open = calibrate(0)?;
        let pps = |(pulses, ticks): (u32, u32)| pulses as f64 * CAL_TICKS_PER_S / ticks as f64;
        if pps(cal_loaded) <= pps(cal_open) {
            return Err(format!(
                "calibration failed: {:.1} pulses/s with the reference load, {:.1} without",
                pps(cal_loaded),
                pps(cal_open)
            )
            .into());
        }
        let cal = Calibration::from_points(pps(cal_loaded), pps(cal_open));

        Ok(Session {
            usb,
            info,
            rate,
            cal,
            cal_loaded,
            cal_open,
            started: false,
            opened: Instant::now(),
        })
    }

    /// Metadata to save next to a capture from this session.
    pub fn meta(&self) -> Meta {
        Meta {
            calibration: self.cal,
            sample_rate: self.rate,
            probe_hardware: self.info.hardware,
            probe_firmware: self.info.firmware,
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            cal_loaded: self.cal_loaded,
            cal_open: self.cal_open,
        }
    }

    /// Start the measurement stream.
    pub fn start(&mut self) -> Result<()> {
        self.usb.execute("ET_Start", CMD_ET_START, &[])?;
        self.started = true;
        Ok(())
    }

    /// Read stream bytes into `buf`. Returns 0 if nothing arrived within `timeout`.
    ///
    /// The probe sends a packet once it has about 188 bytes to send, and a short packet ends the
    /// transfer, so this returns as soon as one arrives whatever the size of `buf`. How often that
    /// is depends on the current: every few milliseconds at milliamps, but only every second or so
    /// while the target sleeps (see `LIVE_RATE` in `main.rs`).
    pub fn read(&self, buf: &mut [u8], timeout: Duration) -> Result<usize> {
        match self.usb.handle.read_bulk(EP_DATA_IN, buf, timeout) {
            Ok(n) => {
                if self.usb.verbose {
                    eprintln!("stream: {n} bytes at {:.3} s", self.opened.elapsed().as_secs_f64());
                }
                Ok(n)
            }
            Err(rusb::Error::Timeout) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Stop the stream and return the bytes that were still in flight.
    pub fn stop(&mut self) -> Result<Vec<u8>> {
        if !self.started {
            return Ok(Vec::new());
        }
        self.started = false;
        self.usb.execute("ET_Stop", CMD_ET_STOP, &[])?;
        // TI keeps reading for 500 ms after the stop command.
        let mut tail = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            match self.read(&mut buf, Duration::from_millis(100)) {
                Ok(n) => tail.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        self.usb.execute("ET_Cleanup", CMD_ET_CLEANUP, &[])?;
        Ok(tail)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
        let _ = self.usb.handle.release_interface(CMD_IFACE);
        let _ = self.usb.handle.release_interface(DATA_IFACE);
    }
}
