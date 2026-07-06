// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! # updater-eh
//!
//! [`updater_core::Transport`] adapters over the `embedded-hal` 1.x /
//! `embedded-io` traits: the platform glue for any master whose HAL speaks
//! those traits — Raspberry Pi Pico 2 W, ESP32, or a Linux box through its
//! embedded-hal shims. Pick the adapter matching your wiring, hand it the
//! HAL objects, and give the result to `updater_core::Session`; nothing
//! else is required.
//!
//! * [`I2cTransport`] — blocking I2C ([`embedded_hal::i2c::I2c`]):
//!   write-then-poll-read, fixed-length reads padded by the device with
//!   `0xFF` idle bytes.
//! * [`UartTransport`] — a byte stream ([`embedded_io::Read`] +
//!   [`embedded_io::Write`]): hardware UART, USB-CDC, RS-485, anything
//!   stream-shaped.
//! * [`SpiTransport`] — [`embedded_hal::spi::SpiDevice`]: the host clocks
//!   idle bytes to poll; the device shifts `0x00` while busy and `0x7E`
//!   when the response starts.
//! * [`SoftUartTransport`] — two GPIOs + a delay
//!   ([`embedded_hal::digital`], [`embedded_hal::delay::DelayNs`]):
//!   bit-banged 9600-baud 8N1 for pin-budget emergencies.
//!
//! The stream transports (UART, SPI, softuart) share one wire binding —
//! `0x7E` sync + frame, `updater_core::stream` — and one polling
//! discipline: bounded attempts with a configurable delay, like the I2C
//! adapter's busy-poll. Every adapter is `no_std`, allocation-free and
//! panic-free by construction.

#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
#![warn(clippy::pedantic, missing_docs)]

mod i2c;
mod softuart;
mod spi;
mod uart;

pub use i2c::{I2cTransport, I2cTransportError};
pub use softuart::{
    SoftUartTransport, SoftUartTransportError, DEFAULT_SOFTUART_POLL_ATTEMPTS,
    DEFAULT_SOFTUART_POLL_INTERVAL_NS, SOFTUART_BIT_NS,
};
pub use spi::{SpiTransport, SpiTransportError, DEFAULT_SPI_BYTE_INTERVAL_NS};
pub use uart::{UartTransport, UartTransportError};

/// Default poll budget: attempts per response wait.
pub const DEFAULT_POLL_ATTEMPTS: u32 = 200;
/// Default delay between poll attempts (5 ms — with the default budget,
/// a ~1 s window sized for short commands and single page writes).
///
/// `ERASE_APP` is NOT covered on larger parts: the device erases the whole
/// region page by page and holds the bus for the duration — an AVR64EA28
/// takes roughly 5 s (480 pages × ~10 ms each). Widen the window with
/// `with_poll` so that attempts × delay ≥ `app_pages` × per-page erase time,
/// taking the per-page figure from the device datasheet.
pub const DEFAULT_POLL_INTERVAL_NS: u32 = 5_000_000;
/// Default babble bound for the stream transports: non-frame events
/// (hunted garbage bytes, dropped frames, framing errors) tolerated per
/// request before the line is declared desynchronized.
pub const DEFAULT_SCAN_LIMIT: u32 = 4096;
