# Porting the device bootloader to a new MCU

A port implements exactly one contract — the nine functions in
`device/include/updater/port.h` — plus a `main()` loop with the
obligations below. The proven core (`device/core/`) is C11 with zero MCU
headers; it never addresses the boot region, and it calls the flash
functions only with in-range pages (proven). The port is the one layer
the proofs cannot reach, which is why every port ships a `PORT_AUDIT.md`
(§4).

Reference ports: `device/ports/avr_ea_twi` (transactional, I2C),
`device/ports/avr_ea_uart` (stream, hardware UART),
`device/ports/rp2350_uart` (stream, cmake lane, flash-geometry mapping).
The behavioral reference loop is pinned by the conformance suite:
`conformance/sim/sim_port.c`.

## 1. The nine port functions

Read `port.h` itself — it is written as the contract document. Summary:

| Function | Called by | Obligation |
|---|---|---|
| `port_info` | core (`upd_init`) | fill geometry (protocol page size ≤ 250, app_pages) + identity |
| `port_flash_erase_page` | core (ERASE_APP) | erase one protocol page; called ascending, `page < app_pages` (proven); multi-page erase units act on the first page of each unit and no-op the rest |
| `port_flash_write_page` | core (WRITE_PAGE) | program exactly `page_size` bytes; MUST tolerate repeat writes of identical data (host retries; ECC/write-once parts must compare-and-skip or buffer — never fault) |
| `port_flash_read_byte` | core (VERIFY, boot gate) | return what a boot would see — flush any write-coalescing buffer first |
| `port_recv` | your main loop | transactional transports only: whole frame or `false`; never blocks. Stream ports return `false` and use `link.h` instead |
| `port_send` | your main loop | transactional transports only: emit one response frame (≤ 20 bytes) |
| `port_ticks_ms` | your main loop | free-running milliseconds; wrap is fine; gates only the entry window |
| `port_entry_requested` | your main loop, once at startup | did the app request entry (magic+complement pair, `app_stub.h`)? MUST clear the pair |
| `port_jump_to_app` | core (`upd_boot_if_valid`, sole call site) | transfer control; never returns |

Execution model: everything runs in ONE polled context. Nothing is
called from an ISR; no implementation may depend on interrupts being
enabled.

Transports: implement `port_recv`/`port_send` for transaction-shaped
wires (I2C), or pump bytes through a `link_t`
(`device/include/updater/link.h`, backed by `device/core/link_stream.c`)
for byte streams (UART/SPI/softuart) — one path or the other, never
both. Pump skeletons: `device/ports/skeletons/`.

## 2. Main-loop obligations (checklist)

Mirrors the header-top block of `device/include/updater/update.h`; the
reference loops are `ports/avr_ea_twi/main.c`, `ports/avr_ea_uart/main.c`
and the pinned `conformance/sim/sim_port.c`.

- [ ] **BOOT ordering:** reply first; jump only once the reply has fully
      left the wire — transport-specific (TWI: `twi_response_consumed()`;
      UART: `uart_tx_drain()`). An I2C client cannot push; jumping
      earlier kills the response.
- [ ] **Clear `s->boot_pending` yourself** before calling
      `upd_boot_if_valid`; the core only sets it.
- [ ] `upd_boot_if_valid` **re-validates flash**; the stale
      `boot_pending` flag is never trusted, so a refused jump is safe to
      ignore.
- [ ] **Resident latch:** ANY CRC-valid frame cancels the autoboot — set
      your resident flag whenever `upd_handle` runs. Unparseable input
      does NOT cancel it (answer it `ST_BAD_FRAME` on transactional
      wires; streams drop it silently).
- [ ] **T_ENTRY window:** while not resident, once `port_ticks_ms()`
      reaches your `UPDATER_T_ENTRY_MS` (300 ms in shipped ports),
      attempt `upd_boot_if_valid`.
- [ ] **Rescue residency:** if that attempt refuses (no valid app),
      latch resident so the device stays reachable for rescue flashing.
- [ ] **Buffers from geometry:** RX `page_size + 8 + 3` bytes; response
      payload `UPD_RSP_MAX`; TX `UPD_RSP_MAX + UPD_FRAME_OVERHEAD`.
      Never size from the 252/255 codec ceilings or the sim's 255 cap.
- [ ] **Jump hygiene:** hand the app a reset-equivalent machine — quiesce
      every peripheral you touched, mask/clear pending interrupt state
      the app could inherit (see the RP2350 port's NVIC scrub and the
      TWI port's client-off for what this means in practice).

## 3. Install path

The bootloader itself is installed once per board via a debug adapter
(`updater-cli install`, templates in `install.toml`). A new port adds a
`[targets.<name>]` entry: `tool`, argv template with `{image}`/`{port}`
placeholders, optional `pre` steps (e.g. a fuse write). Protect the
bootloader region by hardware means where the part offers them (AVR-EA:
BOOTSIZE fuse — the bootloader physically cannot rewrite itself; RP2350:
the link against `memmap_bootloader.ld` fails if the image outgrows its
64 KiB region).

## 4. PORT_AUDIT.md (required)

Every port ships a `PORT_AUDIT.md`: the compensating control for the
unproven layer. Required sections, in the shape of the shipped audits
(`avr_ea_twi/PORT_AUDIT.md`, `rp2350_uart/PORT_AUDIT.md`):

```markdown
# <port> Port Audit

Citation key: <datasheet/SDK editions, exact versions, header files>

## Symbol verification method
How every register/bit spelling was resolved against the real toolchain.

## <each source file> — one table per file
| # | Access | Why | Citation | Risk if wrong |
Every register access gets a row: what it does, why it must be there,
the datasheet/SDK citation (section number, not vibes), and what breaks
if it is wrong.

## Disassembly checks
Behavior-critical sequences confirmed in the built ELF (e.g. protected-
write key sequences, the jump-gate instruction sequence).

## Must be checked at hardware bring-up
Numbered list of everything proof and compile cannot reach: clock/fuse
premises, bus-stretch tolerances of the intended host, errata review,
re-entry pair end-to-end, erase-duration vs host poll budget.
```

Flag port assumptions that would not survive a part swap (e.g. the
RP2350 audit's F3: plain-NOR multi-pass programming — re-audit before an
ECC'd flash part).

## 5. Gates a port must pass

- Builds warning-clean under `-Wall -Wextra -Werror`.
- A hard size gate: AVR Makefiles check against the 4 KiB boot section;
  the RP2350 port's linker script makes the link itself the gate.
- AVR-class ports with startup-code capture (`.init3`) run the
  Makefiles' objdump gate proving the capture is push/call/ret-free.
- The core it links is the proven one — do not fork `device/core/`.
- `make test` and `make prove` at the repo root stay green (the port
  adds no code to either lane, but the shared headers are in both).
