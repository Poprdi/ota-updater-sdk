/* Reset entry, 1 ms tick, poll loop, entry-window policy, jump — the same
 * loop shape as avr_ea_twi/main.c with the transport swapped: frames
 * arrive through link_stream over the uart_pump instead of the TWI state
 * machine. Protocol behavior mirrors the reference main loop pinned by
 * the conformance sim (conformance/sim/sim_port.c sim_request), with one
 * documented stream-transport divergence: there is no ST_BAD_FRAME reply
 * path, because link_poll only ever surfaces CRC-valid frames — garbage
 * and torn frames are dropped inside the link layer and recovery is the
 * host's timeout+retry (link.h). Unparseable input therefore also cannot
 * cancel autoboot, exactly like the TWI port's BAD_FRAME path. */
#include <avr/io.h>
#include <stdbool.h>
#include <stdint.h>
#include "port_cfg.h"
#include "updater/link.h"
#include "updater/port.h"
#include "updater/proto.h"
#include "updater/update.h"

/* ---- 1 ms tick: TCB0, polled (identical to avr_ea_twi, audit M2/M3) --- */
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
    USART0.CTRLB = 0;                   /* RX/TX off: no bootloader USART
                                           state may carry into the app
                                           (25.5.7). PA0 stays a GPIO output
                                           driving high = UART idle mark, so
                                           the host's RX line never floats
                                           across the handoff. */
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
    uart_init();

    static upd_session_t s;
    static uint8_t rx[UPDATER_RX_BUF_SIZE];        /* sized by port geometry,
                                                      NOT the sim's 255 cap */
    static uint8_t payload[UPDATER_RSP_PAYLOAD_MAX];
    static uint8_t tx[UPDATER_TX_BUF_SIZE];
    static uart_pump_t pump;
    static link_t      lnk;

    uart_pump_init(&pump, &uart0_pump_ops, (void *)0);
    link_init(&lnk, &pump.io, rx, (uint8_t)sizeof rx);

    upd_init(&s);
    bool resident = port_entry_requested();   /* app-requested entry skips
                                                 the T_ENTRY window */

    for (;;) {
        upd_frame_t req;
        if (link_poll(&lnk, &req)) {
            uint8_t plen = upd_handle(&s, &req, payload, sizeof payload);
            resident = true;              /* any valid frame holds us in the
                                             bootloader (spec: Entry model) */
            link_send(&pump.io,
                      tx, upd_frame_build(tx, (uint8_t)sizeof tx,
                                          (uint8_t)(req.cmd | UPD_RSP_FLAG),
                                          payload, plen));
        }

        /* BOOT: reply first, jump after the reply has actually left the
         * wire — the UART pushes, so "consumed" is simply the transmitter
         * running dry (uart_tx_drain), where the TWI port had to wait for
         * the host's read. The gate re-validates the image. */
        if (s.boot_pending) {
            s.boot_pending = false;       /* single attempt, no busy re-CRC */
            uart_tx_drain();
            (void)upd_boot_if_valid(&s.info);
        }

        if (!resident && port_ticks_ms() >= UPDATER_T_ENTRY_MS) {
            if (!upd_boot_if_valid(&s.info))
                resident = true;          /* no valid app: stay reachable for
                                             rescue flashing */
        }
    }
}
