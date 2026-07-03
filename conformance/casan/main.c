/* Sanitizer campaign: the same conformance scenarios re-run in plain C
 * under ASan + UBSan (-fno-sanitize-recover=all). The Rust tests prove the
 * contract; this run proves the C side reaches those states with zero
 * undefined behaviour and zero memory errors. Exit 0 = clean.
 */
#include <stdio.h>
#include <string.h>

#include "sim_port.h"
#include "updater/crc32.h"
#include "updater/crc8.h"
#include "updater/proto.h"

static int g_fail;
#define CHECK(cond)                                                        \
    do {                                                                   \
        if (!(cond)) {                                                     \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);\
            g_fail = 1;                                                    \
        }                                                                  \
    } while (0)

#define APP_LEN 3000u

static uint8_t g_app[APP_LEN];

/* Same generator family as the Rust campaigns: deterministic image. */
static void fill_app(void)
{
    uint32_t s = 0x2A2A2A2Au;
    for (uint32_t i = 0; i < APP_LEN; i++) {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        g_app[i] = (uint8_t)(s & 0xFFu);
    }
}

static uint32_t app_crc(void)
{
    uint32_t c = upd_crc32_init();
    for (uint32_t i = 0; i < APP_LEN; i++)
        c = upd_crc32_update(c, g_app[i]);
    return upd_crc32_final(c);
}

/* One request/response exchange; returns the status byte, or a negative
 * code when no valid command-matched response came back. */
static int exchange(uint8_t cmd, const uint8_t *payload, uint8_t plen)
{
    uint8_t req[256];
    uint8_t resp[259];
    uint8_t n = upd_frame_build(req, 255u, cmd, payload, plen);
    if (n == 0u)
        return -1;
    uint16_t rn = sim_request(req, n, resp);
    if (rn == 0u)
        return -2;                       /* dead device / no reply */
    upd_frame_t f;
    if (rn > 255u || !upd_frame_parse(resp, (uint8_t)rn, &f))
        return -3;
    if (f.cmd != (uint8_t)(cmd | UPD_RSP_FLAG) || f.len < 1u)
        return -4;
    return f.payload[0];
}

/* Render page `page` of the padded+footered image into out[SIM_PAGE_SIZE]. */
static void render_page(uint32_t crc, uint16_t page, uint8_t *out)
{
    for (uint32_t i = 0; i < SIM_PAGE_SIZE; i++) {
        uint32_t off = (uint32_t)page * SIM_PAGE_SIZE + i;
        out[i] = (off < APP_LEN) ? g_app[off] : 0xFFu;
    }
    if (page == SIM_APP_PAGES - 1u) {
        uint8_t *ftr = out + SIM_PAGE_SIZE - 16u;
        ftr[0] = 0x4Fu; ftr[1] = 0x54u; ftr[2] = 0x41u; ftr[3] = 0x55u;
        for (uint8_t i = 0; i < 4u; i++) {
            ftr[4u + i]  = (uint8_t)((APP_LEN >> (8u * i)) & 0xFFu);
            ftr[8u + i]  = (uint8_t)((crc >> (8u * i)) & 0xFFu);
            ftr[12u + i] = 0xFFu;
        }
    }
}

static const uint8_t ERASE_MAGIC[4] = { 0x45u, 0x52u, 0x41u, 0x53u };

/* Full update: ERASE, WRITE every page (all 32 — the C campaign exercises
 * the whole region), VERIFY. Returns 0 on success, -1 on any refusal or
 * lost reply (the power-cut runs die in here on purpose). */
static int do_update(uint32_t crc)
{
    if (exchange(UPD_CMD_ERASE_APP, ERASE_MAGIC, 4u) != UPD_ST_OK)
        return -1;
    for (uint16_t p = 0; p < SIM_APP_PAGES; p++) {
        uint8_t payload[2u + SIM_PAGE_SIZE];
        payload[0] = (uint8_t)(p & 0xFFu);
        payload[1] = (uint8_t)((p >> 8) & 0xFFu);
        render_page(crc, p, payload + 2);
        if (exchange(UPD_CMD_WRITE_PAGE, payload, (uint8_t)sizeof payload)
            != UPD_ST_OK)
            return -1;
    }
    uint8_t vp[8];
    for (uint8_t i = 0; i < 4u; i++) {
        vp[i]      = (uint8_t)((APP_LEN >> (8u * i)) & 0xFFu);
        vp[4u + i] = (uint8_t)((crc >> (8u * i)) & 0xFFu);
    }
    if (exchange(UPD_CMD_VERIFY, vp, 8u) != UPD_ST_OK)
        return -1;
    return 0;
}

