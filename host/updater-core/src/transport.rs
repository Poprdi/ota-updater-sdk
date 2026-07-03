//! The [`Transport`] trait: the single seam between the portable session
//! engine and a platform.
//!
//! A transport moves one encoded request frame to the device and one
//! response into the caller's buffer. Everything else — framing, retries,
//! protocol sequencing — lives above it in [`crate::session`], so porting
//! the SDK to a new master CPU is exactly one `impl Transport`.

/// One request/response exchange with the device.
///
/// # Contract
///
/// * `req` holds exactly one encoded frame; the implementation must deliver
///   it unmodified.
/// * The response is written to the front of `rsp`; the returned length is
///   how many bytes were placed there (at most `rsp.len()`).
/// * The returned bytes must contain the device's response frame, and it
///   may be followed by `0xFF` filler — the session decodes with
///   [`crate::frame::decode_padded`]. A fixed-length transport (I2C master
///   read) can therefore simply fill all of `rsp` in one read and return
///   `rsp.len()`; the device pads its tail with `0xFF` idle bytes.
/// * `rsp` is sized by the session to the worst-case response for the
///   command in flight; implementations must not require more room.
/// * Blocking for the device is the implementation's business (poll, retry
///   on address NACK, clock stretch…); return `Err` only once the exchange
///   has genuinely failed. The session retries failed exchanges a bounded
///   number of times — the request frames are idempotent by protocol
///   design, so re-sending after a lost response is safe.
pub trait Transport {
    /// The transport's own error type.
    type Err;

    /// Send `req`, receive into `rsp`, return the number of response bytes.
    ///
    /// # Errors
    ///
    /// Implementation-defined; any error aborts the current attempt and the
    /// session may re-invoke `request` with the same `req`.
    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err>;
}

/// A `&mut T` transport forwards to `T`: lets callers keep ownership (e.g.
/// to inspect a test double after the session is done).
impl<T: Transport + ?Sized> Transport for &mut T {
    type Err = T::Err;

    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Self::Err> {
        (**self).request(req, rsp)
    }
}
