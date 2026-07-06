# Updater wire protocol v1 — normative specification

This document is the source of truth for the wire. A third implementation
built against nothing but this file must interoperate with the shipped C
device core (`device/core/`) and Rust host core (`host/updater-core/`).
Where a limit lives in code, the code location is cited; if code and this
document ever disagree, that is a bug in one of them — file it.

The words MUST / MUST NOT / SHOULD are used normatively.

## 1. Model

Master-polled request/response. The host sends one request frame; the
device answers with exactly one response frame. There is never more than
one command outstanding. The device is a bootloader: single polled main
loop, no interrupts, no unsolicited transmissions.

## 2. Frame codec

One frame layout in both directions:

```
[CMD:1] [LEN:1] [payload:LEN] [CRC8:1]
```

- **CRC-8 (SMBus parameters):** polynomial 0x07, init 0x00, MSB-first, no
  reflection, no final XOR — computed over `CMD` through the last payload
  byte. Check value: `crc8("123456789") = 0xF4`.
- **Codec ceilings (normative home):** `LEN` is a u8 and a whole frame
  MUST fit in 255 bytes, therefore `LEN <= 252`
  (`255 - 3` bytes of overhead). Constants: `UPD_LEN_MAX` /
  `UPD_FRAME_MAX` in `device/include/updater/proto.h`. These bound the
  *codec*; actual accepted sizes are tighter (§7).
- **Response frame:** `CMD` is the request command with bit 7 set
  (`CMD | 0x80`); the first payload byte is a status code (§4), so every
  response has `LEN >= 1`. For an unparseable request answered on a
  transactional transport, the echoed `CMD` is the first received byte
  (0x00 if none) with bit 7 set.
- A frame failing the CRC or length check changes no device state.
  Transactional transports (I2C) answer it `ST_BAD_FRAME`; stream
  transports drop it silently and re-hunt sync (§10) — the host's
  retry recovers either way.

## 3. Commands

| CMD | Name | Request payload | OK response payload (after ST) |
|---|---|---|---|
| 0x01 | INFO | — (LEN 0) | 11 bytes, §3.1 |
| 0x02 | ERASE_APP | `45 52 41 53` ("ERAS") | — |
| 0x03 | WRITE_PAGE | `page_index:u16 LE` + exactly `page_size` data bytes | — |
| 0x04 | VERIFY | `length:u32 LE` + `crc32:u32 LE` | — |
| 0x05 | BOOT | — (LEN 0) | — (reply first, then jump, §9) |
| 0x06 | ECHO | 0–16 arbitrary bytes | the same bytes |

CMD 0x00 and 0x07–0x7F are reserved (unknown commands get `ST_BAD_CMD`);
0x80–0xFF is the response space.

### 3.1 INFO

Response payload is exactly 12 bytes (including ST). It carries **no base
address** — the device exposes the app region as offsets only:

| Offset | Field | Size |
|---|---|---|
| 0 | ST | 1 |
| 1 | proto_version (currently 1) | 1 |
| 2 | bl_version | 1 |
| 3 | device_id (port-assigned, e.g. "AE64", "RP23") | 4 |
| 7 | page_size, u16 LE (protocol page, §6) | 2 |
| 9 | app_pages, u16 LE | 2 |
| 11 | app_valid: 1 if the boot gate (§8) passes now, else 0 | 1 |

### 3.2 ERASE_APP

The 4-byte magic IS the frame: any length or content mismatch yields
`ST_BAD_MAGIC` and no erase. On success the device erases the entire app
region (page 0 to `app_pages - 1`, ascending) before replying, and latches
"erased this session". **The bus is held for the whole erase** — seconds
to minutes depending on the part (see INTEGRATION.md, Gotchas).

### 3.3 WRITE_PAGE

Payload MUST be `page_size + 2` bytes: page index (u16 LE) then exactly
one protocol page of data. Rejections, in order checked:
`ST_NOT_ERASED` if no ERASE_APP succeeded in this session (since reset);
`ST_BAD_FRAME` if the payload length is wrong;
`ST_OUT_OF_RANGE` if `page_index >= app_pages`.
Pages MAY arrive in any order and MAY repeat (§5).

### 3.4 VERIFY

Payload MUST be 8 bytes: `length` (u32 LE) then `crc32` (u32 LE).
`length > page_size * app_pages - 16` yields `ST_OUT_OF_RANGE` (the last
16 bytes of the region are the footer, §8). Otherwise the device
recomputes CRC-32 (IEEE 802.3, reflected, poly 0xEDB88320,
init/xorout 0xFFFFFFFF) over app-region bytes `[0, length)` and answers
`ST_OK` on match, `ST_BAD_CRC` otherwise. Stateless and read-only.

