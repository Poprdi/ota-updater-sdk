#include "updater/crc8.h"

/*@ requires n == 0 || \valid_read(p + (0 .. n-1));
    assigns \nothing; */
uint8_t upd_crc8(const uint8_t *p, uint8_t n)
{
    uint8_t crc = 0;
    /*@ loop invariant 0 <= i <= n;
        loop assigns i, crc;
        loop variant n - i; */
    for (uint8_t i = 0; i < n; i++) {
        crc ^= p[i];
        /*@ loop invariant 0 <= b <= 8;
            loop assigns b, crc;
            loop variant 8 - b; */
        for (uint8_t b = 0; b < 8; b++) {
            /* Shift in a wide unsigned temporary and mask before the cast:
             * the mod-256 truncation is explicit arithmetic, so the cast
             * itself is always value-preserving (CBMC --conversion-check
             * proves it; semantics identical to the classic u8 shift). */
            unsigned v = (unsigned)crc << 1;
            if (crc & 0x80u)
                v ^= 0x07u;
            crc = (uint8_t)(v & 0xFFu);
        }
    }
    return crc;
}
