// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! [`Transport`] over two GPIOs: bit-banged 9600-baud 8N1 — the "two
//! spare random pins" escape hatch, mirroring the device-side
//! `softuart_pump` timing discipline byte for byte.
//!
//! A UART receiver has no clock line; every sample time derives from the
//! start bit's falling edge. This adapter detects the edge by sampling RX
//! every poll interval, waits 1.5 bit times (into the center of data
//! bit 0), samples every 1.0 bit time LSB-first, and checks the stop bit
//! at 9.5 bit times — the same arithmetic, and therefore the same ±0.5-bit
//! error budget, as the device pump (see
//! `device/ports/skeletons/softuart_pump.h` for the full derivation; 9600
//! is the deliberate speed cap). A byte whose stop bit reads low (framing
//! error) is dropped after a bounded wait for the line to return to mark,
//! so a break never cascades into phantom start bits.
//!
//! **The poll interval is also the edge-detection latency** and eats the
//! shared 0.5-bit budget: keep it well under half a bit time (52 us). The
//! default samples every 2 us. Half-duplex by construction — the protocol
//! never talks both directions at once.
//!
//! `DelayNs` accuracy is the whole timing base here. A busy-wait timer
//! delay is ideal; an OS sleep (thread scheduling) is generally NOT
//! accurate enough to bit-bang reliably — expect CRC drops and retries if
//! you try this from a non-realtime userspace.

use core::fmt;

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{InputPin, OutputPin};
use updater_core::stream::{RxScanner, Scan, SYNC};
use updater_core::Transport;

use crate::DEFAULT_SCAN_LIMIT;

/// Nominal 9600-baud bit time in nanoseconds (1e9 / 9600, rounded).
pub const SOFTUART_BIT_NS: u32 = 104_167;
/// Half a bit time.
const HALF_BIT_NS: u32 = SOFTUART_BIT_NS / 2;
/// 1.5 bit times: start-bit edge to the center of data bit 0.
const BIT_1_5_NS: u32 = SOFTUART_BIT_NS.saturating_add(HALF_BIT_NS);

/// Default start-edge poll budget: attempts × interval = a ~1 s window,
/// sized for short commands and single page writes.
///
/// `ERASE_APP` is NOT covered on larger parts: the device erases the whole
/// region page by page and holds the line for the duration — an AVR64EA28
/// takes roughly 5 s (480 pages × ~10 ms each). Widen the window with
/// [`SoftUartTransport::with_poll`] so that attempts × interval ≥
/// `app_pages` × per-page erase time, taking the per-page figure from the
/// device datasheet.
pub const DEFAULT_SOFTUART_POLL_ATTEMPTS: u32 = 500_000;
/// Default RX sampling interval while idle (2 us — this is edge-detection
/// latency, keep it well under half a bit time).
pub const DEFAULT_SOFTUART_POLL_INTERVAL_NS: u32 = 2_000;

/// What went wrong on the pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftUartTransportError<TE, RE> {
    /// Driving the TX pin failed.
    Tx(TE),
    /// Sampling the RX pin failed.
    Rx(RE),
    /// The poll budget ran out with the line idle.
    Exhausted {
        /// Number of idle samples taken.
        attempts: u32,
    },
    /// The scan budget ran out: bytes keep arriving but no valid frame
    /// (noise, or not our protocol on these pins).
    Desync {
        /// Non-frame bytes (including framing errors) before giving up.
        scanned: u32,
    },
}

impl<TE: fmt::Debug, RE: fmt::Debug> fmt::Display for SoftUartTransportError<TE, RE> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tx(e) => write!(f, "softuart tx pin failed: {e:?}"),
            Self::Rx(e) => write!(f, "softuart rx pin failed: {e:?}"),
            Self::Exhausted { attempts } => write!(
                f,
                "line stayed idle for {attempts} sample(s) — is the device in the \
                 bootloader? if this was erase/flash, it may still be erasing — widen \
                 the poll budget (`with_poll`)"
            ),
            Self::Desync { scanned } => write!(
                f,
                "no valid frame in {scanned} received byte(s) — noise, or wrong pins?"
            ),
        }
    }
}

impl<TE: fmt::Debug, RE: fmt::Debug> core::error::Error for SoftUartTransportError<TE, RE> {}

/// [`Transport`] over a TX [`OutputPin`], an RX [`InputPin`] and a
/// [`DelayNs`], speaking 9600-baud 8N1.
///
/// ```no_run
/// # fn wire<Tx, Rx, Delay>(tx: Tx, rx: Rx, delay: Delay)
/// # where Tx: embedded_hal::digital::OutputPin,
/// #       Rx: embedded_hal::digital::InputPin,
/// #       Delay: embedded_hal::delay::DelayNs {
/// let mut buf = [0u8; 512];
/// let transport = updater_eh::SoftUartTransport::new(tx, rx, delay);
/// let mut session = updater_core::Session::new(transport, &mut buf);
/// # }
/// ```
#[derive(Debug)]
pub struct SoftUartTransport<Tx, Rx, D> {
    tx: Tx,
    rx: Rx,
    delay: D,
    poll_attempts: u32,
    poll_interval_ns: u32,
    scan_limit: u32,
}

