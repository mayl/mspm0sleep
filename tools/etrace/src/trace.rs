//! Pulses on a millisecond grid, with statistics and resampling for display.

use crate::cal::{Calibration, SUPPLY_V};
use crate::decode::{Decoder, Event};

/// Pulse history at 1 ms resolution.
///
/// Stored as a running total so the pulse count of any window is one
/// subtraction, whatever its length.
pub struct Trace {
    /// Ticks per second.
    rate: u32,
    /// `cum[i]` = pulses delivered before millisecond `first_ms + i`.
    cum: Vec<u64>,
    /// Millisecond index of `cum[0]`; grows when old history is dropped.
    first_ms: u64,
    /// Pulses in the millisecond still being filled.
    pending: u64,
    /// Keep at most this many milliseconds (0 = everything).
    max_ms: usize,
    pub overcurrent: bool,
}

impl Trace {
    pub fn new(rate: u32) -> Self {
        Trace {
            rate: rate.max(1),
            cum: vec![0],
            first_ms: 0,
            pending: 0,
            max_ms: 0,
            overcurrent: false,
        }
    }

    /// Drop history older than `max_ms` milliseconds as new data arrives.
    pub fn with_history_limit(mut self, max_ms: usize) -> Self {
        self.max_ms = max_ms;
        self
    }

