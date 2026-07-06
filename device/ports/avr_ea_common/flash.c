/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher
 *
 * NVMCTRL boundary — DS40002443A section 11. Bounds are the core's proven
 * obligation (page < app_pages, offset < region); this file does plain
 * address math and the datasheet command sequences, nothing else.
 *
 * Split erase/write sequence is section 11.3.2.3 "Programming", Option 2,
 * matching Microchip's own AVR64EA48 read-while-write example
 * (github.com/microchip-pic-avr-examples/avr64ea48-nvm-read-while-write-studio).
 * NOTE: on the EA the address/data go to the page buffer FIRST and the
 * CTRLA command executes immediately (section 11.3.2.4) — the reverse of
 * the AVR-DA "set mode, then store" order. */
#include <avr/io.h>
#include "port_geom.h"     /* transport-independent: this file is shared by
                              every AVR-EA port and must not see any one
                              port's transport configuration */
#include "updater/port.h"

_Static_assert(UPDATER_PAGE_SIZE == PROGMEM_PAGE_SIZE,
               "port page size must match the device flash page");
_Static_assert(UPDATER_APP_BASE
               + (uint32_t)UPDATER_APP_PAGES * UPDATER_PAGE_SIZE
               == PROGMEM_SIZE, "app region must end at top of flash");

void port_info(port_info_t *out)
{
    static const uint8_t id[4] = UPDATER_DEVICE_ID;
    out->page_size  = UPDATER_PAGE_SIZE;
    out->app_pages  = UPDATER_APP_PAGES;
    out->bl_version = UPDATER_BL_VERSION;
    for (uint8_t i = 0; i < 4u; i++)
        out->device_id[i] = id[i];
}

/* NVMCTRL.CTRLA is under CCP with the SPM key (Table 11-7); the unlock
 * must reach CTRLA within 4 instructions, which _PROTECTED_WRITE_SPM
 * guarantees (section 11.3.2.4 steps 2-3). */
static void nvm_cmd(uint8_t cmd)
{
    _PROTECTED_WRITE_SPM(NVMCTRL.CTRLA, cmd);
}

/* Section 11.3.2.4 step 1: confirm no ongoing operation via BOTH busy
 * flags (FLBUSY and EEBUSY) in NVMCTRL.STATUS before touching CTRLA. */
static void nvm_wait(void)
{
    while (NVMCTRL.STATUS & (NVMCTRL_FLBUSY_bm | NVMCTRL_EEBUSY_bm)) { }
}

/* Flash byte address -> pointer into the 32 KiB data-space window at
 * MAPPED_PROGMEM_START (0x8000), selecting the 32 KiB block via
 * CTRLB.FLMAP (section 11.3.2 "Addressing Flash in CPU Data Space").
 * CTRLB is under CCP with the IOREG key (Table 11-7) — a plain write is
 * silently ignored — hence _PROTECTED_WRITE; the avr-libc startup's own
 * __do_flmap_init does the same (visible in the disassembly).
 * The caller must flmap_restore() afterwards: the C runtime sets FLMAP to
 * the section it links .rodata against (crt __do_flmap_init; CTRLB resets
 * to 0x30 = top section, 11.5.2), so the window must never stay moved. */
static uint8_t flmap_saved;

static volatile uint8_t *flmap_window(uint32_t addr)
{
    flmap_saved = NVMCTRL.CTRLB;
    _PROTECTED_WRITE(NVMCTRL.CTRLB,
        (uint8_t)((flmap_saved & (uint8_t)~NVMCTRL_FLMAP_gm)
        | (uint8_t)(((addr >> 15) << NVMCTRL_FLMAP_gp) & NVMCTRL_FLMAP_gm)));
    return (volatile uint8_t *)(uint16_t)(MAPPED_PROGMEM_START
                                          + (addr & (MAPPED_PROGMEM_SIZE - 1u)));
}

static void flmap_restore(void)
{
    _PROTECTED_WRITE(NVMCTRL.CTRLB, flmap_saved);
}

void port_flash_erase_page(uint16_t page)
{
    uint32_t addr = UPDATER_APP_BASE + (uint32_t)page * UPDATER_PAGE_SIZE;
    nvm_wait();
    nvm_cmd(NVMCTRL_CMD_NOCMD_gc);      /* command changes must pass through NOCMD
                                           or STATUS.ERROR flags CMDCOLLISION (11.3.2.4) */
    /* One byte must land in the page buffer for FLPER to act on this page
     * (11.3.2.4.2); 0xFF because buffer loads AND with prior content
     * (11.3.2.2), making the dummy value flash-neutral either way. */
    *flmap_window(addr) = 0xFFu;
    nvm_cmd(NVMCTRL_CMD_FLPER_gc);      /* erase starts on the command write (11.3.2.3 Option 2) */
    nvm_wait();
    nvm_cmd(NVMCTRL_CMD_NOCMD_gc);
    flmap_restore();
}

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    uint32_t addr = UPDATER_APP_BASE + (uint32_t)page * UPDATER_PAGE_SIZE;
    nvm_wait();
    nvm_cmd(NVMCTRL_CMD_NOCMD_gc);
    /* ST stores through the window load the page buffer, not flash
     * (11.3.2.2). The buffer is auto-erased after every page write/erase
     * command and after reset (11.3.2.2 list), so it is guaranteed blank
     * here — every write_page follows an erase or a previous write. */
    volatile uint8_t *dst = flmap_window(addr);
    for (uint16_t i = 0; i < UPDATER_PAGE_SIZE; i++)
        dst[i] = data[i];
    nvm_cmd(NVMCTRL_CMD_FLPW_gc);       /* program buffer into the page (11.3.2.4.1) */
    nvm_wait();
    nvm_cmd(NVMCTRL_CMD_NOCMD_gc);
    flmap_restore();
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    /* Plain LD through the mapped window (11.3.2.1). offset < region is
     * the core's proven precondition. */
    volatile uint8_t *src = flmap_window(UPDATER_APP_BASE + offset);
    uint8_t v = *src;
    flmap_restore();
    return v;
}
