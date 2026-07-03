#ifndef UPDATER_CRC8_H
#define UPDATER_CRC8_H
#include <stdint.h>

/* CRC-8/ATM, poly 0x07, init 0x00 — frame integrity on the wire. */
uint8_t upd_crc8(const uint8_t *p, uint8_t n);

#endif
