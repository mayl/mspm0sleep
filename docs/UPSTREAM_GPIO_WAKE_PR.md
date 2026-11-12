# Draft fix for embassy-rs/embassy#7513 (GPIO wake on MSPM0)

Working notes for Larry. **Not PR text**: embassy's proposed AI policy (PR #6618)
does not allow AI-written PR descriptions, commit messages or review replies, so
those have to be written by hand. Nothing has been posted upstream.

## Where it is

- Branch `mspm0-gpio-wake` in the `embassy/` submodule, pushed to the fork
  (`mayl/embassy`), based on upstream `main` at 5e697db33.
- Three commits, `embassy-mspm0/src/gpio.rs` only (+ two changelog lines). The
  commit subjects are placeholders marked `DRAFT n/3 (reword before PR)`.
- The parent repo still pins the submodule to unmodified upstream
  (`mspm0l_lowpower`). To build our firmware against the fix:
  `git -C embassy checkout mspm0-gpio-wake`.

Review with:

```bash
git -C embassy log --oneline upstream/main..mspm0-gpio-wake
git -C embassy diff upstream/main mspm0-gpio-wake            # everything
git -C embassy show <commit>                                  # one step at a time
```

## The three changes

1. **Arm before reading the level** in `wait_for_high` / `wait_for_low`.
   `wait_inner` is split into `arm()` (set polarity, clear the latched event;
   the existing code, moved) and `wait_armed()` (the existing wait-map future,
   moved). The level waits now call `arm()` first and read the pin afterwards,
   as `embassy-stm32`'s `ExtiInput` does.
2. **Fast wake while waiting.** `wait_armed()` holds a small guard that sets
   the pin's `FASTWAKE` bit and clears it when the wait ends or is dropped.
   Without the bit no GPIO edge is detected in STANDBY1 (TRM SLAU847F 2.9.4:
   STANDBY1 leaves only "TIMG0, TIMG1, or asynchronous fast wake from GPIO ...").
3. **GPIO_ERR_01** in edge waits. `wait_inner` becomes a three-step loop: if the
   pin already sits at the level the edge leads to (say low, waiting for a
   fall), it first waits for the opposite edge, then arms the real one. Every
   step arms and then reads the pin, so the chip never sleeps armed for an edge
   the pin level rules out. Any-edge waits are unchanged.

## Design choices you may want to question

