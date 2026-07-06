/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef SIM_PORT_H
#define SIM_PORT_H
/* Simulated device for the conformance harness: the real device core
 * (the five device core C files) over an in-memory flash. Shared by the Rust FFI layer
 * (conformance/src/lib.rs) and the sanitizer runner (conformance/casan).
 *
 * Dev-only. Static state throughout — callers must serialize access
 * (the Rust side holds a process-wide mutex; casan is single-threaded).
 */
#include <stdbool.h>
#include <stdint.h>

/* Geometry, matching the device test fixtures (device/test/fake_port.c). */
#define SIM_PAGE_SIZE 128u
#define SIM_APP_PAGES 32u
#define SIM_REGION    (SIM_PAGE_SIZE * SIM_APP_PAGES)   /* 4096 bytes */

/* Reboot: re-init the session (fresh, not erased, no boot pending), clear
 * the jumped/dead flags and any armed power cut, zero the op counter.
 * preserve_flash=false additionally wipes flash to 0xFF (factory-fresh);
 * preserve_flash=true keeps flash exactly as the "power loss" left it. */
void sim_reset(bool preserve_flash);

/* Arm a power cut on the Nth flash op (erase OR write) counted from this
 * call; 0 disarms. The cut lands MID-op — the torn op completes only its
 * first SIM_PAGE_SIZE/2 bytes (write: first half written, rest keeps prior
 * content, i.e. 0xFF on freshly erased flash; erase: first half erased,
 * rest keeps prior content) — modelling a real page tear. From that op on
 * the device is dead: further flash ops are no-ops and sim_request()
 * returns 0 (no response escapes a dead device) until sim_reset(). */
void sim_powercut_after(uint32_t flash_ops);

/* Did an armed cut fire since the last sim_reset()? */
bool sim_powercut_hit(void);

/* Flash ops (erases + writes) performed since the last sim_reset(). */
uint32_t sim_flash_ops(void);

/* One full request/response cycle: the exact parse -> handle -> build path
 * the AVR main loop runs. Returns the response length written to resp
 * (resp must hold >= 259 bytes), or 0 if the device is dead or has jumped
 * to the app. Unparseable input (bad CRC, LEN/buflen mismatch, truncation,
 * len > 255) is answered with a valid frame carrying ST_BAD_FRAME whose
 * CMD byte is the first received byte | 0x80 (0x80 alone if no byte
 * arrived) — reference main-loop behavior. A BOOT reply is emitted BEFORE
 * the jump, exactly like the real main loop (reply, then
 * upd_boot_if_valid).
 *
 * The sim accepts frames up to the protocol's LEN ceiling (255-byte
 * payload) because it exists to exercise the core's own bounds checks; a
 * real port only buffers what the largest legal command needs — page_size
 * + 8 (136 here) — and drops longer frames at the wire. What this harness
 * pins for oversized-but-parseable frames is therefore reference-CORE
 * behavior, not a wire guarantee: the AVR main.c author must size the RX
 * buffer from the port geometry, not copy the 255 cap. */
uint16_t sim_request(const uint8_t *frame, uint16_t len, uint8_t *resp);

/* Stream-path twin of sim_request: feeds the bytes through the REAL
 * link_stream.c (sync hunt, LEN-driven assembly), handles every complete
 * frame, and emits each reply through link_send (0x7E + frame) into resp.
 * Returns the number of response-stream bytes written; resp must hold
 * >= 512 bytes (worst case: two max replies never occur, but one sync +
 * 255-byte frame per handled request — size for the requests you batch,
 * or the sim aborts on overflow, contract-violation style).
 *
 * Stream semantics differ from sim_request on unparseable input: the link
 * drops garbage/CRC-corrupt bytes SILENTLY (no ST_BAD_FRAME reply) — loss
 * recovery belongs to the host's timeout+retry, per link.h. Link state
 * persists across calls (a frame may be torn across pumps) and resets on
 * sim_reset(). Dead or jumped devices return 0. */
uint16_t sim_request_stream(const uint8_t *bytes, uint16_t len, uint8_t *resp,
                            uint16_t cap);

/* Has the BOOT gate fired (port_jump_to_app reached) since sim_reset()? */
bool sim_jumped(void);

/* The raw app-region flash array, SIM_REGION bytes. Snapshot it, corrupt
 * it — it is the sim's live storage. */
uint8_t *sim_flash(void);

#endif
