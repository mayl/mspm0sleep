# CCS Theia 1.5.1 install: how TI tooling handles an MSPM0 in low power / wedged

Root: `/nix/store/r9i9vskavbcw0qlpaa5i4pk6lckh7477-ccs-theia-unwrapped-1.5.1.00003/ccs/ccs_base` (abbrev. `$B`).
Static analysis only. No USB touched. Nothing modified in any git repo.

Tags used below:
- [FILE] = stated verbatim in a GEL/XML text file
- [DISASM] = read from x86-64 disassembly of a shared library (radare2); this is what the code does, but names are mine
- [INFER] = my inference from [FILE]/[DISASM]
- [GENERAL] = TI public docs / my background knowledge, NOT from the install

Key files:
- `$B/emulation/gel/mspm0_cs_dap_init.gel`  (CS_DAP-level GEL: reset, DSSM mailbox)
- `$B/emulation/gel/mspm0l1306.gel`         (device GEL: connect hooks, hotmenu items, wait-for-debug)
- `$B/common/targetdb/devices/MSPM0L1306.xml`  (AP map)
- `$B/common/targetdb/Modules/AP/{PWRAP,SECAP,CFGAP}.xml`  (AP register names/offsets only, NO bitfields)
- `$B/common/targetdb/cpus/sec_ap.xml`      (SEC-AP register bitfields)
- `$B/common/targetdb/Modules/msp/mspm0l11xx_l13xx/DEBUGSS_1.0.xml` (device-side DEBUGSS/DSSM registers)
- `$B/DebugServer/propertyDB/MSPM0_FlashProperties.xml`  (CCS flash-settings UI incl. DSSM buttons, wait-for-debug)
- `$B/emulation/drivers/libprsc.so`  ("PRSC" = power/reset/sleep controller; contains the real PWR-AP logic)

The PWR-AP register bit layout is NOT in any text file in the install. It exists only as code in libprsc.so, so Q1 is from disassembly.

---------------------------------------------------------------------------------------------------
## Q1. Vendor APs, IDRs, register maps, PWR-AP bit layout

### 1a. AP index / role map  [FILE]
`MSPM0L1306.xml` lines (router CS_DAP_0):

| role | instance baseaddr | note |
|---|---|---|
| CFGAP | `baseaddr="0x00020100"` | AP index 1 |
| SECAP | `baseaddr="0x00020200"` | AP index 2; also `<cpu id="SEC_AP">` "Access Port Designator" `0x02000002` |
| PWRAP | `baseaddr="0x00020400"` | AP index 4 |
| AHB (CORTEX_M0P) | designator `0x02000000` | AP 0 |

(Mapping baseaddr -> AP number as `(addr>>8)&0xF` is my reading [INFER]; it agrees with the SEC_AP designator `0x02000002`.)

So the file says CFG-AP=AP1, SEC-AP=AP2, PWR-AP=AP4, AHB-AP=AP0. **Your assumed numbering is the one CCS uses.** AP3 is not instantiated in the targetdb. `DEBUGSS_SPECIAL_AUTH` (DEBUGSS_1.0.xml line ~144) names the full list of firewalled APs: SECAPEN, SWDPORTEN, DFTAPEN, ETAPEN, CFGAPEN, AHBAPEN, PWRAPEN (bits 0..6), so AP3 is presumably the DFT-AP or ET-AP [INFER]; the install does not say which.

IDRs: the install does not list any AP IDR values (grep for 0x002e000x finds nothing) [absence]. Decoding by ARM ADIv5 layout [GENERAL]: IDR[27:17]=0x17 is TI's JEP106 code, IDR[3:0] is a TI-defined type: your AP2(SEC)=0, AP1(CFG)=1, AP4(PWR)=2, AP3=3. The low nibble is therefore a per-AP type code, not the AP index, which is why it doesn't "match" the numbering. That mapping is consistent with the roles above but is an inference [INFER]; the files don't state it.

