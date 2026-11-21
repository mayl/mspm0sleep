//! The operations against a pretend MSPM0 behind a CMSIS-DAP probe.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::dap::command::*;
use crate::*;

/// AP 0 faults while the chip sleeps, which it does unless FORCEACTIVE is set.
#[derive(Default)]
struct Chip {
    sleepy: bool,
    dprec0: u32,
    ctrl_stat: u32,
    select: u32,
    tar: u32,
    /// DHCSR as the core holds it (without the key).
    dhcsr: u32,
    /// Every memory write through the AHB-AP: (address, value).
    mem_writes: Vec<(u32, u32)>,
    /// NRST levels driven, in order.
    nrst: Vec<bool>,
    resets: u32,
    /// Stay unreachable for this many more AHB-AP accesses.
    slow: u32,
    /// Every report sent, cut to its meaningful length.
    log: Vec<Vec<u8>>,
}

impl Chip {
    fn asleep(&mut self) -> bool {
        if self.sleepy && self.dprec0 & FORCEACTIVE == 0 {
            return true;
        }
        if self.slow > 0 {
            self.slow -= 1;
            return true;
        }
        false
    }
}

struct Fake(Arc<Mutex<Chip>>);

impl Transport for Fake {
    fn command(&mut self, data: &[u8]) -> io::Result<[u8; 64]> {
        let mut c = self.0.lock().unwrap();
        c.log.push(data.to_vec());
        let mut rx = [0u8; 64];
        rx[0] = data[0];
        match data[0] {
            DAP_SWJ_PINS => {
                let high = data[1] & 0x80 != 0;
                if high && c.nrst.last() == Some(&false) {
                    // A reset clears what a debugger set in the core, not the PWR-AP.
                    c.resets += 1;
                    c.dhcsr = 0;
                }
                c.nrst.push(high);
            }
            DAP_TRANSFER => {
                assert_eq!(&data[1..3], [0, 1], "one transfer per DAP_Transfer");
                let req = data[3];
                let value = data
                    .get(4..8)
                    .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                let (ap, read, addr) = (req & 1 != 0, req & 2 != 0, req & 0xc);
                let (apsel, bank) = ((c.select >> 24) as u8, (c.select & 0xf0) as u8);
                let mut out = 0u32;
                let mut fault = false;
                match (ap, read) {
                    (false, true) => out = if addr == 0x4 { c.ctrl_stat } else { 0x6ba0_2477 },
                    (false, false) => match addr {
                        0x4 => {
                            c.ctrl_stat = if value & 0x5000_0000 != 0 {
                                value | 0xa000_0000
                            } else {
                                0
                            }
                        }
                        0x8 => c.select = value,
                        _ => {}
                    },
                    (true, _) if apsel == 0 => {
                        if c.asleep() {
                            fault = true;
                        } else if read && bank == 0xf0 {
                            out = 0x8477_0001;
                        } else if read && addr == 0xc {
                            out = match c.tar {
                                DHCSR => c.dhcsr | if c.dhcsr & 2 != 0 { 1 << 17 } else { 0 },
                                tar => tar ^ 0x5555_5555,
                            };
                        } else if !read && addr == 0x4 {
                            c.tar = value;
                        } else if !read && addr == 0xc {
                            let tar = c.tar;
                            c.mem_writes.push((tar, value));
                            if tar == DHCSR {
                                assert_eq!(value & 0xffff_0000, DHCSR_KEY);
                                c.dhcsr = value & 0xffff;
                            }
                        }
                    }
                    (true, true) if apsel == PWR_AP => out = if bank == 0xf0 { 0x002e_0002 } else { c.dprec0 },
                    (true, false) if apsel == PWR_AP => c.dprec0 = value,
                    (true, true) if apsel < 4 && bank == 0xf0 => out = 0x002e_0000 + apsel as u32,
                    _ => {}
                }
                rx[1] = 1;
                rx[2] = if fault { 4 } else { 1 };
                rx[3..7].copy_from_slice(&out.to_le_bytes());
            }
            _ => {}
        }
        Ok(rx)
    }
}

