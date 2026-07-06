// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Golden vectors fixed by the spec (docs/superpowers/specs/2026-07-03) plus
//! behavioral tests for the no_std frame codec and image handling.
//!
//! The library is `#![no_std]` / zero-alloc; this integration-test crate runs
//! on the host and may use `std`, `Vec` and `unwrap` freely.

use updater_core::frame::{self, Frame, CMD_ECHO, CMD_INFO, PAYLOAD_MAX};
use updater_core::image::{self, Image, FOOTER_LEN};
use updater_core::Error;

// ---------------------------------------------------------------------------
// Frame codec
// ---------------------------------------------------------------------------

#[test]
fn golden_vectors() {
    let mut buf = [0u8; 255];

    let n = frame::encode(CMD_INFO, &[], &mut buf).unwrap();
    assert_eq!(&buf[..n], &[0x01, 0x00, 0x15]);

    let n = frame::encode(CMD_ECHO, &[0xAA, 0xBB], &mut buf).unwrap();
    assert_eq!(&buf[..n], &[0x06, 0x02, 0xAA, 0xBB, 0x10]);

    assert_eq!(image::crc32(b"123456789"), 0xCBF4_3926);

    let raw = [0x06, 0x02, 0xAA, 0xBB, 0x10];
    let f = frame::decode(&raw).unwrap();
    assert_eq!(f, Frame { cmd: 0x06, payload: &[0xAA, 0xBB] });

    assert!(frame::decode(&[0x06, 0x02, 0xAA, 0xBB, 0x11]).is_err()); // bad CRC
    assert!(frame::decode(&[]).is_err());
}

#[test]
fn wire_constants_mirror_proto_h() {
    assert_eq!(frame::PROTO_VERSION, 1);
    assert_eq!(frame::CMD_INFO, 0x01);
    assert_eq!(frame::CMD_ERASE_APP, 0x02);
    assert_eq!(frame::CMD_WRITE_PAGE, 0x03);
    assert_eq!(frame::CMD_VERIFY, 0x04);
    assert_eq!(frame::CMD_BOOT, 0x05);
    assert_eq!(frame::CMD_ECHO, 0x06);
    assert_eq!(frame::RSP_FLAG, 0x80);
    assert_eq!(frame::ST_OK, 0x00);
    assert_eq!(frame::ST_BAD_FRAME, 0x01);
    assert_eq!(frame::ST_BAD_CMD, 0x02);
    assert_eq!(frame::ST_NOT_ERASED, 0x03);
    assert_eq!(frame::ST_OUT_OF_RANGE, 0x04);
    assert_eq!(frame::ST_BAD_CRC, 0x05);
    assert_eq!(frame::ST_BAD_MAGIC, 0x06);
    assert_eq!(frame::ST_NO_APP, 0x07);
    assert_eq!(frame::ECHO_MAX, 16);
    assert_eq!(frame::FRAME_OVERHEAD, 3);
    // Largest frame is 255 bytes on the wire (LEN is a u8, total = LEN + 3).
    assert_eq!(PAYLOAD_MAX, 252);
    assert_eq!(frame::ERASE_MAGIC, *b"ERAS");
}

#[test]
fn encode_bounds() {
    let mut buf = [0u8; 255];

    // Largest legal payload: 252 bytes -> 255-byte frame.
    let payload = [0x5Au8; PAYLOAD_MAX];
    let n = frame::encode(CMD_ECHO, &payload, &mut buf).unwrap();
    assert_eq!(n, 255);
    let f = frame::decode(&buf[..n]).unwrap();
    assert_eq!(f.payload, &payload[..]);

    // One more byte must be refused, matching the C codec.
    let too_big = [0u8; PAYLOAD_MAX + 1];
    assert!(matches!(
        frame::encode(CMD_ECHO, &too_big, &mut buf),
        Err(Error::PayloadTooLarge { len: 253 })
    ));

    // Output buffer too small is reported, not panicked on.
    let mut tiny = [0u8; 4];
    assert!(matches!(
        frame::encode(CMD_ECHO, &[0xAA, 0xBB], &mut tiny),
        Err(Error::BufferTooSmall { needed: 5 })
    ));

    // Exact-fit buffer works.
    let mut exact = [0u8; 5];
    assert_eq!(frame::encode(CMD_ECHO, &[0xAA, 0xBB], &mut exact).unwrap(), 5);
}

