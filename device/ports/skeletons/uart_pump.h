#ifndef UPDATER_UART_PUMP_H
#define UPDATER_UART_PUMP_H
#include <stdbool.h>
#include <stdint.h>
#include "updater/link.h"

/* uart_pump — link_stream glue for a hardware UART, written against four
 * register operations instead of any MCU header. Copy this pair of files
 * into a port and implement the ops over your part's registers; nothing
 * here needs editing.
 *
 * Ops contract (what your callbacks must guarantee):
 *
 *   rx_ready(ctx)  -> true only when one received byte can be read RIGHT
 *                     NOW without blocking (e.g. "RX-complete flag set").
 *                     Must return false once the RX FIFO is drained —
 *                     link_poll pumps until the FIFO runs dry OR a frame
 *                     completes (remaining bytes stay queued for the next
 *                     call), so a stuck-true rx_ready is an infinite
 *                     loop.
 *   rx_read(ctx)   -> return that byte. Called exactly once per
 *                     rx_ready()==true, never otherwise; it may therefore
 *                     pop a FIFO / clear a flag unconditionally. If your
 *                     hardware has no "ready" flag separate from the read,
 *                     buffer one byte in ctx.
 *   tx_ready(ctx)  -> true when one byte can be written without loss
 *                     (e.g. "data register empty"). Must eventually become
 *                     true after a write completes: put_byte busy-waits on
 *                     it with no timeout (see below).
 *   tx_write(ctx,b)-> hand b to the transmitter. Called only after
 *                     tx_ready()==true.
 *
 * Error policy: RX overrun/framing/parity errors need no handling here —
 * a lost or corrupted byte makes the frame CRC fail, link_stream drops the
 * frame, and the host's timeout+retry recovers (link.h). Clear any sticky
 * hardware error flags inside rx_read/rx_ready if your part requires that
 * to keep receiving. If your loop ever decides the stream is wedged
 * mid-frame, calling link_init again is the stall-abandon path: it
 * discards the half-assembled frame and re-hunts sync (link.h).
 *
 * Blocking TX is deliberate: the bootloader is single-threaded and polled,
 * and a UART transmitter always drains (hardware guarantee), so waiting in
 * put_byte is the same class of stall as the TWI port's clock stretch —
 * bounded by the byte time, not by the peer. */

typedef struct {
    bool    (*rx_ready)(void *ctx);
    uint8_t (*rx_read)(void *ctx);
    bool    (*tx_ready)(void *ctx);
    void    (*tx_write)(void *ctx, uint8_t b);
} uart_pump_ops_t;

typedef struct {
    const uart_pump_ops_t *ops;
    void                  *ctx;
    link_io_t              io;   /* pass &pump.io to link_init / link_send */
} uart_pump_t;

/* ops/ctx must outlive the pump; ops is not copied. */
void uart_pump_init(uart_pump_t *p, const uart_pump_ops_t *ops, void *ctx);

#endif
