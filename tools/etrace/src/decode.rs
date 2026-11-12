//! Decoder for the record stream the probe sends on its data endpoint.
//!
//! The grammar follows `DecoderStateMachineMSP432::Run` in TI's
//! `libenergytracestandalone.so`. A header byte splits into three fields:
//!
//! ```text
//! kind = (b >> 5) & 7     sub = (b >> 2) & 7     low = b & 3
//! ```
//!
//! | Header | After the header        | Meaning                                   |
//! |--------|-------------------------|-------------------------------------------|
//! | `0x34` | sample rate, u16 LE     | stream start; next 10 pulse records are dropped |
//! | `0x5c` | nothing                 | heartbeat: the tick counter wrapped (+256 ticks) |
//! | `0x70` | tick u8, pulses u16 LE  | pulse record                              |
//! | `0x74` | tick u8, pulses u8      | pulse record, short form                  |
//! | `0x80` | nothing                 | overcurrent                               |
//! | `0xa0` | tick u8                 | paused                                    |
//! | `0x69` `0x65` `0xed` | 5, 6, 4 bytes | digital and program-counter records (other probes); skipped |
//!
//! Anything else is counted as bad data and skipped one byte at a time.
//!
//! Time comes from the stream only: a tick is `1 / sample_rate` seconds, a
//! pulse record carries the low 8 bits of the tick counter, and each heartbeat
//! adds 256. That keeps time right when the current is so low that pulse
//! records are more than 256 ticks apart.

/// Something the stream reported, in stream order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The stream (re)started at `rate` ticks per second.
    Config { rate: u32 },
    /// At time `tick`, `pulses` charge pulses had been delivered since the previous record.
    Pulses { tick: u64, pulses: u32 },
    /// Time reached `tick` with no pulses to report.
    Tick { tick: u64 },
    /// The probe's overcurrent protection tripped.
    Overcurrent,
}

/// Counters for a decoded stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub configs: u64,
    pub heartbeats: u64,
    pub pulse_records: u64,
    /// Pulse records dropped after a stream start (TI drops the first 10; the first carries a junk count).
    pub discarded: u64,
    pub overcurrent: u64,
    pub paused: u64,
    /// Records of kinds this decoder skips.
    pub other_records: u64,
    /// Bytes that are not part of any record.
    pub bad_bytes: u64,
}

const DISCARD_AFTER_CONFIG: u8 = 10;