#[test]
fn decode_is_total() {
    // Length byte inconsistent with the buffer length.
    assert!(frame::decode(&[0x06, 0x03, 0xAA, 0xBB, 0x10]).is_err());
    assert!(frame::decode(&[0x06, 0x01, 0xAA, 0xBB, 0x10]).is_err());
    // Shorter than the 3-byte minimum.
    assert!(frame::decode(&[0x01]).is_err());
    assert!(frame::decode(&[0x01, 0x00]).is_err());
    // The device's LEN is a u8 and its buffers cap at 255 bytes, so frames
    // longer than 255 bytes do not exist on the wire: a 256-byte sequence
    // with LEN = 253 and a correct CRC must still be rejected (the C parser
    // could never accept it).
    let mut long = vec![0x06u8, 0xFD];
    long.extend_from_slice(&[0x00; 253]);
    long.push(frame::crc8(&long));
    assert_eq!(long.len(), 256);
    assert!(frame::decode(&long).is_err());
    // Flipping any single byte of a valid frame must fail CRC or length.
    let good = [0x06, 0x02, 0xAA, 0xBB, 0x10];
    for i in 0..good.len() {
        let mut bad = good;
        bad[i] ^= 0x01;
        assert!(frame::decode(&bad).is_err(), "byte {i} flip accepted");
    }
}

/// Hand-rolled property test (deterministic xorshift; no dev-dependencies so
/// the suite runs offline): encode/decode round-trips for arbitrary payloads,
/// and decode never panics on arbitrary noise.
#[test]
fn roundtrip_and_noise() {
    let mut state: u64 = 0x243F_6A88_85A3_08D3; // pi digits, nothing up my sleeve
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut frame_buf = [0u8; 255];
    for _ in 0..2000 {
        let len = (next() % 253) as usize; // 0..=252
        let cmd = (next() & 0xFF) as u8;
        let payload: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        let n = frame::encode(cmd, &payload, &mut frame_buf).unwrap();
        assert_eq!(n, len + 3);
        let f = frame::decode(&frame_buf[..n]).unwrap();
        assert_eq!((f.cmd, f.payload), (cmd, &payload[..]));
    }

    for _ in 0..2000 {
        let len = (next() % 300) as usize;
        let noise: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        let _ = frame::decode(&noise); // must not panic
    }
}

// ---------------------------------------------------------------------------
// Image handling
// ---------------------------------------------------------------------------

/// Render every page the iterator selects through the fill-in seam.
fn collect_pages(img: &Image<'_>) -> Vec<(u16, Vec<u8>)> {
    let mut buf = vec![0u8; usize::from(img.page_size())];
    img.pages()
        .map(|i| {
            img.page_into(i, &mut buf).unwrap();
            (i, buf.clone())
        })
        .collect()
}

#[test]
fn image_footer() {
    // 4 pages x 16 B region; 20-byte app -> footer in the last 16 bytes.
    let app = [0x42u8; 20];
    let img = Image::from_bin(&app, 16, 4).unwrap();
    assert_eq!(img.len(), 20);
    assert!(!img.is_empty());

    let pages = collect_pages(&img);
    // pages 0,1 hold data; page 2 is all-0xFF and skipped; page 3 = footer page
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0], (0, vec![0x42; 16]));
    let mut p1 = vec![0x42u8; 4];
    p1.extend_from_slice(&[0xFF; 12]);
    assert_eq!(pages[1], (1, p1));

    assert_eq!(pages[2].0, 3);
    assert_eq!(img.footer_page_index(), 3);
    let footer = &pages[2].1;
    assert_eq!(&footer[0..4], b"OTAU");
    assert_eq!(u32::from_le_bytes(footer[4..8].try_into().unwrap()), 20);
    assert_eq!(
        u32::from_le_bytes(footer[8..12].try_into().unwrap()),
        img.crc32()
    );
    assert_eq!(&footer[12..16], &[0xFF; 4]);

    // Images larger than region - 16 are refused.
    assert!(matches!(
        Image::from_bin(&[0u8; 49], 16, 4),
        Err(Error::ImageTooLarge { len: 49, capacity: 48 })
    ));
    // Exactly region - 16 is accepted.
    assert!(Image::from_bin(&[0u8; 48], 16, 4).is_ok());
}