impl<Tx, Rx, D> SoftUartTransport<Tx, Rx, D> {
    /// Wrap the pins and delay. Configure TX as output-high and RX as an
    /// input (pulled up if the wiring floats) **before** constructing, so
    /// the device never sees a glitch that reads as a start bit; every
    /// request re-asserts idle mark before transmitting. Polling defaults
    /// to [`DEFAULT_SOFTUART_POLL_ATTEMPTS`] ×
    /// [`DEFAULT_SOFTUART_POLL_INTERVAL_NS`], the babble bound to
    /// `DEFAULT_SCAN_LIMIT`.
    pub fn new(tx: Tx, rx: Rx, delay: D) -> Self {
        Self {
            tx,
            rx,
            delay,
            poll_attempts: DEFAULT_SOFTUART_POLL_ATTEMPTS,
            poll_interval_ns: DEFAULT_SOFTUART_POLL_INTERVAL_NS,
            scan_limit: DEFAULT_SCAN_LIMIT,
        }
    }

    /// Override the idle poll budget: up to `attempts` RX samples (clamped
    /// to at least 1), `interval_ns` apart. The interval doubles as
    /// edge-detection latency — keep it well under half a bit (52 us).
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

    /// Take the pins and delay back.
    pub fn release(self) -> (Tx, Rx, D) {
        (self.tx, self.rx, self.delay)
    }
}

impl<Tx: OutputPin, Rx: InputPin, D: DelayNs> SoftUartTransport<Tx, Rx, D> {
    /// Transmit one 8N1 byte: start bit, 8 data bits LSB first, stop bit
    /// (line left at mark, so back-to-back bytes are legal).
    fn send_byte(&mut self, byte: u8) -> Result<(), SoftUartTransportError<Tx::Error, Rx::Error>> {
        self.tx.set_low().map_err(SoftUartTransportError::Tx)?; // start bit
        self.delay.delay_ns(SOFTUART_BIT_NS);
        for bit in 0..8u32 {
            // LSB first (UART order); shift bounded by the loop range.
            if byte.wrapping_shr(bit) & 1 != 0 {
                self.tx.set_high().map_err(SoftUartTransportError::Tx)?;
            } else {
                self.tx.set_low().map_err(SoftUartTransportError::Tx)?;
            }
            self.delay.delay_ns(SOFTUART_BIT_NS);
        }
        self.tx.set_high().map_err(SoftUartTransportError::Tx)?; // stop = mark
        self.delay.delay_ns(SOFTUART_BIT_NS);
        Ok(())
    }
}

impl<Tx: OutputPin, Rx: InputPin, D: DelayNs> Transport for SoftUartTransport<Tx, Rx, D> {
    type Err = SoftUartTransportError<Tx::Error, Rx::Error>;

    /// Send `0x7E` + `req` bit-banged, then receive and scan bytes until
    /// one valid frame lands at the front of `rsp`; returns its length.
    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        // Idle mark for one bit time before traffic: a clean high-to-low
        // start edge even if the line was just released.
        self.tx.set_high().map_err(SoftUartTransportError::Tx)?;
        self.delay.delay_ns(SOFTUART_BIT_NS);
        self.send_byte(SYNC)?;
        for &b in req {
            self.send_byte(b)?;
        }

        let mut scanner = RxScanner::new();
        let mut attempts: u32 = 0;
        let mut junk: u32 = 0;
        loop {
            // Hunt the start edge, sampling every poll interval. Bounded:
            // exits at poll_attempts >= 1.
            while self.rx.is_high().map_err(SoftUartTransportError::Rx)? {
                attempts = attempts.wrapping_add(1);
                if attempts >= self.poll_attempts {
                    return Err(SoftUartTransportError::Exhausted { attempts });
                }
                self.delay.delay_ns(self.poll_interval_ns);
            }

            // Start edge (or its aftermath) just observed: 1.5 bit times
            // to the center of data bit 0, then 1.0 per bit, LSB first.
            self.delay.delay_ns(BIT_1_5_NS);
            let mut byte: u8 = 0;
            for bit in 0..8u32 {
                if self.rx.is_high().map_err(SoftUartTransportError::Rx)? {
                    // Shift bounded by the loop range.
                    byte |= 1u8.wrapping_shl(bit);
                }
                self.delay.delay_ns(SOFTUART_BIT_NS);
            }

            // Stop-bit center (9.5 bit times). Low = framing error: drop
            // the byte, but wait (bounded) for mark first so the low tail
            // does not re-trigger as a phantom start bit — the device pump
            // does exactly the same.
            if self.rx.is_low().map_err(SoftUartTransportError::Rx)? {
                for _ in 0..20u8 {
                    if self.rx.is_high().map_err(SoftUartTransportError::Rx)? {
                        break;
                    }
                    self.delay.delay_ns(HALF_BIT_NS);
                }
                // Bounded: exits at scan_limit >= 1.
                junk = junk.wrapping_add(1);
                if junk >= self.scan_limit {
                    return Err(SoftUartTransportError::Desync { scanned: junk });
                }
                continue;
            }

            match scanner.push(byte, rsp) {
                Scan::Done { len } => return Ok(len),
                Scan::Frame => {}
                Scan::Hunt | Scan::Dropped => {
                    // Bounded: exits at scan_limit >= 1.
                    junk = junk.wrapping_add(1);
                    if junk >= self.scan_limit {
                        return Err(SoftUartTransportError::Desync { scanned: junk });
                    }
                }
            }
        }
    }
}
