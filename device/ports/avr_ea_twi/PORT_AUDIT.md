# AVR-EA/TWI Port Audit

The port is the one layer the Frama-C/CBMC proofs cannot reach. This file
is the compensating control: every register access, why it is there, its
citation, and what breaks if it is wrong.

**Citation key:** `§n` = "AVR64EA28/32/48 Preliminary Data Sheet",
Microchip DS40002443A. `hdr` = `ioavr64ea28.h` shipped with avr-gcc 14.3.0
(`/usr/lib/avr/include/avr/ioavr64ea28.h`). `ex` = Microchip's validated
AVR64EA48 example `avr64ea48-nvm-read-while-write-studio` (main.c).
`rc` = proven-in-service robocup client `ir_sensor_board/src/twi_slave.c`.

## Symbol verification method

1. Every register/bit/group symbol used by the port was resolved by the
   real compiler: `avr-gcc -mmcu=avr64ea28 -fsyntax-only` over a file
   referencing all 40+ symbols (addresses and masks) — compile succeeds,
   so every spelling exists in `ioavr64ea28.h` as used.
2. Numeric values were cross-read from the header (`grep` of the
   definitions) and, where behavior-critical, re-confirmed in the built
   ELF's disassembly (column "disasm check" below).

## ../avr_ea_common/flash.c — NVMCTRL (§11)

