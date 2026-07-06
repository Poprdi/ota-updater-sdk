//! The session engine: `INFO → ERASE → WRITE* → VERIFY` (and `BOOT`,
//! `ECHO`) over any [`Transport`], with bounded retries and zero
//! allocation.
//!
//! # Buffer discipline
//!
//! [`Session::new`] takes one caller-provided `frame_buf`. For every
//! exchange it is split into a transmit window followed by a receive
//! window, each sized exactly for the command in flight; a `WRITE_PAGE`
//! renders the flash page **in place** into the transmit window's payload
//! bytes via [`Image::page_into`] — no page-sized scratch buffer exists
//! anywhere. A buffer too small for a command yields
//! [`Error::BufferTooSmall`] with the exact size needed, never a panic.
//!
//! Sizing rule: `frame_buf` must hold the largest request plus the largest
//! response of any command used. For `flash` that is
//! `(page_size + 2 + 3) + (12 + 3)` bytes — 320 covers every legal
//! geometry.
//!
//! # Retries
//!
//! The wire protocol is idempotent by design, so a lost or mangled
//! exchange is repaired by re-sending the identical request: transport
//! errors, undecodable responses and `ST_BAD_FRAME` (device saw a mangled
//! request) are retried up to [`RETRY_BUDGET`] times after the initial
//! attempt. Any other device status is a fact about the device, not the
//! wire, and is returned immediately as [`Error::Device`].

use crate::error::Error;
use crate::frame::{
    self, CMD_BOOT, CMD_ECHO, CMD_ERASE_APP, CMD_INFO, CMD_VERIFY, CMD_WRITE_PAGE, ECHO_MAX,
    ERASE_MAGIC, FRAME_OVERHEAD, PROTO_VERSION, RSP_FLAG, ST_BAD_FRAME, ST_OK,
};
use crate::image::Image;
use crate::transport::Transport;

/// Retries after the initial attempt for retryable failures (transport
/// error, undecodable response, `ST_BAD_FRAME`): at most `1 + RETRY_BUDGET`
/// requests hit the wire per exchange.
pub const RETRY_BUDGET: u8 = 3;

/// Response payload of `INFO`: ST + proto + `bl_version` + `id[4]` +
/// `page_size` LE + `app_pages` LE + `app_valid`.
const INFO_RSP_PAYLOAD: usize = 12;
/// Response payload of every status-only reply.
const ST_RSP_PAYLOAD: usize = 1;

/// What a device reported about itself in response to `INFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Protocol version the device speaks.
    pub proto: u8,
    /// Bootloader version.
    pub bl_version: u8,
    /// Device identity bytes (port-defined).
    pub device_id: [u8; 4],
    /// Protocol page size in bytes — the `WRITE_PAGE` transfer unit. This
    /// is the wire's unit, not necessarily the device's physical flash
    /// page (a port may coalesce protocol pages into larger program
    /// units; e.g. the RP2350 port maps two 128-byte protocol pages onto
    /// one 256-byte flash page).
    pub page_size: u16,
    /// Number of pages in the app region.
    pub app_pages: u16,
    /// Whether the currently flashed application passes the boot gate.
    pub app_valid: bool,
}

/// A host-side update session over a [`Transport`].
///
/// `no_std`, zero-alloc: the caller provides both the transport and the
/// one frame buffer every exchange runs through.
#[derive(Debug)]
pub struct Session<'b, T: Transport> {
    transport: T,
    buf: &'b mut [u8],
}

impl<'b, T: Transport> Session<'b, T> {
    /// Create a session over `transport` using `frame_buf` for every wire
    /// exchange (see the module docs for the sizing rule).
    pub fn new(transport: T, frame_buf: &'b mut [u8]) -> Self {
        Self { transport, buf: frame_buf }
    }

    /// Query the device's identity and geometry.
    ///
    /// # Errors
    ///
    /// [`Error::Transport`] / [`Error::BadFrame`] / [`Error::Device`] per
    /// the retry rules; [`Error::BufferTooSmall`] if `frame_buf` cannot
    /// hold the exchange.
    pub fn info(&mut self) -> Result<DeviceInfo, Error<T::Err>> {
        let rest = self.transact(CMD_INFO, &[], INFO_RSP_PAYLOAD)?;
        parse_info(rest)
    }

