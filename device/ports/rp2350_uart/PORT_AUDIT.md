# RP2350 (Pico 2 W) / UART0 Port Audit

The port is the one layer the Frama-C/CBMC proofs cannot reach. This file
is the compensating control: every pico-sdk/hardware access, why it is
there, its citation, and what breaks if it is wrong.

**Citation key:** `sdk:<path>` = pico-sdk **2.1.1** (tag `2.1.1`, commit
`bddd20f` "SDK 2.1.1 Release", cloned to `~/pico-sdk`), paths relative to
the SDK root. The SDK's generated `hardware/regs/` headers embed the
RP2350 datasheet register descriptions verbatim and are cited as the
register-level authority; `uart-audit` = `../avr_ea_uart/PORT_AUDIT.md`
(policy rows referenced by ID). Toolchain: arm-none-eabi-gcc 14.2.1,
cmake + ninja; board `pico2_w`, platform `rp2350-arm-s` (Arm Secure,
Cortex-M33).

## Scope and reuse

Second CPU family, same architecture of trust: only `flash.c`, `uart.c`,
`entry.c` and `main.c` touch hardware. `uart_pump.c` and `link_stream.c`
are the same portable translation units as in avr_ea_uart, covered by the
host test lane and (for link_stream) the bounded proof harness; the core
(`update.c`, `proto.c`, `crc32.c`, `crc8.c`) is compiled unmodified. The
transport chain is `PL011 registers → uart.c ops → uart_pump →
link_stream → core`. The policy rows of uart-audit that are pure protocol
policy (no ST_BAD_FRAME on stream transports; reply-then-jump; unread RX
error flags) carry over verbatim and are marked so below.

## Symbol verification method

