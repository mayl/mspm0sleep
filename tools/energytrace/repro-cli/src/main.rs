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
//!   4-byte LE samples = raw DC/DC converter pulse counts
//!   Byte[0..2] = 24-bit signed sample value
//!   Byte[3] = digital flags (unused in analog mode)
//!
//! Conversion pipeline (reverse-engineered from libenergytracestandalone.so):
//!   1. Sample extraction: (byte[2]<<16) | (byte[1]<<8) | byte[0]
//!      Sign adjustment if > 0x7FFFFF → subtract 0x1000000
//!   2. Calibration: accumulator += (sample - calibLine.offset)
//!      When accumulator exceeds threshold:
//!      energy_pulses = accumulator * calibLine.slope
//!      accumulator is reset
//!   3. Current conversion:
//!      GetCurrentInNA(index) = offset * 1000000.0 / calib_loads[index]
//!      where offset is the calibrated offset, calib_loads are from probe

use rusb::{Context, DeviceHandle, UsbContext};
use std::time::Duration;

const TI_VID: u16 = 0x0451;
const XDS110_PIDS: &[u16] = &[0xbef3, 0xbef4, 0x1cbe, 0x029e, 0x029f, 0x02a5];

// ICDI protocol constants
const SYNC_BYTE: u8 = 0x2a;
const CMD_ET_SETUP: u8 = 0x1d;
#[allow(dead_code)]
const CMD_ET_CALIBRATE: u8 = 0x1e;
const CMD_ET_START: u8 = 0x1f;
const CMD_ET_STOP: u8 = 0x20;
const CMD_XDS_CONNECT_ET: u8 = 0x28;
const CMD_ET_SETUP_RANGE: u8 = 0x30;
const CMD_ET_DCDC_SET_VCC: u8 = 0x24;
const CMD_ET_DCDC_RESTART: u8 = 0x25;

// Interface 2: Command channel (ICDI framing)
const CMD_IFACE: u8 = 2;
const EP_CMD_IN: u8 = 0x83;
const EP_CMD_OUT: u8 = 0x02;

