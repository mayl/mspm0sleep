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

## Hub power control: only half works

`uhubctl -l 1-2.3 -p 3 -a off` drops the probe off the bus but does **not**
remove power from the board: a marker written to target RAM, and the debug halt
state, survived 15 s "off". The hub is USB3, so it enumerates twice
(`2109:2817` on bus 1, `2109:0817` on bus 2) and VBUS is only cut when the port
is off on both. The udev rules grant plugdev access to `2817` only, so
`uhubctl` cannot see the USB3 half. Fix: match `0817` in both hub rules.

Re-enumerating the USB2 half is still useful: it clears a stalled XDS110 ICDI
interface (`tools/unwedge/replug.sh`).
