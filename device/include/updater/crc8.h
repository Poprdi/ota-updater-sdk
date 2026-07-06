/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef UPDATER_CRC8_H
#define UPDATER_CRC8_H
#include <stdint.h>

/* CRC-8 (SMBus parameters: poly 0x07, init 0x00, xorout 0x00) — frame
 * integrity on the wire. */
uint8_t upd_crc8(const uint8_t *p, uint8_t n);

#endif
