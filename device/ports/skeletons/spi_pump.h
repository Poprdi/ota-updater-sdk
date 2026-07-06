#ifndef UPDATER_SPI_PUMP_H
#define UPDATER_SPI_PUMP_H
#include <stdbool.h>
#include <stdint.h>
#include "updater/link.h"

/* spi_pump — link_stream glue for a hardware SPI *client* (device side),
 * implementing link.h's SPI busy-byte convention: the device shifts out
 * 0x00 on every exchange until a response is armed, then 0x7E + the frame
 * verbatim, then 0x00 again. The host clocks idle bytes and scans MISO
 * for the 0x7E sync.
 *
 * THE ONE-BYTE LAG (why this pump cannot look like uart_pump):
 * An SPI client never transmits on its own — the host's clock shifts both
 * directions simultaneously, and the byte that appears on MISO during an
 * exchange is whatever sat in the client's shift/data register BEFORE the
 * host started clocking it. The client only learns an exchange happened
 * after it completed — too late to choose that exchange's output. So the
 * response must be pre-loaded one byte AHEAD: data_write(b) stakes b for
 * the NEXT exchange, and after link_send arms a response the host always
 * sees one final busy byte (the previously staked 0x00) before the 0x7E.
 * Hosts following link.h's convention are unaffected — they scan for sync.
 *
 * Ops contract:
 *
 *   xfer_done(ctx)  -> true once a full byte exchange has completed since
 *                      the last data_read (e.g. "transfer complete flag").
 *                      Must return false when no new exchange happened —
 *                      link_poll pumps until it does.
 *   data_read(ctx)  -> the byte the host shifted IN during that exchange;
 *                      clears the done condition. Called exactly once per
 *                      xfer_done()==true.
 *   data_write(ctx,b)> stake b as the byte shifted OUT during the NEXT
 *                      exchange. Called once from init (the first idle
 *                      byte, so the register is never undefined) and once
 *                      after every data_read.
 *
 * Pacing: the pump must run (link_poll) between host exchanges — the
 * inter-byte gap the host leaves is the device's compute budget. If the
 * host clocks faster than the loop spins, bytes are lost or the staked
 * byte repeats (hardware-specific), the frame CRC fails, and timeout+retry
 * recovers — same degradation path as a UART overrun, no extra handling.
 *
 * Usage per request/response cycle (see test_pumps.c):
 *   link_poll(&l, &f)          — pump RX; returns true on a complete frame
 *   spi_pump_tx_clear(&p)      — drop any stale half-shifted response
 *                                 (host may have abandoned the previous
 *                                 read mid-way and retried)
 *   link_send(&p.io, buf, n)   — queue 0x7E + frame; goes out as the host
 *                                 clocks
 *   spi_pump_tx_idle(&p)       — true once the response has fully LEFT the
 *                                 wire (not merely been staked): gates a
 *                                 BOOT jump exactly like the TWI port's
 *                                 twi_response_consumed() */

typedef struct {
    bool    (*xfer_done)(void *ctx);
    uint8_t (*data_read)(void *ctx);
    void    (*data_write)(void *ctx, uint8_t b);
} spi_pump_ops_t;

typedef struct {
    const spi_pump_ops_t *ops;
    void                 *ctx;
    uint8_t              *tx_buf;      /* caller-owned response queue; size
                                          it sync + largest response frame
                                          (UPDATER_TX_BUF_SIZE + 1 style) */
    uint8_t               tx_cap;
    uint8_t               tx_len;
    uint8_t               tx_idx;
    bool                  staked_from_queue; /* byte now in the shift register
                                                is response payload, not
                                                filler: it has not left the
                                                wire yet */
    link_io_t             io;
} spi_pump_t;

/* ops/ctx/tx_buf must outlive the pump. Stakes the first 0x00 idle byte. */
void spi_pump_init(spi_pump_t *p, const spi_pump_ops_t *ops, void *ctx,
                   uint8_t *tx_buf, uint8_t tx_cap);

/* Drop queued-but-unshifted response bytes. Call when a new request frame
 * arrives, before building its response. */
void spi_pump_tx_clear(spi_pump_t *p);

/* True when no response byte remains queued OR staked in the shift
 * register — i.e. everything link_send queued has been clocked onto MISO. */
bool spi_pump_tx_idle(const spi_pump_t *p);

#endif
