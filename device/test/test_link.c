#include <string.h>
#include "check.h"
#include "updater/link.h"
#include "updater/proto.h"
#include "updater/crc8.h"

/* ---- scripted stream: nonblocking get_byte over a fixed byte array ----- */

typedef struct {
    const uint8_t *data;
    unsigned       len;
    unsigned       pos;
    int            trickle;   /* report "no byte yet" between every delivery */
    int            gap;
} stream_t;

static bool stream_get(void *ctx, uint8_t *b)
{
    stream_t *s = (stream_t *)ctx;
    if (s->pos >= s->len)
        return false;
    if (s->trickle) {
        s->gap = !s->gap;
        if (s->gap)
            return false;
    }
    *b = s->data[s->pos++];
    return true;
}

static void stream_put_unused(void *ctx, uint8_t b)
{
    (void)ctx;
    (void)b;
}

/* ---- recording sink for link_send -------------------------------------- */

typedef struct {
    uint8_t  out[300];
    unsigned n;
} rec_t;

static bool rec_get_unused(void *ctx, uint8_t *b)
{
    (void)ctx;
    (void)b;
    return false;
}

static void rec_put(void *ctx, uint8_t b)
{
    rec_t *r = (rec_t *)ctx;
    if (r->n < sizeof r->out)
        r->out[r->n++] = b;
}

/* ---- fixtures ----------------------------------------------------------- */

static stream_t g_s;
static link_t   g_l;
static uint8_t  g_lbuf[136];   /* page_size + 8 sizing, mirrors the TWI port */

static const link_io_t g_io = { stream_get, stream_put_unused, &g_s };

static void feed_into(uint8_t *buf, uint8_t buf_len,
                      const uint8_t *data, unsigned len, int trickle)
{
    g_s.data    = data;
    g_s.len     = len;
    g_s.pos     = 0;
    g_s.trickle = trickle;
    g_s.gap     = 0;
    link_init(&g_l, &g_io, buf, buf_len);
}

static void feed(const uint8_t *data, unsigned len, int trickle)
{
    feed_into(g_lbuf, (uint8_t)sizeof g_lbuf, data, len, trickle);
}

