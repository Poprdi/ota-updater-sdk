/* Minimal validation app for the rp2350_uart port — the image the first
 * hardware cycle flashes through the bootloader. Linked at the app region
 * (memmap_app.ld, XIP base + 64 KiB); the bootloader enters it via
 * VTOR/MSP/reset-handler, so this is an ordinary pico-sdk binary.
 *
 * Observable behavior (chosen so validation needs only the UART wiring
 * that the bootloader itself needs):
 *   - toggles GPIO25 every 500 ms. NOTE: on the Pico 2 W the onboard LED
 *     hangs off the CYW43 wireless chip, NOT GPIO25 — the toggle is
 *     scope/LED-on-a-jumper observable, deliberately avoiding the cyw43
 *     driver stack in a validation image. On a non-W Pico 2 the onboard
 *     LED blinks.
 *   - prints a heartbeat line on UART0 (GPIO0/1, 115200 8N1 — the same
 *     wiring the bootloader uses), so the serial adapter shows the app
 *     is alive immediately after BOOT.
 *   - re-enters the bootloader when it receives the byte 'U' on UART0:
 *     exercises updater/app_stub.h's RP2350 updater_reboot_to_bootloader
 *     (watchdog-scratch pair + watchdog reboot), i.e. the app-requested
 *     entry path, without power-cycling.
 *
 * The 500 ms wait deliberately uses sleep_ms (alarm IRQ + WFE), not a
 * busy-poll, so the heartbeat is falsifiable evidence that the bootloader
 * hands off reset-equivalent state: with PRIMASK left set across the jump
 * the app would hang in the first sleep_ms forever. */
#include "hardware/gpio.h"
#include "hardware/uart.h"
#include "pico/stdlib.h"

#include "updater/app_stub.h"

int main(void)
{
    gpio_init(25);
    gpio_set_dir(25, GPIO_OUT);

    uart_init(uart0, 115200);
    gpio_set_function(0, GPIO_FUNC_UART);
    gpio_set_function(1, GPIO_FUNC_UART);

    bool on = false;
    for (;;) {
        gpio_put(25, on);
        on = !on;
        uart_puts(uart0, "blink app alive\r\n");

        /* IRQ-dependent wait (see header note): 50 x 10 ms sleep_ms
         * slices accumulate to the 500 ms heartbeat period while the
         * 'U'-byte poll stays responsive (<= 10 ms latency). */
        for (unsigned slice = 0; slice < 50; slice++) {
            if (uart_is_readable(uart0) &&
                uart_getc(uart0) == 'U') {
                uart_puts(uart0, "re-entering bootloader\r\n");
                uart_tx_wait_blocking(uart0);
                updater_reboot_to_bootloader();
            }
            sleep_ms(10);
        }
    }
}