/// Streaming decoder. Feed it bytes as they arrive; records split across calls are handled.
pub struct Decoder {
    carry: Vec<u8>,
    /// Ticks added by heartbeats since the last stream start.
    base: u64,
    /// Tick counter value treated as time zero for the current stream segment.
    origin: Option<u64>,
    /// Timeline position where the current stream segment started.
    offset: u64,
    /// Latest time reported, on the continuous timeline.
    now: u64,
    discard: u8,
    pub counters: Counters,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            carry: Vec::new(),
            base: 0,
            origin: None,
            offset: 0,
            now: 0,
            discard: DISCARD_AFTER_CONFIG,
            counters: Counters::default(),
        }
    }

    /// Latest time seen, in ticks since the first record.
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Bytes held back because they are the start of an incomplete record.
    pub fn pending(&self) -> usize {
        self.carry.len()
    }

    /// Timeline position for tick-counter value `counter`. Never goes backwards.
    fn time(&mut self, counter: u64) -> u64 {
        let origin = *self.origin.get_or_insert(counter);
        self.now = self.now.max(self.offset + counter.saturating_sub(origin));
        self.now
    }

    /// Decode `data`, calling `on` for each event.
    pub fn feed(&mut self, data: &[u8], mut on: impl FnMut(Event)) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        let mut i = 0;

        while i < buf.len() {
            let b = buf[i];
            let (kind, sub, low) = ((b >> 5) & 7, (b >> 2) & 7, b & 3);
            // Bytes that follow the header, or None for a byte that starts no record.
            let body = match (kind, sub, low) {
                (1, _, 0) => Some(2),
                (2, _, 0) => Some(0),
                (3, 4, 0) => Some(3),
                (3, 5, 0) => Some(2),
                (3, 2, 1) => Some(5),
                (3, 1, 1) => Some(6),
                (7, 3, 1) => Some(4),
                (4, _, 0) => Some(0),
                (5, _, _) => Some(1),
                _ => None,
            };
            let Some(body) = body else {
                self.counters.bad_bytes += 1;
                i += 1;
                continue;
            };
            if i + 1 + body > buf.len() {
                break; // incomplete record: wait for more bytes
            }
            let rec = &buf[i + 1..i + 1 + body];
            i += 1 + body;

            match kind {
                1 => {
                    self.counters.configs += 1;
                    // A new stream segment continues the timeline where the last one ended.
                    self.offset = self.now;
                    self.base = 0;
                    self.origin = None;
                    self.discard = DISCARD_AFTER_CONFIG;
                    on(Event::Config {
                        rate: u16::from_le_bytes([rec[0], rec[1]]) as u32,
                    });
                }
                2 => {
                    self.counters.heartbeats += 1;
                    self.base += 256;
                    let tick = self.time(self.base);
                    on(Event::Tick { tick });
                }
                3 if low == 0 => {
                    self.counters.pulse_records += 1;
                    let tick = self.time(self.base + rec[0] as u64);
                    let pulses = if body == 3 {
                        u16::from_le_bytes([rec[1], rec[2]]) as u32
                    } else {
                        rec[1] as u32
                    };
                    if self.discard > 0 {
                        self.discard -= 1;
                        self.counters.discarded += 1;
                        on(Event::Tick { tick });
                    } else {
                        on(Event::Pulses { tick, pulses });
                    }
                }
                4 => {
                    self.counters.overcurrent += 1;
                    on(Event::Overcurrent);
                }
                5 => {
                    self.counters.paused += 1;
                    let tick = self.time(self.base + rec[0] as u64);
                    on(Event::Tick { tick });
                }
                _ => self.counters.other_records += 1,
            }
        }

        buf.drain(..i);
        self.carry = buf;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(data: &[u8]) -> (Vec<Event>, Decoder) {
        let mut d = Decoder::new();
        let mut ev = Vec::new();
        d.feed(data, |e| ev.push(e));
        (ev, d)
    }

    /// Stream start as the probe sends it, then enough records to get past the discard.
    fn start() -> Vec<u8> {
        let mut v = vec![0x34, 0x10, 0x27, 0x5c, 0x70, 0x00, 0x9e, 0x18];
        for t in 1..10u8 {
            v.extend_from_slice(&[0x70, t, 0x01, 0x00]);
        }
        v
    }

    #[test]
    fn stream_start_and_discard() {
        let mut data = start();
        data.extend_from_slice(&[0x70, 0x0c, 0x02, 0x00]);
        let (ev, d) = run(&data);
        assert_eq!(ev[0], Event::Config { rate: 10000 });
        assert_eq!(ev[1], Event::Tick { tick: 0 }); // the heartbeat defines time zero
        assert_eq!(d.counters.discarded, 10);
        // The junk count 0x189e in the first record is dropped along with the next nine.
        assert!(ev[2..12].iter().all(|e| matches!(e, Event::Tick { .. })));
        assert_eq!(ev[12], Event::Pulses { tick: 12, pulses: 2 });
        assert_eq!(d.counters.bad_bytes, 0);
    }

    #[test]
    fn heartbeats_carry_time_across_sparse_records() {
        let mut data = start();
        // Three heartbeats with no pulse record between them, then one pulse.
        data.extend_from_slice(&[0x5c, 0x5c, 0x5c, 0x70, 0x05, 0x01, 0x00]);
        let (ev, d) = run(&data);
        assert_eq!(
            *ev.last().unwrap(),
            Event::Pulses {
                tick: 3 * 256 + 5,
                pulses: 1
            }
        );
        assert_eq!(d.counters.heartbeats, 4);
    }

    #[test]
    fn records_split_across_feeds() {
        let mut data = start();
        data.extend_from_slice(&[0x70, 0x20, 0x03, 0x00, 0x70, 0x21, 0x04, 0x00]);
        let (whole, _) = run(&data);
        for cut in 1..data.len() {
            let mut d = Decoder::new();
            let mut ev = Vec::new();
            d.feed(&data[..cut], |e| ev.push(e));
            d.feed(&data[cut..], |e| ev.push(e));
            assert_eq!(ev, whole, "split at {cut}");
            assert_eq!(d.pending(), 0);
        }
    }

    #[test]
    fn bad_bytes_are_counted_and_skipped() {
        let mut data = start();
        data.extend_from_slice(&[0x01, 0x02, 0x70, 0x30, 0x01, 0x00]);
        let (ev, d) = run(&data);
        assert_eq!(d.counters.bad_bytes, 2);
        assert_eq!(*ev.last().unwrap(), Event::Pulses { tick: 0x30, pulses: 1 });
    }

    #[test]
    fn overcurrent_paused_and_short_pulse_records() {
        let mut data = start();
        data.extend_from_slice(&[0x80, 0xa0, 0x40, 0x74, 0x41, 0x07]);
        let (ev, d) = run(&data);
        let n = ev.len();
        assert_eq!(ev[n - 3], Event::Overcurrent);
        assert_eq!(ev[n - 2], Event::Tick { tick: 0x40 });
        assert_eq!(ev[n - 1], Event::Pulses { tick: 0x41, pulses: 7 });
        assert_eq!((d.counters.overcurrent, d.counters.paused), (1, 1));
    }

    #[test]
    fn restart_continues_the_timeline() {
        let mut data = start();
        data.extend_from_slice(&[0x70, 0x64, 0x01, 0x00]); // tick 100
        data.extend_from_slice(&start());
        data.extend_from_slice(&[0x70, 0x32, 0x01, 0x00]); // 50 ticks into the second segment
        let (ev, d) = run(&data);
        assert_eq!(*ev.last().unwrap(), Event::Pulses { tick: 150, pulses: 1 });
        assert_eq!(d.counters.configs, 2);
    }

    #[test]
    fn time_never_goes_backwards() {
        let mut data = start();
        data.extend_from_slice(&[0x70, 0x80, 0x01, 0x00, 0x70, 0x10, 0x01, 0x00]);
        let (ev, _) = run(&data);
        assert_eq!(*ev.last().unwrap(), Event::Pulses { tick: 0x80, pulses: 1 });
    }
}
