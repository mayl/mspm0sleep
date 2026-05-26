//! Repro-CLI: Probe XDS110 via libusb using the ICDI transport protocol
//! for EnergyTrace access — standalone, no CCS dependency.
//!
//! Based on REVERSE_ENGINEERING.md and disassembly of
//! libenergytracestandalone.so:
//!
//! Protocol (ICDI framing on interface 2, ep 0x02 OUT / 0x83 IN):
//!   [0x2a sync][len_lo][len_hi][cmd][payload...]
//!   Response: [0x2a sync][len_lo][len_hi][status:4][payload...]
//!
//! Data stream (raw bulk_read on interface 6, ep 0x87 IN):
//!   4-byte sample, layout VERIFIED 2026-05-07 against busy-loop ± LED loads
//!   (beads mspm0sleep-a78.3):
//!     byte[0] = 0x70 (frame marker, constant)
//!     byte[1] = cumulative 10 kHz time-window counter (mod 256) — NOT a value
//!     byte[2] = DC/DC charge pulses delivered since the last record
//!     byte[3] = digital flags (0 in pure-analog mode)
//!   The probe self-decimates: summing byte[2] over all records gives total
//!   pulses regardless of decimation. There is NO 24-bit signed sample here —
//!   an earlier "(byte[2]<<16)|(byte[1]<<8)|byte[0]" interpretation (theory B)
//!   was disproven by the captures (byte[0] is constant 0x70, not a low byte).
//!
//! Conversion:
//!   pulse_rate_hz = sum(byte[2]) / measurement_seconds
//!   current_nA    = pulse_rate_hz * 1_000_000 / cal2
//!   current_µA    = current_nA / 1_000
//!
//! cal2 is the per-unit calibration scale. We now read it LIVE from the probe
//! via ET_Calibrate (cmd 0x1e) — verified 2026-05-25 (beads mspm0sleep-dln):
//! cal2 ≈ 10186 (rock-stable across tickCount), cal1 ≈ 17 (a settling offset).
//! This is the same constant TI's GetCurrentInNA divides by. The normal flow
//! auto-calibrates (Step 2); CALIBRATE=1 runs a standalone tickCount probe;
//! CAL2 overrides; DEFAULT_CAL2 is only the fallback if ET_Calibrate fails.
//! (TI's full path also reads a `_CalibLoads` reference table from a board-data
//! .dat file, but the per-unit scale we need is cal2, read directly here. See
//! REVERSE_ENGINEERING.md §"ET_Calibrate WORKS".)

use plotters::prelude::*;
use rusb::{Context, DeviceHandle, UsbContext};
use std::time::Duration;

/// Calibration constant: nanoamps per (pulse-per-second). The TI library
/// derives this per target from the probe's on-EEPROM `_CalibLoads` table
/// (read via SMG_LoadBoardData) — see REVERSE_ENGINEERING.md §"EnergyTrace
/// Calibration Architecture". We don't have that table yet, so cal2 is anchored
/// empirically: with a known LaunchPad load (LED1 through its series resistor)
/// the LED-on − busy-loop pulse-rate delta pins `cal2 = Δpulses_per_s · 1e6 /
/// I_led_nA`. Override at runtime with the CAL2 env var while iterating.
const DEFAULT_CAL2: f64 = 10186.0;

const TI_VID: u16 = 0x0451;
const XDS110_PIDS: &[u16] = &[0xbef3, 0xbef4, 0x1cbe, 0x029e, 0x029f, 0x02a5];

// ICDI protocol constants
const SYNC_BYTE: u8 = 0x2a;
const CMD_XDS_CONNECT: u8 = 0x01;
const CMD_ET_SETUP: u8 = 0x1d;
#[allow(dead_code)]
const CMD_ET_CALIBRATE: u8 = 0x1e;
const CMD_ET_START: u8 = 0x1f;
const CMD_ET_STOP: u8 = 0x20;
const CMD_XDS_CONNECT_ET: u8 = 0x28;
const CMD_ET_SETUP_RANGE: u8 = 0x30;
const CMD_ET_DCDC_SET_VCC: u8 = 0x24;
const CMD_ET_DCDC_RESTART: u8 = 0x25;
// XDS_EEPROMRead (libjscxds110.so @0x47504): read the probe's on-board EEPROM,
// the suspected store for the per-unit `_CalibLoads` calibration table.
//   OUT payload (cmd_len=8): [addr:u16 LE][len:u16 LE]
//   response   (resp_len=len+7): [status:u32][<len> EEPROM bytes]
// `len` must be <= 0x10f9 (else the lib returns 0xffffff86). See beads
// mspm0sleep-dln and the `xds110-eeprom-read-cmd` memory.
const CMD_XDS_EEPROM_READ: u8 = 0x3f;
const EEPROM_MAX_CHUNK: u16 = 0x10f9;

// Interface 2: Command channel (ICDI framing)
const CMD_IFACE: u8 = 2;
const EP_CMD_IN: u8 = 0x83;
const EP_CMD_OUT: u8 = 0x02;

// Interface 6: Data stream (raw EnergyTrace records)
const DATA_IFACE: u8 = 6;
const EP_DATA_IN: u8 = 0x87;

// CMSIS-DAP v2 commands. Interface 2 carries BOTH CMSIS-DAP v2 and the TI
// ICDI/ET vendor framing on the same bulk endpoints (0x02 OUT / 0x83 IN).
// These are raw single-byte commands — no ICDI 0x2a framing — used only to
// clear the DAP state probe-rs leaves behind after flashing. See
// REVERSE_ENGINEERING.md §"CMSIS-DAP v2 Interface" and beads mspm0sleep-a78.6.
const DAP_DISCONNECT: u8 = 0x03;

const MAX_BUF_SIZE: usize = 0x1100; // 4352
const ET_DATA_BUF_SIZE: usize = 0x186a0; // 100000, same as TI's polling loop
const CMD_TIMEOUT: Duration = Duration::from_millis(4000);
const DATA_TIMEOUT: Duration = Duration::from_millis(500);

// ---------------------------------------------------------------------------
// ICDI buffer / command builder
// ---------------------------------------------------------------------------

/// ICDI transfer buffer for command/response on interface 2.
struct IcdiPacket {
    buf: [u8; MAX_BUF_SIZE],
}

