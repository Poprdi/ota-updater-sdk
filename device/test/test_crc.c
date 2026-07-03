#include "check.h"
#include "updater/crc8.h"
#include "updater/crc32.h"

int main(void)
{
    /* normative golden vectors from the spec */
    const uint8_t info[] = { 0x01, 0x00 };
    CHECK(upd_crc8(info, 2) == 0x15);
    const uint8_t echo[] = { 0x06, 0x02, 0xAA, 0xBB };
    CHECK(upd_crc8(echo, 4) == 0x10);
    CHECK(upd_crc8(info, 0) == 0x00);

    uint32_t c = upd_crc32_init();
    const char *s = "123456789";
    for (const char *p = s; *p; p++)
        c = upd_crc32_update(c, (uint8_t)*p);
    CHECK(upd_crc32_final(c) == 0xCBF43926UL);
    CHECK(upd_crc32_final(upd_crc32_init()) == 0x00000000UL);

    TEST_RESULT("crc");
}
