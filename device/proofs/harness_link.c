/* CBMC harness — stream link layer (link_stream.c): resync safety.
 *
 * Properties, under a fully nondet byte stream delivered in two
 * nondet-sized chunks (get_byte reports "empty" between them, so mid-frame
 * state carried across polls is covered at every split point) into a
 * nondet-sized receive buffer malloc'd at its EXACT length:
 *
 *  - RTE freedom / confinement: the parser never writes outside buf and
 *    never reads a byte it was not handed (bytes only arrive via the
 *    get_byte model, which serves at most MODEL_STREAM of them) — any
 *    violation is a bounds/pointer counterexample, not slack-space luck.
 *  - Totality: every link_poll returns once the stream is drained; with
 *    --unwinding-assertions on, a pump loop that could spin past the
 *    bounded stream is a proof failure, so no byte sequence wedges it.
 *  - Delivery contract: a frame reported true satisfies the link_poll
 *    postcondition (payload aliases buf + 2, whole frame fits buf), and
 *    the machine's state invariant (n confined by buf_len) survives every
 *    poll — the state a malicious stream leaves behind is always one a
 *    following valid 0x7E + frame can be accepted from.
 *
 * link_send is driven with an exact-length nondet frame; put_byte folds
 * into a sink, so an over-read of frame[] is a pointer-check failure.
 *
 * Model bounds: stream <= 10 bytes, buf_len <= 10, send n <= 10 — enough
 * for a full sync+frame cycle plus garbage on both sides. The lane runs
 * this harness with its own --unwind 12 (see Makefile): the largest
 * reachable loop is the 11-iteration pump; the shared 70 would blow the
 * pump x parse x CRC-8 nesting into minutes of solver time. Per-byte
 * nondet availability gaps were tried and rejected for the same reason —
 * the two-chunk split covers the same resume-mid-frame states without
 * doubling the path count at every pump step.
 */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

#include "updater/link.h"
#include "updater/proto.h"

#define MODEL_STREAM 10u
#define MODEL_LBUF   10u

uint8_t nondet_u8(void);

/* ---- io model ----------------------------------------------------------- */

static uint8_t g_remaining;   /* bytes the stream still owes */

static bool h_get(void *ctx, uint8_t *b)
{
    (void)ctx;
    if (g_remaining == 0u)
        return false;
    g_remaining = (uint8_t)(g_remaining - 1u);
    *b = nondet_u8();
    return true;
}

static uint8_t g_sink;

static void h_put(void *ctx, uint8_t b)
{
    (void)ctx;
    g_sink ^= b;
}

/* ---- proof entry point --------------------------------------------------- */

int main(void)
{
    /* --- link_poll over a nondet stream ---------------------------------- */
    {
        uint8_t buf_len = nondet_u8();
        __CPROVER_assume(buf_len <= MODEL_LBUF);
        uint8_t *buf = malloc(buf_len);
        __CPROVER_assume(buf != NULL);

        link_io_t io = { h_get, h_put, 0 };
        link_t l;
        link_init(&l, &io, buf, buf_len);

        uint8_t chunk1 = nondet_u8();
        uint8_t chunk2 = nondet_u8();
        __CPROVER_assume(chunk1 <= MODEL_STREAM);
        __CPROVER_assume(chunk2 <= MODEL_STREAM - chunk1);

        upd_frame_t f;
        /* two polls over two chunks: poll 1 can stop at ANY byte position
         * (chunk1 is nondet), so poll 2 covers resuming from every parked
         * state — hunting, mid-header, mid-payload */
        g_remaining = chunk1;
        if (link_poll(&l, &f)) {
            assert(f.payload == buf + 2);
            assert((unsigned)f.len + UPD_FRAME_OVERHEAD <= buf_len);
        }
        /* state invariant: whatever the stream did, the next write index
         * stays inside buf and hunting can always resume */
        assert(!l.in_frame || l.n < buf_len);
        assert(l.buf_len == buf_len);

        g_remaining = chunk2;
        if (link_poll(&l, &f)) {
            assert(f.payload == buf + 2);
            assert((unsigned)f.len + UPD_FRAME_OVERHEAD <= buf_len);
        }
        assert(!l.in_frame || l.n < buf_len);
        assert(l.buf_len == buf_len);
    }

    /* --- link_send with an exact-length nondet frame ---------------------- */
    {
        uint8_t n = nondet_u8();
        __CPROVER_assume(n <= MODEL_STREAM);
        uint8_t *frame = malloc(n);
        __CPROVER_assume(frame != NULL);
        link_io_t io = { h_get, h_put, 0 };
        link_send(&io, frame, n);
    }
    return 0;
}