### 1b. Register maps  [FILE]
`PWRAP.xml` (all 32-bit; names/offsets only, no fields):
```
PWRAP_DPREC0 0x00 ... PWRAP_DPREC47 0xBC   (DPREC<n> at 4*n)
PWRAP_SPREC  0xF0
PWRAP_IDR    0xFC
```
`SECAP.xml`:
```
SECAP_TDR 0x00  SECAP_TCR 0x04  SECAP_RDR 0x08  SECAP_RCR 0x0C
```
`cpus/sec_ap.xml` bitfields:
- TRANSMIT_DATA  : TXDAT[31:0]. "Writing this sets the TXDATAVAIL bit in the Transmit Control Register"
- TRANSMIT_CONTROL: CONTROL[31:1] "Device specific control information"; TXDATAVAIL[0] "Set automatically when the TX Data Register is written. Cleared automatically when the system debug logic indicates it has accepted TX Data"
- RECEIVE_DATA   : RXDAT[31:0]. "Reading this clears the RXDATAVAIL bit"
- RECEIVE_CONTROL: CONTROL[31:1]; RXDATAVAIL[0] "Set ... when the system security logic indicates that RX Data Register is valid. Cleared ... when the RX data register is read"

`CFGAP.xml`:
```
CFGAP_IDCODE 0x00  CFGAP_USERCODE 0x04  CFGAP_DBGCR 0x08  CFGAP_SYSSTATCTRL 0x0C  CFGAP_IDR 0xFC
```
(Your 0x10 and 0x14 reads are not described anywhere in the install.)

Device-side view of the SEC-AP mailbox, `DEBUGSS_1.0.xml` [FILE]: DEBUGSS_TXD 0x1100, TXCTL 0x1104 (bit0 TRANSMIT: "Indicates data request in DSSM.TXD, set on write via Debug AP to DSSM.TXD ... A read of the DSSM.TXD register by SW will clear"; bits31:1 "Generic TX flags that can be set by external debug tool"), RXD 0x1108, RXCTL 0x110C (bit0 RECEIVE; bits7:1 RECEIVE_FLAGS "set by SW and read by external debug tool"). DEBUGSS_IIDX/IMASK/RIS: TXIFG, RXIFG, PWRUPIFG ("Power-up interrupt. A debug session has started."), PWRDWNIFG.
DEBUGSS_SPECIAL_AUTH 0x1200 (R): SECAPEN b0, SWDPORTEN b1, DFTAPEN b2, ETAPEN b3, CFGAPEN b4, AHBAPEN b5, PWRAPEN b6. PWRAPEN text: "the debug tools can then access the PWR-AP to power and reset state of the CPU. When deasserted, a DAPBUS firewall will isolate the AP". DEBUGSS_APP_AUTH 0x1210: DBGEN b0, NIDEN b1, SPIDEN b2, SPNIDEN b3.
(SPECIAL_AUTH is the thing that, if cleared by security policy/NONMAIN config, would make an AP vanish behind its firewall. Reading it requires AHB access, so it is not readable while AHB-AP is dead.)

Name semantics (what "DPREC"/"SPREC" stand for) are not given anywhere in the install. Neither is the term "FORCEACTIVE" as a register field; but libprsc.so exports `GRTR_GetForceActiveStatus` and has property strings "PRSC ForceActive Capability Disable" and "PRSC InhibitSleep Capability Disable".

### 1c. DPREC0 bit layout (from libprsc.so)  [DISASM]
How the driver is wired [INFER but fairly solid]: the MSPM0 CS_DAP router is configured `Device Management = 1` ("Power-AP"; `tixds510cs_dap.xml`: choices "Legacy"/"Power-AP", device XML `<property ... Value="1" id="Device Management"/>`), and libprsc.so's generic router functions (`GRTR_*`) are gated on (router type, revision). The branch used for `(type==4, revision==1)` is the Power-AP/CS_DAP one; there is also a `type==7` branch. Which numeric type MSPM0 gets is not stated; I'm assuming type 4/rev 1 because it shares the same capability word 0xF5F1 as type 7 in `GRTR_GetCapabilities` while "legacy" gets 0x55D0.

Every read-modify-write in libprsc uses `RTR_RegisterUpdate{Set,Clear,ReadVerifySet,ReadVerifyClear}` with **mask 0xFF9DFBBF** (read reg, `&= mask`, `|= set`, write, then optionally poll until `(reg & verify)==verify`). The mask zeroes bits 6, 10, 17, 22 on every write-back, i.e. those are never re-written as 1 by CCS unless it wants to: they behave as write-1 / write-1-to-clear style bits [INFER].

