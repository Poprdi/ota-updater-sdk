# AVR-EA/USART0 Port Audit

The port is the one layer the Frama-C/CBMC proofs cannot reach. This file
is the compensating control: every register access, why it is there, its
citation, and what breaks if it is wrong.

**Citation key:** `§n` = "AVR64EA28/32/48 Preliminary Data Sheet",
Microchip DS40002443A. `hdr` = `ioavr64ea28.h` shipped with avr-gcc 14.3.0
(`/usr/lib/avr/include/avr/ioavr64ea28.h`). `twi-audit` =
`../avr_ea_twi/PORT_AUDIT.md` (rows referenced by their IDs there).

## Scope and reuse

This port shares `../avr_ea_common/flash.c`, `../avr_ea_common/entry.c`
and `../avr_ea_common/port_geom.h` with the TWI port — their register
accesses are audited **once**, in twi-audit rows F1–F9 and E1–E6, which
apply verbatim here (identical translation units, identical flags). The
`.init3` no-push/call/ret condition (twi-audit E2) is enforced by this
Makefile on every build, same as the TWI one.

The transport chain is `USART0 registers → uart.c ops → uart_pump →
link_stream → core`. Only `uart.c` and this port's `main.c` touch
hardware; `uart_pump.c` and `link_stream.c` are portable C covered by the
host test lane (`device/test/test_pumps.c`, `test_link.c`) and, for
link_stream, the bounded proof harness (`device/proofs/harness_link.c`).

## Symbol verification method

Same as twi-audit: every register/bit/group symbol used by the port was
resolved by the real compiler (`avr-gcc -mmcu=avr64ea28 -fsyntax-only`
over a file referencing all of them — addresses and masks); numeric values
cross-read from `hdr` and, where behavior-critical, re-confirmed in the
built ELF's disassembly (column "disasm check" below).

## uart.c — USART0, polled (§25)

