#ifndef UPDATER_APP_STUB_H
#define UPDATER_APP_STUB_H
/* Application-side re-entry stub (header-only). Ship this to app projects
 * (e.g. the motor_controller firmware): on the project's chosen trigger —
 * typically a write to a reserved/OVERRIDE register in its I2C register
 * file — call updater_reboot_to_bootloader(). It never returns; the
 * bootloader comes up resident (entry window skipped) and the host can
 * start an update session.
 *
 * Mechanism (all targets): write a magic + one's-complement pair to a
 * location that survives a soft reset but not power-on, then soft-reset.
 * The bootloader reads and clears the pair early in its init; power-on
 * garbage cannot fake the pair — the complement check rejects it.
 *
 * AVR-EA: the pair lives in the last 4 bytes of SRAM (SRAM keeps its
 * content through a software reset). The stack starts there, so the
 * bootloader's entry.c captures the pair in .init3, before the C
 * runtime's first CALL instruction can push a return address over it
 * (DS40002443A section 8.4). Constraint: app and bootloader MUST be built
 * for the same MCU — both sides derive the pair's address from their own
 * <avr/io.h> INTERNAL_SRAM_END, so a device mismatch silently breaks
 * re-entry. Verified for AVR64EA28 (SRAM 0x6800-0x7FFF => pair 0x7FFC).
 *
 * RP2350: the pair lives in watchdog SCRATCH2/SCRATCH3 — "Scratch
 * register. Information persists through soft reset of the chip"
 * (pico-sdk hardware/regs/watchdog.h) — and the reset is
 * watchdog_reboot(0, 0, 0), whose pc=0 normal-boot path makes the ROM
 * re-run the flash image at the XIP base, i.e. the bootloader (pico-sdk
 * src/rp2_common/hardware_watchdog/watchdog.c). SCRATCH4..7 are
 * deliberately avoided: the SDK and the ROM's watchdog boot vectoring
 * assign meaning to them (same file, magic 0xb007c0d3 protocol); the SDK
 * writes no other scratch register. No stack race exists on this target —
 * the pair is in peripheral registers, not under the stack. */
#include <stdint.h>

#if defined(__AVR_ARCH__)
#include <avr/io.h>

#define UPDATER_ENTRY_MAGIC 0xB007u
/* Last 4 SRAM bytes: magic at END-3, complement at END-1 (16-bit each). */
#define UPDATER_ENTRY_PTR   ((volatile uint16_t *)(INTERNAL_SRAM_END - 3u))
#define UPDATER_ENTRY_NPTR  ((volatile uint16_t *)(INTERNAL_SRAM_END - 1u))

static inline void updater_reboot_to_bootloader(void)
{
    /* No interrupt may push onto the stack between writing the pair and
     * the reset — the pair sits where pushes land. */
    __asm__ __volatile__("cli" ::: "memory");
    *UPDATER_ENTRY_PTR  = UPDATER_ENTRY_MAGIC;
    *UPDATER_ENTRY_NPTR = (uint16_t)~UPDATER_ENTRY_MAGIC;
    /* RSTCTRL.SWRR is under CCP-IOREG (DS40002443A Table 14-2); writing
     * SWRE = 1 resets immediately (14.3.2.1.5, 14.5.2). */
    _PROTECTED_WRITE(RSTCTRL.SWRR, RSTCTRL_SWRE_bm);
    for (;;) { }   /* unreachable; reset is already in flight */
}

#elif defined(PICO_RP2350)
#include "hardware/structs/watchdog.h"
#include "hardware/watchdog.h"

#define UPDATER_ENTRY_MAGIC_RP2350   0xB007CA11u
/* watchdog_hw->scratch[] indices of the magic and its complement. */
#define UPDATER_ENTRY_SCRATCH_MAGIC  2u
#define UPDATER_ENTRY_SCRATCH_CHECK  3u

static inline void updater_reboot_to_bootloader(void)
{
    watchdog_hw->scratch[UPDATER_ENTRY_SCRATCH_MAGIC] =
        UPDATER_ENTRY_MAGIC_RP2350;
    watchdog_hw->scratch[UPDATER_ENTRY_SCRATCH_CHECK] =
        (uint32_t)~UPDATER_ENTRY_MAGIC_RP2350;
    /* pc = 0: "reboot into regular flash path" — the ROM re-picks the
     * image at the XIP base (the bootloader). delay 0 fires the watchdog
     * immediately (WATCHDOG_CTRL_TRIGGER). */
    watchdog_reboot(0u, 0u, 0u);
    for (;;) { }   /* unreachable; reset is already in flight */
}

#else
#  error "updater/app_stub.h has no stub for this target - add an #elif above: write a magic + one's-complement pair to storage that survives a soft reset but not power-on, then soft-reset (see the header comment; build app and bootloader for the same MCU)"
#endif

#endif
