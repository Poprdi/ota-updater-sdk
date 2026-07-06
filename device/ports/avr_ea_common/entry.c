/* Bootloader side of the re-entry pair. Address/magic definitions are
 * shared with the app via updater/app_stub.h — single source of truth. */
#include <avr/io.h>
#include <stdbool.h>
#include "updater/app_stub.h"
#include "updater/port.h"

/* .noinit: the flag is written below in .init3, BEFORE .init4 clears .bss —
 * a normal static would be zeroed again. */
static bool entry_flag __attribute__((section(".noinit")));

/* Capture + clear the pair in .init3, i.e. after the C runtime has set up
 * r1/SREG (.init2) but BEFORE the first CALL instruction: SP resets to
 * RAMEND (top of SRAM, DS40002443A section 8.4), so the very first push —
 * crt's `call main` at the latest — lands on 0x7FFE/0x7FFF and destroys
 * the complement word. `naked` because an epilogue RET here would pop a
 * garbage return address; .initN sections fall through.
 * AUDIT: the disassembly of this function must contain no push/call/ret
 * before the pair is read (checked in PORT_AUDIT.md). */
__attribute__((used, naked, section(".init3")))
static void updater_entry_capture(void)
{
    uint16_t m = *UPDATER_ENTRY_PTR;
    uint16_t c = *UPDATER_ENTRY_NPTR;
    entry_flag = (m == UPDATER_ENTRY_MAGIC)
              && (c == (uint16_t)~UPDATER_ENTRY_MAGIC);
    /* Always clear: a stale pair must not re-trigger on the next reset. */
    *UPDATER_ENTRY_PTR  = 0u;
    *UPDATER_ENTRY_NPTR = 0u;
}

bool port_entry_requested(void)
{
    return entry_flag;
}
