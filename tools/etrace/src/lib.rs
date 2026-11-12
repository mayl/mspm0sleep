//! EnergyTrace current measurement with the XDS110 debug probe found on TI
//! LaunchPads, without Code Composer Studio.
//!
//! The probe powers the target through a DC/DC converter that delivers charge
//! in fixed-size pulses and reports how many pulses it delivered and when.
//! Current is the pulse rate, scaled by a two-point calibration the probe runs
//! against a built-in reference load.
//!
//! - [`probe`]: USB transport and the EnergyTrace command sequence.
//! - [`decode`]: the record stream the probe sends while measuring.
//! - [`cal`]: calibration and the sidecar file saved next to a capture.
//! - [`trace`]: pulses on a millisecond grid, with statistics and resampling.
//! - [`plot`]: PNG, SVG and CSV output.
//! - [`scope`]: the terminal oscilloscope.
//!
//! Protocol details were worked out from TI's libraries; see
//! `tools/energytrace/THEIA_VERIFICATION.md` in this repository.

pub mod cal;
pub mod decode;
pub mod plot;
pub mod probe;
pub mod scope;
pub mod trace;

/// Error with a message meant for the person at the terminal.
#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error(s.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
