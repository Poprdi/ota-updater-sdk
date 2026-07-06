/* Polled UART0 (ARM PL011) — pico-sdk hardware_uart. Zero protocol
 * knowledge: this file is only the four uart_pump register ops plus
 * init/drain; framing lives in link_stream, commands in the core.
 *
 * No interrupts: PRIMASK is never cleared for anything (the bootloader
 * enables no IRQ at the NVIC or the peripheral), UARTIMSC keeps every
 * interrupt masked at its reset value of 0 — the FR flags are set by
 * hardware regardless and are polled through the pump ops, the same
 * polled discipline as the AVR ports.
 *
 * Pins: UART0 on GPIO0 (TX) / GPIO1 (RX) — the Pico/Pico 2 default UART
 * pinout (sdk:src/boards/include/boards/pico2_w.h:
 * PICO_DEFAULT_UART_TX_PIN 0, PICO_DEFAULT_UART_RX_PIN 1). */
#include "hardware/gpio.h"
#include "hardware/uart.h"

#include "port_cfg.h"

void updater_uart_init(void)
{
    /* uart_init resets/unresets the UART block, programs the fractional
     * baud divisor from clk_peri (returning the achieved rate), sets 8N1
     * with FIFOs enabled (LCR_H WLEN=8, FEN=1) and enables UARTEN|TXE|RXE
     * (sdk:src/rp2_common/hardware_uart/uart.c uart_init). 8N1 is the
     * frame format the whole SDK speaks. */
    (void)uart_init(uart0, UPDATER_UART_BAUD);
    /* Route the pins AFTER the UART is enabled: the PL011 TX output is
     * already driving the idle mark by then, so the host never sees a
     * floating or low glitch it could latch as a start bit (same concern
     * as the AVR port's OUT-before-DIR order). */
    gpio_set_function(0, GPIO_FUNC_UART);   /* TX */
    gpio_set_function(1, GPIO_FUNC_UART);   /* RX */
}

/* ---- uart_pump register ops (contract in ../skeletons/uart_pump.h) ---- */

static bool u0_rx_ready(void *ctx)
{
    (void)ctx;
    /* FR.RXFE clear = at least one byte in the RX FIFO; readable RIGHT
     * NOW, and false once drained — exactly the pump contract
     * (sdk:src/rp2_common/hardware_uart/include/hardware/uart.h
     * uart_is_readable). */
    return uart_is_readable(uart0);
}

static uint8_t u0_rx_read(void *ctx)
{
    (void)ctx;
    /* DR read pops the RX FIFO; bits [11:8] carry the per-byte error
     * flags (OE/BE/PE/FE) and are deliberately dropped by the cast: a
     * damaged or lost byte fails the frame CRC, link_stream drops the
     * frame, and the host's timeout+retry recovers — the same policy as
     * every stream pump (uart_pump.h, link.h). PL011 error flags need no
     * clearing to keep receiving (RSR is not consulted). */
    return (uint8_t)uart_get_hw(uart0)->dr;
}

static bool u0_tx_ready(void *ctx)
{
    (void)ctx;
    /* FR.TXFF clear = TX FIFO has room; comes true within one byte time
     * once the shifter drains, which is what makes the pump's blocking
     * wait safe (uart_is_writable, same header). */
    return uart_is_writable(uart0);
}

static void u0_tx_write(void *ctx, uint8_t b)
{
    (void)ctx;
    uart_get_hw(uart0)->dr = b;     /* only after tx_ready == true */
}

const uart_pump_ops_t uart0_pump_ops = {
    u0_rx_ready, u0_rx_read, u0_tx_ready, u0_tx_write
};

void updater_uart_tx_drain(void)
{
    /* FR.BUSY covers the transmit shift register, not just the FIFO:
     * "the UART is busy transmitting data ... until the complete byte,
     * including all the stop bits, has been sent" (PL011 FR.BUSY;
     * uart_tx_wait_blocking polls exactly this bit). Gates the BOOT jump
     * so de-initializing the UART cannot truncate the reply's last byte. */
    uart_tx_wait_blocking(uart0);
}
