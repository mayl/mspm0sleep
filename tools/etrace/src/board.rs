//! The LaunchPads this tool knows by name, and choosing a probe by board.
//!
//! TI programs a serial number into the on-board XDS110 that starts with the
//! board's name (`ML13...` on an LP-MSPM0L1306), so the probe identifies the
//! board.

use xds110_sleep::probe;

use crate::{Error, Result};

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

/// A probe and, if known, the board it sits on.
#[derive(Debug)]
pub struct Probe {
    pub probe: probe::Probe,
    pub board: Option<&'static Board>,
}

impl std::ops::Deref for Probe {
    type Target = probe::Probe;
    fn deref(&self) -> &probe::Probe {
        &self.probe
    }
}

fn board_named(name: &str) -> Result<&'static Board> {
    BOARDS.iter().find(|b| b.name == name).ok_or_else(|| {
        let known: Vec<_> = BOARDS.iter().map(|b| b.name).collect();
        Error(format!("unknown board {name:?} (known: {})", known.join(", ")))
    })
}

/// The board whose probes have serial numbers like `serial`.
pub fn board_of(serial: &str) -> Option<&'static Board> {
    BOARDS.iter().find(|b| serial.starts_with(b.serial_prefix))
}

/// Pick the probe to use. `board` is a name from [`BOARDS`]; `serial` is a
/// serial number or its start and wins over `board` for finding the probe.
/// With neither, there must be exactly one probe.
pub fn select(board: Option<&str>, serial: Option<&str>) -> Result<Probe> {
    choose(&probe::list(), board, serial)
}

fn choose(all: &[probe::Probe], board: Option<&str>, serial: Option<&str>) -> Result<Probe> {
    let board = board.filter(|b| !b.is_empty()).map(board_named).transpose()?;
    let prefix = serial.filter(|s| !s.is_empty()).or(board.map(|b| b.serial_prefix));
    // A serial number here is always a prefix, never a probe-rs selector.
    let matching: Vec<probe::Probe> = all
        .iter()
        .filter(|p| prefix.is_none_or(|want| p.serial.starts_with(want)))
        .cloned()
        .collect();
    let probe = match (matching.as_slice(), prefix) {
        ([], Some(want)) => {
            return Err(xds110_sleep::Error::ProbeNotFound {
                wanted: Some(want.to_string()),
            }
            .into())
        }
        (some, _) => probe::choose(some, None)?,
    };
    Ok(Probe {
        // A board named by hand also says what the chip is, whatever the serial number.
        board: board.or(board_of(&probe.serial)),
        probe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Vec<probe::Probe> {
        ["MC110001", "ML130001"]
            .into_iter()
            .map(|s| probe::Probe {
                serial: s.into(),
                pid: 0xbef3,
            })
            .collect()
    }

    #[test]
    fn board_picks_its_probe() {
        let p = choose(&two(), Some("c1104"), None).unwrap();
        assert_eq!(p.serial, "MC110001");
        assert_eq!(p.board.unwrap().chip, "MSPM0C1104");
        assert_eq!(p.probe_rs_selector(), "0451:bef3:MC110001");
    }

    #[test]
    fn serial_wins_and_names_the_board() {
        let p = choose(&two(), None, Some("ML13")).unwrap();
        assert_eq!(p.board.unwrap().name, "l1306");
    }

    #[test]
    fn two_probes_need_a_choice() {
        let e = choose(&two(), None, None).unwrap_err();
        assert_eq!(
            e.to_string(),
            "several XDS110 probes are plugged in (MC110001, ML130001): choose one with --board or --serial"
        );
        assert!(choose(&two(), Some("g3507"), None).is_err());
        let e = choose(&two(), None, Some("XX")).unwrap_err();
        assert_eq!(e.to_string(), "no XDS110 probe with serial XX... found on USB");
        assert_eq!(
            choose(&[], None, None).unwrap_err().to_string(),
            "no XDS110 probe found on USB"
        );
    }

    #[test]
    fn one_probe_needs_none() {
        assert_eq!(choose(&two()[..1], None, None).unwrap().serial, "MC110001");
    }
}
