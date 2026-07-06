// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Borrowed-image handling: footer construction and page iteration without
//! allocation.
//!
//! An [`Image`] borrows the raw application binary and knows the target's
//! page geometry. The last 16 bytes of the app region hold the footer the
//! device's boot gate validates: magic `"OTAU"`, image length (u32 LE),
//! CRC-32 (u32 LE), then four `0xFF` bytes.
//!
//! # The page seam
//!
//! Flash pages that only pad the image cannot be borrowed from the input
//! (padding bytes and the footer do not exist in the caller's buffer), so a
//! borrowed-slice iterator cannot yield every page as a fixed page-size
//! window without allocating. Instead the seam is split in two and stays
//! uniform for the consumer:
//!
//! * [`Image::pages`] iterates the **indices** of pages that must be
//!   written: pages holding at least one non-`0xFF` byte, plus — always —
//!   the footer page. All-`0xFF` pages are skipped because erased flash
//!   already reads `0xFF`.
//! * [`Image::page_into`] renders **any** page (data, padding, footer) into
//!   a caller-provided buffer, exactly `page_size` bytes.
//!
//! A no-alloc consumer therefore needs one page-sized buffer (or a window
//! into its frame buffer) and a single code path:
//!
//! ```
//! # use updater_core::image::Image;
//! let app = [0x42u8; 20];
//! let img = Image::from_bin(&app, 16, 4)?;
//! let mut page = [0u8; 16];
//! for index in img.pages() {
//!     img.page_into(index, &mut page)?;
//!     // send WRITE_PAGE(index, page) ...
//! }
//! # Ok::<(), updater_core::Error>(())
//! ```

use crate::error::Error;
use crate::frame::PAYLOAD_MAX;

/// Size of the image footer at the end of the app region.
pub const FOOTER_LEN: usize = 16;
/// Footer magic (`"OTAU"`).
pub const FOOTER_MAGIC: [u8; 4] = *b"OTAU";
/// Largest usable page: a `WRITE_PAGE` payload is `page_size + 2` bytes and
/// must fit one frame.
pub const PAGE_SIZE_MAX: usize = PAYLOAD_MAX - 2;

/// CRC-32 (IEEE 802.3, reflected, init `0xFFFF_FFFF`, final XOR) over
/// `data`. Mirrors the device's `upd_crc32_*` and the footer field.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8u8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// An application image borrowed from the caller, bound to a page geometry.
///
/// Construction validates that the image fits the region with room for the
/// footer; afterwards every accessor is total.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    data: &'a [u8],
    page_size: u16,
    app_pages: u16,
    len: u32,
    crc: u32,
}

impl<'a> Image<'a> {
    /// Wrap a raw binary for a device with `app_pages` pages of `page_size`
    /// bytes.
    ///
    /// # Errors
    ///
    /// [`Error::BadGeometry`] if `page_size` or `app_pages` is zero, if a
    /// page is smaller than the 16-byte footer, or if `page_size` exceeds
    /// [`PAGE_SIZE_MAX`]; [`Error::ImageTooLarge`] if `bytes` exceeds
    /// `page_size * app_pages - 16`.
    pub fn from_bin(bytes: &'a [u8], page_size: u16, app_pages: u16) -> Result<Self, Error> {
        let bad_geometry = Error::BadGeometry { page_size, app_pages };
        let ps = usize::from(page_size);
        if app_pages == 0 || !(FOOTER_LEN..=PAGE_SIZE_MAX).contains(&ps) {
            return Err(bad_geometry);
        }
        let region = ps.checked_mul(usize::from(app_pages)).ok_or(bad_geometry)?;
        let capacity = region.checked_sub(FOOTER_LEN).ok_or(bad_geometry)?;
        if bytes.len() > capacity {
            return Err(Error::ImageTooLarge { len: bytes.len(), capacity });
        }
        // capacity < 2^32 for any u16 geometry, so this cannot fail; the
        // error arm keeps construction total.
        let len = u32::try_from(bytes.len())
            .map_err(|_| Error::ImageTooLarge { len: bytes.len(), capacity })?;
        Ok(Self { data: bytes, page_size, app_pages, len, crc: crc32(bytes) })
    }