impl IcdiPacket {
    fn new(cmd: u8, payload: &[u8]) -> Self {
        let mut p = IcdiPacket {
            buf: [0u8; MAX_BUF_SIZE],
        };
        let bytes_after_header = 1 + payload.len(); // cmd(1) + payload
        p.buf[0x00] = SYNC_BYTE;
        p.buf[0x01] = (bytes_after_header & 0xff) as u8;
        p.buf[0x02] = ((bytes_after_header >> 8) & 0xff) as u8;
        p.buf[0x03] = cmd;
        p.buf[0x04..0x04 + payload.len()].copy_from_slice(payload);
        p.buf[0x04 + payload.len()] = 0; // term
        p
    }

    fn len(&self) -> usize {
        3 + (self.buf[0x01] as usize | ((self.buf[0x02] as usize) << 8))
    }

    fn response_status(&self) -> i32 {
        i32::from_le_bytes([
            self.buf[0x03],
            self.buf[0x04],
            self.buf[0x05],
            self.buf[0x06],
        ])
    }
}

// ---------------------------------------------------------------------------
// USB helpers
// ---------------------------------------------------------------------------

/// Claim an interface, detaching kernel driver if active.
fn claim_iface(handle: &DeviceHandle<Context>, iface: u8) -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(true) = handle.kernel_driver_active(iface) {
        let _ = handle.detach_kernel_driver(iface);
    }
    handle.claim_interface(iface)?;
    println!("Claimed interface {iface}");
    Ok(())
}

struct Xds110Handle {
    handle: DeviceHandle<Context>,
    _ctx: Context,
}

fn open_xds110() -> Result<Xds110Handle, Box<dyn std::error::Error>> {
    let ctx = Context::new()?;
    let device = ctx
        .devices()?
        .iter()
        .find(|d| {
            d.device_descriptor().map_or(false, |desc| {
                desc.vendor_id() == TI_VID && XDS110_PIDS.contains(&desc.product_id())
            })
        })
        .ok_or("XDS110 probe not found")?;

    let handle = device.open()?;
    let desc = device.device_descriptor()?;

    println!(
        "Found XDS110: {:04x}:{:04x}",
        desc.vendor_id(),
        desc.product_id()
    );

    // NOTE: do not call handle.reset() — on this XDS110v3 firmware libusb_reset_device
    // disappears the probe from the USB bus and a physical replug is required to
    // recover. After running probe-rs (CMSIS-DAP) the probe is left "connected";
    // dap_reset() (called from main) clears that with a DAP_Disconnect so no
    // physical replug is needed between flash and measure. See mspm0sleep-a78.6.

    // Claim both interfaces up front (mirrors libjscxds110.so:_InitializeICDIDeviceBySerial,
    // which always claims iface 2 then iface 6 before any ICDI command is sent).
    claim_iface(&handle, CMD_IFACE)?;
    claim_iface(&handle, DATA_IFACE)?;

    let xds = Xds110Handle { handle, _ctx: ctx };

    // Drain any stale data on the IN endpoints from a previous session.
    drain_endpoint(&xds, EP_CMD_IN);
    drain_endpoint(&xds, EP_DATA_IN);

    Ok(xds)
}

fn icdi_send(xds: &Xds110Handle, packet: &IcdiPacket) -> Result<(), rusb::Error> {
    let len = packet.len();
    let n = xds.handle
        .write_bulk(EP_CMD_OUT, &packet.buf[..len], CMD_TIMEOUT)?;
    eprintln!("    [TX {n}/{len}B] {:02x?}", &packet.buf[..len]);
    Ok(())
}

/// Drain any stale data from the IN endpoint with a short timeout; used to
/// flush residue from a prior session before issuing new commands. Bounded
/// so we don't spin forever if the probe is still actively streaming.
fn drain_endpoint(xds: &Xds110Handle, ep: u8) {
    let mut tmp = [0u8; 4096];
    let deadline = std::time::Instant::now() + Duration::from_millis(300);
    let mut total: usize = 0;
    while std::time::Instant::now() < deadline {
        match xds.handle.read_bulk(ep, &mut tmp, Duration::from_millis(50)) {
            Ok(0) => return,
            Ok(n) => {
                total += n;
                if total >= 8 * 1024 {
                    eprintln!("    [drain ep=0x{ep:02x}] still flowing after {total}B, giving up");
                    return;
                }
            }
            Err(_) => return,
        }
    }
    if total > 0 {
        eprintln!("    [drain ep=0x{ep:02x}] discarded {total} bytes total");
    }
}

fn icdi_recv(
    xds: &Xds110Handle,
    packet: &mut IcdiPacket,
) -> Result<usize, rusb::Error> {
    match xds.handle.read_bulk(EP_CMD_IN, &mut packet.buf, CMD_TIMEOUT) {
        Ok(n) => Ok(n),
        Err(rusb::Error::Timeout) => Ok(0),
        Err(e) => Err(e),
    }
}

fn icdi_execute(
    xds: &Xds110Handle,
    cmd: u8,
    payload: &[u8],
    retries: usize,
) -> Result<i32, Box<dyn std::error::Error>> {
    let tx = IcdiPacket::new(cmd, payload);
    let mut rx = IcdiPacket::new(0, &[]);
    rx.buf.fill(0);

    for attempt in 0..retries {
        icdi_send(xds, &tx)?;

        let n = match icdi_recv(xds, &mut rx) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  recv attempt {attempt}: {e}");
                continue;
            }
        };
        if n == 0 {
            eprintln!("  recv attempt {attempt}: timeout");
            continue;
        }
        if rx.buf[0] != SYNC_BYTE {
            eprintln!("  recv attempt {attempt}: bad sync 0x{:02x}", rx.buf[0]);
            continue;
        }
        let status = rx.response_status();
        println!(
            "  → cmd=0x{cmd:02x} attempt={attempt} recv={n}B status={status}",
        );
        return Ok(status);
    }
    Err("XDS_Execute: all retries exhausted".into())
}

// ---------------------------------------------------------------------------
// CMSIS-DAP state reset
// ---------------------------------------------------------------------------