    /// Link smoke test: send `data`, require the device to echo it back.
    ///
    /// # Errors
    ///
    /// [`Error::PayloadTooLarge`] if `data` exceeds [`ECHO_MAX`];
    /// [`Error::BadFrame`] if the device echoes different bytes; otherwise
    /// as [`Session::info`].
    pub fn echo(&mut self, data: &[u8]) -> Result<(), Error<T::Err>> {
        if data.len() > ECHO_MAX {
            return Err(Error::PayloadTooLarge { len: data.len() });
        }
        // Echo reply payload: ST + the echoed bytes.
        let cap = data.len().wrapping_add(ST_RSP_PAYLOAD); // <= 17
        let rest = self.transact(CMD_ECHO, data, cap)?;
        if rest == data {
            Ok(())
        } else {
            Err(Error::BadFrame)
        }
    }

    /// Ask the device to boot the application.
    ///
    /// Note: the final ACK is inherently ambiguous — the device may boot
    /// successfully while its OK reply is lost on the wire, so treat
    /// post-boot silence or a transport error here as expected rather than
    /// as proof the boot failed.
    ///
    /// # Errors
    ///
    /// [`Error::Device`]`(ST_NO_APP)` if the boot gate rejects the flashed
    /// image; otherwise as [`Session::info`].
    pub fn boot(&mut self) -> Result<(), Error<T::Err>> {
        self.transact(CMD_BOOT, &[], ST_RSP_PAYLOAD)?;
        Ok(())
    }

    /// Full update: `INFO` → protocol gate → geometry gate → `ERASE_APP`
    /// → one `WRITE_PAGE` per [`Image::pages`] index → `VERIFY`.
    ///
    /// `progress` is called with `(pages_done, pages_total)` — once up
    /// front with `(0, total)` and once after each written page.
    ///
    /// # Errors
    ///
    /// [`Error::ProtocolVersion`] if the device speaks a different
    /// protocol; [`Error::BadGeometry`] (carrying the **device's**
    /// geometry) if `img` was built for a different page size or page
    /// count; otherwise as [`Session::info`].
    pub fn flash(
        &mut self,
        img: &Image<'_>,
        progress: &mut dyn FnMut(u16, u16),
    ) -> Result<(), Error<T::Err>> {
        let info = self.info()?;
        if info.proto != PROTO_VERSION {
            return Err(Error::ProtocolVersion { device: info.proto });
        }
        if info.page_size != img.page_size() || info.app_pages != img.page_count() {
            // The image was validated against a geometry at construction;
            // it just isn't this device's. Report what the device has.
            return Err(Error::BadGeometry {
                page_size: info.page_size,
                app_pages: info.app_pages,
            });
        }

        self.transact(CMD_ERASE_APP, &ERASE_MAGIC, ST_RSP_PAYLOAD)?;

        // pages() yields at most app_pages <= u16::MAX indices, so the
        // conversion is lossless; unwrap_or keeps this total.
        let total = u16::try_from(img.pages().count()).unwrap_or(u16::MAX);
        let mut done: u16 = 0;
        progress(done, total);
        for index in img.pages() {
            self.write_page(img, index)?;
            done = done.saturating_add(1);
            progress(done, total);
        }

        let len = img.len().to_le_bytes();
        let crc = img.crc32().to_le_bytes();
        let verify = [
            len[0], len[1], len[2], len[3],
            crc[0], crc[1], crc[2], crc[3],
        ];
        self.transact(CMD_VERIFY, &verify, ST_RSP_PAYLOAD)?;
        Ok(())
    }

    /// Consume the session, giving the transport back.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// One `WRITE_PAGE` exchange; the page is rendered by
    /// [`Image::page_into`] directly into the frame buffer's payload
    /// bytes — buffer discipline: no second page-sized buffer.
    fn write_page(&mut self, img: &Image<'_>, index: u16) -> Result<(), Error<T::Err>> {
        let ps = usize::from(img.page_size());
        // Frame: CMD LEN idx_lo idx_hi page[ps] CRC. ps <= 250 by Image's
        // geometry validation, so none of these sums can wrap.
        let payload_len = ps.wrapping_add(2);
        let tx_len = payload_len.wrapping_add(FRAME_OVERHEAD);
        let rx_len = ST_RSP_PAYLOAD.wrapping_add(FRAME_OVERHEAD);
        let (tx, rx) = split_windows(self.buf, tx_len, rx_len)?;

        // payload_len <= 252 fits u8; the error arm keeps this total.
        let len_byte =
            u8::try_from(payload_len).map_err(|_| Error::PayloadTooLarge { len: payload_len })?;
        let idx = index.to_le_bytes();
        {
            let Some((header, rest)) = tx.split_first_chunk_mut::<4>() else {
                return Err(Error::BufferTooSmall { needed: tx_len }); // unreachable: tx_len >= 5
            };
            *header = [CMD_WRITE_PAGE, len_byte, idx[0], idx[1]];
            let Some(page) = rest.get_mut(..ps) else {
                return Err(Error::BufferTooSmall { needed: tx_len }); // unreachable, ditto
            };
            img.page_into(index, page).map_err(Error::widen)?;
        }
        // CRC-8 over CMD LEN idx page — everything but the last byte. The
        // error arms are unreachable (tx.len() == tx_len >= 5) but keep the
        // path total without a panic.
        let crc_at = tx_len.wrapping_sub(1);
        let too_small = || Error::BufferTooSmall { needed: tx_len };
        let crc = frame::crc8(tx.get(..crc_at).ok_or_else(too_small)?);
        *tx.get_mut(crc_at).ok_or_else(too_small)? = crc;

        exchange(&mut self.transport, tx, rx, CMD_WRITE_PAGE | RSP_FLAG)?;
        Ok(())
    }