fn chip(c: Chip) -> Arc<Mutex<Chip>> {
    Arc::new(Mutex::new(c))
}

fn dap(chip: &Arc<Mutex<Chip>>) -> Dap {
    let mut d = Dap::new(Box::new(Fake(chip.clone()))).unwrap();
    d.set_tick(Duration::ZERO);
    d
}

/// The SWD transfers in the log since `from`, as (request, data written).
fn transfers(chip: &Arc<Mutex<Chip>>, from: usize) -> Vec<(u8, Option<u32>)> {
    let c = chip.lock().unwrap();
    c.log[from..]
        .iter()
        .filter(|r| r[0] == DAP_TRANSFER)
        .map(|r| (r[3], r.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))))
        .collect()
}

const DP_R_IDR: u8 = 0x02;
const DP_W_ABORT: u8 = 0x00;
const DP_R_CTRL: u8 = 0x06;
const DP_W_CTRL: u8 = 0x04;
const DP_W_SELECT: u8 = 0x08;
const AP_R: u8 = 0x03;
const AP_W: u8 = 0x01;

#[test]
fn status_tells_asleep_from_awake_and_changes_nothing() {
    let asleep = chip(Chip {
        sleepy: true,
        ..Default::default()
    });
    let st = status(&mut dap(&asleep)).unwrap();
    assert!(st.asleep && !st.force_active() && !st.debug_powered());
    assert_eq!(st.dpidr, 0x6ba0_2477);
    assert_eq!(
        st.ap_idr,
        [
            None,
            Some(0x002e_0001),
            Some(0x002e_0002),
            Some(0x002e_0003),
            Some(0x002e_0002)
        ]
    );
    {
        let c = asleep.lock().unwrap();
        // The power-up request made for the check is taken back.
        assert_eq!((c.dprec0, c.ctrl_stat, c.mem_writes.len(), c.nrst.len()), (0, 0, 0, 0));
    }
    let awake = chip(Chip::default());
    let st = status(&mut dap(&awake)).unwrap();
    assert!(!st.asleep);
    assert_eq!(st.ap_idr[0], Some(0x8477_0001));
    // A debugger's power-up request is left as found.
    awake.lock().unwrap().ctrl_stat = 0xf000_0000;
    assert!(status(&mut dap(&awake)).unwrap().debug_powered());
    assert_eq!(awake.lock().unwrap().ctrl_stat, 0xf000_0000);
}

#[test]
fn wake_holds_the_chip_active_without_resetting_or_halting_it() {
    let chip = chip(Chip {
        sleepy: true,
        slow: 3,
        ..Default::default()
    });
    let woken = wake(&mut dap(&chip)).unwrap();
    assert_eq!(
        (woken.dprec0_before, woken.dprec0_after, woken.ahb_ap_idr),
        (0, FORCEACTIVE, 0x8477_0001)
    );
    assert!(!woken.caught_after_reset);
    let st = status(&mut dap(&chip)).unwrap();
    assert!(!st.asleep && st.force_active() && st.debug_powered());
    let c = chip.lock().unwrap();
    assert_eq!((c.resets, c.nrst.len(), c.mem_writes.len()), (0, 0, 0));
}

#[test]
fn release_drops_the_holds_resets_and_writes_only_dhcsr() {
    let chip = chip(Chip {
        sleepy: true,
        ..Default::default()
    });
    wake(&mut dap(&chip)).unwrap();
    chip.lock().unwrap().dhcsr = 3; // a debugger left the core halted
    assert!(release(&mut dap(&chip)).unwrap().halt_withdrawn);
    {
        let c = chip.lock().unwrap();
        assert_eq!((c.dprec0, c.ctrl_stat, c.dhcsr), (0, 0, 0), "no hold left");
        assert_eq!((c.nrst.as_slice(), c.resets), ([false, true].as_slice(), 1));
        // The no-NONMAIN, no-flash rule: the one memory write is the halt withdrawal.
        assert_eq!(c.mem_writes, [(DHCSR, DHCSR_KEY)]);
    }
    assert!(status(&mut dap(&chip)).unwrap().asleep, "asleep again afterwards");
}

