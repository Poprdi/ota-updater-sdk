#ifndef UPDATER_PORT_CFG_H
#define UPDATER_PORT_CFG_H
/* AVR64EA28/USART0 port configuration + intra-port wiring. Geometry,
 * buffer sizing and the shared datasheet/citation preamble live in the
 * common header ../avr_ea_common/port_geom.h (shared with avr_ea_twi,
 * together with flash.c and entry.c); only UART-specific pieces are here. */
#include <stdbool.h>
#include <stdint.h>
#include "../avr_ea_common/port_geom.h"
#include "uart_pump.h"      /* ../skeletons, on the Makefile include path */

/* Build parameter; any standard rate the BAUD register can express at
 * 20 MHz works (uart.c refuses out-of-range values at compile time). */
#ifndef UPDATER_UART_BAUD
#  define UPDATER_UART_BAUD 115200UL
#endif

/* CLK_PER after main() clears the prescaler = OSCHF at the FUSE.OSCCFG
 * factory default of 20 MHz (same premise as the TWI port, audit row M1).
 * The BAUD register value scales from this: a part fused to another OSCHF
 * frequency talks at the wrong rate — see PORT_AUDIT bring-up item 1. */
#define UPDATER_F_CLK_PER 20000000ULL

/* Intra-port interfaces (not part of the core's port.h contract). */
extern const uart_pump_ops_t uart0_pump_ops;
void uart_init(void);
void uart_tx_drain(void);   /* response fully on the wire; gates the BOOT jump */

#endif