Bits, as used by the code (function in parentheses):

| bit | mask | role | evidence |
|---|---|---|---|
| 0, 1 | 0x3 | read only seen as status; **both set** (plus bit 21) = "power loss detected" | `GRTR_GetExtendedRouterStatus2`: `and 2`, `and 1`, `and 0x200000` all required -> field 0x13 |
| 2 | 0x4 | clock status: set -> clockStatus=9, clear -> 8 | `GRTR_GetRouterStatus` `and eax,4`. Also a verify-bit after ForceClockPower |
| 3 | 0x8 | **FORCE ACTIVE** (control). `GRTR_GetForceActiveStatus` reads `and eax,8`; `GRTR_ForceClockPower` sets it; `GRTR_ReleaseClockPower` clears it | disasm 0x17f84, 0x18205, 0x15f36 |
| 5 | 0x20 | **powered-up status** (read-only): set -> powerStatus=3, clear -> 4. Verify bit after ForceClockPower | GetRouterStatus `and 0x20` |
| 6 | 0x40 | cleared on every write (mask) | mask |
| 7 | 0x80 | "powerDownDesired" flag (only decoded in the type7/rev2 branch of ExtendedStatus2) | decode 0x16cfe |
| 4 | 0x10 | "clockDownDesired" flag (same branch) | decode 0x16d28 |
| 10 | 0x400 | exeStatus: set -> 2 else 1 (CPU executing/halted indicator; sense not named). Cleared by mask on write | GetRouterStatus |
| 11-12 | 0x1800 | debug-mode field (used shifted `<<11` for ICEPick types only) | GRTR_EnableDebug (types 2/6) |
| 13 | 0x2000 | **DEBUG ENABLE**: `GRTR_EnableDebug` sets 0x2000 for type 4/7 with ReadVerify; `GRTR_DisableDebug` clears 0x3800 | disasm 0x17321, 0x17488 |
| 14 | 0x4000 | **SET WIR / wait-in-reset** (reset mode field bits[15:14]: 0 -> code 0xA "normal", 1 -> 0xB WIR, 2 -> 0xC block-reset, 3 -> 0xD). `GRTR_SetWIR` sets 0x4000; `GRTR_ClearWIR` clears 0xC000 | disasm 0x17615, 0x177e5 |
| 15 | 0x8000 | **BLOCK RESET** (`GRTR_EnableBlockReset` sets, `GRTR_DisableBlockReset` clears 0xC000) | 0x17b71 |
| 16 | 0x10000 | **HALT ON RESET** (`GRTR_SetHaltOnReset` sets 0x10000 with verify, `ClearHaltOnReset` clears it). Status read back as haltOnResetStatus 0x11 (set) / 0x12 | disasm 0x160c4 |
| 17 | 0x20000 | **RELEASE WIR** pulse (`GRTR_ReleaseWIR` sets 0x20000). When read set: resetStatus = 5 or 6 depending on bit 18 else 7 | 0x179b1, GetRouterStatus |
| 18 | 0x40000 | with bit17 set picks resetStatus 5 vs 6 | GetRouterStatus |
| 21 | 0x200000 | sticky **power-loss-detected** status (see bits 0/1). CCS writes the bit back as 1 after seeing it, i.e. read-and-clear by write-1 [INFER on the clear semantic] | ExtendedStatus2 `or [var],0x200000` then RTR_RegisterWrite |
| 22 | 0x400000 | sticky status; if set with bit 17 clear -> field 0x10 ("reset detected"-like); also written back to clear | ExtendedStatus2 |
| 23 | 0x800000 | decoded only in the type7/rev2 branch ("retention"/"accessible"-like status; write-back clear) | 0x16d50 |
| 19, 20 | 0x80000, 0x100000 | **NOT referenced by any code in libprsc.so**. Only appear in the GEL constant `0x00120008` (bit 20 | bit 17 | bit 3). Meaning not in the install | grep of disassembly |