| # | Access | Why | Citation | Risk if wrong |
|---|--------|-----|----------|---------------|
| U1 | `USART0.BAUD = 694` (115200) / computed per build parameter | Fractional baud generator: `BAUD = 64·f_CLK_PER/(S·f_BAUD)` rounded, S=16 Normal / 8 CLK2X; register holds the divisor <<6 (6 fractional bits), valid 64–65535. At 20 MHz/115200: 694 → true rate 115 274 Bd, **+0.06%** error (fractional bits are why no U2X juggling is needed, unlike classic UBRR math). CLK2X selected at compile time only when the Normal-mode value would drop below 64 (i.e. above f_CLK_PER/16 = 1.25 MBd); out-of-range rates fail `_Static_assert` | §25.3.2.2.1, Table 25-1 (equations + `USART.BAUD ≥ 64` condition); **disasm check passed**: `ldi 0xB6/0x02` (694) at 115200, `ldi 0x6D/0x05` (1389) at 57600, `sts 0x0808` = USART0.BAUD (hdr offset 0x08) | Wrong bit clock → every byte framing-garbage; host sees nothing or noise. NOTE: value scales from the 20 MHz fuse premise — see bring-up item 1 |
| U2 | `USART0.CTRLC = CMODE_ASYNC \| PMODE_DISABLED \| SBMODE_1BIT \| CHSIZE_8BIT` | 8N1, the frame format the whole SDK speaks; written before enable per the init sequence | §25.3.1 (init order), §25.5.8; hdr group values | Host/device frame-format mismatch → permanent garbage |
| U3 | `PORTA.OUTSET = PIN0_bm` then `DIRSET` | §25.3.1 step 3: "Configure the TXD pin as an output". TXD = PA0 on the default route (PORTMUX.USARTROUTEA reset 0x00 = USART0 DEFAULT: TxD PA0, RxD PA1 — no PORTMUX write needed; RXD PA1 stays a reset-state input). OUT set high **before** DIR so the pin never drives a low glitch the host could latch as a start bit | §25.3.1; §17.3.3 + hdr `PORTMUX_USART0_DEFAULT_gc = 0x00`; §16 (PORT OUTSET/DIRSET W1S semantics) | TXD never driven (host RX floats) or a boot-time glitch injects a phantom byte |
| U4 | `USART0.CTRLA = 0` | Restates the reset value: all interrupt enables off. SREG.I also stays 0 for the bootloader's whole life — flags below are polled, and polling needs no enable bits (unlike the TWI PIEN quirk, twi-audit T3, USART flags have no enable-gated *setting* behavior) | §25.5.6 | — (defensive only) |
| U5 | `USART0.CTRLB = RXEN \| TXEN \| RXMODE` | Enable both directions; RXMODE carries the Normal/CLK2X decision paired with U1's S value — the two must agree or the rate is off by exactly 2x | §25.5.7; hdr `RXMODE_NORMAL_gc/CLK2X_gc` | RX or TX dead; mismatched RXMODE = half/double rate |
| U6 | poll `USART0.STATUS & USART_RXCIF_bm` (rx_ready) | RXCIF = unread data in the receive buffer; self-clears when the buffer is emptied by reading RXDATAL — exactly the pump's "drain until false" contract, no W1C handling needed | §25.5.5 (RXCIF text) | Stuck-true would hang link_poll; stuck-false deafens the port |
| U7 | `USART0.RXDATAL` read (rx_read) | Pops the 2-level receive FIFO. RXDATAH error flags (FERR/BUFOVF/PERR) are **deliberately never read**: a damaged/lost byte fails the frame CRC, link_stream drops the frame, host timeout+retry recovers (link.h policy). 8-bit mode: RXDATAL alone suffices, no RXDATAH read-order constraint applies | §25.5.1; §25.3.2.4.1 (error flags describe the FIFO-top byte; only informational here) | Reading DATAH-first patterns from other parts is unnecessary here; wrong-order reads could desync flags on 9-bit setups (not used) |
| U8 | poll `USART0.STATUS & USART_DREIF_bm` (tx_ready) | DREIF = TXDATA free for the next byte; hardware guarantees it within one byte time, which is what makes uart_pump's timeout-less wait bounded | §25.5.5 (DREIF text) | TX wedge if misread; bytes overwritten if ignored |
| U9 | `USART0.STATUS = USART_TXCIF_bm` before each `TXDATAL` write (tx_write) | TXCIF is sticky (W1C): without the clear, a *previous* response's completion satisfies U10's drain instantly and the BOOT jump could truncate the current reply's last byte. Clearing on every byte load makes "TXCIF set" mean "everything written since is fully shifted". STATUS's other writable bits are W1C flags or the write-only WFB, so writing only TXCIF's position disturbs nothing | §25.5.5 (TXCIF: "set when the entire frame in the transmit shift register has been shifted out and there are no new data in the transmit buffer"; cleared by writing '1') | Stale-completion race → BOOT truncates the response tail |
| U10 | `while (!(USART0.STATUS & USART_TXCIF_bm))` (uart_tx_drain) | Gates the BOOT jump: a UART pushes, so "response consumed" = shifter dry — the stream-transport analog of the TWI port's `twi_response_consumed()` (which had to wait for the host's *read* instead). Only called after a response was sent, so TXCIF (cleared by U9) genuinely refers to that response | §25.5.5 | Jump with bytes still shifting → host loses the BOOT ACK (retry sees the app, but the contract says reply-then-jump) |
| U11 | jump gate: `USART0.CTRLB = 0` | No bootloader USART state may carry into the app; PA0 intentionally **stays** a GPIO output driving high = UART idle mark, so the host's RX never floats across the handoff (an app that wants PA0 reconfigures it like any other pin after reset-state assumptions) | §25.5.7 (RXEN/TXEN clear releases the pins to PORT control, §25.3.2.3.1/.4.2) | App inherits a live USART claiming PA0/PA1 |

## main.c — clock, tick, jump (§12, §23, §15)

Identical to the TWI port's rows **M1–M3** (CLKCTRL prescaler clear, TCB0
1 ms polled tick) and **M5** (word-address ICALL to APP_BASE/2): the code
is the same lines with the same citations — see twi-audit. The jump gate
differs only in U11 above (USART0 disabled instead of TWI0; twi-audit M4's
"no peripheral state leaks" rationale carries over). Disasm check for M5
repeated on this ELF: `ldi r30,0x00; ldi r31,0x08; icall` present in
`port_jump_to_app`.

## Stream-transport behavior deltas (deliberate, spec-clean)

- **No ST_BAD_FRAME reply**: link_stream surfaces only CRC-valid frames;
  garbage/torn input is dropped inside the link layer and the host's
  timeout+retry recovers (link.h contract). Consequence: unparseable input
  cannot cancel autoboot — same net policy as the TWI port, where
  BAD_FRAME also left autoboot running.
