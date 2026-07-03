//! # updater-core
//!
//! Host-side core of the updater SDK: wire-frame codec and image handling
//! for driving a device running the updater bootloader.
//!
//! The crate is `#![no_std]`, allocation-free and dependency-free so the
//! **identical** code runs on a Raspberry Pi (Linux), a Raspberry Pi Pico
//! 2 W or an ESP32: callers provide every buffer, decoded frames and images borrow
//! their input, and platform glue is confined to a transport implementation
//! (separate crates).
//!
//! * [`frame`] — encode/decode of `CMD LEN payload CRC8` frames, wire
//!   constants mirroring the device's `proto.h`, CRC-8.
//! * [`image`] — borrowed `.bin` images: footer construction, CRC-32 and
//!   the skip-`0xFF` page iteration seam.
//! * [`transport`] — the [`Transport`] trait, the one seam a platform must
//!   fill in (Linux i2c-dev, embedded-hal I2C, …).
//! * [`session`] — the [`Session`] engine: INFO/ECHO/FLASH/BOOT with
//!   bounded retries through one caller-provided frame buffer.
//! * [`error`] — the hand-rolled [`Error`] type, generic over a transport
//!   error.
//!
//! Intel-HEX parsing intentionally does **not** live here; converting
//! `.hex` to raw binary is a host-tool concern (`updater-cli`).

#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
#![warn(clippy::pedantic, missing_docs)]

pub mod error;
pub mod frame;
pub mod image;
pub mod session;
pub mod transport;

pub use error::Error;
pub use session::{DeviceInfo, Session};
pub use transport::Transport;
