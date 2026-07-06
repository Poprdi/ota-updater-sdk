#ifndef UPDATER_UPDATE_H
#define UPDATER_UPDATE_H
/* Main-loop obligations (what YOUR loop must do around this core; the
 * reference loops are ports/avr_ea_twi/main.c, ports/avr_ea_uart/main.c
 * and the pinned conformance loop in conformance/sim/sim_port.c):
 *  - BOOT ordering: reply first, and jump only once the reply has fully
 *    left the wire — transport-specific (TWI: twi_response_consumed();
 *    UART: uart_tx_drain(); a TWI client cannot push, so jumping earlier
 *    kills the response).
 *  - Clear s->boot_pending yourself before calling upd_boot_if_valid;
 *    the core only sets it.
 *  - upd_boot_if_valid RE-VALIDATES flash; the stale boot_pending flag is
 *    never trusted, so a refused jump is safe to ignore.
 *  - Resident latch: ANY CRC-valid frame cancels the autoboot — set your
 *    resident flag whenever upd_handle runs.
 *  - T_ENTRY window: while not resident, once port_ticks_ms() reaches the
 *    port's UPDATER_T_ENTRY_MS, attempt upd_boot_if_valid.
 *  - Rescue residency: if that attempt refuses (no valid app), latch
 *    resident so the device stays reachable for rescue flashing. */
#include <stdbool.h>
#include <stdint.h>
#include "updater/port.h"
#include "updater/proto.h"

typedef struct {
    port_info_t info;
    bool        erased;        /* this session completed ERASE_APP */
    bool        boot_pending;  /* BOOT accepted; main jumps only after the
                                * reply has fully left the wire (BOOT
                                * ordering above) */
} upd_session_t;

void    upd_init(upd_session_t *s);
/* Handles one parsed request; fills rsp payload (starting with ST byte).
 * Give rsp_cap >= UPD_RSP_MAX (17, proto.h: ST + a 16-byte ECHO, the
 * largest reply; INFO needs 12) so no reply is ever cut short.
 * Returns the rsp payload length: 0 if and only if rsp_cap == 0 (no room
 * for even the status byte), otherwise >= 1. Never touches flash outside
 * [0, info.app_pages) — proven. */
uint8_t upd_handle(upd_session_t *s, const upd_frame_t *req,
                   uint8_t *rsp, uint8_t rsp_cap);
/* Full-image footer check; the ONLY path to port_jump_to_app — proven. */
bool    upd_boot_if_valid(const port_info_t *info);
bool    upd_app_valid(const port_info_t *info);

#endif
