/* Host test double for port.h. Test-only: lives in test/, linked into every
 * test binary (each links all of core/, and core/update.c calls the port
 * contract). Asserts on every out-of-range flash access so an invariant
 * violation in the core fails loudly here even before the CBMC proofs. */
#include <assert.h>
#include <setjmp.h>
#include <stdlib.h>
#include <string.h>

#include "fake_port.h"
#include "updater/crc32.h"
#include "updater/update.h"

#define FAKE_PAGE_SIZE 128u
#define FAKE_APP_PAGES 32u
#define FAKE_REGION    (FAKE_PAGE_SIZE * FAKE_APP_PAGES)
#define FAKE_APP_LEN   200u

static uint8_t  g_flash[FAKE_REGION];
static uint32_t g_erase_count;
static uint32_t g_write_count;
static uint16_t g_max_touched;
static jmp_buf  g_jump_env;
static bool     g_jump_armed;
static uint32_t g_app_len;
static uint32_t g_app_crc;

static void touch(uint16_t page)
{
    if (page > g_max_touched)
        g_max_touched = page;
}

/* ---- port contract ---- */

void port_info(port_info_t *out)
{
    out->app_base     = 0x1000u;
    out->page_size    = 128u;
    out->app_pages    = 32u;
    out->device_id[0] = (uint8_t)'T';
    out->device_id[1] = (uint8_t)'E';
    out->device_id[2] = (uint8_t)'S';
    out->device_id[3] = (uint8_t)'T';
    out->bl_version   = 1u;
}

void port_flash_erase_page(uint16_t page)
{
    assert(page < FAKE_APP_PAGES);
    memset(&g_flash[(uint32_t)page * FAKE_PAGE_SIZE], 0xFF, FAKE_PAGE_SIZE);
    g_erase_count++;
    touch(page);
}

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    assert(page < FAKE_APP_PAGES);
    memcpy(&g_flash[(uint32_t)page * FAKE_PAGE_SIZE], data, FAKE_PAGE_SIZE);
    g_write_count++;
    touch(page);
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    assert(offset < FAKE_REGION);
    return g_flash[offset];
}

bool port_recv(uint8_t *buf, uint8_t *len)
{
    (void)buf;
    (void)len;
    return false;
}

void port_send(const uint8_t *buf, uint8_t len)
{
    (void)buf;
    (void)len;
}

uint16_t port_ticks_ms(void)
{
    return 0u;
}

bool port_entry_requested(void)
{
    return false;
}

void port_jump_to_app(void)
{
    if (g_jump_armed)
        longjmp(g_jump_env, 1);
    /* A jump outside fake_port_jump_catch means a test drove the gate
     * without a catcher — abort rather than fall through "never returns". */
    abort();
}

/* ---- test helpers ---- */

void fake_port_reset(void)
{
    memset(g_flash, 0xFF, sizeof g_flash);
    g_erase_count = 0u;
    g_write_count = 0u;
    g_max_touched = 0u;
    g_jump_armed  = false;
    g_app_len     = 0u;
    g_app_crc     = 0u;
}

uint32_t fake_port_erase_count(void) { return g_erase_count; }
uint32_t fake_port_write_count(void) { return g_write_count; }
uint16_t fake_port_max_touched_page(void) { return g_max_touched; }

void fake_port_flash_valid_app(void)
{
    uint8_t app[FAKE_APP_LEN];
    uint8_t pagebuf[FAKE_PAGE_SIZE];
    uint32_t c;
    uint32_t i;

    for (i = 0u; i < FAKE_APP_LEN; i++)
        app[i] = (uint8_t)((i * 7u + 3u) & 0xFFu);

    c = upd_crc32_init();
    for (i = 0u; i < FAKE_APP_LEN; i++)
        c = upd_crc32_update(c, app[i]);
    g_app_crc = upd_crc32_final(c);
    g_app_len = FAKE_APP_LEN;

    /* app bytes: pages 0 and 1, tail padded 0xFF */
    memcpy(pagebuf, app, FAKE_PAGE_SIZE);
    port_flash_write_page(0u, pagebuf);
    memset(pagebuf, 0xFF, sizeof pagebuf);
    memcpy(pagebuf, app + FAKE_PAGE_SIZE, FAKE_APP_LEN - FAKE_PAGE_SIZE);
    port_flash_write_page(1u, pagebuf);

    /* footer: last 16 bytes of the region = tail of the last page.
     * "OTAU" | len LE | crc LE | FF FF FF FF */
    memset(pagebuf, 0xFF, sizeof pagebuf);
    {
        uint8_t *f = &pagebuf[FAKE_PAGE_SIZE - 16u];
        uint8_t k;
        f[0] = 0x4Fu;
        f[1] = 0x54u;
        f[2] = 0x41u;
        f[3] = 0x55u;
        for (k = 0u; k < 4u; k++) {
            f[4u + k] = (uint8_t)((g_app_len >> (8u * k)) & 0xFFu);
            f[8u + k] = (uint8_t)((g_app_crc >> (8u * k)) & 0xFFu);
        }
    }
    port_flash_write_page((uint16_t)(FAKE_APP_PAGES - 1u), pagebuf);
}

void fake_port_valid_app_params(uint32_t *len, uint32_t *crc)
{
    *len = g_app_len;
    *crc = g_app_crc;
}

void fake_port_corrupt_app_byte(void)
{
    g_flash[5] = (uint8_t)(g_flash[5] ^ 0x01u);
}

bool fake_port_jump_catch(const port_info_t *info)
{
    if (setjmp(g_jump_env) != 0) {
        g_jump_armed = false;
        return true; /* port_jump_to_app was reached */
    }
    g_jump_armed = true;
    (void)upd_boot_if_valid(info);
    g_jump_armed = false;
    return false; /* gate refused; no jump */
}
