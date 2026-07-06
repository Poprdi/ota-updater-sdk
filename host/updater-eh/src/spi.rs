//! [`Transport`] over an `embedded-hal` [`SpiDevice`].
//!
//! Wire binding (see `device/ports/skeletons/spi_pump.h` for the slave
//! half): the request leaves as `0x7E` + frame in ONE transaction (one
//! chip-select assertion); the response is found by clocking idle bytes
//! and scanning MISO. The slave shifts `0x00` while idle or busy, and —
//! the slave-side one-byte lag — at least one stale byte precedes the
//! `0x7E` sync; both are hunt fodder for the shared stream scanner.
//!
//! Two delays shape the exchange:
//!
//! * the **poll interval** between busy bytes (the device may be erasing
//!   for hundreds of milliseconds), bounded by the attempt budget exactly
//!   like the I2C adapter's NACK poll;
//! * the **byte pacing** between in-frame exchanges — an SPI slave only
//!   services its shift register between the host's exchanges, so the gap
//!   the host leaves is the device's compute budget. The default
//!   ([`DEFAULT_SPI_BYTE_INTERVAL_NS`]) is generous for MHz-class MCUs;
//!   shorten it once your device's pump loop is measured.

use core::fmt;

use embedded_hal::delay::DelayNs;
use embedded_hal::spi::{Operation, SpiDevice};
use updater_core::stream::{RxScanner, Scan, SYNC};
use updater_core::Transport;

use crate::{DEFAULT_POLL_ATTEMPTS, DEFAULT_POLL_INTERVAL_NS};

/// Default gap between in-frame byte exchanges: 50 us of compute budget
/// for the slave's pump loop per byte.
pub const DEFAULT_SPI_BYTE_INTERVAL_NS: u32 = 50_000;

/// What went wrong on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpiTransportError<E> {
    /// The request transaction failed.
    Write(E),
    /// A response-scan read failed.
    Read(E),
    /// The poll budget ran out without a valid response frame (busy
    /// bytes, garbage and dropped frames all count as attempts).
    Exhausted {
        /// Number of non-frame bytes clocked.
        attempts: u32,
    },
}

impl<E: embedded_hal::spi::Error> fmt::Display for SpiTransportError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Write(e) => write!(f, "spi write failed: {}", e.kind()),
            Self::Read(e) => write!(f, "spi read failed: {}", e.kind()),
            Self::Exhausted { attempts } => write!(
                f,
                "no response within {attempts} clocked byte(s) — is the device in the \
                 bootloader? if this was erase/flash, it may still be erasing — widen \
                 the poll budget (`with_poll`)"
            ),
        }
    }
}

impl<E: fmt::Debug + embedded_hal::spi::Error> core::error::Error for SpiTransportError<E> {}

/// [`Transport`] over a blocking [`SpiDevice`] (a bus + chip-select pair;
/// every HAL provides one).
///
/// ```no_run
/// # fn wire<Spi, Delay>(spi: Spi, delay: Delay)
/// # where Spi: embedded_hal::spi::SpiDevice, Delay: embedded_hal::delay::DelayNs {
/// let mut buf = [0u8; 512];
/// let transport = updater_eh::SpiTransport::new(spi, delay);
/// let mut session = updater_core::Session::new(transport, &mut buf);
/// # }
/// ```
#[derive(Debug)]
pub struct SpiTransport<S, D> {
    spi: S,
    delay: D,
    poll_attempts: u32,
    poll_interval_ns: u32,
    byte_interval_ns: u32,
}

impl<S, D> SpiTransport<S, D> {
    /// Wrap an SPI device, using `delay` for poll and pacing gaps;
    /// polling defaults to [`DEFAULT_POLL_ATTEMPTS`] ×
    /// [`DEFAULT_POLL_INTERVAL_NS`], pacing to
    /// [`DEFAULT_SPI_BYTE_INTERVAL_NS`].
    pub fn new(spi: S, delay: D) -> Self {
        Self {
            spi,
            delay,
            poll_attempts: DEFAULT_POLL_ATTEMPTS,
            poll_interval_ns: DEFAULT_POLL_INTERVAL_NS,
            byte_interval_ns: DEFAULT_SPI_BYTE_INTERVAL_NS,
        }
    }

    /// Override the poll budget: up to `attempts` non-frame bytes
    /// (clamped to at least 1), `interval_ns` apart.
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

    /// Override the in-frame byte pacing gap.
    #[must_use]
    pub fn with_pacing(mut self, byte_interval_ns: u32) -> Self {
        self.byte_interval_ns = byte_interval_ns;
        self
    }

    /// Take the bus and delay back.
    pub fn release(self) -> (S, D) {
        (self.spi, self.delay)
    }
}

impl<S: SpiDevice, D: DelayNs> Transport for SpiTransport<S, D> {
    type Err = SpiTransportError<S::Error>;

    /// Send `0x7E` + `req` in one transaction, then clock idle bytes and
    /// scan MISO until one valid frame lands at the front of `rsp`;
    /// returns its exact length.
    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        self.spi
            .transaction(&mut [Operation::Write(&[SYNC]), Operation::Write(req)])
            .map_err(SpiTransportError::Write)?;

        let mut scanner = RxScanner::new();
        let mut attempts: u32 = 0;
        loop {
            let mut byte = [0u8; 1];
            self.spi.read(&mut byte).map_err(SpiTransportError::Read)?;
            match scanner.push(byte[0], rsp) {
                Scan::Done { len } => return Ok(len),
                // In-frame: leave the slave its per-byte compute gap.
                Scan::Frame => self.delay.delay_ns(self.byte_interval_ns),
                Scan::Hunt | Scan::Dropped => {
                    // Busy byte, stale lag byte or a dropped frame: one
                    // poll spent. Bounded: exits at poll_attempts >= 1.
                    attempts = attempts.wrapping_add(1);
                    if attempts >= self.poll_attempts {
                        return Err(SpiTransportError::Exhausted { attempts });
                    }
                    self.delay.delay_ns(self.poll_interval_ns);
                }
            }
        }
    }
}