/// Send one raw CMSIS-DAP v2 command on interface 2 and read its reply.
/// Best-effort: the command is one byte, the reply is `[cmd, status, ...]`.
/// A timeout is reported but not fatal — on a freshly-replugged probe there is
/// no DAP connection to tear down, so a missing/negative reply is expected and
/// harmless.
fn dap_command(xds: &Xds110Handle, cmd: &[u8], label: &str) -> Option<Vec<u8>> {
    if let Err(e) = xds.handle.write_bulk(EP_CMD_OUT, cmd, CMD_TIMEOUT) {
        eprintln!("    [DAP {label}] write failed: {e}");
        return None;
    }
    let mut rx = [0u8; 64];
    match xds.handle.read_bulk(EP_CMD_IN, &mut rx, CMD_TIMEOUT) {
        Ok(n) => {
            println!("    [DAP {label}] reply {n}B: {:02x?}", &rx[..n]);
            Some(rx[..n].to_vec())
        }
        Err(rusb::Error::Timeout) => {
            println!("    [DAP {label}] no reply (timeout) — probe was likely already idle");
            None
        }
        Err(e) => {
            eprintln!("    [DAP {label}] read failed: {e}");
            None
        }
    }
}

/// Return the XDS110 to a clean state after probe-rs CMSIS-DAP flashing.
///
/// probe-rs talks CMSIS-DAP v2 on interface 2 and, when it exits, leaves the
/// probe's debug-port state machine "connected". The TI ICDI/ET vendor
/// commands (XDS_ConnectET, ET_Setup, …) sent on the same endpoints then time
/// out until the probe is physically unplugged and replugged. Issuing a
/// CMSIS-DAP `DAP_Disconnect` (cmd 0x03) here tears that connection down so the
/// vendor framing is accepted again — no replug. See beads mspm0sleep-a78.6.
///
/// Skippable via `SKIP_DAP_RESET=1` for debugging the raw fresh-replug path.
fn dap_reset(xds: &Xds110Handle) {
    if std::env::var("SKIP_DAP_RESET").is_ok() {
        println!("  SKIP_DAP_RESET set — not sending DAP_Disconnect");
        return;
    }
    println!("  DAP_Disconnect (cmd=0x03) to clear probe-rs CMSIS-DAP state...");
    dap_command(xds, &[DAP_DISCONNECT], "Disconnect");
    // Flush any DAP reply residue before the ICDI vendor commands take over.
    drain_endpoint(xds, EP_CMD_IN);
}

// ---------------------------------------------------------------------------
// EnergyTrace API
// ---------------------------------------------------------------------------

/// Run one ET_Calibrate (cmd=0x1e) with a single tickCount. Kept (not wired
/// into the default flow) because driving it correctly needs the probe's
/// per-load tickCount table; see beads mspm0sleep-a78.4 / the SMG_LoadBoardData
/// notes in REVERSE_ENGINEERING.md. Response payload is status(u32) + cal1(u32)
/// + cal2(u32) — verified against CalibrateTicks in libenergytracestandalone.so.
#[allow(dead_code)]
fn et_calibrate(
    xds: &Xds110Handle,
    tick_count: u16,
) -> Result<(u32, u32), Box<dyn std::error::Error>> {
    println!("  ET_Calibrate (cmd=0x1e) tickCount={tick_count}...");
    // Payload: [tickCount:2] = 2 bytes
    let payload = [
        (tick_count & 0xff) as u8,
        ((tick_count >> 8) & 0xff) as u8,
    ];

    let tx = IcdiPacket::new(CMD_ET_CALIBRATE, &payload);
    let mut rx = IcdiPacket::new(0, &[]);
    rx.buf.fill(0);

    for attempt in 0..2 {
        icdi_send(xds, &tx)?;
        let n = match icdi_recv(xds, &mut rx) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  recv attempt {attempt}: {e}");
                continue;
            }
        };
        if n == 0 {
            eprintln!("  recv attempt {attempt}: timeout");
            continue;
        }
        if rx.buf[0] != SYNC_BYTE {
            eprintln!("  recv attempt {attempt}: bad sync 0x{:02x}", rx.buf[0]);
            continue;
        }
        // TI code: cmd_len=6, resp_len=15. Payload in TX = 2 bytes at buf[0x14..0x15].
        // Response: cal1 at buf[0x1117..0x111a], cal2 at buf[0x111b..0x111e]
        // In our simplified buffer: buf[0x03]=sync, but the response frame starts at sync.
        // TI response at buf+0x1110: sync:len:status:payload
        // Our response: buf[0]=sync, buf[1-2]=len, buf[3-6]=status, buf[7+]=payload
        let status = rx.response_status();
        if status == 0 {
            let cal1 = u32::from_le_bytes([
                rx.buf[7], rx.buf[8], rx.buf[9], rx.buf[10],
            ]);
            let cal2 = u32::from_le_bytes([
                rx.buf[11], rx.buf[12], rx.buf[13], rx.buf[14],
            ]);
            println!(
                "  → cmd=0x1e attempt={attempt} recv={n}B status={status} cal1=0x{cal1:08x} cal2=0x{cal2:08x}"
            );
            return Ok((cal1, cal2));
        }
        println!("  → cmd=0x1e attempt={attempt} recv={n}B status={status}");
        return Err(format!("ET_Calibrate returned status {status}").into());
    }
    Err("ET_Calibrate: all retries exhausted".into())
}

fn et_connect(xds: &Xds110Handle) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  XDS_ConnectET (cmd=0x28)...");
    icdi_execute(xds, CMD_XDS_CONNECT_ET, &[], 2)
}

/// Plain (non-EnergyTrace) connect. `XDS_Open` in libjscxds110.so always sends
/// either XDS_Connect or XDS_ConnectET immediately after claiming interfaces,
/// before any other ICDI command. Used by the EEPROM-dump path.
fn xds_connect(xds: &Xds110Handle) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  XDS_Connect (cmd=0x01)...");
    icdi_execute(xds, CMD_XDS_CONNECT, &[], 2)
}

