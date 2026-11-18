#!/usr/bin/env python3
"""Catch an MSPM0 right after reset and halt it at its reset vector.

For a firmware that faults at boot: the chip then resets in a loop (a HardFault inside HardFault
locks the core up, which resets the chip), is awake only a few ms per turn, and PWR-AP FORCEACTIVE
does not hold. What works: hold NRST low, write DPREC0 = 0x1E008 (FORCEACTIVE plus bits 13..16,
which keep the core waiting in reset), release NRST, request a halt with the reset vector caught,
then drop bits 13..16. Found on an MSPM0C1104 linked with too much RAM.

usage: [BOARD=c1104] catch_reset.py [trace]   trace: run on, stop at a HardFault, print registers
"""
import sys, time
from dap import Dap, Fault, DapError

DHCSR, DCRSR, DCRDR, DEMCR = 0xE000EDF0, 0xE000EDF4, 0xE000EDF8, 0xE000EDFC


def catch(d):
    for _ in range(10):
        d.connect()
        d.nrst(0)
        time.sleep(0.05)
        try:
            d.ap_write(4, 0, 0x1E008)
        except Fault:
            d.clear_errors()
        d.nrst(1)
        t0 = time.time()
        while time.time() - t0 < 0.5:
            try:
                d.ap_write(4, 0, 0x1E008)
                d.mem_write32(DHCSR, 0xA05F0003)   # halt request, pending while the core waits in reset
                d.mem_write32(DEMCR, (1 << 24) | (1 << 10) | 1)   # catch the reset vector and HardFault
                d.ap_write(4, 0, 0x8)              # let the core out of reset, keep FORCEACTIVE
                for _ in range(50):
                    if d.mem_read32(DHCSR) & 0x20000:
                        return
                    time.sleep(0.01)
            except Fault:
                d.clear_errors()
    raise DapError("could not halt the chip after reset")


def reg(d, n):
    d.mem_write32(DCRSR, n)
    return d.mem_read32(DCRDR)


if __name__ == "__main__":
    d = Dap()
    catch(d)
    print("halted: PC %#x SP %#x" % (reg(d, 15), reg(d, 13)))
    if len(sys.argv) > 1:
        d.mem_write32(DEMCR, (1 << 24) | (1 << 10))   # HardFault only from here
        d.mem_write32(DHCSR, 0xA05F0001)
        for i in range(60):
            time.sleep(0.1)
            try:
                h = d.mem_read32(DHCSR)
            except Fault:
                d.clear_errors(); print("t=%.1f lost access" % (i / 10)); break
            if h & 0x20000:
                sp = reg(d, 13)
                print("t=%.1f halted: PC %#x LR %#x SP %#x xPSR %#x" % (i / 10, reg(d, 15), reg(d, 14), sp, reg(d, 16)))
                print("stack:", " ".join("%08x" % d.mem_read32(sp + 4 * k) for k in range(12)))
                break
        else:
            d.mem_write32(DHCSR, 0xA05F0003)
            print("still running after 6 s: PC %#x SP %#x" % (reg(d, 15), reg(d, 13)))
    # leaves the core halted with FORCEACTIVE set, ready for probe-rs
