//! The error type of this crate.

use std::fmt;
use std::path::PathBuf;

/// Why an operation on a probe or on the chip behind it failed.
#[derive(Debug)]
pub enum Error {
    /// The target answered an SWD transfer with FAULT. On an MSPM0 that is
    /// mostly an access port that is power-gated because the chip is asleep.
    Fault {
        /// The CMSIS-DAP transfer request byte.
        request: u8,
    },
    /// An SWD transfer failed in another way (no answer, WAIT, protocol error).
    Transfer {
        /// The acknowledge byte the probe reported.
        ack: u8,
        /// How many transfers the probe says it completed.
        count: u8,
        /// The CMSIS-DAP transfer request byte.
        request: u8,
    },
    /// The probe answered a CMSIS-DAP command with something else.
    BadResponse {
        /// The command byte that was sent.
        command: u8,
        /// The first four bytes that came back.
        response: [u8; 4],
    },
    /// Talking to the probe failed; the message says where.
    Io(std::io::Error),
    /// The probe's hidraw node exists but may not be opened by this user.
    PermissionDenied {
        /// The node, e.g. `/dev/hidraw3`.
        path: PathBuf,
    },
    /// The probe is on USB but has no CMSIS-DAP hidraw node.
    NoHidraw {
        /// Serial number of the probe.
        serial: String,
    },
    /// No XDS110 fits the selector, or none is plugged in.
    ProbeNotFound {
        /// The serial number (or its start) that was asked for, if any.
        wanted: Option<String>,
    },
    /// More than one XDS110 fits, and touching the wrong board is not safe.
    SeveralProbes {
        /// Serial numbers of the probes that fit.
        serials: Vec<String>,
    },
    /// A probe selector names a probe that is not an XDS110, or cannot be read.
    BadSelector {
        /// The selector as given.
        selector: String,
    },
    /// The debug port did not acknowledge the power-up request.
    PowerUpTimeout {
        /// DP CTRL/STAT as last read.
        ctrl_stat: u32,
    },
    /// The chip's AHB-AP does not answer even with FORCEACTIVE set. Seen when
    /// a firmware crashes at boot and resets in a loop; [`crate::catch`] can
    /// halt such a chip.
    NotResponding {
        /// PWR-AP DPREC0 before FORCEACTIVE was written.
        dprec0_before: u32,
    },
    /// The chip could not be halted right after reset.
    CatchFailed,
    /// This platform has no way to reach the probe (only Linux has).
    Unsupported,
}

impl Error {
    /// Whether this is an SWD FAULT.
    pub fn is_fault(&self) -> bool {
        matches!(self, Error::Fault { .. })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fault { request } => write!(f, "SWD FAULT (request {request:#04x})"),
            Error::Transfer { ack, count, request } => {
                write!(f, "SWD transfer failed: ack {ack:#x}, count {count} (request {request:#04x})")
            }
            Error::BadResponse { command, response } => write!(
                f,
                "CMSIS-DAP: bad response {response:02x?} to command {command:#04x} (is another program using the probe?)"
            ),
            Error::Io(e) => e.fmt(f),
            Error::PermissionDenied { path } => write!(
                f,
                "no permission to open {} (add a udev rule for the XDS110's hidraw node, or run as root)",
                path.display()
            ),
            Error::NoHidraw { serial } => write!(f, "no CMSIS-DAP hidraw node for the XDS110 with serial {serial}"),
            Error::ProbeNotFound { wanted: Some(want) } => {
                write!(f, "no XDS110 probe with serial {want}... found on USB")
            }
            Error::ProbeNotFound { wanted: None } => f.write_str("no XDS110 probe found on USB"),
            Error::SeveralProbes { serials } => write!(
                f,
                "several XDS110 probes are plugged in ({}): choose one",
                serials.join(", ")
            ),
            Error::BadSelector { selector } => write!(f, "{selector:?} does not name an XDS110 probe"),
            Error::PowerUpTimeout { ctrl_stat } => {
                write!(f, "debug power-up not acknowledged: CTRL/STAT = {ctrl_stat:#010x}")
            }
            Error::NotResponding { .. } => f.write_str(
                "the AHB-AP does not answer even with the chip forced active (does its firmware crash and reset in a loop?)",
            ),
            Error::CatchFailed => f.write_str("could not halt the chip after reset"),
            Error::Unsupported => f.write_str("XDS110 access is only implemented for Linux"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Result with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
