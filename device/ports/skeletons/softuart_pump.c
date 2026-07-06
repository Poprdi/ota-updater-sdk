/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#include "softuart_pump.h"

/* Bit-banged 8N1 at SOFTUART_BIT_US per bit. All timing rationale — the
 * 1.5-bit first sample, the 0.5-bit error budget, the poll-latency
 * requirement — lives in softuart_pump.h next to the ops contract. */

static bool pump_get(void *self, uint8_t *b)
{
    softuart_pump_t      *p   = (softuart_pump_t *)self;
    const softuart_pump_ops_t *ops = p->ops;

    if (ops->pin_rx(p->ctx))
        return false;              /* line at mark: no start bit pending */

    /* Start edge (or its aftermath) just observed. 1.5 bit times from
     * here is the center of data bit 0 — any latency between the true
     * edge and this sample shifts ALL subsequent samples late, which is
     * exactly the "edge-detection latency" term of the header's budget. */
    ops->delay_us(p->ctx, (uint16_t)(SOFTUART_BIT_US + SOFTUART_HALF_BIT_US));

    uint8_t v = 0u;
    for (uint8_t i = 0u; i < 8u; i++) {          /* LSB first (UART order) */
        if (ops->pin_rx(p->ctx))
            v |= (uint8_t)(1u << i);
        ops->delay_us(p->ctx, (uint16_t)SOFTUART_BIT_US);
    }

    /* Now at the stop-bit center (9.5 bit times). A low here means the
     * timing chain broke or the line is noise: drop the byte silently —
     * the frame CRC and host retry own recovery (header). But do NOT hunt
     * a new start bit while the line is still low: the remaining low
     * region would re-trigger as a phantom start and cascade misreads
     * through the following real bytes (hardware UARTs likewise re-arm
     * start detection only from mark). The wait is bounded to about one
     * character time so a held-low line (break, host reset) returns
     * control to the main loop, which simply re-enters here. */
    if (!ops->pin_rx(p->ctx)) {
        for (uint8_t i = 0u; i < 20u; i++) {
            if (ops->pin_rx(p->ctx))
                break;
            ops->delay_us(p->ctx, (uint16_t)SOFTUART_HALF_BIT_US);
        }
        return false;
    }

    *b = v;
    return true;
}

static void pump_put(void *self, uint8_t b)
{
    softuart_pump_t      *p   = (softuart_pump_t *)self;
    const softuart_pump_ops_t *ops = p->ops;

    ops->pin_tx(p->ctx, false);                  /* start bit */
    ops->delay_us(p->ctx, (uint16_t)SOFTUART_BIT_US);
    for (uint8_t i = 0u; i < 8u; i++) {
        ops->pin_tx(p->ctx, ((b >> i) & 1u) != 0u);
        ops->delay_us(p->ctx, (uint16_t)SOFTUART_BIT_US);
    }
    ops->pin_tx(p->ctx, true);                   /* stop bit = idle mark:
                                                    line is left high, so
                                                    back-to-back put_byte
                                                    calls are legal */
    ops->delay_us(p->ctx, (uint16_t)SOFTUART_BIT_US);
}

void softuart_pump_init(softuart_pump_t *p, const softuart_pump_ops_t *ops,
                        void *ctx)
{
    p->ops         = ops;
    p->ctx         = ctx;
    p->io.get_byte = pump_get;
    p->io.put_byte = pump_put;
    p->io.ctx      = p;
    ops->pin_tx(ctx, true);        /* establish idle mark before any traffic */
}
