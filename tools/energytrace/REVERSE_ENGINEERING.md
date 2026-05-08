# EnergyTrace Protocol Reverse Engineering

## Objective

Reverse-engineer the USB protocol used by TI's EnergyTrace hardware to measure current/voltage/energy on MSPM0 and MSP430 targets, so we can build a standalone CLI tool (no CCS, no libmsp430.so dependency).

## libmsp430.so Findings

### Store Paths (Nix)

These are needed by any agent doing binary analysis on this library:

| Artifact | Nix expression | Store Path |
|---|---|---|
| msp-debug-stack-bin (libmsp430.so) | `nixpkgs#mspds-bin` (unfree) | `/nix/store/xa7y5gynaznhyfj0143f3i03bbzkv16g-msp-debug-stack-bin-3.15.1.1` |
| mspds-bin source (TI zip) | via nixpkgs | `/nix/store/wrhnyhk4mg22kd0n74n37ws1mcx51jbj-MSP430_DLL_Developer_Package_Rev_3_15_1_1.zip` |
| energytrace-util | `.#energytrace-util` | `/nix/store/55s5brbp3f60wfbj72fqm4ns2jylff02-energytrace-util-unstable-2021-10-24` |
| energytrace-util source | via flake.nix (GitHub) | `/nix/store/q911pqvhwmi20ivlgfamvy2lmsmlm0vv-source` |

The source path is obtained with:
```bash
nix eval .#energytrace-util.src.outPath
```

The mspds store path is obtained with:
```bash
nix eval 'nixpkgs#mspds-bin.outPath'
```

The dynamic symbols of libmsp430.so are examined with:
```bash
nix develop .# --no-pure-eval --command bash -c 'objdump -T <PATH>/lib/libmsp430.so'
```

### Binary Analysis Commands

All analysis of libmsp430.so requires entering the Nix dev shell first:
```bash
nix develop .# --no-pure-eval
# then within the shell:
objdump -T /nix/store/.../lib/libmsp430.so       # dynamic symbols
objdump -d /nix/store/.../lib/libmsp430.so       # full disassembly
nm -D /nix/store/.../lib/libmsp430.so            # dynamic symbol table
readelf -n /nix/store/.../lib/libmsp430.so       # build ID
readelf -r /nix/store/.../lib/libmsp430.so | grep libusb  # relocations
```

Located at `/nix/store/xa7y5gynaznhyfj0143f3i03bbzkv16g-msp-debug-stack-bin-3.15.1.1/lib/libmsp430.so`
(also in CCS Theia at `/nix/store/.../ccs/ccs_base/DebugServer/drivers/libmsp430.so` — same build ID)

### Build ID: `6368abda53a5e149e31ea0aa50e884627409adcd`

### Dynamic Dependencies (DT_NEEDED)
- `librt.so.1`
- `libpthread.so.0`
- `libstdc++.so.6`
- `libm.so.6`
- `libgcc_s.so.1`
- `libc.so.6`
- `ld-linux-x86-64.so.2`

**Notably absent: no libusb, no udev, no HIDAPI.** TI statically linked libusb-1.0 directly into this .so.

### Evidence of Static libusb
Strings present in the binary include every libusb function:
- `libusb_init`, `libusb_exit`
- `libusb_open_device_with_vid_pid`
- `libusb_claim_interface`, `libusb_release_interface`
- `libusb_bulk_transfer`, `libusb_interrupt_transfer`
- `libusb_control_transfer`
- `libusb_get_device_list`, `libusb_free_device_list`
- `LIBUSB_ERROR_*` definitions
- `hid_init` (HIDAPI also statically linked)
- `eventfd_select_interrupter` (libusb internal)

### USB VID/PID
Debug probes are identified by `VID:0x2047 PID:0x` — the PID string is truncated/constructed at runtime (likely built from a probe-type table). 0x2047 is Texas Instruments' USB vendor ID.

### Transport: Interrupt Transfers Only (not Bulk)

Despite `libusb_bulk_transfer` being compiled into the binary, it has **zero call sites** in the TI C++ code — it is dead code. The same is true for `libusb_control_transfer` (no TI callers; only called internally by libusb itself). All three call sites of `libusb_interrupt_transfer` are inside libusb's own internal paths:

| Call Address | Inside Function | Context |
|---|---|---|
| `0xf1052` | libusb interrupt_transfer async path | completion/retry from read() |
| `0xf2242` | libusb event handling | processing completed URB |
| `0xf2bc1` | libusb error recovery | ioctl(USBDEVFS_SUBMITURB) retry |

This means the XDS110v3 debug probe uses **interrupt endpoints exclusively** for all communication, including EnergyTrace data. The existing pcap confirms this — all observed traffic was URB_INTERRUPT with the `0x3e <type> 0d 01` framing. No BULK endpoint 0x81 traffic exists.

## Exported EnergyTrace API Functions

| Function | Address | Purpose |
|---|---|---|
| `MSP430_EnableEnergyTrace` | 0x60b86 | Start EnergyTrace capture |
| `MSP430_DisableEnergyTrace` | 0x60bb9 | Stop EnergyTrace capture |
| `MSP430_ResetEnergyTrace` | 0x60be3 | Reset/counters zero |

All are `T` (text/code) symbols — exported C-linkage functions.

## Related USB API Functions

| Function | Address | Purpose |
|---|---|---|
| `MSP430_Initialize` | 0x6018a | Open USB connection ("TIUSB") |
| `MSP430_Close` | 0x60287 | Close connection |
| `MSP430_VCC` | 0x60d54 | Set target voltage |
| `MSP430_OpenDevice` | 0x60c61 | Open target device |
| `MSP430_GetFoundDevice` | 0x6025e | Get device capabilities |
| `MSP430_Run` | 0x61217 | Run target (FREE_RUN) |
| `MSP430_GetInterface_Type` | 0x60c0d | Query debug interface type |
| `MSP430_RegisterMessageCallback` | 0x60c37 | Register callback |
| `MSP430_SetTargetArchitecture` | 0x60169 | Set arch type |
| `MSP430_LoadDeviceDb` | 0x60103 | Load device database |
| `MSP430_Configure` | 0x60ced | Configure debug session |

## Existing pcap Analysis

We have a USB capture at `pcap1.pcapng` taken from an LP-MSPM0L1306 LaunchPad's XDS110 debug probe.

### Finding: This capture contains NO EnergyTrace data

Analysis shows only **URB_INTERRUPT** traffic — the XDS110 probe's idle-periodic heartbeat / enumeration packets. These are the background messages the probe emits once connected, regardless of whether EnergyTrace is active.

### Packet structure (interrupt endpoint)

```
3e <type> 0d 01 <payload...>
```