#[test]
fn footer_shares_a_page_with_data() {
    // 2 pages x 32 B; 40 bytes of data: the footer page carries 8 data bytes,
    // 8 bytes of 0xFF pad, then the 16-byte footer.
    let app: Vec<u8> = (0..40u8).collect();
    let img = Image::from_bin(&app, 32, 2).unwrap();
    let pages = collect_pages(&img);
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[1].0, 1);
    let p1 = &pages[1].1;
    assert_eq!(&p1[0..8], &app[32..40]);
    assert_eq!(&p1[8..16], &[0xFF; 8]);
    assert_eq!(&p1[16..20], b"OTAU");
    assert_eq!(u32::from_le_bytes(p1[20..24].try_into().unwrap()), 40);
    assert_eq!(
        u32::from_le_bytes(p1[24..28].try_into().unwrap()),
        img.crc32()
    );
    assert_eq!(&p1[28..32], &[0xFF; 4]);
}

#[test]
fn all_ff_pages_are_skipped_but_footer_never_is() {
    // Data page that is entirely 0xFF is skipped (flash is already erased to
    // 0xFF), but the footer page is always emitted.
    let mut app = vec![0x11u8; 16];
    app.extend_from_slice(&[0xFF; 16]); // page 1: all 0xFF within data
    app.extend_from_slice(&[0x22; 4]); // page 2: partially used
    let img = Image::from_bin(&app, 16, 8).unwrap();
    let idx: Vec<u16> = img.pages().collect();
    assert_eq!(idx, vec![0, 2, 7]);

    // An empty image still yields exactly the footer page.
    let img = Image::from_bin(&[], 16, 4).unwrap();
    assert_eq!(img.len(), 0);
    assert!(img.is_empty());
    let idx: Vec<u16> = img.pages().collect();
    assert_eq!(idx, vec![3]);
}

#[test]
fn image_geometry_is_validated() {
    // Degenerate geometries are refused up front.
    assert!(matches!(
        Image::from_bin(&[], 0, 4),
        Err(Error::BadGeometry { page_size: 0, app_pages: 4 })
    ));
    assert!(matches!(
        Image::from_bin(&[], 16, 0),
        Err(Error::BadGeometry { .. })
    ));
    // A page smaller than the footer cannot host it.
    assert!(matches!(
        Image::from_bin(&[], 8, 4),
        Err(Error::BadGeometry { .. })
    ));
    // A page that cannot travel in one WRITE_PAGE frame (page_size + 2 > 252).
    assert!(matches!(
        Image::from_bin(&[], 251, 4),
        Err(Error::BadGeometry { .. })
    ));
    assert!(Image::from_bin(&[], 250, 4).is_ok());
    assert_eq!(FOOTER_LEN, 16);
}

