/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher
 *
 * Reset entry, 1 ms tick, poll loop, entry-window policy, jump.
 * Protocol behavior mirrors the reference main loop pinned by the
 * conformance sim (conformance/sim/sim_port.c sim_request). */
#include <avr/io.h>
#include <stdbool.h>
#include <stdint.h>
#include "port_cfg.h"
#include "updater/port.h"
#include "updater/proto.h"
#include "updater/update.h"

/* ---- 1 ms tick: TCB0, polled ------------------------------------------- */
/* Periodic Interrupt mode: count to CCMP, set CAPT, restart (DS40002443A
 * 23.3.3.1.1) — "interrupt" is a misnomer here, INTCTRL stays 0 and CAPT
 * is polled. 20 MHz / 20000 = 1 kHz. */
static uint16_t g_ms;

static void timer_init(void)
{
    TCB0.CCMP  = 20000u - 1u;
    TCB0.CTRLB = TCB_CNTMODE_INT_gc;
    TCB0.CTRLA = TCB_CLKSEL_DIV1_gc | TCB_ENABLE_bm;   /* CLK_PER, undivided */
}

uint16_t port_ticks_ms(void)
{
    if (TCB0.INTFLAGS & TCB_CAPT_bm) {
        TCB0.INTFLAGS = TCB_CAPT_bm;    /* W1C */
        g_ms++;                         /* may undercount while a flash op
                                           blocks the loop — irrelevant, the
                                           tick only gates the entry window */
    }
    return g_ms;
}

/* ---- jump gate (called only by the core's proven upd_boot_if_valid) ---- */
void port_jump_to_app(void)
{
    __asm__ __volatile__("cli" ::: "memory");  /* never sei()'d, but the gate
                                                  must not depend on that */
    TWI0.SCTRLA = 0;                    /* client off: no bootloader bus state
                                           may carry into the app (27.5.10) */
    TCB0.CTRLA  = 0;
    TCB0.INTFLAGS = TCB_CAPT_bm;
    /* avr-gcc function-pointer values are word addresses: call word
     * APP_BASE/2 = byte 0x1000, the app's reset vector. The app's own
     * interrupt vectors also live there: CPUINT.CTRLA IVSEL reset value 0
     * places vectors directly after the boot section (15.5.1). */
    ((void (*)(void))(UPDATER_APP_BASE / 2u))();
    __builtin_unreachable();
}

int main(void)
{
    /* CLK_MAIN is OSCHF out of reset (MCLKCTRLA reset 0x00), running at
     * the FUSE.OSCCFG.OSCHFFRQ frequency — factory default 20 MHz. The
     * main prescaler resets ENABLED (MCLKCTRLB reset 0x11 = DIV6, 12.5.2);
     * writing 0 clears PEN => CLK_PER = 20 MHz. CCP-IOREG protected. */
    _PROTECTED_WRITE(CLKCTRL.MCLKCTRLB, 0);
    timer_init();
    twi_init();

    static upd_session_t s;
    static uint8_t rx[UPDATER_RX_BUF_SIZE];        /* sized by port geometry,
                                                      NOT the sim's 255 cap */
    static uint8_t payload[UPDATER_RSP_PAYLOAD_MAX];
    static uint8_t tx[UPDATER_TX_BUF_SIZE];

    upd_init(&s);
    bool resident = port_entry_requested();   /* app-requested entry skips
                                                 the T_ENTRY window */

    for (;;) {
        uint8_t len;
        if (port_recv(rx, &len)) {
            upd_frame_t req;
            uint8_t cmd;
            uint8_t plen;
            if (upd_frame_parse(rx, len, &req)) {
                cmd  = req.cmd;
                plen = upd_handle(&s, &req, payload, sizeof payload);
                resident = true;          /* any valid frame holds us in the
                                             bootloader (spec: Entry model) */
            } else {
                /* Reference behavior for unparseable input (sim_port.c):
                 * ST_BAD_FRAME, CMD echoes the first byte (0x00 if none);
                 * session state untouched, autoboot NOT cancelled. */
                cmd        = (len >= 1u) ? rx[0] : 0x00u;
                payload[0] = UPD_ST_BAD_FRAME;
                plen       = 1u;
            }
            port_send(tx, upd_frame_build(tx, sizeof tx,
                                          (uint8_t)(cmd | UPD_RSP_FLAG),
                                          payload, plen));
        }

        /* BOOT: reply first, jump after the host has actually read the
         * reply off the wire (a TWI client cannot push; jumping earlier
         * would kill the response). The gate re-validates the image. */
        if (s.boot_pending && twi_response_consumed()) {
            s.boot_pending = false;       /* single attempt, no busy re-CRC */
            (void)upd_boot_if_valid(&s.info);
        }

        if (!resident && port_ticks_ms() >= UPDATER_T_ENTRY_MS) {
            if (!upd_boot_if_valid(&s.info))
                resident = true;          /* no valid app: stay reachable for
                                             rescue flashing */
        }
    }
}
