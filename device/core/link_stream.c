#include "updater/link.h"

/* Two-state receive machine, deliberately minimal:
 *
 *   hunting  (in_frame == false): every byte is discarded until 0x7E; the
 *            sync byte itself is never stored.
 *   in-frame (in_frame == true) : bytes are appended to buf; once LEN
 *            (buf[1]) is in, the frame end is fixed at LEN + 3 bytes and
 *            everything up to it — 0x7E included — is a frame byte.
 *
 * State invariant between polls (why the buf writes below are in bounds):
 *   in_frame ==> buf_len >= 3                        (checked at sync)
 *   in_frame && n >= 2 ==> buf[1] + 3 <= buf_len     (checked at LEN)
 *                          && n < buf[1] + 3         (completion exits frame)
 * so the next write index n is always < buf_len. No wedge is possible: an
 * in-frame state resolves after at most buf[1] + 3 - n more bytes, then the
 * machine is hunting again and the next 0x7E + valid frame is accepted.
 *
 * u8 edge cases (proto.c discipline: wide unsigned arithmetic, compare
 * before any narrowing): total = LEN + 3 <= 258 is computed in unsigned
 * int; LEN >= 253 makes total > 255 >= buf_len, so such frames are dropped
 * at the LEN byte and the n counter never approaches a u8 wrap (n <= 254
 * before any increment). */

/*@ requires \valid(l) && \valid_read(io);
    requires buf_len == 0 || \valid(buf + (0 .. buf_len-1));
    assigns *l;
    ensures l->io == io && l->buf == buf && l->buf_len == buf_len;
    ensures l->n == 0 && l->in_frame == \false; */
void link_init(link_t *l, const link_io_t *io, uint8_t *buf, uint8_t buf_len)
{
    l->io       = io;
    l->buf      = buf;
    l->buf_len  = buf_len;
    l->n        = 0u;
    l->in_frame = false;
}

/*@ requires \valid(l) && \valid(out);
    requires \valid_read(l->io) && l->io->get_byte != \null;
    requires l->buf_len == 0 || \valid(l->buf + (0 .. l->buf_len-1));
    requires \separated(out, l, l->buf + (0 .. l->buf_len-1));
    assigns l->n, l->in_frame, l->buf[0 .. l->buf_len-1], *out;
    ensures \result ==> out->payload == l->buf + 2
                        && (unsigned)out->len + UPD_FRAME_OVERHEAD
                           <= l->buf_len; */
bool link_poll(link_t *l, upd_frame_t *out)
{
    uint8_t b;
    /* No loop variant: termination rests on the io contract (get_byte must
     * eventually report an empty FIFO, link.h) and is not expressible
     * without a ghost model of the stream. The proof lane discharges it
     * bounded — harness_link.c's stream is finite with unwinding
     * assertions on, so an unbounded pump is a proof failure, not a hang.
     * Callback side effects live in the port's state, outside any
     * footprint this unit can name. */
    /*@ loop invariant l->in_frame ==> l->buf_len >= UPD_FRAME_OVERHEAD;
        loop invariant l->in_frame && l->n >= 2 ==>
            (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD <= l->buf_len
            && l->n < (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD;
        loop invariant l->in_frame ==> l->n < l->buf_len;
        loop assigns b, l->n, l->in_frame, l->buf[0 .. l->buf_len-1], *out; */
    while (l->io->get_byte(l->io->ctx, &b)) {
        if (!l->in_frame) {
            /* A buffer below the 3-byte frame minimum can never complete a
             * frame; refusing sync here keeps the write below in bounds
             * even for degenerate buf_len 0..2. */
            if (b == UPD_LINK_SYNC && l->buf_len >= UPD_FRAME_OVERHEAD) {
                l->in_frame = true;
                l->n        = 0u;
            }
            continue;
        }
        l->buf[l->n] = b;
        l->n = (uint8_t)(l->n + 1u);   /* n <= 254 before ++: no wrap */
        if (l->n < 2u)
            continue;                  /* LEN not known yet */
        unsigned total = (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD; /* <= 258 */
        if (total > l->buf_len) {      /* declared frame can never fit */
            l->in_frame = false;
            continue;
        }
        if (l->n < total)
            continue;
        l->in_frame = false;           /* frame complete, valid or not */
        if (upd_frame_parse(l->buf, l->n, out))
            return true;
        /* CRC failure: drop silently and resume hunting — the protocol's
         * timeout+retry owns loss recovery; a stream ACK would duplicate
         * the CRC layer. */
    }
    return false;
}

/* assigns \nothing is stated from this unit's frame: put_byte's effects
 * are the port's own state, not memory this contract can name. */
/*@ requires \valid_read(io) && io->put_byte != \null;
    requires n == 0 || \valid_read(frame + (0 .. n-1));
    assigns \nothing; */
void link_send(const link_io_t *io, const uint8_t *frame, uint8_t n)
{
    io->put_byte(io->ctx, (uint8_t)UPD_LINK_SYNC);
    /*@ loop invariant 0 <= i <= n;
        loop assigns i;
        loop variant n - i; */
    for (uint8_t i = 0u; i < n; i++)
        io->put_byte(io->ctx, frame[i]);
}