    /// Decode a whole raw capture.
    pub fn from_raw(data: &[u8], default_rate: u32) -> (Trace, Decoder) {
        let mut trace = Trace::new(default_rate);
        let mut dec = Decoder::new();
        dec.feed(data, |e| trace.push(e));
        (trace, dec)
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn ms_of(&self, tick: u64) -> u64 {
        tick * 1000 / self.rate as u64
    }

    /// First millisecond still held.
    pub fn start_ms(&self) -> u64 {
        self.first_ms
    }

    /// Number of complete milliseconds seen since the start of the stream.
    pub fn end_ms(&self) -> u64 {
        self.first_ms + self.cum.len() as u64 - 1
    }

    pub fn push(&mut self, event: Event) {
        match event {
            Event::Config { rate } => {
                // Only the first stream start can set the rate; a later change would rescale old data.
                if rate > 0 && self.end_ms() == 0 && self.pending == 0 {
                    self.rate = rate;
                }
            }
            Event::Tick { tick } => self.advance(tick),
            Event::Pulses { tick, pulses } => {
                self.advance(tick);
                self.pending += pulses as u64;
            }
            Event::Overcurrent => self.overcurrent = true,
        }
    }

    /// Close every millisecond before the one containing `tick`.
    fn advance(&mut self, tick: u64) {
        let ms = self.ms_of(tick);
        while self.end_ms() < ms {
            let total = self.cum[self.cum.len() - 1] + self.pending;
            self.pending = 0;
            self.cum.push(total);
        }
        if self.max_ms > 0 && self.cum.len() > 2 * self.max_ms {
            let drop = self.cum.len() - self.max_ms - 1;
            self.cum.drain(..drop);
            self.first_ms += drop as u64;
        }
    }

    /// Pulses in `[a_ms, b_ms)`, clipped to the data held.
    pub fn pulses(&self, a_ms: u64, b_ms: u64) -> u64 {
        let clip = |ms: u64| (ms.clamp(self.first_ms, self.end_ms()) - self.first_ms) as usize;
        let (a, b) = (clip(a_ms), clip(b_ms));
        if b > a {
            self.cum[b] - self.cum[a]
        } else {
            0
        }
    }

    /// Mean pulse rate over `[a_ms, b_ms)`, or None if no data lies in that window.
    pub fn pps(&self, a_ms: u64, b_ms: u64) -> Option<f64> {
        let (a, b) = (a_ms.max(self.first_ms), b_ms.min(self.end_ms()));
        (b > a).then(|| self.pulses(a, b) as f64 * 1000.0 / (b - a) as f64)
    }

    /// Pulse rate in `n` consecutive windows of `width_ms` starting at `start_ms`.
    /// Windows not fully covered by data are None. `start_ms` may be negative.
    pub fn columns(&self, start_ms: i64, width_ms: u64, n: usize) -> Vec<Option<f64>> {
        (0..n)
            .map(|i| {
                let a = start_ms + (i as u64 * width_ms) as i64;
                let b = a + width_ms as i64;
                if a < self.first_ms as i64 || b > self.end_ms() as i64 {
                    None
                } else {
                    Some(self.pulses(a as u64, b as u64) as f64 * 1000.0 / width_ms as f64)
                }
            })
            .collect()
    }

    /// Statistics over everything held, with percentiles taken over windows of `bin_ms`.
    pub fn summary(&self, cal: &Calibration, bin_ms: u64) -> Summary {
        let (start, end) = (self.start_ms(), self.end_ms());
        let duration_s = (end - start) as f64 / 1000.0;
        let pulses = self.pulses(start, end);
        let mean_na = self.pps(start, end).map_or(0.0, |pps| cal.na(pps));

        let bin_ms = bin_ms.max(1);
        let mut counts: Vec<u64> = (0..(end - start) / bin_ms)
            .map(|i| self.pulses(start + i * bin_ms, start + (i + 1) * bin_ms))
            .collect();
        // The first window holds the dropped start-up records; leave it out when there are others.
        if counts.len() > 2 {
            counts.remove(0);
        }
        counts.sort_unstable();
        let at = |q: f64| -> Option<u64> {
            (!counts.is_empty()).then(|| counts[((q * counts.len() as f64) as usize).min(counts.len() - 1)])
        };
        let na = |count: Option<u64>| count.map_or(0.0, |c| cal.na(c as f64 * 1000.0 / bin_ms as f64));
        let floor_pps = at(0.10).map_or(0.0, |c| c as f64 * 1000.0 / bin_ms as f64);

        Summary {
            duration_s,
            pulses,
            mean_na,
            bin_ms,
            min_na: na(counts.first().copied()),
            p10_na: na(at(0.10)),
            median_na: na(at(0.50)),
            max_na: na(counts.last().copied()),
            floor_pps,
            charge_uc: mean_na * duration_s / 1000.0,
            energy_uj: mean_na * duration_s / 1000.0 * SUPPLY_V,
            step_na: cal.step_na(bin_ms as f64 / 1000.0),
        }
    }
}

/// Statistics for a stretch of trace. Currents in nA.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Summary {
    pub duration_s: f64,
    pub pulses: u64,
    /// Mean current over the whole stretch.
    pub mean_na: f64,
    /// Window length used for the percentiles below.
    pub bin_ms: u64,
    pub min_na: f64,
    pub p10_na: f64,
    pub median_na: f64,
    pub max_na: f64,
    /// 10th-percentile window, as a raw pulse rate. Shows how far the floor is from the calibration offset.
    pub floor_pps: f64,
    /// Charge delivered, in microcoulombs.
    pub charge_uc: f64,
    /// Energy delivered at 3.3 V, in microjoules.
    pub energy_uj: f64,
    /// One pulse per window, in nA: the resolution of the percentile figures.
    pub step_na: f64,
}

/// Format a current given in nA with a unit that keeps three or four digits.
pub fn fmt_current(na: f64) -> String {
    let a = na.abs();
    if a >= 1e6 {
        format!("{:.3} mA", na / 1e6)
    } else if a >= 1e5 {
        format!("{:.0} µA", na / 1e3)
    } else if a >= 1e3 {
        format!("{:.1} µA", na / 1e3)
    } else {
        format!("{:.2} µA", na / 1e3)
    }
}

