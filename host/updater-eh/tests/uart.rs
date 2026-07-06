//! Tests for the embedded-io UART transport against a scripted fake port.
//!
//! The fake implements `embedded_io::{Read, Write}` the way serial HALs
//! behave: reads deliver whatever the wire has in arbitrary chunk sizes,
//! return `Ok(0)` when a HAL-level timeout elapsed with nothing received,
//! and the response stream is `0x7E` + frame with possible garbage around
//! it (line noise, a stale byte from a previous exchange).

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use embedded_hal::delay::DelayNs;
use embedded_io::{ErrorType, Read, Write};
use updater_core::frame::{self, CMD_ECHO, CMD_INFO, RSP_FLAG, ST_OK};
use updater_core::stream::SYNC;
use updater_core::{Session, Transport};
use updater_eh::{UartTransport, UartTransportError};

// ---------------------------------------------------------------------------
// fake serial port
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeIoError(embedded_io::ErrorKind);

impl std::fmt::Display for FakeIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl std::error::Error for FakeIoError {}

impl embedded_io::Error for FakeIoError {
    fn kind(&self) -> embedded_io::ErrorKind {
        self.0
    }
}

const IO_ERR: FakeIoError = FakeIoError(embedded_io::ErrorKind::Other);

/// One scripted RX event.
enum Rx {
    /// `read` returns these bytes (chunking is the script's choice).
    Data(Vec<u8>),
    /// `read` returns `Ok(0)`: HAL timeout with nothing received.
    Idle,
    /// `read` fails.
    Err,
}

#[derive(Default)]
struct FakeSerial {
    /// Everything written, in order.
    written: Vec<u8>,
    /// Scripted reads; when exhausted, further reads return `Ok(0)`.
    rx: VecDeque<Rx>,
    /// When true, the next write fails.
    write_fails: bool,
}

impl FakeSerial {
    fn with_rx(rx: Vec<Rx>) -> Self {
        Self { rx: rx.into(), ..Self::default() }
    }
}

impl ErrorType for FakeSerial {
    type Error = FakeIoError;
}

impl Read for FakeSerial {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, FakeIoError> {
        assert!(!buf.is_empty(), "reads must ask for at least one byte");
        match self.rx.pop_front() {
            None | Some(Rx::Idle) => Ok(0),
            Some(Rx::Err) => Err(IO_ERR),
            Some(Rx::Data(bytes)) => {
                assert!(bytes.len() <= buf.len(), "script chunk exceeds read buffer");
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
        }
    }
}

impl Write for FakeSerial {
    fn write(&mut self, buf: &[u8]) -> Result<usize, FakeIoError> {
        if self.write_fails {
            return Err(IO_ERR);
        }
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), FakeIoError> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct CountingDelay {
    calls: Rc<Cell<u32>>,
}

impl DelayNs for CountingDelay {
    fn delay_ns(&mut self, _ns: u32) {
        self.calls.set(self.calls.get() + 1);
    }
}

fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

/// `0x7E` + frame: what the device's `link_send` puts on the wire.
fn stream(frame_bytes: &[u8]) -> Vec<u8> {
    let mut s = vec![SYNC];
    s.extend_from_slice(frame_bytes);
    s
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn request_frames_with_sync_and_scans_the_response() {
    let req = enc(CMD_INFO, &[]);
    let rsp = enc(CMD_INFO | RSP_FLAG, &[ST_OK, 1, 2, 3]);
    // Response arrives noisy and split across reads: stale byte + busy
    // hunting fodder first, then the frame in two chunks.
    let mut wire = vec![0xA5];
    wire.extend_from_slice(&stream(&rsp));
    let (a, b) = wire.split_at(4);
    let port = FakeSerial::with_rx(vec![Rx::Data(a.to_vec()), Rx::Data(b.to_vec())]);
    let mut t = UartTransport::new(port, CountingDelay::default());

    let mut rx = [0u8; 32];
    let n = t.request(&req, &mut rx).unwrap();
    assert_eq!(&rx[..n], &rsp[..], "exactly the frame, no sync, no garbage");
    let f = frame::decode(&rx[..n]).unwrap();
    assert_eq!(f.payload, &[ST_OK, 1, 2, 3]);

    let (port, _) = t.release();
    assert_eq!(port.written, stream(&req), "request must go out as 0x7E + frame");
}

#[test]
fn idle_reads_consume_poll_budget_with_delays() {
    let req = enc(CMD_ECHO, &[]);
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK]);
    let port = FakeSerial::with_rx(vec![Rx::Idle, Rx::Idle, Rx::Data(stream(&rsp))]);
    let delay = CountingDelay::default();
    let calls = delay.calls.clone();
    let mut t = UartTransport::new(port, delay).with_poll(10, 1_000_000);