/// One ET_Calibrate (cmd 0x1e) attempt with a single tickCount, retries=1.
/// Returns Ok(Some((cal1,cal2))) on a status-0 response, Ok(None) on a non-zero
/// status (graceful refusal), Err on timeout/transport (which stalls iface 2).
fn et_calibrate_once(
    xds: &Xds110Handle,
    tick_count: u16,
) -> Result<Option<(u32, u32)>, Box<dyn std::error::Error>> {
    let payload = [(tick_count & 0xff) as u8, ((tick_count >> 8) & 0xff) as u8];
    let tx = IcdiPacket::new(CMD_ET_CALIBRATE, &payload);
    let mut rx = IcdiPacket::new(0, &[]);
    rx.buf.fill(0);

    icdi_send(xds, &tx)?;
    let n = icdi_recv(xds, &mut rx)?;
    if n == 0 {
        return Err(format!("ET_Calibrate(tickCount={tick_count}) TIMEOUT (no response)").into());
    }
    if rx.buf[0] != SYNC_BYTE {
        return Err(format!("ET_Calibrate: bad sync 0x{:02x}", rx.buf[0]).into());
    }
    let status = rx.response_status();
    if status != 0 {
        println!("  tickCount={tick_count:<6} → status={status} (graceful refusal, no cal)");
        return Ok(None);
    }
    let cal1 = u32::from_le_bytes([rx.buf[7], rx.buf[8], rx.buf[9], rx.buf[10]]);
    let cal2 = u32::from_le_bytes([rx.buf[11], rx.buf[12], rx.buf[13], rx.buf[14]]);
    println!("  tickCount={tick_count:<6} → status=0  cal1={cal1} (0x{cal1:08x})  cal2={cal2} (0x{cal2:08x})  ratio={:.6}", cal1 as f64 / cal2.max(1) as f64);
    Ok(Some((cal1, cal2)))
}

/// CALIBRATE mode: try to coax real cal1/cal2 out of the probe via ET_Calibrate.
/// Does dap_reset + ConnectET + DCDC init (the precondition PerformCalibration
/// runs under), then sweeps the tickCounts in `TICKCOUNTS` (comma-separated,
/// default a spread of hypotheses). Stops on the first success or the first
/// timeout (which stalls iface 2 → replug). See beads mspm0sleep-dln.
fn calibrate_probe(xds: &Xds110Handle) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== ET_CALIBRATE PROBE MODE (cmd 0x1e) ===");
    dap_reset(xds);
    let st = et_connect(xds)?;
    println!("  XDS_ConnectET status = {st}");
    let st = et_dcdc_set_vcc(xds, 3300)?;
    println!("  ET_DCDC_SetVcc status = {st}");
    let st = et_dcdc_restart(xds)?;
    println!("  ET_DCDC_RestartMCU status = {st}");
    if std::env::var("CALIB_SETUP").is_ok() {
        let st = et_setup(xds, 0, 10000, 0, 0)?;
        println!("  ET_Setup status = {st}");
    }

    let list = std::env::var("TICKCOUNTS")
        .unwrap_or_else(|_| "1000,256,16,4096,100,1,10000,65535".into());
    let ticks: Vec<u16> = list
        .split(',')
        .filter_map(|s| s.trim().parse::<u32>().ok())
        .map(|v| v as u16)
        .collect();
    println!("  sweeping tickCounts: {ticks:?}");
    println!("  (a status-0 response gives cal1/cal2; a TIMEOUT stalls iface 2 → replug)\n");

    let mut points: Vec<(u16, u32, u32)> = Vec::new();
    for tc in ticks {
        match et_calibrate_once(xds, tc) {
            Ok(Some((cal1, cal2))) => points.push((tc, cal1, cal2)),
            Ok(None) => continue, // graceful refusal — safe to try next
            Err(e) => {
                eprintln!("  {e}");
                eprintln!("  iface 2 is now stalled — REPLUG the probe, then rerun (set TICKCOUNTS to the untried values)");
                break;
            }
        }
    }
    if points.is_empty() {
        println!("\n  no tickCount in the list produced a status-0 cal1/cal2");
    } else {
        println!("\n  tickCount -> (cal1, cal2, ratio cal1/cal2):");
        for (tc, c1, c2) in &points {
            println!("    {tc:<6}  cal1={c1:<8} cal2={c2:<8} ratio={:.6}", *c1 as f64 / (*c2).max(1) as f64);
        }
    }
    Ok(())
}

/// ET_HardwareInfo (cmd 0x46): query the probe's EnergyTrace hardware descriptor.
/// Mirrors libjscxds110.so:ET_HardwareInfo@0x463dc — cmd_len=4, resp_len=0xc;
/// on success returns a u32 (payload[0..4]) and a u8 (payload[4]).
fn et_hardware_info(xds: &Xds110Handle) -> Result<(u32, u8), Box<dyn std::error::Error>> {
    println!("  ET_HardwareInfo (cmd=0x46)...");
    let tx = IcdiPacket::new(0x46, &[]);
    let mut rx = IcdiPacket::new(0, &[]);
    rx.buf.fill(0);
    for attempt in 0..2 {
        icdi_send(xds, &tx)?;
        let n = icdi_recv(xds, &mut rx)?;
        if n == 0 {
            eprintln!("  recv attempt {attempt}: timeout");
            continue;
        }
        if rx.buf[0] != SYNC_BYTE {
            eprintln!("  recv attempt {attempt}: bad sync 0x{:02x}", rx.buf[0]);
            continue;
        }
        let status = rx.response_status();
        if status != 0 {
            return Err(format!("ET_HardwareInfo status={status}").into());
        }
        let a = u32::from_le_bytes([rx.buf[7], rx.buf[8], rx.buf[9], rx.buf[10]]);
        let b = rx.buf[11];
        return Ok((a, b));
    }
    Err("ET_HardwareInfo: all retries exhausted".into())
}

/// Read `len` bytes from the probe EEPROM starting at `addr` via XDS_EEPROMRead
/// (cmd 0x3f). Returns the raw EEPROM bytes (the response status must be 0).
/// Mirrors libjscxds110.so:XDS_EEPROMRead@0x47504.
fn xds_eeprom_read(
    xds: &Xds110Handle,
    addr: u16,
    len: u16,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if len == 0 || len > EEPROM_MAX_CHUNK {
        return Err(format!("EEPROM read len {len} out of range (1..=0x10f9)").into());
    }
    // Payload: [addr:u16 LE][len:u16 LE]
    let payload = [
        (addr & 0xff) as u8,
        ((addr >> 8) & 0xff) as u8,
        (len & 0xff) as u8,
        ((len >> 8) & 0xff) as u8,
    ];
    let tx = IcdiPacket::new(CMD_XDS_EEPROM_READ, &payload);
    let mut rx = IcdiPacket::new(0, &[]);
    rx.buf.fill(0);

    for attempt in 0..2 {
        icdi_send(xds, &tx)?;
        let n = match icdi_recv(xds, &mut rx) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("  recv attempt {attempt}: {e}");
                continue;
            }
        };
        if n == 0 {
            eprintln!("  recv attempt {attempt}: timeout");
            continue;
        }
        if rx.buf[0] != SYNC_BYTE {
            eprintln!("  recv attempt {attempt}: bad sync 0x{:02x}", rx.buf[0]);
            continue;
        }
        let status = rx.response_status();
        if status != 0 {
            return Err(
                format!("XDS_EEPROMRead(addr=0x{addr:04x}, len={len}) status={status}").into(),
            );
        }
        // Response frame in our flat rx buffer: [0]=sync, [1..3]=len,
        // [3..7]=status(u32), [7..]=payload (the EEPROM bytes).
        let end = 7 + len as usize;
        if n < end {
            return Err(format!(
                "XDS_EEPROMRead short response: got {n}B, need {end}B for len={len}"
            )
            .into());
        }
        return Ok(rx.buf[7..end].to_vec());
    }
    Err("XDS_EEPROMRead: all retries exhausted".into())
}

