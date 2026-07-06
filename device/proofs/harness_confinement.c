/* CBMC harness — Invariant 1: flash confinement.
 *
 * Model: a nondet port (page_size/app_pages bounded by PROOF_SMALL_MODEL),
 * a session in a nondet state (both values of `erased`), and one fully
 * nondet request frame (cmd and len over their FULL 8-bit range — a
 * superset of what the brief requires; payload is exactly `len` malloc'd
 * nondet bytes, so any read past the frame is also caught).
 *
 * The port stubs ASSERT the invariant:
 *   - erase/write:  page   < app_pages
 *   - read:         offset < page_size * app_pages
 *   - jump:         unreachable from upd_handle (BOOT only sets a flag)
 *
 * CBMC explores every path through upd_handle; any flash access outside
 * the app region is a counterexample.
 */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#include "updater/update.h"

#ifdef PROOF_SMALL_MODEL
#define MODEL_MAX_PAGE_SIZE 16u
#define MODEL_MAX_APP_PAGES 4u
#else
#error "harness tuned for PROOF_SMALL_MODEL (unwind bound 70); define it"
#endif

uint8_t  nondet_u8(void);
uint16_t nondet_u16(void);
bool     nondet_bool(void);

static uint16_t g_page_size;
static uint16_t g_app_pages;

/* ---- port model -------------------------------------------------------- */

void port_info(port_info_t *out)
{
    out->page_size    = g_page_size;
    out->app_pages    = g_app_pages;
    out->device_id[0] = nondet_u8();
    out->device_id[1] = nondet_u8();
    out->device_id[2] = nondet_u8();
    out->device_id[3] = nondet_u8();
    out->bl_version   = nondet_u8();
}

void port_flash_erase_page(uint16_t page)
{
    assert(page < g_app_pages);                       /* INVARIANT 1 */
}

static uint8_t g_sink;

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    assert(page < g_app_pages);                       /* INVARIANT 1 */
    /* Core contract: data must have page_size readable bytes. Reading
     * them here turns any short buffer into a pointer-check failure. */
    for (uint16_t i = 0; i < g_page_size; i++)
        g_sink ^= data[i];
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    assert(offset < (uint32_t)g_page_size * g_app_pages);  /* INVARIANT 1 */
    return nondet_u8();
}

void port_jump_to_app(void)
{
    assert(0);   /* upd_handle must never jump; only upd_boot_if_valid may */
}

/* ---- proof entry point -------------------------------------------------- */

int main(void)
{
    g_page_size = nondet_u16();
    g_app_pages = nondet_u16();
    __CPROVER_assume(g_page_size <= MODEL_MAX_PAGE_SIZE);
    __CPROVER_assume(g_app_pages <= MODEL_MAX_APP_PAGES);

    upd_session_t s;
    upd_init(&s);
    s.erased       = nondet_bool();   /* explore both erased states */
    s.boot_pending = nondet_bool();

    upd_frame_t req;
    req.cmd = nondet_u8();
    req.len = nondet_u8();
    uint8_t *payload = malloc(req.len);   /* exactly len readable bytes */
    __CPROVER_assume(payload != NULL);
    req.payload = payload;

    uint8_t  rsp_cap = nondet_u8();
    uint8_t *rsp     = malloc(rsp_cap);   /* exactly rsp_cap writable bytes */
    __CPROVER_assume(rsp != NULL);

    uint8_t n = upd_handle(&s, &req, rsp, rsp_cap);

    /* documented return contract */
    assert(n <= rsp_cap);
    assert(rsp_cap == 0u ? n == 0u : n >= 1u);
    return 0;
}
