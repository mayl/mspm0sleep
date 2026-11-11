#!/usr/bin/env python3
"""Recover SWD access to an MSPM0 that is asleep, and hand it back cleanly.

  unwedge.py status          read-only: debug port, vendor APs, sleep state
  unwedge.py wake            force the chip active via the PWR-AP so that the
                             AHB-AP (memory access) answers; generic tools such
                             as probe-rs 0.32 can then attach and flash
  unwedge.py dump            wake + halt, print PC / sleep config / flash state
  unwedge.py release         undo wake: resume core, drop FORCEACTIVE and the
                             debug power-up request (required before any
                             current measurement), then pulse NRST
  unwedge.py reset           pulse NRST only
  unwedge.py peek <elf>      edge_test readout: counters and GPIO/SYSCTL state,
                             WITHOUT halting or resetting (forces active only
                             for the read, then lets go)
  unwedge.py cfg <elf> <w>   write the edge_test config word and reset
  unwedge.py watch [secs]    after an NRST pulse, poll how long the AHB-AP
                             stays reachable (read-only apart from the reset)

See docs/UNWEDGE.md for what this is working around.
"""
import sys, time
from dap import Dap, Fault, DapError

PWR_AP = 4                    # IDR 0x002e0002
DPREC0 = 0x00
FORCEACTIVE = 1 << 3
SYSCTL = 0x400AF000
DHCSR, DCRSR, DCRDR = 0xE000EDF0, 0xE000EDF4, 0xE000EDF8


def ap0_alive(d):
    try:
        return d.ap_read(0, 0xFC)
    except Fault:
        d.clear_errors()
        return None


def status(d):
    print("DPIDR            %#010x" % d.connect())
    print("CTRL/STAT        %#010x" % d.dp_read(0x4))
    for ap in range(5):
        try:
            print("AP%d IDR          %#010x" % (ap, d.ap_read(ap, 0xFC)))
        except Fault:
            d.clear_errors()
            print("AP%d IDR          FAULT" % ap)
    v = d.ap_read(PWR_AP, DPREC0)
    print("PWR-AP DPREC0    %#010x  (FORCEACTIVE %s)" % (v, "set" if v & FORCEACTIVE else "clear"))
    print("=> AHB-AP %s" % ("reachable" if ap0_alive(d) else
                            "power-gated: chip is in STOP/STANDBY; use `wake`"))


def wake(d):
    d.connect()
    before = d.ap_read(PWR_AP, DPREC0)
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)
    for _ in range(50):
        idr = ap0_alive(d)
        if idr:
            break
        time.sleep(0.01)
    else:
        raise DapError("AHB-AP still faulting after FORCEACTIVE (DPREC0=%#x)" % d.ap_read(PWR_AP, DPREC0))
    print("DPREC0 %#010x -> %#010x, AHB-AP IDR %#010x: awake" % (before, d.ap_read(PWR_AP, DPREC0), idr))


def dump(d):
    wake(d)
    d.mem_write32(DHCSR, 0xA05F0003)
    time.sleep(0.05)
    print("DHCSR            %#010x" % d.mem_read32(DHCSR))
    for r, n in [(15, "PC"), (14, "LR"), (13, "SP")]:
        d.mem_write32(DCRSR, r)
        time.sleep(0.01)
        print("%-16s %#010x" % (n, d.mem_read32(DCRDR)))
    for n, a in [("SYSOSCCFG", SYSCTL + 0x1100), ("MCLKCFG", SYSCTL + 0x1104),
                 ("GENCLKEN", SYSCTL + 0x113C), ("PMODECFG", SYSCTL + 0x1140),
                 ("CLKSTATUS", SYSCTL + 0x1204), ("SCB.SCR", 0xE000ED10), ("VTOR", 0xE000ED08)]:
        print("%-16s %#010x" % (n, d.mem_read32(a)))
    v = [d.mem_read32(0), d.mem_read32(4)]
    print("flash[0],[4]     %#010x %#010x%s" % (v[0], v[1], "  <- BLANK: ROM BSL will run" if v[0] == 0xFFFFFFFF else ""))
    print("NONMAIN[0..3]    " + " ".join("%08x" % d.mem_read32(0x41C00000 + i) for i in (0, 4, 8)))
    print("(core left halted; run `release` when done)")


def elf_symbol(path, name):
    """Address of a symbol in an ELF32-LE file (stdlib only)."""
    import struct
    b = open(path, "rb").read()
    shoff, = struct.unpack_from("<I", b, 0x20)
    shentsize, shnum = struct.unpack_from("<HH", b, 0x2E)
    secs = [struct.unpack_from("<10I", b, shoff + i * shentsize) for i in range(shnum)]
    for sec in secs:
        if sec[1] != 2:                           # SHT_SYMTAB
            continue
        stroff = secs[sec[6]][4]
        for o in range(sec[4], sec[4] + sec[5], 16):
            st_name, st_value = struct.unpack_from("<II", b, o)
            end = b.index(b"\0", stroff + st_name)
            if b[stroff + st_name:end].decode() == name:
                return st_value
    raise DapError("symbol %s not found in %s" % (name, path))