Combination values CCS actually uses:
- `ForceClockPower` (when sub-path force flag set): `RegisterUpdateReadVerifySet(set=0x8, verify=0x2C)` = set bit 3, then poll until bits 5,3,2 (powered, force-active, clock) are all set. Without the force flag: set=0, verify=0x24 (just wait for bits 5 and 2). [DISASM 0x17f84-0x17fbf]
- `EnableDebug`: set 0x2000, verify 0x2000.
- GEL wait-for-debug: `DPREC0 |= 0x4000` (=SetWIR) then `DPREC0 = 0x00120008`.
- GEL OnReset: `DPREC0 |= 0x00020000` (=ReleaseWIR).

### 1d. Decoding your observed values  [INFER built on 1c]
Your DPREC0 observations:
- 0x00a00003 (asleep): bit5=0 (**not powered**), bit2=0 (**clock off**), bit3=0 (no force-active), bits 1:0=11, bits 21 and 23 set. Bit 21 + bits 1:0 together is exactly CCS's "power loss detected" condition. So: the AHB domain lost power (STANDBY/power-gate); AHB-AP is firewalled/dead. This matches your explanation. Bit 23 is not decoded for this router type by CCS; unknown.
- 0x00080027 (awake after reset): bit5=1 powered, bit2=1 clock on, bits 1:0=11, bit 19 set (unknown/undecoded), bit3=0 (no force active yet, device is awake on its own because it is running ROM/BSL).
- After your 0x8 write: 0x00a8002f = 0x00a00003 | bit19 | bit5 | bit3 | bit2 -> powered, clocked, force-active, but the sticky power-loss bits 21/23 still sitting there (CCS would clear 21 by writing it back).
- 0x00a800bf: as above plus bit 7 and bit 4 = the two "powerDownDesired"/"clockDownDesired" flags (names from the type7/rev2 decoder, so applying them to your chip is [INFER]): the core is asking to sleep and bit 3 is holding it awake.
- 0x00a8042f: bit 10 set = CCS's "exeStatus=2" (sense of 1 vs 2 not named in the files).
- Low bits 0x3/0x27/0x2f: bit 5 and bit 2 are the "powered" and "clock" verify bits; bit 3 is force active; bit 1:0 persistent.
- Your experiment "only bit 20 or only bit 19 did nothing": consistent: nothing in CCS drives those alone. CCS's own full write is 0x00120008 (bits 20, 17, 3), of which only bit 3 (and bit 17, release-WIR) have meaning in code.

"FORCEACTIVE = bit 3" is confirmed by CCS code. There is no separate "inhibit sleep" bit in the code path; the string "InhibitSleep Capability Disable" is a capability-property name, and the code I traced uses bit 3 for both.

---------------------------------------------------------------------------------------------------
## Q2. What CCS does on connect/disconnect to an MSPM0 in low power

### Connect  [DISASM `libprsc.so` PRSC_Connect @0xe37d + GRTR_* functions]
1. `PRSC_Connect`: read PWR-AP DPREC via `PRSC_GetGlobalChildExtendedStatus2` (`GRTR_GetExtendedRouterStatus2`: read DPREC, decode as above, write back status bits 21/22/23 to clear). If status code 0x16 appears it just remembers it.
2. Optional authentication step (`AUTH_K3_Connect` / `AUTH_AJSM_Connect`, chosen by the "Authentication Scheme" property; not relevant to MSPM0 as far as the files go).
3. `PRSC_EnableDebug` (if cap enabled): DPREC |= 0x2000 and poll until set.
4. `PRSC_ForceClockPower` (if cap enabled, properties "PRSC ForceActive Capability Disable"/"InhibitSleep Capability Disable" turn these off): if not already force-active, DPREC |= 0x8 and poll until (DPREC & 0x2C)==0x2C (powered+force active+clock).
5. If any of that fails, undo: `PRSC_DisableDebug` (clear 0x3800), `PRSC_ReleaseClockPower` (clear bit 3).
6. Only then does the CORTEX_M0P / AHB-AP connect proceed; device GEL `OnTargetConnect()` then reads `*(unsigned int*)0x41C40004` (Factory DEVICEID) and checks it (Q5).
So CCS's normal connect already does what you found by hand (set DPREC0 bit 3 and wait for bits 5,2). Your discovery is equivalent to CCS's ForceClockPower.

