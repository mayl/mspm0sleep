//! The parts of the `etrace` command-line tool that are not in the library
//! crates it is built on:
//!
//! - [`scope`]: the terminal oscilloscope.
//! - [`plot`]: PNG, SVG and CSV output.
//! - [`board`]: the LaunchPads the tool knows by name.
//! - [`flash`]: writing an image with probe-rs to a chip that may be asleep.
//!
//! Measurement lives in the `energytrace` crate, and debug access to a
//! sleeping MSPM0 in `xds110-sleep`; a program that wants either should
//! depend on those.

pub mod board;
pub mod flash;
pub mod plot;
pub mod scope;

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

impl From<energytrace::Error> for Error {
    fn from(e: energytrace::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<xds110_sleep::Error> for Error {
    fn from(e: xds110_sleep::Error) -> Self {
        Error(match e {
            // The library does not know this tool's options.
            xds110_sleep::Error::SeveralProbes { .. } => format!("{e} with --board or --serial"),
            e => e.to_string(),
        })
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Wake the chip for a debugger, as `etrace wake` does: if it does not
/// answer when forced active, reset it and halt it at its reset vector.
pub fn wake(d: &mut xds110_sleep::Dap) -> Result<xds110_sleep::Woken> {
    xds110_sleep::wake_or_catch(d).map_err(|e| match e {
        xds110_sleep::Error::NotResponding { .. } => "the AHB-AP does not answer, even right after reset".into(),
        e => e.into(),
    })
}
