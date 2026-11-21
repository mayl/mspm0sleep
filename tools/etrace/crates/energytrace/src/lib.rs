//! EnergyTrace current measurement with the XDS110 debug probe found on TI
//! LaunchPads, without Code Composer Studio.
//!
//! The probe powers the target through a DC/DC converter that delivers charge
//! in fixed-size pulses and reports how many pulses it delivered and when.
//! Current is the pulse rate, scaled by a two-point calibration the probe runs
//! against a built-in reference load.
//!
//! - [`session`] (feature `usb`): the capture session on a probe.
//! - [`decode`]: the record stream the probe sends while measuring.
//! - [`cal`]: calibration and the sidecar file saved next to a capture.
//! - [`trace`]: pulses on a millisecond grid, with statistics and resampling.
//! - [`source`]: decoded events from a probe or from a saved capture, one API.
//!
//! # Live
//!
//! ```no_run
//! # #[cfg(feature = "usb")]
//! # fn main() -> Result<(), energytrace::Error> {
//! use std::time::Duration;
//! use energytrace::{Event, Poll, Session, Source};
//!
//! let mut session = Session::open("ML130001", 10_000)?; // calibrates: a second or two
//! let cal = session.calibration();
//! let rate = session.rate() as f64;
//! while session.now() < 10 * session.rate() as u64 {
//!     // Returns after one packet or 200 ms, whichever is first.
//!     if let Poll::Data(_) = session.poll(Duration::from_millis(200), &mut |event| {
//!         if let Event::Pulses { tick, pulses } = event {
//!             println!("{:.4} s: {pulses} pulses", tick as f64 / rate);
//!         }
//!     })? {
//!         // session.last_chunk() holds the raw bytes, to save them.
//!     }
//! }
//! session.finish(|_| {})?;
//! # let _ = cal;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "usb"))]
//! # fn main() {}
//! ```
//!
//! Pulse counts become current over a time window: feed the events to a
//! [`Trace`] and ask it (`trace.pps(a_ms, b_ms)`, then `cal.na(pps)`), or
//! divide pulses by the time between two records yourself.
//!
//! # Replay
//!
//! [`Capture::open`] reads a saved `.bin` and its `.json` sidecar and is a
//! [`Source`] too, so the loop above works on it unchanged. That needs no
//! probe and, with `default-features = false`, no libusb.
//!
//! # Features
//!
//! - `usb` (default): [`Session`], through `rusb` (libusb-1.0). Without it
//!   the crate depends on `serde` and `serde_json` only.
//!
//! # Together with a debugger
//!
//! Tested on an LP-MSPM0L1306 (probe firmware 3.0.0.34) with probe-rs 0.32
//! and with the `xds110-sleep` crate, which both reach the probe through its
//! CMSIS-DAP HID interface. The EnergyTrace interfaces are separate USB
//! interfaces, so both can be open at once, but the order matters:
//!
//! - **Open the session first, then attach the debugger: works.** probe-rs
//!   attached and read memory while a capture was streaming, and the capture
//!   was complete.
//! - **Opening a session while a debugger is attached cuts the debugger
//!   off.** [`Session::open`] sends `DAP_Disconnect`, because a debugger that
//!   has left the probe "connected" makes it ignore EnergyTrace commands.
//!   The session itself opens, calibrates and streams, but SWD transfers on
//!   the connection that existed before fail from then on (no acknowledge)
//!   until the debugger connects again.
//!
//! What a capture shows with a debugger attached is another matter: the
//! debug holds keep the chip awake (about 1.1 mA on that board where the
//! released firmware idles at 2 uA), so sleep current cannot be measured
//! that way. Activity above that level is still visible.
//!
//! Protocol details were worked out from TI's libraries; see
//! `tools/energytrace/THEIA_VERIFICATION.md` in the repository this crate lives in.

#![warn(missing_docs)]

pub mod cal;
pub mod decode;
mod error;
#[cfg(feature = "usb")]
pub mod session;
pub mod source;
pub mod trace;

pub use cal::{Calibration, Meta};
pub use decode::{Counters, Decoder, Event};
pub use error::{Error, Result};
#[cfg(feature = "usb")]
pub use session::{ProbeInfo, Session};
pub use source::{Capture, Poll, Source};
pub use trace::{Summary, Trace};
