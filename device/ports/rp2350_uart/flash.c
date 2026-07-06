/* QSPI flash boundary — pico-sdk hardware_flash over the ROM routines.
 * Bounds are the core's proven obligation (page < app_pages, offset <
 * region); this file does address math, the 128 -> 256-byte write
 * coalescing, and the 128-byte-page -> 4 KiB-sector erase mapping.
 *
 * Geometry mismatch and how it is bridged (PORT_AUDIT.md rows F1-F7):
 *
 *   protocol page   128 B   (port contract, wire frames)
 *   flash program   256 B   (FLASH_PAGE_SIZE — flash_range_program requires
 *                            offset/count aligned to it)
 *   flash erase    4096 B   (FLASH_SECTOR_SIZE — flash_range_erase ditto)
 *
 * WRITE coalescing (holdback buffer): an even protocol page is HELD in RAM
 * instead of programmed; when its odd partner arrives (the host writes
 * ascending), both halves are programmed as one 256-byte flash page. Any
 * other next event flushes the held page with an all-0xFF partner half.
 * 0xFF padding is write-neutral on NOR flash: programming can only clear
 * bits (erased state is 0xFF), so a later program of the partner half —
 * or a host retry re-programming identical data — never corrupts what is
 * already there. Correctness therefore does NOT depend on ascending order;
 * ascending order merely makes every 256-byte page programmed exactly once.
 *
 * Holdback visibility: held data is not yet in flash, so any read
 * (VERIFY/INFO/BOOT CRC walks) flushes first — port_flash_read_byte is the
 * single read path and does exactly that. ERASE_APP drops the held page
 * (the erase invalidates it anyway).
 *
 * ERASE mapping: the core calls port_flash_erase_page for every protocol
 * page 0..app_pages-1; only the call whose page starts a 4 KiB sector
 * (page % 32 == 0) issues flash_range_erase, the rest are no-ops — 8192
 * calls become 256 sector erases covering the region exactly.
 *
 * RAM-function discipline: this file executes from XIP flash, which is
 * unavailable while the QSPI device is being programmed/erased. That is
 * safe because flash_range_erase/flash_range_program and every helper on
 * their path are __no_inline_not_in_flash_func (RAM-resident) and only
 * call ROM routines; they re-enable XIP and flush the XIP cache before
 * returning (sdk:src/rp2_common/hardware_flash/flash.c). Nothing else may
 * fetch from flash meanwhile: interrupts are disabled across each call
 * (this bootloader never enables any, but the wrapper makes the invariant
 * local, per the warning in sdk hardware/flash.h), and core 1 is never
 * started (it sleeps in the ROM).  *
 * hold_valid deliberately survives upd_init: a page held by a dead host
 * session materializes on the next session's first flash read. Harmless —
 * it is data the host asked to write, and WRITE still requires a
 * same-session ERASE.
 */
#include <string.h>

#include "hardware/flash.h"
#include "hardware/sync.h"

#include "port_cfg.h"
#include "updater/port.h"

_Static_assert(2u * UPDATER_PAGE_SIZE == FLASH_PAGE_SIZE,
               "coalescing assumes two protocol pages per flash page");
#define PAGES_PER_SECTOR (FLASH_SECTOR_SIZE / UPDATER_PAGE_SIZE)   /* 32 */
_Static_assert(UPDATER_APP_PAGES % PAGES_PER_SECTOR == 0,
               "app region must be a whole number of erase sectors");
_Static_assert(UPDATER_APP_FLASH_OFFSET % FLASH_SECTOR_SIZE == 0,
               "app region must start on an erase-sector boundary");

void port_info(port_info_t *out)
{
    static const uint8_t id[4] = UPDATER_DEVICE_ID;
    out->page_size  = UPDATER_PAGE_SIZE;
    out->app_pages  = UPDATER_APP_PAGES;
    out->bl_version = UPDATER_BL_VERSION;
    for (uint8_t i = 0; i < 4u; i++)
        out->device_id[i] = id[i];
}

/* ---- holdback state ---------------------------------------------------- */

static uint8_t  hold[UPDATER_PAGE_SIZE];   /* held even protocol page */
static uint16_t hold_page;
static bool     hold_valid;

/* Program one 256-byte flash page from two 128-byte halves; either half
 * may be NULL = 0xFF fill (write-neutral, see file header). even_page is
 * the even protocol page index of the low half. */
static void prog_flash_page(uint16_t even_page,
                            const uint8_t *lo, const uint8_t *hi)
{
    static uint8_t unit[FLASH_PAGE_SIZE];
    if (lo) memcpy(unit, lo, UPDATER_PAGE_SIZE);
    else    memset(unit, 0xFF, UPDATER_PAGE_SIZE);
    if (hi) memcpy(unit + UPDATER_PAGE_SIZE, hi, UPDATER_PAGE_SIZE);
    else    memset(unit + UPDATER_PAGE_SIZE, 0xFF, UPDATER_PAGE_SIZE);

    uint32_t offs = UPDATER_APP_FLASH_OFFSET
                  + (uint32_t)even_page * UPDATER_PAGE_SIZE;
    uint32_t irq = save_and_disable_interrupts();
    flash_range_program(offs, unit, FLASH_PAGE_SIZE);
    restore_interrupts_from_disabled(irq);
}

static void hold_flush(void)
{
    if (!hold_valid)
        return;
    hold_valid = false;             /* before programming: no reentrancy
                                       here, but the order costs nothing */
    prog_flash_page(hold_page, hold, (const uint8_t *)0);
}

/* ---- port contract ------------------------------------------------------ */

void port_flash_erase_page(uint16_t page)
{
    hold_valid = false;             /* erase invalidates held data */
    if (page % PAGES_PER_SECTOR != 0u)
        return;                     /* interior of a sector already erased
                                       by the page that started it */
    uint32_t offs = UPDATER_APP_FLASH_OFFSET
                  + (uint32_t)page * UPDATER_PAGE_SIZE;
    uint32_t irq = save_and_disable_interrupts();
    flash_range_erase(offs, FLASH_SECTOR_SIZE);
    restore_interrupts_from_disabled(irq);
}

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    if (hold_valid && page == hold_page) {
        /* host retry of the held page: replace, don't program twice */
        memcpy(hold, data, UPDATER_PAGE_SIZE);
        return;
    }
    if (hold_valid && page == (uint16_t)(hold_page + 1u)) {
        /* partner arrived: one 256-byte program for both halves */
        uint16_t lo_page = hold_page;
        hold_valid = false;
        prog_flash_page(lo_page, hold, data);
        return;
    }
    hold_flush();
    if ((page & 1u) == 0u) {        /* even half: hold for coalescing */
        memcpy(hold, data, UPDATER_PAGE_SIZE);
        hold_page  = page;
        hold_valid = true;
    } else {                        /* lone odd half: 0xFF low partner */
        prog_flash_page((uint16_t)(page - 1u), (const uint8_t *)0, data);
    }
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    /* Reads must see held data as it will be in flash (VERIFY/BOOT walk
     * the region through this function). */
    hold_flush();
    /* Plain XIP read; flash_range_erase/program flushed the XIP cache
     * before returning, so this cannot see stale pre-write content
     * (sdk:src/rp2_common/hardware_flash/flash.c: flash_flush_cache_func
     * on every path). offset < region is the core's proven precondition. */
    return *(const volatile uint8_t *)(UPDATER_APP_BASE + offset);
}
