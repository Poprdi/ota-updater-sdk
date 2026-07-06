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
//! * [`stream`] — the shared byte-stream framing scanner (`0x7E` sync
//!   hunt + LEN-driven completion) that UART/SPI/GPIO transports build
//!   on; mirrors the device's `link_stream.c`.
//! * [`error`] — the hand-rolled [`Error`] type, generic over a transport
//!   error.
//!
//! Intel-HEX parsing intentionally does **not** live here; converting
//! `.hex` to raw binary is a host-tool concern (`updater-cli`).
//!
//! # End to end
//!
//! One full update, from wire to boot (implement [`Transport`] for your
//! platform, or take a ready adapter from `updater-eh`):
//!
//! ```no_run
//! use updater_core::image::Image;
//! use updater_core::{Session, Transport};
//!
//! /// Your platform glue: one request/response exchange on the wire.
//! struct MyWire;
//! impl Transport for MyWire {
//!     type Err = core::convert::Infallible;
//!     fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
//!         let _ = (req, rsp);
//!         unimplemented!("send req, receive the device's reply into rsp")
//!     }
//! }
//!
//! # fn main() -> Result<(), updater_core::Error<core::convert::Infallible>> {
//! let mut frame_buf = [0u8; 320]; // session sizing rule: covers every geometry
//! let mut session = Session::new(MyWire, &mut frame_buf);
//!
//! let info = session.info()?;              // identity + geometry
//! let app: &[u8] = &[0x42; 1024];          // your application binary (.bin)
//! let img = Image::from_bin(app, info.page_size, info.app_pages)?; // THAT geometry
//! session.flash(&img, &mut |done, total| { // erase + write + verify
//!     let _ = (done, total);               // e.g. draw a progress bar
//! })?;
//! session.boot()?;                         // jump to the app
//! # Ok(())
//! # }
//! ```

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
pub mod stream;
pub mod transport;

pub use error::Error;
pub use session::{DeviceInfo, Session};
pub use transport::Transport;