GPIOA = 0x400A0000
CAPS = {0: "STANDBY1", 1: "STANDBY0", 2: "STOP2", 3: "STOP0", 4: "WFI"}


def describe(word):
    return "%s%s%s%s" % (CAPS.get((word >> 8) & 0xF, "?"), " +FASTWAKE" if word & 1 else "",
                         (" +re-arm %d us after edge, no release wait" % ((word >> 16) * 10)) if word & 2 else "",
                         " [timer-based blinky]" if word & 0x10 else "")


def peek(d, elf, quiet=False):
    """Read the edge_test counters and pin state without halting or resetting."""
    addr = elf_symbol(elf, "BTN_STATS")
    d.connect()
    before = d.ap_read(PWR_AP, DPREC0)
    gated = ap0_alive(d) is None
    if gated:
        d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)
        for _ in range(50):
            if ap0_alive(d):
                break
            time.sleep(0.01)
    magic, mode, s2, state, uart = (d.mem_read32(addr + 4 * i) for i in range(5))
    g = {n: d.mem_read32(GPIOA + o) for n, o in
         [("DIN", 0x1380), ("RIS", 0x1030), ("IMASK", 0x1028), ("FASTWAKE", 0x1404)]}
    pmode, mclk = d.mem_read32(SYSCTL + 0x1140), d.mem_read32(SYSCTL + 0x1104)
    if gated:
        d.ap_write(PWR_AP, DPREC0, 0)
    d.dp_write(0x4, 0)
    r = dict(ok=magic == 0xB07C0DE5, mode=mode, s2=s2, uart=uart, state=state, gated=gated, dprec=before, **g)
    if quiet:
        return r
    if not r["ok"]:
        print("BTN_STATS magic %#010x: edge_test not running (yet)" % magic)
        return r
    print("mode      %s" % describe(mode))
    print("counts    S2 %d, UART line %d" % (s2, uart))
    print("state     %s" % {1: "idle, waiting for an edge", 2: "awake, handling an edge"}.get(state, state))
    print("chip      %s (PWR-AP DPREC0 %#010x before the read)" % (
        "deep sleep, AHB-AP power-gated" if gated else "AHB-AP reachable", before))
    for name, n in (("PA14 S2", 14), ("PA9 UART", 9)):
        bit = lambda v: (v >> n) & 1
        print("%-9s pin %s | RIS %d | IMASK %d | FASTWAKE %d" % (
            name, "high" if bit(g["DIN"]) else "LOW", bit(g["RIS"]), bit(g["IMASK"]), bit(g["FASTWAKE"])))
    print("sysctl    PMODECFG.DSLEEP=%d  MCLKCFG=%#010x" % (pmode & 3, mclk))
    return r


def set_cfg(d, elf, word):
    """Write the edge_test config word, then reset so the firmware picks it up."""
    addr = elf_symbol(elf, "BTN_CFG")
    d.connect()
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)
    for _ in range(50):
        if ap0_alive(d):
            break
        time.sleep(0.01)
    d.mem_write32(addr, 0xC0F16A11)
    d.mem_write32(addr + 4, word)
    release(d)


def pulse_nrst(d):
    d.nrst(0)
    time.sleep(0.05)             # < 1 s: BOOTRST, not POR
    d.nrst(1)


def release(d):
    d.connect()
    d.ap_write(PWR_AP, DPREC0, FORCEACTIVE)      # make sure the core is reachable...
    for _ in range(50):
        try:
            d.mem_write32(DHCSR, 0xA05F0000)     # ...so a debugger's halt request can be withdrawn
            break
        except Fault:
            d.clear_errors()
            time.sleep(0.02)
    else:
        print("warning: could not reach DHCSR; relying on the reset to clear debug state")
    d.ap_write(PWR_AP, DPREC0, 0)
    d.dp_write(0x4, 0)
    pulse_nrst(d)
    print("released and reset")


def watch(d, secs):
    pulse_nrst(d)
    t0 = time.time()
    last = None
    while time.time() - t0 < secs:
        d.connect()
        s = "reachable" if ap0_alive(d) else "FAULT"
        v = d.ap_read(PWR_AP, DPREC0)
        if (s, v) != last:
            print("t=%5.1fs  AHB-AP %-9s DPREC0=%#010x" % (time.time() - t0, s, v))
            last = (s, v)
        d.dp_write(0x4, 0)
        time.sleep(0.2)


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "status"
    d = Dap()
    try:
        if cmd == "status": status(d)
        elif cmd == "wake": wake(d)
        elif cmd == "dump": dump(d)
        elif cmd == "release": release(d)
        elif cmd == "reset": d.connect(power=False); pulse_nrst(d); print("NRST pulsed")
        elif cmd == "peek": peek(d, sys.argv[2])
        elif cmd == "cfg": set_cfg(d, sys.argv[2], int(sys.argv[3], 0))
        elif cmd == "watch": watch(d, float(sys.argv[2]) if len(sys.argv) > 2 else 20)
        else: sys.exit(__doc__)
    finally:
        d.close()


if __name__ == "__main__":
    main()
