// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Tests for the embedded-hal I2C transport against a scripted fake bus.
//!
//! The fake implements `embedded_hal::i2c::I2c` the way the device behaves
//! on a real bus: reads are fixed-length (the master picks the size), the
//! response frame comes first and the tail is `0xFF` idle filler, and the
//! device NACKs while it is busy flashing.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{self, ErrorType, I2c, Operation};
use updater_core::frame::{self, CMD_ECHO, RSP_FLAG, ST_OK};
use updater_core::{Session, Transport};
use updater_eh::{I2cTransport, I2cTransportError};

const ADDR: u8 = 0x20;

// ---------------------------------------------------------------------------
// fake bus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeBusError(i2c::ErrorKind);

impl i2c::Error for FakeBusError {
    fn kind(&self) -> i2c::ErrorKind {
        self.0
    }
}

const NACK: FakeBusError = FakeBusError(i2c::ErrorKind::NoAcknowledge(
    i2c::NoAcknowledgeSource::Address,
));

enum Step {
    /// Expect a write of exactly these bytes; ACK it.
    Write(Vec<u8>),
    /// Expect a write; NACK it.
    WriteNack,
    /// Expect a read; fill with these bytes then 0xFF filler.
    Read(Vec<u8>),
    /// Expect a read; NACK it (device busy).
    ReadNack,
}

struct FakeI2c {
    script: VecDeque<Step>,
}

impl FakeI2c {
    fn new(script: Vec<Step>) -> Self {
        Self { script: script.into() }
    }

    fn done(&self) {
        assert!(self.script.is_empty(), "{} scripted step(s) never happened", self.script.len());
    }
}

impl ErrorType for FakeI2c {
    type Error = FakeBusError;
}

impl I2c for FakeI2c {
    fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), FakeBusError> {
        assert_eq!(address, ADDR, "wrong I2C address");
        for op in operations {
            let step = self.script.pop_front().expect("unexpected bus transaction");
            match (op, step) {
                (Operation::Write(bytes), Step::Write(expect)) => {
                    assert_eq!(*bytes, &expect[..], "wire bytes mismatch");
                }
                (Operation::Write(_), Step::WriteNack) => return Err(NACK),
                (Operation::Read(buf), Step::Read(data)) => {
                    assert!(data.len() <= buf.len(), "scripted response exceeds master read");
                    buf.fill(0xFF); // idle filler after the frame
                    buf[..data.len()].copy_from_slice(&data);
                }
                (Operation::Read(_), Step::ReadNack) => return Err(NACK),
                (op, _) => panic!("operation out of script order: {op:?}"),
            }
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
struct CountingDelay {
    calls: Rc<Cell<u32>>,
    total_ns: Rc<Cell<u64>>,
}

impl DelayNs for CountingDelay {
    fn delay_ns(&mut self, ns: u32) {
        self.calls.set(self.calls.get() + 1);
        self.total_ns.set(self.total_ns.get() + u64::from(ns));
    }
}

fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn request_writes_then_reads_full_buffer_with_filler() {
    let req = enc(CMD_ECHO, &[0x42]);
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x42]);
    let bus = FakeI2c::new(vec![Step::Write(req.clone()), Step::Read(rsp.clone())]);
    let mut t = I2cTransport::new(bus, CountingDelay::default(), ADDR);

    let mut rx = [0u8; 12];
    let n = t.request(&req, &mut rx).unwrap();
    assert_eq!(n, 12, "fixed-length read returns the whole buffer");
    assert_eq!(&rx[..rsp.len()], &rsp[..]);
    assert!(rx[rsp.len()..].iter().all(|&b| b == 0xFF));
    // decode_padded is the session's contract for exactly this shape.
    let f = frame::decode_padded(&rx[..n]).unwrap();
    assert_eq!(f.payload, &[ST_OK, 0x42]);
    t.release().0.done();
}

#[test]
fn request_polls_through_busy_nacks() {
    let req = enc(CMD_ECHO, &[]);
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK]);
    let bus = FakeI2c::new(vec![
        Step::Write(req.clone()),
        Step::ReadNack,
        Step::ReadNack,
        Step::Read(rsp),
    ]);
    let delay = CountingDelay::default();
    let calls = delay.calls.clone();
    let total = delay.total_ns.clone();
    let mut t = I2cTransport::new(bus, delay, ADDR).with_poll(10, 1_000_000);

    let mut rx = [0u8; 8];
    t.request(&req, &mut rx).unwrap();
    assert_eq!(calls.get(), 2, "one delay per NACKed poll");
    assert_eq!(total.get(), 2_000_000);
    t.release().0.done();
}

#[test]
fn request_exhausts_poll_budget_with_typed_error() {
    let req = enc(CMD_ECHO, &[]);
    let bus = FakeI2c::new(vec![
        Step::Write(req.clone()),
        Step::ReadNack,
        Step::ReadNack,
        Step::ReadNack,
    ]);
    let delay = CountingDelay::default();
    let calls = delay.calls.clone();
    let mut t = I2cTransport::new(bus, delay, ADDR).with_poll(3, 1_000);

    let mut rx = [0u8; 8];
    match t.request(&req, &mut rx) {
        Err(I2cTransportError::Exhausted { attempts: 3, last: e }) => assert_eq!(e, NACK),
        other => panic!("expected Exhausted, got {other:?}"),
    }
    // No delay after the final failed attempt.
    assert_eq!(calls.get(), 2);
    t.release().0.done();
}

#[test]
fn write_nack_is_reported_as_write_error() {
    let req = enc(CMD_ECHO, &[]);
    let bus = FakeI2c::new(vec![Step::WriteNack]);
    let mut t = I2cTransport::new(bus, CountingDelay::default(), ADDR);

    let mut rx = [0u8; 8];
    match t.request(&req, &mut rx) {
        Err(I2cTransportError::Write(e)) => assert_eq!(e, NACK),
        other => panic!("expected Write, got {other:?}"),
    }
    t.release().0.done();
}

#[test]
fn errors_render_human_readable() {
    let write = I2cTransportError::Write(NACK);
    let exhausted = I2cTransportError::Exhausted { attempts: 7, last: NACK };
    assert!(!format!("{write}").is_empty());
    let s = format!("{exhausted}");
    assert!(s.contains('7'), "attempt count surfaces in the message: {s}");
}

#[test]
fn session_echo_runs_end_to_end_over_the_adapter() {
    let req = enc(CMD_ECHO, &[0xA5, 0x5A]);
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0xA5, 0x5A]);
    let bus = FakeI2c::new(vec![Step::Write(req), Step::ReadNack, Step::Read(rsp)]);
    let t = I2cTransport::new(bus, CountingDelay::default(), ADDR).with_poll(5, 1_000);

    let mut frame_buf = [0u8; 32];
    let mut session = Session::new(t, &mut frame_buf);
    session.echo(&[0xA5, 0x5A]).unwrap();
    session.into_transport().release().0.done();
}
