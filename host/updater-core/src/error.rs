// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Hand-rolled error type: no dependencies, no allocation, no strings built
//! at runtime — every variant carries the precise facts a caller can act on.

use core::convert::Infallible;
use core::fmt;

/// Everything that can go wrong in the host core.
///
/// `E` is the transport's error type (see the `Transport` trait, which lands
/// with the session layer). Pure codec and image operations never touch a
/// transport, so they return `Error<Infallible>` — the default — and
/// [`Error::widen`] lifts such an error into any `Error<E>` for free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E = Infallible> {
    /// The transport failed; carries the transport's own error.
    Transport(E),
    /// A received byte sequence is not a valid frame (too short, length byte
    /// inconsistent with the buffer, or CRC-8 mismatch).
    BadFrame,
    /// The device answered with a non-OK status byte (`frame::ST_*`).
    Device(u8),
    /// The requested payload does not fit a wire frame
    /// (`len > frame::PAYLOAD_MAX`).
    PayloadTooLarge {
        /// Length of the offending payload in bytes.
        len: usize,
    },
    /// A caller-provided output buffer is too small.
    BufferTooSmall {
        /// Minimum buffer length in bytes that the call requires.
        needed: usize,
    },
    /// The image does not fit the app region once the 16-byte footer is
    /// reserved.
    ImageTooLarge {
        /// Length of the offending image in bytes.
        len: usize,
        /// Usable capacity of the region in bytes (`region - 16`).
        capacity: usize,
    },
    /// The page geometry cannot host a valid image: `page_size` or
    /// `app_pages` is zero, the page is smaller than the 16-byte footer, or a
    /// page would not fit a single `WRITE_PAGE` frame.
    BadGeometry {
        /// Page size in bytes as given by the caller.
        page_size: u16,
        /// Number of app pages as given by the caller.
        app_pages: u16,
    },
    /// A page index at or beyond the number of app pages was requested.
    PageOutOfRange {
        /// The offending page index.
        index: u16,
    },
    /// The device speaks a protocol version this crate does not.
    ProtocolVersion {
        /// The protocol version the device reported.
        device: u8,
    },
}

impl Error<Infallible> {
    /// Lift a transport-free error into an `Error<E>` for any `E`.
    ///
    /// Codec and image APIs return `Error<Infallible>`; session code that
    /// combines them with transport calls uses `.map_err(Error::widen)`.
    #[must_use]
    pub fn widen<E>(self) -> Error<E> {
        match self {
            Error::Transport(never) => match never {},
            Error::BadFrame => Error::BadFrame,
            Error::Device(st) => Error::Device(st),
            Error::PayloadTooLarge { len } => Error::PayloadTooLarge { len },
            Error::BufferTooSmall { needed } => Error::BufferTooSmall { needed },
            Error::ImageTooLarge { len, capacity } => Error::ImageTooLarge { len, capacity },
            Error::BadGeometry { page_size, app_pages } => {
                Error::BadGeometry { page_size, app_pages }
            }
            Error::PageOutOfRange { index } => Error::PageOutOfRange { index },
            Error::ProtocolVersion { device } => Error::ProtocolVersion { device },
        }
    }
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "transport error: {e}"),
            Error::BadFrame => f.write_str("malformed frame (length or CRC-8 mismatch)"),
            Error::Device(st) => {
                write!(f, "device reported status {st:#04x} ({})", crate::frame::st_name(*st))
            }
            Error::PayloadTooLarge { len } => {
                write!(f, "payload of {len} bytes exceeds the frame limit")
            }
            Error::BufferTooSmall { needed } => {
                write!(f, "output buffer too small, need {needed} bytes")
            }
            Error::ImageTooLarge { len, capacity } => {
                write!(f, "image of {len} bytes exceeds region capacity of {capacity} bytes")
            }
            Error::BadGeometry { page_size, app_pages } => write!(
                f,
                "unusable geometry: page_size {page_size}, app_pages {app_pages}"
            ),
            Error::PageOutOfRange { index } => write!(f, "page index {index} out of range"),
            Error::ProtocolVersion { device } => {
                write!(f, "unsupported device protocol version {device}")
            }
        }
    }
}

impl<E: fmt::Debug + fmt::Display> core::error::Error for Error<E> {}
