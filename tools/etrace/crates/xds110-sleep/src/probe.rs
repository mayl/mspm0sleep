//! Finding XDS110 probes and picking exactly one.
//!
//! Probes are listed from sysfs, which needs no permission and does not
//! disturb a probe that another program has open.

use crate::{Error, Result};

/// USB vendor ID of Texas Instruments.
pub const TI_VID: u16 = 0x0451;
/// USB product IDs an XDS110 can have.
pub const XDS110_PIDS: &[u16] = &[0xbef3, 0xbef4, 0x1cbe, 0x029e, 0x029f, 0x02a5];

/// One XDS110 found on USB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Its serial number. On a LaunchPad it starts with the board's name
    /// (`ML13...` on an LP-MSPM0L1306).
    pub serial: String,
    /// Its USB product ID.
    pub pid: u16,
}

impl Probe {
    /// The probe selector probe-rs takes: `VID:PID:serial`.
    pub fn probe_rs_selector(&self) -> String {
        format!("{TI_VID:04x}:{:04x}:{}", self.pid, self.serial)
    }
}

/// Every XDS110 plugged in, sorted by serial number. Empty on platforms
/// other than Linux.
pub fn list() -> Vec<Probe> {
    #[cfg(target_os = "linux")]
    {
        crate::hidraw::probes()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// The one plugged-in probe that `selector` means; see [`choose`].
pub fn resolve(selector: Option<&str>) -> Result<Probe> {
    choose(&list(), selector)
}

/// The one probe among `all` that `selector` means. The selector is
///
/// - `None` or empty: the only probe there is;
/// - a serial number, or the start of one (`ML13`); a probe whose serial
///   number is exactly the selector wins over ones that merely start with it;
/// - a probe-rs selector, `VID:PID` or `VID:PID:SERIAL` (hexadecimal IDs,
///   the serial number in full).
///
/// Fails with [`Error::ProbeNotFound`] if none fits, [`Error::SeveralProbes`]
/// if more than one does, and [`Error::BadSelector`] for a probe-rs selector
/// of another vendor or one that is not hexadecimal.
pub fn choose(all: &[Probe], selector: Option<&str>) -> Result<Probe> {
    let selector = selector.map(str::trim).filter(|s| !s.is_empty());
    let (fits, wanted): (Vec<&Probe>, Option<String>) = match selector {
        None => (all.iter().collect(), None),
        Some(s) if s.contains(':') => {
            let bad = || Error::BadSelector {
                selector: s.to_string(),
            };
            let mut parts = s.splitn(3, ':');
            let (vid, pid, serial) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next());
            if u16::from_str_radix(vid, 16).map_err(|_| bad())? != TI_VID {
                return Err(bad());
            }
            let pid = match pid {
                "" => None,
                pid => Some(u16::from_str_radix(pid, 16).map_err(|_| bad())?),
            };
            let serial = serial.filter(|s| !s.is_empty());
            let fits = all
                .iter()
                .filter(|p| pid.is_none_or(|pid| pid == p.pid) && serial.is_none_or(|s| s == p.serial))
                .collect();
            (fits, serial.map(str::to_string))
        }
        Some(s) => {
            let exact: Vec<&Probe> = all.iter().filter(|p| p.serial == s).collect();
            let fits = if exact.is_empty() {
                all.iter().filter(|p| p.serial.starts_with(s)).collect()
            } else {
                exact
            };
            (fits, Some(s.to_string()))
        }
    };
    match fits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(Error::ProbeNotFound { wanted }),
        many => Err(Error::SeveralProbes {
            serials: many.iter().map(|p| p.serial.clone()).collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Vec<Probe> {
        ["MC110001", "ML130001"]
            .into_iter()
            .map(|s| Probe {
                serial: s.into(),
                pid: 0xbef3,
            })
            .collect()
    }

    #[test]
    fn a_serial_or_its_start_picks_one() {
        assert_eq!(choose(&two(), Some("ML13")).unwrap().serial, "ML130001");
        assert_eq!(choose(&two(), Some("MC110001")).unwrap().serial, "MC110001");
        assert!(matches!(choose(&two(), Some("M")), Err(Error::SeveralProbes { serials }) if serials.len() == 2));
        assert!(matches!(choose(&two(), Some("XX")), Err(Error::ProbeNotFound { wanted: Some(w) }) if w == "XX"));
    }

    #[test]
    fn an_exact_serial_wins_over_a_longer_one() {
        let mut all = two();
        all.push(Probe {
            serial: "ML1300012".into(),
            pid: 0xbef3,
        });
        assert_eq!(choose(&all, Some("ML130001")).unwrap().serial, "ML130001");
    }

    #[test]
    fn a_probe_rs_selector_picks_one() {
        let p = choose(&two(), Some("0451:bef3:ML130001")).unwrap();
        assert_eq!(p.serial, "ML130001");
        assert_eq!(p.probe_rs_selector(), "0451:bef3:ML130001");
        assert_eq!(choose(&two(), Some("0451:BEF3:MC110001")).unwrap().serial, "MC110001");
        // The serial number of a probe-rs selector is not a prefix.
        assert!(matches!(
            choose(&two(), Some("0451:bef3:ML13")),
            Err(Error::ProbeNotFound { .. })
        ));
        assert!(matches!(
            choose(&two(), Some("0451:bef3")),
            Err(Error::SeveralProbes { .. })
        ));
        assert!(matches!(
            choose(&two(), Some("0451:bef4")),
            Err(Error::ProbeNotFound { .. })
        ));
        assert_eq!(choose(&two()[..1], Some("0451:bef3")).unwrap().serial, "MC110001");
        // Another vendor's probe is not ours to touch.
        assert!(matches!(
            choose(&two(), Some("0403:6010:FactoryAIOT Pro")),
            Err(Error::BadSelector { .. })
        ));
        assert!(matches!(
            choose(&two(), Some("zz:bef3")),
            Err(Error::BadSelector { .. })
        ));
    }

    #[test]
    fn no_selector_needs_exactly_one_probe() {
        assert!(matches!(choose(&two(), None), Err(Error::SeveralProbes { .. })));
        assert!(matches!(choose(&two(), Some("  ")), Err(Error::SeveralProbes { .. })));
        assert_eq!(choose(&two()[..1], None).unwrap().serial, "MC110001");
        assert!(matches!(choose(&[], None), Err(Error::ProbeNotFound { wanted: None })));
    }
}
