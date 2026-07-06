/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef UPDATER_LINK_H
#define UPDATER_LINK_H
#include <stdbool.h>
#include <stdint.h>
#include "updater/proto.h"

/* Stream link layer — shared framing for byte-stream transports (UART, SPI,
 * bit-banged GPIO/softuart). I2C is transactional and does not use it.
 *
 * Wire binding: a 0x7E sync byte, then the standard frame verbatim
 * (CMD LEN payload CRC8 — see proto.h). Parsing is length-driven after sync
 * acquisition, so 0x7E bytes inside a frame are harmless: bytes are consumed
 * as frame bytes while mid-frame, and sync hunting only happens between
 * frames. A CRC/length failure or an over-long declared frame silently drops
 * the bytes and re-hunts the next 0x7E; there is no stream-level ACK — the
 * request/response protocol's timeout+retry recovers from loss, and the CRC
 * already covers corruption.
 *
 * SPI convention (prose only — no SPI code lives here): the device shifts
 * out 0x00 while idle or busy; the first 0x7E on MISO starts the response
 * frame. Hosts poll by clocking idle bytes until they see the sync.
 *
 * Buffer sizing: the caller owns the receive buffer and sizes it for the
 * largest frame it must accept — page_size + 8 style, e.g. 136 for a
 * 128-byte-page port (mirrors the TWI port's UPDATER_RX_BUF_SIZE). Frames
 * whose declared length cannot fit are dropped before any overflow.
 */

#define UPD_LINK_SYNC 0x7Eu

typedef struct {
    /* Nonblocking receive: true and *b filled when a byte was available,
     * false when the stream has nothing yet. Must eventually return false
     * once the port's FIFO is drained — link_poll pumps until the FIFO
     * runs dry OR a frame completes (remaining bytes stay queued for the
     * next call), so a stuck-true get_byte is an infinite loop. */
    bool (*get_byte)(void *ctx, uint8_t *b);
    void (*put_byte)(void *ctx, uint8_t b);
    void *ctx;
} link_io_t;

typedef struct {
    const link_io_t *io;
    uint8_t         *buf;       /* caller-owned frame assembly buffer */
    uint8_t          buf_len;
    uint8_t          n;         /* bytes assembled since the last sync */
    bool             in_frame;  /* sync seen, accumulating length-driven */
} link_t;

/* Also the stall-reset path: calling link_init again on a live link
 * discards any half-assembled frame and returns to sync hunting — do this
 * when the caller decides the stream is wedged (e.g. after its own
 * timeout mid-frame). No other reset entry point exists or is needed. */
void link_init(link_t *l, const link_io_t *io, uint8_t *buf, uint8_t buf_len);

/* Pumps bytes from the port and RETURNS AT THE FIRST complete, CRC-valid
 * frame (validated by upd_frame_parse — one codec, no duplicate
 * validation): any bytes still in the port's FIFO stay there, so callers
 * loop on link_poll to drain batched input. Returns false once the port
 * runs dry with no complete frame. *out then aliases l->buf and stays
 * valid until the next link_poll on the same link. Invalid frames are
 * dropped silently.  *
 * link_t carries a load-bearing internal invariant (machine-checked in
 * link_stream.c): only ever obtain one from link_init and mutate it through
 * link_poll — a hand-constructed or field-poked link_t is out of contract.
 */
bool link_poll(link_t *l, upd_frame_t *out);

/* Emits 0x7E, then the n frame bytes verbatim (frame as produced by
 * upd_frame_build). */
void link_send(const link_io_t *io, const uint8_t *frame, uint8_t n);

#endif