fn et_setup(
    xds: &Xds110Handle,
    mode: u8,
    sample_rate: u32,
    dig_mode: u8,
    dig_enable: u8,
) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  ET_Setup (cmd=0x1d) mode={mode} rate={sample_rate}...");
    // Payload: [mode:1][dig_mode:1][sample_rate:4][dig_enable:1] = 7 bytes
    let payload = [
        mode,
        dig_mode,
        (sample_rate & 0xff) as u8,
        ((sample_rate >> 8) & 0xff) as u8,
        ((sample_rate >> 16) & 0xff) as u8,
        ((sample_rate >> 24) & 0xff) as u8,
        dig_enable,
    ];
    icdi_execute(xds, CMD_ET_SETUP, &payload, 2)
}

fn et_start(xds: &Xds110Handle) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  ET_Start (cmd=0x1f)...");
    icdi_execute(xds, CMD_ET_START, &[], 2)
}

fn et_setup_range(xds: &Xds110Handle, range: u8) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  ET_Setup_Range (cmd=0x30) range={range}...");
    icdi_execute(xds, CMD_ET_SETUP_RANGE, &[range], 2)
}

fn et_dcdc_set_vcc(xds: &Xds110Handle, vcc_mv: u16) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  ET_DCDC_SetVcc (cmd=0x24) vcc_mv={vcc_mv}...");
    icdi_execute(xds, CMD_ET_DCDC_SET_VCC, &vcc_mv.to_le_bytes(), 2)
}

fn et_dcdc_restart(xds: &Xds110Handle) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  ET_DCDC_RestartMCU (cmd=0x25)...");
    icdi_execute(xds, CMD_ET_DCDC_RESTART, &[], 2)
}

fn et_stop(xds: &Xds110Handle) -> Result<i32, Box<dyn std::error::Error>> {
    println!("  ET_Stop (cmd=0x20)...");
    icdi_execute(xds, CMD_ET_STOP, &[], 2)
}

/// Read raw EnergyTrace data from the data stream endpoint (0x87 on interface 6).
/// This is a raw bulk_read with no ICDI framing — the probe streams event records
/// directly after ET_Start.
fn et_read_data(
    xds: &Xds110Handle,
    buf: &mut [u8],
    timeout: Duration,
) -> Result<usize, Box<dyn std::error::Error>> {
    match xds.handle.read_bulk(EP_DATA_IN, buf, timeout) {
        Ok(n) => Ok(n),
        Err(rusb::Error::Timeout) => Ok(0),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// ET record parsing
// ---------------------------------------------------------------------------

// NOTE: the libmsp430 "eventID=8" record struct and the 24-bit-signed
// `ProcessAnalogSamples` accumulator path that used to live here have been
// removed. Neither applies to the XDS110v3 iface-6 stream: that stream is the
// already-decimated `byte[2]` pulse-count format documented at the top of this
// file and verified in beads mspm0sleep-a78.3. The TI accumulator path decodes
// a different (MSP430/older-probe) wire format. See REVERSE_ENGINEERING.md
// §"EnergyTrace Calibration Architecture" for the full TI calibration flow and
// why it needs the probe's on-EEPROM `_CalibLoads` table.

// ---------------------------------------------------------------------------
// Plotting
// ---------------------------------------------------------------------------

/// Print a 60×20 ASCII line plot of the binned current series to stdout, so
/// the shape is visible in terminal output without opening the SVG.
fn print_ascii_plot(bin_pulses: &[u64], bin_ms: f64, cal2: f64) {
    if bin_pulses.is_empty() {
        return;
    }
    const WIDTH: usize = 80;
    const HEIGHT: usize = 18;
    let bin_s = bin_ms / 1000.0;
    let na_per_pulse = 1_000_000.0 / cal2;

    // Resample to WIDTH columns by averaging.
    let cols: Vec<f64> = (0..WIDTH)
        .map(|c| {
            let lo = c * bin_pulses.len() / WIDTH;
            let hi = ((c + 1) * bin_pulses.len() / WIDTH).max(lo + 1);
            let sum: u64 = bin_pulses[lo..hi.min(bin_pulses.len())].iter().sum();
            let n = (hi - lo).max(1);
            // pulses/sec across this bin range, then to µA.
            (sum as f64 / (n as f64 * bin_s)) * na_per_pulse / 1_000.0
        })
        .collect();

    let lo = cols.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = cols.iter().copied().fold(0.0f64, f64::max);
    let span = (hi - lo).max(1.0);

    println!("\n  Time-series ({} ms bins, {} columns over {:.2} s):", bin_ms as u64, WIDTH, bin_pulses.len() as f64 * bin_s);
    println!("  {:>6} µA ┐", hi as i64);
    let mut grid = vec![vec![' '; WIDTH]; HEIGHT];
    for (c, &v) in cols.iter().enumerate() {
        let row = HEIGHT - 1 - (((v - lo) / span * (HEIGHT - 1) as f64).round() as usize).min(HEIGHT - 1);
        grid[row][c] = '*';
    }
    for row in &grid {
        println!("            │{}", row.iter().collect::<String>());
    }
    println!("  {:>6} µA ┴{}", lo as i64, "─".repeat(WIDTH));
    println!("            0{}{:>w$.1}s", " ".repeat(WIDTH - 6), bin_pulses.len() as f64 * bin_s, w = 5);
}

/// Compute the time-vs-current series from binned pulse counts.
fn compute_series(bin_pulses: &[u64], bin_ms: f64, cal2: f64) -> Vec<(f64, f64)> {
    let bin_s = bin_ms / 1000.0;
    let na_per_pulse = 1_000_000.0 / cal2;
    bin_pulses
        .iter()
        .enumerate()
        .map(|(i, &p)| {
            let t = (i as f64 + 0.5) * bin_s;
            let current_ua = (p as f64 / bin_s) * na_per_pulse / 1_000.0;
            (t, current_ua)
        })
        .collect()
}

/// Compute (t_max, y_lo, y_hi) bounds for plotting.
fn series_bounds(series: &[(f64, f64)]) -> (f64, f64, f64) {
    let t_max = series.last().map(|&(t, _)| t).unwrap_or(1.0);
    let i_max = series.iter().map(|&(_, c)| c).fold(0f64, f64::max);
    let i_min = series.iter().map(|&(_, c)| c).fold(f64::INFINITY, f64::min);
    let y_pad = (i_max - i_min).max(1.0) * 0.1;
    (t_max, (i_min - y_pad).min(0.0), i_max + y_pad)
}

/// Render the line + 0-baseline onto the supplied drawing area. Caller chooses
/// the backend (BitMapBackend for PNG, SVGBackend for SVG).
fn draw_chart<DB: DrawingBackend>(
    root: DrawingArea<DB, plotters::coord::Shift>,
    series: &[(f64, f64)],
) -> Result<(), Box<dyn std::error::Error>>
where
    DB::ErrorType: 'static,
{
    let (t_max, y_lo, y_hi) = series_bounds(series);
    root.fill(&WHITE)?;
    let mut chart = ChartBuilder::on(&root)
        .margin(20)
        .build_cartesian_2d(0f64..t_max, y_lo..y_hi)?;
    chart
        .configure_mesh()
        .disable_x_axis()
        .disable_y_axis()
        .disable_x_mesh()
        .disable_y_mesh()
        .draw()?;
    chart.draw_series(std::iter::once(PathElement::new(
        vec![(0.0, 0.0), (t_max, 0.0)],
        ShapeStyle::from(&BLACK.mix(0.2)),
    )))?;
    chart.draw_series(LineSeries::new(series.iter().copied(), &BLUE))?;
    root.present()?;
    Ok(())
}

/// Render to PNG and SVG side-by-side. Both share the same base path; the
/// SVG path replaces .png with .svg (or appends .svg if the base has no
/// extension).
fn render_plot(
    bin_pulses: &[u64],
    bin_ms: f64,
    cal2: f64,
    out_path: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if bin_pulses.is_empty() {
        return Err("no data to plot".into());
    }
    let series = compute_series(bin_pulses, bin_ms, cal2);

    // PNG (no text labels — plotters' default text path needs fontconfig).
    let png_root = BitMapBackend::new(out_path, (1400, 480)).into_drawing_area();
    draw_chart(png_root, &series)?;

    // Same plot, SVG.
    let svg_path = std::path::Path::new(out_path)
        .with_extension("svg")
        .to_string_lossy()
        .into_owned();
    let svg_root = SVGBackend::new(&svg_path, (1400, 480)).into_drawing_area();
    draw_chart(svg_root, &series)?;
    Ok(svg_path)
}

// ---------------------------------------------------------------------------
// EEPROM dump
// ---------------------------------------------------------------------------

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                s.parse().ok()
            }
        })
        .unwrap_or(default)
}

