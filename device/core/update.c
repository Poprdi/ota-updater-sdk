/* Update session FSM — the proof-bearing core.
 *
 * Invariant 1 (confinement): every port_flash_* call in this file passes a
 * page argument p with p < s->info.app_pages. ACSL asserts at each call
 * site make this a proof obligation, not a convention.
 *
 * Invariant 2 (jump gating): port_jump_to_app is called from exactly one
 * site in the SDK — upd_boot_if_valid below — dominated by
 * upd_app_valid() == true.
 *
 * Portability notes:
 *  - All bounds arithmetic is done in uint32_t. page_size and app_pages are
 *    uint16_t; page_size * app_pages MUST NOT be left to integer promotion
 *    (on a 16-bit int platform 512 * 128 already overflows int, which is
 *    undefined behaviour). region_bytes() casts BEFORE multiplying.
 *  - All multibyte wire values are assembled/parsed byte-by-byte in
 *    little-endian order; never via memcpy/type punning, so the core is
 *    endianness-agnostic.
 */
#include <assert.h>

#include "updater/update.h"
#include "updater/crc32.h"

/*@ requires \valid_read(i);
    assigns \nothing;
    ensures \result == (uint32_t)i->page_size * i->app_pages; */
static uint32_t region_bytes(const port_info_t *i)
{
    /* cast BEFORE the multiply: u16*u16 would promote to int */
    return (uint32_t)i->page_size * i->app_pages;
}

/*@ requires \valid_read(info);
    assigns \nothing; */
static uint32_t flash_crc(const port_info_t *info, uint32_t length)
{
    uint32_t c = upd_crc32_init();
    (void)info;
    /*@ loop invariant 0 <= off <= length;
        loop assigns off, c;
        loop variant length - off; */
    for (uint32_t off = 0; off < length; off++)
        c = upd_crc32_update(c, port_flash_read_byte(off));
    return upd_crc32_final(c);
}

/*@ requires \valid_read(info);
    assigns \nothing; */
bool upd_app_valid(const port_info_t *info)
{
    /* A region smaller than the 16-byte footer cannot hold a valid app.
     * This also guards the region - 16 subtractions below (and the len
     * bound check) against unsigned wrap if a port reports a degenerate
     * region (page_size or app_pages of 0, or region < 16). */
    if (region_bytes(info) < 16u)
        return false;
    uint32_t base = region_bytes(info) - 16u;
    if (port_flash_read_byte(base + 0) != 0x4Fu ||
        port_flash_read_byte(base + 1) != 0x54u ||
        port_flash_read_byte(base + 2) != 0x41u ||
        port_flash_read_byte(base + 3) != 0x55u)
        return false;
    uint32_t len = 0, crc = 0;
    /*@ loop invariant 0 <= i <= 4;
        loop assigns i, len, crc;
        loop variant 4 - i; */
    for (uint8_t i = 0; i < 4; i++) {
        len |= (uint32_t)port_flash_read_byte(base + 4u + i) << (8u * i);
        crc |= (uint32_t)port_flash_read_byte(base + 8u + i) << (8u * i);
    }
    if (len > region_bytes(info) - 16u)
        return false;
    return flash_crc(info, len) == crc;
}

/* Invariant 2 (jump gating): port_jump_to_app is called from exactly one
 * site in the SDK, below, dominated by upd_app_valid() == true. */
bool upd_boot_if_valid(const port_info_t *info)
{
    if (!upd_app_valid(info))
        return false;
    port_jump_to_app();
    return true;   /* unreachable on real ports; reached by test doubles */
}

void upd_init(upd_session_t *s)
{
    port_info(&s->info);
    s->erased       = false;
    s->boot_pending = false;
}

/* ---- command handlers -------------------------------------------------- */

static uint8_t handle_info(const upd_session_t *s, uint8_t *rsp,
                           uint8_t rsp_cap)
{
    /* ST, proto, bl_ver, id[4], page_size LE, app_pages LE, app_valid */
    if (rsp_cap < 12u) {
        rsp[0] = UPD_ST_BAD_FRAME;
        return 1u;
    }
    rsp[0]  = UPD_ST_OK;
    rsp[1]  = (uint8_t)UPD_PROTO_VERSION;
    rsp[2]  = s->info.bl_version;
    rsp[3]  = s->info.device_id[0];
    rsp[4]  = s->info.device_id[1];
    rsp[5]  = s->info.device_id[2];
    rsp[6]  = s->info.device_id[3];
    rsp[7]  = (uint8_t)(s->info.page_size & 0xFFu);         /* LE lo */
    rsp[8]  = (uint8_t)((s->info.page_size >> 8) & 0xFFu);  /* LE hi */
    rsp[9]  = (uint8_t)(s->info.app_pages & 0xFFu);         /* LE lo */
    rsp[10] = (uint8_t)((s->info.app_pages >> 8) & 0xFFu);  /* LE hi */
    rsp[11] = (uint8_t)(upd_app_valid(&s->info) ? 1u : 0u);
    return 12u;
}

