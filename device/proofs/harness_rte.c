/* CBMC harness — RTE freedom across the whole device core.
 *
 * No property asserts of substance: CBMC's automatic checks (--bounds-check
 * --pointer-check --conversion-check --div-by-zero-check
 * --signed-overflow-check --unsigned-overflow-check --unwinding-assertions)
 * carry the proof. Every public core entry point is driven with nondet
 * inputs; buffers are malloc'd at their EXACT declared length so any
 * off-by-one read/write is a pointer-check counterexample, not a silent
 * in-bounds access to slack space.
 *
 * Model bounds (see README): codec buffers <= 24 bytes, crc8 input <= 16
 * bytes, port geometry per PROOF_SMALL_MODEL. Frame len and rsp_cap for
 * upd_handle range over the FULL uint8_t domain.
 */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#include "updater/update.h"
#include "updater/proto.h"
#include "updater/crc8.h"
#include "updater/crc32.h"

#ifdef PROOF_SMALL_MODEL
#define MODEL_MAX_PAGE_SIZE 16u
#define MODEL_MAX_APP_PAGES 4u
#else
#error "harness tuned for PROOF_SMALL_MODEL (unwind bound 70); define it"
#endif

#define MODEL_MAX_BUF 24u   /* codec buffers; covers all header/CRC edge cases */

uint8_t  nondet_u8(void);
uint16_t nondet_u16(void);
uint32_t nondet_u32(void);
bool     nondet_bool(void);

static uint16_t g_page_size;
static uint16_t g_app_pages;

/* ---- port model (no invariant asserts here — RTEs only) ---------------- */

void port_info(port_info_t *out)
{
    out->app_base     = 0x1000u;
    out->page_size    = g_page_size;
    out->app_pages    = g_app_pages;
    out->device_id[0] = nondet_u8();
    out->device_id[1] = nondet_u8();
    out->device_id[2] = nondet_u8();
    out->device_id[3] = nondet_u8();
    out->bl_version   = nondet_u8();
}

void port_flash_erase_page(uint16_t page) { (void)page; }

static uint8_t g_sink;

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    (void)page;
    /* core contract: data has page_size readable bytes */
    for (uint16_t i = 0; i < g_page_size; i++)
        g_sink ^= data[i];
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    (void)offset;
    return nondet_u8();
}

void port_jump_to_app(void) { }

/* ---- proof entry point -------------------------------------------------- */

int main(void)
{
    g_page_size = nondet_u16();
    g_app_pages = nondet_u16();
    __CPROVER_assume(g_page_size <= MODEL_MAX_PAGE_SIZE);
    __CPROVER_assume(g_app_pages <= MODEL_MAX_APP_PAGES);

    /* --- upd_crc8 -------------------------------------------------------- */
    {
        uint8_t n = nondet_u8();
        __CPROVER_assume(n <= 16u);
        uint8_t *p = malloc(n);
        __CPROVER_assume(p != NULL);
        (void)upd_crc8(p, n);
    }

    /* --- upd_crc32_* ------------------------------------------------------ */
    {
        uint32_t c = upd_crc32_init();
        c = upd_crc32_update(nondet_u32(), nondet_u8());
        (void)upd_crc32_final(c);
    }

    /* --- upd_frame_parse -------------------------------------------------- */
    {
        uint8_t buflen = nondet_u8();
        __CPROVER_assume(buflen <= MODEL_MAX_BUF);
        uint8_t *buf = malloc(buflen);
        __CPROVER_assume(buf != NULL);
        upd_frame_t f;
        if (upd_frame_parse(buf, buflen, &f)) {
            assert((unsigned)f.len + UPD_FRAME_OVERHEAD == buflen);
            assert(f.payload == buf + 2);
            assert(f.cmd == buf[0]);
        }
    }

    /* --- upd_frame_build -------------------------------------------------- */
    {
        uint8_t cap = nondet_u8();
        __CPROVER_assume(cap <= MODEL_MAX_BUF);
        uint8_t *buf = malloc(cap);
        __CPROVER_assume(buf != NULL);
        uint8_t  len = nondet_u8();               /* full range: wrap cases */
        uint8_t *payload = malloc(len);
        __CPROVER_assume(payload != NULL);
        uint8_t n = upd_frame_build(buf, cap, nondet_u8(), payload, len);
        assert(n == 0u || n == (unsigned)len + UPD_FRAME_OVERHEAD);
    }

    /* --- upd_handle (all commands, both erased states) -------------------- */
    {
        upd_session_t s;
        upd_init(&s);
        s.erased       = nondet_bool();
        s.boot_pending = nondet_bool();

        upd_frame_t req;
        req.cmd = nondet_u8();
        req.len = nondet_u8();
        uint8_t *payload = malloc(req.len);
        __CPROVER_assume(payload != NULL);
        req.payload = payload;

        uint8_t  rsp_cap = nondet_u8();
        uint8_t *rsp     = malloc(rsp_cap);
        __CPROVER_assume(rsp != NULL);

        uint8_t n = upd_handle(&s, &req, rsp, rsp_cap);
        assert(n <= rsp_cap);
    }
    return 0;
}
