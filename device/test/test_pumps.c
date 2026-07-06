#include <string.h>
#include "check.h"
#include "updater/link.h"
#include "updater/proto.h"
#include "uart_pump.h"
#include "spi_pump.h"
#include "softuart_pump.h"

/* Transport-pump smoke tests: every pump is driven end-to-end through
 * link_stream with a scripted fake ops struct — RX assembles a real frame,
 * TX output is captured and decoded byte- (or bit-) accurately. The pumps
 * are portable C with zero MCU knowledge; these tests are the executable
 * form of their ops contracts. */

/* ---- shared golden frames ---------------------------------------------- */

static uint8_t  fr_echo[8], fr_info[8];
static uint8_t  n_echo, n_info;

/* ======================================================================== */
/* uart_pump: {rx_ready, rx_read, tx_ready, tx_write} register-ops fake     */
/* ======================================================================== */

typedef struct {
    const uint8_t *rx;
    unsigned       rx_len, rx_pos;
    int            gap;            /* rx_ready false every other call while
                                      data remains: models FIFO latency */
    int            ready_flag;     /* set by rx_ready==true, cleared by read */
    int            read_violation; /* rx_read called without a prior ready   */
    uint8_t        tx[64];
    unsigned       ntx;
    unsigned       tx_stall;       /* tx_ready false this many polls per byte */
    unsigned       stall_ctr;
} uf_t;

static bool uf_rx_ready(void *ctx)
{
    uf_t *u = (uf_t *)ctx;
    if (u->rx_pos >= u->rx_len)
        return false;
    u->gap = !u->gap;
    if (u->gap)
        return false;
    u->ready_flag = 1;
    return true;
}

static uint8_t uf_rx_read(void *ctx)
{
    uf_t *u = (uf_t *)ctx;
    if (!u->ready_flag)
        u->read_violation = 1;     /* contract: read only after ready==true */
    u->ready_flag = 0;
    return u->rx[u->rx_pos++];
}

static bool uf_tx_ready(void *ctx)
{
    uf_t *u = (uf_t *)ctx;
    if (u->stall_ctr < u->tx_stall) {
        u->stall_ctr++;
        return false;
    }
    return true;
}

static void uf_tx_write(void *ctx, uint8_t b)
{
    uf_t *u = (uf_t *)ctx;
    u->stall_ctr = 0;              /* next byte stalls again */
    if (u->ntx < sizeof u->tx)
        u->tx[u->ntx++] = b;
}

static const uart_pump_ops_t uf_ops = {
    uf_rx_ready, uf_rx_read, uf_tx_ready, uf_tx_write
};

static void test_uart_pump(void)
{
    uf_t        u;
    uart_pump_t p;
    uint8_t     lbuf[136];
    link_t      l;
    upd_frame_t f;

    memset(&u, 0, sizeof u);
    uart_pump_init(&p, &uf_ops, &u);

    /* RX: garbage, sync, echo frame — delivered with ready-gaps */
    uint8_t s[16];
    unsigned n = 0;
    s[n++] = 0x42;
    s[n++] = UPD_LINK_SYNC;
    memcpy(s + n, fr_echo, n_echo); n += n_echo;
    u.rx     = s;
    u.rx_len = n;

    link_init(&l, &p.io, lbuf, (uint8_t)sizeof lbuf);
    unsigned got = 0;
    for (unsigned i = 0; i < 64; i++)
        if (link_poll(&l, &f))
            got++;
    CHECK(got == 1);
    CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
    CHECK(f.payload[0] == 0xAA && f.payload[1] == 0xBB);
    CHECK(u.read_violation == 0);

    /* TX: link_send through the pump; tx_ready stalls 3x per byte and the
     * pump must wait each time, not drop */
    u.tx_stall = 3;
    link_send(&p.io, fr_echo, n_echo);
    CHECK(u.ntx == 1u + n_echo);
    CHECK(u.tx[0] == UPD_LINK_SYNC);
    CHECK(memcmp(u.tx + 1, fr_echo, n_echo) == 0);
}

/* ======================================================================== */
/* spi_pump: {xfer_done, data_read, data_write} shift-register fake         */
/* ======================================================================== */