#[test]
fn page_into_is_total() {
    let app = [0x42u8; 20];
    let img = Image::from_bin(&app, 16, 4).unwrap();

    let mut buf = [0u8; 16];
    assert!(matches!(
        img.page_into(4, &mut buf),
        Err(Error::PageOutOfRange { index: 4 })
    ));

    let mut small = [0u8; 15];
    assert!(matches!(
        img.page_into(0, &mut small),
        Err(Error::BufferTooSmall { needed: 16 })
    ));

    // Oversized buffers are fine; only the first page_size bytes are written.
    let mut big = [0xEEu8; 32];
    img.page_into(0, &mut big).unwrap();
    assert_eq!(&big[..16], &[0x42; 16]);
    assert_eq!(&big[16..], &[0xEE; 16]);

    // page_into also serves pages the iterator skips (all-0xFF).
    img.page_into(2, &mut buf).unwrap();
    assert_eq!(buf, [0xFF; 16]);
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[test]
fn errors_display_and_widen() {
    // Every variant renders a non-empty, non-debug Display string.
    let cases: Vec<Error<&str>> = vec![
        Error::Transport("bus stall"),
        Error::BadFrame,
        Error::Device(frame::ST_BAD_CRC),
        Error::PayloadTooLarge { len: 300 },
        Error::BufferTooSmall { needed: 5 },
        Error::ImageTooLarge { len: 49, capacity: 48 },
        Error::BadGeometry { page_size: 0, app_pages: 4 },
        Error::PageOutOfRange { index: 9 },
        Error::ProtocolVersion { device: 2 },
    ];
    for e in cases {
        assert!(!format!("{e}").is_empty());
    }

    // Codec errors (Error<Infallible>) widen into any transport error type.
    let e: Error = Error::BadFrame;
    let widened: Error<std::io::Error> = e.widen();
    assert!(matches!(widened, Error::BadFrame));

    // And they satisfy core::error::Error for std interop.
    fn is_error<E: core::error::Error>(_: &E) {}
    is_error(&Error::<core::convert::Infallible>::BadFrame);
}

// ---------------------------------------------------------------------------
// decode_padded: fixed-length-read tolerance
// ---------------------------------------------------------------------------

#[test]
fn decode_padded_accepts_exact_and_padded_frames() {
    let mut buf = [0u8; 64];
    let n = frame::encode(CMD_ECHO, &[0xAA, 0xBB], &mut buf).unwrap();
    let exact = &buf[..n];
    let want = Frame { cmd: CMD_ECHO, payload: &[0xAA, 0xBB] };

    // k = 0: every decode-accepted input is decode_padded-accepted.
    assert_eq!(frame::decode_padded(exact).unwrap(), want);

    // k > 0: trailing 0xFF filler is trimmed by the LEN byte, not guessed.
    let mut padded = exact.to_vec();
    padded.extend_from_slice(&[0xFF; 27]);
    assert_eq!(frame::decode_padded(&padded).unwrap(), want);
}

#[test]
fn decode_padded_is_length_driven_crc_ff_survives_padding() {
    // Find a 1-byte payload whose frame CRC-8 is 0xFF: a blind
    // strip-trailing-0xFF would eat the CRC; the LEN-driven trim must not.
    let mut buf = [0u8; 8];
    let found = (0..=255u8).find(|&b| {
        let n = frame::encode(CMD_ECHO, &[b], &mut buf).unwrap();
        buf[n - 1] == 0xFF
    });
    let b = found.expect("some payload byte yields CRC 0xFF");
    let n = frame::encode(CMD_ECHO, &[b], &mut buf).unwrap();
    let mut padded = buf[..n].to_vec();
    padded.extend_from_slice(&[0xFF; 5]);
    let f = frame::decode_padded(&padded).unwrap();
    assert_eq!(f.payload, &[b]);
}

#[test]
fn decode_padded_rejects_non_ff_tail_truncation_and_junk() {
    let mut buf = [0u8; 64];
    let n = frame::encode(CMD_ECHO, &[0xAA], &mut buf).unwrap();

    // Non-0xFF bytes after the frame mean desync, not filler.
    let mut tail = buf[..n].to_vec();
    tail.extend_from_slice(&[0xFF, 0x00, 0xFF]);
    assert!(frame::decode_padded(&tail).is_err());

    // Truncated: LEN promises more bytes than the buffer holds.
    assert!(frame::decode_padded(&buf[..n - 1]).is_err());

    // Degenerate inputs.
    assert!(frame::decode_padded(&[]).is_err());
    assert!(frame::decode_padded(&[0x81]).is_err());
    // A not-ready device returns all 0xFF: LEN = 255 > PAYLOAD_MAX.
    assert!(frame::decode_padded(&[0xFF; 300]).is_err());

    // Corrupt CRC still rejects after the trim, exactly like decode.
    let mut bad = buf[..n].to_vec();
    *bad.last_mut().unwrap() ^= 1;
    bad.extend_from_slice(&[0xFF; 4]);
    assert!(frame::decode_padded(&bad).is_err());
}
