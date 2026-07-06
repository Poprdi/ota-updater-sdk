#include "updater/link.h"

/* WP proof model for the io callbacks (__FRAMAC__ only — the shipped
 * build calls through the function pointers unchanged, and the CBMC lane
 * verifies that real indirect-call path):
 *
 * WP cannot reason about a call through a function pointer whose target
 * set is open — every port supplies its own get_byte/put_byte, so a
 * `@calls` enumeration is impossible by design. Under Frama-C the two
 * invocation sites route through these extern prototypes instead, whose
 * contracts state EXACTLY the callback contract from link.h: get_byte may
 * write *b (plus its own port-side state, which no object in this unit
 * names — same framing argument as the port.h contracts); put_byte only
 * touches port state. No strengthening is smuggled in: the callbacks'
 * results stay fully nondeterministic, including a get_byte that returns
 * true forever — which is why link_poll carries `terminates \false`
 * (termination is the io contract's obligation, link.h). */
#ifdef __FRAMAC__
/*@ requires \valid(b);
    assigns *b; */
extern bool updlink_wp_get_byte(void *ctx, uint8_t *b);
/*@ assigns \nothing; */
extern void updlink_wp_put_byte(void *ctx, uint8_t b);
#define LINK_GET_BYTE(io, b) updlink_wp_get_byte((io)->ctx, (b))
#define LINK_PUT_BYTE(io, v) updlink_wp_put_byte((io)->ctx, (v))
#else
#define LINK_GET_BYTE(io, b) ((io)->get_byte((io)->ctx, (b)))
#define LINK_PUT_BYTE(io, v) ((io)->put_byte((io)->ctx, (v)))
#endif

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

/* The requires/ensures pair below is the file-header state invariant made
 * machine-checked: it holds after link_init (n == 0, hunting) and every
 * link_poll re-establishes it, so for any caller that got its link_t from
 * link_init and only ever hands it back to link_poll it holds inductively
 * at each call. It is a REAL precondition — a corrupted link_t with
 * in_frame set and n >= buf_len would index past the buffer. */
/*@ requires \valid(l) && \valid(out);
    requires \valid_read(l->io) && l->io->get_byte != \null;
    requires l->buf_len == 0 || \valid(l->buf + (0 .. l->buf_len-1));
    requires \separated(out, l, l->buf + (0 .. l->buf_len-1));
    requires l->in_frame ==> l->buf_len >= UPD_FRAME_OVERHEAD;
    requires l->in_frame && l->n >= 2 ==>
        (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD <= l->buf_len
        && l->n < (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD;
    requires l->in_frame ==> l->n < l->buf_len;
    terminates \false;
    assigns l->n, l->in_frame, l->buf[0 .. l->buf_len-1], *out;
    ensures l->in_frame ==> l->buf_len >= UPD_FRAME_OVERHEAD;
    ensures l->in_frame && l->n >= 2 ==>
        (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD <= l->buf_len
        && l->n < (unsigned)l->buf[1] + UPD_FRAME_OVERHEAD;
    ensures l->in_frame ==> l->n < l->buf_len;
    ensures \result ==> out->payload == l->buf + 2
                        && (unsigned)out->len + UPD_FRAME_OVERHEAD
                           <= l->buf_len; */
bool link_poll(link_t *l, upd_frame_t *out)
{
    uint8_t b;
    /* Snapshots of the fields the loop never writes: the loop annotations
     * then range over logic constants instead of heap loads re-read under
     * every memory update — same object code, but WP/Alt-Ergo discharge
     * the loop goals instead of timing out. */
    const link_io_t *const io      = l->io;
    uint8_t         *const buf     = l->buf;
    const uint8_t          buf_len = l->buf_len;
    /* No loop variant: termination rests on the io contract (get_byte must
     * eventually report an empty FIFO, link.h) and is not expressible
     * without a ghost model of the stream — hence `terminates \false`
     * above (honest: no termination claim is made deductively). The proof
     * lane discharges it bounded — harness_link.c's stream is finite with
     * unwinding assertions on, so an unbounded pump is a proof failure,
     * not a hang. Callback side effects live in the port's state, outside
     * any footprint this unit can name. */
    /*@ loop invariant l->in_frame ==> buf_len >= UPD_FRAME_OVERHEAD;
        loop invariant l->in_frame && l->n >= 2 ==>
            (unsigned)buf[1] + UPD_FRAME_OVERHEAD <= buf_len
            && l->n < (unsigned)buf[1] + UPD_FRAME_OVERHEAD;
        loop invariant l->in_frame ==> l->n < buf_len;
        loop assigns b, l->n, l->in_frame, buf[0 .. buf_len-1], *out; */
    while (LINK_GET_BYTE(io, &b)) {
        if (!l->in_frame) {
            /* A buffer below the 3-byte frame minimum can never complete a
             * frame; refusing sync here keeps the write below in bounds
             * even for degenerate buf_len 0..2. */
            if (b == UPD_LINK_SYNC && buf_len >= UPD_FRAME_OVERHEAD) {
                l->in_frame = true;
                l->n        = 0u;
            }
            continue;
        }
        buf[l->n] = b;
        l->n = (uint8_t)(l->n + 1u);   /* n <= 254 before ++: no wrap */
        if (l->n < 2u)
            continue;                  /* LEN not known yet */
        unsigned total = (unsigned)buf[1] + UPD_FRAME_OVERHEAD; /* <= 258 */
        if (total > buf_len) {         /* declared frame can never fit */
            l->in_frame = false;
            continue;
        }
        if (l->n < total)
            continue;
        l->in_frame = false;           /* frame complete, valid or not */
        if (upd_frame_parse(buf, l->n, out))
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
    LINK_PUT_BYTE(io, (uint8_t)UPD_LINK_SYNC);
    /*@ loop invariant 0 <= i <= n;
        loop assigns i;
        loop variant n - i; */
    for (uint8_t i = 0u; i < n; i++)
        LINK_PUT_BYTE(io, frame[i]);
}
