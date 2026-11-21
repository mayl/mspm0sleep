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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusb::{Context, DeviceHandle, UsbContext};

use crate::cal::{Calibration, Meta};
use crate::decode::{Counters, Decoder, Event};
use crate::source::{Poll, Source};
use crate::trace::Trace;
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

/// Something a [`Session`] did, passed to the callback given to
/// [`Session::open_logged`] (for a verbose mode).
#[derive(Clone, Copy, Debug)]
pub enum Log<'a> {
    /// Bytes a previous user left queued on an endpoint were discarded.
    Drained {
        /// The IN endpoint.
        endpoint: u8,
        /// How many bytes.
        bytes: usize,
    },
    /// A command was sent and answered.
    Command {
        /// Name of the command.
        name: &'static str,
        /// The request as sent.
        tx: &'a [u8],
        /// The response as received.
        rx: &'a [u8],
        /// How long the probe took.
        elapsed: Duration,
    },
    /// A packet of the measurement stream arrived.
    Stream {
        /// Its length.
        bytes: usize,
        /// Time since the session was opened.
        at: Duration,
    },
}

/// Callback for [`Log`] entries.
pub type Logger = Box<dyn FnMut(Log<'_>) + Send>;

/// An open XDS110 with both interfaces claimed.
struct Usb {
    handle: DeviceHandle<Context>,
    logger: Option<Mutex<Logger>>,
}

impl Usb {
    fn log(&self, entry: Log<'_>) {
        if let Some(logger) = &self.logger {
            (logger.lock().unwrap_or_else(|e| e.into_inner()))(entry);
        }
    }

    fn open(serial: &str, logger: Option<Logger>) -> Result<Usb> {
        let ctx = Context::new()?;
        let device = ctx
            .devices()?
            .iter()
            .find(|d| {
                let Ok(desc) = d.device_descriptor() else { return false };
                desc.vendor_id() == TI_VID
                    && XDS110_PIDS.contains(&desc.product_id())
                    && d.open()
                        .and_then(|h| h.read_serial_number_string_ascii(&desc))
                        .is_ok_and(|s| s == serial)
            })
            .ok_or_else(|| Error::ProbeNotFound {
                serial: serial.to_string(),
            })?;
        // The handle is never reset: on this probe a USB reset drops it off the bus until it is replugged.
        let handle = device.open()?;
        for iface in [CMD_IFACE, DATA_IFACE] {
            if handle.kernel_driver_active(iface).unwrap_or(false) {
                let _ = handle.detach_kernel_driver(iface);
            }
            handle.claim_interface(iface)?;
        }
        let usb = Usb {
            handle,
            logger: logger.map(Mutex::new),
        };
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
        if total > 0 {
            self.log(Log::Drained {
                endpoint: ep,
                bytes: total,
            });
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
    fn execute(&self, name: &'static str, cmd: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let mut tx = vec![SYNC];
        tx.extend_from_slice(&((1 + payload.len()) as u16).to_le_bytes());
        tx.push(cmd);
        tx.extend_from_slice(payload);
        let sent = Instant::now();
        self.handle.write_bulk(EP_CMD_OUT, &tx, CMD_TIMEOUT)?;

        let mut rx = [0u8; 4352];
        let n = match self.handle.read_bulk(EP_CMD_IN, &mut rx, CMD_TIMEOUT) {
            Ok(n) => n,
            Err(rusb::Error::Timeout) => return Err(Error::NoAnswer { command: name }),
            Err(e) => return Err(e.into()),
        };
        self.log(Log::Command {
            name,
            tx: &tx,
            rx: &rx[..n],
            elapsed: sent.elapsed(),
        });
        let bad = |detail: String| Error::BadResponse { command: name, detail };
        if n < 7 || rx[0] != SYNC {
            return Err(bad(format!(
                "malformed response ({n} bytes: {:02x?})",
                &rx[..n.min(16)]
            )));
        }
        let len = u16::from_le_bytes([rx[1], rx[2]]) as usize;
        if len + 3 != n {
            return Err(bad(format!("response length field says {len} bytes, got {}", n - 3)));
        }
        let status = i32::from_le_bytes([rx[3], rx[4], rx[5], rx[6]]);
        if status != 0 {
            return Err(Error::Refused { command: name, status });
        }
        Ok(rx[7..n].to_vec())
    }
}

/// What the probe reported about itself.
#[derive(Clone, Copy, Debug)]
pub struct ProbeInfo {
    /// Firmware version, one byte per part.
    pub firmware: u32,
    /// Hardware version; it decides the calibration loads.
    pub hardware: u16,
}

impl ProbeInfo {
    /// Firmware version as TI prints it, e.g. `3.0.0.34`.
    pub fn firmware_string(&self) -> String {
        let b = self.firmware.to_be_bytes();
        format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
    }
}

/// A configured and calibrated EnergyTrace session on one probe.
///
/// Opening it calibrates. After that, either take the raw stream
/// ([`Session::start`], [`Session::read`], [`Session::stop`], or
/// [`Session::pump`] for the whole loop), or let the session decode it
/// ([`Source::poll`], then [`Session::finish`]); do not mix the two, or the
/// decoder misses what was read raw. Dropping the session stops the probe.
///
/// A session is `Send`, so it can be opened in one thread and polled in another.
pub struct Session {
    usb: Usb,
    /// What the probe said it is.
    pub info: ProbeInfo,
    /// Ticks per second the probe was configured for.
    pub rate: u32,
    /// The scale from this session's calibration.
    pub cal: Calibration,
    /// Raw calibration reading with the reference load: (pulses, ticks).
    pub cal_loaded: (u32, u32),
    /// Raw calibration reading with no load: (pulses, ticks).
    pub cal_open: (u32, u32),
    started: bool,
    opened: Instant,
    decoder: Decoder,
    chunk: Vec<u8>,
    chunk_len: usize,
}

impl Session {
    /// Open the probe with this serial number, configure it for `rate` ticks
    /// per second (10 000 is what TI uses) and calibrate. Takes a second or two.
    pub fn open(serial: &str, rate: u32) -> Result<Session> {
        Session::open_logged(serial, rate, None)
    }

    /// [`Session::open`], reporting every command and stream packet to `logger`.
    pub fn open_logged(serial: &str, rate: u32, logger: Option<Logger>) -> Result<Session> {
        let usb = Usb::open(serial, logger)?;
        usb.dap_disconnect();
        usb.execute("XDS_ConnectET", CMD_XDS_CONNECT_ET, &[])?;

        let short = |command| Error::BadResponse {
            command,
            detail: "short response".into(),
        };
        let v = usb.execute("XDS_Version", CMD_XDS_VERSION, &[])?;
        if v.len() < 6 {
            return Err(short("XDS_Version"));
        }
        let info = ProbeInfo {
            firmware: u32::from_le_bytes([v[0], v[1], v[2], v[3]]),
            hardware: u16::from_le_bytes([v[4], v[5]]),
        };
        if !SUPPORTED_HARDWARE.contains(&info.hardware) {
            return Err(Error::UnsupportedHardware {
                hardware: info.hardware,
            });
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
                return Err(short("ET_Calibrate"));
            }
            let pulses = u32::from_le_bytes([r[0], r[1], r[2], r[3]]);
            let ticks = u32::from_le_bytes([r[4], r[5], r[6], r[7]]);
            if ticks == 0 {
                return Err(Error::BadResponse {
                    command: "ET_Calibrate",
                    detail: "probe reported zero elapsed time".into(),
                });
            }
            Ok((pulses, ticks))
        };
        let cal_loaded = calibrate(1)?;
        let cal_open = calibrate(0)?;
        let pps = |(pulses, ticks): (u32, u32)| pulses as f64 * CAL_TICKS_PER_S / ticks as f64;
        if pps(cal_loaded) <= pps(cal_open) {
            return Err(Error::Calibration {
                loaded_pps: pps(cal_loaded),
                open_pps: pps(cal_open),
            });
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
            decoder: Decoder::new(),
            chunk: vec![0u8; 64 * 1024],
            chunk_len: 0,
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
                self.usb.log(Log::Stream {
                    bytes: n,
                    at: self.opened.elapsed(),
                });
                Ok(n)
            }
            Err(rusb::Error::Timeout) => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Start the stream and pass each chunk of it to `sink` until `stop` is
    /// set, then stop the probe and pass on what was still in flight.
    ///
    /// `sink` may fail with an error type of its own; the probe is stopped
    /// before that error is returned.
    pub fn pump<E: From<Error>>(
        &mut self,
        stop: &AtomicBool,
        mut sink: impl FnMut(&[u8]) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E> {
        let mut buf = vec![0u8; 64 * 1024];
        self.start()?;
        let run: std::result::Result<(), E> = (|| {
            while !stop.load(Ordering::SeqCst) {
                let n = self.read(&mut buf, Duration::from_millis(200))?;
                if n > 0 {
                    sink(&buf[..n])?;
                }
            }
            Ok(())
        })();
        let tail = self.stop();
        run?;
        sink(&tail?)
    }

    /// Measure for `duration`, on the probe's clock, and return the trace.
    /// Current and statistics come from it with [`Session::cal`]:
    /// `trace.summary(&session.cal, 100)`.
    ///
    /// The probe sends data in packets, and one can hold over a second of it
    /// while the target sleeps, so the trace may be that much longer than asked.
    pub fn record(&mut self, duration: Duration) -> Result<Trace> {
        let ticks = (duration.as_secs_f64() * self.rate as f64) as u64;
        let mut trace = Trace::new(self.rate);
        let mut decoder = Decoder::new();
        let stop = AtomicBool::new(false);
        self.pump(&stop, |bytes| {
            if !stop.load(Ordering::SeqCst) {
                decoder.feed(bytes, |e| trace.push(e));
                stop.store(decoder.now() >= ticks, Ordering::SeqCst);
            }
            Ok::<(), Error>(())
        })?;
        Ok(trace)
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

impl Session {
    /// Stop the stream and decode what was still in flight, for a session
    /// driven with [`Source::poll`]. The raw bytes of that tail are in
    /// [`Session::last_chunk`] afterwards.
    pub fn finish(&mut self, mut on: impl FnMut(Event)) -> Result<()> {
        let tail = self.stop()?;
        self.decoder.feed(&tail, &mut on);
        self.chunk_len = tail.len().min(self.chunk.len());
        self.chunk[..self.chunk_len].copy_from_slice(&tail[..self.chunk_len]);
        Ok(())
    }

    /// The raw bytes the last [`Source::poll`] (or [`Session::finish`])
    /// decoded: append them to a file to record the capture.
    pub fn last_chunk(&self) -> &[u8] {
        &self.chunk[..self.chunk_len]
    }
}

impl Source for Session {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn calibration(&self) -> Calibration {
        self.cal
    }

    fn now(&self) -> u64 {
        self.decoder.now()
    }

    fn counters(&self) -> Counters {
        self.decoder.counters
    }

    /// Starts the stream on the first call. Never returns [`Poll::End`].
    fn poll(&mut self, timeout: Duration, on: &mut dyn FnMut(Event)) -> Result<Poll> {
        if !self.started {
            self.start()?;
        }
        let mut chunk = std::mem::take(&mut self.chunk);
        let read = self.read(&mut chunk, timeout);
        self.chunk = chunk;
        self.chunk_len = *read.as_ref().unwrap_or(&0);
        let n = read?;
        if n == 0 {
            return Ok(Poll::Idle);
        }
        self.decoder.feed(&self.chunk[..n], on);
        Ok(Poll::Data(n))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
        let _ = self.usb.handle.release_interface(CMD_IFACE);
        let _ = self.usb.handle.release_interface(DATA_IFACE);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_session_can_move_to_another_thread() {
        fn is_send<T: Send>() {}
        is_send::<super::Session>();
    }
}
