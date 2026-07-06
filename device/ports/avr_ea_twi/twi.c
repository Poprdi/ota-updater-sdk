/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher
 *
 * Polled TWI0 client — DS40002443A section 27 (client operation 27.3.2.3).
 * Zero protocol knowledge: this file moves bytes between the bus and two
 * buffers; framing/CRC/commands live entirely in the core.
 *
 * No interrupts: SREG.I stays 0 for the bootloader's whole life, so the
 * DIEN/APIEN enables are irrelevant (interrupt = flag AND enable AND I,
 * 27.5.10); the SSTATUS flags themselves are set by hardware regardless
 * and are polled here. PIEN is the one enable that matters even when
 * polling: without it a Stop condition never sets APIF (27.5.10, APIEN
 * note 2). While APIF/DIF are pending the client holds SCL low
 * (SSTATUS.CLKHOLD, 27.5.12) — that hardware stretch between our poll
 * iterations is the spec's "device clock-stretches until the response is
 * ready" mechanism; no timing logic is needed here.
 *
 * SSTATUS flag semantics (APIF/DIF/AP/DIR/RXACK, first-tx-byte-on-DIF
 * quirk, BUSERR teardown) mirror the proven interrupt-mode client in
 * ir_sensor_board/src/twi_slave.c, reshaped for polling. */
#include <avr/io.h>
#include <stdbool.h>
#include <stdint.h>
#include "port_cfg.h"
#include "updater/port.h"

static uint8_t rx_buf[UPDATER_RX_BUF_SIZE];
static uint8_t rx_len;          /* bytes collected in the current write txn  */
static uint8_t rx_frame;        /* length of a parked, complete frame        */
static bool    rx_over;         /* frame exceeded the buffer: drop at wire   */
static bool    rx_done;         /* complete frame parked, main not yet told  */
static bool    in_write;        /* inside a host-write transaction           */
static bool    awaiting;        /* frame handed to main, response not armed  */

static uint8_t tx_buf[UPDATER_TX_BUF_SIZE];
static uint8_t tx_len;          /* 0 = nothing armed (reads get 0xFF filler) */
static uint8_t tx_idx;
static bool    tx_first;        /* EA quirk: SDATA written during the APIF
                                   (address) phase is not shifted out; the
                                   first byte must be loaded on the first
                                   DIF (see ir_sensor_board twi_slave.c)    */
static bool    tx_active;       /* inside a host-read transaction            */
static bool    tx_consumed;     /* armed response fully streamed once        */

void twi_init(void)
{
    /* Client address in SADDR[7:1]; bit 0 = general-call recognition, off
     * (27.5.13). SDA/SCL are TWI0's default PA2/PA3 route (PORTMUX reset,
     * section 17); the bus has external pull-ups — internal ones stay off
     * (same electrical setup as the ir_sensor_board client). */
    TWI0.SADDR   = (uint8_t)(UPDATER_TWI_ADDR << 1);
    TWI0.SSTATUS = TWI_DIF_bm | TWI_APIF_bm | TWI_BUSERR_bm | TWI_COLL_bm; /* W1C stale flags */
    TWI0.SCTRLA  = TWI_PIEN_bm | TWI_ENABLE_bm;
}

static void end_read_txn(void)
{
    tx_active = false;
    tx_first  = false;
    if (tx_len != 0u && tx_idx >= tx_len)
        tx_consumed = true;
}

/* One poll step: services at most one pending SSTATUS condition. Called
 * from port_recv, i.e. once per main-loop lap. */