Common format observed:
- First byte `0x3e` = length prefix (62 decimal)
- Second byte = message type:
  - `0x39`: repeating ~every 3s (probe heartbeat)
  - `0x2d`: frequent data (device status / debug info)
  - `0x2f`: device state info  
  - `0x22`: shorter messages
- `0d 01` = constant TI debug protocol marker (NOT an EnergyTrace eventID)

### Why this is NOT EnergyTrace

- EnergyTrace records would have `eventID` as first byte of record (`0x01` through `0x09`)
- Event ID=8 records would be 18 bytes total; these packets are 30-50+ bytes
- The `0x3e` length + `0d01` pattern matches TI's lower-level debug probe framing protocol (used for JTAG/SBW communication, status, etc.)
- No EnergyTrace setup was active when this capture was taken — the probe was simply enumerated

### What needs to be captured instead

A capture taken while `energytrace-util` or CCS Theia has EnergyTrace enabled and running. Expected traffic:
- USB **INTERRUPT** transfers on the probe's interrupt endpoint
- The EnergyTrace records will be embedded inside the TI framing: `[0x3e][len][0x0d 0x01][ET records...]`
- Each ET record: `[08][7B timestamp][4B current_nA][2B voltage_mV][4B energy_0.1uJ]`
- The framing suggests the probe's internal MCU interleaves JTAG/SBW debug messages with EnergyTrace data on the same interrupt pipe

### How to Capture

```bash
# Capture all USB traffic to/from the TI probe
sudo usbmon -i usbN > etrace_capture.pcap
# or using usbmon kernel module directly:
sudo cat /sys/kernel/debug/usb/usbmon/0t > etrace_capture.txt
```

Then run the energytrace-util tool while capturing:
```bash
sudo cat /sys/kernel/debug/usb/usbmon/0t > etrace_capture.txt &
energytrace-util 10 > measurement.log
sudo kill %1
```

## XDS110v3 / CMSIS-DAP v2 Probe Analysis

### Background: libmsp430.so is IRRELEVANT for this probe

The earlier analysis of libmsp430.so (Build ID: `6368abda53a5e149e31ea0aa50e884627409adcd`) applies only to **older XDS110 models** that use VID `0x2047`. The MSPM0L1306 LaunchPad we have (`LP-MSPM0L1306`) uses a newer XDS110v3 probe with:

- **VID:PID**: `0451:bef3` (Texas Instruments, not `0x2047`)
- **Firmware**: `03.00.00.22`
- **Protocol**: CMSIS-DAP v2
- **Library**: `libicdi_emu.so` (NOT libmsp430.so)

libmsp430.so searches for `VID 0x2047` — this probe uses `VID 0x0451`, so libmsp430.so cannot even find it. **libicdi_emu.so** (in CCS Theia at `DebugServer/drivers/`) is the actual XDS110v3 driver.

### Full USB Interface Layout (0451:bef3)

```
Bus XXX Device YYY: ID 0451:bef3 Texas Instruments
Device Descriptor:
  bcdUSB            2.00
  idVendor          0x0451
  idProduct         0xbef3
  bcdDevice         03.00.00.22
  
  Interface Descriptor 0:
    bInterfaceClass      2 (Communications)
    bInterfaceSubClass   2 (Abstract Modem)
    bInterfaceProtocol   0
    iInterface           "XDS110"
    Endpoint(s): 0x81 IN (INT), 0x01 OUT (INT)  -- CDC ACM Control
    * This is the COMM port for target UART
  
  Interface Descriptor 1:
    bInterfaceClass      10 (CDC Data)
    bInterfaceSubClass   0
    bInterfaceProtocol   0
    iInterface           "XDS110"
    Endpoint(s): 0x82 IN (BULK), 0x02 OUT (BULK)  -- CDC ACM Data
    * UART serial data from target MCU
  
  Interface Descriptor 2:
    bInterfaceClass      255 (Vendor-specific)
    bInterfaceSubClass   0
    bInterfaceProtocol   0
    iInterface           "XDS110"
    Endpoint(s): 0x83 IN (BULK), 0x02 OUT (BULK)  -- CMSIS-DAP v2
    * Speaks CMSIS-DAP v2 protocol (confirmed via DAP_Info)
  
  Interface Descriptor 3:
    bInterfaceClass      10 (CDC Data)
    bInterfaceSubClass   0
    bInterfaceProtocol   2
    iInterface           "XDS110"
    Endpoint(s): 0x84 IN (BULK), 0x04 OUT (BULK)  -- CDC Data (UART2?)
  
  Interface Descriptor 4:
    bInterfaceClass      2 (Communications)
    bInterfaceSubClass   2 (Abstract Modem)
    bInterfaceProtocol   0
    iInterface           "XDS110"
    Endpoint(s): 0x84 IN (INT), 0x03 OUT (INT)  -- CDC ACM Control
  
  Interface Descriptor 5:
    bInterfaceClass      3 (HID)
    bInterfaceSubClass   0
    bInterfaceProtocol   0
    iInterface           "XDS110"
    Endpoint(s): 0x86 IN (INT), 0x04 OUT (INT)  -- CMSIS-DAP v1 (HID)
    * Non-responsive with this firmware (CMSIS-DAP v1 HID transport not active)
  
  Interface Descriptor 6:
    bInterfaceClass      255 (Vendor-specific)
    bInterfaceSubClass   0
    bInterfaceProtocol   0
    iInterface           "XDS110"
    Endpoint(s): 0x87 IN (BULK), 0x05 OUT (BULK)
    * TI-specific vendor interface (purpose unknown, possibly custom debug transport)
```

Note: Endpoint `0x81` is shared between Iface 0 (INT) and Iface 3 (the second CDC Data iface).
Note: Endpoint `0x02` is shared as OUT endpoint for both Iface 1 and Iface 2.

### CMSIS-DAP v2 Interface (Iface 2) — Confirmed

Interface 2 responds to standard CMSIS-DAP v2 protocol over bulk endpoints `0x02 OUT / 0x83 IN`:

| DAP_Info ID | Response |
|---|---|
| 0x00 (Vendor) | `Texas Instruments` |
| 0x01 (Product) | `Texas Instruments` |
| 0x02 (Serial) | `XDS110 with CMSIS-DAP` |
| 0x03 (FW version) | `ML130001` |
| 0x04 (Device status) | non-zero (connected) |
| 0x06 (Packet size) | varies |
| 0x07 (Capabilities) | valid response |
| 0x0f-0x12 | valid responses |
| 0x7f (Reset) | valid response |

**Key finding**: DAP_Info(0x03) reports FW as `ML130001` — this is the CMSIS-DAP firmware identifier, NOT the XDS110 main firmware version (`03.00.00.22`). CMSIS-DAP version: `1.2.0`.

### What Does NOT Work