// Interface 6: Data stream (raw EnergyTrace records)
const DATA_IFACE: u8 = 6;
const EP_DATA_IN: u8 = 0x87;

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
    // recover. After running probe-rs (CMSIS-DAP), a manual replug is currently
    // needed before ICDI commands work; see beads epic mspm0sleep-a78.

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
/// flush any residue from a prior session before issuing new commands.
fn drain_endpoint(xds: &Xds110Handle, ep: u8) {
    let mut tmp = [0u8; 4096];
    loop {
        match xds.handle.read_bulk(ep, &mut tmp, Duration::from_millis(50)) {
            Ok(0) => return,
            Ok(n) => eprintln!("    [drain ep=0x{ep:02x}] discarded {n} bytes: {:02x?}", &tmp[..n.min(32)]),
            Err(_) => return,
        }
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
// EnergyTrace API
// ---------------------------------------------------------------------------

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

#[allow(dead_code)]
#[derive(Debug, Default)]
struct EtEvent {
    timestamp_us: u64,
    current_na: u32,
    voltage_mv: u16,
    energy_01uj: u32, // 0.1 microjoule units
}

/// DecodeStateMachineLPRF-compatible analog sample processor.
///
/// Matches the algorithm at `ProcessAnalogSamples` (0x44fe2 in
/// libenergytracestandalone.so):
///
///   1. Extract 24-bit signed sample from each 4-byte chunk:
///      `(byte[2]<<16) | (byte[1]<<8) | byte[0]`
///      Subtract 0x1000000 if value > 0x7FFFFF
///   2. Accumulate `(sample - calibLine.offset)` in a double accumulator
///   3. When accumulator > 0, produce an energy pulse count:
///      `pulses = (int)(accumulator * calibLine.slope)`
///   4. Accumulator is then reset
///
/// Returns (sample_values, energy_pulse_counts) for each 4-byte chunk.
#[allow(dead_code)]
fn process_analog_samples(
    data: &[u8],
    calib_lines: &[(f64, f64)], // [(slope, offset), ...]
) -> Vec<(i32, i32)> {
    let mut results = Vec::new();
    if calib_lines.is_empty() {
        return results;
    }
    let mut accumulator: f64 = 0.0;

    for chunk in data.chunks_exact(4) {
        // 24-bit signed extraction (byte[0], byte[1], byte[2])
        let raw = (chunk[2] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[0] as u32;
        let sample = if raw > 0x7FFFFF {
            (raw.wrapping_sub(0x1000000)) as i32
        } else {
            raw as i32
        };
        let _dig_flags = chunk[3]; // byte[3] = digital flags (unused in analog mode)

        // Select calibration line: line[0] is used unless
        // sample exceeds calibLines[1].offset
        let calib = if calib_lines.len() > 1 && (sample as f64) > calib_lines[1].1 {
            &calib_lines[1]
        } else {
            &calib_lines[0]
        };

        // Accumulate: accumulator += (sample - calibLine.offset)
        accumulator += sample as f64 - calib.1;

        // When accumulator exceeds 0, produce energy pulse count
        let energy_pulses = if accumulator > 0.0 {
            let pulses = (accumulator * calib.0) as i32;
            accumulator = 0.0;
            pulses
        } else {
            0
        };

        results.push((sample, energy_pulses));
    }

    results
}

/// Convert pulse counts to current (microamps) using the
/// EnergyTrace_LPRF::GetCurrentInNA formula:
///   current_nA = offset * 1_000_000.0 / calib_load
/// where calib_load is determined from calibration data.
#[allow(dead_code)]
fn pulses_to_current_ua(pulses: i32, offset: f64, calib_load: f64) -> f64 {
    if calib_load == 0.0 {
        return 0.0;
    }
    let current_na = offset * 1_000_000.0 / calib_load;
    // Scale pulses by current and convert to µA
    pulses as f64 * current_na / 1_000_000.0
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== XDS110 EnergyTrace via ICDI Protocol ===");

    let xds = open_xds110()?;

    // Flow mirrors XDS_Open + EnergyTrace_LPRF::InitEnergyTrace from
    // libjscxds110.so / libenergytracestandalone.so:
    //   1. XDS_ConnectET   — must be the FIRST command on the wire
    //   2. ET_Calibrate    — performed inside InitEnergyTrace::PerformCalibration
    //   3. ET_Setup
    //   4. ET_Start

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

    // 2. Calibrate (optional — skipped for now). EmulatorComm_XDS110::CalibrateTicks
    // is invoked from EnergyTrace_LPRF::PerformCalibration in a loop over
    // `_CalibLoads`, with tickCount taken from each load entry. The right
    // tickCount values are not yet known, and tickCount=0 gets no response
    // from this XDS110v3 firmware. Streaming raw data still works without
    // it; we just lose the µA conversion factor (we'll recover it from
    // physics — known LED current vs busy-loop delta — in a follow-up).
    let cal2: f64 = 10186.0; // placeholder from REVERSE_ENGINEERING.md; current calc is approximate

    // 3. ET_Setup (analog profiling mode, 10 kHz samples)
    println!("\n--- Step 3: Setup EnergyTrace ---");
    let mode: u8 = 0; // ET_PROFILING_ANALOG
    let sample_rate: u32 = 10000;
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

    let start = std::time::Instant::now();
    let mut buf = vec![0u8; ET_DATA_BUF_SIZE];
    let mut total_bytes = 0u64;
    let mut urb_count = 0u64;
    let mut sample_count = 0u64;
    let mut pulse_total = 0u64; // sum of byte[2] across all samples
    while start.elapsed() < Duration::from_secs(5) {
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
    println!("  cal2 (placeholder) : {cal2}");
    println!("  Estimated current  : {:.3} µA  ({:.3} mA)", current_ua, current_ua / 1000.0);
    println!("  Raw stream saved to: {raw_path}");

    // 6. ET_Stop
    println!("\n--- Step 6: Stop Collection ---");
    let status = et_stop(&xds)?;
    println!("  status = {status}");

    xds.handle.release_interface(CMD_IFACE)?;
    xds.handle.release_interface(DATA_IFACE)?;
    println!("\nDone.");
    Ok(())
}