typedef struct {
    uint8_t  shift;        /* byte pre-loaded for the next exchange */
    int      done;
    uint8_t  mosi;
    uint8_t  miso_log[64]; /* what the host actually saw, per exchange */
    unsigned nlog;
} sf_t;

/* Host clocks one full byte exchange: the slave shifts out whatever was
 * pre-loaded BEFORE this exchange started — the one-byte lag under test. */
static void sf_clock(sf_t *f, uint8_t mosi)
{
    if (f->nlog < sizeof f->miso_log)
        f->miso_log[f->nlog] = f->shift;
    f->nlog++;
    f->mosi = mosi;
    f->done = 1;
}

static bool sf_xfer_done(void *ctx)
{
    return ((sf_t *)ctx)->done != 0;
}

static uint8_t sf_data_read(void *ctx)
{
    sf_t *f = (sf_t *)ctx;
    f->done = 0;
    return f->mosi;
}

static void sf_data_write(void *ctx, uint8_t b)
{
    ((sf_t *)ctx)->shift = b;
}

static const spi_pump_ops_t sf_ops = { sf_xfer_done, sf_data_read, sf_data_write };

static void test_spi_pump(void)
{
    sf_t        f;
    spi_pump_t  p;
    uint8_t     txq[24];
    uint8_t     lbuf[136];
    link_t      l;
    upd_frame_t fr;

    memset(&f, 0, sizeof f);
    f.shift = 0xEE;                      /* undefined power-up register */
    spi_pump_init(&p, &sf_ops, &f, txq, (uint8_t)sizeof txq);
    CHECK(f.shift == 0x00);              /* init arms the idle byte */
    CHECK(spi_pump_tx_idle(&p));

    link_init(&l, &p.io, lbuf, (uint8_t)sizeof lbuf);

    /* host clocks the request in; every exchange must show 0x00 on MISO */
    unsigned got = 0;
    sf_clock(&f, UPD_LINK_SYNC);
    if (link_poll(&l, &fr))
        got++;
    for (unsigned i = 0; i < n_echo; i++) {
        sf_clock(&f, fr_echo[i]);
        if (link_poll(&l, &fr))
            got++;
    }
    CHECK(got == 1);
    CHECK(fr.cmd == UPD_CMD_ECHO && fr.len == 2);
    CHECK(f.nlog == 1u + n_echo);
    for (unsigned i = 0; i < f.nlog; i++)
        CHECK(f.miso_log[i] == 0x00);    /* busy convention while no response */

    /* arm the response */
    spi_pump_tx_clear(&p);
    link_send(&p.io, fr_info, n_info);   /* queues 0x7E + frame, no wire I/O */
    CHECK(!spi_pump_tx_idle(&p));
    CHECK(f.nlog == 1u + n_echo);        /* still nothing clocked */

    /* host polls with idle bytes: first exchange still shows the pre-armed
     * 0x00 (one-byte lag), then 0x7E + frame, then 0x00 filler again */
    unsigned base = f.nlog;
    unsigned wire = 1u + (unsigned)n_info;      /* sync + frame */
    for (unsigned i = 0; i < wire + 2u; i++) {
        sf_clock(&f, 0x00);
        CHECK(!link_poll(&l, &fr));      /* host idle bytes are not frames */
        if (i == wire - 1u)
            CHECK(!spi_pump_tx_idle(&p)); /* last byte only in shift reg */
        if (i == wire)
            CHECK(spi_pump_tx_idle(&p)); /* now truly on the wire */
    }
    CHECK(f.miso_log[base] == 0x00);     /* the lag byte */
    CHECK(f.miso_log[base + 1u] == UPD_LINK_SYNC);
    CHECK(memcmp(f.miso_log + base + 2u, fr_info, n_info) == 0);
    CHECK(f.miso_log[base + 1u + wire] == 0x00);   /* back to busy/idle */
    CHECK(f.miso_log[base + 2u + wire] == 0x00);
}

/* ======================================================================== */
/* softuart_pump: virtual-time waveform fake, bit-accurate both directions  */
/* ======================================================================== */

#define SEG_MAX 700u

typedef struct {
    struct { uint32_t t_end; uint8_t level; } segs[SEG_MAX];
    unsigned nseg;
    uint32_t t_total;
    uint32_t now;
    uint32_t sample_cost;  /* us per pin_rx call: models real poll latency */
    struct { uint32_t t; uint8_t level; } tx[SEG_MAX];
    unsigned ntx;
} sw_t;

