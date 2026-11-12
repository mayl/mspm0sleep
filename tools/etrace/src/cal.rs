//! Calibration, and the metadata saved next to a raw capture.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

/// Current drawn by the probe's reference load during calibration:
/// 3300 mV across 2200 ohm, in nA. Holds for probe hardware 0x28 and 0x2d.
pub const REFERENCE_LOAD_NA: f64 = 1_500_000.0;

/// Target supply voltage, fixed by the probe.
pub const SUPPLY_V: f64 = 3.3;

/// Linear map from pulse rate to current.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// nA per (pulse per second).
    pub na_per_pps: f64,
    /// Pulse rate with nothing connected, in pulses per second.
    pub x0_pps: f64,
}

impl Calibration {
    /// Typical values for the XDS110 on an LP-MSPM0L1306. Used for captures
    /// that carry no calibration of their own; expect a few percent of error
    /// in scale and about half a microamp in offset.
    pub const NOMINAL: Calibration = Calibration {
        na_per_pps: 493.5,
        x0_pps: 16.2,
    };

    /// Two-point fit: `loaded_pps` with the reference load switched in,
    /// `open_pps` with it switched out.
    pub fn from_points(loaded_pps: f64, open_pps: f64) -> Self {
        Calibration {
            na_per_pps: REFERENCE_LOAD_NA / (loaded_pps - open_pps),
            x0_pps: open_pps,
        }
    }

    /// Current in nA for a pulse rate, never negative.
    pub fn na(&self, pps: f64) -> f64 {
        ((pps - self.x0_pps) * self.na_per_pps).max(0.0)
    }

    /// Current in uA for a pulse rate, never negative.
    pub fn ua(&self, pps: f64) -> f64 {
        self.na(pps) / 1000.0
    }

    /// Current step of one pulse in a window of `window_s` seconds, in nA.
    /// This is the amplitude resolution at that time resolution.
    pub fn step_na(&self, window_s: f64) -> f64 {
        self.na_per_pps / window_s
    }
}

/// Sidecar written as `<capture>.json` next to a raw `<capture>.bin`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub calibration: Calibration,
    /// Ticks per second requested from the probe.
    pub sample_rate: u32,
    pub probe_hardware: u16,
    pub probe_firmware: u32,
    /// Seconds since the Unix epoch when the capture started.
    pub started_unix: u64,
    /// Raw calibration readings: (pulses, ticks) with the reference load, then open.
    pub cal_loaded: (u32, u32),
    pub cal_open: (u32, u32),
}

impl Meta {
    pub fn path_for(capture: &Path) -> PathBuf {
        capture.with_extension("json")
    }

    pub fn save(&self, capture: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(Self::path_for(capture), text + "\n")?;
        Ok(())
    }

    /// The sidecar for `capture`, if there is one.
    pub fn load(capture: &Path) -> Option<Meta> {
        let text = std::fs::read_to_string(Self::path_for(capture)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_point_fit() {
        // Readings from the LP-MSPM0L1306 probe: 3112 pulses in 10187 ticks loaded, 17 in 10186 open.
        let c = Calibration::from_points(3112.0 * 10000.0 / 10187.0, 17.0 * 10000.0 / 10186.0);
        assert!((c.na_per_pps - 493.6).abs() < 0.5, "{}", c.na_per_pps);
        assert!((c.x0_pps - 16.69).abs() < 0.01);
        assert!((c.na(c.x0_pps + 1000.0) - 1000.0 * c.na_per_pps).abs() < 1e-6);
        assert_eq!(c.na(0.0), 0.0);
    }
}