1. **TI framing (`0x3e <type> 0d 01`)**: The old framing format from XDS110 (pre-v3) does NOT work on this probe. Sent on iface 2 and iface 6 — no valid response.
2. **CMSIS-DAP vendor extensions (0x80-0x8F)**: Sending CMSIS-DAP vendor commands (DAP_Vendor) with IDs 0x80-0x8F on iface 2 returns no response. Either the commands are wrong, need different payloads, or EnergyTrace isn't accessed through CMSIS-DAP vendor extensions on this firmware.
3. **HID interface (Iface 5)**: CMSIS-DAP v1 HID transport — all interrupt transfers time out. Not active on this firmware.
4. **Endpoint 0x81 (pcap traffic)**: Previously assumed to be debug protocol, but actually carries CDC ACM UART serial data from the target MCU. Not JTAG/SBW or EnergyTrace data.

### Interface Probing Results

| Interface | Type | Protocol | Responds? | Notes |
|---|---|---|---|---|
| 0 | CDC ACM Control | INT | N/A | Serial port control |
| 1 | CDC ACM Data | BULK | N/A | UART data from target |
| 2 | Vendor-specific | BULK | **YES** | CMSIS-DAP v2 — DAP_Info, DAP_Connect work |
| 3-4 | CDC | INT/BULK | N/A | Second UART port |
| 5 | HID | INT | No | CMSIS-DAP v1 — all transfers timeout |
| 6 | Vendor-specific | BULK | Partial | Responds to CMSIS-DAP commands with zeros; TI framing echoes back |

### EnergyTrace Data Stream Format (XDS110v3 / 0451:bef3)

**This is different from the libmsp430.so eventID=8 record format.** The XDS110v3 probe on interface 6 streams raw 4-byte samples, not framed event records:

```
4-byte sample layout:
  byte[0] = 0x70 (constant — indicator byte)
  byte[1] = low byte of 16-bit DC/DC converter pulse counter
  byte[2] = high byte of 16-bit counter (byte[2]<<8 | byte[1] = counter16)
  byte[3] = digital flags (0 in analog profiling mode)
```

The first 8 bytes of each bulk_read from endpoint 0x87 are a timestamp header; actual 4-byte samples follow.

### Current Conversion (verified 2026-05-07 against busy-loop ± LED loads)

**Sample byte interpretation (CORRECTED — see below for what was wrong before):**

```
4-byte sample layout:
  byte[0] = 0x70 (frame marker, constant)
  byte[1] = cumulative 10 kHz time-window counter (mod 256). NOT a current value.
  byte[2] = number of DC/DC charge pulses delivered since the last record.
  byte[3] = digital flags (0 in pure-analog mode)
```

The probe self-decimates: at high current it emits ~1 record per 100 µs window
with `byte[2]` ∈ {1, 2, 3, ...}; at low current it groups multiple windows into
a single record (`byte[1]` jumps by N) and `byte[2]` is the pulse count over
those N windows. Summing `byte[2]` across every record gives total pulses
regardless of decimation, which is what the current calculation needs.

**Conversion formula:**

```
pulse_total_per_sec = sum(byte[2]) / measurement_seconds
current_nA          = pulse_total_per_sec * 1_000_000 / cal2
current_µA          = current_nA / 1_000           ← divide by 1000, not 1e6
```

For `cal2 = 10186` (placeholder; see calibration TODOs below).

**The 8-byte timestamp header is ONLY on the FIRST URB after ET_Start.**
Subsequent URBs start with the first sample at byte 0. The previous code
stripped 8 bytes from every URB, which corrupted alignment.

**Verified readings (LP-MSPM0L1306 + busy-loop firmware, cal2=10186):**

| Firmware                       | Pulses/sample | Pulses/sec | Estimated current |
|---|---|---|---|
| `et_calib_busy_loop`           | 1.00          | 2,498      | 245 µA            |
| `et_calib_busy_loop_led`       | 1.27          | 11,434     | 1,122 µA          |
| Δ (LED contribution)           |               | 8,936      | ~877 µA           |

The 245 µA for active busy-loop matches the MSPM0L1306 RUN-mode current at
the post-reset 4 MHz MCLK. The 877 µA LED delta is plausible for the
LP-MSPM0L1306 LED1 series resistor (1 kΩ–3.3 kΩ).

### Earlier-agent claim corrected

Commit `c4c970c` claimed "verified ~1.1 µA idle". That was actually 1.1 **mA**;
the print path divided by 1e6 to convert nA → µA when it should have divided
by 1e3. The calibration math itself is approximately correct.

### Calibration TODOs

- `ET_Calibrate (cmd=0x1e)` with `tickCount=0` gets no response. The TI flow
  loops over `EnergyTrace_LPRF::_CalibLoads`, calling `ET_Calibrate` with each
  load's `tickCount`. Valid `tickCount` values are not yet known — they're
  populated from a configuration source we have not located. Until that's
  resolved, `cal2 = 10186` is a placeholder pulled from REVERSE_ENGINEERING
  history; absolute readings are within order-of-magnitude but not calibrated.
- The full TI flow uses TWO calibration lines `(slope, offset)` per
  `_calibLine` and a `ProcessAnalogSamples` algorithm to convert raw samples
  to energy pulses. The simple "sum byte[2]" path works for analog profiling
  but does not match TI's full path.
