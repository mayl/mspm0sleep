//! The decoder and statistics against captures recorded from a real probe
//! (LP-MSPM0L1306, firmware in `firmware/`; see `docs/LOWPOWER_RESULTS.md`).

use std::path::PathBuf;

use etrace::cal::Calibration;
use etrace::trace::Trace;

fn capture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../energytrace/captures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn busy_loop_reads_about_1_7_ma() {
    let (trace, dec) = Trace::from_raw(&capture("busy_upstream.bin"), 10_000);
    let s = trace.summary(&Calibration::NOMINAL, 100);
    assert_eq!(dec.counters.configs, 1);
    assert_eq!(dec.counters.discarded, 10);
    assert!((7.9..8.1).contains(&s.duration_s), "{}", s.duration_s);
    assert!((1.65e6..1.72e6).contains(&s.mean_na), "{}", s.mean_na);
    // A busy loop is steady: every 100 ms window is within 2% of the mean.
    assert!(s.min_na > 0.98 * s.mean_na && s.max_na < 1.02 * s.mean_na, "{s}");
}

#[test]
fn deep_sleep_reads_a_couple_of_microamps() {
    // About 20 pulses/s against an open-load offset of about 16: pulse records are
    // far more than 256 ticks apart, so time has to come from the heartbeats.
    let (trace, dec) = Trace::from_raw(&capture("lp_idle_noJ15.bin"), 10_000);
    let s = trace.summary(&Calibration::NOMINAL, 100);
    assert_eq!(dec.counters.bad_bytes, 0);
    assert!((59.0..61.0).contains(&s.duration_s), "{}", s.duration_s);
    assert!((1_000.0..2_500.0).contains(&s.mean_na), "{}", s.mean_na);
    assert_eq!(s.floor_pps, 20.0);
}

#[test]
fn thermistor_divider_reads_165_ua() {
    // With jumper J15 fitted the board's 20 kohm thermistor divider draws 3.3 V / 20 kohm = 165 uA.
    // This is the independent check on the calibration scale.
    let (trace, _) = Trace::from_raw(&capture("lp_idle_upstream.bin"), 10_000);
    let s = trace.summary(&Calibration::NOMINAL, 100);
    assert!((162_000.0..168_000.0).contains(&s.median_na), "{}", s.median_na);
}

#[test]
fn damaged_stream_is_reported_not_fatal() {
    let (trace, dec) = Trace::from_raw(&capture("rom_bsl_standby.bin"), 10_000);
    assert!(dec.counters.bad_bytes > 0);
    assert!(trace.end_ms() > 20_000);
}
