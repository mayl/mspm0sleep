//! Writing an image to an MSPM0 with probe-rs, whether or not the chip is asleep.

use std::path::Path;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use xds110_sleep::{Dap, Probe, Released, Woken};

use crate::Result;

/// What [`flash`] reports while it works.
pub enum Step<'a> {
    /// The chip was woken for probe-rs.
    Woken(Woken),
    /// Everything probe-rs printed (for a verbose mode).
    ProbeRs(&'a str),
    /// The image is written and the chip was let go and reset.
    Released(Released),
}

/// Wake the chip, run `probe-rs download`, then release the chip so that it
/// runs untouched. `open` opens the probe's debug port (and can install a
/// trace callback); it is called once before and once after probe-rs, which
/// uses the same probe in between.
///
/// `image` is an ELF file, or a raw binary for address 0 if its name ends in
/// `.bin`. probe-rs is run as a program and must be on the PATH.
pub fn flash(
    probe: &Probe,
    chip: &str,
    image: &Path,
    open: impl Fn() -> Result<Dap>,
    mut on: impl FnMut(Step<'_>),
) -> Result<()> {
    if !image.is_file() {
        return Err(format!("{}: no such file", image.display()).into());
    }
    let mut last = String::new();
    // The probe occasionally fails its first CMSIS-DAP command, hence the retries.
    for attempt in 0..3 {
        if attempt > 0 {
            sleep(Duration::from_secs(1));
        }
        // probe-rs 0.32 cannot attach to a sleeping MSPM0, so wake it first. The
        // connection is closed again before probe-rs runs: it uses the same probe.
        match open().and_then(|mut d| crate::wake(&mut d)) {
            Ok(woken) => on(Step::Woken(woken)),
            Err(e) => {
                last = e.to_string();
                continue;
            }
        }
        let mut cmd = Command::new("probe-rs");
        cmd.args(["download", "--chip", chip, "--protocol", "swd", "--probe"])
            .arg(probe.probe_rs_selector())
            .arg(image);
        if image.extension().is_some_and(|e| e == "bin") {
            cmd.args(["--binary-format", "bin", "--base-address", "0"]);
        }
        let out = cmd
            .output()
            .map_err(|e| format!("could not run probe-rs: {e} (it is in the Nix dev shell)"))?;
        let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
        on(Step::ProbeRs(&text));
        if text.contains("Finished") {
            on(Step::Released(xds110_sleep::release(&mut open()?)?));
            return Ok(());
        }
        last = text
            .trim()
            .lines()
            .last()
            .unwrap_or("probe-rs printed nothing")
            .to_string();
    }
    Err(format!("flash failed: {last}").into())
}
