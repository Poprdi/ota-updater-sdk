//! # updater-eh
//!
//! [`updater_core::Transport`] over `embedded-hal` 1.x blocking I2C: the
//! platform glue for any master whose HAL speaks embedded-hal — Raspberry
//! Pi Pico 2 W, ESP32, or a Linux box through `linux-embedded-hal`.
//!
//! The exchange is write-then-poll-read: the request frame goes out in one
//! I2C write, then the response is read in one **fixed-length** read of the
//! whole `rsp` buffer (the master must pick a read length before the device
//! has said how long its response is). The device pads the tail of the read
//! with `0xFF` idle bytes; the session decodes such responses with
//! `updater_core::frame::decode_padded`. While the device is busy (erasing
//! or burning a page) it NACKs its address or clock-stretches — the read is
//! therefore retried a bounded number of times with a configurable delay
//! between attempts.

#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
#![warn(clippy::pedantic, missing_docs)]

use core::fmt;

use embedded_hal::delay::DelayNs;
use embedded_hal::i2c::{I2c, SevenBitAddress};
use updater_core::Transport;

/// Default poll budget: attempts per response read.
pub const DEFAULT_POLL_ATTEMPTS: u32 = 200;
/// Default delay between poll attempts (5 ms — with the default budget,
/// a one-second window, enough for a full-region erase on AVR-class flash).
pub const DEFAULT_POLL_INTERVAL_NS: u32 = 5_000_000;

/// What went wrong on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum I2cTransportError<E> {
    /// The request write failed.
    Write(E),
    /// Every poll read failed; carries the attempt count and last bus error.
    Exhausted {
        /// Number of read attempts made.
        attempts: u32,
        /// The bus error from the final attempt.
        last: E,
    },
}

impl<E: embedded_hal::i2c::Error> fmt::Display for I2cTransportError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(e) => write!(f, "i2c write failed: {}", e.kind()),
            Self::Exhausted { attempts, last } => write!(
                f,
                "device did not answer within {attempts} poll attempt(s), last i2c error: {}",
                last.kind()
            ),
        }
    }
}

impl<E: fmt::Debug + embedded_hal::i2c::Error> core::error::Error for I2cTransportError<E> {}

/// [`Transport`] over a blocking embedded-hal I2C bus.
#[derive(Debug)]
pub struct I2cTransport<I2C, D> {
    i2c: I2C,
    delay: D,
    addr: SevenBitAddress,
    poll_attempts: u32,
    poll_interval_ns: u32,
}

impl<I2C, D> I2cTransport<I2C, D> {
    /// Wrap `i2c` targeting the 7-bit device address `addr`, using `delay`
    /// between poll attempts; polling defaults to
    /// [`DEFAULT_POLL_ATTEMPTS`] × [`DEFAULT_POLL_INTERVAL_NS`].
    pub fn new(i2c: I2C, delay: D, addr: SevenBitAddress) -> Self {
        Self {
            i2c,
            delay,
            addr,
            poll_attempts: DEFAULT_POLL_ATTEMPTS,
            poll_interval_ns: DEFAULT_POLL_INTERVAL_NS,
        }
    }

    /// Override the poll budget: up to `attempts` reads (clamped to at
    /// least 1), `interval_ns` apart.
    #[must_use]
    pub fn with_poll(mut self, attempts: u32, interval_ns: u32) -> Self {
        self.poll_attempts = attempts.max(1);
        self.poll_interval_ns = interval_ns;
        self
    }

    /// Take the bus and delay back (e.g. to reuse the peripheral after the
    /// update session).
    pub fn release(self) -> (I2C, D) {
        (self.i2c, self.delay)
    }
}

impl<I2C: I2c, D: DelayNs> Transport for I2cTransport<I2C, D> {
    type Err = I2cTransportError<I2C::Error>;

    /// Write `req`, then poll-read the response as one fixed-length read
    /// filling all of `rsp` (device pads with `0xFF`); always returns
    /// `rsp.len()` on success.
    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        self.i2c
            .write(self.addr, req)
            .map_err(I2cTransportError::Write)?;

        let mut attempt: u32 = 0;
        loop {
            // Bounded: the loop exits at poll_attempts, which is >= 1.
            attempt = attempt.wrapping_add(1);
            match self.i2c.read(self.addr, rsp) {
                Ok(()) => return Ok(rsp.len()),
                Err(e) => {
                    if attempt >= self.poll_attempts {
                        return Err(I2cTransportError::Exhausted { attempts: attempt, last: e });
                    }
                    self.delay.delay_ns(self.poll_interval_ns);
                }
            }
        }
    }
}