### When connect still fails  [FILE]
`mspm0l1306.gel` StartUp() / `mspm0_cs_dap_init.gel` StartUp(): `GEL_HandleTargetError` for error codes `-613`, `-2062` (device GEL) and `-613`, `-615` (CS_DAP GEL). Text, verbatim: "This could be caused by the device having gone to low power mode. Try forcing an external reset pressing the 'Force Reset' button. If the error persists, try: 1) Invoking BSL, 2) Calling a DSSM Mass erase, or, 3) Calling a DSSM Factory Reset."
- "Force Reset" button runs `GEL_MSPM0_Board_Reset()` -> `GEL_DAPInit_remotePORReset()` (NRST **1500** units = `RESET_LEN_POR`) in device GEL, or `GEL_DAPInit_MSPM0_Board_Reset()` (NRST **1** unit = `RESET_LEN_BOOTRST`) in the CS_DAP GEL. The time unit of `GEL_SetTimer` is milliseconds per CCS GEL docs [GENERAL], so 1500 ms vs 1 ms.
- Mechanism: `GEL_DAPInit_externalReset()` calls on the SEC_AP target `GEL_AdvancedReset("XDS System Reset (Assert)")` (probe drives NRST low), sets a timer for the pulse length, then `GEL_AdvancedReset("XDS System Reset (De-Assert)")`. For the POR variant it then does GEL_Disconnect()/GEL_Connect() on the CS_DAP (so the whole connect sequence above reruns).

### Differences for STOP / STANDBY / SHUTDOWN
**The install contains no distinct handling for STOP vs STANDBY vs SHUTDOWN.** There is no per-mode code or text. Only the generic "may be operating in low-power mode" flow in the libs, and the -613/-2062/-615 handlers. (SHUTDOWN loses the debug domain / state and wakes through a reset-like path [GENERAL, TRM], which is why NRST/POR is the suggested remedy, but CCS does not say that.) libtixds510cs_dap.so also carries generic strings "Device may be operating in low-power mode. Do you want to bring it out of this mode? Choose 'Yes' to force the device to wake up and retry the operation..." (offsets 0x1d9d8/0x1dab0), same for cortexm0/sec_ap libs.

### Disconnect  [DISASM PRSC_DisConnect @0xe9f8]
Calls `PRSC_ReleaseClockPower` -> clear DPREC0 bit 3 (force-active) (guarded by a capability/"keep powered" flag; the property string "Keep Debug Powered on Disconnect" exists in libtixds510cs_dap.so, offset 0x1c590). I did not see a DisableDebug call in the disconnect path (only in the connect error-cleanup). So after a normal disconnect the chip is free to sleep again: that is how a device ends up "asleep" with 0x00a00003.

---------------------------------------------------------------------------------------------------
## Q3. SEC-AP / DSSM mailbox: commands, handshake, resets  [FILE: mspm0_cs_dap_init.gel]

Command codes (lines 36-40, 51-53):
```
DSSM_BC_FACTORY_RESET 0x020A     DSSM_BC_MASS_ERASE 0x020C     DSSM_BC_PW_AUTH 0x030E
DSSM_DATA_EXCHANGE    0x00EE     DSSM_WAIT_FOR_DEBUG 0x0206
Responses (RDR>>8):   DSSM_CMD_RECEIVED 0x0100   DSSM_CMD_NOT_RECEIVED 0x0101   DSSM_ERROR_UNEXPECTED_COMMAND 0x0102
Data-phase ack:       (RDR>>8) == 0x200 after the 4 password words
Masks: SECAP_CTL_MASK 0xFFFF, SECAP_CMD_MASK 0x00FE; reset pulse RESET_LEN_POR 1500, RESET_LEN_BOOTRST 1
```
Handshake (`GEL_DAPInit_remoteSECAPCommand` -> `GEL_DAPInit_SECAPCommand`, lines 361-451):
1. Connect CS_DAP if not connected, then connect SEC_AP (AP2).
2. `SECAP_TCR = command; SECAP_TDR = 0;`  (TDR write sets TXDATAVAIL).
3. Wait 1000 (ms, [GENERAL] timer unit) via GEL timer.
4. If "auto" variant: `GEL_DAPInit_MSPM0_Board_Reset()` = NRST pulse of 1 (BOOTRST length), then SEC_AP GEL_Disconnect + GEL_Connect. If "manual": prints "Press the reset button..." and the user pulses NRST during the wait.
   **So the command is posted first and the reset after; the boot ROM executes the mailbox command during the next boot.**
