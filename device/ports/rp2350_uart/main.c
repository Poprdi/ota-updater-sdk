/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher
 *
 * Reset entry, ms tick, poll loop, entry-window policy, jump — the same
 * loop shape as avr_ea_uart/main.c on a second CPU family: frames arrive
 * through link_stream over the uart_pump, the core decides everything,
 * and the only ARM-specific pieces are the tick source and the
 * VTOR/MSP/reset-handler jump. Protocol behavior mirrors the reference
 * main loop pinned by the conformance sim, with the same documented
 * stream-transport divergence as avr_ea_uart: no ST_BAD_FRAME reply path,
 * because link_poll only surfaces CRC-valid frames — garbage is dropped
 * in the link layer and recovery is the host's timeout+retry (link.h). */
#include <stdbool.h>
#include <stdint.h>

#include "hardware/structs/m33.h"
#include "hardware/sync.h"
#include "hardware/uart.h"
#include "pico/time.h"

#include "port_cfg.h"
#include "updater/link.h"
#include "updater/port.h"
#include "updater/proto.h"
#include "updater/update.h"

/* ---- 1 ms tick ---------------------------------------------------------
 * The 64-bit microsecond timebase runs after crt0's runtime_init (clock
 * and tick generator setup — sdk:src/rp2_common/pico_runtime/runtime.c);
 * no peripheral is claimed and nothing needs de-initializing at the
 * jump. Truncation to uint16_t wraps at 65.536 s with the same modular
 * semantics as the AVR ports' software counter — the tick only gates the
 * entry window. */
uint16_t port_ticks_ms(void)
{
    return (uint16_t)to_ms_since_boot(get_absolute_time());
}

/* ---- jump gate (called only by the core's proven upd_boot_if_valid) ----
 * The app is a plain vector-table image linked at UPDATER_APP_BASE
 * (memmap_app.ld): word 0 = initial MSP, word 1 = reset handler — entered
 * exactly the way the ROM enters a flash image, except the ROM has
 * already done the flash/XIP setup for us and it stays valid across the
 * handoff. */
void port_jump_to_app(void)
{
    /* De-init: no bootloader UART state may carry into the app.
     * uart_deinit puts the PL011 back into reset (sdk:src/rp2_common/
     * hardware_uart/uart.c). GPIO0/1 deliberately KEEP their UART
     * function: GPIO0 then presents the pad's pull-up idle level rather
     * than a floating line, and an app that wants the pins reconfigures
     * them like any other (reset-state assumptions are the app's own). */
    uart_deinit(uart0);

    /* Mask exceptions for the transition, then scrub the NVIC so the
     * handoff is reset-equivalent: the ROM enters a flash image with no
     * NVIC line enabled or pending, and the app must see the same. The
     * bootloader itself enables no IRQ (PICO_TIME_DEFAULT_ALARM_POOL_
     * DISABLED=1 keeps pico_time from enabling the alarm IRQ at runtime
     * init — see CMakeLists/U9), but the gate must not depend on that:
     * write all-ones to every NVIC_ICERn (0xE000E180+, disable) and
     * NVIC_ICPRn (0xE000E280+, clear-pending) bank present. RP2350 has
     * 52 external IRQs (NUM_IRQS, sdk:src/rp2350/hardware_regs/include/
     * hardware/platform_defs.h) -> 2 banks, matching m33_hw->nvic_icer[2]
     * / nvic_icpr[2] (sdk:src/rp2350/hardware_structs/include/hardware/
     * structs/m33.h). */
    __asm volatile ("cpsid i" ::: "memory");
    for (unsigned i = 0; i < count_of(m33_hw->nvic_icer); i++) {
        m33_hw->nvic_icer[i] = 0xFFFFFFFFu;
        m33_hw->nvic_icpr[i] = 0xFFFFFFFFu;
    }
    __dsb();
    __isb();

    const volatile uint32_t *vt = (const volatile uint32_t *)UPDATER_APP_BASE;
    uint32_t sp    = vt[0];
    uint32_t reset = vt[1];

    /* Point exceptions at the app's table before touching SP: any fault
     * from here on must resolve through the app's vectors
     * (sdk:src/rp2350/hardware_structs/include/hardware/structs/m33.h
     * m33_hw->vtor, M33_VTOR). Barriers order the write against the
     * following stack/branch per the ARMv8-M requirements on VTOR
     * updates. */
    m33_hw->vtor = (uint32_t)vt;
    __dsb();
    __isb();

    /* Clear the stack limit before moving MSP: if a guard was active a
     * lower app stack would fault instantly. crt0 does the same for its
     * own entry (sdk:src/rp2_common/pico_crt0/crt0.S "Make sure stack
     * limit is 0"). One asm block for MSPLIM/MSP/CPSIE/BX: after MSP
     * moves, this function must not touch its own stack again.
     *
     * cpsie AFTER the new MSP is loaded, LAST before bx: the ROM enters
     * flash images with PRIMASK=0 and pico-sdk crt0 never executes cpsie
     * (verified: zero cpsie in the built app ELF), so a PRIMASK=1
     * handoff would leave the app permanently unable to take IRQs —
     * sleep_ms/WFE would hang forever. Unmasking here is safe: the NVIC
     * scrub above guarantees no line is enabled or pending, and VTOR
     * already points at the app's table for any (fault) exception. */
    __asm volatile (
        "movs r3, #0        \n"
        "msr  msplim, r3    \n"
        "msr  msp, %0       \n"
        "cpsie i            \n"
        "bx   %1            \n"
        : : "r" (sp), "r" (reset) : "r3");
    __builtin_unreachable();
}

int main(void)
{
    /* First statement: capture + clear the watchdog-scratch entry pair
     * (entry.c) before any other SDK call. */
    updater_entry_capture();
    updater_uart_init();

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
         * wire — FR.BUSY dry (updater_uart_tx_drain), the PL011 analog of
         * the AVR port's TXCIF wait. The gate re-validates the image. */
        if (s.boot_pending) {
            s.boot_pending = false;       /* single attempt, no busy re-CRC */
            updater_uart_tx_drain();
            (void)upd_boot_if_valid(&s.info);
        }

        if (!resident && port_ticks_ms() >= UPDATER_T_ENTRY_MS) {
            if (!upd_boot_if_valid(&s.info))
                resident = true;          /* no valid app: stay reachable for
                                             rescue flashing */
        }
    }
}
