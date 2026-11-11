#!/usr/bin/env python3
"""Drive falling edges onto the target's PA9 from the XDS110 backchannel UART.

A UART line idles high. Sending 0xFF gives exactly one low pulse, the start
bit, 1/baud wide. Sending 0x00 gives a low pulse 9 bit-times wide.

Measured on XDS110 firmware 03.00.00.22: widths are right from 150 baud up
(0x00 = 60 ms) including non-standard rates; BELOW 150 baud they are wrong
(110 -> 3.1 ms, 75 -> 41 ms, 50 -> 23 ms). 60 ms is the longest usable pulse.

usage: uart_pulse.py <count> <baud> [byte=0xff] [gap_s=0.25]
"""
import glob, os, sys, termios, time


def find_tty():
    for t in sorted(glob.glob("/sys/class/tty/ttyACM*")):
        try:
            iface = os.path.realpath(t + "/device")
            dev = os.path.dirname(iface)
            if (open(dev + "/idVendor").read().strip() == "0451"
                    and open(iface + "/bInterfaceNumber").read().strip() == "00"):
                return "/dev/" + os.path.basename(t)
        except OSError:
            pass
    raise SystemExit("XDS110 application UART not found")


def open_uart(baud):
    """Open the backchannel UART raw, 8N1. Non-standard rates use termios2/BOTHER."""
    fd = os.open(find_tty(), os.O_RDWR | os.O_NOCTTY)
    a = termios.tcgetattr(fd)
    a[0] = 0; a[1] = 0; a[3] = 0                       # raw
    a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL  # 8N1
    std = getattr(termios, "B%d" % baud, None)
    if std is not None:
        a[4] = a[5] = std
        termios.tcsetattr(fd, termios.TCSANOW, a)
    else:
        import fcntl, struct
        termios.tcsetattr(fd, termios.TCSANOW, a)
        TCGETS2, TCSETS2, BOTHER, CBAUD = 0x802C542A, 0x402C542B, 0o010000, 0o010017
        t = list(struct.unpack("4IB19B2I", fcntl.ioctl(fd, TCGETS2, bytes(44))))
        t[2] = (t[2] & ~CBAUD) | BOTHER
        t[-2] = t[-1] = baud
        fcntl.ioctl(fd, TCSETS2, struct.pack("4IB19B2I", *t))
    return fd


def pulses(count, baud, byte=0xFF, gap=0.25):
    fd = open_uart(baud)
    time.sleep(0.3)                                  # let the line settle after any reconfiguration
    for _ in range(count):
        os.write(fd, bytes([byte]))
        termios.tcdrain(fd)
        time.sleep(gap)
    os.close(fd)


if __name__ == "__main__":
    a = sys.argv[1:]
    pulses(int(a[0]), int(a[1]), int(a[2], 0) if len(a) > 2 else 0xFF, float(a[3]) if len(a) > 3 else 0.25)