- **No 0xFF padded-read filler**: that was a TWI read-transaction
  artifact (twi-audit T12); a UART only transmits armed bytes.
- **RX overflow**: frames whose declared LEN exceeds the 136-byte buffer
  are dropped at the LEN byte by link_stream (proof-covered), replacing
  twi.c's wire-level swallow (twi-audit T13).

## Cross-cutting checks

- **Zero protocol logic in the port**: `grep -E 'CRC|UPD_CMD|UPD_ST|LEN'
  uart.c ../skeletons/uart_pump.c` → one policy comment in uart.c (U7's
  why), no code. Frame parse/build/status decisions are core calls made by
  main.c.
- **Size gate**: `text+data = 2564 / 4096` bytes for `firmware_115200.hex`
  and `firmware_57600.hex` (avr-size -B; gate fails the build over budget).
  `[re-verified 2026-07-06: 2588 at Task 13; 2564 since the Task-16
  seam-wave header changes — both baud variants rebuilt and re-measured
  today]`
- **.init3 self-check**: green on both baud variants (see Makefile;
  identical check now also guards the TWI port).
- **-DNDEBUG**: same rationale as twi-audit.

## Must be checked at hardware bring-up (proof/compile cannot reach these)

1. **FUSE.OSCCFG = 20 MHz** (twi-audit M1 premise) — for THIS port it is
   not merely a slow tick: BAUD scales from f_CLK_PER, so a 16 MHz-fused
   part talks 25% off and never syncs. Verify the fuse (or a loopback
   echo) before blaming wiring. **BOOTSIZE fuse = 8** before first flash.
2. **Level/idle discipline of the host adapter**: 3.3 V/5 V logic UART,
   idle-high. A USB-TTL adapter that idles low or glitches on open would
   feed phantom start bits (harmless to framing — CRC drops them — but
   noisy).
3. **ERASE_APP stall**: 480 page erases run inside one request with RX
   ignored; at 115200 the host must not send the next frame until the
   ERASE response arrives (the CLI's request/response discipline already
   waits). USART RX overruns during the stall lose bytes silently —
   confirm the host library never pipelines requests.
4. **Silicon errata**: review the AVR64EA erratum sheet for the purchased
   date code (the AVR32EA sibling has published USART errata, DS80001091)
   before trusting the first flash cycle.
5. **Re-entry pair end-to-end** (twi-audit E2): stub-reset from a test app
   and confirm the bootloader stays resident — retest on this port even
   though the code is shared, since the link is the part proofs can't see.

## Re-verification record (2026-07-06)

Every row above was re-verified against the current tree and today's
rebuilt ELFs (115200 and 57600 variants rebuilt clean: text+data
2564/4096, size gate + .init3 gate pass). Row → current source location:

| Row | Current source (file:line) | Status |
|---|---|---|
| U1 | uart.c:12-44, :57 | verified; disasm re-run on today's ELFs: `ldi 0xB6` (694) → `sts 0x0808` at 115200; `ldi 0x6D` (1389) → `sts 0x0808` at 57600 |
| U2 | uart.c:58-59 | verified |
| U3 | uart.c:48-55 | verified |
| U4 | uart.c:60-61 | verified |
| U5 | uart.c:62 | verified |
| U6 | uart.c:67-76 | verified |
| U7 | uart.c:78-82 (policy comment :71-75) | verified |
| U8 | uart.c:84-91 | verified |
| U9 | uart.c:93-104 | verified |
| U10 | uart.c:110-118 | verified |
| U11 | main.c:45-54 (`USART0.CTRLB = 0` at :49) | verified; disasm re-run: `port_jump_to_app` = `cli; ldi r30,0x00; ldi r31,0x08; std Z+6,r1` (USART0.CTRLB=0 — Z doubles as the USART0 base 0x0800), TCB0 teardown via X=0x0B00, `icall` |
| reuse F1–F9, E1–E6 | ../avr_ea_common/flash.c, entry.c; ../../include/updater/app_stub.h | verified via the TWI audit's re-verification record (same translation units); .init3 gate re-run green on this port's ELFs today |
| reuse M1–M3, M5 | main.c:65-71 (M1), :26-42 (M2/M3), :55-62 (M5) | verified — identical lines to avr_ea_twi/main.c (M2/M3 identity is cited at main.c:20; the M1 premise is cited at port_cfg.h:19) |

