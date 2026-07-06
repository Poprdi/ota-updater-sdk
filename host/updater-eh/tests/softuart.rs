//! Tests for the bit-banged softuart transport over fake pins and a
//! virtual clock.
//!
//! The fake `DelayNs` advances a shared virtual time; the RX pin computes
//! its level from a scheduled waveform and the TX pin records every
//! transition with its timestamp. An 8N1 decoder over the TX record then
//! verifies the adapter's line discipline (start bit, LSB-first data,
//! stop bit, ~104 us bit time) without any real-time sleeping.

use std::cell::RefCell;
use std::rc::Rc;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{self, ErrorType, InputPin, OutputPin};
use updater_core::frame::{self, CMD_ECHO, RSP_FLAG, ST_OK};
use updater_core::stream::SYNC;
use updater_core::{Session, Transport};
use updater_eh::{SoftUartTransport, SoftUartTransportError, SOFTUART_BIT_NS};

const BIT: u64 = SOFTUART_BIT_NS as u64;

// ---------------------------------------------------------------------------
// virtual world: one clock, two pins, one waveform
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakePinError;

impl digital::Error for FakePinError {
    fn kind(&self) -> digital::ErrorKind {
        digital::ErrorKind::Other
    }
}

#[derive(Default)]
struct World {
    now_ns: u64,
    /// TX transitions as (time, level).
    tx_events: Vec<(u64, bool)>,
    /// Low intervals [start, end) on RX; the line is high elsewhere.
    rx_lows: Vec<(u64, u64)>,
    tx_fails: bool,
    rx_fails: bool,
}

impl World {
    fn rx_level(&self) -> bool {
        !self.rx_lows.iter().any(|&(s, e)| self.now_ns >= s && self.now_ns < e)
    }

    /// Schedule one 8N1 byte starting (start bit falling edge) at `t`;
    /// returns the time the stop bit ends.
    fn schedule_byte(&mut self, t: u64, b: u8) -> u64 {
        self.rx_lows.push((t, t + BIT)); // start bit
        for i in 0..8u32 {
            if (b >> i) & 1 == 0 {
                let lo = t + u64::from(i + 1) * BIT;
                self.rx_lows.push((lo, lo + BIT));
            }
        }
        t + 10 * BIT // 1 start + 8 data + 1 stop
    }

    /// Schedule bytes back-to-back (device `put_byte` leaves no gap).
    fn schedule_stream(&mut self, mut t: u64, bytes: &[u8]) -> u64 {
        for &b in bytes {
            t = self.schedule_byte(t, b);
        }
        t
    }
}

#[derive(Clone)]
struct Shared(Rc<RefCell<World>>);

struct TxPin(Rc<RefCell<World>>);
struct RxPin(Rc<RefCell<World>>);
struct Clock(Rc<RefCell<World>>);

impl ErrorType for TxPin {
    type Error = FakePinError;
}

impl OutputPin for TxPin {
    fn set_low(&mut self) -> Result<(), FakePinError> {
        let mut w = self.0.borrow_mut();
        if w.tx_fails {
            return Err(FakePinError);
        }
        let now = w.now_ns;
        w.tx_events.push((now, false));
        Ok(())
    }

    fn set_high(&mut self) -> Result<(), FakePinError> {
        let mut w = self.0.borrow_mut();
        if w.tx_fails {
            return Err(FakePinError);
        }
        let now = w.now_ns;
        w.tx_events.push((now, true));
        Ok(())
    }
}

impl ErrorType for RxPin {
    type Error = FakePinError;
}

impl InputPin for RxPin {
    fn is_high(&mut self) -> Result<bool, FakePinError> {
        let w = self.0.borrow();
        if w.rx_fails {
            return Err(FakePinError);
        }
        Ok(w.rx_level())
    }

    fn is_low(&mut self) -> Result<bool, FakePinError> {
        self.is_high().map(|h| !h)
    }
}

impl DelayNs for Clock {
    fn delay_ns(&mut self, ns: u32) {
        self.0.borrow_mut().now_ns += u64::from(ns);
    }
}

fn world() -> (Shared, TxPin, RxPin, Clock) {
    let w = Rc::new(RefCell::new(World::default()));
    (Shared(w.clone()), TxPin(w.clone()), RxPin(w.clone()), Clock(w))
}

/// TX level at time `t` from the recorded transitions (idle-high before
/// the first event).
fn tx_level_at(events: &[(u64, bool)], t: u64) -> bool {
    events.iter().take_while(|&&(et, _)| et <= t).last().map_or(true, |&(_, l)| l)
}

