/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#include <string.h>
#include "check.h"
#include "updater/crc32.h"
#include "updater/update.h"
#include "fake_port.h"

/* helper: run one command through parse-less direct dispatch */
static uint8_t run(upd_session_t *s, uint8_t cmd, const uint8_t *pl,
                   uint8_t len, uint8_t *rsp)
{
    upd_frame_t f = { cmd, len, pl };
    return upd_handle(s, &f, rsp, 255);
}

/* Assemble little-endian explicitly: the wire format is LE by spec, and the
 * brief's `memcpy(vp, &len32, 4)` only matched it on LE hosts. This keeps
 * the test honest on any C11 platform. */
static void put_le32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v & 0xFFu);
    p[1] = (uint8_t)((v >> 8) & 0xFFu);
    p[2] = (uint8_t)((v >> 16) & 0xFFu);
    p[3] = (uint8_t)((v >> 24) & 0xFFu);
}

int main(void)
{
    upd_session_t s;
    uint8_t rsp[255];
    static const uint8_t erase_magic[] = { 0x45, 0x52, 0x41, 0x53 };

    /* INFO: layout per spec — ST, proto, bl_ver, id[4], page_size LE,
       app_pages LE, app_valid */
    fake_port_reset();
    upd_init(&s);
    CHECK(run(&s, UPD_CMD_INFO, 0, 0, rsp) == 12);
    CHECK(rsp[0] == UPD_ST_OK && rsp[1] == UPD_PROTO_VERSION);
    CHECK(rsp[7] == 128 && rsp[8] == 0);          /* page_size 128 LE */
    CHECK(rsp[9] == 32 && rsp[10] == 0);          /* app_pages 32 LE  */
    CHECK(rsp[11] == 0);                          /* blank flash: no app */

    /* ERASE without magic refused, nothing erased */
    CHECK(run(&s, UPD_CMD_ERASE_APP, (const uint8_t *)"XXXX", 4, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_BAD_MAGIC && fake_port_erase_count() == 0);

    /* WRITE before ERASE refused */
    uint8_t page[130] = { 0 };                     /* idx LE + 128 data */
    CHECK(run(&s, UPD_CMD_WRITE_PAGE, page, 130, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_NOT_ERASED && fake_port_write_count() == 0);

    /* ERASE with magic erases exactly app_pages pages, in-range only */
    CHECK(run(&s, UPD_CMD_ERASE_APP, erase_magic, 4, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_OK);
    CHECK(fake_port_erase_count() == 32 && fake_port_max_touched_page() < 32);

    /* WRITE wrong length refused; out-of-range page refused */
    CHECK(run(&s, UPD_CMD_WRITE_PAGE, page, 100, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_BAD_FRAME);
    page[0] = 32; page[1] = 0;
    CHECK(run(&s, UPD_CMD_WRITE_PAGE, page, 130, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_OUT_OF_RANGE && fake_port_write_count() == 0);

    /* happy path: write a tiny app + footer, VERIFY, BOOT jumps */
    fake_port_flash_valid_app();   /* helper writes app bytes + correct footer via the port */
    uint32_t len32, crc32v; fake_port_valid_app_params(&len32, &crc32v);
    uint8_t vp[8];
    put_le32(vp, len32); put_le32(vp + 4, crc32v);   /* explicit LE: any host */
    CHECK(run(&s, UPD_CMD_VERIFY, vp, 8, rsp) == 1 && rsp[0] == UPD_ST_OK);

    /* VERIFY with wrong CRC → BAD_CRC; oversize length → OUT_OF_RANGE */
    vp[4] = (uint8_t)(vp[4] ^ 0xFFu);
    CHECK(run(&s, UPD_CMD_VERIFY, vp, 8, rsp) == 1 && rsp[0] == UPD_ST_BAD_CRC);
    uint32_t huge = 32u * 128u - 15u; put_le32(vp, huge);
    CHECK(run(&s, UPD_CMD_VERIFY, vp, 8, rsp) == 1 && rsp[0] == UPD_ST_OUT_OF_RANGE);

    /* BOOT with valid footer sets boot_pending; upd_boot_if_valid jumps */
    CHECK(run(&s, UPD_CMD_BOOT, 0, 0, rsp) == 1 && rsp[0] == UPD_ST_OK);
    CHECK(s.boot_pending);
    CHECK(fake_port_jump_catch(&s.info));          /* longjmp-based: true = jumped */

    /* corrupt one app byte → BOOT refused, no jump */
    fake_port_corrupt_app_byte();
    upd_init(&s);
    CHECK(run(&s, UPD_CMD_BOOT, 0, 0, rsp) == 1 && rsp[0] == UPD_ST_NO_APP);
    CHECK(!fake_port_jump_catch(&s.info));

    /* ECHO round-trips <=16 bytes, refuses more */
    const uint8_t eb[] = { 1, 2, 3 };
    CHECK(run(&s, UPD_CMD_ECHO, eb, 3, rsp) == 4);
    CHECK(rsp[0] == UPD_ST_OK && memcmp(rsp + 1, eb, 3) == 0);
    uint8_t big[17] = { 0 };
    CHECK(run(&s, UPD_CMD_ECHO, big, 17, rsp) == 1 && rsp[0] == UPD_ST_BAD_FRAME);

    /* unknown command */
    CHECK(run(&s, 0x55, 0, 0, rsp) == 1 && rsp[0] == UPD_ST_BAD_CMD);

    /* ---- fresh session: FSM-level WRITE success + boundary coverage ---- */
    fake_port_reset();
    upd_init(&s);
    CHECK(run(&s, UPD_CMD_ERASE_APP, erase_magic, 4, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_OK && fake_port_erase_count() == 32);

    /* WRITE success at idx 0 through upd_handle: ST_OK, write counter bumps,
       and the 128 pattern bytes land at flash offset 0 (read back through
       the port contract — fake flash is reachable via port_flash_read_byte) */
    uint8_t wp[130];
    wp[0] = 0x00; wp[1] = 0x00;
    for (uint32_t i = 0; i < 128u; i++)
        wp[2u + i] = (uint8_t)((i * 3u + 1u) & 0xFFu);
    CHECK(run(&s, UPD_CMD_WRITE_PAGE, wp, 130, rsp) == 1 && rsp[0] == UPD_ST_OK);
    CHECK(fake_port_write_count() == 1);
    {
        int landed = 1;
        for (uint32_t i = 0; i < 128u; i++)
            if (port_flash_read_byte(i) != (uint8_t)((i * 3u + 1u) & 0xFFu))
                landed = 0;
        CHECK(landed);
    }

    /* boundary accept: idx == app_pages - 1 (31) lands at offset 31*128 */
    wp[0] = 31; wp[1] = 0;
    for (uint32_t i = 0; i < 128u; i++)
        wp[2u + i] = (uint8_t)((0xA0u ^ i) & 0xFFu);
    CHECK(run(&s, UPD_CMD_WRITE_PAGE, wp, 130, rsp) == 1 && rsp[0] == UPD_ST_OK);
    CHECK(fake_port_write_count() == 2);
    {
        int landed = 1;
        for (uint32_t i = 0; i < 128u; i++)
            if (port_flash_read_byte(31u * 128u + i) !=
                (uint8_t)((0xA0u ^ i) & 0xFFu))
                landed = 0;
        CHECK(landed);
    }

    /* ERASE with truncated magic (len 3) refused; nothing (re)erased */
    CHECK(run(&s, UPD_CMD_ERASE_APP, erase_magic, 3, rsp) == 1);
    CHECK(rsp[0] == UPD_ST_BAD_MAGIC && fake_port_erase_count() == 32);

    /* VERIFY with wrong payload length refused */
    CHECK(run(&s, UPD_CMD_VERIFY, vp, 7, rsp) == 1 &&
          rsp[0] == UPD_ST_BAD_FRAME);

    /* VERIFY at exactly length == region - 16 (4080): proves the bound
       accepts equality AND exercises a full-length CRC match. Expected CRC
       is computed here over the bytes THIS test put in flash via the FSM
       (page 0 pattern, page 31 pattern, 0xFF fill between) — independent of
       the core's flash reader. */
    {
        uint32_t c = upd_crc32_init();
        for (uint32_t off = 0; off < 4080u; off++) {
            uint8_t b;
            if (off < 128u)
                b = (uint8_t)((off * 3u + 1u) & 0xFFu);
            else if (off >= 31u * 128u)
                b = (uint8_t)((0xA0u ^ (off - 31u * 128u)) & 0xFFu);
            else
                b = 0xFFu;
            c = upd_crc32_update(c, b);
        }
        put_le32(vp, 4080u);
        put_le32(vp + 4, upd_crc32_final(c));
        CHECK(run(&s, UPD_CMD_VERIFY, vp, 8, rsp) == 1 &&
              rsp[0] == UPD_ST_OK);
    }

    /* ECHO at exactly UPD_ECHO_MAX (16): accepted, payload round-trips */
    uint8_t e16[16];
    for (uint8_t i = 0; i < 16u; i++)
        e16[i] = (uint8_t)(0x30u + i);
    CHECK(run(&s, UPD_CMD_ECHO, e16, 16, rsp) == 17);
    CHECK(rsp[0] == UPD_ST_OK && memcmp(rsp + 1, e16, 16) == 0);

    /* INFO reports app_valid == 1 once a valid app + footer is in flash */
    fake_port_flash_valid_app();
    CHECK(run(&s, UPD_CMD_INFO, 0, 0, rsp) == 12);
    CHECK(rsp[0] == UPD_ST_OK && rsp[11] == 1);

    TEST_RESULT("update");
}
