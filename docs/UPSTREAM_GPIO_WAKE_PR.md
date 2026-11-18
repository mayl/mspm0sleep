# Draft fix for embassy-rs/embassy#7513 (GPIO wake on MSPM0)

Working notes for Larry. **Not PR text**: embassy's proposed AI policy (PR #6618)
does not allow AI-written PR descriptions, commit messages or review replies, so
those have to be written by hand. Nothing has been posted upstream.

## Where it is

- Branch **`mspm0-gpio-wake-v2`** in the `embassy/` submodule, pushed to the fork
  (`mayl/embassy`), based on upstream `main` at 2b69fd062 (2026-11-18). That main
  includes i509VCB's GPIO rework (#7326, merged 2026-11-17), which rewrote the
  code the first draft changed.
- Three commits, subjects marked `DRAFT n/3 (reword before PR)`: +120/-6 in
  `gpio.rs` (+110), `build.rs` (+14) and the changelog (+2). No API change.
- The first draft, `mspm0-gpio-wake` (four commits on main of 5e697db33), is
  kept for reference. It no longer applies to main.
- The parent repo pins the submodule to unmodified upstream
  (`mspm0l_lowpower`, now 2b69fd062). To build the firmware against the fix:
  `git -C embassy checkout mspm0-gpio-wake-v2`.

Review with:

```bash
git -C embassy log --oneline upstream/main..mspm0-gpio-wake-v2
git -C embassy diff upstream/main mspm0-gpio-wake-v2          # everything
git -C embassy show <commit>                                   # one step at a time
```

## The three changes

All in `Flex<'d, Async>`; `Input` and `OutputOpenDrain` delegate to it.

1. **Arm before reading the level** in `wait_for_high` / `wait_for_low`.
   Upstream's `wait_inner` is split into `arm()` (set polarity, clear the latched
   event; existing code, moved) and `wait_armed()` (the existing `poll_fn`,
   moved). The level waits call `arm()` first and read the pin afterwards.
2. **Fast wake while waiting.** `wait_armed()` creates a `FastWake` guard that
   sets the pin's `FASTWAKE` bit; the future owns it, so the bit is cleared when
   the wait ends or is dropped. Without the bit no GPIO edge is detected in
   STANDBY1 (TRM SLAU847F 2.9.4).
3. **GPIO_ERR_01**, only on affected families. `build.rs` (`get_chip_cfgs`) sets
   a `gpio_err_01` cfg for G110x, G150x, G310x, G350x, L110x, L130x, L134x.
   `wait_inner` has two `#[cfg]` versions:
   - without the erratum: `arm(polarity)` then `wait_armed()`, which is what
     upstream does today plus fast wake;
   - with it: a three-step loop. If the pin already sits at the level the edge
     leads to (say low, waiting for a fall), it first waits for the opposite
     edge, then arms the real one. Every step arms and then reads the pin, so
     the chip never sleeps armed for an edge the pin level rules out. Any-edge
     waits are unchanged.

## Design choices you may want to question

- **The erratum version is an `async fn`, the other is not.** Upstream's
  `wait_inner` returns `impl Future` and arms when called. The loop arms on
  first poll. Callers only ever `.await` it at once, so it should not matter,
  but it is a difference between the two cfgs.
- **Family list.** Copied from the errata metadata on CordlessCoder's branch
  (`METADATA.has_erratum`), which upstream's pinned metapac does not have. Only
  the L110x/L13xx sheet (SLAZ741E) was read here; check the G sheets. The
  C1104 measurements below agree with it not being listed.
- **Two `#[cfg]` functions rather than `if !cfg!(gpio_err_01) { return }`.**
  The `if` form was tried on the first draft and made every chip 100 to 270
  bytes larger: the second await stays in the future.
- **Loop instead of straight-line code.** A straight-line version was 420 bytes
  bigger on the first draft.
- **Code size**, `button` example, release, Rust 1.100 (text / bss):

  | Chip | main | with the fix |
  |---|---|---|
  | L1306 (erratum) | 9220 / 1124 | 9620 / 1148 (+400 / +24) |
  | L2228 | 9376 / 1124 | 9568 / 1132 (+192 / +8) |
  | C1104 | 7052 / 172 | 7172 / 180 (+120 / +8) |