static void twi_poll(void)
{
    uint8_t st = TWI0.SSTATUS;

    if (st & (TWI_BUSERR_bm | TWI_COLL_bm)) {
        /* Hardware has already aborted the transaction. Clear the flags
         * (W1C, 27.5.12) and drop half-latched state; a parked complete
         * frame or armed response from an earlier good transaction stays
         * valid. Mirrors the ir_sensor_board teardown. */
        TWI0.SSTATUS = TWI_BUSERR_bm | TWI_COLL_bm;
        rx_len   = 0u;
        rx_over  = false;
        in_write = false;
        tx_active = false;
        tx_first  = false;
        return;
    }

    if (st & TWI_APIF_bm) {
        if (st & TWI_AP_bm) {                    /* address match (27.3.2.3.1) */
            if (in_write) {
                /* repeated start ends the write segment: park the frame
                 * (hosts may use write-Sr-read instead of write-Stop-read) */
                in_write = false;
                if (rx_len != 0u && !rx_over) {
                    rx_done  = true;
                    rx_frame = rx_len;
                }
                rx_len  = 0u;
                rx_over = false;
            }
            if (rx_done || awaiting) {
                /* Single rx buffer parked, or response not yet computed:
                 * leave APIF pending WITHOUT an SCMD — the client keeps
                 * SCL stretched (CLKHOLD) until main catches up. */
                return;
            }
            if (st & TWI_DIR_bm) {               /* host read */
                tx_first  = true;
                tx_idx    = 0u;                  /* re-reads restart the response:
                                                    idempotent for host retries */
                tx_active = true;
            } else {                             /* host write */
                in_write = true;
                rx_len   = 0u;
                rx_over  = false;
                tx_len   = 0u;                   /* new request invalidates any
                                                    stale response */
            }
            TWI0.SCTRLB = TWI_SCMD_RESPONSE_gc;  /* ACK the address (27.5.11) */
        } else {                                 /* Stop condition */
            if (tx_active)
                end_read_txn();
            if (in_write) {
                in_write = false;
                if (rx_len != 0u && !rx_over) {  /* whole frame or nothing */
                    rx_done  = true;
                    rx_frame = rx_len;
                }
                rx_len  = 0u;
                rx_over = false;
            }
            TWI0.SCTRLB = TWI_SCMD_COMPTRANS_gc; /* complete txn, clears APIF (27.5.11) */
        }
        return;
    }

    if (st & TWI_DIF_bm) {
        if (st & TWI_DIR_bm) {                   /* client transmit (27.3.2.3.3) */
            if (!tx_first && (st & TWI_RXACK_bm)) {
                end_read_txn();                  /* host NACK = end of read */
                TWI0.SCTRLB = TWI_SCMD_COMPTRANS_gc;
                return;
            }
            tx_first = false;                    /* on the first DIF, RXACK is
                                                    stale — always send */
            /* 0xFF filler beyond the response: the host performs a fixed-
             * length padded read and discards the tail. */
            TWI0.SDATA  = (tx_idx < tx_len) ? tx_buf[tx_idx++] : 0xFFu;
            TWI0.SCTRLB = TWI_SCMD_RESPONSE_gc;
        } else {                                 /* client receive (27.3.2.3.2) */
            uint8_t d = TWI0.SDATA;              /* read clears DIF        */
            if (in_write) {
                if (rx_len < (uint8_t)sizeof rx_buf)
                    rx_buf[rx_len++] = d;
                else
                    rx_over = true;              /* oversized: swallow + drop the
                                                    frame at Stop; host times out
                                                    and retries (NACKing mid-write
                                                    trips some host adapters) */
            }
            TWI0.SCTRLB = TWI_SCMD_RESPONSE_gc;  /* ACK the byte */
        }
    }
}

bool port_recv(uint8_t *buf, uint8_t *len)
{
    twi_poll();
    if (!rx_done)
        return false;
    for (uint8_t i = 0; i < rx_frame; i++)       /* caller buffer is
                                                    UPDATER_RX_BUF_SIZE (main.c) */
        buf[i] = rx_buf[i];
    *len        = rx_frame;
    rx_done     = false;
    awaiting    = true;                          /* stretch any bus activity until
                                                    main arms the response */
    tx_len      = 0u;
    tx_consumed = false;
    return true;
}

void port_send(const uint8_t *buf, uint8_t len)
{
    if (len > (uint8_t)sizeof tx_buf)            /* cannot happen (main builds
                                                    <= TX_BUF); plain bound, not
                                                    protocol logic */
        len = (uint8_t)sizeof tx_buf;
    for (uint8_t i = 0; i < len; i++)
        tx_buf[i] = buf[i];
    tx_len      = len;
    tx_idx      = 0u;
    tx_consumed = false;
    awaiting    = false;                         /* releases the stretch on the
                                                    next poll */
}

bool twi_response_consumed(void)
{
    return tx_consumed;
}
