/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#include "updater/crc32.h"

/*@ assigns \nothing;
    ensures \result == 0xFFFFFFFF; */
uint32_t upd_crc32_init(void) { return 0xFFFFFFFFUL; }

/*@ assigns \nothing; */
uint32_t upd_crc32_update(uint32_t crc, uint8_t byte)
{
    crc ^= byte;
    /*@ loop invariant 0 <= b <= 8;
        loop assigns b, crc;
        loop variant 8 - b; */
    for (uint8_t b = 0; b < 8; b++)
        crc = (crc & 1UL) ? (crc >> 1) ^ 0xEDB88320UL : crc >> 1;
    return crc;
}

/*@ assigns \nothing;
    ensures \result == (crc ^ 0xFFFFFFFF); */
uint32_t upd_crc32_final(uint32_t crc) { return crc ^ 0xFFFFFFFFUL; }
