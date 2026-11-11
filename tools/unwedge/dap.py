"""Minimal CMSIS-DAP v1 (HID) SWD client for the XDS110, stdlib only.

Gives raw DP/AP register access so we can talk to MSPM0's vendor access
ports (PWR-AP, SEC-AP, CFG-AP) that generic ARM tooling ignores.
"""
import glob, os, struct, time


class DapError(Exception):
    pass


class Fault(DapError):
    pass


def find_hidraw(vid=0x0451, pid=0xBEF3):
    for h in sorted(glob.glob("/sys/class/hidraw/hidraw*")):
        try:
            ue = open(h + "/device/uevent").read()
        except OSError:
            continue
        if "HID_ID=0003:%08X:%08X" % (vid, pid) in ue:
            return "/dev/" + os.path.basename(h)
    raise DapError("no XDS110 CMSIS-DAP hidraw node found")


class Dap:
    def __init__(self, path=None, khz=1000, verbose=False):
        self.fd = os.open(path or find_hidraw(), os.O_RDWR)
        self.verbose = verbose
        self.cmd(bytes([0x02, 0x01]))                       # DAP_Connect SWD
        self.cmd(bytes([0x11]) + struct.pack("<I", khz * 1000))  # SWJ_Clock
        self.cmd(bytes([0x04, 0x00]) + struct.pack("<HH", 64, 0))  # TransferConfigure
        self.cmd(bytes([0x13, 0x00]))                       # SWD_Configure
        self.select = None

    def close(self):
        try:
            self.cmd(bytes([0x03]))                         # DAP_Disconnect
        finally:
            os.close(self.fd)

    def cmd(self, data):
        os.write(self.fd, b"\x00" + data.ljust(64, b"\x00"))
        r = os.read(self.fd, 64)
        if r[0] != data[0]:
            raise DapError("bad response %s to %s" % (r[:4].hex(), data[:4].hex()))
        return r

    # --- wire level -------------------------------------------------------
    def swj_seq(self, bits, value):
        n = (bits + 7) // 8
        self.cmd(bytes([0x12, bits & 0xFF]) + value.to_bytes(n, "little"))

    def line_reset(self):
        self.swj_seq(51, (1 << 51) - 1)
        self.swj_seq(16, 0xE79E)            # JTAG-to-SWD
        self.swj_seq(51, (1 << 51) - 1)
        self.swj_seq(8, 0)
        self.select = None

    def pins(self, out, sel, wait_us=0):
        """DAP_SWJ_Pins. bit0 SWCLK, bit1 SWDIO, bit7 nRESET. Returns pin state."""
        return self.cmd(bytes([0x10, out, sel]) + struct.pack("<I", wait_us))[1]

    def nrst(self, level):
        return self.pins(0x80 if level else 0x00, 0x80)

    def _xfer(self, req, data=None):
        pkt = bytes([0x05, 0x00, 0x01, req])
        if data is not None:
            pkt += struct.pack("<I", data)
        r = self.cmd(pkt)
        ack = r[2] & 7
        if self.verbose:
            print("   xfer req=%02x data=%s -> cnt=%d ack=%d %s" % (
                req, None if data is None else hex(data), r[1], r[2], r[3:7].hex()))
        if ack == 4:
            raise Fault("FAULT (req=%#x)" % req)
        if ack != 1 or r[1] != 1:
            raise DapError("transfer failed ack=%#x cnt=%d (req=%#x)" % (r[2], r[1], req))
        return struct.unpack("<I", r[3:7])[0]

    # --- DP / AP ----------------------------------------------------------
    def dp_read(self, addr):
        return self._xfer(0x02 | (addr & 0xC))

    def dp_write(self, addr, val):
        self._xfer(addr & 0xC, val)

    def _sel(self, ap, addr):
        s = (ap << 24) | (addr & 0xF0)
        if s != self.select:
            self.dp_write(0x8, s)
            self.select = s

    def ap_read(self, ap, addr):
        self._sel(ap, addr)
        return self._xfer(0x03 | (addr & 0xC))

    def ap_write(self, ap, addr, val):
        self._sel(ap, addr)
        self._xfer(0x01 | (addr & 0xC), val)

    def clear_errors(self):
        self.dp_write(0x0, 0x1E)            # ABORT: clear all sticky flags

    def connect(self, power=True):
        self.line_reset()
        idr = self.dp_read(0x0)
        self.clear_errors()
        if power:
            self.dp_write(0x4, 0x50000000)  # CSYSPWRUPREQ | CDBGPWRUPREQ
            t = time.time()
            while self.dp_read(0x4) >> 28 != 0xF:
                if time.time() - t > 1:
                    raise DapError("debug power-up not acknowledged: CTRL/STAT=%#x" % self.dp_read(0x4))
        return idr

    # --- MEM-AP (AP0) -----------------------------------------------------
    def mem_read32(self, addr, ap=0):
        self.ap_write(ap, 0x00, 0x23000052)  # CSW: 32-bit, no auto-inc
        self.ap_write(ap, 0x04, addr)
        return self.ap_read(ap, 0x0C)

    def mem_write32(self, addr, val, ap=0):
        self.ap_write(ap, 0x00, 0x23000052)
        self.ap_write(ap, 0x04, addr)
        self.ap_write(ap, 0x0C, val)