/// Decode the recorded TX waveform as 8N1, asserting clean stop bits.
fn decode_tx(events: &[(u64, bool)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut cursor = 0u64;
    loop {
        // Next falling edge at or after cursor.
        let Some(&(edge, _)) = events
            .iter()
            .find(|&&(et, l)| !l && et >= cursor && tx_level_at(events, et.saturating_sub(1)))
        else {
            return bytes;
        };
        let mut b = 0u8;
        for i in 0..8u32 {
            let sample = edge + 3 * BIT / 2 + u64::from(i) * BIT;
            if tx_level_at(events, sample) {
                b |= 1 << i;
            }
        }
        let stop = edge + 19 * BIT / 2;
        assert!(tx_level_at(events, stop), "stop bit must be high (mark)");
        bytes.push(b);
        cursor = edge + 10 * BIT;
    }
}

fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

fn stream(frame_bytes: &[u8]) -> Vec<u8> {
    let mut s = vec![SYNC];
    s.extend_from_slice(frame_bytes);
    s
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn request_is_transmitted_as_8n1_sync_plus_frame() {
    let (shared, tx, rx, clock) = world();
    let mut t = SoftUartTransport::new(tx, rx, clock).with_poll(8, 1_000);

    let req = enc(CMD_ECHO, &[0xC4]);
    let mut rsp_buf = [0u8; 16];
    // No response scheduled: the request must still leave the wire, then
    // the poll budget runs out.
    match t.request(&req, &mut rsp_buf) {
        Err(SoftUartTransportError::Exhausted { attempts: 8 }) => {}
        other => panic!("expected Exhausted, got {other:?}"),
    }

    let w = shared.0.borrow();
    assert_eq!(decode_tx(&w.tx_events), stream(&req), "TX waveform must be 0x7E + frame");
}

#[test]
fn response_with_pre_sync_garbage_is_received() {
    let (shared, tx, rx, clock) = world();
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0xC4]);
    {
        // Garbage byte, a gap, then the frame back-to-back — scheduled
        // well after the ~5 ms the request needs on the wire.
        let mut w = shared.0.borrow_mut();
        let after_garbage = w.schedule_byte(20_000_000, 0xB7);
        let wire = stream(&rsp);
        w.schedule_stream(after_garbage + 4 * BIT, &wire);
    }
    // Sample every 2 us while idle; budget generous for the virtual clock.
    let mut t = SoftUartTransport::new(tx, rx, clock).with_poll(1_000_000, 2_000);

    let req = enc(CMD_ECHO, &[0xC4]);
    let mut rx_buf = [0u8; 16];
    let n = t.request(&req, &mut rx_buf).unwrap();
    assert_eq!(&rx_buf[..n], &rsp[..]);
}

#[test]
fn break_condition_is_dropped_and_recovered_from() {
    let (shared, tx, rx, clock) = world();
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK]);
    {
        let mut w = shared.0.borrow_mut();
        // A 15-bit break: reads as a byte with a low stop bit (framing
        // error) and a still-low line afterwards — must be dropped and
        // must not cascade into the real frame that follows.
        w.rx_lows.push((20_000_000, 20_000_000 + 15 * BIT));
        w.schedule_stream(20_000_000 + 20 * BIT, &stream(&rsp));
    }
    let mut t = SoftUartTransport::new(tx, rx, clock).with_poll(1_000_000, 2_000);

    let req = enc(CMD_ECHO, &[]);
    let mut rx_buf = [0u8; 16];
    let n = t.request(&req, &mut rx_buf).unwrap();
    assert_eq!(&rx_buf[..n], &rsp[..]);
}

#[test]
fn silence_exhausts_the_poll_budget_typed() {
    let (_shared, tx, rx, clock) = world();
    let mut t = SoftUartTransport::new(tx, rx, clock).with_poll(5, 1_000);

    let mut rx_buf = [0u8; 16];
    match t.request(&enc(CMD_ECHO, &[]), &mut rx_buf) {
        Err(SoftUartTransportError::Exhausted { attempts: 5 }) => {}
        other => panic!("expected Exhausted, got {other:?}"),
    }
}

#[test]
fn pin_errors_are_typed() {
    let (shared, tx, rx, clock) = world();
    shared.0.borrow_mut().tx_fails = true;
    let mut t = SoftUartTransport::new(tx, rx, clock);
    let mut rx_buf = [0u8; 16];
    match t.request(&enc(CMD_ECHO, &[]), &mut rx_buf) {
        Err(SoftUartTransportError::Tx(FakePinError)) => {}
        other => panic!("expected Tx, got {other:?}"),
    }

    let (shared, tx, rx, clock) = world();
    shared.0.borrow_mut().rx_fails = true;
    let mut t = SoftUartTransport::new(tx, rx, clock).with_poll(4, 1_000);
    match t.request(&enc(CMD_ECHO, &[]), &mut rx_buf) {
        Err(SoftUartTransportError::Rx(FakePinError)) => {}
        other => panic!("expected Rx, got {other:?}"),
    }
}

#[test]
fn errors_render_human_readable() {
    let e: SoftUartTransportError<FakePinError, FakePinError> =
        SoftUartTransportError::Exhausted { attempts: 3 };
    assert!(format!("{e}").contains('3'));
    let e: SoftUartTransportError<FakePinError, FakePinError> =
        SoftUartTransportError::Tx(FakePinError);
    assert!(!format!("{e}").is_empty());
}

#[test]
fn session_echo_runs_end_to_end_over_the_adapter() {
    let (shared, tx, rx, clock) = world();
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x3C]);
    shared.0.borrow_mut().schedule_stream(30_000_000, &stream(&rsp));
    let t = SoftUartTransport::new(tx, rx, clock).with_poll(1_000_000, 2_000);

    let mut frame_buf = [0u8; 64];
    let mut session = Session::new(t, &mut frame_buf);
    session.echo(&[0x3C]).unwrap();
    let (_tx, _rx, _clock) = session.into_transport().release();
    let w = shared.0.borrow();
    assert_eq!(decode_tx(&w.tx_events), stream(&enc(CMD_ECHO, &[0x3C])));
}