### 3.5 BOOT

If the boot gate (§8) passes, the device replies `ST_OK`, then jumps to
the app **only after the reply has fully left the wire** (§9). If not, it
replies `ST_NO_APP` and stays in the bootloader. The gate re-validates
flash immediately before the jump; the earlier check is never trusted
stale.

### 3.6 ECHO

Up to 16 payload bytes (`UPD_ECHO_MAX`); longer requests get
`ST_BAD_FRAME`. The response repeats the request payload after ST.

## 4. Status codes

| Value | Name | Meaning |
|---|---|---|
| 0x00 | ST_OK | success |
| 0x01 | ST_BAD_FRAME | CRC/length failure, or malformed payload length |
| 0x02 | ST_BAD_CMD | unknown command |
| 0x03 | ST_NOT_ERASED | WRITE_PAGE without a same-session ERASE_APP |
| 0x04 | ST_OUT_OF_RANGE | page index or VERIFY length outside the region |
| 0x05 | ST_BAD_CRC | VERIFY mismatch |
| 0x06 | ST_BAD_MAGIC | ERASE_APP magic wrong |
| 0x07 | ST_NO_APP | BOOT refused: no valid image |

Only `ST_BAD_FRAME` means "wire damage — retry the identical frame".
Every other non-OK status is a fact about the device and retrying the
same frame yields the same answer.

## 5. Idempotence (normative guarantee)

Every command MUST tolerate being re-executed with an identical frame.
This is the loss-recovery mechanism: when a response is lost or mangled,
the host re-sends the *identical* request (the shipped host retries up to
3 times after the initial attempt, `updater-core/src/session.rs
RETRY_BUDGET`). Consequences:

- Repeating INFO / VERIFY / ECHO is trivially safe (read-only).
- Repeating ERASE_APP erases again — slow, but state-identical.
- Repeating BOOT after a lost OK may find the device already gone; the
  host MUST treat post-BOOT silence as expected, not as failure.
- **Repeating WRITE_PAGE re-programs a page with the same data.** Ports
  MUST absorb this. On plain NOR flash re-programming identical data is
  harmless (programming only clears bits). On flashes where
  re-programming a written page is illegal (ECC, write-once), the port
  MUST absorb the repeat itself — compare-and-skip or buffering — and
  MUST NOT fault. See `device/include/updater/port.h`.

There is no session teardown: any state (the erased latch) resets with
the device.

## 6. The protocol page

`page_size` reported by INFO is the **protocol page**: the unit
WRITE_PAGE transfers and the unit the device's erase/write callbacks
take. It is NOT required to equal the physical flash page or sector.

Cap: `page_size <= 250`. Derivation: a WRITE_PAGE payload is
`page_size + 2` and a payload holds at most 252 bytes (§2), hence
`page_size <= 250`.

A port whose flash programs or erases in different physical units maps
protocol pages onto them internally. Worked example
(`device/ports/rp2350_uart/flash.c`): 128-byte protocol pages are
coalesced two-per-256-byte flash program and 32-per-4-KiB sector erase;
reads flush any coalescing buffer first so VERIFY and the boot gate
always see what a boot would see.

## 7. Buffer rules

- **Device RX:** a port MUST accept frames with `LEN` up to
  `page_size + 8` and MAY drop anything longer at the wire (the largest
  legal frame is WRITE_PAGE's `page_size + 5` bytes; the +8 is deliberate
  slack). RX buffer: `page_size + 8 + 3` bytes (136 for 128-byte pages).
  Size from geometry — never from the 252/255 codec ceilings, and never
  from the conformance sim's 255-byte cap, which pins reference-core
  behavior and is not a wire guarantee.
- **Device TX:** the largest response payload is `UPD_RSP_MAX = 17`
  (ST + 16 echoed bytes; INFO's 12 is smaller), so a response frame is at
  most 20 bytes.
- **Host frame buffer:** sized per `updater-core/src/session.rs` —
  largest request plus largest response of any command used; for `flash`
  that is `(page_size + 2 + 3) + (12 + 3)`; **320 bytes covers every
  legal geometry**.

## 8. Image footer and the boot gate

The last 16 bytes of the app region hold the footer:

| Offset in footer | Field |
|---|---|
| 0 | magic `"OTAU"` = `4F 54 41 55` |
| 4 | image length, u32 LE |
| 8 | CRC-32 of app bytes `[0, length)`, u32 LE |
| 12 | `FF FF FF FF` (reserved) |

The boot gate (`upd_app_valid`) runs on **every** boot decision: magic
present, `length <= region - 16`, recomputed CRC-32 equals the stored
one. A torn update fails its own CRC and the device stays in the
bootloader, reachable and reflashable. No auxiliary "valid" flag exists
that could desync from flash contents.

The **host** writes the footer as part of the final page
(`updater-core/src/image.rs`); the application never emits it. Linker
obligation on the app: the image MUST fit in `region - 16` bytes — the
host refuses larger images before touching the device
(`Error::ImageTooLarge`).

## 9. Entry model

- The bootloader runs first on every reset. While not *resident*, once
  its millisecond tick reaches the port's `T_ENTRY` window (300 ms in
  all shipped ports; port-configurable) it attempts to boot the app.