5. `GEL_DAPInit_WaitForResponse`: spin until `SECAP_RCR & 0xFFFF` has bit0 (RXDATAVAIL) set; rxCmd = RCR & 0xFE; resp = `SECAP_RDR`.
6. Success iff rxCmd == (cmd & 0xFE) and (resp>>8)==0x100. For password auth (data length 4): send 4 words (`SECAP_TDR = word; SECAP_TCR = 0x00EE;` waiting for TCR bit0 clear between words), wait for response, require (resp>>8)==0x200.
7. Mass erase / factory reset: GEL then says "Please terminate debug session, power-cycle and restart debug session."
Password words come from device properties `MSPM0SWDPassword0..3` (MSPM0L1306.xml; "Enable SWD Password" `EnableSWDPassword`).
Wait-for-debug (0x0206): "Device is now in reset mode." (`MSPM0_Mailbox_WaitForDebug_Auto/Manual`).

CCS UI wiring: `MSPM0_FlashProperties.xml` buttons: "Mass erase manual/auto" -> `MSPM0_MailboxMassErase_Manual/Auto()`; "Factory reset manual/auto" -> `MSPM0_Mailbox_FactoryReset_Manual/Auto()` (device GEL lines 275-319); password auth: `MSPM0_Mailbox_PasswordAuthentication_Auto()` (GEL_Connect afterwards). The "Auto" variants first GEL_Disconnect the main CPU, "Manual" assume you press reset.

What the files do NOT say: what the ROM does internally for factory reset vs mass erase (factory reset resets NONMAIN to defaults + mass erase per TI TRM [GENERAL]), nor whether SECAP is reachable when DEBUGSS_SPECIAL_AUTH.SECAPEN=0 (device-side doc says SEC-AP "firewall"; SWD must be permitted).
Relevant for you: SEC-AP is AP2 and has a different firewall bit (SECAPEN) from AHB/PWR; it stays reachable regardless of low power since the response goes via DEBUGSS in AON domain [INFER]. You already saw AP2 respond.

---------------------------------------------------------------------------------------------------
## Q4. Blank device / BSL / NRST timing

What the install says [FILE]:
- `MSPM0_FlashProperties.xml`: "Enable wait for debug (Warning: **Do not enable if flash is empty**)", default 0.
- GEL error text lists "1) Invoking BSL, 2) DSSM Mass erase, 3) DSSM Factory Reset" as recovery for the low-power-induced connect error. No further BSL description; no BSL pin name, no timeout, no "10 s".
- NRST naming: `RESET_LEN_POR (1500)` vs `RESET_LEN_BOOTRST (1)` and messages "Initiating POR Board Reset" / "Initiating BOOTRST Board Reset". So CCS uses ~1.5 s low for a POR-class reset and ~1 ms low for a BOOTRST-class reset (units ms per GEL docs [GENERAL]).
- Wait-for-debug on connect (`OnPreTargetConnect`/`OnPreFileLoaded` when property enabled; `mspm0l1306.gel` 208-214, 331-351): 10x { `PWRAP_SPREC |= 0x1; PWRAP_DPREC0 |= 0x4000; PWRAP_SPREC |= 0x1;` }, a spin delay of 10000, then `PWRAP_DPREC0 = 0x00120008`. DPREC0 bit 14 is "SetWIR" (wait-in-reset) per libprsc. SPREC bit 0 is not decoded by libprsc (the only SPREC user, `GRTR_GlobalStart`, sets bit 11 and clears bit 9 for type 7), so SPREC bit 0's meaning is **not in the install**. There is also `MSPM0_ResetCtl_WaitForDebug_NoNRST` hotmenu doing the same without NRST ("WFD alternative if user does not have nRST connected": GEL_Connect then the same 10x writes and "Device is now in the reset handler.").
- `OnReset()`: `DPREC0 |= 0x00020000` (release WIR). `OnRestart()`: GEL_Halt, same, then `GEL_AdvancedReset("System Reset", 1)`.

