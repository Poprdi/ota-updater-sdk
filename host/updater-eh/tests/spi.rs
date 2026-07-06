// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Tests for the SpiDevice transport against a scripted fake bus.
//!
//! The fake models the device-side `spi_pump` contract: the slave shifts
//! out `0x00` while idle or busy, the response appears as `0x7E` + frame,
//! and — the one-byte lag — at least one stale byte (whatever sat in the
//! shift register) precedes the sync. The host must scan MISO for the
//! sync, clocking idle bytes to poll.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use embedded_hal::delay::DelayNs;
use embedded_hal::spi::{self, ErrorType, Operation, SpiDevice};
use updater_core::frame::{self, CMD_ECHO, CMD_INFO, RSP_FLAG, ST_OK};
use updater_core::stream::SYNC;
use updater_core::{Session, Transport};
use updater_eh::{SpiTransport, SpiTransportError};

// ---------------------------------------------------------------------------
// fake SPI device
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeSpiError(spi::ErrorKind);

impl spi::Error for FakeSpiError {
    fn kind(&self) -> spi::ErrorKind {
        self.0
    }
}

const SPI_ERR: FakeSpiError = FakeSpiError(spi::ErrorKind::Other);

#[derive(Default)]
struct FakeSpi {
    /// Bytes written, grouped per transaction (per CS assertion).
    written: Vec<Vec<u8>>,
    /// Scripted MISO bytes; an empty queue shifts 0x00 (idle slave).
    miso: VecDeque<u8>,
    /// Total bytes clocked in by reads.
    bytes_read: usize,
    /// When true, every transaction fails.
    bus_fails: bool,
}

impl FakeSpi {
    fn with_miso(miso: Vec<u8>) -> Self {
        Self { miso: miso.into(), ..Self::default() }
    }
}

impl ErrorType for FakeSpi {
    type Error = FakeSpiError;
}

impl SpiDevice for FakeSpi {
    fn transaction(
        &mut self,
        operations: &mut [Operation<'_, u8>],
    ) -> Result<(), FakeSpiError> {
        if self.bus_fails {
            return Err(SPI_ERR);
        }
        let mut written = Vec::new();
        for op in operations {
            match op {
                Operation::Write(bytes) => written.extend_from_slice(bytes),
                Operation::Read(buf) => {
                    for slot in buf.iter_mut() {
                        // Idle slave shifts 0x00 (the busy byte).
                        *slot = self.miso.pop_front().unwrap_or(0x00);
                        self.bytes_read += 1;
                    }
                }
                other => panic!("adapter used an unexpected SPI operation: {other:?}"),
            }
        }
        if !written.is_empty() {
            self.written.push(written);
        }
        Ok(())
    }
}

/// Records every delay with its duration so poll pacing and byte pacing
/// can be told apart.
#[derive(Clone, Default)]
struct RecordingDelay {
    ns: Rc<RefCell<Vec<u32>>>,
}

impl DelayNs for RecordingDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.ns.borrow_mut().push(ns);
    }
}

fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