- **Fast wake is cleared when no wait is pending.** With the bit left on, any
  activity on the pin requests the fast clock even when nobody is waiting (note
  at the end of TRM 9.2.3). The cost is one more critical section per wait.
- **`IMASK` is still not cleared when a wait future is dropped.** Existing
  behaviour, left alone.
- **Order of the two reads in the loop.** The pin level is read before the
  event flag: an edge after arming is then seen in at least one of them.

## Test evidence (2026-11-18)

Two boards: LP-MSPM0L1306 (has GPIO_ERR_01) and LP-MSPM0C1104 (not listed).
Pulses go into the UART RX pin (PA9 / PA26) from the XDS110; counters are read
over SWD. "main" is unmodified upstream 2b69fd062, "fix" is 1d1fcc22f. One
command per build: `BOARD=... TARGET=... tools/edge_all.sh <label>`. Raw output:
`tools/energytrace/captures/edge_{main2,port}_{l1306,c1104}.log` and
`edge_matrix_*.json`.

**The run of the fix is incomplete.** The machine ran low on memory and the
background jobs were stopped. Done: everything on main, and for the fix the
matrix and the first width sweep on both boards. Not yet run on the port: the
`wait_for_high` width sweep, bounce, pin-held-low, the timer-based blinky and
the idle current. The first draft passed all of those on the L1306
(2026-11-12, table at the end).

**Idle-mode matrix**, timer-free firmware, firmware leaves FASTWAKE alone,
counted / 20 pulses. Chip confirmed asleep after every STOP and STANDBY cell.

| Idle mode | Pulse | L1306 main | L1306 fix | C1104 main | C1104 fix |
|---|---|---|---|---|---|
| WFI, STOP0 | all four shapes | 20 | 20 | 20 | 20 |
| STOP2 | 8.7 us | 7 | 20 | 2 | 20 |
| STOP2 | 833 us, 30 ms, 30 ms re-armed while low | 20 | 20 | 20 | 20 |
| STANDBY0 | 8.7 us | 4 | 20 | 4 | 20 |
| STANDBY0 | 833 us, 30 ms, 30 ms re-armed while low | 20 | 20 | 20 | 20 |
| STANDBY1 (default) | 8.7 us, 833 us, 30 ms | 0 | 20 | 0 | 20 |
| STANDBY1 (default) | 30 ms, re-armed while low | 0 | 20 | 0 | 20 |

**The erratum, isolated**: main with FASTWAKE set by the test firmware, i.e.
what change 2 alone would give.

| | L1306 | C1104 |
|---|---|---|
| STANDBY1, 30 ms pulse, re-armed while low | **1/20** | 20/20 |
| Width sweep 9 to 153 us in 1 us steps, 50 pulses each, plain falling wait, re-armed at once | 54 of 145 widths; from 63 us up almost every width counts 1 of 50 | 145 of 145 |

So the C1104 does not have the deaf state and the L1306 does, which is what
the cfg assumes.

**Width sweep with the fix** (STANDBY1, 9 to 153 us in 1 us steps, 50 pulses
each, plain `wait_for_falling_edge()` in a loop, HAL sets FASTWAKE): 145 of 145
widths fully counted on both boards (7,250 pulses each).

**`wait_for_high()` then `wait_for_falling_edge()`** on main with firmware-set
FASTWAKE: L1306 stuck at 37, 38, 39 us (1 of 50, firmware left waiting for a
level that had already come); C1104 half the pulses lost at 43 to 46 us. That
is the race change 1 closes. Not yet re-run with the fix.

**Bounce and pin-held-low on main**: nothing counted in STANDBY1 on either
board (0 of 21 in the hold test), as expected without fast wake.