    /// Encode `cmd` + `payload`, run the retried exchange, return the
    /// response payload after the (validated-OK) status byte.
    fn transact(
        &mut self,
        cmd: u8,
        payload: &[u8],
        rsp_payload_cap: usize,
    ) -> Result<&[u8], Error<T::Err>> {
        let tx_len = payload
            .len()
            .checked_add(FRAME_OVERHEAD)
            .ok_or(Error::PayloadTooLarge { len: payload.len() })?;
        // rsp_payload_cap is an internal constant <= 17: cannot wrap.
        let rx_len = rsp_payload_cap.wrapping_add(FRAME_OVERHEAD);
        let (tx, rx) = split_windows(self.buf, tx_len, rx_len)?;
        frame::encode(cmd, payload, tx).map_err(Error::widen)?;

        let rlen = exchange(&mut self.transport, tx, rx, cmd | RSP_FLAG)?;
        // Decode once more on the immutable view to hand the payload out;
        // `exchange` already validated this exact byte range, so the error
        // arms are unreachable but keep the path total.
        let raw = rx.get(..rlen).ok_or(Error::BadFrame)?;
        let decoded = frame::decode_padded(raw).map_err(Error::widen)?;
        let (_st, rest) = decoded.payload.split_first().ok_or(Error::BadFrame)?;
        Ok(rest)
    }
}

/// Split `buf` into a `tx_len` transmit window and an `rx_len` receive
/// window, or say exactly how many bytes the caller must provide.
fn split_windows<E>(
    buf: &mut [u8],
    tx_len: usize,
    rx_len: usize,
) -> Result<(&mut [u8], &mut [u8]), Error<E>> {
    let needed = tx_len.saturating_add(rx_len);
    let too_small = Error::BufferTooSmall { needed };
    let Some(region) = buf.get_mut(..needed) else {
        return Err(too_small);
    };
    // needed >= tx_len, so this split cannot fail; the error arm keeps the
    // function total.
    region.split_at_mut_checked(tx_len).ok_or(too_small)
}

/// Why a received response cannot be accepted.
enum Reject {
    /// Not a decodable frame / wrong command / empty payload: wire damage.
    Mangled,
    /// Device decoded garbage on its side (`ST_BAD_FRAME`): wire damage.
    DeviceBadFrame,
    /// Any other non-OK status: a fact about the device, final.
    Fatal(u8),
}

/// Validate `raw` as the response to `expect_cmd`.
fn validate(raw: &[u8], expect_cmd: u8) -> Result<(), Reject> {
    let Ok(decoded) = frame::decode_padded(raw) else {
        return Err(Reject::Mangled);
    };
    if decoded.cmd != expect_cmd {
        return Err(Reject::Mangled);
    }
    let Some((&st, _)) = decoded.payload.split_first() else {
        return Err(Reject::Mangled); // the device always sends >= 1 byte
    };
    match st {
        ST_OK => Ok(()),
        ST_BAD_FRAME => Err(Reject::DeviceBadFrame),
        other => Err(Reject::Fatal(other)),
    }
}