#[test]
fn a_chip_that_never_answers_is_an_error_and_wake_does_not_catch_it() {
    let chip = chip(Chip {
        slow: u32::MAX,
        ..Default::default()
    });
    let e = wake(&mut dap(&chip)).unwrap_err();
    assert!(matches!(e, Error::NotResponding { dprec0_before: 0 }), "{e}");
    let c = chip.lock().unwrap();
    assert!(c.mem_writes.is_empty() && c.nrst.is_empty());
    // release still gets through its sequence, and says the halt request was out of reach.
    drop(c);
    assert!(!release(&mut dap(&chip)).unwrap().halt_withdrawn);
    assert_eq!(chip.lock().unwrap().resets, 1);
}

#[test]
fn wake_or_catch_resets_and_halts_only_when_wake_fails() {
    let fine = chip(Chip {
        sleepy: true,
        ..Default::default()
    });
    assert!(!wake_or_catch(&mut dap(&fine)).unwrap().caught_after_reset);
    assert_eq!(fine.lock().unwrap().resets, 0);

    // Unreachable for the 50 polls of wake, then reachable.
    let looping = chip(Chip {
        slow: 50,
        ..Default::default()
    });
    let woken = wake_or_catch(&mut dap(&looping)).unwrap();
    assert!(woken.caught_after_reset);
    let c = looping.lock().unwrap();
    assert_eq!(c.resets, 1);
    assert_eq!(c.dprec0, FORCEACTIVE, "wait-in-reset bits dropped again");
    assert_eq!(c.dhcsr & 3, 3, "halted");
    let addresses: Vec<u32> = c.mem_writes.iter().map(|w| w.0).collect();
    assert_eq!(addresses, [DHCSR, 0xe000_edfc]);
}

#[test]
fn reset_pulses_nrst_without_asking_for_power() {
    let chip = chip(Chip::default());
    reset(&mut dap(&chip)).unwrap();
    let c = chip.lock().unwrap();
    assert_eq!(
        (c.nrst.as_slice(), c.ctrl_stat, c.dprec0),
        ([false, true].as_slice(), 0, 0)
    );
    assert!(c.mem_writes.is_empty());
}

#[test]
fn dump_reads_the_core_and_only_writes_debug_registers() {
    let chip = chip(Chip::default());
    let dump = dump(&mut dap(&chip)).unwrap();
    assert_eq!(dump.dhcsr & (1 << 17), 1 << 17, "halted");
    assert_eq!(dump.vectors, [0x5555_5555, 4 ^ 0x5555_5555]);
    assert!(!dump.flash_blank());
    assert_eq!(dump.registers.len(), 7);
    let c = chip.lock().unwrap();
    assert!(
        c.mem_writes.iter().all(|w| w.0 == DHCSR || w.0 == 0xe000_edf4),
        "{:x?}",
        c.mem_writes
    );
    assert_eq!(c.resets, 0);
}

#[test]
fn the_trace_callback_sees_every_transfer() {
    let chip = chip(Chip {
        sleepy: true,
        ..Default::default()
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let mut d = dap(&chip);
    d.set_trace(Some(Box::new(move |t| sink.lock().unwrap().push(*t))));
    let from = chip.lock().unwrap().log.len();
    status(&mut d).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), transfers(&chip, from).len());
    assert!(
        seen.iter().any(|t| t.ack == 4),
        "the FAULT of the gated AHB-AP is reported too"
    );
}