    let mut rx = [0u8; 16];
    t.request(&req, &mut rx).unwrap();
    assert_eq!(calls.get(), 2, "one delay per idle read");
}

#[test]
fn silence_exhausts_the_poll_budget_typed() {
    let req = enc(CMD_ECHO, &[]);
    let port = FakeSerial::with_rx(vec![]); // nothing ever arrives
    let delay = CountingDelay::default();
    let calls = delay.calls.clone();
    let mut t = UartTransport::new(port, delay).with_poll(3, 1_000);

    let mut rx = [0u8; 16];
    match t.request(&req, &mut rx) {
        Err(UartTransportError::Exhausted { attempts: 3 }) => {}
        other => panic!("expected Exhausted, got {other:?}"),
    }
    // No delay after the final failed attempt (I2C adapter discipline).
    assert_eq!(calls.get(), 2);
}

#[test]
fn corrupt_frame_is_dropped_and_the_retransmission_accepted() {
    let req = enc(CMD_ECHO, &[0x11]);
    let good = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x11]);
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 0x01;
    let mut wire = stream(&bad); // CRC failure: silently dropped
    wire.extend_from_slice(&stream(&good));
    let port = FakeSerial::with_rx(vec![Rx::Data(wire)]);
    let mut t = UartTransport::new(port, CountingDelay::default());

    let mut rx = [0u8; 16];
    let n = t.request(&req, &mut rx).unwrap();
    assert_eq!(&rx[..n], &good[..]);
}

#[test]
fn garbage_flood_hits_the_scan_limit_typed() {
    let req = enc(CMD_ECHO, &[]);
    // A babbling wire: endless non-sync junk, never a frame.
    let junk: Vec<Rx> = (0..16).map(|_| Rx::Data(vec![0x55; 8])).collect();
    let port = FakeSerial::with_rx(junk);
    let mut t = UartTransport::new(port, CountingDelay::default()).with_scan_limit(20);

    let mut rx = [0u8; 16];
    match t.request(&req, &mut rx) {
        Err(UartTransportError::Desync { scanned }) => assert!(scanned >= 20),
        other => panic!("expected Desync, got {other:?}"),
    }
}

#[test]
fn write_and_read_errors_are_typed() {
    let req = enc(CMD_ECHO, &[]);

    let port = FakeSerial { write_fails: true, ..FakeSerial::default() };
    let mut t = UartTransport::new(port, CountingDelay::default());
    let mut rx = [0u8; 16];
    match t.request(&req, &mut rx) {
        Err(UartTransportError::Write(e)) => assert_eq!(e, IO_ERR),
        other => panic!("expected Write, got {other:?}"),
    }

    let port = FakeSerial::with_rx(vec![Rx::Err]);
    let mut t = UartTransport::new(port, CountingDelay::default());
    match t.request(&req, &mut rx) {
        Err(UartTransportError::Read(e)) => assert_eq!(e, IO_ERR),
        other => panic!("expected Read, got {other:?}"),
    }
}

#[test]
fn errors_render_human_readable() {
    let e: UartTransportError<FakeIoError> = UartTransportError::Exhausted { attempts: 9 };
    assert!(format!("{e}").contains('9'));
    let e: UartTransportError<FakeIoError> = UartTransportError::Desync { scanned: 77 };
    assert!(format!("{e}").contains("77"));
    let e: UartTransportError<FakeIoError> = UartTransportError::Write(IO_ERR);
    assert!(!format!("{e}").is_empty());
}

#[test]
fn session_echo_runs_end_to_end_over_the_adapter() {
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0xA5, 0x5A]);
    let port = FakeSerial::with_rx(vec![Rx::Idle, Rx::Data(stream(&rsp))]);
    let t = UartTransport::new(port, CountingDelay::default()).with_poll(5, 1_000);

    let mut frame_buf = [0u8; 64];
    let mut session = Session::new(t, &mut frame_buf);
    session.echo(&[0xA5, 0x5A]).unwrap();
    let (port, _) = session.into_transport().release();
    assert_eq!(port.written, stream(&enc(CMD_ECHO, &[0xA5, 0x5A])));
}
