/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef UPDATER_CRC32_H
#define UPDATER_CRC32_H
#include <stdint.h>

/* CRC-32/IEEE reflected, poly 0xEDB88320, init/xorout 0xFFFFFFFF.
 * Streaming byte API: the device feeds flash reads through it without a
 * buffer; bitwise because a table would eat 1/4 of the boot section. */
uint32_t upd_crc32_init(void);
uint32_t upd_crc32_update(uint32_t crc, uint8_t byte);
uint32_t upd_crc32_final(uint32_t crc);

#endif