/// Print a classic 16-byte-per-line hexdump with ASCII gutter.
fn hexdump(data: &[u8], base: u16) {
    for (i, row) in data.chunks(16).enumerate() {
        let addr = base as usize + i * 16;
        let hex: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = row
            .iter()
            .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
            .collect();
        println!("  {addr:04x}  {:<47}  {ascii}", hex.join(" "));
    }
}

/// Dump the probe EEPROM via XDS_EEPROMRead (cmd 0x3f). Self-contained: does the
/// DAP reset + connect, then reads `EEPROM_SIZE` bytes from `EEPROM_ADDR` in
/// `EEPROM_CHUNK`-byte requests, writes them to `EEPROM_OUT`, and hexdumps them.
/// This is the USB/EEPROM path for recovering the probe's `_CalibLoads` table
/// (beads mspm0sleep-dln). A failed read stalls iface 2 for the rest of the
/// session — we stop on the first error and warn to replug.
fn eeprom_dump(xds: &Xds110Handle) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== EEPROM DUMP MODE (XDS_EEPROMRead cmd 0x3f) ===");

    // Clear any probe-rs CMSIS-DAP state, then connect (ICDI requires a connect
    // as the first command). CONNECT selects which: et (default, verified) or
    // xds (plain 0x01) or none.
    dap_reset(xds);
    match std::env::var("CONNECT").unwrap_or_else(|_| "et".into()).as_str() {
        "none" => println!("  CONNECT=none — skipping connect"),
        "xds" => {
            let st = xds_connect(xds)?;
            println!("  XDS_Connect status = {st}");
        }
        _ => {
            let st = et_connect(xds)?;
            println!("  XDS_ConnectET status = {st}");
        }
    }

    // Query the ET hardware descriptor first (well-formed, cheap). Tells us
    // whether the probe exposes an EnergyTrace HW block / version at all.
    match et_hardware_info(xds) {
        Ok((a, b)) => println!("  ET_HardwareInfo => u32=0x{a:08x} ({a})  u8=0x{b:02x} ({b})"),
        Err(e) => eprintln!("  ET_HardwareInfo failed: {e}"),
    }

    // EEPROM_KEEPGOING: continue past per-address rejections instead of
    // stopping. Lets us learn whether a -390 rejection stalls iface 2 (next
    // read times out) or is graceful (next read also returns a clean status),
    // and whether any address region is readable at all.
    let keep_going = std::env::var("EEPROM_KEEPGOING").is_ok();
    let start_addr = env_u32("EEPROM_ADDR", 0) as u16;
    let total = env_u32("EEPROM_SIZE", 0x400) as usize; // 1 KiB default
    let chunk = env_u32("EEPROM_CHUNK", 64).clamp(1, EEPROM_MAX_CHUNK as u32) as u16;
    let out = std::env::var("EEPROM_OUT").unwrap_or_else(|_| "/tmp/xds110_eeprom.bin".into());
    println!(
        "  reading {total} bytes from 0x{start_addr:04x} in {chunk}-byte chunks (CONNECT, EEPROM_ADDR/SIZE/CHUNK/OUT to override)"
    );

    let mut data: Vec<u8> = Vec::with_capacity(total);
    let mut addr = start_addr;
    // Bound the loop by *address* covered, not by bytes collected — otherwise a
    // keep-going sweep where every read errors never fills `data` and spins.
    let end_addr = start_addr.wrapping_add(total as u16);
    while addr < end_addr {
        let want = chunk.min(end_addr.wrapping_sub(addr));
        match xds_eeprom_read(xds, addr, want) {
            Ok(bytes) => {
                data.extend_from_slice(&bytes);
                addr = addr.wrapping_add(want);
            }
            Err(e) => {
                eprintln!("  err at 0x{addr:04x}: {e}");
                if keep_going {
                    addr = addr.wrapping_add(want);
                    continue;
                }
                eprintln!("  (iface 2 may now be stalled — physically replug before the next run)");
                break;
            }
        }
    }

    std::fs::write(&out, &data)?;
    println!("\n  Dumped {} bytes to {out}\n", data.len());
    hexdump(&data, start_addr);
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== XDS110 EnergyTrace via ICDI Protocol ===");

    let xds = open_xds110()?;

    // EEPROM dump mode (EEPROM_DUMP=1): recover the probe's calibration store
    // via XDS_EEPROMRead, then exit without running the ET capture flow.
    if std::env::var("EEPROM_DUMP").is_ok() {
        let r = eeprom_dump(&xds);
        xds.handle.release_interface(CMD_IFACE)?;
        xds.handle.release_interface(DATA_IFACE)?;
        return r;
    }

    // CALIBRATE mode (CALIBRATE=1): probe ET_Calibrate for real cal1/cal2.
    if std::env::var("CALIBRATE").is_ok() {
        let r = calibrate_probe(&xds);
        let _ = xds.handle.release_interface(CMD_IFACE);
        let _ = xds.handle.release_interface(DATA_IFACE);
        return r;
    }

    // Flow mirrors XDS_Open + EnergyTrace_LPRF::InitEnergyTrace from
    // libjscxds110.so / libenergytracestandalone.so:
    //   0. DAP_Disconnect  — clear any CMSIS-DAP state left by probe-rs
    //   1. XDS_ConnectET   — must be the FIRST ICDI command on the wire
    //   2. ET_Calibrate    — performed inside InitEnergyTrace::PerformCalibration
    //   3. ET_Setup
    //   4. ET_Start

    // 0. Clear probe-rs CMSIS-DAP state so ICDI works without a physical
    // replug. No-op on a freshly-replugged probe. See mspm0sleep-a78.6.
    println!("\n--- Step 0: Reset CMSIS-DAP state ---");
    dap_reset(&xds);

    // 1. XDS_ConnectET (must come first; XDS_Open in the TI library always
    // sends this immediately after claiming interfaces 2 and 6).
    println!("\n--- Step 1: Connect EnergyTrace ---");
    let status = et_connect(&xds)?;
    println!("  status = {status}");

    // 1b. DCDC init — set VCC to 3300 mV and restart the DCDC MCU. Without
    // these, the probe streams sample-counter ticks but doesn't actually
    // measure target current. Equivalent of MSP430_VCC(3300) in the libmsp430
    // (XDS110 pre-v3) flow.
    println!("\n--- Step 1b: DCDC init ---");
    let status = et_dcdc_set_vcc(&xds, 3300)?;
    println!("  ET_DCDC_SetVcc status = {status}");
    let status = et_dcdc_restart(&xds)?;
    println!("  ET_DCDC_RestartMCU status = {status}");

    // 2. Calibrate — read the probe's real calibration constant LIVE via
    // ET_Calibrate (cmd 0x1e), mirroring EnergyTrace_LPRF::PerformCalibration.
    // VERIFIED 2026-05-25 (beads mspm0sleep-dln): the probe returns cal1
    // (a settling offset) and cal2 (the per-unit scale constant). cal2 is
    // rock-stable at ~10186 for any tickCount; cal1 settles to ~17 once
    // tickCount >= 10. cal2 IS the "nA per (pulse/s)" scale in TI's GetCurrentInNA
    // model (current_nA = pulses · 1e6 / cal2), so we read it straight from the
    // probe instead of hardcoding. tickCount=1000 is in the settled region and
    // returns gracefully (no iface stall). CAL2 env still overrides.
    println!("\n--- Step 2: Calibrate (read cal2 from probe) ---");
    let cal2: f64 = if let Ok(s) = std::env::var("CAL2") {
        let v = s.parse().unwrap_or(DEFAULT_CAL2);
        println!("  cal2 = {v} nA per (pulse/s) (CAL2 override)");
        v
    } else {
        match et_calibrate_once(&xds, 1000) {
            Ok(Some((cal1, c2))) => {
                println!("  ET_Calibrate → cal1(offset)={cal1}, cal2(scale)={c2}; using live cal2");
                c2 as f64
            }
            Ok(None) => {
                eprintln!("  ET_Calibrate refused (non-zero status); falling back to DEFAULT_CAL2={DEFAULT_CAL2}");
                DEFAULT_CAL2
            }
            Err(e) => {
                eprintln!("  ET_Calibrate failed ({e}); falling back to DEFAULT_CAL2={DEFAULT_CAL2}");
                DEFAULT_CAL2
            }
        }
    };

    // 3. ET_Setup (analog profiling mode, 10 kHz samples by default)
    println!("\n--- Step 3: Setup EnergyTrace ---");
    let mode: u8 = 0; // ET_PROFILING_ANALOG
    let sample_rate: u32 = std::env::var("SAMPLE_RATE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10000);
    let dig_mode: u8 = 0;
    let dig_enable: u8 = 0;
    let status = et_setup(&xds, mode, sample_rate, dig_mode, dig_enable)?;
    println!("  status = {status}");

    // 3b. ET_Setup_Range — selects current sense range. Optional; only sent
    // if the RANGE env var is set. Plausible values: 0 (low-current) and 1
    // (high-current), but not yet verified.
    if let Ok(s) = std::env::var("RANGE") {
        if let Ok(range) = s.parse::<u8>() {
            println!("\n--- Step 3b: Set range ---");
            let status = et_setup_range(&xds, range)?;
            println!("  status = {status}");
        }
    }

    // 4. ET_Start
    println!("\n--- Step 4: Start Collection ---");
    let status = et_start(&xds)?;
    println!("  status = {status}");

    // 5. Poll for data — dump raw URBs to disk and compute current estimate.
    //
    // Sample-byte interpretation (verified against busy-loop + LED loads):
    //   byte[0] = 0x70 (frame marker, constant)
    //   byte[1] = sample-sequence counter, increments by `byte[2]` each frame
    //             (i.e. byte[1] is the cumulative count of charge pulses;
    //              byte[2] is the per-sample pulse delta for this frame)
    //   byte[2] = pulses-since-last-sample for this frame
    //   byte[3] = digital flags (0 in pure analog mode)
    //
    // The XDS110-ET DCDC delivers a fixed-charge pulse per increment, so
    // per-second pulse rate × (1e6 / cal2) gives current in nA. cal2=10186
    // is the placeholder pulled from REVERSE_ENGINEERING.md; replace once
    // ET_Calibrate is working.
    println!("\n--- Step 5: Polling data (5 s, raw dump) ---");

    let raw_path = std::env::var("RAW_OUT").unwrap_or_else(|_| "/tmp/etrace_raw.bin".into());
    let mut raw_file = std::fs::File::create(&raw_path)?;
    use std::io::Write;

    let duration_secs: u64 = std::env::var("DURATION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    println!("  Capture duration: {duration_secs} s");

    // Bin width for the time-series plot. 10 ms = 100 sample windows at 10 kHz.
    let bin_ms: f64 = std::env::var("BIN_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    // Time-base for the binning is derived from the sample rate we requested
    // in ET_Setup, not the default. byte[1] is a per-window counter at this
    // rate.
    let sample_period_s = 1.0 / (sample_rate as f64);
    let windows_per_bin: u64 = ((bin_ms / 1000.0) / sample_period_s).round() as u64;

    let start = std::time::Instant::now();
    let mut buf = vec![0u8; ET_DATA_BUF_SIZE];
    let mut total_bytes = 0u64;
    let mut urb_count = 0u64;
    let mut sample_count = 0u64;
    let mut pulse_total = 0u64; // sum of byte[2] across all samples
    // Time-binning state. byte[1] is a cumulative 10 kHz window counter (mod 256);
    // we track the running window count by accumulating its delta on each record,
    // handling the 256-wraparound. cum_windows × 100 µs = elapsed time.
    let mut prev_b1: Option<u8> = None;
    let mut cum_windows: u64 = 0;
    let mut bin_pulses: Vec<u64> = Vec::new();
    while start.elapsed() < Duration::from_secs(duration_secs) {
        match et_read_data(&xds, &mut buf, DATA_TIMEOUT) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(n) => {
                urb_count += 1;
                total_bytes += n as u64;
                raw_file.write_all(&buf[..n])?;

                // First URB has an 8-byte timestamp header, subsequent URBs
                // start directly at sample 0. Sample stride is 4 bytes.
                let payload = if urb_count == 1 && n > 8 {
                    &buf[8..n]
                } else {
                    &buf[..n]
                };
                for chunk in payload.chunks_exact(4) {
                    if chunk[0] != 0x70 {
                        continue; // skip non-sample bytes
                    }
                    sample_count += 1;
                    pulse_total += chunk[2] as u64;

                    // Advance window counter by the byte[1] delta (mod 256).
                    let b1 = chunk[1];
                    let step = match prev_b1 {
                        None => 1u64,
                        Some(prev) => {
                            let d = b1.wrapping_sub(prev);
                            if d == 0 { 256 } else { d as u64 }
                        }
                    };
                    cum_windows += step;
                    prev_b1 = Some(b1);

                    let bin_idx = (cum_windows / windows_per_bin) as usize;
                    if bin_idx >= bin_pulses.len() {
                        bin_pulses.resize(bin_idx + 1, 0);
                    }
                    bin_pulses[bin_idx] += chunk[2] as u64;
                }

                if urb_count <= 2 {
                    let preview = &buf[..n.min(48)];
                    println!("  URB#{urb_count} {n} bytes: {:02x?}", preview);
                }
            }
            Err(e) => {
                eprintln!("  read error: {e}");
                break;
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let avg_pulses_per_sample = pulse_total as f64 / sample_count.max(1) as f64;
    let pulse_rate_hz = pulse_total as f64 / elapsed;
    let na_per_pulse = 1_000_000.0 / cal2;
    let current_na = pulse_rate_hz * na_per_pulse;
    let current_ua = current_na / 1_000.0; // nA → µA: divide by 1000 (NOT 1e6 — that was the previous bug)
    println!();
    println!("  URBs read       : {urb_count}");
    println!("  Total bytes     : {total_bytes}");
    println!("  Samples         : {sample_count}");
    println!("  Avg sample rate : {:.0} Hz", sample_count as f64 / elapsed);
    println!("  Pulse total     : {pulse_total}");
    println!("  Pulses/sample   : {:.4}", avg_pulses_per_sample);
    println!("  Pulses/sec      : {:.0}", pulse_rate_hz);
    println!("  cal2 (nA per pulse/s): {cal2}");
    println!("  Estimated current  : {:.3} µA  ({:.3} mA)", current_ua, current_ua / 1000.0);
    println!("  Raw stream saved to: {raw_path}");

    // Render time-vs-current plot via plotters (PNG — no text labels because
    // plotters' default text path needs fontconfig; the ASCII summary
    // printed below carries the numeric context).
    let plot_path = std::env::var("PLOT_OUT").unwrap_or_else(|_| {
        std::path::Path::new(&raw_path)
            .with_extension("png")
            .to_string_lossy()
            .into_owned()
    });
    match render_plot(&bin_pulses, bin_ms, cal2, &plot_path) {
        Err(e) => eprintln!("  plot render failed: {e}"),
        Ok(svg_path) => {
            println!("  Plot rendered to   : {plot_path}");
            println!("  SVG version        : {svg_path}");
        }
    }
    print_ascii_plot(&bin_pulses, bin_ms, cal2);

    // 6. ET_Stop
    println!("\n--- Step 6: Stop Collection ---");
    let status = et_stop(&xds)?;
    println!("  status = {status}");

    xds.handle.release_interface(CMD_IFACE)?;
    xds.handle.release_interface(DATA_IFACE)?;
    println!("\nDone.");
    Ok(())
}
