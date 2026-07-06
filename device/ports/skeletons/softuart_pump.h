/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef UPDATER_SOFTUART_PUMP_H
#define UPDATER_SOFTUART_PUMP_H
#include <stdbool.h>
#include <stdint.h>
#include "updater/link.h"

/* softuart_pump — 9600-baud 8N1 bit-banged UART over two GPIOs and a
 * microsecond delay, for parts (or pin budgets) with no hardware UART.
 * Half-duplex and fully polled: while a byte is being received or sent
 * the CPU does nothing else, which is exactly the bootloader's situation.
 *
 * Ops contract:
 *
 *   pin_tx(ctx, level) -> drive the TX line; true = logic high. The line
 *                         must idle HIGH (UART mark). init() drives it
 *                         high once; configure the pin as output-high
 *                         BEFORE calling init so the host never sees a
 *                         glitch that could read as a start bit.
 *   pin_rx(ctx)        -> sample the RX line; true = high. Must be a live
 *                         sample, not latched.
 *   delay_us(ctx, us)  -> busy-wait at least us microseconds. Accuracy
 *                         directly consumes the timing budget below; a
 *                         calibrated cycle loop or a free-running timer
 *                         both work.
 *
 * TIMING (why 9600, why these constants, what the budget is):
 * A UART receiver has no clock line; it derives every sample time from
 * the start bit's falling edge. We detect that edge, wait 1.5 bit times
 * (into the CENTER of data bit 0), then sample every 1.0 bit time — LSB
 * first — and finally check the stop bit at 9.5 bit times. Center
 * sampling maximizes the margin symmetrically: the LAST decision (stop
 * bit) sits 9.5 bit times after the edge, so the sample still lands
 * inside the correct bit as long as ALL accumulated error stays under
 * 0.5 bit time (52 us at 9600):
 *
 *     9.5 x (rate mismatch) + (edge-detection latency) + (call overhead)
 *         < 0.5 bit time
 *
 * Budgeted at ±2% clock error PER SIDE (worst case ~4% relative — cheap
 * internal RC oscillators): 9.5 x 4% = 0.38 bit times, leaving ~12 us for
 * everything else. Two practical consequences:
 *   1. 9600 is the deliberate speed cap: the absolute-time slack scales
 *      with the bit time, and 104 us bits keep the leftover margin above
 *      GPIO/delay overheads on slow parts. Doubling the baud halves it.
 *   2. Edge-detection latency counts against the SAME budget: get_byte
 *      only sees the start edge when it happens to sample the line, so
 *      between bytes of a frame the main loop must return to link_poll
 *      within that leftover margin (~10 us at worst-case clocks; relaxed
 *      proportionally with better oscillators). Each byte re-syncs on its
 *      own start edge, so error never accumulates across bytes — only
 *      within the 10-bit character.
 *
 * A byte whose stop-bit check fails (framing error — noise, or a start
 * edge caught too late) is silently dropped: a damaged byte makes the
 * frame CRC fail anyway, and the host's timeout+retry owns recovery
 * (link.h). RX is ignored while put_byte transmits (half-duplex); the
 * request/response protocol never talks in both directions at once. */

#define SOFTUART_BIT_US      104u  /* 1e6 / 9600 = 104.17: -0.16% device-side
                                      rate error, charged against the budget
                                      above */
#define SOFTUART_HALF_BIT_US (SOFTUART_BIT_US / 2u)

typedef struct {
    void (*pin_tx)(void *ctx, bool level);
    bool (*pin_rx)(void *ctx);
    void (*delay_us)(void *ctx, uint16_t us);
} softuart_pump_ops_t;

typedef struct {
    const softuart_pump_ops_t *ops;
    void                 *ctx;
    link_io_t             io;   /* pass &pump.io to link_init / link_send */
} softuart_pump_t;

/* ops/ctx must outlive the pump. Drives TX to idle mark (high). */
void softuart_pump_init(softuart_pump_t *p, const softuart_pump_ops_t *ops,
                        void *ctx);

#endif