- **Loop instead of straight-line code in change 3.** A straight-line version
  (arm, detour, arm again, each with its own `await`) reads more easily but is
  420 bytes bigger, because `arm()` and the wait future get inlined at every
  call site. Sizes of `examples/mspm0l1306` `button`, release:

  | | .text | .bss |
  |---|---|---|
  | upstream `main` | 12292 | 160 |
  | + change 1 | 12348 | 168 |
  | + change 2 | 12528 | 176 |
  | + change 3, straight-line (not kept) | 13104 | 184 |
  | + change 3, loop (kept) | 12684 | 184 |

  Total cost: 392 bytes of flash and 24 bytes of RAM per task that waits on a
  pin. The maintainer of this crate cares about both (see PR #7326).
- **Not feature-gated.** Fast wake and the erratum handling are compiled in
  for every chip and with or without `low-power`. That keeps the diff small.
  The erratum is listed for L and G parts only; C and H parts pay the bytes
  without (as far as the errata sheets say) needing them.
- **Fast wake is cleared when no wait is pending.** With the bit left on, any
  activity on the pin requests the fast clock even when nobody is waiting (note
  at the end of TRM 9.2.3). The cost is one more critical section per wait.
- **`IMASK` is still not cleared when a wait future is dropped.** That is
  existing behaviour and was left alone to keep the scope narrow.
- **Order of the two reads in change 3.** The pin level is read before the
  event flag. The argument: an edge after arming either is not yet visible in
  the level (so the pin is "not at the level" and we wait normally) or has set
  the flag by the time it is read. The other order has a window.
- **Stale comment not touched.** `low_power/mod.rs` already says the STANDBY1
  half of GPIO_ERR_01 "is handled". With this change that becomes true.
- **Conflict with PR #7326** (GPIO interrupt bindings, open, same file). It
  rewrites `wait_inner`; whichever lands second needs a rebase.

## Test evidence (LP-MSPM0L1306, patched HAL, firmware does not touch FASTWAKE)

All runs are with the final code (commit 3ff39ab4c). "Before" is unmodified
upstream, from `docs/LOWPOWER_RESULTS.md`. Pulses go into PA9 from the XDS110
UART; counters are read over SWD.

**Idle-mode matrix**, timer-free firmware, 20 pulses per cell
(`FWS=0 OUT=edge_matrix_fix.json tools/edge_matrix.py`):

| Idle mode | 8.7 us | 833 us | 30 ms | 30 ms, re-armed while low |
|---|---|---|---|---|
| WFI, STOP0 | 20/20 | 20/20 | 20/20 | 20/20 |
| STOP2 | 20/20 (before: 5) | 20/20 | 20/20 | 20/20 |
| STANDBY0 | 20/20 (before: 4) | 20/20 | 20/20 | 20/20 |
| STANDBY1 (default) | 20/20 (before: 0) | 20/20 (before: 0) | 20/20 (before: 0) | 20/20 (before: 0; 1 with FASTWAKE set by hand) |

**Pulse-width sweep**, STANDBY1, re-armed immediately, 50 pulses per width
(`STRATS=0,2 FW=0 tools/edge_window.py 50`, and again with `WIDTHS=40:120:1`):

| Firmware pattern | 64 widths, 9 to 833 us | 81 widths, 40 to 120 us in 1 us steps |
|---|---|---|
| plain `wait_for_falling_edge()` in a loop | 64 of 64 | 81 of 81 |
| `wait_for_high()`, then `wait_for_falling_edge()` | 64 of 64 (before: stuck at 74 and 76 us) | 81 of 81 |

That is 14,500 pulses with none lost.

**Each change on its own:**

- Change 1 alone (FASTWAKE set by the firmware): the `wait_for_high()` sweep
  goes from 62 of 64 widths to 64 of 64.
- Changes 1 and 2: STANDBY1 goes from 0/20 to 20/20 in the first three
  columns; the "re-armed while low" cell is 1/20, which is the erratum.
- Change 3 turns that cell into 20/20.

**Bounce stress** (`tools/edge_bounce.py`): bursts of five pulses with bit
times from 9 to 500 us (20 bursts each), then 20 clean pulses. STANDBY1 and
STANDBY0: the 20 clean pulses were counted 20/20 after every burst width.
Edges inside the fast bursts are not all counted (20 to 60 of 100 below
60 us, 100 of 100 from 60 us up), which is expected: they arrive faster than
the firmware can handle them.

**Pin held low** (`tools/edge_hold.py`, UART break): the firmware counts the
falling edge, re-arms with the pin still low and so waits for the rising edge
first. Held for 10 s, the chip was asleep at 5 s and at 10 s (AHB-AP
power-gated, count still 1), and counted 20/20 pulses after release. Repeated
with no SWD reads in between (`QUIET=1`): 21 of 21 edges in total.

**Timer-based firmware** (`lp_blinky`, 50 ms timer debounce, default idle
mode, `tools/edge_blinky.py`): 20/20 at 833 us, 30 ms and 60 ms (before: 0/20,
2 to 4/20, 4/20).

**Idle current** (EnergyTrace, J15 removed): `lp_blinky` waiting on two pins
has a floor of 20 pulses/s (2.1 uA), the same 20 pulses/s as before the change
and as `lp_idle`, which uses no GPIO wait. One pulse/s is about 0.5 uA, so any
cost of fast wake is below that.

**Builds:** all 18 feature sets in `embassy-mspm0`'s CI build list and
`examples/mspm0l1306` compile without warnings. Nightly rustfmt
(1.12.0-nightly, 2026-11-09) with the repo's `rustfmt.toml` reports no changes
for `gpio.rs`.

The board is left running `lp_blinky` built against the fix, so pressing S2
should flash the LED at once and toggle the blink rate on every press.

## What only you can do before opening a PR

- Read and understand every line (policy: "review and understand every single
  line of AI-written code").
- Reword the three commit messages and write the PR description yourself, and
  say in it that the code was written with an LLM.
- Run `./fmtall.sh` with the repo's pinned nightly (`nightly-2026-08-26`) if
  you change anything. A newer nightly rustfmt was run here on `gpio.rs` only.
- Decide whether to wait for a maintainer reply on #7513 first.
- Mark the PR ready only when CI passes; that is what puts it in the review
  queue (`CONTRIBUTING.md`).

## Not tested

- Any chip other than the MSPM0L1306, and ports other than GPIOA.
- GPIO_ERR_01 case 1 (STANDBY0, pin de-asserted for under 3 LFCLK cycles) as
  a separate case; the bounce test below goes through that range without loss.
- CI itself (only the `embassy-mspm0` build matrix from its `Cargo.toml` and
  `examples/mspm0l1306` were built locally).
