#!/usr/bin/env python3
"""Hold PA9 low (UART break) while edge_test waits for a falling edge on it,
and check that the chip is asleep during the hold and still counts afterwards.

With the patched HAL the firmware is then inside the GPIO_ERR_01 detour: it
got one falling edge, re-armed at once with the pin still low, and is waiting
for the pin to go high before it arms the falling edge again.

usage: edge_hold.py [hold_seconds=5]   (edge_test flashed; STANDBY1, immediate re-arm, FASTWAKE left to the HAL)
env:   QUIET=1   no SWD reads between the first and the last one (a read forces the chip active for a moment,
                 which could hide a deaf state)
"""
import fcntl, os, sys, termios, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "tools", "unwedge"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import unwedge, uart_pulse
from edge_matrix_common import with_dap

ELF = os.path.join(ROOT, "firmware/target-notimer/thumbv6m-none-eabi/release/edge_test")
HOLD = float(sys.argv[1]) if len(sys.argv) > 1 else 5.0
WORD = 0x002


QUIET = os.environ.get("QUIET") == "1"


def peek(label, always=False):
    if QUIET and not always:
        return None
    r = with_dap(unwedge.peek, ELF, True)
    print("%-34s count %2d  PA9 %s  RIS %d IMASK %d FASTWAKE %d  %s" % (
        label, r["uart"], "high" if r["DIN"] >> 9 & 1 else "LOW ", r["RIS"] >> 9 & 1, r["IMASK"] >> 9 & 1,
        r["FASTWAKE"] >> 9 & 1, "asleep (AHB-AP power-gated)" if r["gated"] else "AWAKE (AHB-AP reachable)"), flush=True)
    return r


with_dap(unwedge.set_cfg, ELF, WORD)
t0 = time.time()
while True:
    time.sleep(1.0)
    r = with_dap(unwedge.peek, ELF, True)
    if (r["ok"] and r["mode"] == WORD) or time.time() - t0 > 30:
        break
time.sleep(1.5)
first = peek("idle, pin high", True)
fd = uart_pulse.open_uart(1200)
fcntl.ioctl(fd, 0x5427)                           # TIOCSBRK: TX low until TIOCCBRK
time.sleep(HOLD)
peek("pin held low for %.0f s" % HOLD)
time.sleep(HOLD)
peek("pin held low for %.0f s" % (2 * HOLD))
fcntl.ioctl(fd, 0x5428)                           # TIOCCBRK
os.close(fd)
time.sleep(1.0)
peek("released")
uart_pulse.pulses(20, 1200, 0xFF, 0.15)
time.sleep(0.3)
last = peek("after 20 more pulses", True)
print("counted %d edges in total; expected 21 (one for the hold, 20 after it)" % (last["uart"] - first["uart"]))