NOT in the install: the BSL 10 s inactivity timeout, ROM behaviour with blank flash, ROM entry into STANDBY, the BSL-invoke pin. Your interpretation (blank flash -> ROM runs BSL -> times out -> low power -> AHB powered down) is plausible and consistent with the status bits (power-loss bit 21 + bits 1:0 set, bit 5/2 clear), and CCS's own warning against wait-for-debug on empty flash plus its "try invoking BSL" hint points the same way, but nothing in the install confirms the timeout or the ROM's blank-device flow. Public TI TRM/BSL user guide would be the source [GENERAL]: BSL invoke is a GPIO pin at boot (BSLPIN) / NONMAIN BCR/BSL config; NRST low longer than about 1 s causes POR vs a short pulse causing BOOTRST (this matches CCS's 1500/1 split, but the exact threshold is from memory, not verified here).

---------------------------------------------------------------------------------------------------
## Q5. Decoding the AP1 (CFG-AP) values

[FILE] `mspm0l1306.gel` lines 180-203, 231-249: `gGEL_DeviceID = *(unsigned int *)(0x41C40004)` (FACTORY region); `GEL_isMSPM0L11XX_L13XX()` is `(gGEL_DeviceID & 0xFFFFFFE) == 0x0BB8202E`; `GEL_isMSPM0Rev0()` is `(id & 0xF0000000) == 0` ("preliminary silicon"/early samples, warns).
Family compare values in the install's other GELs: L11xx/L13xx = 0x0BB8202E, L122x/L222x (L1227/L1228..) = 0x0BB9F02E, G1x0x/G3x0x = 0x0BB8802E, G15x/G35x(1518..) = 0x0BBA902E, C110x = 0x0BBA102E.

Your AP1 [0x00] = 0x1bb8202f. Under the GEL mask (ignore bit31:28 and bit0): (0x1bb8202f & 0x0FFFFFFE) = 0x0BB8202E, which **matches the L11xx/L13xx family value** (your chip is an L1306, consistent). Bits [31:28] = 0x1 -> version/revision nibble 1 (not 0, so not an "early sample" by CCS's rev0 test). Bit 0 = 1 as in a standard JTAG IDCODE. Standard IEEE-1149.1 reading [GENERAL]: [11:1] = 0x017 = TI's JEP106 code, [27:12] = 0xBB820 part number. The install does not state that CFGAP_IDCODE (AP1 offset 0) is the same word as FACTORY DEVICEID at 0x41C40004; the identical layout/value strongly suggests so, but that equivalence is my inference. The install has no table mapping 0xBB820 to "L1306" vs "L1305/L1304/..." (the GEL treats all L11xx/L13xx identically; the specific part would come from the factory region's part-number fields, not given here).
AP1 [0x04] = 0x8152bb70 (CFGAP_USERCODE): not described in the install. [0x08] DBGCR=0x400000d8, [0x0C] SYSSTATCTRL (you didn't list it), [0x10]=0x0004110a and [0x14]=0x00000096: offsets 0x10/0x14 aren't defined in CFGAP.xml, no decode available.

---------------------------------------------------------------------------------------------------
## Practical read-out for the recovery path (from the above)
- CCS's own routine for "AHB-AP FAULT while DP is up" = PWR-AP DPREC0: set bit 3 and wait for (DPREC0 & 0x2C)==0x2C; optionally set bit 13 (0x2000, debug enable) first. You already reproduced that.
- To stop a firmware/ROM from sleeping before it can be debugged: wait-in-reset: DPREC0 |= 0x4000 (bit 14), pulse NRST, then connect, then release with bit 17 (0x20000). CCS also writes 0x00120008 after this (bits 20|17|3). (This is the "Wait for debug" flow. CCS warns against it with an empty flash.) Halt-on-reset is separate: bit 16.
- DSSM mailbox recovery sequence is in Q3: post command through SEC-AP (AP2) TCR/TDR, issue NRST (1 ms BOOTRST-style pulse in "auto"), poll RCR bit0, check RDR>>8==0x100. After mass erase/factory reset power-cycle.
