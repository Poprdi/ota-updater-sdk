#include "uart_pump.h"

/* The pump itself is just the impedance match between link_io_t's
 * get_byte/put_byte and the four register ops; all framing lives in
 * link_stream. Kept in its own translation unit so a port links it
 * unmodified and the contract in uart_pump.h stays the single source
 * of truth. */

static bool pump_get(void *self, uint8_t *b)
{
    uart_pump_t *p = (uart_pump_t *)self;
    if (!p->ops->rx_ready(p->ctx))
        return false;
    *b = p->ops->rx_read(p->ctx);
    return true;
}

static void pump_put(void *self, uint8_t b)
{
    uart_pump_t *p = (uart_pump_t *)self;
    /* No timeout: tx_ready is a hardware "register free" flag that always
     * comes true within one byte time (uart_pump.h, ops contract). */
    while (!p->ops->tx_ready(p->ctx)) { }
    p->ops->tx_write(p->ctx, b);
}

void uart_pump_init(uart_pump_t *p, const uart_pump_ops_t *ops, void *ctx)
{
    p->ops         = ops;
    p->ctx         = ctx;
    p->io.get_byte = pump_get;
    p->io.put_byte = pump_put;
    p->io.ctx      = p;
}