/// Run one request with bounded retries; on success the first `Ok(n)`
/// bytes of `rx` hold a validated OK response frame (plus filler).
fn exchange<T: Transport>(
    transport: &mut T,
    tx: &[u8],
    rx: &mut [u8],
    expect_cmd: u8,
) -> Result<usize, Error<T::Err>> {
    let mut last = Error::BadFrame;
    for _attempt in 0..=RETRY_BUDGET {
        match transport.request(tx, rx) {
            Err(e) => last = Error::Transport(e),
            Ok(n) => {
                // Clamp: a transport reporting more than it was given room
                // for is treated as a mangled response, not trusted.
                let rlen = n.min(rx.len());
                let raw = rx.get(..rlen).unwrap_or(&[]);
                match validate(raw, expect_cmd) {
                    Ok(()) => return Ok(rlen),
                    Err(Reject::Mangled) => last = Error::BadFrame,
                    Err(Reject::DeviceBadFrame) => last = Error::Device(ST_BAD_FRAME),
                    Err(Reject::Fatal(st)) => return Err(Error::Device(st)),
                }
            }
        }
    }
    Err(last)
}

/// Kani model-checking harnesses. Compiled only under `cargo kani`
/// (`cfg(kani)`), never in normal builds, tests or clippy runs; proof text
/// may therefore panic — a panic here *is* the failed assertion.
///
/// The harnessed seams are the session's pure response-validation
/// functions, [`validate`] and [`parse_info`] — everything `exchange` and
/// `transact` do with received bytes flows through them, so their totality
/// is the session's "arbitrary response bytes never panic" property
/// without dragging a symbolic [`Transport`] into the solver.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Response bound: 24 bytes covers both real response shapes (status-
    /// only, 4 bytes; INFO, 15 bytes) plus 0xFF filler and truncated /
    /// over-long junk around them. `validate` has no response-length-
    /// dependent control flow beyond what `decode_padded` (proven to 40
    /// bytes in `frame::proofs`) already has, so a tighter bound keeps the
    /// double CRC fold cheap.
    const RSP_MAX: usize = 24;

    /// `validate` never panics for arbitrary response bytes and an
    /// arbitrary expected command; acceptance implies exactly the
    /// documented contract.
    #[kani::proof]
    #[kani::unwind(32)]
    fn validate_total() {
        let raw: [u8; RSP_MAX] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= RSP_MAX);
        let Some(view) = raw.get(..len) else { return };
        let expect_cmd: u8 = kani::any();

        match validate(view, expect_cmd) {
            Ok(()) => {
                let Ok(f) = frame::decode_padded(view) else {
                    panic!("validate accepted an undecodable response")
                };
                assert!(f.cmd == expect_cmd);
                assert!(f.payload.first() == Some(&ST_OK));
            }
            // Any rejection is legal for junk; totality is the property.
            Err(Reject::Mangled | Reject::DeviceBadFrame) => {}
            Err(Reject::Fatal(st)) => assert!(st != ST_OK && st != ST_BAD_FRAME),
        }
    }

    /// `parse_info` never panics for arbitrary bytes; it accepts exactly
    /// the 11-byte post-status INFO payload, and every field decodes as
    /// documented: byte-for-byte proto / bl_version / device_id, LE u16
    /// page_size and app_pages, app_valid == (byte != 0).
    #[kani::proof]
    #[kani::unwind(20)]
    fn parse_info_total() {
        let raw: [u8; 16] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 16);
        let Some(view) = raw.get(..len) else { return };

        match parse_info::<core::convert::Infallible>(view) {
            Ok(info) => {
                assert!(len == INFO_RSP_PAYLOAD - 1);
                let [proto, bl, d0, d1, d2, d3, ps_lo, ps_hi, ap_lo, ap_hi, valid] = *view
                else {
                    panic!("Ok implies an 11-byte payload")
                };
                assert!(info.proto == proto);
                assert!(info.bl_version == bl);
                assert!(info.device_id == [d0, d1, d2, d3]);
                assert!(info.page_size == u16::from_le_bytes([ps_lo, ps_hi]));
                assert!(info.app_pages == u16::from_le_bytes([ap_lo, ap_hi]));
                assert!(info.app_valid == (valid != 0));
            }
            Err(Error::BadFrame) => assert!(len != INFO_RSP_PAYLOAD - 1),
            Err(_) => panic!("parse_info yields only BadFrame"),
        }
    }
}

/// Parse the INFO response payload (after the status byte).
fn parse_info<E>(rest: &[u8]) -> Result<DeviceInfo, Error<E>> {
    let [proto, bl_version, d0, d1, d2, d3, ps_lo, ps_hi, ap_lo, ap_hi, valid] = *rest else {
        return Err(Error::BadFrame);
    };
    Ok(DeviceInfo {
        proto,
        bl_version,
        device_id: [d0, d1, d2, d3],
        page_size: u16::from_le_bytes([ps_lo, ps_hi]),
        app_pages: u16::from_le_bytes([ap_lo, ap_hi]),
        app_valid: valid != 0,
    })
}