Every SDK function, macro and register struct member used by the port was
resolved by the real cross-compiler during the build (`-Wall -Wextra
-Werror` on the port's own files), and the behavior-critical sequences
were re-confirmed in the linked ELF's disassembly (column "disasm check").
SDK function semantics were verified by reading the 2.1.1 sources at the
cited paths, not from docs alone.

## Geometry (port_cfg.h)

| What | Value | Why / citation |
|------|-------|----------------|
| Bootloader region | 64 KiB at flash base (XIP 0x10000000) | `memmap_bootloader.ld` pins `FLASH LENGTH = 64k` — the **link fails** if the image outgrows the region (built image: 7424 B text `[re-verified 2026-07-06; ~9.3 KiB before the Task-17 alarm-pool removal, 7432 B at that fix]`). XIP_BASE per `sdk:src/rp2350/hardware_regs/include/hardware/regs/addressmap.h` |
| App region | 1 MiB at flash offset 0x10000 (`UPDATER_APP_BASE` 0x10010000) | The XIP address is port-internal (reads/jump); `flash_range_*` take the flash offset — both derived from the single `UPDATER_APP_FLASH_OFFSET`. INFO carries only page_size/app_pages, never a base address |
| page_size / app_pages | 128 / 8192 | protocol page = ½ × `FLASH_PAGE_SIZE` (256) and 1/32 × `FLASH_SECTOR_SIZE` (4096), `sdk:src/rp2_common/hardware_flash/include/hardware/flash.h`; `_Static_assert`s in flash.c pin all three ratios and the sector alignment of the region |
| Device | 4 MiB QSPI flash (W25Q32RV class) on `pico2_w` | `sdk:src/boards/include/boards/pico2_w.h` (`PICO_FLASH_SIZE_BYTES`, `boot2_w25q080`); flash above the app region is never addressed — the core's proven confinement plus the port's fixed offsets |

## flash.c — hardware_flash over the ROM routines

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| F1 | `flash_range_erase(offs, FLASH_SECTOR_SIZE)` — only when `page % 32 == 0` | ERASE_APP mapping: the core calls `port_flash_erase_page` for **every** protocol page 0..8191; the call starting a 4 KiB sector erases it, the other 31 are no-ops → 256 sector erases covering the 1 MiB exactly. `flash_range_erase` requires sector-aligned offset/count (`invalid_params_if` on `FLASH_SECTOR_SIZE`) | `sdk:src/rp2_common/hardware_flash/flash.c` (`flash_range_erase`), `flash.h` (alignment contract) | Misaligned call = SDK assert/undefined; skipping a sector leaves stale data that fails VERIFY |
| F2 | `flash_range_program(offs, unit, FLASH_PAGE_SIZE)` — always 256 bytes, 256-aligned | Coalescing holdback: an even protocol page is held in RAM; its odd partner completes a 256-byte flash page (host writes ascending). Any other next event flushes the held half padded with 0xFF. `flash_range_program` requires `FLASH_PAGE_SIZE` alignment of offset and count | `sdk:src/rp2_common/hardware_flash/flash.c` (`flash_range_program`), `flash.h` | Misalignment = SDK assert; unflushed holdback would make VERIFY read 0xFF where data belongs (prevented by F5) |
| F3 | 0xFF padding + possible re-program of the same 256-byte page | NOR programming only clears bits (erased = 0xFF), so an all-0xFF half-page program is write-neutral and a host **retry** re-programming identical data changes nothing. Plain W25Q-class NOR (no ECC restrictions on multi-pass page program). With an ascending host, every page is in fact programmed exactly once; correctness does not depend on it | W25Q datasheet family behavior (AND-programming), noted as a **port assumption**; holdback logic in `flash.c` | On an ECC'd flash part multi-pass programming could be illegal — re-audit F3 before reusing this file for such a part |
| F4 | `save_and_disable_interrupts()` / `restore_interrupts_from_disabled()` around every flash op | `flash.h` warns the range functions are unsafe if interrupt handlers/vector table live in flash. This bootloader never enables an IRQ and never starts core 1 (it sleeps in the ROM), but the wrapper makes the invariant local instead of global. **The "never enables an IRQ" leg is true only together with U9**: as first audited this row leaned on pico_stdlib defaults, and that was falsified — `pico_stdlib → pico_time` enabled the alarm NVIC IRQ with a flash-resident handler at runtime init until `PICO_TIME_DEFAULT_ALARM_POOL_DISABLED=1` was added (Task-17 fix; CMakeLists cites "PORT_AUDIT U8/U9, F4") `[corrected — Task-17 fix; re-verified 2026-07-06]` | `sdk:src/rp2_common/hardware_flash/include/hardware/flash.h` (safety note), `sdk:src/rp2_common/hardware_sync/include/hardware/sync.h`; `sdk:src/common/pico_time/time.c` (`runtime_init_default_alarm_pool`) | An IRQ fetching from flash mid-program = hard fault/hang with XIP down |
| F5 | `hold_flush()` at the top of `port_flash_read_byte` | Held data is not yet in flash; VERIFY/INFO/BOOT walk the region through this single read path, so flushing here makes holdback invisible to the core | `flash.c` (single read path by construction — `grep` shows no other XIP read of the app region) | VERIFY would CRC 0xFF instead of the held page → spurious BAD_CRC (or worse, a stale-pass) |
| F6 | XIP read `*(const volatile uint8_t *)(UPDATER_APP_BASE + offset)` | Reads go through the XIP window; both range functions call the ROM's `flash_flush_cache` before returning ("needed to remove CSn IO force as well as cache flushing"), so post-write reads cannot see stale cache | `sdk:src/rp2_common/hardware_flash/flash.c` (`flash_flush_cache_func()` on every path) | Stale XIP cache → VERIFY validates old bytes |
| F7 | RAM-function discipline: port code stays in flash | While the QSPI device is being programmed, XIP is unavailable — nothing may fetch from flash. `flash_range_erase/program` and every helper on their path are `__no_inline_not_in_flash_func` (RAM-resident) and call only ROM routines; they restore XIP before returning. Our caller frames (in flash) are safe: the CPU is inside the RAM/ROM functions for the whole XIP-down window, and F4 guarantees no asynchronous re-entry into flash code | `sdk:src/rp2_common/hardware_flash/flash.c` (every helper marked), `sdk:src/rp2_common/pico_platform_sections/include/pico/platform/sections.h` (`__no_inline_not_in_flash_func` = `__noinline __not_in_flash_func`) | A flash-resident helper on the call path = fetch from a disabled XIP → lockup mid-erase |
| F7b | flash_range_program source buffer read by ROM while XIP is down | data must be RAM-resident | satisfied by construction: static .bss hold[]/unit[] (flash.c) | passing a flash-resident const buffer would fault mid-program |
| F8 | `port_flash_erase_page` drops the holdback (`hold_valid = false`) | ERASE_APP invalidates any held page; dropping (not flushing) is correct because the erase that follows would destroy it anyway | `flash.c` | Flushing instead could program into a sector about to be erased — harmless but misleading; keeping it could resurrect pre-erase data |

## uart.c — PL011 UART0, polled

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| U1 | `uart_init(uart0, UPDATER_UART_BAUD)` | Resets/unresets the PL011 block, programs the fractional baud divisor from `clk_peri` (returns the achieved rate), sets **8N1 + FIFOs enabled** in one LCR_H write, then enables `UARTEN|TXE|RXE`. 8N1 is the SDK-wide frame format. Divisor from the live `clock_get_hz(clk_peri)` — no fuse-premise fragility like the AVR port's fixed 20 MHz (uart-audit U1 note) | `sdk:src/rp2_common/hardware_uart/uart.c` (`uart_init`: reset, `uart_set_baudrate`, inlined `uart_set_format`, CR write) | Wrong rate/format → framing garbage; host sees nothing |
| U2 | `gpio_set_function(0/1, GPIO_FUNC_UART)` **after** `uart_init` | UART0 default pinout on Pico boards: GPIO0 = TX, GPIO1 = RX. Routing after enable means the PL011 already drives the idle mark when the pad connects — the host never sees a low glitch that could latch as a start bit (same concern as uart-audit U3's OUT-before-DIR) | `sdk:src/boards/include/boards/pico2_w.h` (`PICO_DEFAULT_UART_TX_PIN 0`, `RX_PIN 1`), `sdk:src/rp2_common/hardware_gpio/include/hardware/gpio.h` | TX never driven, or a boot-time glitch injects a phantom byte |
| U3 | `uart_is_readable(uart0)` (rx_ready) | `FR.RXFE` clear = byte available RIGHT NOW; false once the RX FIFO drains — exactly the pump's "drain until false" contract | `sdk:src/rp2_common/hardware_uart/include/hardware/uart.h` (`uart_is_readable`, FR.RXFE) | Stuck-true hangs link_poll; stuck-false deafens the port |
| U4 | `(uint8_t)uart_get_hw(uart0)->dr` (rx_read) | DR read pops the RX FIFO; bits [11:8] are the per-byte OE/BE/PE/FE flags, deliberately dropped by the cast: a damaged byte fails the frame CRC, link_stream drops the frame, host timeout+retry recovers (uart-audit U7 policy). PL011 needs no flag clearing to keep receiving | `sdk:src/rp2350/hardware_structs/include/hardware/structs/uart.h` (UARTDR layout) | Treating DR as 8-bit-clean is exactly the CRC-covers-it policy; consuming without RXFE check would read garbage (prevented by pump contract: read only after ready) |
| U5 | `uart_is_writable(uart0)` (tx_ready) | `FR.TXFF` clear = TX FIFO has room; comes true within one byte time as the shifter drains — makes the pump's timeout-less wait bounded (32-deep FIFO, responses ≤ 20 bytes, so in practice it never waits) | `sdk:src/rp2_common/hardware_uart/include/hardware/uart.h` (`uart_is_writable`) | TX wedge if misread; overwritten bytes if ignored |
| U6 | `uart_get_hw(uart0)->dr = b` (tx_write) | Hand the byte to the FIFO; called only after U5 per the ops contract | same as U4 | — |
| U7 | `uart_tx_wait_blocking(uart0)` (updater_uart_tx_drain) | Gates the BOOT jump: polls `FR.BUSY`, which covers the transmit **shift register** including stop bits, not just FIFO empty — the PL011 analog of the AVR TXCIF wait (uart-audit U9/U10); no sticky-flag clearing dance is needed because BUSY is level, not latched | `sdk:src/rp2_common/hardware_uart/include/hardware/uart.h` (`uart_tx_wait_blocking` polls `UART_UARTFR_BUSY_BITS`) | Jump with bytes still shifting → host loses the BOOT ACK |
| U8 | interrupts: none enabled anywhere | `UARTIMSC` stays at reset 0 (uart_init leaves it; we never touch it), PRIMASK never cleared inside the bootloader, and **no NVIC line is enabled by anything linked in — guaranteed by U9, NOT by pico_stdlib defaults**. As first written this row claimed the default link pulled in nothing that enables an IRQ; that was FALSIFIED: `pico_stdlib → pico_time` registered the default alarm pool at runtime init, claiming a hardware alarm and enabling its NVIC IRQ with a flash-resident handler. FR flags are set by hardware regardless — pure polling, same discipline as both AVR ports `[corrected — Task-17 fix; re-verified 2026-07-06]` | `sdk:src/rp2_common/hardware_uart/uart.c` (no IMSC write in init path); `sdk:src/common/pico_time/time.c:79-106` (`runtime_init_default_alarm_pool` — what U9 disables) | The F4 flash-op safety argument silently loses its no-IRQ leg (F4's wrapper still saves it — that is why the wrapper exists) |
| U9 | `PICO_TIME_DEFAULT_ALARM_POOL_DISABLED=1` — CMakeLists.txt (bootloader target ONLY) | Keeps pico_time from claiming a hardware alarm and enabling its NVIC IRQ at runtime init; the bootloader only reads the timebase (M1) and must run IRQ-free. `blink` (and any real app) keeps the pool for `sleep_ms`. Macro name verified in sdk time.h. Evidence it works: `nm bootloader.elf` contains NO alarm symbol (no `alarm_pool_irq_handler`, no `__pre_init_runtime_init_default_alarm_pool`; both present at the pre-fix baseline) — re-verified 2026-07-06. Cited by CMakeLists.txt:70-76 ("PORT_AUDIT U8/U9, F4") and main.c:54-57 ("see CMakeLists/U9") `[row added in the Task-17 fix]` | `sdk:src/common/pico_time/include/pico/time.h:313-314`; `sdk:src/common/pico_time/time.c:79-106` | An SDK update silently reintroducing the pool breaks the U8/F4 premise — re-check the nm evidence on every SDK bump |

## entry.c — watchdog-scratch re-entry pair

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| E1 | `watchdog_hw->scratch[2]` / `[3]` as magic + one's-complement pair | "Scratch register. Information persists through soft reset of the chip." — survives the app's `watchdog_reboot`, cleared only by power-on (and POR garbage cannot fake the pair: the complement check rejects it). Shared definition with the app via `updater/app_stub.h` (single source of truth, same as AVR) | `sdk:src/rp2350/hardware_regs/include/hardware/regs/watchdog.h` (WATCHDOG_SCRATCH2/3 description) | Pair not surviving = app-requested entry silently becomes a plain reboot (T_ENTRY window still works) |
| E2 | Why SCRATCH2/3 and not others | The SDK and the ROM's watchdog boot vectoring assign meaning to SCRATCH4..7 (`0xb007c0d3` magic protocol, `WATCHDOG_NON_REBOOT_MAGIC`); SCRATCH0..7 are otherwise free and the SDK writes **no** other scratch register (verified by grep over sdk src) | `sdk:src/rp2_common/hardware_watchdog/watchdog.c` (`watchdog_reboot`, `watchdog_enable` — the only scratch writers) | Colliding with the ROM's vectoring registers could redirect the reboot itself |
| E3 | Capture + clear as `main()`'s first statement — no `.init3`-style early hook | Unlike AVR, the pair is in peripheral registers, not under the stack: no push can destroy it, and crt0/`runtime_init` touch no watchdog scratch register. Always cleared so a stale pair cannot re-trigger | `sdk:src/rp2_common/pico_crt0/crt0.S`, `sdk:src/rp2_common/pico_runtime/runtime.c` (no scratch access) | Not clearing = every subsequent reset re-enters resident mode |
| E4 | App side: `watchdog_reboot(0, 0, 0)` (app_stub.h) | `pc = 0` selects "reboot into regular flash path" — the ROM re-picks the image at the XIP base, i.e. this bootloader; delay 0 fires `WATCHDOG_CTRL_TRIGGER` immediately | `sdk:src/rp2_common/hardware_watchdog/watchdog.c` (`watchdog_reboot`, `scratch[4] = 0` path; `_watchdog_enable` TRIGGER) | A pc≠0 vectored reboot would bypass the bootloader entirely |

## main.c — tick, loop, jump (M rows)

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| M1 | `to_ms_since_boot(get_absolute_time())` truncated to `uint16_t` | 1 ms tick for the entry window only. The 64-bit µs timebase runs after crt0's `runtime_init` (clock + tick-generator setup); nothing is claimed, nothing to de-init at the jump. Truncation wraps at 65.536 s with the same modular semantics as the AVR ports' software counter | `sdk:src/common/pico_time/include/pico/time.h`, `sdk:src/rp2_common/pico_runtime/runtime.c` (runtime_init chain) | Wrong tick → entry window too short/long; nothing else consumes it |
| M2 | Loop shape: link_poll → upd_handle → link_send; `resident` on any valid frame; BOOT = reply, drain, `upd_boot_if_valid` | Identical to avr_ea_uart/main.c (reference behavior pinned by the conformance sim), including the documented stream divergence: no ST_BAD_FRAME reply, link_poll only surfaces CRC-valid frames | uart-audit "main.c" section; `conformance/sim/sim_port.c` | Divergence from the sim contract = conformance drift |
| M3 | `uart_deinit(uart0)` in the jump gate | No bootloader UART state may carry into the app: puts the PL011 back into reset. GPIO0/1 keep their UART funcsel so the host's RX line sees the pad's pull-up rather than a float across the handoff (uart-audit U11 rationale); the app reconfigures its pins like any other | `sdk:src/rp2_common/hardware_uart/uart.c` (`uart_deinit` → `uart_reset`) | App inherits a live UART claiming the pins |
| M4 | `cpsid i`, then **NVIC scrub**: all-ones to every `nvic_icer[]` (0xE000E180+, disable) and `nvic_icpr[]` (0xE000E280+, clear-pending) bank, then `__dsb(); __isb()` — main.c:64-70 | Reset-equivalent handoff: the ROM enters a flash image with no NVIC line enabled or pending, and the app must see the same. The bootloader itself enables no IRQ (U9), **but the gate must not depend on that**. RP2350 has 52 external IRQs (`NUM_IRQS`) → 2 banks, matching `m33_hw->nvic_icer[2]`/`nvic_icpr[2]`. `cpsid i` masks the transition itself (same defensive stance as the AVR ports' `cli`) `[rewritten — Task-17 fix ("M3" in task-17-report.md); re-verified 2026-07-06]` | `sdk:src/rp2350/hardware_regs/include/hardware/platform_defs.h:25` (`NUM_IRQS` 52); `sdk:src/rp2350/hardware_structs/include/hardware/structs/m33.h:499` (`nvic_icer[2]`); **disasm check passed (2026-07-06)**: `cpsid i` → `str.w 0xFFFFFFFF` to 0xE000E180/0xE000E280/0xE000E184/0xE000E284 → `dsb sy; isb sy` | A line left enabled or pending fires the instant M6 unmasks — vectoring through the app's table into half-torn bootloader state |
| M5 | `m33_hw->vtor = app_vt; __dsb(); __isb();` BEFORE touching SP — main.c:72-84 | Exceptions must resolve through the app's table from here on; barriers order the VTOR write against the subsequent stack switch and branch per ARMv8-M requirements on vector-table moves `[re-verified 2026-07-06 against the rewritten gate]` | `sdk:src/rp2350/hardware_structs/include/hardware/structs/m33.h` (`m33_hw->vtor`, M33_VTOR); `sdk:src/rp2_common/hardware_sync/include/hardware/sync.h` (`__dsb`/`__isb`); **disasm check passed (2026-07-06)**: `str.w r2,[r3,#0xd08]` (VTOR = 0xE000ED08) + `dsb sy; isb sy` in `port_jump_to_app` | Fault during the handoff would vector through the bootloader's table with the app's SP — undebuggable crash |
| M6 | One asm block, in order: `movs r3,#0; msr MSPLIM,r3; msr MSP,vt[0]; cpsie i; bx vt[1]` — main.c:86-106 | ARMv8-M stack limit cleared before moving MSP (an active guard above the app's stack would fault instantly; crt0 precedent "Make sure stack limit is 0"). After MSP moves, the function must not touch its own stack — hence a single asm block ending in `bx`. **`cpsie i` AFTER the new MSP is loaded, LAST before `bx`**: the ROM enters flash images with PRIMASK=0 and pico-sdk crt0 never executes `cpsie` (verified: zero `cpsie` in the built app ELF), so a PRIMASK=1 handoff left every BOOT-entered app permanently unable to take IRQs — `sleep_ms`/WFE hung forever; invisible on silicon only while the validation app busy-polled. Unmasking here is safe: M4 guarantees no line enabled/pending, M5 already points VTOR at the app. Entered exactly like the ROM enters a flash image: SP = word 0, reset handler = word 1 `[rewritten — Task-17 fix ("M6" in task-17-report.md); re-verified 2026-07-06]` | `sdk:src/rp2_common/pico_crt0/crt0.S:447` (msplim clear; `.vectors` layout: word 0 = stack top, word 1 = `_reset_handler`); **disasm check passed (2026-07-06)**: `movs r3,#0; msr MSPLIM,r3; msr MSP,r1; cpsie i; bx r0` | The pre-fix defect this rewrite fixed: every app entered via BOOT ran IRQ-dead. Garbage SP/PC = immediate hard fault; MSPLIM above the app stack = instant STKOF fault |
| M7 | `updater_entry_capture()` before any other SDK call in `main` — main.c:111-113 | Ordering guarantee for E3 | entry.c | — |

## IMAGE_DEF and the app-image contract

The **bootloader** is an ordinary pico-sdk flash binary: crt0 embeds the
PICOBIN IMAGE_DEF block the RP2350 ROM requires in `.embedded_block`
(`sdk:src/rp2_common/pico_crt0/crt0.S`; verified with `picotool info -a
build/bootloader.uf2`: "Metadata Block 1 ... block type: image def, ARM
Secure" at 0x10000138). The ROM therefore picks and enters the bootloader
on every boot; the flash/QSPI/XIP setup it performs stays valid for the
whole bootloader life and across the jump.

The **app** images this SDK flashes are NOT picked by the ROM — the
bootloader enters them directly (M5/M6). Apps are plain vector-table
binaries linked at the app region. For a pico-sdk app the entire contract
is one line against this port's linker script:

```cmake
pico_set_linker_script(<app> ${CMAKE_CURRENT_LIST_DIR}/memmap_app.ld)
```

(`memmap_app.ld` = SDK `memmap_default.ld` with `FLASH ORIGIN =
0x10010000, LENGTH = 1024k - 16`; the 16 bytes are the boot-gate footer
the **host** appends — the app never emits it.) The flashable artifact is
the **`.bin`**, not the `.uf2`/`.elf`:
`updater-cli --transport uart flash build/blink.bin`. `app/blink.c` in
this port is the minimal reference app (GPIO25 toggle + UART0 heartbeat +
`'U'` → `updater_reboot_to_bootloader()`), built as the `blink` target by
the same CMakeLists. Note: on the Pico 2 **W** the onboard LED is on the
CYW43 radio, not GPIO25 — the heartbeat line on UART0 is the intended
observable; GPIO25 still toggles for a scope. Since the Task-17 fix,
blink's 500 ms wait deliberately uses `sleep_ms` (alarm IRQ + WFE), not a
busy-poll: with PRIMASK stuck across the jump the app would hang before
the first heartbeat, so a visible heartbeat is falsifiable on-silicon
evidence of the M6 reset-equivalent handoff (app/blink.c header).

## Bring-up notes

1. **A silent USB bus is SUCCESS.** This bootloader has no USB stack.
   After the UF2 install the BOOTSEL drive disappears and *nothing*
   re-enumerates; the device is listening on UART0 (GPIO0 TX / GPIO1 RX,
   115200 8N1 by default). Recovery is always available: hold BOOTSEL
   while plugging USB and the ROM's drive returns regardless of what is
   in flash.
2. **ERASE_APP is slow and the host must wait.** One command triggers 256
   × 4 KiB sector erases (~45 ms typical, up to ~400 ms worst-case each →
   roughly 12–100 s total). Give the CLI a generous poll budget, e.g.
   `--poll-attempts 12000 --poll-delay-ms 10` (120 s ceiling); the default
   100 × 10 ms budget WILL time out on ERASE_APP. (Observed live erases
   completed in ~3 s — about 4x faster than W25Q typical: both ran over
   mostly-0xFF, already-erased regions where erase-verify exits early. Do
   NOT size poll budgets from that observation; the first genuinely dirty
   1 MiB erase pays the full 12–100 s envelope.) The CLI additionally
   auto-scales the erase exchange budget from INFO geometry
   (app_pages × 10 ms — host/updater-cli/src/main.rs
   `PER_PAGE_ERASE_WORST_MS`, which cites this note); an explicit
   `--poll-attempts` wins when larger.
3. **Baud is a build parameter** (`-DUPDATER_UART_BAUD=...`), derived at
   runtime from `clk_peri` — no clock-fuse premise to re-verify per board
   (contrast: AVR bring-up item 1).
4. **Wiring for the host cycle:** 3V3-level UART only. Adapter TX →
   GPIO1 (Pico pin 2), adapter RX → GPIO0 (Pico pin 1), GND → GND (e.g.
   pin 3); Pico powered via its own USB.
5. **Multi-image flash layout caution:** anything above XIP+0x110000
   (bootloader 64 KiB + app 1 MiB) is outside the updater's world and is
   never erased or written by it.

6. **"App valid: no after cold power-on" is NOT a POR read bug — checked
   on silicon, 2026-07-06** (full evidence chain:
   `.superpowers/sdd/task-17-por-bug.md`). A live device in exactly that
   post-POR state was probed read-only through the existing protocol,
   using VERIFY as a flash-read oracle (host-computed CRC32 of candidate
   prefixes; per-byte content recovery by 256-candidate CRC probes).
   Result: `port_flash_read_byte`'s plain XIP reads were **byte-perfect
   across the entire verifiable app region (offsets 0..region−17)** in
   the post-POR state — the region simply *contained* a partially
   flashed image (ERASE_APP + 73 pages written, footer never written)
   from a flash session interrupted by the power pull itself, so
   `upd_app_valid` correctly returned false. Every XIP/QMI hypothesis is
   ruled out empirically, and by source: the RP2350 ROM performs flash/
   XIP setup before entering any flash image (memmap_default.ld note:
   "the bootrom performs a simple best-effort XIP setup"), and the
   post-program/erase restore path the bootloader relies on is the ROM's
   generic mode anyway — `flash_enable_xip_via_boot2()` on RP2350 calls
   `ROM_FUNC_FLASH_ENTER_CMD_XIP` ("Set up XIP for 03h read on bus
   access (slow but generic)", sdk:src/rp2_common/hardware_flash/
   flash.c). No boot-path-dependent read state exists for this port; no
   XIP init code is needed (or added) in the bootloader. Diagnosis rule:
   before suspecting the read path, VERIFY(6300-byte-app length,
   host CRC32 of the .bin) — if that returns OK, reads are fine and the
   footer/content is what differs. Recovery from a torn flash needs no
   BOOTSEL: reflash the app through the resident bootloader.

7. **One reader per tty.** The protocol has exactly one response per
   request; any second process reading /dev/ttyACM0 (a `tio` viewer, a
   second CLI/agent session) steals response bytes at random. Under
   contention, commands time out — and worse, a reassembled frame can
   pass CRC8 by chance and deliver a *wrong-looking but valid* response
   (observed on silicon 2026-07-06: a contended `info` reported
   "app valid: no" moments after a successful reflash; a single-reader
   heartbeat check proved the app valid and running). Close viewers and
   serialize sessions before trusting any reading.

## Re-verification record (2026-07-06)

Every row above was re-verified against the current tree, ~/pico-sdk
(2.1.1, `bddd20f`) and the current `build/bootloader.elf`
(text 7424 B ≤ 64 KiB link-enforced gate). Row → current source location:

| Row | Current source (file:line) | Status |
|---|---|---|
| Geometry | port_cfg.h:23-59; memmap_bootloader.ld:34; memmap_app.ld:37 | verified (`FLASH ORIGIN 0x10000000 LENGTH 64k`; `ORIGIN 0x10010000 LENGTH 1024k - 16`) |
| F1 | flash.c:110-121 | verified; sdk flash.c:114 re-read |
| F2 | flash.c:83-97, 123-145 | verified; sdk flash.c:151 re-read |
| F3 | flash.c:15-21, 83-97 | verified (cited by device/include/updater/port.h:88 and docs/PORTING.md:117) |
| F4 | flash.c:94-96, 118-120 | verified; corrected this pass to carry the U9 dependency (see row) |
| F5 | flash.c:147-157 (`hold_flush()` then read) | verified |
| F6 | flash.c:151-157 | verified; `flash_flush_cache_func` on every path, sdk flash.c:123-161 re-read |
| F7 | flash.c:33-42 (header contract) + sdk sources | verified; `__no_inline_not_in_flash_func` end-to-end re-read 2026-07-06 |
| F7b | flash.c:76, 86 (static `hold[]`/`unit[]` in .bss) | verified |
| F8 | flash.c:110-113 (`hold_valid = false` before the sector test) | verified |
| U1 | uart.c:19-26 | verified; sdk uart.c `uart_init` re-read |
| U2 | uart.c:28-32 | verified; pico2_w.h:31,34 re-checked (TX 0 / RX 1) |
| U3 | uart.c:37-45 | verified |
| U4 | uart.c:47-57 | verified |
| U5 | uart.c:59-66 | verified |
| U6 | uart.c:68-72 | verified |
| U7 | uart.c:78-86 | verified |
| U8 | uart.c:4-9 (premise block) | verified as corrected (see row) |
| U9 | CMakeLists.txt:70-81 | verified; macro re-checked in sdk time.h:313-314; nm evidence re-run (no alarm symbols) |
| E1 | entry.c:29-37; app_stub.h:61-64 | verified; watchdog.h:112,122 quote re-checked |
| E2 | entry.c:12-15; app_stub.h:29-32 | verified |
| E3 | entry.c:17-21, 31-41; main.c:111-113 | verified |
| E4 | app_stub.h:66-77 | verified |
| M1 | main.c:24-34 | verified |
| M2 | main.c:131-157 | verified |
| M3 | main.c:44-50 (`uart_deinit`) | verified |
| M4 | main.c:52-70 | verified as rewritten; disasm re-run (NVIC scrub stores) |
| M5 | main.c:72-84 | verified; disasm re-run (VTOR 0xE000ED08 + barriers) |
| M6 | main.c:86-106 | verified as rewritten; disasm re-run (`msr MSPLIM; msr MSP; cpsie i; bx`) |
| M7 | main.c:109-113 | verified |
| IMAGE_DEF section | memmap_app.ld:1-11; CMakeLists.txt:98-111; app/blink.c:1-25 | verified against sources; the picotool block address (0x10000138, ARM Secure) is a Task-17 record from the shipped UF2 — not re-run this pass (requires picotool; UNVERIFIED-HISTORICAL at the address level, structurally implied by crt0 `.embedded_block`) |
| Bring-up notes 1-7 | install.toml:45-57 (note 1); main.rs:31-39, :526 (note 2, which they cite); port_cfg.h:63-69 (note 3); task-17-report.md (note 4 wiring); port_cfg.h:13-17 (note 5); task-17-por-bug.md + sdk flash.c:65 re-read (note 6); task-17-por-bug.md §Independent re-verification (note 7) | verified; note 5's original wording is lost — substance per the recovery fragments (UNVERIFIED-HISTORICAL at the wording level) |
