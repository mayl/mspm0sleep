#!/usr/bin/env python3
"""Drive falling edges onto the target's PA9 from the XDS110 backchannel UART.

A UART line idles high. Sending 0xFF gives exactly one low pulse, the start
bit, 1/baud wide. Sending 0x00 gives a low pulse 9 bit-times wide.

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
    fd = os.open(find_tty(), os.O_RDWR | os.O_NOCTTY)
    a = termios.tcgetattr(fd)
    speed = getattr(termios, "B%d" % baud)
    a[0] = 0; a[1] = 0; a[3] = 0                       # raw
    a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL  # 8N1
    a[4] = a[5] = speed
    termios.tcsetattr(fd, termios.TCSANOW, a)
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