/// MISO stream per the spi_pump contract: `lag` stale bytes, busy zeros,
/// then 0x7E + frame.
fn miso_stream(stale: &[u8], busy: usize, frame_bytes: &[u8]) -> Vec<u8> {
    let mut s = stale.to_vec();
    s.extend(std::iter::repeat(0x00).take(busy));
    s.push(SYNC);
    s.extend_from_slice(frame_bytes);
    s
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn request_writes_sync_plus_frame_in_one_transaction_and_scans_miso() {
    let req = enc(CMD_INFO, &[]);
    let rsp = enc(CMD_INFO | RSP_FLAG, &[ST_OK, 7, 7, 7]);
    // One stale lag byte (non-zero garbage!) + two busy bytes before sync.
    let bus = FakeSpi::with_miso(miso_stream(&[0xC3], 2, &rsp));
    let mut t = SpiTransport::new(bus, RecordingDelay::default());

    let mut rx = [0u8; 32];
    let n = t.request(&req, &mut rx).unwrap();
    assert_eq!(&rx[..n], &rsp[..]);

    let (bus, _) = t.release();
    let mut expect = vec![SYNC];
    expect.extend_from_slice(&req);
    assert_eq!(
        bus.written,
        vec![expect],
        "0x7E + request must leave in ONE transaction (one CS assertion)"
    );
}

#[test]
fn busy_zeros_use_poll_interval_frame_bytes_use_byte_pacing() {
    let req = enc(CMD_ECHO, &[]);
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK]);
    let bus = FakeSpi::with_miso(miso_stream(&[], 3, &rsp));
    let delay = RecordingDelay::default();
    let ns = delay.ns.clone();
    let mut t = SpiTransport::new(bus, delay).with_poll(100, 5_000_000).with_pacing(50_000);

    let mut rx = [0u8; 16];
    t.request(&req, &mut rx).unwrap();
    let recorded = ns.borrow();
    // 3 busy bytes -> 3 poll delays; sync + 3 in-frame bytes (all but the
    // last) -> pacing delays between exchanges.
    assert_eq!(recorded.iter().filter(|&&d| d == 5_000_000).count(), 3);
    assert!(recorded.contains(&50_000), "byte pacing must be applied");
}

#[test]
fn all_busy_exhausts_the_poll_budget_typed() {
    let req = enc(CMD_ECHO, &[]);
    let bus = FakeSpi::default(); // MISO is 0x00 forever
    let mut t = SpiTransport::new(bus, RecordingDelay::default()).with_poll(4, 1_000);

    let mut rx = [0u8; 16];
    match t.request(&req, &mut rx) {
        Err(SpiTransportError::Exhausted { attempts: 4 }) => {}
        other => panic!("expected Exhausted, got {other:?}"),
    }
    let (bus, _) = t.release();
    assert_eq!(bus.bytes_read, 4, "one byte clocked per poll attempt");
}

#[test]
fn corrupt_frame_dropped_then_retransmission_accepted() {
    let req = enc(CMD_ECHO, &[0x22]);
    let good = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x22]);
    let mut bad = good.clone();
    bad[0] ^= 0x40; // CRC will fail
    let mut miso = miso_stream(&[], 1, &bad);
    miso.extend_from_slice(&miso_stream(&[], 2, &good));
    let bus = FakeSpi::with_miso(miso);
    let mut t = SpiTransport::new(bus, RecordingDelay::default()).with_poll(32, 1_000);

    let mut rx = [0u8; 16];
    let n = t.request(&req, &mut rx).unwrap();
    assert_eq!(&rx[..n], &good[..]);
}

#[test]
fn bus_errors_are_typed_write_first() {
    let req = enc(CMD_ECHO, &[]);
    let bus = FakeSpi { bus_fails: true, ..FakeSpi::default() };
    let mut t = SpiTransport::new(bus, RecordingDelay::default());

    let mut rx = [0u8; 16];
    match t.request(&req, &mut rx) {
        Err(SpiTransportError::Write(e)) => assert_eq!(e, SPI_ERR),
        other => panic!("expected Write, got {other:?}"),
    }
}

#[test]
fn errors_render_human_readable() {
    let e: SpiTransportError<FakeSpiError> = SpiTransportError::Exhausted { attempts: 12 };
    assert!(format!("{e}").contains("12"));
    let e: SpiTransportError<FakeSpiError> = SpiTransportError::Read(SPI_ERR);
    assert!(!format!("{e}").is_empty());
}

#[test]
fn session_echo_runs_end_to_end_over_the_adapter() {
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x5A]);
    let bus = FakeSpi::with_miso(miso_stream(&[0xEE], 5, &rsp));
    let t = SpiTransport::new(bus, RecordingDelay::default()).with_poll(50, 1_000);

    let mut frame_buf = [0u8; 64];
    let mut session = Session::new(t, &mut frame_buf);
    session.echo(&[0x5A]).unwrap();
}
