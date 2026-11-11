# EnergyTrace reverse-engineering: verification against CCS Theia 1.5.1

Static analysis only. No USB hardware was touched. Nothing here was tested on the probe. The items later checked on the hardware are listed in the last section, "Hardware follow-up".

Install analysed: `/nix/store/r9i9vskavbcw0qlpaa5i4pk6lckh7477-ccs-theia-unwrapped-1.5.1.00003/ccs`

| Library (under `ccs_base/`) | Build ID | Matches doc? |
|---|---|---|
| `common/uscif/libjscxds110.so` | `0491178dd5dbdae794746df29a690e1afaa2f801` | addresses line up (`ET_Setup` 0x45eb8, `XDS_Execute` 0x44825, `XDS_ReadDataPort` 0x47be7, `XDS_EEPROMRead` 0x47504) |
| `emulation/analysis/bin/libenergytracestandalone.so` | `04fcadfa615f1b9490fb3d28a15b1d946ff9d609` | same build ID as quoted in the doc |
| `emulation/analysis/bin/libenergytraceccs.so` | `3ff393fa5b6e23f65351e45cd606910b0953b275` | IDE-side twin; same classes and constants (spot-checked) |
| `DebugServer/drivers/libicdi_emu.so` | `1f62b2645ec04e9c11e3f840d60e959a161f4374` | same build ID as quoted in the doc |
| `common/uscif/libxdsboard.so` | `bd6390b25929f7fba88642b1fdb290dafdd15226` | not analysed further |

Method: `objdump -d -C` of the libraries, plus Ghidra 12.0.4 headless decompilation of about 40 functions in `libenergytracestandalone.so`. Ghidra adds 0x100000 to addresses; all addresses below are the raw ELF ones. TI's record grammar was then re-implemented in a throwaway Python script and run over the saved captures in `captures/*.bin`.

Wording used below:
- **read** means I read the disassembly or decompilation of that function.
- **capture** means it was checked against bytes in `captures/*.bin`.
- **inferred** means a conclusion I did not see directly in code.

## The headline

The doc analysed the wrong TI class. `IEnergyTraceManager::EnergyTraceManagerCreator` (0x58182) tries `EnergyTrace_LPRF` first, and falls back to `EnergyTrace_MSP432` when the LPRF constructor throws. The two classes are different technologies:

| Class | TI name (from `stune` help text) | Accepted XDS110 hardware versions | Decoder |
|---|---|---|---|
| `EnergyTrace_LPRF` | "ETHDR" (XDS110 standalone probe with EnergyTrace HDR add-on) | 0x21, 0x23 | `DecoderStateMachineLPRF`, 24-bit samples, `ProcessAnalogSamples` |
| `EnergyTrace_MSP432` | "ET-MSP" (DC/DC pulse counting) | 0x21, 0x28, 0x2d | `DecoderStateMachineMSP432`, pulse records |

The stream in our captures (`34 10 27 5c 70 ...`) parses cleanly only with the `DecoderStateMachineMSP432` grammar. The LPRF decoder has no record type for the `0x34` or `0x5c` header bytes and would report bad data. So every conclusion the doc drew from `EnergyTrace_LPRF::PerformCalibration` and `EnergyTrace_LPRF::GetCurrentInNA` applies to different hardware.

In the MSP432 class, TI's current is a two-point linear fit against a known 2200 Ω calibration load. It is not `pulse_rate * 1e6 / cal2`. With the calibration numbers recorded in `REVERSE_ENGINEERING.md`, TI's scale is about 460 nA per (pulse/s), where repro-cli uses 98.2. **repro-cli readings are low by a factor of about 4.69**, if this probe's hardware version is 0x28 or 0x2d (not verified, see "Could not determine").

## Summary table

