//! One way to read decoded events, whether they come from a probe or a file.

use std::path::Path;
use std::time::Duration;

use crate::cal::{Calibration, Meta};
use crate::decode::{Counters, Decoder, Event};
use crate::trace::Trace;
use crate::{Error, Result};

/// What one [`Source::poll`] call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    /// This many stream bytes were decoded. They may have produced no event
    /// (time-keeping bytes), but [`Source::now`] has moved on.
    Data(usize),
    /// Nothing arrived within the timeout.
    Idle,
    /// The stream is over: the end of a file.
    End,
}

/// A stream of EnergyTrace events with the calibration to read it:
/// [`crate::Session`] for a probe, [`Capture`] for a saved file.
///
/// Time is in ticks of [`Source::rate`] per second, counted from the first
/// record of the stream. [`Event::Pulses`] and [`Event::Tick`] carry it, and
/// [`Source::now`] is the latest. To align it with another clock, note that
/// clock when the first event arrives; the probe sends data in packets, up to
/// a second and a half apart while the target sleeps, so an event can be that
/// much older than its arrival.
pub trait Source {
    /// Ticks per second the stream was configured for. A stream can state
    /// its own rate ([`Event::Config`]); they agree unless a file lost its sidecar.
    fn rate(&self) -> u32;

    /// The scale that turns pulse rates into current: `calibration().na(pps)`.
    fn calibration(&self) -> Calibration;

    /// Latest stream time, in ticks.
    fn now(&self) -> u64;

    /// How many records of each kind were decoded so far, and bad bytes.
    fn counters(&self) -> Counters;

    /// Decode what arrives within `timeout` (one packet from a probe, one
    /// block of a file) and pass each event to `on`. Never blocks longer
    /// than `timeout`.
    fn poll(&mut self, timeout: Duration, on: &mut dyn FnMut(Event)) -> Result<Poll>;
}

/// Tick rate assumed for a capture without a sidecar: what TI's tools use.
pub const DEFAULT_RATE: u32 = 10_000;

/// A saved capture: the raw stream (`.bin`) and, if it has one, the sidecar
/// next to it (`.json`) with its calibration.
pub struct Capture {
    raw: Vec<u8>,
    /// The sidecar, if there was one.
    pub meta: Option<Meta>,
    pos: usize,
    decoder: Decoder,
}

impl Capture {
    /// Read `path` and its sidecar. A missing or unreadable sidecar is not
    /// an error: [`Capture::meta`] is `None` and the calibration nominal.
    pub fn open(path: &Path) -> Result<Capture> {
        let raw = std::fs::read(path).map_err(|source| Error::File {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Capture::from_parts(raw, Meta::load(path)))
    }

    /// A capture from bytes already in memory.
    pub fn from_parts(raw: Vec<u8>, meta: Option<Meta>) -> Capture {
        Capture {
            raw,
            meta,
            pos: 0,
            decoder: Decoder::new(),
        }
    }

    /// The whole raw stream.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Decode the whole capture at once into a [`Trace`], whatever was polled so far.
    pub fn trace(&self) -> (Trace, Decoder) {
        Trace::from_raw(&self.raw, self.rate())
    }

    /// Start over from the beginning.
    pub fn rewind(&mut self) {
        self.pos = 0;
        self.decoder = Decoder::new();
    }
}

impl Source for Capture {
    fn rate(&self) -> u32 {
        self.meta.as_ref().map_or(DEFAULT_RATE, |m| m.sample_rate)
    }

    /// The sidecar's calibration, or [`Calibration::NOMINAL`] without one.
    fn calibration(&self) -> Calibration {
        self.meta.as_ref().map_or(Calibration::NOMINAL, |m| m.calibration)
    }

    fn now(&self) -> u64 {
        self.decoder.now()
    }

    fn counters(&self) -> Counters {
        self.decoder.counters
    }

    /// Decodes the next 4096 bytes at once; the timeout is not used. To
    /// replay in real time, wait until the caller's clock reaches [`Source::now`].
    fn poll(&mut self, _timeout: Duration, on: &mut dyn FnMut(Event)) -> Result<Poll> {
        let end = (self.pos + 4096).min(self.raw.len());
        if end == self.pos {
            return Ok(Poll::End);
        }
        self.decoder.feed(&self.raw[self.pos..end], on);
        let n = end - self.pos;
        self.pos = end;
        Ok(Poll::Data(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream start at 10 kHz, then pulse records 100 ticks apart.
    fn stream(records: usize) -> Vec<u8> {
        let mut raw = vec![0x34, 0x10, 0x27];
        for i in 0..records {
            raw.extend([0x74, (i * 100 % 256) as u8, 5]);
            if (i + 1) * 100 / 256 > i * 100 / 256 {
                raw.push(0x5c);
            }
        }
        raw
    }

    #[test]
    fn polling_a_capture_gives_the_same_events_as_decoding_it_whole() {
        let raw = stream(5000);
        let mut whole = Vec::new();
        Decoder::new().feed(&raw, |e| whole.push(e));

        let mut capture = Capture::from_parts(raw, None);
        let (mut polled, mut polls) = (Vec::new(), 0);
        while capture.poll(Duration::ZERO, &mut |e| polled.push(e)).unwrap() != Poll::End {
            polls += 1;
        }
        assert!(polls > 3, "{polls}");
        assert_eq!(polled, whole);
        assert_eq!(capture.rate(), DEFAULT_RATE);
        assert_eq!(capture.calibration(), Calibration::NOMINAL);
        assert_eq!(capture.counters().configs, 1);
        // Time is in the events and in now(); a heartbeat after the last record moves now() on.
        let Some(Event::Pulses { tick, .. }) = polled.iter().rev().find(|e| matches!(e, Event::Pulses { .. })) else {
            panic!("no pulses")
        };
        assert_eq!(*tick, 4999 * 100);
        assert!((*tick..*tick + 256).contains(&capture.now()), "{}", capture.now());

        capture.rewind();
        let mut again = Vec::new();
        capture.poll(Duration::ZERO, &mut |e| again.push(e)).unwrap();
        assert_eq!(again[..], whole[..again.len()]);
    }

    #[test]
    fn the_sidecar_supplies_rate_and_calibration() {
        let cal = Calibration {
            na_per_pps: 480.0,
            x0_pps: 15.0,
        };
        let meta = Meta {
            calibration: cal,
            sample_rate: 50_000,
            probe_hardware: 0x28,
            probe_firmware: 0x0300_0022,
            started_unix: 0,
            cal_loaded: (3112, 10187),
            cal_open: (17, 10186),
        };
        let dir = std::env::temp_dir().join(format!("energytrace-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.bin");
        std::fs::write(&path, stream(10)).unwrap();
        meta.save(&path).unwrap();
        let capture = Capture::open(&path).unwrap();
        assert_eq!((capture.rate(), capture.calibration()), (50_000, cal));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(Capture::open(&path), Err(Error::File { .. })));
    }
}
