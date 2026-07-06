#ifndef UPDATER_PORT_CFG_H
#define UPDATER_PORT_CFG_H
/* RP2350 (Pico 2 W) / UART0 port configuration + intra-port wiring.
 *
 * Citation key for every rp2350_uart source file (see PORT_AUDIT.md):
 *   `sdk:<path>` = pico-sdk 2.1.1 (commit bddd20f, "SDK 2.1.1 Release"),
 *   paths relative to the SDK root. Register descriptions are quoted from
 *   the SDK's generated `hardware/regs/` headers, which embed the RP2350
 *   datasheet register text verbatim.
 *
 * Geometry: the ROM boots the bootloader image placed at the start of
 * flash (XIP_BASE = 0x10000000); the bootloader owns the first 64 KiB
 * (enforced by memmap_bootloader.ld: FLASH LENGTH = 64k — the link FAILS
 * if the image outgrows the region). The app region is the next 1 MiB.
 * The Pico 2 W carries 4 MiB of QSPI flash (sdk:src/boards/include/boards/
 * pico2_w.h, PICO_FLASH_SIZE_BYTES = 4 MiB); everything above the app
 * region is untouched by the updater. */
#include <stdbool.h>
#include <stdint.h>
#include "uart_pump.h"      /* ../skeletons, on the CMake include path */
#include "updater/proto.h"  /* UPD_RSP_MAX for the TX sizing below */

/* ---- flash geometry ---------------------------------------------------- */

/* App region start, as an offset from the start of flash — the address
 * form flash_range_erase/program take (sdk:src/rp2_common/hardware_flash/
 * include/hardware/flash.h: "flash_offs ... offset into flash"). */
#define UPDATER_APP_FLASH_OFFSET 0x10000u

/* App region start as a CPU (XIP window) address — what
 * port_flash_read_byte and the jump gate dereference (a port-internal
 * address: INFO carries only page_size/app_pages, never a base address).
 * XIP_BASE is 0x10000000 (sdk:src/rp2350/hardware_regs/include/hardware/
 * regs/addressmap.h). */
#define UPDATER_APP_BASE    (0x10000000u + UPDATER_APP_FLASH_OFFSET)

/* Protocol pages are 128 bytes — half the QSPI flash's 256-byte program
 * page (FLASH_PAGE_SIZE) and 1/32 of its 4096-byte erase sector
 * (FLASH_SECTOR_SIZE); flash.c coalesces/maps accordingly. */
#define UPDATER_PAGE_SIZE   128u
#define UPDATER_APP_PAGES   8192u   /* 1 MiB / 128 */
_Static_assert((uint32_t)UPDATER_PAGE_SIZE * UPDATER_APP_PAGES
               == 0x100000u, "app region must be exactly 1 MiB");

#define UPDATER_DEVICE_ID   { 'R', 'P', '2', '3' }
#define UPDATER_BL_VERSION  1u
#define UPDATER_T_ENTRY_MS  300u

/* ---- buffer sizing (same rationale as avr_ea_common/port_geom.h) ------ */

/* Largest legal wire frame is page_size + 5 = 133 (WRITE_PAGE: CMD + LEN +
 * index(2) + page(128) + CRC8); the spec allows accepting LEN up to
 * page_size + 8, hence 136. Longer frames are dropped at the LEN byte by
 * link_stream. */
#define UPDATER_RX_BUF_SIZE     136u
/* TX: largest response payload (proto.h UPD_RSP_MAX = ST + ECHO_MAX = 17)
 * plus framing. */
#define UPDATER_RSP_PAYLOAD_MAX UPD_RSP_MAX
#define UPDATER_TX_BUF_SIZE     (UPDATER_RSP_PAYLOAD_MAX + UPD_FRAME_OVERHEAD)

/* ---- build parameters -------------------------------------------------- */

/* Set from CMake (-DUPDATER_UART_BAUD=...); any rate the PL011 divisor can
 * express from clk_peri works — uart_init returns the achieved rate
 * (sdk:src/rp2_common/hardware_uart/uart.c uart_set_baudrate). The host
 * CLI supports 9600..230400. */
#ifndef UPDATER_UART_BAUD
#  define UPDATER_UART_BAUD 115200u
#endif

/* ---- intra-port interfaces (not part of the core's port.h contract) ---- */

/* Names carry an updater_ prefix: the pico-sdk already owns uart_init(). */
void updater_uart_init(void);
void updater_uart_tx_drain(void);   /* response fully on the wire; gates BOOT */
void updater_entry_capture(void);   /* read+clear the scratch pair; first
                                       statement of main() */
extern const uart_pump_ops_t uart0_pump_ops;

#endif
