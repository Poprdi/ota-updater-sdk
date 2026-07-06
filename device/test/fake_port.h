/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef FAKE_PORT_H
#define FAKE_PORT_H
/* Host-side test double for the port contract (test-only, never shipped).
 * Flash is a 32*128 byte array initialised to 0xFF; every erase/write is
 * recorded; port_jump_to_app longjmps so tests survive the "never returns"
 * contract. */
#include <stdbool.h>
#include <stdint.h>
#include "updater/port.h"

void     fake_port_reset(void);
uint32_t fake_port_erase_count(void);
uint32_t fake_port_write_count(void);
/* Highest page index ever passed to erase/write since reset (0 if none). */
uint16_t fake_port_max_touched_page(void);
/* Writes a 200-byte pattern app plus a correct footer ("OTAU" | len LE |
 * crc LE | FF*4 in the last 16 bytes of the region) via port_flash_write_page. */
void     fake_port_flash_valid_app(void);
void     fake_port_valid_app_params(uint32_t *len, uint32_t *crc);
/* Flips one byte inside the app image (simulated corruption). */
void     fake_port_corrupt_app_byte(void);
/* Runs upd_boot_if_valid(info) under setjmp; returns true iff the port
 * jump was taken (longjmp), false if the gate refused. */
bool     fake_port_jump_catch(const port_info_t *info);

#endif
