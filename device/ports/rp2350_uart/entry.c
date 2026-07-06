/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher
 *
 * Bootloader side of the re-entry pair. Address/magic definitions are
 * shared with the app via updater/app_stub.h — single source of truth.
 *
 * RP2350 mechanism: two watchdog scratch registers hold magic + one's
 * complement. "Scratch register. Information persists through soft reset
 * of the chip." (sdk:src/rp2350/hardware_regs/include/hardware/regs/
 * watchdog.h, WATCHDOG_SCRATCH2/3) — so the pair written by the app
 * survives the watchdog_reboot that follows it, and only a power-on
 * reset clears the registers to 0 (which cannot fake the pair: the
 * complement check rejects 0/0 and any single-register garbage).
 *
 * SCRATCH2/3 are free for this use: the SDK and the ROM's watchdog boot
 * vectoring only assign meaning to SCRATCH4..7 (sdk:src/rp2_common/
 * hardware_watchdog/watchdog.c watchdog_reboot/watchdog_enable — the only
 * scratch writers in the SDK).
 *
 * No early-capture discipline is needed here, unlike the AVR ports'
 * .init3 dance: the pair lives in peripheral registers, not at the top of
 * SRAM, so no stack push can destroy it. crt0/runtime_init touch no
 * watchdog scratch register; main() calls updater_entry_capture() as its
 * first statement, before any SDK call. */
#include <stdbool.h>

#include "hardware/structs/watchdog.h"

#include "updater/app_stub.h"
#include "updater/port.h"

static bool entry_flag;

void updater_entry_capture(void)
{
    entry_flag =
        (watchdog_hw->scratch[UPDATER_ENTRY_SCRATCH_MAGIC]
             == UPDATER_ENTRY_MAGIC_RP2350) &&
        (watchdog_hw->scratch[UPDATER_ENTRY_SCRATCH_CHECK]
             == (uint32_t)~UPDATER_ENTRY_MAGIC_RP2350);
    /* Always clear: a stale pair must not re-trigger on the next reset. */
    watchdog_hw->scratch[UPDATER_ENTRY_SCRATCH_MAGIC] = 0u;
    watchdog_hw->scratch[UPDATER_ENTRY_SCRATCH_CHECK] = 0u;
}

bool port_entry_requested(void)
{
    return entry_flag;
}