/* ---- 1. golden exchange ------------------------------------------------ */

static void run_golden(void)
{
    sim_reset(false);
    static const uint8_t info_req[3] = { 0x01u, 0x00u, 0x15u };
    uint8_t resp[259];
    uint16_t rn = sim_request(info_req, 3u, resp);
    CHECK(rn == 15u);
    upd_frame_t f;
    CHECK(upd_frame_parse(resp, (uint8_t)rn, &f));
    CHECK(f.cmd == (UPD_CMD_INFO | UPD_RSP_FLAG));
    CHECK(f.len == 12u);
    CHECK(f.payload[0] == UPD_ST_OK && f.payload[1] == UPD_PROTO_VERSION);
    CHECK(f.payload[7] == 128u && f.payload[8] == 0u);   /* page_size LE */
    CHECK(f.payload[9] == 32u && f.payload[10] == 0u);   /* app_pages LE */
    CHECK(f.payload[11] == 0u);                          /* no app yet   */
}

/* ---- 2. full update campaign ------------------------------------------- */

static void run_campaign(uint32_t crc)
{
    sim_reset(false);
    CHECK(do_update(crc) == 0);
    CHECK(exchange(UPD_CMD_BOOT, NULL, 0u) == UPD_ST_OK);
    CHECK(sim_jumped());
    /* flash spot check: first byte, last data byte, footer magic */
    CHECK(sim_flash()[0] == g_app[0]);
    CHECK(sim_flash()[APP_LEN - 1u] == g_app[APP_LEN - 1u]);
    CHECK(sim_flash()[SIM_REGION - 16u] == 0x4Fu);
}

/* ---- 3. power-cut sweep at page boundaries ------------------------------ */

static void run_powercut_sweep(uint32_t crc)
{
    /* Every flash op is one page op, so cutting at each op index 1..total
     * IS the page-boundary sweep: 32 erases + 32 writes = 64 cuts. */
    const uint32_t total = SIM_APP_PAGES + SIM_APP_PAGES;
    for (uint32_t n = 1; n <= total; n++) {
        sim_reset(false);
        sim_powercut_after(n);
        CHECK(do_update(crc) != 0);      /* the cut must abort the update  */
        CHECK(sim_powercut_hit());       /* ...and op n must be reached    */

        sim_reset(true);                 /* power restored, flash torn     */
        CHECK(exchange(UPD_CMD_BOOT, NULL, 0u) == UPD_ST_NO_APP);
        CHECK(!sim_jumped());

        CHECK(do_update(crc) == 0);      /* no brick: clean re-flash works */
        CHECK(exchange(UPD_CMD_BOOT, NULL, 0u) == UPD_ST_OK);
        CHECK(sim_jumped());
    }
    CHECK(sim_flash_ops() > 0u);
}

/* ---- 4. FSM sweep: 2 states x 256 cmds, bad-CRC variant ----------------- */

static void run_fsm_sweep(void)
{
    static uint8_t before[SIM_REGION];
    for (int erased = 0; erased <= 1; erased++) {
        for (uint16_t c = 0; c <= 255u; c++) {
            sim_reset(false);
            if (erased)
                CHECK(exchange(UPD_CMD_ERASE_APP, ERASE_MAGIC, 4u)
                      == UPD_ST_OK);
            memcpy(before, sim_flash(), SIM_REGION);

            uint8_t cmd = (uint8_t)c;
            uint8_t req[3] = { cmd, 0u, 0u };
            req[2] = (uint8_t)(upd_crc8(req, 2u) ^ 0xFFu);   /* break CRC */
            uint8_t resp[259];
            uint16_t rn = sim_request(req, 3u, resp);
            upd_frame_t f;
            CHECK(rn >= 4u && rn <= 255u);
            CHECK(upd_frame_parse(resp, (uint8_t)rn, &f));
            CHECK(f.len >= 1u && f.payload[0] == UPD_ST_BAD_FRAME);
            CHECK(memcmp(before, sim_flash(), SIM_REGION) == 0);
        }
    }
}

int main(void)
{
    fill_app();
    uint32_t crc = app_crc();

    run_golden();
    run_campaign(crc);
    run_powercut_sweep(crc);
    run_fsm_sweep();

    if (g_fail) {
        fprintf(stderr, "casan: FAILURES\n");
        return 1;
    }
    printf("casan: golden + campaign + 64-cut sweep + 2x256 FSM sweep clean\n");
    return 0;
}
