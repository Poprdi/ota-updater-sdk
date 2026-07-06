/* Polled USART0 — DS40002443A section 25. Zero protocol knowledge: this
 * file is only the four uart_pump register ops plus init/drain; framing
 * lives in link_stream, commands in the core.
 *
 * No interrupts: SREG.I stays 0 for the bootloader's whole life and
 * USART0.CTRLA keeps every enable at 0 — the STATUS flags are set by
 * hardware regardless and are polled through the pump ops (same polled
 * discipline as the TWI port). */
#include <avr/io.h>
#include "port_cfg.h"

/* ---- BAUD register value, computed at compile time ---------------------
 * Table 25-1 (section 25.3.2.2.1, fractional baud generator):
 *
 *     BAUD = 64 * f_CLK_PER / (S * f_BAUD),  rounded to nearest
 *
 * with S = 16 samples/bit in Normal mode, S = 8 in Double-Speed (CLK2X)
 * mode; the register holds the divisor left-shifted 6 bits (6 fractional
 * bits), valid range 64..65535.
 *
 * At 20 MHz: 115200 -> 694 (true rate 115 274, +0.06%); 57600 -> 1389
 * (57 596, -0.006%). Normal mode covers everything up to f_CLK_PER/16 =
 * 1.25 Mbaud; CLK2X is selected automatically only past that, buying one
 * more octave (Table 25-1 conditions column). The fractional divisor is
 * why U2X is NOT needed at common rates on this part, unlike classic
 * megaAVR UBRR math. */
#define UART_BAUD_NORMAL \
    ((64ULL * UPDATER_F_CLK_PER + 8ULL * UPDATER_UART_BAUD) \
     / (16ULL * UPDATER_UART_BAUD))
#define UART_BAUD_CLK2X \
    ((64ULL * UPDATER_F_CLK_PER + 4ULL * UPDATER_UART_BAUD) \
     / (8ULL * UPDATER_UART_BAUD))

#if UART_BAUD_NORMAL >= 64
#  define UART_BAUD_REG    UART_BAUD_NORMAL
#  define UART_RXMODE_GC   USART_RXMODE_NORMAL_gc
#else
#  define UART_BAUD_REG    UART_BAUD_CLK2X
#  define UART_RXMODE_GC   USART_RXMODE_CLK2X_gc
_Static_assert(UART_BAUD_REG >= 64,
               "UPDATER_UART_BAUD too fast even for CLK2X at this clock");
#endif
_Static_assert(UART_BAUD_REG <= 65535,
               "UPDATER_UART_BAUD too slow for the 16-bit BAUD register");

void uart_init(void)
{
    /* USART0 default pin route: TXD = PA0, RXD = PA1 (PORTMUX.USARTROUTEA
     * resets to 0x00 = USART0 DEFAULT, section 17.3.3; hdr
     * PORTMUX_USART0_DEFAULT_gc). RXD stays a plain input (PORT reset
     * state). Init order per section 25.3.1: BAUD, CTRLC, TXD pin as
     * output, then enable — OUT is set high before DIR so the host never
     * sees a low glitch that could read as a start bit. */
    PORTA.OUTSET = PIN0_bm;
    PORTA.DIRSET = PIN0_bm;

    USART0.BAUD  = (uint16_t)UART_BAUD_REG;
    USART0.CTRLC = USART_CMODE_ASYNCHRONOUS_gc | USART_PMODE_DISABLED_gc
                 | USART_SBMODE_1BIT_gc | USART_CHSIZE_8BIT_gc;  /* 8N1 */
    USART0.CTRLA = 0;   /* reset value, restated: all interrupt enables off,
                           flags are polled (25.5.6) */
    USART0.CTRLB = USART_RXEN_bm | USART_TXEN_bm | UART_RXMODE_GC;
}

/* ---- uart_pump register ops (contract in ../skeletons/uart_pump.h) ---- */

static bool u0_rx_ready(void *ctx)
{
    (void)ctx;
    /* RXCIF: unread data in the receive buffer; clears itself when the
     * buffer is emptied by reading RXDATAL (25.5.5). RXDATAH error flags
     * (FERR/BUFOVF/PERR) are deliberately never read: a damaged or lost
     * byte fails the frame CRC and the host's timeout+retry recovers —
     * the same policy as every stream pump (uart_pump.h, link.h). */
    return (USART0.STATUS & USART_RXCIF_bm) != 0u;
}

static uint8_t u0_rx_read(void *ctx)
{
    (void)ctx;
    return USART0.RXDATAL;      /* pops the 2-level RX buffer (25.5.1) */
}

static bool u0_tx_ready(void *ctx)
{
    (void)ctx;
    /* DREIF: TXDATA is free; hardware guarantees it comes true within one
     * byte time, which is what makes the pump's blocking wait safe
     * (25.5.5). */
    return (USART0.STATUS & USART_DREIF_bm) != 0u;
}

static void u0_tx_write(void *ctx, uint8_t b)
{
    (void)ctx;
    /* TXCIF is sticky (W1C, 25.5.5) and would satisfy uart_tx_drain with
     * a stale completion from an earlier response; clearing it on every
     * byte load means "TXCIF set" always refers to the bytes written
     * since, i.e. the response being drained. STATUS's other writable
     * bits are W1C flags or WFB (write-1-only), so writing just TXCIF's
     * position disturbs nothing. */
    USART0.STATUS  = USART_TXCIF_bm;
    USART0.TXDATAL = b;         /* 25.5.3 */
}

const uart_pump_ops_t uart0_pump_ops = {
    u0_rx_ready, u0_rx_read, u0_tx_ready, u0_tx_write
};

void uart_tx_drain(void)
{
    /* TXCIF: entire frame shifted out AND no new data buffered (25.5.5).
     * Unlike the TWI client — which cannot push and must wait for the
     * host to read (twi_response_consumed) — a UART pushes; "response
     * consumed" simply means the shifter finished. Gates the BOOT jump so
     * disabling USART0 cannot truncate the reply's last byte. */
    while ((USART0.STATUS & USART_TXCIF_bm) == 0u) { }
}