int main(void)
{
    upd_frame_t f;

    /* golden frames used throughout */
    uint8_t fr_echo[8];
    uint8_t fr_info[8];
    const uint8_t pl_echo[] = { 0xAA, 0xBB };
    uint8_t n_echo = upd_frame_build(fr_echo, sizeof fr_echo,
                                     UPD_CMD_ECHO, pl_echo, 2);
    uint8_t n_info = upd_frame_build(fr_info, sizeof fr_info,
                                     UPD_CMD_INFO, (const uint8_t *)0, 0);
    CHECK(n_echo == 5 && n_info == 3);

    /* ---- empty stream: nothing to parse -------------------------------- */
    feed((const uint8_t *)0, 0, 0);
    CHECK(!link_poll(&g_l, &f));

    /* ---- pure garbage, no sync byte: frame-shaped bytes are NOT a frame - */
    {
        const uint8_t garbage[] = { 0x01, 0x00, 0x15, 0xFF, 0x00, 0x42 };
        feed(garbage, sizeof garbage, 0);
        CHECK(!link_poll(&g_l, &f));
    }

    /* ---- sync hunt through garbage, then a frame ------------------------ */
    {
        uint8_t s[16];
        unsigned n = 0;
        s[n++] = 0x11; s[n++] = 0x22; s[n++] = 0x33;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_echo, n_echo); n += n_echo;
        feed(s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
        CHECK(f.payload == g_lbuf + 2);            /* aliased, not copied */
        CHECK(f.payload[0] == 0xAA && f.payload[1] == 0xBB);
        CHECK(!link_poll(&g_l, &f));               /* stream drained */
    }

    /* ---- 0x7E inside the payload: length-driven parse must NOT resync -- */
    {
        const uint8_t pl_sync[] = { UPD_LINK_SYNC, UPD_LINK_SYNC, 0x03 };
        uint8_t fr[8];
        uint8_t nf = upd_frame_build(fr, sizeof fr, UPD_CMD_ECHO, pl_sync, 3);
        CHECK(nf == 6);
        uint8_t s[8];
        s[0] = UPD_LINK_SYNC;
        memcpy(s + 1, fr, nf);
        feed(s, 1u + nf, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.len == 3 && memcmp(f.payload, pl_sync, 3) == 0);
        CHECK(!link_poll(&g_l, &f));               /* no phantom second frame */
    }

    /* ---- torn frame, then a good frame ---------------------------------- */
    /* Torn frame declares LEN=5 (total 8) but the sender died after one
     * payload byte; later traffic completes the byte count, CRC fails, and
     * the hunter finds the next sync. Filler is forced to break the CRC so
     * the test cannot pass by a fluke checksum match. */
    {
        uint8_t s[24];
        unsigned n = 0;
        s[n++] = UPD_LINK_SYNC;
        s[n++] = UPD_CMD_ECHO;   /* CMD  */
        s[n++] = 0x05;           /* LEN=5 -> total 8 */
        s[n++] = 0xAA;           /* only payload byte that arrived */
        s[n++] = 0x00; s[n++] = 0x00; s[n++] = 0x00; s[n++] = 0x00; /* filler */
        s[n] = (uint8_t)(upd_crc8(s + 1, 7) ^ 0x5Au);  /* guaranteed bad CRC */
        n++;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_echo, n_echo); n += n_echo;
        feed(s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
        CHECK(f.payload[0] == 0xAA && f.payload[1] == 0xBB);
        CHECK(!link_poll(&g_l, &f));
    }

    /* ---- CRC-corrupt frame, then re-hunt finds the following frame ------ */
    {
        uint8_t bad[8];
        memcpy(bad, fr_echo, n_echo);
        bad[n_echo - 1u] ^= 0xFFu;
        uint8_t s[16];
        unsigned n = 0;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, bad, n_echo); n += n_echo;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_info, n_info); n += n_info;
        feed(s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_INFO && f.len == 0);
        CHECK(!link_poll(&g_l, &f));
    }

    /* ---- overflow: LEN says more than the link buffer holds ------------- */
    /* 6-byte link buffer; declared total 13 can never fit -> drop at the
     * LEN byte, re-hunt, recover the small frame that follows. */
    {
        uint8_t tiny[6];
        uint8_t s[24];
        unsigned n = 0;
        s[n++] = UPD_LINK_SYNC;
        s[n++] = UPD_CMD_WRITE_PAGE;
        s[n++] = 0x0A;                       /* LEN=10 -> total 13 > 6 */
        for (unsigned i = 0; i < 11; i++)    /* declared remainder, no 0x7E */
            s[n++] = 0x00;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_info, n_info); n += n_info;
        feed_into(tiny, (uint8_t)sizeof tiny, s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_INFO && f.len == 0);
        CHECK(f.payload == tiny + 2);
        CHECK(!link_poll(&g_l, &f));
    }

    /* ---- LEN=255 garbage: total 258 overflows every possible buffer ----- */
    {
        uint8_t s[16];
        unsigned n = 0;
        s[n++] = UPD_LINK_SYNC;
        s[n++] = 0x00;
        s[n++] = 0xFF;                       /* LEN=255 -> immediate drop */
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_echo, n_echo); n += n_echo;
        feed(s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
    }

    /* ---- malicious wedge attempt: huge declared LEN eats bytes ----------
     * (including 0x7E bytes inside the doomed frame) but the parser must
     * recover after exactly total bytes -- no permanent wedge. */
    {
        uint8_t s[80];
        unsigned n = 0;
        s[n++] = UPD_LINK_SYNC;
        s[n++] = 0x01;
        s[n++] = 40;                          /* total 43, fits the 136 buf */
        for (unsigned i = 0; i < 40; i++)     /* payload full of fake syncs */
            s[n++] = UPD_LINK_SYNC;
        s[n] = (uint8_t)(upd_crc8(s + 1, 42) ^ 0x01u);  /* bad CRC */
        n++;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_info, n_info); n += n_info;
        feed(s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_INFO && f.len == 0);
        CHECK(!link_poll(&g_l, &f));
    }

    /* ---- byte-at-a-time delivery: get_byte gaps between every byte ------ */
    {
        uint8_t s[8];
        s[0] = UPD_LINK_SYNC;
        memcpy(s + 1, fr_echo, n_echo);
        feed(s, 1u + n_echo, 1);
        unsigned got = 0;
        for (unsigned i = 0; i < 32; i++)
            if (link_poll(&g_l, &f))
                got++;
        CHECK(got == 1);
        CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
        CHECK(f.payload[0] == 0xAA && f.payload[1] == 0xBB);
    }

    /* ---- back-to-back frames in one pump: one frame per poll ------------ */
    {
        uint8_t s[16];
        unsigned n = 0;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_echo, n_echo); n += n_echo;
        s[n++] = UPD_LINK_SYNC;
        memcpy(s + n, fr_info, n_info); n += n_info;
        feed(s, n, 0);
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
        CHECK(f.payload[0] == 0xAA && f.payload[1] == 0xBB);   /* still intact */
        CHECK(link_poll(&g_l, &f));
        CHECK(f.cmd == UPD_CMD_INFO && f.len == 0);
        CHECK(!link_poll(&g_l, &f));
    }

    /* ---- degenerate link buffer (< frame overhead): never parses,
     *      never writes anywhere ------------------------------------------ */
    {
        uint8_t two[2] = { 0x5A, 0x5A };
        uint8_t s[8];
        s[0] = UPD_LINK_SYNC;
        memcpy(s + 1, fr_info, n_info);
        feed_into(two, 2, s, 1u + n_info, 0);
        CHECK(!link_poll(&g_l, &f));
        CHECK(two[0] == 0x5A && two[1] == 0x5A);   /* untouched */
    }

    /* ---- link_send: 0x7E then the frame bytes verbatim ------------------ */
    {
        rec_t r = { {0}, 0 };
        const link_io_t rio = { rec_get_unused, rec_put, &r };
        link_send(&rio, fr_echo, n_echo);
        CHECK(r.n == 1u + n_echo);
        CHECK(r.out[0] == UPD_LINK_SYNC);
        CHECK(memcmp(r.out + 1, fr_echo, n_echo) == 0);

        r.n = 0;
        link_send(&rio, fr_echo, 0);               /* n=0: sync byte only */
        CHECK(r.n == 1 && r.out[0] == UPD_LINK_SYNC);
    }

    TEST_RESULT("link");
}
