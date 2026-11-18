#!/usr/bin/env python3
"""Which LaunchPad the tools talk to. Select with the environment variable BOARD.

  BOARD=l1306 (default)  LP-MSPM0L1306: S2 on PA14, backchannel UART TXD -> PA9
  BOARD=c1104            LP-MSPM0C1104: S2 on PA16, backchannel UART TXD -> PA26

The probe is found by the serial-number prefix TI programs into the on-board
XDS110 (ML13... / MC11...), so both boards can be plugged in at once.

usage: board.py serial|chip|elf     (for shell scripts)
"""
import os, sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BOARDS = {
    "l1306": dict(serial="ML13", chip="MSPM0L1306", s2=14, rx=9, target="target-notimer", features="lp,l1306"),
    "c1104": dict(serial="MC11", chip="MSPM0C1104", s2=16, rx=26, target="target-c1104", features="lp,c1104"),
}
NAME = os.environ.get("BOARD", "l1306")
if NAME not in BOARDS:
    raise SystemExit("BOARD must be one of: " + ", ".join(BOARDS))
B = BOARDS[NAME]
SERIAL, CHIP, S2_BIT, RX_BIT = B["serial"], B["chip"], B["s2"], B["rx"]
# TARGET overrides the cargo target directory, to test an edge_test built against another embassy.
TARGET = os.environ.get("TARGET", B["target"])
ELF = os.path.join(ROOT, "firmware", TARGET, "thumbv6m-none-eabi/release/edge_test")


def full_serial():
    """The whole serial number of the selected probe, from sysfs."""
    import glob
    for d in glob.glob("/sys/bus/usb/devices/*/serial"):
        try:
            dev = os.path.dirname(d)
            if open(dev + "/idVendor").read().strip() == "0451" and open(d).read().strip().startswith(SERIAL):
                return open(d).read().strip()
        except OSError:
            pass
    raise SystemExit("no XDS110 with serial %s... (BOARD=%s) is plugged in" % (SERIAL, NAME))


if __name__ == "__main__":
    what = sys.argv[1] if len(sys.argv) > 1 else ""
    print({"serial": full_serial, "chip": lambda: CHIP, "elf": lambda: ELF, "features": lambda: B["features"],
           "target": lambda: TARGET}.get(what, lambda: sys.exit(__doc__))())