/// The exact reports of the three operations statloom relies on, against a
/// sleeping chip: a change here is a change in what goes to the hardware.
#[test]
fn golden_sequences() {
    let line_reset: Vec<Vec<u8>> = vec![
        vec![DAP_SWJ_SEQUENCE, 51, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x07],
        vec![DAP_SWJ_SEQUENCE, 16, 0x9e, 0xe7],
        vec![DAP_SWJ_SEQUENCE, 51, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x07],
        vec![DAP_SWJ_SEQUENCE, 8, 0],
    ];
    let setup: Vec<Vec<u8>> = vec![
        vec![DAP_CONNECT, 1],
        vec![DAP_SWJ_CLOCK, 0x40, 0x42, 0x0f, 0x00],
        vec![DAP_TRANSFER_CONFIGURE, 0, 64, 0, 0, 0],
        vec![DAP_SWD_CONFIGURE, 0],
    ];
    let sel = |ap: u32, bank: u32| (DP_W_SELECT, Some(ap << 24 | bank));
    let connect = [
        (DP_R_IDR, None),
        (DP_W_ABORT, Some(0x1e)),
        (DP_W_CTRL, Some(0x5000_0000)),
        (DP_R_CTRL, None),
    ];

    let chip = chip(Chip {
        sleepy: true,
        ..Default::default()
    });

    // wake
    wake(&mut dap(&chip)).unwrap();
    {
        let c = chip.lock().unwrap();
        assert_eq!(c.log[..4], setup[..]);
        assert_eq!(c.log[4..8], line_reset[..]);
        assert_eq!(c.log.last().unwrap(), &vec![DAP_DISCONNECT]);
        let others: Vec<u8> = c.log.iter().map(|r| r[0]).filter(|b| *b != DAP_TRANSFER).collect();
        assert_eq!(
            others.len(),
            9,
            "setup, line reset, disconnect and nothing else: {others:x?}"
        );
    }
    let mut expect = connect.to_vec();
    expect.extend([
        sel(4, 0),
        (AP_R, None),              // DPREC0 before
        (AP_W, Some(FORCEACTIVE)), // the hold
        sel(0, 0xf0),
        (AP_R | 0xc, None), // AHB-AP IDR answers
        sel(4, 0),
        (AP_R, None), // DPREC0 after
    ]);
    assert_eq!(transfers(&chip, 0), expect);

    // release
    let from = chip.lock().unwrap().log.len();
    release(&mut dap(&chip)).unwrap();
    let mut expect = connect.to_vec();
    expect.extend([
        (AP_W, Some(FORCEACTIVE)),
        sel(0, 0),
        (AP_W, Some(0x2300_0052)),     // CSW
        (AP_W | 0x4, Some(DHCSR)),     // TAR
        (AP_W | 0xc, Some(DHCSR_KEY)), // DRW: withdraw the halt request
        sel(4, 0),
        (AP_W, Some(0)),      // drop FORCEACTIVE
        (DP_W_CTRL, Some(0)), // drop the power-up request
    ]);
    // The SELECT cache is per connection, so the first PWR-AP access selects again.
    expect.insert(4, sel(4, 0));
    assert_eq!(transfers(&chip, from), expect);
    {
        let c = chip.lock().unwrap();
        let pins: Vec<&Vec<u8>> = c.log[from..].iter().filter(|r| r[0] == DAP_SWJ_PINS).collect();
        assert_eq!(
            pins,
            [
                &vec![DAP_SWJ_PINS, 0x00, 0x80, 0, 0, 0, 0],
                &vec![DAP_SWJ_PINS, 0x80, 0x80, 0, 0, 0, 0]
            ]
        );
    }

    // status, on the chip that is asleep again
    let from = chip.lock().unwrap().log.len();
    status(&mut dap(&chip)).unwrap();
    let mut expect = vec![(DP_R_IDR, None), (DP_W_ABORT, Some(0x1e)), (DP_R_CTRL, None)];
    expect.extend(connect);
    expect.push((DP_R_CTRL, None));
    expect.extend([sel(0, 0xf0), (AP_R | 0xc, None), (DP_W_ABORT, Some(0x1e))]); // AP0: FAULT, cleared
    for ap in 1..5 {
        expect.extend([sel(ap, 0xf0), (AP_R | 0xc, None)]);
    }
    expect.extend([sel(4, 0), (AP_R, None)]); // DPREC0
    expect.extend([sel(0, 0xf0), (AP_R | 0xc, None), (DP_W_ABORT, Some(0x1e))]); // asleep?
    expect.push((DP_W_CTRL, Some(0))); // power request taken back
    assert_eq!(transfers(&chip, from), expect);
}
