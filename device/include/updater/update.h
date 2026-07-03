#ifndef UPDATER_UPDATE_H
#define UPDATER_UPDATE_H
#include <stdbool.h>
#include <stdint.h>
#include "updater/port.h"
#include "updater/proto.h"

typedef struct {
    port_info_t info;
    bool        erased;        /* this session completed ERASE_APP */
    bool        boot_pending;  /* BOOT accepted; main jumps after replying */
} upd_session_t;

void    upd_init(upd_session_t *s);
/* Handles one parsed request; fills rsp payload (starting with ST byte).
 * Returns the rsp payload length: 0 if and only if rsp_cap == 0 (no room
 * for even the status byte), otherwise >= 1. Never touches flash outside
 * [0, info.app_pages) — proven. */
uint8_t upd_handle(upd_session_t *s, const upd_frame_t *req,
                   uint8_t *rsp, uint8_t rsp_cap);
/* Full-image footer check; the ONLY path to port_jump_to_app — proven. */
bool    upd_boot_if_valid(const port_info_t *info);
bool    upd_app_valid(const port_info_t *info);

#endif
