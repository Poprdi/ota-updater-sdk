/* Simulated device: the 9 port_* functions over a static flash array,
 * plus power-cut injection, confinement enforcement and the request cycle.
 *
 * Confinement is enforced with abort(), not assert(): the CBMC proofs
 * already show the core never issues an out-of-range flash op, and this
 * belt must stay fastened even if someone builds with NDEBUG. Any abort
 * here IS a conformance failure.
 */
#include <stdlib.h>
#include <string.h>

#include "sim_port.h"
#include "updater/proto.h"
#include "updater/update.h"

static uint8_t       g_flash[SIM_REGION];
static upd_session_t g_session;
static uint32_t      g_ops;      /* flash ops since sim_reset            */
static uint32_t      g_cut_at;   /* op number (since reset) that tears; 0 = off */
static bool          g_cut_hit;
static bool          g_dead;     /* power lost: silent until sim_reset   */
static bool          g_jumped;   /* BOOT gate fired                      */

/* ---- the port contract ------------------------------------------------- */

void port_info(port_info_t *out)
{
    out->app_base     = 0x1000u;
    out->page_size    = (uint16_t)SIM_PAGE_SIZE;
    out->app_pages    = (uint16_t)SIM_APP_PAGES;
    out->device_id[0] = (uint8_t)'S';
    out->device_id[1] = (uint8_t)'I';
    out->device_id[2] = (uint8_t)'M';
    out->device_id[3] = (uint8_t)'0';
    out->bl_version   = 1u;
}

/* Count one flash op; returns true if the armed power cut lands on it.
 * The torn op still transfers its first half — a mid-page tear, the way
 * real flash dies — and everything after it is silence. */
static bool op_tears(void)
{
    g_ops++;
    if (g_cut_at != 0u && g_ops == g_cut_at) {
        g_cut_hit = true;
        g_dead    = true;
        return true;
    }
    return false;
}

void port_flash_erase_page(uint16_t page)
{
    if (page >= SIM_APP_PAGES)
        abort();                       /* confinement violation */
    if (g_dead)
        return;                        /* power already lost    */
    uint8_t *p = &g_flash[(uint32_t)page * SIM_PAGE_SIZE];
    if (op_tears()) {
        memset(p, 0xFF, SIM_PAGE_SIZE / 2u);
        return;
    }
    memset(p, 0xFF, SIM_PAGE_SIZE);
}

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    if (page >= SIM_APP_PAGES)
        abort();                       /* confinement violation */
    if (g_dead)
        return;
    uint8_t *p = &g_flash[(uint32_t)page * SIM_PAGE_SIZE];
    if (op_tears()) {
        memcpy(p, data, SIM_PAGE_SIZE / 2u);
        return;
    }
    memcpy(p, data, SIM_PAGE_SIZE);
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    if (offset >= SIM_REGION)
        abort();                       /* confinement violation */
    return g_flash[offset];
}

/* The sim drives parse->handle->build directly (sim_request below), so the
 * link-layer half of the contract is inert here; the symbols exist because
 * a port implements all nine. */
bool port_recv(uint8_t *buf, uint8_t *len)
{
    (void)buf;
    (void)len;
    return false;
}

void port_send(const uint8_t *buf, uint8_t len)
{
    (void)buf;
    (void)len;
}

uint16_t port_ticks_ms(void)
{
    return 0u;
}

bool port_entry_requested(void)
{
    return false;
}

void port_jump_to_app(void)
{
    /* Test-double semantics (like device/test/fake_port.c): record the
     * gate firing and return, so upd_boot_if_valid's return path runs. */
    g_jumped = true;
}

/* ---- sim controls ------------------------------------------------------ */

void sim_reset(bool preserve_flash)
{
    if (!preserve_flash)
        memset(g_flash, 0xFF, sizeof g_flash);
    g_ops     = 0u;
    g_cut_at  = 0u;
    g_cut_hit = false;
    g_dead    = false;
    g_jumped  = false;
    upd_init(&g_session);
}

void sim_powercut_after(uint32_t flash_ops)
{
    /* Relative to now: the Nth op from this call tears. */
    g_cut_at = (flash_ops == 0u) ? 0u : g_ops + flash_ops;
}

bool sim_powercut_hit(void)
{
    return g_cut_hit;
}

uint32_t sim_flash_ops(void)
{
    return g_ops;
}

bool sim_jumped(void)
{
    return g_jumped;
}

uint8_t *sim_flash(void)
{
    return g_flash;
}

uint16_t sim_request(const uint8_t *frame, uint16_t len, uint8_t *resp)
{
    if (g_dead || g_jumped)
        return 0u;                     /* nobody home */

    /* Payload scratch: largest handler output is INFO (12) / ECHO (17);
     * 252 is the wire maximum, so no handler can ever be truncated. */
    uint8_t payload[252];
    uint8_t plen;
    uint8_t cmd;

    upd_frame_t req;
    if (len <= 255u && upd_frame_parse(frame, (uint8_t)len, &req)) {
        cmd  = req.cmd;
        plen = upd_handle(&g_session, &req, payload, (uint8_t)sizeof payload);
    } else {
        /* Reference main-loop behavior for unparseable input: answer
         * ST_BAD_FRAME, echoing the first received byte as CMD (0x00 if
         * nothing arrived). State untouched: upd_handle never ran. */
        cmd        = (len >= 1u) ? frame[0] : 0x00u;
        payload[0] = UPD_ST_BAD_FRAME;
        plen       = 1u;
    }

    uint8_t n = upd_frame_build(resp, 255u, (uint8_t)(cmd | UPD_RSP_FLAG),
                                payload, plen);

    if (g_dead)
        return 0u;   /* the cut landed mid-handling: no reply escapes */

    /* Main-loop BOOT ordering: reply first, then take the gate. The gate
     * re-validates flash; the pending flag is never trusted stale. */
    if (g_session.boot_pending) {
        g_session.boot_pending = false;
        (void)upd_boot_if_valid(&g_session.info);
    }
    return n;
}