| # | Claim in `REVERSE_ENGINEERING.md` / `main.rs` | Verdict | Evidence |
|---|---|---|---|
| 1a | Record is 4 bytes starting with constant marker 0x70 | PARTLY | Byte 0 is a bit-field header (type, subtype, 2 low bits). 0x70 is one of nine record kinds. `DecoderStateMachineMSP432::Run` 0x4e88c, state 0 at 0x4e918 (read, capture) |
| 1b | byte1 = 10 kHz window counter mod 256 | CONFIRMED | State 2 at 0x4ede5 stores it at `this+0x52`; state 11 uses `this+0x54 + byte` as the tick count, tick period = 1000/sampfreq ms (read) |
| 1c | byte2 = pulse count since last record | PARTLY | It is the low byte of a 16-bit little-endian pulse count. `movzwl 0x48(this)` at 0x4f5b9 (read) |
| 1d | byte3 = digital flags | WRONG | It is the high byte of the pulse count (stored at `this+0x49` by state 4, 0x4ef1f) (read) |
| 1e | Lone 0x5c injected at each counter wrap, breaking alignment | PARTLY | 0x5c is a one-byte "Heartbeat" record (type 2). It adds 256 to the tick base. State 1 at 0x4f4a0 (read, capture) |
| 1f | 8-byte timestamp header on the first URB | WRONG | It is a 3-byte config record `34 10 27` (sample rate 10000), a heartbeat, and a first pulse record with a junk count. TI discards the first 10 pulse records after a config record. State 14 at 0x5063a (read, capture) |
| 2a | `current_nA = pulse_rate * 1e6 / cal2` | WRONG | Two-point fit, see section 2. `EnergyTrace_MSP432::PerformCalibration` 0x49c4a, decoder state 11 at 0x4f58e (read) |
| 2b | cal2 is a per-unit scale constant | WRONG | cal2 is the calibration duration in sample ticks (logged by TI as `timeTicks`). `SetCalibrationValues` 0x4e364 (read) |
| 2c | cal1 is a small settling offset | WRONG | cal1 is the pulse count during calibration with the selected load (logged as `pwmTicks`) (read) |
| 2d | `ET_Calibrate` argument is a tick count / integration length | WRONG | It is the calibration-load selector from `_CalibLoads[i]` at +8 (read). What each value switches in is firmware behaviour (inferred from the doc's sweep) |
| 2e | `_CalibLoads` is built empty and filled from `ccBoard%d.dat` or the probe EEPROM | WRONG | Hardcoded in both constructors from `.rodata`: 0x3e335 (LPRF), 0x49426 (MSP432) (read) |
| 2f | `GetCurrentInNA` divides by cal2 | WRONG | Returns `mV * 1000 * 1000 / R_ohm`, the expected current of the reference resistor. 0x4a0ca, 0x3ed5e (read) |
| 2g | Two-segment `_calibLine` fit | PARTLY | N points give N-1 segments. Hardware 0x28/0x2d has 2 points, so one line. Hardware 0x21 has 3 points, two lines (read) |
| 2h | `ProcessAnalogSamples` does not apply to our stream | CONFIRMED | It belongs to the LPRF/ETHDR decoder only (0x44fe2) (read) |
| 3a | Framing `[2a][len u16][cmd][payload]`, response `[2a][len u16][status u32]...` | CONFIRMED | `XDS_Execute_NoLog` 0x44676 (read) |
| 3b | Command numbers 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x23, 0x24, 0x25, 0x28, 0x30, 0x3f, 0x46 | CONFIRMED | Table in section 3 (read) |
| 3c | `ET_Setup` payload `mode(1) x(1) rate(4) y(1)` | CONFIRMED (layout) | 0x45eb8. The names "dig_mode" and "dig_enable" are wrong, see section 3 (read) |
| 3d | `ET_Calibrate` response `status, cal1 u32, cal2 u32`, 15 bytes | CONFIRMED | 0x460d0 (read) |
| 3e | `ET_HardwareInfo` 0x46, response u32 + u8 | CONFIRMED | 0x463dc, response length 0xc (read) |
| 3f | `XDS_EEPROMRead` 0x3f, `addr u16, len u16`, len ≤ 0x10f9 | CONFIRMED | 0x47504 (read) |
| 3g | Data read: raw bulk read on the endpoint at ctx+0x1c, 500 ms timeout, 100000-byte buffer | CONFIRMED | `XDS_ReadDataPort` 0x47be7, `EnergyTrace_Collector::Run` 0x3d748 (read) |
| 4a | Order is ConnectET, SetVcc, RestartMCU, Calibrate, Setup, Start | WRONG | TI: Setup_Range(0), Setup_Dig(0), Setup, then Calibrate, then Start. No DCDC commands. Section 4 (read) |
| 4b | DCDC SetVcc + RestartMCU "mirror PerformCalibration" | WRONG | Neither EnergyTrace library calls them on the measurement path. Section 4 (read) |
| 4c | Leave `sample_rate = 10000` | PARTLY | TI's standalone default is 2000 (`stune` help: "Default is 2K"). 10000 is accepted (read; rate effects on firmware not determined) |
| 4d | Leave `ET_Setup_Range` unsent | WRONG vs TI | The MSP432 path always sends `ET_Setup_Range(0)` then `ET_Setup_Dig(0)`. `PerformSetup` 0x49b5e (read) |
| 5 | Things TI does that we do not | see section 5 | discard of 10 records, no-load baseline, adaptive averaging window, 100 nA rounding, overcurrent and pause records, `ET_Cleanup` |

## 1. Record grammar on interface 6 / ep 0x87

Parser: `DecoderStateMachineMSP432::Run()` at 0x4e88c in `libenergytracestandalone.so`. It is a table-driven state machine. State 0 reads the header byte, picks a list of follow-on states from `.rodata` (0x70800 to 0x70910), and each state consumes bytes.

Header decode (0x4e948 to 0x4e983):

```
type = (b >> 5) & 7      ; sar $5 ; and $7
sub  = (b >> 2) & 7      ; sar $2 ; and $7
low  =  b & 3
```

Record kinds (all read from the state-0 dispatch and the state tables):

| Header | Example | Bytes after header | Meaning | Handler |
|---|---|---|---|---|
| type 1, low 0 | `0x34` | 2: sample rate u16 LE | Stream start. Logs "Config samp freq". Resets tick base, time and energy, and arms a discard of the next 10 pulse records | state 14, 0x5063a |
| type 2, low 0 | `0x5c` | 0 | "Heartbeat". Tick base += 256, time += 256 ticks | state 1, 0x4f4a0 |
| type 3, sub 4, low 0 | `0x70` | 3: tick u8, pulses u16 LE | Pulse record | states 2,3,4 (or 2,8) then 11 at 0x4f58e |
| type 3, sub 5, low 0 | `0x74` | 2: tick u8, pulses u8 | Pulse record, 8-bit count | states 2,3 then 11 |
| type 3, sub 2, low 1 | `0x69` | 5: tick u8, 4 data bytes | Digital state, low 32 bits (ET+ only) | states 2..6 |
| type 3, sub 1, low 1 | `0x65` | 6: tick u8, PC u32, state u8 | PC + power-state sample (ET+ only) | state 13, 0x5024d |
| type 7, sub 3, low 1 | `0xed` | 4 | Digital state, high 32 bits (ET+ only) | state 21, 0x50406 |
| type 4, low 0 | `0x80` | 0 | "OVERCURRENT!!!" → `OverCurrentDetected(limit)` | state 15, 0x5073f |
| type 5 | `0xa0` | 1: tick u8 | "Paused". Re-bases the previous tick, clears accumulators | state 22, 0x50890 |
| anything else | | | "BAD DATA" → `BadDataPacketDetected()` | 0x509f9 |

Pulse record handling, state 11 (excerpt, 0x4f595):

```
4f595: mov 0x54(this),%edx        ; tick base (heartbeats * 256)
4f59f: movzbl 0x52(this),%eax     ; tick byte
4f5a6: add %edx,%eax              ; -> tick count
4f5b5: add $0x48,%rax
4f5b9: movzwl (%rax),%eax         ; pulses, 16-bit
```

Check against captures: every file in `captures/` parses with this grammar. Across them the only headers seen are `0x34` (once per stream), `0x5c` and `0x70`. Every heartbeat coincides with a wrap of the tick byte, and no wrap occurs without one. The high pulse byte is zero in every record except the first one after `0x34`. Two files (`lp_idle_upstream.bin`, `rom_bsl_standby.bin`) contain a short damaged spot (a truncated record) which TI would flag as bad data.

What repro-cli gets wrong here:

- **First record.** The record straight after `34 10 27 5c` carries a junk 16-bit count (for example `70 00 58 a2` = 41560). TI ignores it because it discards 10 records. repro-cli skips the first 8 bytes of URB #1 instead, which only works when the stream starts at byte 0. In `busy_upstream.log` URB #1 began with a stale record (`70 b8 01 00 34 10 27 5c 70 00 ea 6c`), so the skip landed in the wrong place and `70 00 ea 6c` was counted as 234 pulses.
- **Heartbeats.** repro-cli reconstructs time from the tick byte modulo 256 and treats a delta of 0 as 256. If more than 256 ticks pass between records, time is lost. In the captures so far the largest gap is 30 ticks, so this has not bitten yet. It will at genuinely low currents.
- **byte 3.** Ignored. Harmless in the captures so far, wrong in principle.

## 2. Conversion maths

### What TI computes (ET-MSP path)

Calibration loads, hardcoded in `EnergyTrace_MSP432::EnergyTrace_MSP432` (0x49426 to 0x4950d). Each entry is `{double R_ohm; u16 selector}`:

| Hardware version (`XDS_Version`) | Entries pushed, in order |
|---|---|
| 0x2d or 0x28 | `{2200.0, 1}`, `{DBL_MAX (open), 0}` |
| 0x21 | `{2200.0, 4}`, `{82.0, 1}`, `{DBL_MAX, 0}` |

`.rodata` constants: 0x70798 = 2200.0, 0x707a0 = DBL_MAX, 0x707a8 = 82.0, 0x707b0 = 1000.0, 0x707b8 = -280.0.

For reference, the LPRF/ETHDR table (0x3e335, not our path): `{DBL_MAX, 0}`, `{2200.0, 4}`, then `{82.0, 1}` or `{36.865, 2}` depending on range. Constants 0x6e640 = 2200.0, 0x6e648 = 82.0, 0x6e650 = 36.865, 0x6e658 = 4000000.0, 0x6e660 = 20000000.0.

`EnergyTrace_MSP432::PerformCalibration` (0x49c4a), in C-like form:

```
for each load i:
    ET_Calibrate(load[i].selector, &pwmTicks, &timeTicks)     // cal1, cal2
    t_s = timeTicks / samplingFreq
    x   = pwmTicks / (1000.0 * t_s)                           // pulses per ms
    y   = GetCurrentInNA(i)
    insert {x, y, pwmTicks, timeTicks} into calibPoints, sorted by ascending y
for j = 1 .. n-1:
    slope = (pt[j].y - pt[j-1].y) / (pt[j].x - pt[j-1].x)
    calibLines.push({slope, offset = pt[j-1].x})
    energyPerTick.push( (mV/1000) * (pt[j].y / (pt[j].x - pt[0].x) / 1000) / 1000 )
```

`EnergyTrace_MSP432::GetCurrentInNA` (0x4a0ca):

```
R == DBL_MAX:  hw 0x21 ? mV * -280.0 / 1000.0 : 0.0
otherwise:     mV * 1000.0 * 1000.0 / R          // 3300 mV, 2200 ohm -> 1 500 000 nA
```

`mV` is fixed at 3300 (0xce4) for hardware 0x28 and 0x2d.

Per pulse record, decoder state 11 (0x4f58e to 0x50120):

```
dTicks = tick - prevTick
accPulses += pulses;  accTicks += dTicks
accMs = accTicks * (1000.0 / samplingFreq)
if firstAfterDiscard || (accPulses >= minPulses && accMs >= 1):
    x = accPulses / accMs                                    // pulses per ms
    pick segment i: advance while i < n-2 and x > pt[i+1].x
    I = (x - line[i].offset) * line[i].slope + pt[i].y
    I = I > 0 ? (uint)I : 0
    I = ((I + 50) / 100) * 100                               // round to 100 nA
    accPulses = accTicks = 0
// otherwise the previous I is reused for this record
energy += max(0, pulses - pt[0].x * dt_ms) * energyPerTick[i] * 10.0   // units of 0.1 uJ
emit (time_ms, 3300 mV, I nA, energy)
```

`minPulses = (uint)(pt[0].x * 1000)`, set at the end of `SetCalibrationValues` (0x4e6bb). That is the number of pulses the no-load point produces in one second.

The rounding is the `imul $0x51eb851f` / `shr $5` / `imul $0x64` sequence at 0x4fac1.

For hardware 0x28/0x2d this reduces to one line through the no-load point:

```
I_nA = (x - x0) * 1 500 000 / (x1 - x0)
x  = pulses per ms in the stream
x1 = pulses per ms with the 2200 ohm load (ET_Calibrate selector 1)
x0 = pulses per ms with no load          (ET_Calibrate selector 0)
```

### How far repro-cli is from it

Using the sweep recorded in `REVERSE_ENGINEERING.md` (selector 1 → cal1 3336, cal2 10185; selectors ≥ 10 → cal1 17, cal2 10186) and 10 kHz ticks:

- x1 = 3336 / (1000 × 1.0185) = 3.2754 pulses/ms
- x0 ≈ 17 / (1000 × 1.0186) = 0.0167 pulses/ms. This uses the "selector ≥ 10" result as a stand-in for selector 0, which is an assumption.
- TI slope = 1 500 000 / (3.2754 − 0.0167) = 460 304 nA per (pulse/ms) = **460.3 nA per (pulse/s)**
- repro-cli slope = 1e6 / 10186 = **98.2 nA per (pulse/s)**
- Ratio = **4.69**

The offset also differs. repro-cli subtracts `cal1 * 1e6 / cal2` ≈ 1.7 µA. TI's equivalent is `x0 * slope` ≈ 7.7 µA.

The doc's sweep supports the "load selector" reading (inferred, not from code): after subtracting 17, selectors 1, 2 and 4 give 3319, 2230 and 1086 pulses, in the ratio 1 : 0.672 : 0.327. That is what 2.2 kΩ, 3.3 kΩ and 6.8 kΩ would give. Selector 1 = 2.2 kΩ matches TI's table for hardware 0x28/0x2d.

Captures re-decoded both ways (files as they were when I read them; another session was rewriting some):

| Capture | Pulses/s | repro-cli model | TI model |
|---|---|---|---|
| `sleep_floor.bin` | 385 | 36 µA | 169 µA |
| `lp_idle_upstream.bin` | 855 | 82 µA | 386 µA |
| `lp_blinky_upstream.bin` | 1567 | 152 µA | 714 µA |
| `blinky_calib.bin` | 7115 | 697 µA | 3.27 mA |
| `led_on_calib.bin` | 11520 | 1.13 mA | 5.29 mA |

One observation from the captures: the low-power ones have a floor of one pulse every 26 to 29 ticks, which is 153 to 169 µA on TI's scale. The floor differs between firmware builds, so it looks like a real load rather than a probe keep-alive. I did not determine what it is.

## 3. ICDI commands on interface 2

Framing, `XDS_Execute_NoLog` (0x44676), signature `(ctx, cmd_len, resp_len, retries, timeout_ms)`:

```
446f4: movb $0x2a,0x10(buf)             ; sync
446f8: len = cmd_len - 3 -> 0x11, 0x12  ; u16 LE
...   response must be exactly resp_len bytes,
      buf[0x1110] == 0x2a, u16 at 0x1111 == resp_len - 3,
      status = u32 at 0x1113; any mismatch returns 0xfffffefb (-261)
```

Bounds: `4 <= cmd_len <= 0x1100`, `7 <= resp_len <= 0x1100`. Every ET command uses retries 2, timeout 4000 ms.

ET and DCDC commands in `libjscxds110.so` (all read):

| Cmd | Function (address) | Payload after cmd byte | Response after status | cmd_len / resp_len | main.rs |
|---|---|---|---|---|---|
| 0x03 | `XDS_Version` (0x44a0c) | none | fw version u32, hardware version u16 | 4 / 13 | not sent |
| 0x1d | `ET_Setup` (0x45eb8) | u8 mode, u8 stream type, u32 rate, u8 option | none | 11 / 7 | same bytes |
| 0x1e | `ET_Calibrate` (0x460d0) | u16 load selector | u32 pulse ticks, u32 time ticks | 6 / 15 | same bytes, wrong meaning |
| 0x1f | `ET_Start` (0x4617b) | none | none | 4 / 7 | same |
| 0x20 | `ET_Stop` (0x461c7) | none | none | 4 / 7 | same |
| 0x21 | `ET_Cleanup` (0x46213) | none | none | 4 / 7 | not sent |
| 0x22 | `ET_DCDC_GetMCUVersion` (0x4625f) | none | u32 version | 4 / 11 | not sent |
| 0x23 | `ET_DCDC_PowerDownMCU` (0x462db) | none | none | 4 / 7 | not sent |
| 0x24 | `ET_DCDC_SetVcc` (0x46327) | u16 mV | none | 6 / 7 | same bytes |
| 0x25 | `ET_DCDC_RestartMCU` (0x46390) | none | none | 4 / 7 | same |
| 0x28 | `XDS_ConnectET` (0x464f3) | none | none | 4 / 7 | same |
| 0x30 | `ET_Setup_Range` (0x45f70) | u8 range | none | 5 / 7 | same bytes, sent only with `RANGE=` |
| 0x31 | `ET_Setup_Dig` (0x45fd7) | u8 digital mode | none | 5 / 7 | not sent |
| 0x44 | `ET_Setup_Dig_Params` (0x4603e) | u8 n, n bytes (TI sends n = 10) | none | n+5 / 7 | not sent |
| 0x46 | `ET_HardwareInfo` (0x463dc) | none | u32, u8 | 4 / 12 | same |
| 0x3f | `XDS_EEPROMRead` (0x47504) | u16 addr, u16 len (≤ 0x10f9) | len bytes | 8 / len+7 | same |

No command is encoded differently on the wire by main.rs. The differences are in meaning and in which commands are sent:

- `ET_Setup` byte 0 (mode): the MSP432 class passes 0, the LPRF class passes 1. So 0 selects the ET-MSP technology, not "analog profiling".
- `ET_Setup` byte 1: the stream type from the caller (0 = `et`, 2 = `et+`). main.rs calls it `dig_mode`.
- `ET_Setup` byte 6: 0 by default, 3 when a caller option is set (I did not identify which option). main.rs calls it `dig_enable`.
- main.rs does not check the response length or the length field. TI treats a mismatch as a failed command.

The same table for the non-ET commands (cmd byte, lengths) can be regenerated from the disassembly; `XDS_Connect` 0x01, `XDS_Disconnect` 0x02, `XDS_SetSupply` 0x32, `XDS_PowerIsolate` 0x39 and `XDS_EEPROMWrite` 0x40 are the ones near this work.

## 4. Setup sequence

TI's order for the ET-MSP path in `et` (analog only) mode. Sources: `energytrace::configure` 0x5b552, `EnergyTrace_MSP432` constructor 0x48982, `PerformSetup` 0x49b5e, `InitEnergyTrace` 0x49a80, `StartEnergyTrace` 0x4a1f2, `StopEnergyTrace` 0x4a898, `CleanupEnergyTrace` 0x4ac9c, `energytrace::stop` 0x5c65e (all read).

| Step | TI | main.rs today |
|---|---|---|
| Open | `XDS_Open` sends `XDS_ConnectET` when its ET flag is set (0x445a5) | DAP_Disconnect, then `XDS_ConnectET` |
| Version check | `XDS_Version`; hardware must be one of 0x21, 0x23, 0x28, 0x2b, 0x2d (bit mask 0x290a00000000 at 0x589c2) | none |
| Setup | `ET_Setup_Range(0)`, `ET_Setup_Dig(0)`, `ET_Setup(0, type, rate, option)` in the constructor | `ET_Setup` only, and after calibration |
| DCDC | nothing | `ET_DCDC_SetVcc(3300)`, `ET_DCDC_RestartMCU` |
| Calibrate | `ET_Calibrate(1)` then `ET_Calibrate(0)` on hardware 0x28/0x2d, after setup | `ET_Calibrate(1000)` once, before setup |
| Start | reader thread started first, then `ET_Start` | `ET_Start`, then read loop |
| Stop | `ET_Stop`, sleep 500 ms while the reader keeps draining, stop reader | `ET_Stop`, no drain |
| Cleanup | `ET_Cleanup` (0x21) | not sent |

Defaults: sample rate comes from the caller and must be non-zero. The `stune` command-line tool (`emulation/analysis/bin/stune/stune`) documents "Default is 2K". Range is always 0 on this path (the `-r lo|hi` option is "only applicable to XDS110 standalone probe with ETHDR add-on"). Voltage is fixed at 3300 mV for hardware 0x28/0x2d. Overcurrent limit handed to the decoder is 75 (100 for hardware 0x21). There are no delays other than the 500 ms after stop.

On the DCDC commands: `EmulatorComm_XDS110::Reset(1)` (0x3d20c) does `ET_DCDC_RestartMCU` and then `ET_DCDC_SetVcc(3300)`, the reverse of main.rs's order. But nothing in `libenergytracestandalone.so` or `libenergytraceccs.so` calls `Reset` or `Powerdown` through the vtable; I searched both for calls through those slots. So the claim in `REVERSE_ENGINEERING.md` that these "mirror `PerformCalibration`" is not supported by the code.

The sample rate matters for calibration because TI computes `t = timeTicks / samplingFreq`. TI has already sent `ET_Setup` at that point. main.rs calibrates before `ET_Setup`, so it relies on the probe's tick rate at that moment being 10 kHz.

## 5. What TI does that repro-cli does not

Ordered by likely effect on accuracy.

1. **Reference-load calibration.** The scale comes from a known 2200 Ω load, giving the factor of about 4.69 above.
2. **No-load baseline.** A second `ET_Calibrate` with no load gives x0, which is subtracted from both current and energy.
3. **Discard after stream start.** The first 10 pulse records after a config record are dropped (`this+0x2e9 = 10`, set at 0x50733 and in the constructor at 0x4e060). This removes the junk first record.
4. **Tick-based time.** Time comes from tick counts and heartbeats, never the host clock. repro-cli divides total pulses by host elapsed time for the mean.
5. **Adaptive averaging window.** Current is recomputed only after at least `x0 * 1000` pulses and at least 1 ms have accumulated. Between updates the previous value is held.
6. **Rounding.** Reported current is rounded to the nearest 100 nA and clamped at zero.
7. **Overcurrent and pause records.** `0x80` and `0xa0` are handled. repro-cli would skip their header as a resync byte, and for `0xa0` would then misread the tick byte that follows.
8. **Drain and cleanup.** 500 ms drain after `ET_Stop`, then `ET_Cleanup`.

Things I looked for and did not find in either library: temperature compensation, voltage compensation (voltage is the constant 3300 in the output), periodic re-calibration (`PerformCalibration` has one caller, `InitEnergyTrace`), or any filtering beyond the window in item 5.

A debugging aid in TI's code: if the environment variable `ET_LOG_FILE` is set, the decoder writes the calibration points ("Point ticks/ms, pwmTicks, timeTicks, currentNA"), the line slopes, and one CSV row per record ("Seq num,Pulse,Time Ticks,Delta pulse,...,Current(nA),Power(uW),Total Energy(0.1uJ)"). Constructor at 0x4e0c4. Running `stune` with that set would give ground truth to compare against.

## Corrections for repro-cli, in priority order

1. **Replace the conversion.** Calibrate twice, `ET_Calibrate(1)` and `ET_Calibrate(0)`. Compute `x = pulses * rate / (1000 * timeTicks)` for each, then `I_nA = (x_stream - x0) * 1 500 000 / (x1 - x0)`, clamped at zero. Delete the `cal1 * 1e6 / cal2` baseline and the "cal2 is nA per pulse/s" comments. Expect readings to rise by about 4.69×.
2. **Query `XDS_Version` (cmd 0x03, 13-byte response) and print the hardware version.** Use the table in section 2 to pick the loads. Stop with a clear message if it is not 0x28 or 0x2d, because the 2200 Ω assumption then needs rechecking.
3. **Reorder the flow to match TI.** ConnectET, Version, `ET_Setup_Range(0)`, `ET_Setup_Dig(0)`, `ET_Setup`, the two calibrations, start reading, `ET_Start`, capture, `ET_Stop`, drain 500 ms, `ET_Cleanup`.
4. **Take the DCDC commands out of the default flow.** Keep them behind an environment variable. TI does not send them, and a DCDC restart at the start of every measurement may disturb the target.
5. **Rewrite `EtDecoder` to TI's grammar.** Dispatch on the header bit-fields. Read pulses as u16. Add 256 ticks per heartbeat instead of inferring wraps. On a config record, reset state and drop the next 10 pulse records. Handle `0x80` and `0xa0`. Count anything else as bad data. Remove the "skip 8 bytes of URB #1" special case.
6. **Use tick time for the mean.** Divide total pulses by total ticks / rate, not by host elapsed time.
7. **Optional, for TI-like instantaneous values.** Implement the adaptive window (at least `x0 * 1000` pulses and 1 ms) and the 100 nA rounding, and TI's energy formula.
8. **Validate responses.** Check the response length and length field as `XDS_Execute_NoLog` does.
9. **Fix `REVERSE_ENGINEERING.md`.** The sections "ET_Calibrate WORKS", "EnergyTrace Calibration Architecture", "Is the calibration per-serial or per-board-type?", "Original GetCurrentInNA" and the "8-byte timestamp header" note are superseded by this file. The `ccBoard%d.dat` theory is wrong: `SMG_LoadBoardData` loads the probe connection config (`SEPK.POD_DRVR`, `SEPK.POD_SUPPLY`, `SEPK.POD_VOLTAGE`), not calibration data.

## Could not determine

- **This probe's hardware version.** `XDS_Version` was never run on it and I could not touch the hardware. Everything in section 2 for "our" probe assumes 0x28 or 0x2d. The selector sweep in the doc fits that table, which is circumstantial support only.
- **Whether `ET_Calibrate(0)` works on this firmware.** TI's path requires it. The doc says a selector of 0 timed out, but that was an early attempt and 0 is absent from the later sweep. The 4.69 factor does not depend on it much (x0 is small), the offset does.
- **What selectors 2, 4 and ≥ 10 switch in.** That is firmware behaviour. The firmware images are in the install (`common/uscif/xds110/firmware_3.0.0.32.bin`, `eZ_FetDcdcController.txt`) and were not disassembled.
- **Whether calibration time ticks follow the configured sample rate** or are always 10 kHz. Also firmware.
- **Effect of `ET_Setup_Range(0)` and `ET_Setup_Dig(0)` on the stream.** The doc's own experiment saw the record rate change when Range was sent. If these are added, calibration and capture must both be done after them, as TI does.
- **What `ET_DCDC_RestartMCU` does to target power.**
- **Which caller option sets `ET_Setup` byte 6 to 3.**
- **JavaScript or Java conversion maths.** I found none. `theia/resources/app.asar` contains only UI plumbing strings (`EnergyTraceEnabled`, `EnergyTraceInProgress`, `trace.trace.EnergyTrace.duration`). The IDE path uses `libenergytraceccs.so`, which has the same classes; I spot-checked the load table, the discard count and the rounding there but did not read it in full.
- **The link from `SMG_open` mode 8 to `XDS_ConnectET`.** `GetProbeHandle` (0x58348) passes mode 8 for `et`; that this ends up as the ET flag of `XDS_Open` is inferred.
- **The doc's claim that `XDS_EEPROMRead` has no callers.** Not rechecked.
- **Physical cause of the 153 to 169 µA floor in the low-power captures.**


## Hardware follow-up (2026-11-11, after this analysis)

Run on the LP-MSPM0L1306's on-board XDS110 with `TI_CAL=1 repro-cli` and the new default flow.

| Open item above | Result on hardware |
|---|---|
| Probe hardware version | `XDS_Version` returns firmware `0x03000022`, hardware `0x0028`. The 2200 ohm table applies. |
| Does `ET_Calibrate(0)` work? | Yes, when sent in TI's order (ConnectET, Version, Setup_Range(0), Setup_Dig(0), Setup). Selector 1: 3112 pulses / 10187 ticks. Selector 0: 17 pulses / 10186 ticks. |
| Scale | 493.2 to 493.7 nA per pulse/s across five consecutive calibrations; open-load offset 15.7 to 16.7 pulses/s. (The 460 figure above was derived from older sweep numbers.) |
| Effect of `Setup_Range(0)` / `Setup_Dig(0)` on the stream | None seen: the idle floor is 350 pulses/s in both the old and the TI order. |
| `ET_Calibrate` timeouts in the old flow | With the old order (DCDC SetVcc + RestartMCU first, argument 1000) every call timed out and stalled interface 2, for awake and sleeping targets alike. In TI's order it has not failed. Which of the two differences is responsible was not isolated. |

Implemented in repro-cli from the corrections list: 1 (two-point conversion), 2 (version check),
3 (order, except the 500 ms drain and `ET_Cleanup`), 4 (DCDC commands only under `LEGACY_FLOW=1`),
and the parts of 5 that affect totals (u16 pulse count, config record, 10-record discard, no URB #1
skip). Not done: full header bit-field dispatch with `0x80` / `0xa0` records and bad-data
counting, tick-count from heartbeats, 6, 7, 8.
