/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher
 *
 * CBMC harness — Invariant 2: jump gating.
 *
 * Model: flash is a concrete array of fully nondet bytes (footer included),
 * STABLE across reads — so upd_app_valid is a deterministic predicate of
 * the flash contents and the harness can evaluate it independently.
 *
 * Property: after upd_boot_if_valid, `jumped ==> upd_app_valid(...)` held.
 * Because the flash model is stable, `valid` computed by the harness before
 * the call equals the check upd_boot_if_valid performs internally, so
 * assert(!jumped || valid) is exactly "jumped implies app_valid was true".
 *
 * Side properties proven along the way: the boot path never erases or
 * writes flash (stubs assert unreachable), never reads outside the app
 * region, and upd_boot_if_valid's return value reports the jump exactly.
 */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>

#include "updater/update.h"

#ifdef PROOF_SMALL_MODEL
#define MODEL_MAX_PAGE_SIZE 16u
#define MODEL_MAX_APP_PAGES 4u
#else
#error "harness tuned for PROOF_SMALL_MODEL (unwind bound 70); define it"
#endif

#define MODEL_MAX_REGION (MODEL_MAX_PAGE_SIZE * MODEL_MAX_APP_PAGES)   /* 64 */

uint8_t  nondet_u8(void);
uint16_t nondet_u16(void);

static uint16_t g_page_size;
static uint16_t g_app_pages;
static uint8_t  g_flash[MODEL_MAX_REGION];
static bool     g_jumped;

/* ---- port model -------------------------------------------------------- */

void port_info(port_info_t *out)
{
    out->page_size    = g_page_size;
    out->app_pages    = g_app_pages;
    out->device_id[0] = nondet_u8();
    out->device_id[1] = nondet_u8();
    out->device_id[2] = nondet_u8();
    out->device_id[3] = nondet_u8();
    out->bl_version   = nondet_u8();
}

uint8_t port_flash_read_byte(uint32_t offset)
{
    assert(offset < (uint32_t)g_page_size * g_app_pages);   /* confinement */
    return g_flash[offset];                    /* stable, deterministic */
}

void port_flash_erase_page(uint16_t page)
{
    (void)page;
    assert(0);   /* boot path must never erase */
}

void port_flash_write_page(uint16_t page, const uint8_t *data)
{
    (void)page; (void)data;
    assert(0);   /* boot path must never write */
}

void port_jump_to_app(void)
{
    g_jumped = true;
}

/* ---- proof entry point -------------------------------------------------- */

int main(void)
{
    g_page_size = nondet_u16();
    g_app_pages = nondet_u16();
    __CPROVER_assume(g_page_size <= MODEL_MAX_PAGE_SIZE);
    __CPROVER_assume(g_app_pages <= MODEL_MAX_APP_PAGES);

    /* fully nondet flash image, footer bytes included */
    for (uint32_t i = 0; i < MODEL_MAX_REGION; i++)
        g_flash[i] = nondet_u8();

    port_info_t info;
    port_info(&info);

    bool valid = upd_app_valid(&info);   /* the gate predicate, pre-evaluated */
    bool ret   = upd_boot_if_valid(&info);

    assert(!g_jumped || valid);          /* INVARIANT 2: jumped ==> valid  */
    assert(!valid || g_jumped);          /* completeness: valid ==> jumped */
    assert(ret == g_jumped);             /* return value reports the jump  */
    return 0;
}
