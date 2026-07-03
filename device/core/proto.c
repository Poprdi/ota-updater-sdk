#include "updater/proto.h"
#include "updater/crc8.h"

/* Wire frame: [0]=CMD  [1]=LEN  [2 .. 2+LEN-1]=payload  [2+LEN]=CRC8 over
 * bytes 0 .. 1+LEN (CRC-8/ATM, see crc8.h).
 *
 * u8 edge cases, by construction (all length arithmetic is done in
 * unsigned int, which cannot wrap for these operands, so no truncating
 * cast is ever taken — CBMC's conversion/overflow checks prove it; the
 * accept/reject sets are identical to the earlier u8-wrap formulation):
 *  - parse: LEN >= 253 makes LEN + 3 > 255 >= buflen, so such frames are
 *    rejected. The largest valid frame is LEN = 252, buflen = 255.
 *  - build: len >= 253 makes total > 255, refused before any truncation
 *    to the u8 wire size.
 */

/*@ requires buflen == 0 || \valid_read(buf + (0 .. buflen-1));
    requires \valid(out);
    requires \separated(out, buf + (0 .. buflen-1));
    assigns *out;
    ensures \result ==> out->cmd == buf[0] && out->len == buf[1]
                        && out->payload == buf + 2
                        && buflen == out->len + UPD_FRAME_OVERHEAD;
    ensures !\result ==> \old(*out) == *out; */
bool upd_frame_parse(const uint8_t *buf, uint8_t buflen, upd_frame_t *out)
{
    if (buflen < UPD_FRAME_OVERHEAD)
        return false;
    uint8_t len = buf[1];
    if ((unsigned)len + UPD_FRAME_OVERHEAD != buflen)   /* no wrap: <= 258 */
        return false;
    if (upd_crc8(buf, (uint8_t)(buflen - 1u)) != buf[buflen - 1u])
        return false;
    out->cmd = buf[0];
    out->len = len;
    out->payload = buf + 2;
    return true;
}

/*@ requires cap == 0 || \valid(buf + (0 .. cap-1));
    requires len == 0 || \valid_read(payload + (0 .. len-1));
    requires len == 0 || \separated(payload + (0 .. len-1), buf + (0 .. 1));
    assigns buf[0 .. cap-1];
    behavior refuse:
      assumes len + UPD_FRAME_OVERHEAD > 255 || cap < len + UPD_FRAME_OVERHEAD;
      assigns \nothing;
      ensures \result == 0;
    behavior ok:
      assumes len + UPD_FRAME_OVERHEAD <= 255 && cap >= len + UPD_FRAME_OVERHEAD;
      assigns buf[0 .. len + UPD_FRAME_OVERHEAD - 1];
      ensures \result == len + UPD_FRAME_OVERHEAD;
      ensures buf[0] == cmd && buf[1] == len;
    complete behaviors;
    disjoint behaviors; */
uint8_t upd_frame_build(uint8_t *buf, uint8_t cap, uint8_t cmd,
                        const uint8_t *payload, uint8_t len)
{
    /* Aliasing contract (see \separated above): payload must not overlap
     * buf[0..1], which are written before payload is read. payload may
     * otherwise point into buf; the copy below is ascending with
     * destination buf+2 <= source, so building a response in place over a
     * received frame (payload == buf + 2, as produced by upd_frame_parse)
     * is supported and covered by test_proto.c. */
    unsigned total = (unsigned)len + UPD_FRAME_OVERHEAD;   /* <= 258, no wrap */
    if (total > 255u || cap < total)   /* won't fit the u8 wire size / buf */
        return 0;
    buf[0] = cmd;
    buf[1] = len;
    /*@ loop invariant 0 <= i <= len;
        loop assigns i, buf[2 .. 2+len-1];
        loop variant len - i; */
    for (uint8_t i = 0; i < len; i++)
        buf[2u + i] = payload[i];
    buf[total - 1u] = upd_crc8(buf, (uint8_t)(total - 1u));
    return (uint8_t)total;   /* total <= 255 here: value-preserving */
}