static void sw_reset(sw_t *w, uint32_t sample_cost)
{
    memset(w, 0, sizeof *w);
    w->sample_cost = sample_cost;
}

static void sw_level(sw_t *w, uint8_t level, uint32_t dur)
{
    w->t_total += dur;
    if (w->nseg > 0u && w->segs[w->nseg - 1u].level == level) {
        w->segs[w->nseg - 1u].t_end = w->t_total;
        return;
    }
    if (w->nseg < SEG_MAX) {
        w->segs[w->nseg].t_end = w->t_total;
        w->segs[w->nseg].level = level;
        w->nseg++;
    }
}

/* One 8N1 character at bit_us per bit: start(0), LSB-first data, stop.
 * stop_level 1 is a legal stop bit; 0 forges a framing error. */
static void sw_byte(sw_t *w, uint8_t b, uint32_t bit_us, uint8_t stop_level)
{
    sw_level(w, 0u, bit_us);
    for (unsigned i = 0; i < 8u; i++)
        sw_level(w, (uint8_t)((b >> i) & 1u), bit_us);
    sw_level(w, stop_level, bit_us);
}

static uint8_t sw_rx_level(const sw_t *w, uint32_t t)
{
    for (unsigned i = 0; i < w->nseg; i++)
        if (t < w->segs[i].t_end)
            return w->segs[i].level;
    return 1u;                           /* line idles at mark */
}

static bool sw_pin_rx(void *ctx)
{
    sw_t   *w = (sw_t *)ctx;
    uint8_t l = sw_rx_level(w, w->now);
    w->now += w->sample_cost;
    return l != 0u;
}

static void sw_pin_tx(void *ctx, bool level)
{
    sw_t *w = (sw_t *)ctx;
    if (w->ntx < SEG_MAX) {
        w->tx[w->ntx].t     = w->now;
        w->tx[w->ntx].level = level ? 1u : 0u;
        w->ntx++;
    }
}

static void sw_delay_us(void *ctx, uint16_t us)
{
    ((sw_t *)ctx)->now += us;
}

static const softuart_pump_ops_t sw_ops = { sw_pin_tx, sw_pin_rx, sw_delay_us };

static uint8_t sw_tx_level_at(const sw_t *w, uint32_t t)
{
    uint8_t l = 1u;                      /* init drives idle mark */
    for (unsigned i = 0; i < w->ntx; i++) {
        if (w->tx[i].t <= t)
            l = w->tx[i].level;
        else
            break;
    }
    return l;
}

/* Decode the captured TX waveform exactly as a hardware UART would:
 * find the start edge, sample bit k at start + 1.5 + k bit-times (LSB
 * first), verify the stop bit at start + 9.5 bit-times. */
static unsigned sw_tx_decode(const sw_t *w, uint8_t *out, unsigned max)
{
    uint32_t cur = 0;
    unsigned n   = 0;
    while (n < max) {
        uint32_t t0    = 0;
        int      found = 0;
        for (unsigned i = 0; i < w->ntx; i++) {
            if (w->tx[i].t >= cur && w->tx[i].level == 0u) {
                t0    = w->tx[i].t;
                found = 1;
                break;
            }
        }
        if (!found)
            break;
        CHECK(sw_tx_level_at(w, t0 + SOFTUART_BIT_US / 2u) == 0u); /* start */
        uint8_t b = 0;
        for (unsigned k = 0; k < 8u; k++) {
            uint32_t ts = t0 + SOFTUART_BIT_US + SOFTUART_BIT_US / 2u
                        + (uint32_t)k * SOFTUART_BIT_US;
            if (sw_tx_level_at(w, ts) != 0u)
                b |= (uint8_t)(1u << k);
        }
        CHECK(sw_tx_level_at(w, t0 + 9u * SOFTUART_BIT_US
                                + SOFTUART_BIT_US / 2u) == 1u);    /* stop */
        out[n++] = b;
        cur = t0 + 10u * SOFTUART_BIT_US;
    }
    return n;
}

