//! Linux: probes from sysfs, CMSIS-DAP reports through hidraw.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::dap::Transport;
use crate::probe::{Probe, TI_VID, XDS110_PIDS};
use crate::{Error, Result};

fn hex(path: &Path) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(path).ok()?.trim(), 16).ok()
}

pub(crate) fn probes() -> Vec<Probe> {
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
        found.push(Probe {
            serial: serial.trim().to_string(),
            pid,
        });
    }
    found.sort_by(|a, b| a.serial.cmp(&b.serial));
    found
}

/// The hidraw node of a probe's CMSIS-DAP interface.
fn find(probe: &Probe) -> Result<PathBuf> {
    let mut nodes: Vec<_> = fs::read_dir("/sys/class/hidraw")
        .map_err(|e| io::Error::new(e.kind(), format!("/sys/class/hidraw: {e}")))?
        .flatten()
        .collect();
    nodes.sort_by_key(|e| e.file_name());
    let id = format!("HID_ID=0003:{:08X}:{:08X}", TI_VID, probe.pid);
    let uniq = format!("HID_UNIQ={}", probe.serial);
    for node in nodes {
        let Ok(uevent) = fs::read_to_string(node.path().join("device/uevent")) else {
            continue;
        };
        if uevent.lines().any(|l| l == id) && uevent.lines().any(|l| l == uniq) {
            return Ok(PathBuf::from("/dev").join(node.file_name()));
        }
    }
    Err(Error::NoHidraw {
        serial: probe.serial.clone(),
    })
}

/// The CMSIS-DAP HID interface of an XDS110, through the kernel's hidraw
/// node. The EnergyTrace interfaces are left alone and nothing is detached.
pub struct Hidraw(File);

impl Hidraw {
    /// Open the hidraw node of `probe`.
    pub fn open(probe: &Probe) -> Result<Hidraw> {
        let path = find(probe)?;
        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(dev) => Ok(Hidraw(dev)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Err(Error::PermissionDenied { path }),
            Err(e) => Err(Error::Io(io::Error::new(e.kind(), format!("{}: {e}", path.display())))),
        }
    }
}

impl Transport for Hidraw {
    fn command(&mut self, report: &[u8]) -> io::Result<[u8; 64]> {
        let mut tx = [0u8; 65]; // report ID 0, then the 64-byte report
        tx[1..1 + report.len()].copy_from_slice(report);
        self.0.write_all(&tx)?;
        let mut rx = [0u8; 64];
        if self.0.read(&mut rx)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the probe did not answer"));
        }
        Ok(rx)
    }
}
