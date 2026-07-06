// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! [`Transport`] over an `embedded-io` byte stream (hardware UART,
//! USB-CDC, RS-485 — anything stream-shaped).
//!
//! The request leaves as `0x7E` + frame (the shared stream binding,
//! `updater_core::stream`); the response is found by scanning received
//! bytes for the sync and completing LEN-driven, so line noise before or
//! between frames is tolerated and a CRC-corrupt response is silently
//! dropped in favor of whatever follows (the session's retry owns
//! recovery, exactly as on the device).
//!
//! Blocking discipline: [`embedded_io::Read`] blocks until at least one
//! byte arrives. HALs with their own receive timeout surface "nothing yet"
//! as `Ok(0)`; this adapter treats that as one poll attempt — delay, try
//! again, bounded like the I2C adapter's NACK poll. On HALs that block
//! forever, the poll budget is simply never consulted and a dead device
//! shows up as the HAL's own error (or an eternal wait — give such HALs a
//! read timeout if you can).

use core::fmt;

use embedded_hal::delay::DelayNs;
use embedded_io::{Read, Write};
use updater_core::stream::{RxScanner, Scan, SYNC};
use updater_core::Transport;

use crate::{DEFAULT_POLL_ATTEMPTS, DEFAULT_POLL_INTERVAL_NS, DEFAULT_SCAN_LIMIT};

/// What went wrong on the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UartTransportError<E> {
    /// Sending the request (write or flush) failed.
    Write(E),
    /// A read failed.
    Read(E),
    /// The poll budget ran out with the device silent.
    Exhausted {
        /// Number of idle reads made.
        attempts: u32,
    },
    /// The scan budget ran out: the line babbles but no valid frame
    /// arrived (wrong baud rate? another protocol on the port?).
    Desync {
        /// Non-frame bytes discarded before giving up.
        scanned: u32,
    },
}

impl<E: embedded_io::Error> fmt::Display for UartTransportError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(e) => write!(f, "uart write failed: {e}"),
            Self::Read(e) => write!(f, "uart read failed: {e}"),
            Self::Exhausted { attempts } => write!(
                f,
                "device sent nothing within {attempts} poll attempt(s) — is it in the \
                 bootloader? if this was erase/flash, it may still be erasing — widen \
                 the poll budget (`with_poll`)"
            ),
            Self::Desync { scanned } => write!(
                f,
                "no valid frame in {scanned} scanned byte(s) — wrong baud rate or wrong port?"
            ),
        }
    }
}

impl<E: fmt::Debug + embedded_io::Error> core::error::Error for UartTransportError<E> {}

/// [`Transport`] over one object speaking [`embedded_io::Read`] +
/// [`embedded_io::Write`].
///
/// ```no_run
/// # fn wire<Serial, Delay>(serial: Serial, delay: Delay)
/// # where Serial: embedded_io::Read + embedded_io::Write, Delay: embedded_hal::delay::DelayNs {
/// // Sized per the Session buffer rule (`updater_core::session` docs):
/// // largest request + largest response; 320 covers every legal geometry.
/// let mut buf = [0u8; 512];
/// let transport = updater_eh::UartTransport::new(serial, delay);
/// let mut session = updater_core::Session::new(transport, &mut buf);
/// # }
/// ```
#[derive(Debug)]
pub struct UartTransport<T, D> {
    io: T,
    delay: D,
    poll_attempts: u32,
    poll_interval_ns: u32,
    scan_limit: u32,
}

impl<T, D> UartTransport<T, D> {
    /// Wrap a serial port, using `delay` between idle polls; polling
    /// defaults to [`DEFAULT_POLL_ATTEMPTS`] × [`DEFAULT_POLL_INTERVAL_NS`]
    /// and the babble bound to [`DEFAULT_SCAN_LIMIT`].
    pub fn new(io: T, delay: D) -> Self {
        Self {
            io,
            delay,
            poll_attempts: DEFAULT_POLL_ATTEMPTS,
            poll_interval_ns: DEFAULT_POLL_INTERVAL_NS,
            scan_limit: DEFAULT_SCAN_LIMIT,
        }
    }

    /// Override the poll budget: up to `attempts` idle reads (clamped to
    /// at least 1), `interval_ns` apart.
    #[must_use]
    pub fn with_poll(mut self, attempts: u32, interval_ns: u32) -> Self {
        self.set_poll(attempts, interval_ns);
        self
    }

    /// Re-tune the poll budget on a live transport (same clamping as
    /// [`Self::with_poll`]) — e.g. widen it for one long `ERASE_APP`
    /// exchange and narrow it back afterwards.
    pub fn set_poll(&mut self, attempts: u32, interval_ns: u32) {
        self.poll_attempts = attempts.max(1);
        self.poll_interval_ns = interval_ns;
    }

    /// Override the babble bound: non-frame bytes tolerated per request
    /// (clamped to at least 1).
    #[must_use]
    pub fn with_scan_limit(mut self, bytes: u32) -> Self {
        self.scan_limit = bytes.max(1);
        self
    }

    /// Take the port and delay back.
    pub fn release(self) -> (T, D) {
        (self.io, self.delay)
    }
}

impl<T: Read + Write, D: DelayNs> Transport for UartTransport<T, D> {
    type Err = UartTransportError<T::Error>;

    /// Send `0x7E` + `req`, then scan the receive stream until one valid
    /// frame lands at the front of `rsp`; returns its exact length.
    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        self.io.write_all(&[SYNC]).map_err(UartTransportError::Write)?;
        self.io.write_all(req).map_err(UartTransportError::Write)?;
        self.io.flush().map_err(UartTransportError::Write)?;

        let mut scanner = RxScanner::new();
        let mut attempts: u32 = 0;
        let mut junk: u32 = 0;
        let mut chunk = [0u8; 32];
        loop {
            let n = self.io.read(&mut chunk).map_err(UartTransportError::Read)?;
            if n == 0 {
                // HAL-level timeout with nothing received: one poll spent.
                // Bounded: exits at poll_attempts >= 1.
                attempts = attempts.wrapping_add(1);
                if attempts >= self.poll_attempts {
                    return Err(UartTransportError::Exhausted { attempts });
                }
                self.delay.delay_ns(self.poll_interval_ns);
                continue;
            }
            // n <= chunk.len() by the Read contract; min() keeps a lying
            // HAL from panicking us.
            for &byte in chunk.get(..n.min(chunk.len())).unwrap_or(&[]) {
                match scanner.push(byte, rsp) {
                    Scan::Done { len } => return Ok(len),
                    Scan::Frame => {}
                    Scan::Hunt | Scan::Dropped => {
                        // Bounded: exits at scan_limit >= 1.
                        junk = junk.wrapping_add(1);
                        if junk >= self.scan_limit {
                            return Err(UartTransportError::Desync { scanned: junk });
                        }
                    }
                }
            }
        }
    }
}
