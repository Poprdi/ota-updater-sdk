#include "spi_pump.h"

/* Response bytes flow: link_send -> put (queue) -> get (stake into the
 * shift register after each completed exchange) -> the wire, one exchange
 * later. See spi_pump.h for why the staking is always one byte ahead. */

#define SPI_PUMP_IDLE 0x00u   /* link.h: device outputs 0x00 while idle/busy */

static uint8_t next_out(spi_pump_t *p)
{
    if (p->tx_idx < p->tx_len) {
        p->staked_from_queue = true;
        return p->tx_buf[p->tx_idx++];
    }
    p->staked_from_queue = false;
    return SPI_PUMP_IDLE;
}

static bool pump_get(void *self, uint8_t *b)
{
    spi_pump_t *p = (spi_pump_t *)self;
    if (!p->ops->xfer_done(p->ctx))
        return false;
    /* The completed exchange also proves the previously staked byte went
     * out on MISO — that is the only observable "byte left the wire"
     * event an SPI client gets, and what tx_idle is derived from. */
    *b = p->ops->data_read(p->ctx);
    p->ops->data_write(p->ctx, next_out(p));
    return true;
}

static void pump_put(void *self, uint8_t b)
{
    spi_pump_t *p = (spi_pump_t *)self;
    /* Queue only; the host's clock moves the bytes. Overflow cannot happen
     * when tx_buf is sized for the largest response (spi_pump.h); if a
     * caller still overruns, dropping the tail turns it into a CRC-bad
     * frame on the host — detected, retried — instead of memory damage. */
    if (p->tx_len < p->tx_cap)
        p->tx_buf[p->tx_len++] = b;
}

void spi_pump_init(spi_pump_t *p, const spi_pump_ops_t *ops, void *ctx,
                   uint8_t *tx_buf, uint8_t tx_cap)
{
    p->ops               = ops;
    p->ctx               = ctx;
    p->tx_buf            = tx_buf;
    p->tx_cap            = tx_cap;
    p->tx_len            = 0u;
    p->tx_idx            = 0u;
    p->staked_from_queue = false;
    p->io.get_byte       = pump_get;
    p->io.put_byte       = pump_put;
    p->io.ctx            = p;
    /* Power-up data-register content is undefined on most parts; stake the
     * idle byte now so even an immediate host poll reads 0x00, never junk
     * that could alias 0x7E. */
    ops->data_write(ctx, SPI_PUMP_IDLE);
}

void spi_pump_tx_clear(spi_pump_t *p)
{
    p->tx_len = 0u;
    p->tx_idx = 0u;
    /* A staked stale byte cannot be recalled from the shift register; it
     * appears as one garbage byte mid-hunt on the host, which the sync
     * scan skips. */
}

bool spi_pump_tx_idle(const spi_pump_t *p)
{
    return p->tx_idx == p->tx_len && !p->staked_from_queue;
}
