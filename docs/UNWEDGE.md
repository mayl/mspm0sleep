# The "wedged" LP-MSPM0L1306: what it was and how to get out of it

Investigated 2026-11-11 (bead mspm0sleep-8g0). Tooling: `tools/unwedge/`.

## Summary

The board was not stuck in a sleep state of our making. **Main flash was
completely blank**, so the boot ROM ran the bootstrap loader (BSL), which gives
up after ~10 s without a host and puts the chip in STANDBY. In STANDBY the
AHB-AP (the access port debuggers read memory through) is power-gated, so a
generic SWD tool gets a valid debug port and then a FAULT on its first memory
access. probe-rs 0.32 (nixpkgs) uses its generic ARM sequence for MSPM0 and
cannot get past that; probe-rs master has an MSPM0 sequence that can.

Any firmware that deep-sleeps and rarely wakes produces the same symptom, so
the recovery below is needed routinely with low-power firmware, not just once.

## Evidence

| Observation | Value |
|---|---|
| DPIDR | `0x6ba02477`, debug + system power-up both ACKed |
| AP0 (AHB-AP) IDR read | FAULT, STICKYERR set (`0x84770001` once awake) |
| AP1 / AP2 / AP4 IDR | `0x002e0001` / `0x002e0000` / `0x002e0002` (CFG-AP / SEC-AP / PWR-AP); AP3 `0x002e0003` |
| PWR-AP `DPREC0` (AP4, 0x00) while wedged | `0x00a00003` |
| ...after writing bit 3 | `0x00a8002f`, AHB-AP answers |
| Core state once reachable | PC `0x01001edc` (boot ROM), VTOR `0x20000000`, `SCB.SCR`=4 (SLEEPDEEP), `PMODECFG`=1 (STANDBY) |
| Main flash, all 64 KiB | `0xFFFFFFFF` |
| NONMAIN `0x41C00000` | factory defaults (`00000001 aabbaabb aabbaabb ...`), nothing locked |

Timeline after an NRST pulse on the blank chip, polled read-only:

    t= 0.4s .. 9.2s   DPREC0=0x00080027   AHB-AP reachable
    t=10.2s onwards   DPREC0=0x00a00003   AHB-AP FAULT

That 10 s edge is the BSL inactivity timeout. (TI's install does not document
the timeout; this is our measurement.)

Why flash was blank is not known. The last recorded action in June was
flashing `sleep_floor`; an erase that was not followed by a successful program
would leave exactly this state.

## PWR-AP `DPREC0` bits

From disassembly of CCS's `libprsc.so` (details and addresses in
[CCS_MSPM0_LOWPOWER.md](CCS_MSPM0_LOWPOWER.md)), cross-checked against what the
chip did:

| Bit | Meaning | Checked on hardware? |
|---|---|---|
| 3 | FORCEACTIVE (control): keep the device out of low power | yes: alone sufficient to revive the AHB-AP |
| 2, 5 | status: clock on, powered | yes: set when awake (`..27`/`..2f`), clear when asleep (`..03`) |
| 21 (with 1:0) | sticky "power loss detected" | yes: appears at the moment the AHB-AP disappears |
| 13 | debug enable | no (not needed for recovery) |
| 14-17 | wait-in-reset / block reset / halt-on-reset | no |
| 19, 20 | not used by CCS; probe-rs master calls them DEBUGPOWER / INHIBITSLEEP | written alone they did **not** revive the AHB-AP |

CCS's connect sequence is the same thing: clear sticky bits, set bit 13, set
bit 3, poll until `(DPREC0 & 0x2c) == 0x2c`, then attach the AHB-AP. On
disconnect it clears bit 3 so the chip can sleep again.

## Recovery recipe

    tools/unwedge/unwedge.py status     # read-only; says whether the AHB-AP is gated
    tools/unwedge/unwedge.py wake       # PWR-AP FORCEACTIVE; survives the tool exiting
    probe-rs download --chip MSPM0L1306 --protocol swd <elf>
    tools/unwedge/unwedge.py release    # un-halt, clear FORCEACTIVE + power-up request, NRST

`release` matters: FORCEACTIVE and a pending debug power-up request keep the
chip awake, which would invalidate any current measurement taken afterwards.
`tools/measure.sh` wraps the whole flash / release / capture sequence.

`unwedge.py` is a ~150-line CMSIS-DAP v1 client over hidraw (stdlib only), so
it needs the XDS110 hidraw node to be writable (udev rule for `0451:bef3`).

Other ways in, in order of escalation:

1. **Attach inside the window after reset.** The ROM BSL stays awake 10 s;
   every firmware in `firmware/` spins for 3 s at boot (`attach_window()`) for
   the same reason. `unwedge.py reset` pulses NRST from the probe.
2. **PWR-AP FORCEACTIVE** (above). Works from any STOP/STANDBY state seen so far.
3. **DSSM mailbox via SEC-AP** (mass erase `0x020C`, factory reset `0x020A`),
   which is what CCS offers when connect fails. Sequence is in
   CCS_MSPM0_LOWPOWER.md, taken from TI's GEL script. **Not exercised here**;
   it was not needed, and factory reset rewrites NONMAIN.

## Hub power control (uhubctl)

The LaunchPad sits on port 3 of a VIA VL817 hub. The hub is USB3, so it
enumerates twice: `2109:2817` on bus 1 (USB2 half, `1-2.3`) and `2109:0817` on
bus 2 (USB3 half, `2-2.3`). VBUS is only removed when the port is off on
**both** halves. `uhubctl -l 1-2.3 -p 3 -a off` pairs them automatically, but
only if it can open both hub devices.

This bit us on 2026-11-11: the udev rules granted plugdev access to `2817`
only. `uhubctl` then listed two hubs instead of four, the probe dropped off the
bus on "off", and the port status read `0000 off`, yet the board stayed
powered. Proof was a marker written to target RAM (and the debug halt state)
surviving 15 s "off". After widening both hub rules to
`ATTR{idProduct}=="2817|0817"` and `udevadm trigger --subsystem-match=usb
--action=change`, the same test loses the marker and the board LEDs go dark.

Do not trust the port status line as evidence of a power cycle; if in doubt
repeat the RAM-marker test. A quick check is that `uhubctl` lists four hubs.

Re-enumeration alone (even without a real power cut) clears a stalled XDS110
ICDI interface; `tools/unwedge/replug.sh` does a full off/on.
