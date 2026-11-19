<!-- Draft reply for embassy-rs/embassy#7513. AI-drafted: rewrite in your own words before posting. -->
<!-- Posted by Larry (edited) on 2026-11-18. Since then the fix has also passed the wait_for_high sweep,
     bounce, hold, blinky and idle-current tests on main 2b69fd062: docs/UPSTREAM_GPIO_WAKE_PR.md. -->

I ran my rig against @CordlessCoder's `mspm0-low-power` branch (tip 71f54dfee) on an LP-MSPM0L1306. That is not the exact commit linked above (087b1c788), but the approach is the same: FASTWAKE while armed, both edges always detected, direction decided in the handler by reading DIN.

It fixes what I reported (STANDBY1: 20/20 at 833 us and 30 ms, also when re-armed with the pin still low; pin held low 10 s: asleep throughout, 21/21). One regression: short pulses are dropped in every deep-sleep mode, including STOP0 where main catches them. 8.7 us pulses: 0/20 everywhere. Width sweep in STANDBY1: 16 us and shorter 0/50, 17 us 15/50, 18 us and up all counted. The cause is the DIN read in the handler: if the pulse has ended by then, the event is taken for the opposite edge and discarded. Fine for a button, not for a short strobe.

GPIO_ERR_01 lists a second workaround: don't enter STANDBY1 while the pulse is still active. I have a draft on current main (after #7326) that does that: the polarity stays single-edge so the hardware latches the edge, and if the pin is already at the post-edge level when the wait starts, it first waits for the pin to leave it. That part is behind a `gpio_err_01` cfg; other chips only get fast wake.

Counted out of 20 pulses, firmware does not touch FASTWAKE, chip asleep in every STOP/STANDBY cell:

| Idle mode | Pulse | L1306 main | L1306 fix | C1104 main | C1104 fix |
|---|---|---|---|---|---|
| STOP0 | any | 20 | 20 | 20 | 20 |
| STOP2 | 8.7 us | 7 | 20 | 2 | 20 |
| STANDBY0 | 8.7 us | 4 | 20 | 4 | 20 |
| STOP2, STANDBY0 | 833 us, 30 ms | 20 | 20 | 20 | 20 |
| STANDBY1 | 8.7 us, 833 us, 30 ms | 0 | 20 | 0 | 20 |
| STANDBY1 | 30 ms, re-armed while low | 0 | 20 | 0 | 20 |

Width sweep with the fix, STANDBY1, 9 to 153 us in 1 us steps, 50 pulses each: none lost on either board.

The C1104 does not show the erratum: with FASTWAKE set by hand on main, "re-armed while low" is 20/20 there and 1/20 on the L1306.

- Branch: https://github.com/mayl/embassy/tree/mspm0-gpio-wake-v2 (+120/-6, no API change)
- Cost in the `button` example: 400 bytes of flash and 24 of RAM on L1306, 120 and 8 on C1104.
- The `gpio_err_01` family list is in `build.rs` (G110x, G150x, G310x, G350x, L110x, L130x, L134x), since main's metadata has no errata yet.
- Limits: one L1306 and one C1104 board, GPIOA only.

Test scripts and raw results: https://github.com/mayl/mspm0sleep (`tools/edge_all.sh`). They pulse the UART RX pin from the XDS110 backchannel UART and read counters back over SWD. Happy to rerun against any other commit.