/// A current in nA as a short axis label: `2.5 mA`, `100 µA`, `0`.
pub fn fmt_axis(na: f64) -> String {
    if na == 0.0 {
        return "0".into();
    }
    let (value, unit) = if na >= 1e6 {
        (na / 1e6, "mA")
    } else {
        (na / 1e3, "\u{b5}A")
    };
    let text = format!("{value:.3}");
    format!("{} {unit}", text.trim_end_matches('0').trim_end_matches('.'))
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:.1} s  mean {}  |  {} ms windows: min {}  p10 {}  median {}  max {}  (step {})  |  floor {:.0} pulses/s  |  {:.1} µJ",
            self.duration_s,
            fmt_current(self.mean_na),
            self.bin_ms,
            fmt_current(self.min_na),
            fmt_current(self.p10_na),
            fmt_current(self.median_na),
            fmt_current(self.max_na),
            fmt_current(self.step_na),
            self.floor_pps,
            self.energy_uj,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One pulse every `period` ticks for `ticks` ticks at 10 kHz.
    fn steady(period: u64, ticks: u64) -> Trace {
        let mut t = Trace::new(10_000);
        for tick in (period..=ticks).step_by(period as usize) {
            t.push(Event::Pulses { tick, pulses: 1 });
        }
        t.push(Event::Tick { tick: ticks });
        t
    }

    #[test]
    fn window_sums_and_rates() {
        let t = steady(10, 50_000); // 1 pulse per ms for 5 s
        assert_eq!(t.end_ms(), 5000);
        assert_eq!(t.pulses(0, 5000), 4999); // the pulse at the very end is in the open millisecond
        assert_eq!(t.pulses(1000, 2000), 1000);
        assert_eq!(t.pps(1000, 2000), Some(1000.0));
        assert_eq!(t.pps(6000, 7000), None);
    }

    #[test]
    fn columns_mark_missing_data() {
        let t = steady(10, 20_000); // 2 s
        let c = t.columns(-500, 500, 6);
        assert_eq!(c[0], None); // before the start
        assert_eq!(c[2], Some(1000.0));
        assert_eq!(c[5], None); // past the end
    }

    #[test]
    fn sparse_pulses_keep_their_time() {
        let mut t = Trace::new(10_000);
        t.push(Event::Pulses { tick: 5_000, pulses: 1 }); // 0.5 s
        t.push(Event::Tick { tick: 20_000 }); // 2 s
        assert_eq!(t.end_ms(), 2000);
        assert_eq!(t.pulses(0, 500), 0);
        assert_eq!(t.pulses(500, 501), 1);
        assert_eq!(t.pulses(501, 2000), 0);
    }

    #[test]
    fn history_limit_drops_old_data() {
        let mut t = Trace::new(10_000).with_history_limit(1000);
        for tick in (10..=100_000u64).step_by(10) {
            t.push(Event::Pulses { tick, pulses: 1 });
        }
        assert!(t.end_ms() - t.start_ms() <= 2000);
        assert!(t.end_ms() - t.start_ms() >= 1000);
        assert_eq!(t.end_ms(), 10_000);
        assert_eq!(t.pps(t.end_ms() - 500, t.end_ms()), Some(1000.0));
    }

    #[test]
    fn summary_of_a_steady_current() {
        let cal = Calibration {
            na_per_pps: 500.0,
            x0_pps: 0.0,
        };
        let s = steady(10, 100_000).summary(&cal, 100); // 1000 pps for 10 s
        assert!((s.mean_na - 500_000.0).abs() < 100.0, "{}", s.mean_na);
        assert_eq!(s.median_na, 500_000.0);
        assert!(
            (s.energy_uj - 0.5e-3 * 10.0 * 3.3 * 1e6).abs() < 10.0,
            "{}",
            s.energy_uj
        );
    }

    #[test]
    fn axis_labels_are_short() {
        assert_eq!(fmt_axis(0.0), "0");
        assert_eq!(fmt_axis(2_500_000.0), "2.5 mA");
        assert_eq!(fmt_axis(10_000_000.0), "10 mA");
        assert_eq!(fmt_axis(100_000.0), "100 µA");
        assert_eq!(fmt_axis(100.0), "0.1 µA");
        assert_eq!(fmt_axis(1_250.0), "1.25 µA");
    }

    #[test]
    fn current_formatting() {
        assert_eq!(fmt_current(1_680_000.0), "1.680 mA");
        assert_eq!(fmt_current(214_800.0), "215 µA");
        assert_eq!(fmt_current(2_100.0), "2.1 µA");
        assert_eq!(fmt_current(490.0), "0.49 µA");
    }
}
