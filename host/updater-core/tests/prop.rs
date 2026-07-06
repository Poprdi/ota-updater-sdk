// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Proptest property tests for the frame codec and image handling.
//!
//! These complement (not replace) the hermetic hand-rolled property tests
//! in `tests/golden.rs`: proptest brings shrinking and a much wider random
//! search, golden.rs keeps a fully offline safety net. Under Kani the same
//! properties are *proven* for bounded inputs (`src/*.rs`, `mod proofs`);
//! here they are *sampled* across the full input ranges the proofs bound.

use proptest::prelude::*;
use updater_core::frame::{self, FRAME_OVERHEAD, PAYLOAD_MAX};
use updater_core::image::{self, Image, FOOTER_LEN, FOOTER_MAGIC};
use updater_core::Error;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// encode → decode round-trips cmd and payload over the FULL payload
    /// range 0..=252, and the returned length is exact.
    #[test]
    fn roundtrip_full_payload_range(
        cmd: u8,
        payload in proptest::collection::vec(any::<u8>(), 0..=PAYLOAD_MAX),
    ) {
        let mut out = vec![0u8; payload.len() + FRAME_OVERHEAD];
        let n = frame::encode(cmd, &payload, &mut out).expect("payload fits a frame");
        prop_assert_eq!(n, payload.len() + FRAME_OVERHEAD);
        let f = frame::decode(&out[..n]).expect("own frames must decode");
        prop_assert_eq!(f.cmd, cmd);
        prop_assert_eq!(f.payload, &payload[..]);
    }

    /// Both decoders are total over arbitrary junk up to 300 bytes (past
    /// the 255-byte wire maximum), and whenever `decode` accepts a buffer,
    /// `decode_padded` accepts it identically (k = 0 filler case).
    #[test]
    fn decode_total_on_junk(raw in proptest::collection::vec(any::<u8>(), 0..=300)) {
        let plain = frame::decode(&raw);      // must not panic
        let padded = frame::decode_padded(&raw); // must not panic
        if let Ok(f) = plain {
            let g = padded.expect("decode_padded accepts every exact frame");
            prop_assert_eq!(f, g);
        }
    }

    /// decode_padded ≡ decode + 0xFF filler: a frame followed by k 0xFF
    /// bytes is accepted with the identical result, and corrupting any one
    /// filler byte to a non-0xFF value is rejected (desync, not filler).
    #[test]
    fn decode_padded_is_decode_plus_filler(
        cmd: u8,
        payload in proptest::collection::vec(any::<u8>(), 0..=PAYLOAD_MAX),
        filler in 0usize..=48,
        corrupt_at in any::<prop::sample::Index>(),
        junk in any::<u8>().prop_filter("filler corruption must differ from 0xFF", |&b| b != 0xFF),
    ) {
        let n = payload.len() + FRAME_OVERHEAD;
        let mut buf = vec![0u8; n + filler];
        frame::encode(cmd, &payload, &mut buf).expect("payload fits a frame");
        buf[n..].fill(0xFF);

        let f = frame::decode_padded(&buf).expect("frame + 0xFF filler must decode");
        let g = frame::decode(&buf[..n]).expect("bare frame must decode");
        prop_assert_eq!(f, g);
        prop_assert_eq!(f.cmd, cmd);
        prop_assert_eq!(f.payload, &payload[..]);

        if filler > 0 {
            let pos = n + corrupt_at.index(filler);
            buf[pos] = junk;
            prop_assert!(frame::decode_padded(&buf).is_err());
        }
    }

    /// CRC-8 detects every single-bit error: flipping any one bit of an
    /// encoded frame makes `decode` reject it.
    #[test]
    fn single_bit_flip_rejected(
        cmd: u8,
        payload in proptest::collection::vec(any::<u8>(), 0..=PAYLOAD_MAX),
        bit in any::<prop::sample::Index>(),
    ) {
        let n = payload.len() + FRAME_OVERHEAD;
        let mut buf = vec![0u8; n];
        frame::encode(cmd, &payload, &mut buf).expect("payload fits a frame");
        let bit = bit.index(n * 8);
        buf[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(frame::decode(&buf).is_err());
    }
}

proptest! {
    // The image property rebuilds whole flash regions (up to 16 KiB), so
    // fewer, heavier cases.
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// from_bin/page_into consistency: for any accepted image, rendering
    /// ALL pages reconstructs exactly `data ++ 0xFF padding ++ footer`;
    /// `pages()` yields precisely the must-write pages (non-0xFF content or
    /// the footer page) and every yielded page renders; out-of-range
    /// indices and short buffers fail typed.
    #[test]
    fn image_pages_render_consistently(
        data in proptest::collection::vec(
            // Bias towards 0xFF so skippable all-0xFF pages actually occur.
            prop_oneof![2 => Just(0xFFu8), 1 => any::<u8>()],
            0..=1024,
        ),
        page_size in 16u16..=250,
        app_pages in 1u16..=64,
    ) {
        let ps = usize::from(page_size);
        let region = ps * usize::from(app_pages);
        let capacity = region - FOOTER_LEN;

        let img = match Image::from_bin(&data, page_size, app_pages) {
            Ok(img) => img,
            Err(Error::ImageTooLarge { len, capacity: cap }) => {
                prop_assert_eq!(len, data.len());
                prop_assert_eq!(cap, capacity);
                prop_assert!(data.len() > capacity);
                return Ok(());
            }
            Err(e) => return Err(TestCaseError::fail(format!(
                "geometry in the valid range must not fail: {e:?}"
            ))),
        };
        prop_assert_eq!(img.len() as usize, data.len());
        prop_assert_eq!(img.crc32(), image::crc32(&data));

        // Render every page and rebuild the region.
        let mut rendered = Vec::with_capacity(region);
        let mut page = vec![0u8; ps];
        for index in 0..app_pages {
            img.page_into(index, &mut page).expect("in-range page must render");
            rendered.extend_from_slice(&page);
        }

        // data prefix ++ 0xFF fill ++ 16-byte footer, byte for byte.
        prop_assert_eq!(&rendered[..data.len()], &data[..]);
        let footer_at = region - FOOTER_LEN;
        prop_assert!(rendered[data.len()..footer_at].iter().all(|&b| b == 0xFF));
        prop_assert_eq!(&rendered[footer_at..], &img.footer()[..]);
        prop_assert_eq!(&img.footer()[..4], &FOOTER_MAGIC[..]);

        // pages() yields exactly the must-write set, in order.
        let yielded: Vec<u16> = img.pages().collect();
        prop_assert!(yielded.windows(2).all(|w| w[0] < w[1]));
        for index in 0..app_pages {
            let chunk = &rendered[usize::from(index) * ps..][..ps];
            let must_write =
                index == app_pages - 1 || chunk.iter().any(|&b| b != 0xFF);
            prop_assert_eq!(
                yielded.contains(&index),
                must_write,
                "page {} must_write mismatch", index
            );
        }

        // Out-of-range index and short buffer fail typed, per the docs.
        // (prop_assert! stringifies its expression into a format string, so
        // brace-carrying matches! patterns live in a plain bool.)
        let out_of_range_typed = matches!(
            img.page_into(app_pages, &mut page),
            Err(Error::PageOutOfRange { index }) if index == app_pages
        );
        prop_assert!(out_of_range_typed);
        let mut short = vec![0u8; ps - 1];
        let short_buffer_typed = matches!(
            img.page_into(0, &mut short),
            Err(Error::BufferTooSmall { needed }) if needed == ps
        );
        prop_assert!(short_buffer_typed);
    }
}
