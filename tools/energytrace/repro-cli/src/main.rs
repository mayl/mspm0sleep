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
const CMD_ET_CALIBRATE: u8 = 0x1e;
const CMD_ET_START: u8 = 0x1f;
const CMD_ET_STOP: u8 = 0x20;
const CMD_XDS_CONNECT_ET: u8 = 0x28;

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

    // Claim both command interface (2) and data interface (6)
    claim_iface(&handle, CMD_IFACE)?;
    claim_iface(&handle, DATA_IFACE)?;

    Ok(Xds110Handle { handle, _ctx: ctx })
}

fn icdi_send(xds: &Xds110Handle, packet: &IcdiPacket) -> Result<(), rusb::Error> {
    let len = packet.len();
    xds.handle
        .write_bulk(EP_CMD_OUT, &packet.buf[..len], CMD_TIMEOUT)?;
    Ok(())
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

    // 1. Calibrate to get probe calibration values
    println!("\n--- Step 1: Calibrate ---");
    let (cal1_raw, cal2_raw) = et_calibrate(&xds, 0)?;
    let cal2 = cal2_raw as f64;
    println!("  cal1={cal1_raw} cal2={cal2_raw} (tickCount=0)");

    // 2. XDS_ConnectET
    println!("\n--- Step 2: Connect EnergyTrace ---");
    let status = et_connect(&xds)?;
    println!("  status = {status}");

    // 3. ET_Setup (analog profiling mode, 10 kHz samples)
    println!("\n--- Step 3: Setup EnergyTrace ---");
    let mode: u8 = 0; // ET_PROFILING_ANALOG
    let sample_rate: u32 = 10000;
    let dig_mode: u8 = 0;
    let dig_enable: u8 = 0;
    let status = et_setup(&xds, mode, sample_rate, dig_mode, dig_enable)?;
    println!("  status = {status}");

    // 4. ET_Start
    println!("\n--- Step 4: Start Collection ---");
    let status = et_start(&xds)?;
    println!("  status = {status}");

    // 5. Poll for data + convert to engineering units
    println!("\n--- Step 5: Polling data (5 s) ---");
    let start = std::time::Instant::now();
    let mut total_samples = 0u32;
    let mut buf = vec![0u8; ET_DATA_BUF_SIZE];

    // EnergyTrace measures current by counting DC/DC converter charge pulses.
    // The raw 4-byte samples from the probe are counter values. To convert:
    //   1. Extract 24-bit signed sample (byte[2]<<16 | byte[1]<<8 | byte[0])
    //   2. The sample has a constant baseline (0x70 = 112) in the LSB — this is
    //      a fixed component, not measurement data.
    //   3. Subtract the running baseline to isolate the actual pulse counts.
    //   4. Apply calibration: current_nA = pulse_count * 1000000.0 / cal2
    //
    //   From GetCurrentInNA (libenergytracestandalone.so):
    //     current_nA = (double)offset * 1000.0 * 1000.0 / calib_loads[index]
    //   where cal2 = 10186 for this probe.
    //
    //   For ProcessAnalogSamples, the calibLine parameters should be:
    //     line 0: slope = energy_per_pulse_conversion, offset = baseline
    //     line 1: slope = high_range_conversion, offset = upper_threshold
    let na_per_pulse = 1_000_000.0 / cal2;
    let mut prev_16bit: u32 = 0;
    let mut delta_sum: u64 = 0;
    let mut delta_count: u32 = 0;
    while start.elapsed() < Duration::from_secs(5) {
        match et_read_data(&xds, &mut buf, DATA_TIMEOUT) {
            Ok(0) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(n) => {
                // Skip first 8 bytes (timestamp header), process 4-byte samples
                let payload = if n > 8 { &buf[8..n] } else { continue; };
                // Only process if we have complete 4-byte chunks
                let payload = &payload[..payload.len() - (payload.len() % 4)];
                for chunk in payload.chunks_exact(4) {
                    let counter16 = (chunk[2] as u32) << 8 | (chunk[1] as u32);
                    total_samples += 1;

                    if prev_16bit > 0 && counter16 > prev_16bit && counter16 - prev_16bit < 0x100 {
                        let delta = counter16 - prev_16bit;
                        delta_sum += delta as u64;
                        delta_count += 1;
                    }
                    prev_16bit = counter16;

                    if total_samples <= 20 {
                        println!(
                            "  [{total_samples:4}] b=[{:02x} {:02x} {:02x} {:02x}] cnt16={counter16:4}",
                            chunk[0], chunk[1], chunk[2], chunk[3],
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("  read error: {e}");
                break;
            }
        }
    }

    println!();
    if delta_count > 0 {
        let avg_delta = delta_sum as f64 / delta_count as f64;
        let total_seconds = start.elapsed().as_secs_f64();
        let pulse_rate_hz = avg_delta * total_samples as f64 / total_seconds;
        println!("  Avg 16-bit counter delta: {avg_delta:.3} counts/sample");
        println!("  Pulse rate: {pulse_rate_hz:.0} Hz");
        let current_na = pulse_rate_hz * na_per_pulse;
        println!("  Estimated current: {:.3} µA", current_na / 1_000_000.0);
    }
    println!("  Total raw samples: {total_samples}");
    let total_seconds = start.elapsed().as_secs_f64();
    println!("  Total samples: {total_samples} over {total_seconds:.1}s");
    println!("  Sample rate: {:.0} Hz", total_samples as f64 / total_seconds);

    // 6. ET_Stop
    println!("\n--- Step 6: Stop Collection ---");
    let status = et_stop(&xds)?;
    println!("  status = {status}");

    xds.handle.release_interface(CMD_IFACE)?;
    xds.handle.release_interface(DATA_IFACE)?;
    println!("\nDone.");
    Ok(())
}
