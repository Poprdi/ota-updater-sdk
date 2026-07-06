#ifndef UPDATER_PORT_H
#define UPDATER_PORT_H
/* Port contract — the ONLY thing a port implements. The core calls the
 * flash functions only with page < app_pages (proven). The core never
 * addresses the boot region: the port exposes the app region only —
 * confinement is architectural AND proven.
 *
 * Vocabulary — "protocol page": the unit WRITE_PAGE transfers and the unit
 * the erase/write functions below take. page_size is the PROTOCOL page
 * size; it does not have to equal the physical flash page or sector size.
 * Cap: page_size <= 250. Derivation: a frame's LEN field is a u8, so a
 * payload holds at most UPD_LEN_MAX = 252 bytes (proto.h), and a
 * WRITE_PAGE payload is page_size + 2 (index LE + one page), hence
 * page_size + 2 <= 252. A port whose flash programs in larger or smaller
 * physical units maps protocol pages onto it internally — see
 * ports/rp2350_uart/flash.c, which coalesces two 128-byte protocol pages
 * into one 256-byte flash program and 32 of them into one 4 KiB sector
 * erase, as the worked example.
 *
 * Execution model: every function below is called from ONE polled
 * context — the bootloader main loop. Nothing here is called from an ISR,
 * and no implementation may depend on interrupts being enabled.
 *
 * Two kinds of function live in this contract:
 *   - called by the CORE (from inside upd_handle / upd_boot_if_valid,
 *     while your main loop runs them): port_info,
 *     port_flash_erase_page, port_flash_write_page,
 *     port_flash_read_byte, port_jump_to_app;
 *   - called by YOUR MAIN LOOP (the core never does): port_recv,
 *     port_send, port_ticks_ms, port_entry_requested.
 *
 * The PORT owns main() and every policy decision the core deliberately
 * does not make:
 *   - T_ENTRY autoboot: while no valid frame has arrived, once
 *     port_ticks_ms() passes the port's UPDATER_T_ENTRY_MS, attempt
 *     upd_boot_if_valid;
 *   - rescue residency: if that attempt refuses (no valid app), latch
 *     resident so the device stays reachable for rescue flashing;
 *   - BOOT sequencing: send the reply, wait until it has fully left the
 *     wire, THEN call upd_boot_if_valid.
 * The full obligation list is at the top of update.h; the pinned
 * reference loop is conformance/sim/sim_port.c (the loop the conformance
 * campaigns exercise), with ports/avr_ea_twi/main.c and
 * ports/avr_ea_uart/main.c as on-target twins.
 *
 * Transactional vs stream transports: port_recv/port_send are the
 * TRANSACTIONAL receive/send pair — implement them when the wire delivers
 * whole frames (e.g. the TWI port, whose bus state machine assembles a
 * frame per bus transaction). Byte-STREAM transports (UART, SPI, softuart)
 * do not implement them meaningfully: their main loop pumps bytes through
 * a link_t instead (link.h: link_poll / link_send over a get_byte/put_byte
 * pair) and port_recv simply returns false / port_send is a no-op if the
 * symbols must exist. A port uses one path or the other, never both. */
#include <stdbool.h>
#include <stdint.h>

typedef struct {
    uint16_t page_size;     /* PROTOCOL page size, bytes; <= 250 (above)   */
    uint16_t app_pages;     /* app region length in protocol pages         */
    uint8_t  device_id[4];  /* project-assigned, reported in INFO          */
    uint8_t  bl_version;
} port_info_t;

/* Called by the core (via upd_init). Fill *out with the port's geometry
 * and identity. */
void     port_info(port_info_t *out);

/* Called by the core (ERASE_APP), once per protocol page, ascending,
 * page < app_pages (proven). Ports whose erase unit spans several protocol
 * pages act on the first page of each unit and no-op the rest
 * (rp2350_uart/flash.c). */
void     port_flash_erase_page(uint16_t page);

/* Called by the core (WRITE_PAGE), page < app_pages (proven); data has
 * exactly page_size readable bytes. Called only after a full ERASE_APP
 * completed in THIS session — the core refuses earlier writes with
 * ST_NOT_ERASED — so the implementation may assume the target page is in
 * the erased state (modulo the repeat below).
 *
 * Repeat-write tolerance (REQUIRED): the host retries idempotently — when
 * a reply is lost it re-sends the identical request, so this function can
 * be called AGAIN for a page already written, with the SAME data, and
 * must behave as a no-op. On NOR flash re-programming identical data is
 * harmless (programming only clears bits). WARNING for ECC/write-once
 * flashes where re-programming a programmed page is illegal: the port
 * must absorb the repeat itself (compare-and-skip, or buffer like the
 * rp2350 holdback) — it must NOT fault. Re-audit
 * rp2350_uart/PORT_AUDIT.md F3 before reusing that port's flash layer on
 * such a part. */
void     port_flash_write_page(uint16_t page, const uint8_t *data);

/* Called by the core (VERIFY, INFO's app_valid, the boot gate);
 * offset < page_size * app_pages (proven). Must return what a subsequent
 * boot would see — flush any write-coalescing buffer first
 * (rp2350_uart/flash.c). */
uint8_t  port_flash_read_byte(uint32_t offset);

/* Called by your main loop (transactional transports only; stream ports
 * return false — see the header comment). True with the frame in buf and
 * its length in *len when one whole frame has arrived; false when nothing
 * (yet). Never blocks.
 *
 * RX capacity contract: the port accepts frames with LEN up to
 * page_size + 8 and drops longer ones at the wire — buffer
 * page_size + 8 + UPD_FRAME_OVERHEAD bytes (136 total for 128-byte
 * pages). The protocol-wide maxima live in proto.h (UPD_LEN_MAX 252,
 * UPD_FRAME_MAX 255); they bound the CODEC, not your buffer — size from
 * the port geometry, never from the 252/255 ceiling. */
bool     port_recv(uint8_t *buf, uint8_t *len);

/* Called by your main loop (transactional transports only; stream ports
 * use link_send instead). Queue/emit one whole response frame (at most UPD_RSP_MAX + UPD_FRAME_OVERHEAD = 20 bytes — see proto.h). */
void     port_send(const uint8_t *buf, uint8_t len);

/* Called by your main loop (the core never reads time). Free-running
 * milliseconds since reset; wraps every 65536 ms and that is fine — it
 * gates ONLY the T_ENTRY entry window (a one-shot decision taken long
 * before the first wrap); nothing else in the system times off it. */
uint16_t port_ticks_ms(void);

/* Called by your main loop, once at startup: did the application request
 * bootloader entry before soft-resetting? Mechanism (magic +
 * one's-complement pair in reset-surviving storage) and per-target
 * details: app_stub.h.
 *
 * Contract: fires ONCE — the implementation must CLEAR the pair so the
 * next reset is a normal one. The pair survives a soft reset but NOT
 * power-on (the complement check rejects POR garbage). Capture may have
 * to beat the C runtime: on AVR-EA the pair lives under the initial
 * stack, so entry.c snapshots it in .init3, before the first CALL can
 * push over it — a new port must check for the same hazard. */
bool     port_entry_requested(void);

/* Called by the core, from exactly one site: upd_boot_if_valid, after the
 * image validated. Transfer control to the app; never returns. */
void     port_jump_to_app(void);

#endif
