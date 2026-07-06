/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef UPDATER_PORT_CFG_H
#define UPDATER_PORT_CFG_H
/* AVR64EA28/TWI port configuration + intra-port wiring. Geometry, buffer
 * sizing and the shared datasheet/citation preamble live in the common
 * header ../avr_ea_common/port_geom.h (shared with avr_ea_uart, together
 * with flash.c and entry.c); only TWI-specific pieces remain here. */
#include <stdbool.h>
#include <stdint.h>
#include "../avr_ea_common/port_geom.h"

#ifndef UPDATER_TWI_ADDR
#  error "build with -DUPDATER_TWI_ADDR=0xNN (the Makefile builds 0x10 and 0x20)"
#endif

/* Intra-port interfaces (not part of the core's port.h contract). */
void twi_init(void);
bool twi_response_consumed(void);   /* armed response fully read out; gates the BOOT jump */

#endif