    /// Pre-footer payload length in bytes (the `length` field of the
    /// footer).
    #[must_use]
    pub fn len(&self) -> u32 {
        self.len
    }

    /// `true` if the image carries no payload bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// CRC-32 over the payload (the `crc32` field of the footer).
    #[must_use]
    pub fn crc32(&self) -> u32 {
        self.crc
    }

    /// The raw payload this image borrows.
    #[must_use]
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Page size in bytes.
    #[must_use]
    pub fn page_size(&self) -> u16 {
        self.page_size
    }

    /// Total number of pages in the app region.
    #[must_use]
    pub fn page_count(&self) -> u16 {
        self.app_pages
    }

    /// Index of the page holding the footer: always the last page of the
    /// region (a page holds at least [`FOOTER_LEN`] bytes by construction).
    #[must_use]
    pub fn footer_page_index(&self) -> u16 {
        self.app_pages.wrapping_sub(1) // app_pages >= 1 by construction
    }

    /// The 16-byte footer: `"OTAU"`, length LE, CRC-32 LE, `FF FF FF FF`.
    /// Returned by value — it lives on the caller's stack.
    #[must_use]
    pub fn footer(&self) -> [u8; FOOTER_LEN] {
        let l = self.len.to_le_bytes();
        let c = self.crc.to_le_bytes();
        [
            FOOTER_MAGIC[0], FOOTER_MAGIC[1], FOOTER_MAGIC[2], FOOTER_MAGIC[3],
            l[0], l[1], l[2], l[3],
            c[0], c[1], c[2], c[3],
            0xFF, 0xFF, 0xFF, 0xFF,
        ]
    }

    /// Render page `index` into `out`: image bytes where the page overlaps
    /// the payload, `0xFF` padding elsewhere, and the footer overlaid on the
    /// last page. Exactly `page_size` bytes of `out` are written; any excess
    /// is left untouched.
    ///
    /// # Buffer contract
    ///
    /// On any error, `out` is left entirely untouched — no partial render is
    /// observable. This guarantee is verified by the crate's Kani harnesses.
    ///
    /// # Errors
    ///
    /// [`Error::PageOutOfRange`] if `index >= page_count()`;
    /// [`Error::BufferTooSmall`] if `out` is shorter than `page_size`.
    pub fn page_into(&self, index: u16, out: &mut [u8]) -> Result<(), Error> {
        if index >= self.app_pages {
            return Err(Error::PageOutOfRange { index });
        }
        let ps = usize::from(self.page_size);
        let Some(page) = out.get_mut(..ps) else {
            return Err(Error::BufferTooSmall { needed: ps });
        };

        // index < app_pages and ps <= PAGE_SIZE_MAX, so start fits usize.
        let start = usize::from(index).wrapping_mul(ps);
        let payload_tail = self.data.get(start..).unwrap_or(&[]);
        let padded = payload_tail.iter().chain(core::iter::repeat(&0xFF));
        for (dst, &src) in page.iter_mut().zip(padded) {
            *dst = src;
        }

        if index == self.footer_page_index() {
            let offset = ps.wrapping_sub(FOOTER_LEN); // ps >= FOOTER_LEN
            for (dst, src) in page.iter_mut().skip(offset).zip(self.footer()) {
                *dst = src;
            }
        }
        Ok(())
    }

    /// Iterate the indices of pages that must be written: every page with a
    /// non-`0xFF` byte plus the footer page. All-`0xFF` pages are skipped —
    /// erased flash already holds that value.
    #[must_use]
    pub fn pages(&self) -> Pages<'a> {
        Pages { img: *self, cursor: 0 }
    }

    /// Does page `index` need a `WRITE_PAGE`, i.e. is it the footer page or
    /// does it contain a non-`0xFF` payload byte?
    fn must_write(&self, index: u16) -> bool {
        if index == self.footer_page_index() {
            return true;
        }
        let ps = usize::from(self.page_size);
        let start = usize::from(index).wrapping_mul(ps); // bounded, see page_into
        let end = start.wrapping_add(ps).min(self.data.len());
        self.data
            .get(start..end)
            .is_some_and(|window| window.iter().any(|&b| b != 0xFF))
    }
}