- **Resident latch:** any CRC-valid frame received cancels the autoboot
  for the rest of this power cycle. Unparseable bytes do NOT cancel it.
- **Rescue residency:** if the window expires and the boot gate refuses
  (no valid app), the device latches resident — it stays reachable for
  rescue flashing forever rather than re-checking in a loop.
- **Reply-consumed gating:** after an accepted BOOT the device jumps only
  once the reply has verifiably left the wire — transport-specific: the
  TWI port waits until the armed response was fully read out
  (`twi_response_consumed()`), the UART ports drain the TX FIFO
  (`uart_tx_drain()`). An I2C client cannot push data; jumping earlier
  would destroy the response.
- **App-requested re-entry:** the shipped header-only stub
  (`device/include/updater/app_stub.h`) writes a magic + one's-complement
  pair to storage that survives a soft reset but not power-on (AVR-EA:
  last 4 SRAM bytes; RP2350: watchdog SCRATCH2/3), then soft-resets. The
  bootloader reads AND clears the pair early in init and comes up
  resident (window skipped). Power-on garbage cannot fake the pair — the
  complement check rejects it.

## 10. Transport bindings

### 10.1 I2C / TWI (transactional)

Request: one raw I2C write of the frame to the device address.
Response: one raw I2C read. The master must choose a read length before
the device has said how long the response is, so it performs a
**fixed-length padded read**: the device returns the response frame
followed by `0xFF` filler for as long as the master keeps clocking; the
host decodes with padding-tolerant logic
(`updater_core::frame::decode_padded`). While busy (erasing, burning a
page) the device NACKs its address or clock-stretches; the host
poll-reads with a bounded retry budget.

### 10.2 Byte streams — UART, softuart (shared binding)

A `0x7E` sync byte, then the standard frame verbatim. Parsing is
length-driven after sync acquisition, so `0x7E` inside a frame is
harmless. A CRC/length failure or an over-long declared frame silently
drops the bytes and re-hunts the next `0x7E`. There is no stream-level
ACK — the request/response timeout+retry recovers loss, the CRC covers
corruption. Devices: `device/core/link_stream.c`; hosts:
`updater_core::stream::RxScanner`. Line rate is a port build parameter
(shipped device ports default 115200 8N1; softuart is fixed 9600 8N1).

### 10.3 SPI (stream over MISO/MOSI)

Same 0x7E binding, plus: the device shifts out `0x00` while idle or
busy; the first `0x7E` on MISO starts the response frame. The host polls
by clocking idle bytes and MUST tolerate at least one byte of shift lag
(the device's first response byte appears one exchange late). The
shipped host adapter sends sync + request in a single chip-select
assertion and paces response polling (default 50 µs/byte) to give the
device compute time between bytes.

## 11. Golden vectors

Any implementation MUST reproduce these byte-exactly (pinned in
`conformance/tests/golden.rs` and `conformance/casan/`):

| Vector | Bytes |
|---|---|
| INFO request (smallest frame) | `01 00 15` |
| ECHO request, payload `AA BB` | `06 02 AA BB 10` |
| ECHO request, payload `DE AD BE EF` | `06 04 DE AD BE EF B3` |
| CRC-8 check | `crc8("123456789") = 0xF4` |
| CRC-32 check | `crc32("123456789") = 0xCBF43926` |

## 12. Version policy

`proto_version` in INFO is 1. Breaking wire changes bump it; the shipped
host refuses a mismatch (`Error::ProtocolVersion`) before erasing
anything. The reserved command space (0x07–0x7F) and the footer's four
reserved bytes are the compatibility headroom for later extensions
(signing, A/B, compression — all explicitly absent from v1). Additive,
non-breaking extensions use new commands; devices answer unknown
commands `ST_BAD_CMD`, which hosts MUST treat as "feature absent".
