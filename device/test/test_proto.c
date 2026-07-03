#include <string.h>
#include "check.h"
#include "updater/proto.h"
#include "updater/crc8.h"

int main(void)
{
    upd_frame_t f;

    /* golden INFO request parses */
    const uint8_t info[] = { 0x01, 0x00, 0x15 };
    CHECK(upd_frame_parse(info, 3, &f));
    CHECK(f.cmd == UPD_CMD_INFO && f.len == 0);

    /* golden ECHO request parses, payload aliased not copied */
    const uint8_t echo[] = { 0x06, 0x02, 0xAA, 0xBB, 0x10 };
    CHECK(upd_frame_parse(echo, 5, &f));
    CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2 && f.payload == echo + 2);

    /* corrupt CRC, truncated, LEN/buflen mismatch, empty all rejected */
    const uint8_t bad[] = { 0x06, 0x02, 0xAA, 0xBB, 0x11 };
    CHECK(!upd_frame_parse(bad, 5, &f));
    CHECK(!upd_frame_parse(echo, 4, &f));
    CHECK(!upd_frame_parse(echo, 6, &f));      /* trailing garbage counts as mismatch */
    CHECK(!upd_frame_parse(info, 2, &f));
    CHECK(!upd_frame_parse(info, 0, &f));

    /* build produces the golden bytes; round-trips */
    uint8_t buf[64];
    const uint8_t pl[] = { 0xAA, 0xBB };
    CHECK(upd_frame_build(buf, sizeof buf, 0x06, pl, 2) == 5);
    CHECK(memcmp(buf, echo, 5) == 0);
    CHECK(upd_frame_build(buf, sizeof buf, 0x01, (const uint8_t *)0, 0) == 3);
    CHECK(buf[0] == 0x01 && buf[1] == 0x00 && buf[2] == 0x15);

    /* build refuses a too-small buffer instead of overflowing it */
    CHECK(upd_frame_build(buf, 4, 0x06, pl, 2) == 0);
    /* cap exactly equal to the frame size is enough */
    CHECK(upd_frame_build(buf, 5, 0x06, pl, 2) == 5);
    CHECK(memcmp(buf, echo, 5) == 0);

    /* ---- u8 boundary: len = 252 is the largest representable frame ---- */
    uint8_t big[255];
    uint8_t pay[252];
    for (unsigned i = 0; i < sizeof pay; i++)
        pay[i] = (uint8_t)i;

    CHECK(upd_frame_build(big, 255, 0x03, pay, 252) == 255);
    CHECK(big[0] == 0x03 && big[1] == 252);
    CHECK(memcmp(big + 2, pay, 252) == 0);
    CHECK(big[254] == upd_crc8(big, 254));
    CHECK(upd_frame_parse(big, 255, &f));
    CHECK(f.cmd == 0x03 && f.len == 252 && f.payload == big + 2);

    /* one byte short of the max frame is refused, not overflowed */
    CHECK(upd_frame_build(big, 254, 0x03, pay, 252) == 0);

    /* ---- u8 boundary: len = 253/254/255 wrap len+3 past 255 ---- */
    /* build must refuse regardless of cap (total would wrap to 0/1/2) */
    uint8_t huge[255];
    memset(huge, 0x5A, sizeof huge);
    CHECK(upd_frame_build(big, 255, 0x03, huge, 253) == 0);
    CHECK(upd_frame_build(big, 255, 0x03, huge, 254) == 0);
    CHECK(upd_frame_build(big, 255, 0x03, huge, 255) == 0);

    /* parse must reject a LEN byte whose frame could never fit a u8 buflen */
    for (unsigned lenbyte = 253; lenbyte <= 255; lenbyte++) {
        for (unsigned n = 3; n <= 255; n++) {
            memset(big, 0, sizeof big);
            big[0] = 0x03;
            big[1] = (uint8_t)lenbyte;
            if (n >= 1)
                big[n - 1] = upd_crc8(big, (uint8_t)(n - 1)); /* valid CRC: only LEN is wrong */
            CHECK(!upd_frame_parse(big, (uint8_t)n, &f));
        }
    }

    /* ---- in-place use: payload may alias buf + 2 (echo the request) ---- */
    uint8_t rx[64];
    memcpy(rx, echo, 5);
    CHECK(upd_frame_parse(rx, 5, &f));
    CHECK(f.payload == rx + 2);
    /* build the response into the same buffer the payload points into */
    CHECK(upd_frame_build(rx, sizeof rx, 0x06u | 0x80u, f.payload, f.len) == 5);
    CHECK(rx[0] == 0x86 && rx[1] == 0x02 && rx[2] == 0xAA && rx[3] == 0xBB);
    CHECK(rx[4] == upd_crc8(rx, 4));
    CHECK(upd_frame_parse(rx, 5, &f));
    CHECK(f.cmd == (0x06u | UPD_RSP_FLAG) && f.len == 2);

    TEST_RESULT("proto");
}