/// Iterator over the page indices an update must write; see
/// [`Image::pages`].
#[derive(Debug, Clone)]
pub struct Pages<'a> {
    img: Image<'a>,
    cursor: u32,
}

impl Iterator for Pages<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        while self.cursor < u32::from(self.img.app_pages) {
            // cursor < app_pages <= u16::MAX, so the conversion is lossless;
            // `.ok()?` keeps this total without a panic path.
            let index = u16::try_from(self.cursor).ok()?;
            self.cursor = self.cursor.wrapping_add(1);
            if self.img.must_write(index) {
                return Some(index);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = u32::from(self.img.app_pages).saturating_sub(self.cursor);
        // If any page remains, the footer page (always last, always yielded)
        // is among them, so the lower bound is 1.
        (usize::from(remaining > 0), usize::try_from(remaining).ok())
    }
}

impl core::iter::FusedIterator for Pages<'_> {}

/// Kani model-checking harnesses. Compiled only under `cargo kani`
/// (`cfg(kani)`), never in normal builds, tests or clippy runs; proof text
/// may therefore panic — a panic here *is* the failed assertion.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Data bound for the image harnesses. 24 bytes spans more than one
    /// page at the smallest legal page size (16), so the harnesses reach
    /// data pages, a data/padding boundary inside a page, pure padding
    /// pages and the footer overlay. The CRC-32 fold (8 branchy steps per
    /// byte) dominates solver cost, so the bound is kept just large enough
    /// for that coverage.
    const DATA_MAX: usize = 24;

    /// `from_bin` never panics for arbitrary geometry (full u16 range for
    /// both parameters) and bounded data; acceptance implies the documented
    /// invariants.
    #[kani::proof]
    #[kani::unwind(32)]
    fn from_bin_total() {
        let data: [u8; DATA_MAX] = kani::any();
        let dlen: usize = kani::any();
        kani::assume(dlen <= DATA_MAX);
        let Some(bytes) = data.get(..dlen) else { return };
        let page_size: u16 = kani::any();
        let app_pages: u16 = kani::any();

        match Image::from_bin(bytes, page_size, app_pages) {
            Ok(img) => {
                assert!(app_pages >= 1);
                assert!((FOOTER_LEN..=PAGE_SIZE_MAX).contains(&usize::from(page_size)));
                assert!(img.len() as usize == dlen);
                assert!(img.page_size() == page_size);
                assert!(img.page_count() == app_pages);
                assert!(img.footer_page_index() == app_pages.wrapping_sub(1));
                assert!(dlen <= usize::from(page_size) * usize::from(app_pages) - FOOTER_LEN);
            }
            Err(Error::BadGeometry { .. } | Error::ImageTooLarge { .. }) => {}
            Err(_) => panic!("from_bin yields only BadGeometry / ImageTooLarge"),
        }
    }

    /// `page_into` never panics for an arbitrary valid image, an arbitrary
    /// index (in and out of range — full u16) and an arbitrary buffer size;
    /// errors are exactly the documented ones and never touch the buffer,
    /// success never touches bytes past `page_size`.
    ///
    /// Geometry bound: page_size 16..=20, app_pages 1..=3 — enough for
    /// index-out-of-range, footer overlay at both ps == FOOTER_LEN (footer
    /// fills the page) and ps > FOOTER_LEN (data survives in front of it),
    /// and multi-page start offsets. The rendering loop is uniform per
    /// byte, so larger pages add unrolling, not branches.
    #[kani::proof]
    #[kani::unwind(32)]
    fn page_into_total() {
        let data: [u8; DATA_MAX] = kani::any();
        let dlen: usize = kani::any();
        kani::assume(dlen <= DATA_MAX);
        let Some(bytes) = data.get(..dlen) else { return };
        let page_size: u16 = kani::any();
        kani::assume((16..=20).contains(&page_size));
        let app_pages: u16 = kani::any();
        kani::assume((1..=3).contains(&app_pages));
        let Ok(img) = Image::from_bin(bytes, page_size, app_pages) else {
            return; // over-capacity combinations are from_bin_total's job
        };

        let index: u16 = kani::any();
        const OUT_MAX: usize = 24; // > max page_size: covers excess bytes
        let before: [u8; OUT_MAX] = kani::any();
        let mut out = before;
        let out_len: usize = kani::any();
        kani::assume(out_len <= OUT_MAX);
        let Some(window) = out.get_mut(..out_len) else { return };

        let ps = usize::from(page_size);
        match img.page_into(index, window) {
            Ok(()) => {
                assert!(index < app_pages);
                assert!(out_len >= ps);
                // Bytes past page_size are untouched.
                let mut i = ps;
                while i < OUT_MAX {
                    assert!(out[i] == before[i]);
                    i += 1;
                }
            }
            Err(Error::PageOutOfRange { index: i }) => {
                assert!(i == index && index >= app_pages);
                assert!(out == before);
            }
            Err(Error::BufferTooSmall { needed }) => {
                assert!(needed == ps && out_len < ps && index < app_pages);
                assert!(out == before);
            }
            Err(_) => panic!("page_into yields only PageOutOfRange / BufferTooSmall"),
        }
    }

    /// `pages()` never panics and yields only in-range indices, the footer
    /// page always among them.
    ///
    /// Every yielded index therefore *renders*: `page_into_total` proves
    /// index < app_pages with a >= page_size buffer always returns Ok, and
    /// this harness proves every yield satisfies that precondition — the
    /// render claim is their composition. (An in-loop `page_into` call was
    /// tried first; the pages-scan x render product blew the time budget
    /// even at minimal bounds, and it re-proves nothing `page_into_total`
    /// does not already cover.)
    ///
    /// Geometry: concrete ps = 16 (minimum) and ap = 3, all data bytes and
    /// the data length symbolic — covers a must-write data page, skippable
    /// all-0xFF pages, the always-yielded footer page and a data/padding
    /// boundary inside a page; a second block pins the ap = 1 edge (footer
    /// page is page 0 and the only page).
    ///
    /// Bound story (this harness fought back hard): the cost driver is the
    /// unwind product — the for-loop, the iterator's internal scan and the
    /// per-page window scans each get unrolled `unwind` times and the
    /// copies MULTIPLY, with every symbolic loop guard (from a symbolic
    /// data length) forcing full unrolling. Symbolic geometry, then
    /// concrete geometry with symbolic length, then data <= 4 all blew the
    /// budget. What verifies fast: concrete data LENGTH with every data
    /// BYTE symbolic — still a universally quantified statement over all
    /// 2^32 data contents, which is what the yield-set logic actually
    /// branches on (byte == 0xFF or not). Symbolic lengths/geometry are
    /// carried by `from_bin_total`, `page_into_total` and the proptest
    /// `image_pages_render_consistently` (64 pages, 1 KiB data).
    #[kani::proof]
    #[kani::unwind(10)]
    fn pages_yield_in_range_and_footer() {
        let data: [u8; 4] = kani::any(); // partial page 0; pages 1..3 all-0xFF
        let app_pages: u16 = 3;
        let Ok(img) = Image::from_bin(&data, 16, app_pages) else {
            panic!("16 x 3 geometry must accept 4 data bytes")
        };

        // Explicit next() calls instead of a for loop: at most app_pages
        // yields exist, and unrolling by hand stops the unwinder from
        // multiplying loop copies (for-loop x iterator-internal scan).
        let mut it = img.pages();
        let yields = [it.next(), it.next(), it.next()];
        assert!(it.next().is_none(), "no more than app_pages yields");
        let mut saw_footer = false;
        for y in yields {
            if let Some(index) = y {
                assert!(index < app_pages);
                if index == img.footer_page_index() {
                    saw_footer = true;
                }
            }
        }
        assert!(saw_footer, "the footer page must always be written");

        // ap = 1 edge: the single page is the footer page and must be the
        // one and only yield (capacity 0 forces an empty image).
        let Ok(single) = Image::from_bin(&[], 16, 1) else {
            panic!("16 x 1 geometry must accept an empty image")
        };
        let mut it = single.pages();
        assert!(it.next() == Some(0));
        assert!(it.next().is_none());
    }
}
