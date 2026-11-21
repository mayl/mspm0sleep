//! The error type of this crate.

use std::fmt;
use std::path::PathBuf;

/// Why a capture could not be taken or read.
#[derive(Debug)]
pub enum Error {
    /// No XDS110 with this serial number is on USB.
    ProbeNotFound {
        /// The serial number asked for.
        serial: String,
    },
    /// The user may not open the probe's USB device.
    PermissionDenied,
    /// Another program has the probe's EnergyTrace interfaces.
    Busy,
    /// The probe was unplugged.
    Disconnected,
    /// Another USB failure, as libusb describes it.
    Usb(String),
    /// The probe did not answer a command: its command interface is stalled
    /// and stays so until the probe is power-cycled.
    NoAnswer {
        /// Name of the command.
        command: &'static str,
    },
    /// The probe's answer to a command does not parse.
    BadResponse {
        /// Name of the command.
        command: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// The probe refused a command.
    Refused {
        /// Name of the command.
        command: &'static str,
        /// The status word it returned.
        status: i32,
    },
    /// The probe's hardware version is one whose calibration loads this
    /// crate does not know (it knows 0x28 and 0x2d).
    UnsupportedHardware {
        /// The version the probe reported.
        hardware: u16,
    },
    /// Calibration gave no usable scale: the pulse rate with the reference
    /// load is not above the rate without it.
    Calibration {
        /// Pulses per second with the reference load.
        loaded_pps: f64,
        /// Pulses per second with no load.
        open_pps: f64,
    },
    /// A capture file or its sidecar could not be read or written.
    File {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The sidecar could not be turned into JSON.
    Sidecar(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ProbeNotFound { serial } => write!(f, "could not open the XDS110 with serial {serial}"),
            Error::PermissionDenied => {
                f.write_str("no permission to open the XDS110 (add a udev rule for USB ID 0451:bef3, or run as root)")
            }
            Error::Busy => f.write_str("the XDS110 is in use by another program (CCS, UniFlash, another etrace?)"),
            Error::Disconnected => f.write_str("the XDS110 was disconnected"),
            Error::Usb(e) => write!(f, "USB error: {e}"),
            Error::NoAnswer { command } => write!(
                f,
                "{command}: the probe did not answer. Its command interface is probably stalled; \
                 unplug and replug the LaunchPad (or power-cycle its USB port) and try again."
            ),
            Error::BadResponse { command, detail } => write!(f, "{command}: {detail}"),
            Error::Refused { command, status } => write!(f, "{command}: probe returned status {status}"),
            Error::UnsupportedHardware { hardware } => write!(
                f,
                "probe hardware version {hardware:#06x} is not supported: its calibration loads differ from the \
                 2200 ohm reference this tool assumes (supported: 0x28, 0x2d)"
            ),
            Error::Calibration { loaded_pps, open_pps } => write!(
                f,
                "calibration failed: {loaded_pps:.1} pulses/s with the reference load, {open_pps:.1} without"
            ),
            Error::File { path, source } => write!(f, "{}: {source}", path.display()),
            Error::Sidecar(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::File { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(feature = "usb")]
impl From<rusb::Error> for Error {
    fn from(e: rusb::Error) -> Self {
        match e {
            rusb::Error::Access => Error::PermissionDenied,
            rusb::Error::Busy => Error::Busy,
            rusb::Error::NoDevice => Error::Disconnected,
            other => Error::Usb(other.to_string()),
        }
    }
}

/// Result with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
