//! Which LaunchPad to talk to, when more than one is plugged in.
//!
//! TI programs a serial number into the on-board XDS110 that starts with the
//! board's name (`ML13...` on an LP-MSPM0L1306), so the probe identifies the
//! board. Probes are listed from sysfs, which needs no permission and does not
//! disturb a probe that another program has open.

use std::fs;
use std::path::Path;

use crate::{Error, Result};

pub const TI_VID: u16 = 0x0451;
pub const XDS110_PIDS: &[u16] = &[0xbef3, 0xbef4, 0x1cbe, 0x029e, 0x029f, 0x02a5];

/// A LaunchPad this tool knows by name.
#[derive(Debug, PartialEq)]
pub struct Board {
    pub name: &'static str,
    /// Start of the serial number of its XDS110.
    pub serial_prefix: &'static str,
    /// Chip name as probe-rs spells it.
    pub chip: &'static str,
}

pub const BOARDS: &[Board] = &[
    Board {
        name: "l1306",
        serial_prefix: "ML13",
        chip: "MSPM0L1306",
    },
    Board {
        name: "c1104",
        serial_prefix: "MC11",
        chip: "MSPM0C1104",
    },
];

/// One XDS110 found on USB.
#[derive(Debug)]
pub struct Probe {
    pub serial: String,
    pub pid: u16,
    /// The board it sits on, if the serial number says so.
    pub board: Option<&'static Board>,
}

impl Probe {
    /// The probe selector probe-rs takes: `VID:PID:serial`.
    pub fn probe_rs_selector(&self) -> String {
        format!("{TI_VID:04x}:{:04x}:{}", self.pid, self.serial)
    }
}

fn board_named(name: &str) -> Result<&'static Board> {
    BOARDS.iter().find(|b| b.name == name).ok_or_else(|| {
        let known: Vec<_> = BOARDS.iter().map(|b| b.name).collect();
        Error(format!("unknown board {name:?} (known: {})", known.join(", ")))
    })
}

fn board_of(serial: &str) -> Option<&'static Board> {
    BOARDS.iter().find(|b| serial.starts_with(b.serial_prefix))
}

fn hex(path: &Path) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(path).ok()?.trim(), 16).ok()
}

/// Every XDS110 plugged in.
pub fn probes() -> Vec<Probe> {
    let mut found = Vec::new();
    let Ok(dir) = fs::read_dir("/sys/bus/usb/devices") else {
        return found;
    };
    for dev in dir.flatten().map(|e| e.path()) {
        let (Some(vid), Some(pid)) = (hex(&dev.join("idVendor")), hex(&dev.join("idProduct"))) else {
            continue;
        };
        if vid != TI_VID || !XDS110_PIDS.contains(&pid) {
            continue;
        }
        let Ok(serial) = fs::read_to_string(dev.join("serial")) else {
            continue;
        };
        let serial = serial.trim().to_string();
        found.push(Probe {
            board: board_of(&serial),
            serial,
            pid,
        });
    }
    found.sort_by(|a, b| a.serial.cmp(&b.serial));
    found
}

/// Pick the probe to use. `board` is a name from [`BOARDS`]; `serial` is a
/// serial number or its start and wins over `board` for finding the probe.
/// With neither, there must be exactly one probe.
pub fn select(board: Option<&str>, serial: Option<&str>) -> Result<Probe> {
    choose(probes(), board, serial)
}

fn choose(all: Vec<Probe>, board: Option<&str>, serial: Option<&str>) -> Result<Probe> {
    let board = board.filter(|b| !b.is_empty()).map(board_named).transpose()?;
    let prefix = serial.filter(|s| !s.is_empty()).or(board.map(|b| b.serial_prefix));
    let mut matching: Vec<Probe> = all
        .into_iter()
        .filter(|p| prefix.is_none_or(|want| p.serial.starts_with(want)))
        .collect();
    match (matching.len(), prefix) {
        (0, Some(want)) => Err(format!("no XDS110 probe with serial {want}... found on USB").into()),
        (0, None) => Err("no XDS110 probe found on USB".into()),
        (1, _) => {
            let mut probe = matching.remove(0);
            // A board named by hand also says what the chip is, whatever the serial number.
            probe.board = board.or(probe.board);
            Ok(probe)
        }
        _ => {
            let serials: Vec<_> = matching.iter().map(|p| p.serial.as_str()).collect();
            Err(format!(
                "several XDS110 probes are plugged in ({}): choose one with --board or --serial",
                serials.join(", ")
            )
            .into())
        }
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
                board: board_of(s),
            })
            .collect()
    }

    #[test]
    fn board_picks_its_probe() {
        let p = choose(two(), Some("c1104"), None).unwrap();
        assert_eq!(p.serial, "MC110001");
        assert_eq!(p.board.unwrap().chip, "MSPM0C1104");
        assert_eq!(p.probe_rs_selector(), "0451:bef3:MC110001");
    }

    #[test]
    fn serial_wins_and_names_the_board() {
        let p = choose(two(), None, Some("ML13")).unwrap();
        assert_eq!(p.board.unwrap().name, "l1306");
    }

    #[test]
    fn two_probes_need_a_choice() {
        assert!(choose(two(), None, None).is_err());
        assert!(choose(two(), Some("g3507"), None).is_err());
        assert!(choose(two(), None, Some("XX")).is_err());
    }

    #[test]
    fn one_probe_needs_none() {
        let mut one = two();
        one.truncate(1);
        assert_eq!(choose(one, None, None).unwrap().serial, "MC110001");
    }
}