*(Task 13: flash.c and entry.c moved verbatim to `../avr_ea_common/` for
sharing with the avr_ea_uart port — only flash.c's `#include "port_cfg.h"`
became `#include "port_geom.h"`, the geometry-only header split out of this
port's port_cfg.h. Zero register accesses changed; both images rebuilt at
the identical 2614 bytes.)*

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| F1 | `_PROTECTED_WRITE_SPM(NVMCTRL.CTRLA, cmd)` | CTRLA is CCP-protected with the **SPM** key; unlock must reach CTRLA within 4 instructions | §11.3.2.4 steps 2–3, Table 11-7 (`CTRLA → SPM`); hdr `CCP_SPM_gc = 0x9D` | Command write silently ignored → no erase/write ever happens; disasm check: `ldi r17,0x9D; out 0x34; sts 0x1000` |
| F2 | `while (NVMCTRL.STATUS & (NVMCTRL_FLBUSY_bm\|NVMCTRL_EEBUSY_bm))` | §11.3.2.4 step 1 requires confirming **both** busy flags before a new command | §11.3.2.4; hdr `FLBUSY_bm=0x02, EEBUSY_bm=0x01` | Command collision → STATUS.ERROR = CMDCOLLISION, op dropped; disasm: `lds 0x1006; andi 0x03` |
| F3 | `_PROTECTED_WRITE(NVMCTRL.CTRLB, FLMAP…)` | Selects which 32 KiB flash block appears in the 0x8000 data-space window. **CTRLB is CCP-protected with the IOREG key (found during this audit — a plain write is silently ignored)** | Table 11-7 (`CTRLB → IOREG`), §11.3.2 "Addressing Flash in CPU Data Space", §11.5.2; corroborated by avr-libc crt's own `__do_flmap_init` using key 0xD8 in the disassembly | Window never moves → app bytes at flash ≥ 0x8000 (pages ≥ 224) read/write the wrong block; VERIFY passes against wrong data or bricks the image |
| F4 | `flmap_restore()` after every op | crt `__do_flmap_init` sets FLMAP for `.rodata` addressing (CTRLB reset 0x30 = top section); the rest of the system must never see a moved window | §11.5.2 (reset value); crt disassembly | Any later `.rodata`/const access reads the wrong flash block |
| F5 | erase: `NOCMD → dummy store 0xFF → FLPER → wait → NOCMD` | EA order is **buffer-store first, command second** (reverse of AVR-DA). One byte must be in the page buffer for FLPER to act; dummy 0xFF because buffer loads AND with prior content, 0xFF is neutral | §11.3.2.3 Option 2 steps 1–3, §11.3.2.4.2, §11.3.2.2; `ex` does exactly this | Erase targets a stale address or never starts; brief's original DA-style order (command→store) would be wrong on EA |
| F6 | write: `NOCMD → fill 128 bytes via window → FLPW → wait → NOCMD` | §11.3.2.3 Option 2 steps 4–5; ST through the window loads the page buffer, FLPW programs it. Buffer is auto-erased after every page op/reset (§11.3.2.2 list), so it is blank before each fill | §11.3.2.2, §11.3.2.4.1; `ex` | Torn/ANDed data programmed; page corrupt (caught by VERIFY, but wastes the part's endurance) |
| F7 | `NOCMD` between commands | "A change from one command to another must always go through NOCMD/NOOP" | §11.3.2.4 (ERROR = CMDCOLLISION otherwise) | Ops silently rejected |
| F8 | read: `LD` via window | §11.3.2.1: LD with data-space address; bus waits if an op is in flight | §11.3.2.1 | Wrong data → false VERIFY results |
| F9 | `_Static_assert(PAGE_SIZE == PROGMEM_PAGE_SIZE)` etc. | Geometry pinned to the header at compile time | hdr `PROGMEM_PAGE_SIZE=128, PROGMEM_SIZE=65536, MAPPED_PROGMEM_START=0x8000, MAPPED_PROGMEM_SIZE=32768` | — (build fails instead) |

## twi.c — TWI0 client, polled (§27)

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| T1 | `TWI0.SADDR = addr<<1` | Match address lives in SADDR[7:1]; bit 0 = general-call recognition, kept off | §27.3.2.3.1, §27.5.13; `rc`; disasm: 0x20 / 0x40 for the two variants | Device answers wrong address / answers general call |
| T2 | `TWI0.SSTATUS = DIF\|APIF\|BUSERR\|COLL` at init | W1C: drop stale flags from before reset | §27.5.12 (flags cleared by writing '1'); `rc` | Ghost event serviced on first poll |
| T3 | `TWI0.SCTRLA = PIEN\|ENABLE` | **PIEN is required even when polling**: without it a Stop never sets APIF (§27.5.10 APIEN note 2) and frames would never be delivered. DIEN/APIEN deliberately 0: they only gate interrupt generation (flag AND enable AND SREG.I) — flags themselves are set by hardware and are polled; SREG.I additionally stays 0 for the bootloader's whole life | §27.5.10 (DIEN/APIEN/PIEN text), §27.5.12 | No STOP detection → RX frames never complete; disasm: SCTRLA = 0x21 |
| T4 | poll `TWI0.SSTATUS` | Single point of event decode: BUSERR/COLL, APIF+AP (address), APIF (stop), DIF+DIR | §27.5.12; `rc` decode order | — |
| T5 | `SSTATUS = BUSERR\|COLL` on error | Hardware already aborted; clear W1C flags, drop half-latched rx/tx state, do NOT fabricate a Stop event | §27.5.12; `rc` teardown (verbatim semantics) | Next transaction mis-sequenced |
| T6 | address match, parked frame/response pending: **no SCMD written** | Leaving APIF pending keeps CLKHOLD asserted → client stretches SCL until main.c catches up; this is the spec's "device clock-stretches until the response is ready" and the only timing mechanism in the port | §27.5.12 CLKHOLD ("set when an address or data interrupt occurs"); design spec §I2C mapping | Response raced/overwritten, or host reads garbage mid-computation |
| T7 | `SCTRLB = TWI_SCMD_RESPONSE_gc` | ACK address / ACK received byte / release byte just loaded into SDATA; ACKACT bits left at ACK (reset) | §27.5.11 (SCMD RESPONSE 0x3), §27.3.2.3.1–.3; `rc` | Bus hangs (no ACK ever sent) |
| T8 | `SCTRLB = TWI_SCMD_COMPTRANS_gc` on Stop / host NACK | Complete transaction, return client FSM to idle, clears flags | §27.5.11 (COMPTRANS 0x2); `rc` | Client wedged mid-transaction |
| T9 | first TX byte loaded on first **DIF**, not at address match | EA quirk: SDATA written during the APIF phase is not shifted out (`tx_first` bridges address→first DIF); on that first DIF, RXACK is stale and must be ignored | `rc` (documented in-service quirk); §27.3.2.3.3 | First response byte lost / duplicated, or a stale NACK aborts the response before byte 1 |
| T10 | `TWI_RXACK_bm` after a data byte | Host NACK = end of read → COMPTRANS; host ACK = load next byte | §27.5.12 RXACK; `rc` TX_DONE_NACK path | Client keeps driving SDA after host is done → bus error |
| T11 | `TWI0.SDATA` read (RX) / write (TX) | Reading/writing SDATA clears DIF; SCMD RESPONSE then releases the stretch | §27.5.12 (DIF clear methods), §27.5.14 | — |
| T12 | TX past response end sends `0xFF` | Host padded-read contract: host reads a fixed length and discards the tail; filler continues indefinitely | design spec §I2C mapping; task directive | Host read of N > response length would wedge or bus-error |
| T13 | RX overflow (>136): bytes swallowed, frame dropped at Stop, still ACKed | Wire cap = geometry (page_size+8 per spec allowance; largest legal frame is 133), NOT the sim's 255 reference-core cap. ACK-and-drop instead of mid-write NACK because some host adapters error out on it; the dropped frame simply gets no response → host times out and retries | conformance/sim/sim_port.h note; port_cfg.h | Oversized garbage could smash the 136-byte buffer (memory safety of the unproven layer) |

## ../avr_ea_common/entry.c / app_stub.h — re-entry pair (§8.4, §14)

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| E1 | pair at `INTERNAL_SRAM_END-3 … END` = 0x7FFC–0x7FFF | Fixed top-of-SRAM addresses shared by pointer cast on both sides; no linker coordination. SRAM = 0x6800–0x7FFF (6 KiB) | §8.2 Fig 8-1, §8.4; hdr `INTERNAL_SRAM_END = 0x7FFF` (verified: `INTERNAL_SRAM_START 0x6800 + 6144 - 1`) | Pair lands outside RAM or on live data; re-entry never triggers |
| E2 | capture in `.init3`, `naked`, no push/call/ret | The stack starts at RAMEND (§8.4 "program stack is located at the end of SRAM"; crt sets SP=0x7FFF) — the first CALL pushes over 0x7FFE/0x7FFF. Capture must run before any push; `naked` prevents an epilogue RET with no return address on the stack | §8.4; **disasm check passed**: `updater_entry_capture` at 0x00A0 is pure `lds/cpi/sbci/breq/ldi/and/sts`, zero push/call/ret, reads all 4 bytes before clearing | Complement word destroyed by crt's `call main` → app-requested entry silently never works |
| E3 | flag stored in `.noinit` | `.init3` runs before `.init4` clears `.bss`; a normal static would be re-zeroed | avr-libc init section order (observed in disasm: capture at 0xA0 precedes `__do_clear_bss` at 0xEC) | Entry request lost every time |
| E4 | pair always cleared | A stale pair must not re-trigger on the next reset; power-on garbage can't fake it (complement check) | design spec §Entry model | Boot loop into the bootloader |
| E5 | stub: `cli` before writing pair | An interrupt push between pair-write and reset lands on the pair | §8.4 | Corrupted pair → re-entry lost |
| E6 | stub: `_PROTECTED_WRITE(RSTCTRL.SWRR, RSTCTRL_SWRE_bm)` | SWRR is CCP-IOREG protected; SWRE=1 resets immediately. NOTE: datasheet prose says "SWRST bit", but the EA register field and header spelling is **SWRE** (bit 0) | §14.3.2.1.5, §14.5.2 ("Bit 0 – SWRE … a software Reset will occur"), Table 14-2 (SWRR → IOREG); hdr `RSTCTRL_SWRE_bm 0x01` | Write ignored → no reset, app hangs in the stub loop |

## main.c — clock, tick, jump (§12, §23, §15)

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| M1 | `_PROTECTED_WRITE(CLKCTRL.MCLKCTRLB, 0)` | MCLKCTRLB resets to 0x11 (PEN=1, DIV6); writing 0 clears PEN → CLK_PER = CLK_MAIN = OSCHF. OSCHF frequency is **fuse-selected** on EA (FUSE.OSCCFG.OSCHFFRQ, factory default 20 MHz) — not a runtime FRQSEL like AVR-Dx. MCLKCTRLB is CCP-IOREG | §12.5.2 (reset 0x11, PEN bit, CCP property); hdr `FUSE_OSCHFFRQ`, `OSCHFFRQ_20M_gc = 0`; same one-liner as `rc` clock_init | 3.33 MHz instead of 20 → tick 6x slow (entry window 1.8 s; still functional, but out of spec) |
| M2 | `TCB0.CCMP = 19999; CTRLB = CNTMODE_INT; CTRLA = CLKSEL_DIV1\|ENABLE` | Periodic Interrupt mode counts to CCMP, sets CAPT, restarts — polled, INTCTRL stays 0 (no ISR exists) | §23.3.3.1.1, §23.3.2 init steps; disasm: CCMP bytes 0x1F,0x4E | Tick wrong → entry window wrong (bounded harm: window only) |
| M3 | `TCB0.INTFLAGS` poll + W1C `TCB_CAPT_bm` | CAPT is the 1 ms event; write-1-to-clear | §23.3.3.1.1; hdr `TCB_CAPT_bm 0x01` | Tick freezes → device stays in bootloader forever (recoverable, host can still flash) |
| M4 | jump: `cli` → `TWI0.SCTRLA = 0` → `TCB0.CTRLA = 0` → CAPT cleared | No bootloader peripheral state (enabled client on the app's bus address, running timer, pending flags) may leak into the app | §27.5.10 ENABLE; the gate itself is core-proven (single call site) | App sees a phantom TWI client at the bootloader address |
| M5 | `((void (*)(void))(UPDATER_APP_BASE/2))()` | avr-gcc function-pointer values are **word** addresses; ICALL with Z=0x0800 reaches byte 0x1000, the app's reset vector. App vectors also live at 0x1000: CPUINT.CTRLA IVSEL reset = 0 = "vectors directly after the BOOT section" | §15.5.1 IVSEL (value 0), §15.3.2.2; **disasm check passed**: `ldi r30,0x00; ldi r31,0x08; icall` | Jump to byte 0x800 (mid-bootloader) or 0x2000 — crash |

## Cross-cutting checks

- **Zero protocol logic in the port**: `grep -E 'CRC|UPD_CMD|UPD_ST|LEN' twi.c ../avr_ea_common/flash.c ../avr_ea_common/entry.c` → only the byte-movement comment in twi.c (re-run after the Task 13 move; also fixes the escaped-pipe grep record from the Task 10 review). Frame parse/build/status decisions are core calls made by main.c, mirroring `conformance/sim/sim_port.c` (the pinned reference main loop).
- **0xFF filler**: `twi.c` TX path emits `0xFF` for every DIF beyond `tx_len`, with no count limit — matches the host's fixed-length padded read (T12).
- **RX buffer 136**: `UPDATER_RX_BUF_SIZE = 136` = page_size + 8 (spec allowance; largest legal frame 133); deliberately not the sim's 255 (reference-core cap, not a wire guarantee).
- **`-DNDEBUG`**: the core's `assert()`s restate proof obligations already discharged by Frama-C/CBMC; avr-libc's assert would pull `abort()` into the 4 KiB image.
- **Size gate**: `text+data = 2590 / 4096` bytes for both `firmware_0x10.hex` and `firmware_0x20.hex` (avr-size -B, gate enforced in the Makefile — build fails over budget). `[re-verified 2026-07-06: 2614 at the Task-13 move; 2590 since the Task-16 seam-wave header changes — re-measured on today's rebuild]`
- **.init3 check automated (Task 13, from the Task 10 review)**: the Makefile now disassembles `updater_entry_capture` on every build (following `.L*` local labels to the function's true end) and fails on any push/pop/call/rcall/icall/ret/reti or on a missing symbol — E2's one-time disasm audit is a standing gate.

## Must be checked at hardware bring-up (proof/compile cannot reach these)

1. **Polled-flag premise (T3)**: confirm APIF/DIF are set with DIEN/APIEN=0 on real silicon (datasheet wording supports it; the in-service reference `rc` ran interrupt-mode, so this exact configuration is untested on hardware).
2. **FUSE.OSCCFG** at 20 MHz factory default (M1) and **BOOTSIZE fuse = 8** before first flash (`-U bootsize:w:8:m`); with BOOTSIZE unprogrammed the whole flash is BOOT and the app can never be written by the bootloader.
3. **ERASE_APP stretch**: 480 page erases happen inside one request; the host's next read sees one long SCL stretch (seconds). Verify the chosen host adapter (RPi I2C) tolerates it or the host driver retries.
4. **Silicon errata**: review the AVR64EA erratum sheet for the purchased date code (the AVR32EA sibling has published NVMCTRL/TWI errata, DS80001091) before trusting the first flash cycle.
5. **Re-entry pair end-to-end** (E2): stub-reset from a test app and confirm the bootloader stays resident.

## Re-verification record (2026-07-06)

Every row above was re-verified against the current tree and today's
rebuilt ELFs (both variants rebuilt clean: text+data 2590/4096, size gate
+ .init3 gate pass). Row → current source location:

| Row | Current source (file:line) | Status |
|---|---|---|
| F1 | ../avr_ea_common/flash.c:36-39 | verified; disasm re-run: `ldi r17,0x9D` present at the protected-write site |
| F2 | ../avr_ea_common/flash.c:43-46 | verified; disasm re-run: `lds 0x1006; andi 0x03` |
| F3 | ../avr_ea_common/flash.c:57-67 | verified; IOREG key `ldi 0xD8` present at CTRLB write sites |
| F4 | ../avr_ea_common/flash.c:69-72 (called at :87, :105, :114) | verified |
| F5 | ../avr_ea_common/flash.c:74-88 | verified |
| F6 | ../avr_ea_common/flash.c:90-106 | verified |
| F7 | ../avr_ea_common/flash.c:78, :86, :94, :104 | verified |
| F8 | ../avr_ea_common/flash.c:108-116 | verified |
| F9 | ../avr_ea_common/flash.c:17-21; ../avr_ea_common/port_geom.h:23-27 | verified; header values re-grepped in hdr |
| T1 | twi.c:44-48 | verified; disasm re-run: the two variants' ELFs differ in exactly one instruction, `ldi 0x20` vs `ldi 0x40` |
| T2 | twi.c:49 | verified; disasm: `ldi 0xCC` |
| T3 | twi.c:50 | verified; disasm: `ldi 0x21` |
| T4 | twi.c:63-157 | verified |
| T5 | twi.c:67-79 | verified |
| T6 | twi.c:94-99 | verified |
| T7 | twi.c:112, :142, :154 | verified |
| T8 | twi.c:125, :132-135 | verified |
| T9 | twi.c:35-38, :100-104, :131-138 | verified |
| T10 | twi.c:132-135 | verified |
| T11 | twi.c:141, :144 | verified |
| T12 | twi.c:139-141 | verified |
| T13 | twi.c:27, :146-152; ../avr_ea_common/port_geom.h:33-39 | verified |
| E1 | ../../include/updater/app_stub.h:39-42 (same-MCU constraint :19-22) | verified; hdr `INTERNAL_SRAM_END` re-checked |
| E2 | ../avr_ea_common/entry.c:20-30; gate: Makefile (.init3 objdump check) | verified; today's `build/firmware_0x10.elf.init3.dis`: capture at 0x00A0, pure lds/cpi/sbci/breq/ldi/and/sts, zero push/call/ret |
| E3 | ../avr_ea_common/entry.c:8-10 | verified; `__do_clear_bss` at 0xEC in today's disasm, after the 0xA0 capture |
| E4 | ../avr_ea_common/entry.c:27-29 | verified |
| E5 | ../../include/updater/app_stub.h:46-50 | verified |
| E6 | ../../include/updater/app_stub.h:51-53 | verified; hdr `RSTCTRL_SWRE_bm 0x01` re-checked |
| M1 | main.c:53-59 | verified (cited by avr_ea_uart/port_cfg.h:19 as "audit row M1") |
| M2 | main.c:18-23 | verified; disasm: `ldi 0x1F; ldi 0x4E` (19999) |
| M3 | main.c:25-34 | verified (M2/M3 cited by avr_ea_uart/main.c:20) |
| M4 | main.c:37-44 | verified |
| M5 | main.c:45-50 | verified; disasm re-run: `ldi r30,0x00; ldi r31,0x08; icall` |