/* Feed the prepared waveform through the pump until one frame (or give up). */
static unsigned sw_pump_frames(sw_t *w, softuart_pump_t *p,
                               upd_frame_t *f, uint8_t *lbuf, uint8_t cap)
{
    link_t l;
    link_init(&l, &p->io, lbuf, cap);
    unsigned got = 0;
    for (unsigned i = 0; i < 100000u && w->now < w->t_total + 500u; i++)
        if (link_poll(&l, f))
            got++;
    return got;
}

static void test_softuart_rx_at(uint32_t bit_us, uint32_t sample_cost)
{
    sw_t            w;
    softuart_pump_t p;
    uint8_t         lbuf[136];
    upd_frame_t     f;

    sw_reset(&w, sample_cost);
    sw_level(&w, 1u, 200u);                        /* leading idle */
    sw_byte(&w, UPD_LINK_SYNC, bit_us, 1u);
    for (unsigned i = 0; i < n_echo; i++)
        sw_byte(&w, fr_echo[i], bit_us, 1u);

    softuart_pump_init(&p, &sw_ops, &w);
    CHECK(sw_pump_frames(&w, &p, &f, lbuf, (uint8_t)sizeof lbuf) == 1u);
    CHECK(f.cmd == UPD_CMD_ECHO && f.len == 2);
    CHECK(f.payload[0] == 0xAA && f.payload[1] == 0xBB);
}

static void test_softuart(void)
{
    /* nominal sender (104 us/bit) and ±2% skewed senders: the pump's
     * documented tolerance budget must actually hold. sample_cost adds to
     * EVERY bit period (it is per-sample call overhead, part of the
     * budget's "call overhead" term), so it must stay in the ~1-2 us
     * range a real pin read costs — 5 us/sample alone would be a 5% rate
     * error and no budget survives that. */
    test_softuart_rx_at(104u, 2u);
    test_softuart_rx_at(106u, 2u);   /* sender ~1.9% slow */
    test_softuart_rx_at(102u, 1u);   /* sender ~1.9% fast */

    /* framing error (broken stop bit) must not fabricate a frame; the next
     * clean frame must still be received */
    {
        sw_t            w;
        softuart_pump_t p;
        uint8_t         lbuf[136];
        upd_frame_t     f;

        sw_reset(&w, 2u);
        sw_level(&w, 1u, 200u);
        sw_byte(&w, UPD_LINK_SYNC, 104u, 0u);      /* bad stop bit */
        sw_level(&w, 1u, 300u);                    /* recover to idle */
        sw_byte(&w, UPD_LINK_SYNC, 104u, 1u);
        for (unsigned i = 0; i < n_info; i++)
            sw_byte(&w, fr_info[i], 104u, 1u);

        softuart_pump_init(&p, &sw_ops, &w);
        CHECK(sw_pump_frames(&w, &p, &f, lbuf, (uint8_t)sizeof lbuf) == 1u);
        CHECK(f.cmd == UPD_CMD_INFO && f.len == 0);
    }

    /* TX: link_send through the pump, then decode the captured waveform
     * bit-by-bit — start bit, LSB-first data, stop bit all verified */
    {
        sw_t            w;
        softuart_pump_t p;
        uint8_t         dec[16];

        sw_reset(&w, 2u);
        softuart_pump_init(&p, &sw_ops, &w);
        link_send(&p.io, fr_echo, n_echo);

        unsigned nd = sw_tx_decode(&w, dec, sizeof dec);
        CHECK(nd == 1u + n_echo);
        CHECK(dec[0] == UPD_LINK_SYNC);
        CHECK(memcmp(dec + 1, fr_echo, n_echo) == 0);
        CHECK(sw_tx_level_at(&w, w.now + 1000u) == 1u);  /* line left at mark */
    }
}

int main(void)
{
    const uint8_t pl_echo[] = { 0xAA, 0xBB };
    n_echo = upd_frame_build(fr_echo, sizeof fr_echo, UPD_CMD_ECHO, pl_echo, 2);
    n_info = upd_frame_build(fr_info, sizeof fr_info, UPD_CMD_INFO,
                             (const uint8_t *)0, 0);
    CHECK(n_echo == 5 && n_info == 3);

    test_uart_pump();
    test_spi_pump();
    test_softuart();

    TEST_RESULT("pumps");
}
