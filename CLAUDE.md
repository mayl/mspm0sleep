# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->


## Build & Test

Everything runs inside the Nix dev shell (Rust toolchain with `thumbv6m-none-eabi`, probe-rs, uhubctl, radare2). `bd` is NOT in the shell; it is expected on the host PATH.

```bash
# firmware (our crate, built against the embassy submodule)
nix develop .# --command bash -c 'cd firmware && cargo build --release'
# same idle firmware on the ordinary WFI executor, for comparison
nix develop .# --command bash -c 'cd firmware && cargo build --release --no-default-features --features wfi,timer --bin lp_idle --target-dir target-wfi'

# GPIO edge-wake test (no time driver): build+flash, then run the automated matrix
nix develop .# --command tools/button_test.sh build
nix develop .# --command python3 tools/edge_matrix.py 20      # pulses PA9 from the XDS110 UART

# flash + release debug holds + EnergyTrace capture + statistics
nix develop .# --command tools/measure.sh lp_idle my_capture 20

# EnergyTrace utility: build + test, live scope, look at a capture without a terminal
nix develop .# --command bash -c 'cd tools/etrace && cargo build --release && cargo test --release'
nix run .#etrace -- live                      # interactive; needs a real terminal
tools/etrace/target/release/etrace view tools/energytrace/captures/lp_blinky_gpiofix.bin --dump 120x30
```

Do NOT flash with a bare `probe-rs run/download` when the target is asleep: probe-rs 0.32 cannot attach to an MSPM0 in STOP/STANDBY. Use `tools/unwedge/unwedge.py wake` first and `release` afterwards (see `docs/UNWEDGE.md`).

## Architecture Overview

- `embassy/` — submodule, **unmodified upstream** embassy (branch `mspm0l_lowpower` on the fork tracks `embassy-rs/embassy` main). Upstream provides MSPM0 deep sleep via `embassy_mspm0::executor::Executor` + the `low-power` feature; our own earlier sleep work was dropped in favour of it.
- `firmware/` — measurement firmwares for LP-MSPM0L1306 (`lp_idle`, `lp_blinky`, `busy`).
- `tools/etrace` — the EnergyTrace utility (Rust lib + CLI): live terminal scope, headless capture, statistics, export. `tools/measure.sh` uses it. See its README.
- `tools/energytrace/` — captures, reverse-engineering notes, and `repro-cli`, the scratch tool the protocol was worked out with (EEPROM dump, legacy flows). New work goes in `tools/etrace`.
- `tools/unwedge/` — raw CMSIS-DAP access to MSPM0's PWR-AP: wake a sleeping chip for the debugger, and release it again.

### Key Documents

| File | Purpose |
|------|---------|
| `docs/UNWEDGE.md` | Why SWD "dies" on a sleeping/blank MSPM0, PWR-AP bits, recovery recipe, uhubctl caveat |
| `docs/LOWPOWER_RESULTS.md` | Upstream embassy low-power verification + EnergyTrace measurements |
| `docs/UPSTREAM_GPIO_WAKE_PR.md` | Draft upstream fix for GPIO wake (fork branch `mspm0-gpio-wake`): design notes, test evidence, what must be hand-written before a PR |
| `docs/CCS_MSPM0_LOWPOWER.md` | What TI CCS Theia does on connect/disconnect, DSSM mailbox commands (from GEL + disassembly) |
| `tools/etrace/README.md` | Using etrace, what its numbers mean (resolution, offset, what is on the rail) |
| `tools/energytrace/REVERSE_ENGINEERING.md` | XDS110 EnergyTrace USB protocol notes (historical; later sections supersede earlier ones) |
| `tools/energytrace/THEIA_VERIFICATION.md` | Those notes checked claim-by-claim against TI's libraries |

## Conventions & Patterns

_Add your project-specific conventions here_

### Embassy Submodule

The `embassy/` directory is a git submodule pointing to the Embassy fork for this project. AI agents MUST follow these rules when modifying code inside it:

- **No Co-Authored-By lines** in commits within the `embassy/` submodule. Use your standard git config.
- **Work on feature branches** (currently `mspm0l_lowpower`, which is plain upstream) — do not commit directly to `main`. Prefer keeping the submodule unmodified and putting our code in `firmware/`.
- Commit submodule changes **first** inside `embassy/`, then update the parent repo pointer.
- **Nothing AI-written goes to embassy-rs as prose.** Their proposed AI policy (PR #6618) allows AI-written code if disclosed and understood, but not AI-written PR descriptions, commit messages, comments or review replies. Prepare code and notes; the user writes what gets posted. Draft commits on the fork carry `DRAFT ... (reword before PR)` subjects for that reason.