static uint8_t handle_erase(upd_session_t *s, const upd_frame_t *req,
                            uint8_t *rsp)
{
    /* the magic IS the frame: any length/content mismatch is BAD_MAGIC */
    if (req->len != 4u ||
        req->payload[0] != 0x45u ||   /* 'E' */
        req->payload[1] != 0x52u ||   /* 'R' */
        req->payload[2] != 0x41u ||   /* 'A' */
        req->payload[3] != 0x53u) {   /* 'S' */
        rsp[0] = UPD_ST_BAD_MAGIC;
        return 1u;
    }
    /*@ loop invariant 0 <= p <= s->info.app_pages;
        loop assigns p;
        loop variant s->info.app_pages - p; */
    for (uint16_t p = 0u; p < s->info.app_pages; p++) {
        /*@ assert p < s->info.app_pages; */   /* Invariant 1 call site */
        assert(p < s->info.app_pages);
        port_flash_erase_page(p);
    }
    s->erased = true;
    rsp[0] = UPD_ST_OK;
    return 1u;
}

static uint8_t handle_write(upd_session_t *s, const upd_frame_t *req,
                            uint8_t *rsp)
{
    if (!s->erased) {
        rsp[0] = UPD_ST_NOT_ERASED;
        return 1u;
    }
    /* payload = page index LE (2) + one full page of data */
    if ((uint32_t)req->len != (uint32_t)s->info.page_size + 2u) {
        rsp[0] = UPD_ST_BAD_FRAME;
        return 1u;
    }
    uint16_t idx = (uint16_t)((uint16_t)req->payload[0] |
                              (uint16_t)((uint16_t)req->payload[1] << 8));
    if (idx >= s->info.app_pages) {
        rsp[0] = UPD_ST_OUT_OF_RANGE;
        return 1u;
    }
    /*@ assert idx < s->info.app_pages; */     /* Invariant 1 call site */
    assert(idx < s->info.app_pages);
    port_flash_write_page(idx, req->payload + 2);
    rsp[0] = UPD_ST_OK;
    return 1u;
}

static uint8_t handle_verify(const upd_session_t *s, const upd_frame_t *req,
                             uint8_t *rsp)
{
    if (req->len != 8u) {
        rsp[0] = UPD_ST_BAD_FRAME;
        return 1u;
    }
    uint32_t length = 0u, crc = 0u;
    /*@ loop invariant 0 <= i <= 4;
        loop assigns i, length, crc;
        loop variant 4 - i; */
    for (uint8_t i = 0u; i < 4u; i++) {
        length |= (uint32_t)req->payload[i]      << (8u * i);
        crc    |= (uint32_t)req->payload[4u + i] << (8u * i);
    }
    /* region < 16 guard mirrors upd_app_valid: without it the subtraction
     * wraps for a degenerate port geometry and a huge length would pass,
     * sending flash_crc out of the app region (found by CBMC). */
    if (region_bytes(&s->info) < 16u ||
        length > region_bytes(&s->info) - 16u) {
        rsp[0] = UPD_ST_OUT_OF_RANGE;
        return 1u;
    }
    rsp[0] = (flash_crc(&s->info, length) == crc)
                 ? (uint8_t)UPD_ST_OK
                 : (uint8_t)UPD_ST_BAD_CRC;
    return 1u;
}

static uint8_t handle_echo(const upd_frame_t *req, uint8_t *rsp,
                           uint8_t rsp_cap)
{
    if (req->len > UPD_ECHO_MAX ||
        (uint32_t)req->len + 1u > (uint32_t)rsp_cap) {
        rsp[0] = UPD_ST_BAD_FRAME;
        return 1u;
    }
    rsp[0] = UPD_ST_OK;
    /*@ loop invariant 0 <= i <= req->len;
        loop assigns i, rsp[1 .. req->len];
        loop variant req->len - i; */
    for (uint8_t i = 0u; i < req->len; i++)
        rsp[1u + i] = req->payload[i];
    return (uint8_t)(req->len + 1u);
}

/*@ requires \valid(s) && \valid_read(req) && \valid(rsp + (0 .. rsp_cap - 1));
    ensures 0 <= \result <= rsp_cap; */
uint8_t upd_handle(upd_session_t *s, const upd_frame_t *req,
                   uint8_t *rsp, uint8_t rsp_cap)
{
    if (rsp_cap == 0u)
        return 0u;   /* nowhere to put even a status byte */

    switch (req->cmd) {
    case UPD_CMD_INFO:
        return handle_info(s, rsp, rsp_cap);
    case UPD_CMD_ERASE_APP:
        return handle_erase(s, req, rsp);
    case UPD_CMD_WRITE_PAGE:
        return handle_write(s, req, rsp);
    case UPD_CMD_VERIFY:
        return handle_verify(s, req, rsp);
    case UPD_CMD_BOOT:
        /* Main replies first, then calls upd_boot_if_valid — which
         * re-checks. The gate never trusts this stale flag. */
        if (upd_app_valid(&s->info)) {
            s->boot_pending = true;
            rsp[0] = UPD_ST_OK;
        } else {
            rsp[0] = UPD_ST_NO_APP;
        }
        return 1u;
    case UPD_CMD_ECHO:
        return handle_echo(req, rsp, rsp_cap);
    default:
        rsp[0] = UPD_ST_BAD_CMD;
        return 1u;
    }
}
