#ifndef UPDATER_PORT_H
#define UPDATER_PORT_H
/* Port contract (complete) — verbatim from the design spec, §Device side.
 * The ONLY thing a port implements. The core calls flash functions only
 * with page < app_pages (proven). The core never addresses the boot
 * region: the port exposes the app region only — confinement is
 * architectural AND proven. */
#include <stdbool.h>
#include <stdint.h>

typedef struct {
    uint32_t app_base;      /* byte address of app region start            */
    uint16_t page_size;
    uint16_t app_pages;
    uint8_t  device_id[4];  /* project-assigned, reported in INFO          */
    uint8_t  bl_version;
} port_info_t;

void     port_info(port_info_t *out);
void     port_flash_erase_page(uint16_t page);
void     port_flash_write_page(uint16_t page, const uint8_t *data);
uint8_t  port_flash_read_byte(uint32_t offset);      /* offset within app region */
bool     port_recv(uint8_t *buf, uint8_t *len);      /* whole frame or nothing   */
void     port_send(const uint8_t *buf, uint8_t len);
uint16_t port_ticks_ms(void);
bool     port_entry_requested(void);                 /* .noinit magic pair       */
void     port_jump_to_app(void);                     /* never returns            */

#endif
