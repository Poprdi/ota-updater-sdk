#ifndef UPDATER_PORT_GEOM_H
#define UPDATER_PORT_GEOM_H
/* AVR64EA28 transport-independent port configuration: flash geometry,
 * identity, entry-window timing, buffer sizing. Shared by every AVR-EA
 * port (avr_ea_twi, avr_ea_uart) together with flash.c and entry.c in
 * this directory; transport-specific settings stay in each port's own
 * port_cfg.h. Extracted verbatim from avr_ea_twi/port_cfg.h in Task 13 —
 * the values and their audit records (avr_ea_twi/PORT_AUDIT.md) are
 * unchanged.
 *
 * All datasheet citations in the AVR-EA ports refer to:
 *   "AVR64EA28/32/48 Preliminary Data Sheet", Microchip DS40002443A.
 * Register/bit spellings were verified against the ioavr64ea28.h shipped
 * with avr-gcc 14.3.0 (see the PORT_AUDIT.md files for the per-symbol
 * record). */
#include <stdint.h>
#include "updater/proto.h"   /* UPD_RSP_MAX for the TX sizing below */

/* App region: everything above the 4 KiB boot section. Requires the
 * BOOTSIZE fuse programmed to 8 (8 x 512-byte blocks = 0x1000); see the
 * install line in each port's Makefile. DS40002443A section 11.3.1.1.2
 * (BOOTSIZE scaling), section 8.2 Figure 8-1 (64 KiB flash). */
#define UPDATER_APP_BASE    0x1000UL
#define UPDATER_PAGE_SIZE   128u    /* == PROGMEM_PAGE_SIZE, asserted in flash.c */
#define UPDATER_APP_PAGES   480u    /* (64 KiB - 4 KiB) / 128 */
_Static_assert((0x10000UL - UPDATER_APP_BASE) / UPDATER_PAGE_SIZE
               == UPDATER_APP_PAGES, "app geometry inconsistent");

#define UPDATER_DEVICE_ID   { 'A', 'E', '6', '4' }
#define UPDATER_BL_VERSION  1u
#define UPDATER_T_ENTRY_MS  300u

/* RX buffer: the largest legal wire frame is page_size + 5 = 133 bytes
 * (WRITE_PAGE: CMD + LEN + index(2) + page(128) + CRC8). The spec allows a
 * port to accept LEN up to page_size + 8, hence 136. Deliberately NOT the
 * conformance sim's 255-byte cap: that cap pins reference-CORE behavior,
 * not a wire guarantee (see conformance/sim/sim_port.h). Longer frames are
 * dropped at the wire (twi.c) or at the LEN byte (link_stream). */
#define UPDATER_RX_BUF_SIZE     136u
/* TX: largest response payload (proto.h UPD_RSP_MAX = ST + ECHO_MAX = 17)
 * plus framing. */
#define UPDATER_RSP_PAYLOAD_MAX UPD_RSP_MAX
#define UPDATER_TX_BUF_SIZE     (UPDATER_RSP_PAYLOAD_MAX + UPD_FRAME_OVERHEAD)

#endif