**Builds:** all 18 feature sets in `embassy-mspm0`'s CI build list compile at
the tip without new warnings; commits 1 and 2 compile on their own for L1306
and C1104; `button` builds for L1306, L2228, C1104. rustfmt (1.100 stable, the
repo's `rustfmt.toml`) reports no changes for `gpio.rs` and `build.rs`.

## What only you can do before opening a PR

- Read and understand every line (policy: "review and understand every single
  line of AI-written code").
- Reword the three commit messages and write the PR description yourself, and
  say in it that the code was written with an LLM.
- Run `./fmtall.sh` with the repo's pinned nightly if you change anything.
- Mark the PR ready only when CI passes (`CONTRIBUTING.md`).

## Not tested

- The rest of the suite on the port (see above).
- Chips other than MSPM0L1306 and MSPM0C1104; ports other than GPIOA.
- An affected G-family chip.
- CI itself.

## The fix the maintainers pointed to (tested 2026-11-12)

On #7513 a maintainer (i509VCB) said this is known and already fixed by CordlessCoder "in some
commit" that has not been split out yet, and that it probably has to land after #7326. The code
is on CordlessCoder's fork, branch `mspm0-low-power` (tip 71f54dfee, 2026-10-22, based on upstream
main of 2026-10-12). It is not a single commit: the branch is several hundred commits that rework
most of `embassy-mspm0`. The GPIO wake part starts at 9617fcc4b "MSPM0 Wake from deep sleep on a
GPIO edge" and is reworked by several dozen later commits to `gpio.rs`.

What it does for GPIO wake, read from the code:

- Sets `FASTWAKE` while a wait is armed and clears it, with the mask and the latched event, when
  the wait completes or is dropped.
- For `GPIO_ERR_01` it always detects **both** edges on affected families (`cfg(gpio_err_01)`)
  and decides the direction in the interrupt handler by reading the pin. This is TI's first
  workaround; the draft above uses the second.
- `wait_for_high` / `wait_for_low` re-test the level after arming.
- The API changes: an async input is `Input::new_async(pin, Irqs, pull)` with the port interrupt
  bound by the application, and the executor only deep-sleeps with the `low-power` feature
  enabled explicitly.

To build our firmware against it: `git -C embassy checkout 71f54dfee` (remote `cordless`), then
add the firmware feature `cordless-api`, e.g.
`cargo build --release --no-default-features --features lp,cordless-api --bin edge_test --target-dir target-notimer`.
The first run was made without `low-power`: everything passed, but the per-cell sleep check
showed the chip had never left plain WFI, so that run was thrown away.

Results from 2026-11-12, before the port, chip confirmed asleep in every deep-sleep cell. Their branch
has not been re-run since; the other two columns are superseded by the tables above:

| Test (L1306, 2026-11-12) | Upstream main before #7326 | CordlessCoder 71f54dfee | First draft `mspm0-gpio-wake` |
|---|---|---|---|
| STANDBY1, 833 us and 30 ms pulses | 0/20 | 20/20 | 20/20 |
| STANDBY1, 30 ms, re-armed while low | 0/20 | 20/20 | 20/20 |
| 8.7 us pulses, STOP0 | 20/20 | **0/20** | 20/20 |
| 8.7 us pulses, STOP2 / STANDBY0 | 5/20, 4/20 | **0/20** | 20/20 |
| 8.7 us pulses, STANDBY1 | 0/20 | **0/20** | 20/20 |
| Width sweep, plain falling wait, STANDBY1 | not run | lost up to 16 us (0/50), 17 us 15/50, 18 us and up all counted | none lost from 9 us |
| Width sweep, `wait_for_high` then falling wait | stuck at 74 and 76 us | no stuck state; same loss up to 16 us | none lost |
| Bounce stress, then 20 clean pulses | not run | 20/20 after every burst width | 20/20 |
| Pin held low 10 s | not run | asleep throughout, 21 of 21 edges | asleep throughout, 21 of 21 |
| Timer-based blinky, default idle | 0 to 4/20 | 20/20 at 833 us, 30 ms, 60 ms | 20/20 |
| Idle floor waiting on two pins | 20 pulses/s | 20 pulses/s | 20 pulses/s |

So their branch fixes the reported problem. The one difference the rig finds is short pulses: a
pulse that ends before the interrupt handler reads the pin (about 17 us here) is taken for the
opposite edge and dropped, in every deep-sleep mode including STOP0, where unmodified upstream
catches it. Their handler has a comment saying exactly this ("A pulse shorter than the handler
latency ... loses a real edge for `wait_for_rising_edge`"), so it is a known trade-off of the
both-edges workaround, not an oversight. It does not matter for a button. It would matter for a
short strobe from another chip.

Raw results: `tools/energytrace/captures/edge_matrix_cordless.json`, `lp_blinky_cordless.*`,
`lp_idle_cordless.*`. Not tested on their branch: anything other than GPIO wake.