- `ET_DCDC_SetVcc(3300)` + `ET_DCDC_RestartMCU` were added based on the
  disassembly of `EmulatorComm_XDS110::DCDCSetVcc/DCDCRestart`. They both
  return status=0 but did not change the sample stream pattern, so the
  default DCDC state on this XDS110v3 firmware appears to already supply
  3.3 V to the target (matches SLAU869E's "fixed 3.3 V" claim).

**Original GetCurrentInNA from libenergytracestandalone.so (0x3ed5e):**
```c
double GetCurrentInNA(int64_t offset, int index) {
    return (double)offset * 1000.0 * 1000.0 / calib_loads[index];
}
```
Where `calib_loads[0]` is `cal2` (the primary calibration load in µA).

### ProcessAnalogSamples Algorithm

At `0x44fe2` in libenergytracestandalone.so, the `ProcessAnalogSamples` function implements a different conversion path:

1. Extract 24-bit signed sample: `(byte[2]<<16) | (byte[1]<<8) | byte[0]`, subtract 0x1000000 if > 0x7FFFFF
2. Maintain a `double` accumulator at `this+0x318`
3. For each sample: `accumulator += (sample - calibLine.offset)`
4. When `accumulator > 0.0`: produce energy pulse count via `(accumulator * calibLine.slope) as i32`, then reset accumulator to 0

This produces energy pulses (for energy/µJ calculation), not instantaneous current. The 16-bit delta method above is a simpler approach that gives instantaneous current directly.

| Interface | Type | Protocol | Responds? | Notes |
|---|---|---|---|---|
| 0 | CDC ACM Control | INT | N/A | Serial port control |
| 1 | CDC ACM Data | BULK | N/A | UART data from target |
| 2 | Vendor-specific | BULK | **YES** | CMSIS-DAP v2 — DAP_Info, DAP_Connect work |
| 3-4 | CDC | INT/BULK | N/A | Second UART port |
| 5 | HID | INT | No | CMSIS-DAP v1 — all transfers timeout |
| 6 | Vendor-specific | BULK | Partial | Responds to CMSIS-DAP commands with zeros; TI framing echoes back |

### pcap Re-interpretation

The captured pcap (`pcap1.pcapng`) shows traffic on endpoint `0x81` with repeating ~40-byte packets. This is **NOT EnergyTrace data** — it is UART serial data from the target MCU, forwarded through the CDC ACM endpoint. The `0d01` prefix bytes are carriage-return/line-feed or data headers from the target's serial output, not a debug protocol marker.

A valid EnergyTrace capture would need to be taken while EnergyTrace is actively streaming, with the probe in measurement mode.

### EnergyTrace Protocol: ICDI Transport (Reverse-Engineered)

**This is now decoded.** See the full protocol description below in the "Actual XDS110v3 EnergyTrace Protocol" section. Summary:

The protocol uses a TI-specific ICDI transport on **interface 2** (the same interface as CMSIS-DAP v2, same bulk endpoints 0x83 IN / 0x02 OUT). It is NOT CMSIS-DAP. The protocol uses a simple framing: sync byte 0x2a, length, command byte, payload. Commands are sent via `libusb_bulk_transfer`. This is a direct bulk-transfer protocol that bypasses CMSIS-DAP entirely.

The sequence is:
1. Open USB device (VID 0x0451, PID matching table)
2. Claim interface 2
3. Send `XDS_ConnectET` (cmd=0x28) — initializes the EnergyTrace subsystem
4. Send `ET_Setup` (cmd=0x1d) — configure sampling parameters
5. Send `ET_Start` (cmd=0x1f) — begin data streaming
6. Poll `ReadDataPort` repeatedly for measurement records
7. Send `ET_Stop` (cmd=0x20) to stop
8. Send `ET_Cleanup` via `ET_Cleanup` command

### Repro-CLI Tool

The `repro-cli` Rust binary (at `tools/energytrace/repro-cli/`) uses `rusb` for direct libusb access to the probe. Build and run:

```bash
nix develop .# --no-pure-eval
cd tools/energytrace/repro-cli
cargo run                            # measures with current firmware on target
RAW_OUT=path/to/file.bin cargo run   # also dumps raw URB stream
RANGE=1 cargo run                    # send ET_Setup_Range (no observable effect yet)
```

### Operational pitfalls

- **probe-rs (CMSIS-DAP) and our ICDI session can't be back-to-back.** Once
  probe-rs (e.g., `cargo run` flashing) talks to the probe on iface 2 in
  CMSIS-DAP mode, ICDI commands time out until the probe is **physically
  unplugged and replugged**. Test cycle: replug → flash → replug → measure.
- **Do NOT call `libusb_reset_device()` on this XDS110v3 firmware.** It
  removes the probe from the USB bus permanently until physical replug.
- **A failed ICDI command (e.g., a timeout) stalls iface 2 OUT for the rest
  of the session.** Replug to recover.
- **Command ordering matters: `XDS_ConnectET (cmd=0x28)` MUST be the first
  ICDI command sent.** This mirrors `XDS_Open` in `libjscxds110.so`, which
  always sends Connect or ConnectET immediately after claiming interfaces.

### Newly-decoded ICDI command bytes (from libjscxds110.so disassembly)

| cmd  | name                   | cmd_len | resp_len | payload                                         |
|---|---|---|---|---|
| 0x01 | XDS_Connect            | 4       | 7        | (none)                                          |
| 0x1d | ET_Setup               | 0xb     | 7        | mode(1) + dig_mode(1) + sample_rate(4 LE) + dig_enable(1) |
| 0x1e | ET_Calibrate           | 6       | 0xf      | tickCount(2 LE); response: status(4) + cal1(4) + cal2(4) |
| 0x1f | ET_Start               | 4       | 7        | (none)                                          |
| 0x20 | ET_Stop                | 4       | 7        | (none)                                          |
| 0x21 | ET_Cleanup             | 4       | 7        | (none)                                          |
| 0x23 | ET_DCDC_PowerDownMCU   | 4       | 7        | (none)                                          |
| 0x24 | ET_DCDC_SetVcc         | 6       | 7        | vcc_mv(2 LE)                                    |
| 0x25 | ET_DCDC_RestartMCU     | 4       | 7        | (none)                                          |
| 0x28 | XDS_ConnectET          | 4       | 7        | (none) — must be first command                  |
| 0x30 | ET_Setup_Range         | 5       | 7        | range(1)                                        |
| 0x31 | ET_Setup_Dig           | -       | -        | (not yet decoded)                               |
| 0x46 | ET_HardwareInfo        | -       | -        | (not yet decoded)                               |

### libicdi_emu.so — The Real XDS110v3 Driver

Located in CCS Theia at `DebugServer/drivers/libicdi_emu.so`. This is the driver for:
- XDS110v3 probes (VID 0x0451)
- CMSIS-DAP v2 protocol probes
- Likely contains the actual EnergyTrace enable/disable/read implementation

Multiple copies exist in nix store (from different CCS Theia builds). Reference path (newest):
`/nix/store/8v0s9vd6mwnk096lk7r2aywmgd9z1d3v-ccs-theia-unwrapped-1.5.1.00003/ccs/ccs_base/DebugServer/drivers/libicdi_emu.so`

### libFlashMSPM0.so — MSPM0 Flash Programming Library

Located alongside libicdi_emu.so at `DebugServer/bin/libFlashMSPM0.so`. Contains MSPM0-specific operations including possibly EnergyTrace access. Reference path:
`/nix/store/8v0s9vd6mwnk096lk7r2aywmgd9z1d3v-ccs-theia-unwrapped-1.5.1.00003/ccs/ccs_base/DebugServer/bin/libFlashMSPM0.so`

## Actual XDS110v3 EnergyTrace Protocol (from libjscxds110.so)

### Architecture

The EnergyTrace protocol is NOT CMSIS-DAP v2. It uses a **TI-specific ICDI (In-Circuit Debug Interface) protocol** that predates CMSIS-DAP. The XDS110v3 probe exposes both:
- **Interface 2**: CMSIS-DAP v2 (for standard debug operations — flash, halt, step, run)
- **Interface 2 (same bulk endpoints 0x02/0x83)**: ICDI custom protocol for EnergyTrace

The ICDI transport uses **libusb bulk_transfer** on endpoints read from device configuration, NOT the CMSIS-DAP protocol at all for EnergyTrace operations.

### Library Locations (Nix store paths)

These paths are inside the `ccs-theia-unwrapped` package. To get the out path:
```bash
nix eval /home/larry/projects/claude/mspm0sleep#ccs-theia.outPath
```
But the actual libraries live in `ccs-theia-unwrapped` (the unwrapped package, not the wrapped one with desktop integration):
```
/nix/store/8v0s9vd6mwnk096lk7r2aywmgd9z1d3v-ccs-theia-unwrapped-1.5.1.00003
```

Key library paths under that prefix:
- `ccs/ccs_base/emulation/analysis/bin/libenergytracestandalone.so` — EnergyTrace data processing (converts raw pulse counts to engineering units)
- `ccs/ccs_base/common/uscif/libjscxds110.so` — ICDI transport layer (USB bulk I/O via Clibusb)
- `ccs/ccs_base/DebugServer/drivers/libicdi_emu.so` — XDS110v3 device driver

### Library Stack

```
energytrace-util / CCS
    ↓
libenergytracestandalone.so (BuildID 04fcadfa6...)
    └── EmulatorComm_XDS110 class
        ├── SetupEnergyTrace() → calls ET_Setup@PLT
        ├── StartCollection()  → calls ET_Start@PLT
        ├── StopCollection()   → calls ET_Stop@PLT
        ├── ReadDataPort()     → calls XDS_ReadDataPort@PLT
        └── CleanupEnergyTrace() → calls ET_Cleanup@PLT
    ↓
libjscxds110.so (provides all ET_* and XDS_* functions)
    ├── ET_Setup(), ET_Start(), ET_Stop(), ET_Cleanup()
    ├── XDS_Open(), XDS_Close(), XDS_Connect(), XDS_ConnectET()
    ├── XDS_Execute(), XDS_Execute_NoLog()
    ├── USBSendPacket(), USBGetPacket()
    ├── _WriteICDIUSBPacket(), _ReadICDIUSBPacket()
    │   └── Clibusb::bulk_transfer(libusb_device_handle, endpoint, buf, len, &transferred, timeout_ms)
    │       └── dlopen("libusb-1.0.so") → dlsym → libusb_bulk_transfer
    └── Clibusb class
        ├── init(), exit(), open(), close()
        ├── claim_interface(), release_interface()
        ├── get_device_list(), get_device_descriptor()
        ├── bulk_transfer(), control_transfer()
        └── clear_halt()
```

### Packet Format (ICDI Transport Buffer)

The `XDS_Execute_NoLog` function uses a **shared buffer structure** to build commands, send them via bulk write, and receive responses via bulk read:

```
Buffer offset  Field        Size  Description
─────────────  ───────      ────  ───────────
[0x00]         channel      4     Must be non-zero (set to 1, cleared on close)
[0x08]         device_handle 8    libusb_device_handle (pointer from libusb_open)
[0x10]         sync_byte    1     Always 0x2a (42 decimal) — frame sync marker
[0x11]         length_lo    1     Low byte of payload length (total_len - 3)
[0x12]         length_hi    1     High byte of payload length
[0x13]         command      1     Command ID (see table below)
[0x14]         ep_in        1     Bulk IN endpoint address (e.g., 0x83)
[0x15]         ep_out       1     Bulk OUT endpoint address (e.g., 0x02)
[0x16]         ...          var   Command-specific payload (up to 0x1100 - 0x16 bytes)
...
[0x1110]       rx_sync      1     Response sync byte (must be 0x2a on response)
[0x1111]       rx_len_lo    1     Response payload length low byte
[0x1112]       rx_len_hi    1     Response payload length high byte
[0x1113]       rx_status    4     Response status code
[0x1117]       rx_payload   var   Response payload
```

**Max packet size**: 0x1100 (4352) bytes total. The buffer is allocated via `calloc(1, 0x2218)`.

### XDS Protocol Commands

Command bytes are written at `buf[0x13]` and sent via `USBSendPacket` which calls `_WriteICDIUSBPacket`:

| Command | Name | Function | Payload Layout |
|---------|------|----------|----------------|
| 0x00 | XDS_Echo | Echo test | buf[0x14] = data_len; copies data to buf[0x18] |
| 0x0c | XDS_SendRecvScan | JTAG/SWD scan | buf[0x14..0x19] = scan params |
| 0x1d | ET_Setup | Configure EnergyTrace | buf[0x14]=mode, buf[0x16:0x1a]=sample_rate, buf[0x15]=dig_mode, buf[0x1a]=dig_enable |
| 0x1f | ET_Start | Start EnergyTrace collection | (no payload) |
| 0x20 | ET_Stop | Stop EnergyTrace collection | (no payload) |
| 0x28 | XDS_ConnectET | Connect EnergyTrace subsystem | (no payload) |
| 0x29 | XDS_RegisterRead | Read ICDI register | buf[0x14] = register address (4 bytes LE) |
| 0x2a | (frame header) | Frame sync byte | Written at buf[0x10] before any command |

### ET_Setup Parameters

```c++
// Based on disassembly of libjscxds110.so:ET_Setup at 0x45eb8
int ET_Setup(void* ctx, unsigned char mode, unsigned int sample_rate_hz,
             unsigned char dig_mode, unsigned char dig_enable) {
    // ctx points to the ICDI buffer (0x2218 bytes)
    buf = *(void**)ctx;  // The actual buffer
    
    uint8_t* mode_field   = buf + 0x14;  // offset into buf for mode
    uint8_t* dig_mode_f   = buf + 0x15;  // digital mode
    uint32_t* rate_field  = buf + 0x16;  // sample rate (LE, 4 bytes at +0x16..0x19)
    uint8_t* dig_enable_f = buf + 0x1a;  // digital enable
    
    buf[0x13] = 0x1d;                    // command = ET_Setup
    
    *mode_field   = mode;                // e.g. 0 = analog profiling
    *dig_mode_f   = dig_mode;
    *rate_field   = sample_rate_hz;
    *dig_enable_f = dig_enable;
    
    // XDS_Execute(ctx, cmd_len=11, resp_len=7, retries=2, timeout_ms=4000)
    return XDS_Execute(ctx, 0x0b, 0x07, 0x02, 0xfa0);
}
```

### ET_Start / ET_Stop / XDS_ConnectET

These are very simple — just set `buf[0x13]` to the command byte and call `XDS_Execute`:

```c++
// ET_Start: buf[0x13] = 0x1f
XDS_Execute(ctx, 0x04, 0x07, 0x02, 0xfa0);

// ET_Stop: buf[0x13] = 0x20
XDS_Execute(ctx, 0x04, 0x07, 0x02, 0xfa0);

// XDS_ConnectET: buf[0x13] = 0x28
XDS_Execute(ctx, 0x04, 0x07, 0x02, 0xfa0);
```

### XDS_Execute (The Core Transport)

```c++
// Internal function in libjscxds110.so at 0x44825
int XDS_Execute(void* ctx, int cmd_len, int resp_len, int retries, int timeout_ms) {
    buf[0x10] = 0x2a;                     // sync byte
    buf[0x11] = (cmd_len - 3) & 0xFF;     // length low byte
    buf[0x12] = ((cmd_len - 3) >> 8) & 0xFF; // length high byte
    
    for (int attempt = 0; attempt < retries; attempt++) {
        USBSendPacket(ctx, cmd_len);
        if (USBGetPacket(ctx, resp_len, &actual_len, timeout_ms) == 0) {
            break;
        }
    }
    
    // Check response
    if (buf[0x1110] != 0x2a) return error;
    if (buf[0x1111..0x1112] != resp_len - 3) return error;
    status = *(uint32_t*)(buf + 0x1113);
    return status;
}
```

### XDS_Open Sequence

1. Create `Clibusb` wrapper (dlopen libusb-1.0.so, dlsym all functions)
2. Call `libusb_init()`
3. Call `libusb_get_device_list()`
4. Match device against VID/PID table (VID 0x0451, PIDs: 0x1cbe, 0x1cbe, 0x1cbe, 0xbef3, 0xbef4, 0x029e, 0x029f, 0x02a5)
5. Call `libusb_open()` on matching device
6. Call `_InitializeICDIDeviceBySerial()` → libusb init, device list, find device by VID/PID, open
7. Call `XDS_Connect()` or `XDS_ConnectET()` depending on whether EnergyTrace mode is requested
8. Store device handle at `buf[0x08]` and endpoints at `buf[0x14]`/`buf[0x15]`

### EnergyTrace Data Read (Critical: NOT ICDI Framing!)

Data is read via `EmulatorComm_XDS110::ReadDataPort()` which calls `XDS_ReadDataPort()` in libjscxds110.so. **This function does NOT use `XDS_Execute` or ICDI framing** — it performs a raw `libusb_bulk_transfer` read from a **different endpoint than the command channel**.

```c++
// XDS_ReadDataPort at 0x47be7 in libjscxds110.so
// This is NOT an ICDI-framed command. It's a raw bulk read.
int XDS_ReadDataPort(void* ctx, unsigned char* buffer, unsigned int size, int* actual_len) {
    // ctx is the ICDI buffer (0x2218 bytes allocated via calloc)
    // RDI=ctx, RSI=buffer, EDX=size, RCX=actual_len

    // Read the ET data endpoint from ICDI buffer offset 0x1c
    // (NOT the same as the command response endpoint at buf[0x14])
    unsigned char ep = *(uint8_t*)(ctx + 0x1c);  // ep_in for ET data

    // libusb_device_handle from ICDI buffer offset 0x08
    libusb_device_handle* dev = *(void**)(ctx + 0x08);

    // Clibusb global singleton (dlopen'd libusb-1.0.so)
    // vtable[0x98] = libusb_bulk_transfer function pointer
    int result = Clibusb::bulk_transfer(dev, ep, buffer, size, actual_len, 500);
    //                  0x1f4 = 500 ms timeout (stack-pushed)

    if (result != 0)
        return 0xfffffefa;  // error
    return 0;  // SC_ERR_NONE
}
```

Parameters (x86-64 ABI):
| Register | Parameter | Description |
|----------|-----------|-------------|
| `rdi` | `ctx` | ICDI buffer pointer (0x2218 bytes) — contains device handle at +0x08, endpoints |
| `rsi` | `buffer` | Output buffer for received raw EnergyTrace records |
| `edx` | `size` | Max bytes to read (e.g., `0x186a0` = 100000 bytes per ReadDataPort call) |
| `rcx` | `actual_len` | Output: actual number of bytes transferred |
| stack | timeout | `0x1f4` = 500 ms (pushed onto stack) |

The result is **raw EnergyTrace records** directly in `buffer` — no ICDI framing wrapper.

### Dual-Endpoint / Dual-Interface Architecture

The XDS110v3 (0451:bef3) uses **TWO separate interfaces** and **TWO separate endpoints** for EnergyTrace:

| Purpose | Interface | Endpoint | Buffer Offset | API |
|---------|-----------|----------|---------------|-----|
| Commands (ET_Setup, ET_Start, ET_Stop) | Iface 2 (0x02) | OUT: 0x02, IN: 0x83 | `buf[0x14]` = ep_in (0x83), `buf[0x15]` = ep_out (0x02) | `XDS_Execute` → `USBSendPacket`/`USBGetPacket` → ICDI framing with 0x2a sync byte |
| Data stream (EnergyTrace records) | Iface 6 (0x06) | IN: 0x87 (no OUT needed) | `buf[0x1c]` = ep_in_data (0x87) | `XDS_ReadDataPort` → raw `libusb_bulk_transfer`, no framing |

Both interfaces are **claimed during initialization**:
```c++
libusb_claim_interface(dev, buf[0x10]);  // interface 2 (0x02)
libusb_claim_interface(dev, buf[0x18]);  // interface 6 (0x06)
```

The ICDI buffer stores both endpoint pairs. The **command channel** uses endpoints at `buf[0x14]`/`buf[0x15]` (interface 2, 0x83/0x02). The **data channel** for EnergyTrace records uses endpoints at `buf[0x1c]`/`buf[0x1d]` (interface 6, 0x87/0x05).

### ICDI Buffer Endpoint Layout by VID:PID

Data from `_InitializeICDIDeviceBySerial` at `0x34328`:

| VID:PID | Iface (0x10) | ep_in (0x14) | ep_out (0x15) | Iface2 (0x18) | ep_in_data (0x1c) | ep_out_data (0x1d) |
|---------|:---:|:---:|:---:|:---:|:---:|:---:|
| 0x0451:0xbef3 | 2 | 0x83 | 0x02 | **6** | **0x87** | 0x05 |
| 0x0451:0xbef4 | 2 | 0x83 | 0x02 | **6** | **0x87** | 0x05 |
| 0x1cbe:0x029e | 0 | 0x81 | 0x01 | **1** | **0x82** | 0x02 |
| 0x1cbe:0x029f | 0 | 0x81 | 0x01 | **1** | **0x82** | 0x02 |
| 0x1cbe:0x02a5 | 0 | 0x81 | 0x01 | **1** | **0x82** | 0x02 |

**Key finding**: For the XDS110v3 (0451:bef3), EnergyTrace data is read from endpoint **0x87** on **interface 6**, not from endpoint 0x83 on interface 2. Interface 6 is a vendor-specific bulk interface (discovered in USB descriptor probing but previously thought non-functional).

### EnergyTrace Polling Loop

The polling loop lives in `EnergyTrace_Collector::Run()` at `0x3d748` in libenergytracestandalone.so. The loop:

1. Calls `ReadDataPort(buffer, 0x186a0=100000, &actual_len)` via vtable dispatch
2. Writes received data to a `TraceBuffer` 
3. Repeats while a running flag is true
4. On exit, closes the TraceBuffer

The `ReadDataPort` vtable call goes through offset `+0x70` in the `IEmulatorComm` vtable (at `this->IEmulatorComm + 0x70`).

### Interface 6 for XDS110v3 (0451:bef3)

Interface 6 was previously probed and described as "Responds to CMSIS-DAP commands with zeros; TI framing echoes back." Now we understand its true purpose — it is the **EnergyTrace data streaming interface**, not a debug command interface. The ICDI library claims this interface and reads raw EnergyTrace records from its bulk IN endpoint (0x87).

The record format matches the one described in the original EnergyTrace analysis (eventID=8: 1B ID, 7B timestamp, 4B current_nA, 2B voltage_mV, 4B energy_0.1uJ).

## Approach

### Phase 1 — Decompile libicdi_emu.so for EnergyTrace Functions
**COMPLETED.** libicdi_emu.so (BuildID 1f62b2645ec04e9c11e3f840d60e959a161f4374) does NOT contain USB transport code. It is a StellarisDriver that communicates through a GTI protocol layer with dynamically-loaded plugins. The actual USB transport is in libjscxds110.so.

### Phase 2 — Decompile EnergyTrace Transport Library
**COMPLETED.** libjscxds110.so (at `common/uscif/`) provides all ET_* and XDS_* functions. It dynamically loads libusb-1.0.so through a `Clibusb` wrapper class and uses `libusb_bulk_transfer` on interface 2 endpoints (0x83 IN / 0x02 OUT). The protocol is a TI-specific ICDI framing with sync byte `0x2a`, command bytes (0x1d=Setup, 0x1f=Start, 0x20=Stop, 0x28=ConnectET), and bulk transfers.

### Phase 3 — Build Standalone CLI
**IN PROGRESS.** Implement the EnergyTrace protocol in `repro-cli/src/main.rs` using `rusb`:

The protocol requires TWO interfaces:
- **Interface 2** (endpoints 0x02 OUT / 0x83 IN) — for ICDI commands (ET_Setup, ET_Start, ET_Stop, XDS_ConnectET)
- **Interface 6** (endpoint 0x87 IN) — for EnergyTrace data streaming via raw bulk_read

Implementation steps:
1. Open device 0451:bef3, claim interface 2 AND interface 6
2. Send `XDS_ConnectET` (ICDI cmd=0x28 via ICDI framing: `[0x2a][len][cmd=0x28]`) on iface 2
3. Send `ET_Setup` (cmd=0x1d via ICDI framing) with analog profiling mode on iface 2
4. Send `ET_Start` (cmd=0x1f via ICDI framing) on iface 2 — begins streaming
5. Poll by doing **raw bulk_read** from endpoint 0x87 (interface 6) with 500ms timeout
6. Parse eventID=8 records (18 bytes each: 1B ID, 7B ts, 4B current_nA, 2B voltage_mV, 4B energy_0.1uJ)
7. Send `ET_Stop` (cmd=0x20 via ICDI framing) on completion

### Phase 4 — Capture & Validate
Use `usbmon` to capture live EnergyTrace traffic and validate against known record formats. Compare against `energytrace-util` output for accuracy.

## libmsp430.so EnergyTrace API (from MSP430_EnergyTrace.h)

### Data Delivery Model: Push Callback

EnergyTrace does NOT have a "read current" function. Data is delivered via a **push callback** provided by the caller:

```c
typedef void (*PushDataFn)(void* pContext, const uint8_t* pBuffer, uint32_t nBufferSize);
```

The callback is registered as part of the `EnergyTraceCallbacks` struct passed to `MSP430_EnableEnergyTrace`:

```c
typedef struct EnergyTraceCallbacks_tag {
    void* pContext;              // User context, passed to callbacks
    PushDataFn pPushDataFn;      // Called with raw measurement records
    ErrorOccurredFn pErrorOccurredFn;  // Called on errors
} EnergyTraceCallbacks;
```

### Record Format

Each callback invocation delivers one or more records. Each record has an **8-byte header** followed by a **payload** whose layout depends on `eventID`:

```
[1B eventID][7B timestamp (microseconds)]
```

The `eventID` field (enum `EnergyTraceEventID`) determines the payload:

| eventID | Name | Payload Fields | Total Record Size |
|---|---|---|---|
| 1 | `ET_EVENT_CURR` | 32b current (nA) | 12 B |
| 2 | `ET_EVENT_VOLT` | 16b voltage (mV) | 10 B |
| 3 | `ET_EVENT_CURR_VOLT` | 32b current (nA), 16b voltage (mV) | 14 B |
| 4 | `ET_EVENT_STATE` | 64b device state | 16 B |
| 5 | `ET_EVENT_STATE_CURR` | 64b state, 32b current (nA) | 20 B |
| 6 | `ET_EVENT_STATE_VOLT` | 64b state, 16b voltage (mV) | 18 B |
| 7 | `ET_EVENT_STATE_VOLT_CURR` | 64b state, 32b current (nA), 16b voltage (mV), 32b energy (0.1 µJ) | 22 B |
| **8** | **`ET_EVENT_CURR_VOLT_ENERGY`** | **32b current (nA), 16b voltage (mV), 32b energy (0.1 µJ) = 10 B payload** | **18 B** |
| 9 | `ET_EVENT_ALL` | 64b state, 32b current (nA), 16b voltage (mV) | 20 B |

### Default Configuration

For analog-only measurement (no device state, simplest case for reverse engineering):

```c
EnergyTraceSetup setup = {
    .ETMode        = ET_PROFILING_ANALOG,     // eventID=8 records
    .ETFreq        = ET_PROFILING_10K,         // (N/A for analog mode)
    .ETFormat      = ET_ALL,                   // (N/A)
    .ETSampleWindow = ET_EVENT_WINDOW_100,     // (N/A for profiling)
    .ETCallback    = ET_CALLBACKS_CONTINUOUS   // stream continuously
};
```

This yields eventID=8 records (the format `energyTrace-util` expects):

```
Offset  Size  Field
0       1     eventID = 0x08
1       7     timestamp (microseconds, little-endian)
8       4     current (nanoamps, little-endian)
12      2     voltage (millivolts, little-endian)
14      4     energy (0.1 microjoule units, little-endian)
```

### VTable Dispatch

All three EnergyTrace API functions are thin wrappers that call through a vtable pointer stored at global `0x408b08` (class `TI::DLL430::DebugManagerMSP430`):

| API Function | Address | VTable Offset |
|---|---|---|
| `MSP430_EnableEnergyTrace` | 0x60b86 | 0x2e8 |
| `MSP430_DisableEnergyTrace` | 0x60bb9 | 0x2f0 |
| `MSP430_ResetEnergyTrace` | 0x60be3 | 0x2f8 |

The vtable object is obtained via `DebugManagerMSP430` singleton pattern. The actual USB I/O happens inside class methods reached through these vtable slots.

## energtrace-util API Sequence (Annotated)

The source code lives at `tools/energytrace/energytrace.c` in this repo, or the nix store at the source path above. The full call sequence is:

### Include Dependencies

```c
#include <MSP430.h>               // Base API: Initialize, OpenDevice, VCC, Run, Close, etc.
#include <MSP430_EnergyTrace.h>   // EnergyTrace API: Enable, Disable, Reset + structs
#include <MSP430_Debug.h>         // Debugging functions (MSP430_Run, etc.)
```

All headers are from the msp-debug-stack-bin include directory: `/nix/store/xa7y5.../include/`

### Packet Format (callback event_t struct)

```c
typedef struct __attribute__((packed)) {
    uint8_t id;              // eventID: 8 = ET_EVENT_CURR_VOLT_ENERGY
    uint64_t timestamp:56;   // microseconds
    uint32_t current;        // nanoamps (nA)
    uint16_t voltage;        // millivolts (mV)
    uint32_t energy;         // 0.1 microjoules (100 nJ units)
} event_t;                   // total: 18 bytes
```

### API Call Sequence

```
1. MSP430_Initialize("TIUSB", &version)
   └─► 0x6018a:
       └─► strncmp(port, "USB", 3)  -- match "TIUSB" / "USB" / others
       └─► vtable[0x40](singleton)  -- USB open/init
           └─► libusb_init()
           └─► libusb_open_device_with_vid_pid(0x2047, pid)
           └─► libusb_claim_interface(handle, iface)
   Stores singleton pointer at global 0x408b08

2. MSP430_VCC(3300)                     // mV
   └─► 0x60d54:
       └─► vtable[?](singleton, 3300)  -- USB control message to set target VCC

3. MSP430_LoadDeviceDb(NULL)            // optional, needed for newer tilib

4. MSP430_OpenDevice("DEVICE_UNKNOWN", "", 0, 0, DEVICE_UNKNOWN)
   └─► 0x60c61:
       └─► vtable[0x10]()     -- create device object
       └─► vtable[0x58](dev)  -- open/identify target
           └─► interrupt_transfer(ep, buf, len, &actual, timeout)  -- JTAG/SBW ID

5. MSP430_GetFoundDevice(&device, sizeof(device.buffer))
   └─► reads device properties from internal database

6. MSP430_Run(FREE_RUN, 1)
   └─► 0x61217:
       └─► vtable[...](FREE_RUN)  -- release target from JTAG, let it execute

7. EnergyTraceSetup ets = {
       .ETMode        = ET_PROFILING_ANALOG,           // eventID=8 records
       .ETFreq        = ET_PROFILING_1K,               // N/A for analog
       .ETFormat      = ET_ALL,                        // N/A
       .ETSampleWindow = ET_EVENT_WINDOW_100,          // N/A
       .ETCallback    = ET_CALLBACKS_ONLY_DURING_RUN   // stream while running
   };

   MSP430_EnableEnergyTrace(&ets, &cbs, &ha)
   └─► 0x60b86:
       └─► vtable[0x2e8](singleton, &ets, &cbs, &ha)
           └─► (TI C++ method) sends EnergyTrace configuration
               └─► interrupt_transfer(ep, config_cmd, len, ...)
           └─► starts internal polling thread / async callback pump

8. MSP430_ResetEnergyTrace(ha)
   └─► 0x60be3:
       └─► vtable[0x2f8](handle)
           └─► resets internal sample counters
           └─► interrupt_transfer(ep, reset_cmd, len, ...)  -- USB reset to ET MCU

9. sleep(duration)                    // measurement period
   ┌─── async context (polling thread) ───────────────────────────┐
   │  While measuring, the library's async thread:                 │
   │  libusb_interrupt_transfer(ep_in_irq, buf, buf_size, ...)    │
   │     returns chunked measurement records in framing format     │
   │     framing: [0x3e][type][0x0d 0x01][payload...]              │
   │                                                               │
   │  Library parses records:                                      │
   │  [1B eventID=8][7B timestamp][4B current][2B voltage][4B energy]│
   │                                                               │
   │  Calls push_cb() with batches of event_t structs              │
   └───────────────────────────────────────────────────────────────┘

10. MSP430_DisableEnergyTrace(ha)
    └─► 0x60bb9:
        └─► vtable[0x2f0](handle)
            └─► stops measurement thread
            └─► interrupt_transfer(ep, stop_cmd, len, ...)  -- USB stop to ET MCU

11. MSP430_Close(0)
    └─► 0x60287:
        └─► vtable[...](FALSE)  -- release USB interface
            └─► libusb_release_interface()
            └─► libusb_close()
            └─► libusb_exit()
```

### VTable Dispatch Architecture

All three EnergyTrace API functions are thin wrappers that call through a vtable pointer stored at global `0x408b08` (class `TI::DLL430::DebugManagerMSP430`):

| API Function | Address | VTable Offset |
|---|---|---|
| `MSP430_EnableEnergyTrace` | 0x60b86 | 0x2e8 |
| `MSP430_DisableEnergyTrace` | 0x60bb9 | 0x2f0 |
| `MSP430_ResetEnergyTrace` | 0x60be3 | 0x2f8 |

The vtable object is obtained via `DebugManagerMSP430` singleton pattern. The actual USB I/O happens inside class methods reached through these vtable slots.

### USB Control Transfer Usage

The binary has a PLT entry for `libusb_control_transfer` at relocation offset `0x3f7508`, but disassembly shows **no TI C++ code calls it directly**. Control transfers are needed for USB enumeration (set configuration, claim interface, etc.) but these happen inside libusb's internal implementation which is statically linked. TI's code uses the higher-level `libusb_interrupt_transfer` synchronous API for all probe communication after initialization.

All three libusb bulk data transfer functions exist in the binary but only `libusb_interrupt_transfer` has active call sites. The XDS110v3 probe exposes interrupt endpoints (not bulk endpoints), so all JTAG/SBW debug communication and EnergyTrace data flows through interrupt transfers. This is consistent with USB capture data showing only URB_INTERRUPT packets with the `3e <type> 0d01` framing. No bulk endpoint traffic exists.
