<!-- Draft reply for embassy-rs/embassy#7513. AI-drafted: rewrite in your own words before posting. -->

I ran my test rig against @CordlessCoder's `mspm0-low-power` branch (tip 71f54dfee) on an LP-MSPM0L1306. That is not the exact commit linked above (087b1c788), but the GPIO approach is the same: FASTWAKE while armed, both edges always detected, direction decided in the handler by reading DIN.

It fixes what I reported. Chip confirmed asleep in each case, counts are edges seen / pulses sent:

| Test | main | 71f54dfee |
|---|---|---|
| STANDBY1, 833 µs and 30 ms pulses | 0/20 | 20/20 |
| STANDBY1, wait re-armed while pin still low | 0/20 | 20/20 |
| Pin held low 10 s, then 21 edges | not run | 21/21, asleep throughout |
| Bursts of bouncing edges, then 20 clean pulses | not run | 20/20 |

One regression: short pulses are dropped, in every deep-sleep mode.

| 8.7 µs pulses | main | 71f54dfee |
|---|---|---|
| STOP0 | 20/20 | 0/20 |
| STOP2 / STANDBY0 | 5/20, 4/20 | 0/20 |
| STANDBY1 | 0/20 | 0/20 |

Sweeping the width in STANDBY1: 16 µs and shorter 0/50, 17 µs 15/50, 18 µs and longer all counted.

The cause is the DIN read in the handler: if the pulse has ended by then, the pin reads idle, the event is taken for the opposite edge and discarded. The handler comment already says so. Fine for a button, not for a short strobe from another chip.

GPIO_ERR_01 lists a second workaround: don't enter STANDBY1 while the pulse is still active. I have a draft that does that instead: the polarity stays single-edge so the hardware latches the edge, and if the pin is already at the post-edge level when the wait starts, it first waits for the pin to leave that level. Same tests as above all pass, and pulses are counted down to 8.7 µs (the shortest my rig makes).

- Branch: https://github.com/mayl/embassy/tree/mspm0-gpio-wake (on main, +114/−2 in `gpio.rs`, `build.rs` and the changelog, no API change)
- Cost: 392 bytes of flash and 24 bytes of RAM in the `button` example on L1306. The erratum handling is behind a `gpio_err_01` cfg, set in `build.rs` from a family list (G110x, G150x, G310x, G350x, L110x, L130x, L134x) since main's metadata has no errata yet. Other chips only get fast wake.
- Limits: tested on one L1306 board only. After a release, a new edge within about 60 µs can be missed (30 µs on 71f54dfee).

Test scripts and raw results: https://github.com/mayl/mspm0sleep (`tools/edge_matrix.py`, `tools/edge_window.py`, `tools/edge_bounce.py`, `tools/edge_hold.py`). They pulse PA9 from the XDS110 backchannel UART and read counters back over SWD. Happy to rerun against any other commit.
