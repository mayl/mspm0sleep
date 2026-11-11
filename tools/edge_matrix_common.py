"""Shared helper for the edge-wake test scripts."""
import os, sys, time
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "unwedge"))
from dap import Dap, DapError


def with_dap(fn, *a):
    for _ in range(3):                             # the probe occasionally fails its first command
        try:
            d = Dap()
            try:
                return fn(d, *a)
            finally:
                d.close()
        except (DapError, OSError):
            time.sleep(0.5)
    raise SystemExit("probe not responding")
