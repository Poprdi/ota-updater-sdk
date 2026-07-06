// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Session engine tests against a scripted `MockTransport`.
//!
//! Every test asserts the exact request bytes on the wire; the mock pads
//! responses with `0xFF` filler up to the caller's receive buffer, exactly
//! like a fixed-length I2C read from the device, so the padded-decode path
//! is exercised end to end.

use std::collections::VecDeque;

use updater_core::frame::{
    self, CMD_BOOT, CMD_ECHO, CMD_ERASE_APP, CMD_INFO, CMD_VERIFY, CMD_WRITE_PAGE, RSP_FLAG,
    ST_BAD_CRC, ST_BAD_FRAME, ST_NO_APP, ST_OK,
};
use updater_core::image::{self, Image};
use updater_core::{Error, Session, Transport};

// ---------------------------------------------------------------------------
// MockTransport
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
struct MockErr;

impl std::fmt::Display for MockErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mock transport failure")
    }
}

enum Reply {
    /// Respond with these bytes (a frame; the mock pads with 0xFF filler).
    Bytes(Vec<u8>),
    /// Fail the whole request with a transport error.
    Err,
}

struct MockTransport {
    script: VecDeque<(Vec<u8>, Reply)>,
    requests_seen: usize,
}

impl MockTransport {
    fn new(script: Vec<(Vec<u8>, Reply)>) -> Self {
        Self { script: script.into(), requests_seen: 0 }
    }

    fn done(&self) {
        assert!(
            self.script.is_empty(),
            "script not exhausted: {} exchange(s) never happened",
            self.script.len()
        );
    }
}

impl Transport for MockTransport {
    type Err = MockErr;

    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, MockErr> {
        self.requests_seen += 1;
        let (expect, reply) = self
            .script
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected request on the wire: {req:02X?}"));
        assert_eq!(req, &expect[..], "wire bytes mismatch (request #{})", self.requests_seen);
        match reply {
            Reply::Err => Err(MockErr),
            Reply::Bytes(bytes) => {
                assert!(
                    bytes.len() <= rsp.len(),
                    "session receive window ({}) smaller than response ({})",
                    rsp.len(),
                    bytes.len()
                );
                // Fixed-length read semantics: frame first, 0xFF filler after.
                rsp.fill(0xFF);
                rsp[..bytes.len()].copy_from_slice(&bytes);
                Ok(rsp.len())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

fn rsp_st(cmd: u8, st: u8) -> Reply {
    Reply::Bytes(enc(cmd | RSP_FLAG, &[st]))
}

/// INFO response for a 4-page x 16-byte device, proto as given.
fn info_rsp(proto: u8, page_size: u16, app_pages: u16) -> Reply {
    let ps = page_size.to_le_bytes();
    let ap = app_pages.to_le_bytes();
    Reply::Bytes(enc(
        CMD_INFO | RSP_FLAG,
        &[ST_OK, proto, 0x01, 0xDE, 0xAD, 0xBE, 0xEF, ps[0], ps[1], ap[0], ap[1], 0x00],
    ))
}

fn write_req(index: u16, page: &[u8]) -> Vec<u8> {
    let mut payload = index.to_le_bytes().to_vec();
    payload.extend_from_slice(page);
    enc(CMD_WRITE_PAGE, &payload)
}

fn verify_req(len: u32, crc: u32) -> Vec<u8> {
    let mut payload = len.to_le_bytes().to_vec();
    payload.extend_from_slice(&crc.to_le_bytes());
    enc(CMD_VERIFY, &payload)
}

// ---------------------------------------------------------------------------
// info / echo / boot
// ---------------------------------------------------------------------------

#[test]
fn info_parses_device_fields() {
    let mut t = MockTransport::new(vec![(enc(CMD_INFO, &[]), info_rsp(1, 16, 4))]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    let info = s.info().unwrap();
    assert_eq!(info.proto, 1);
    assert_eq!(info.bl_version, 0x01);
    assert_eq!(info.device_id, [0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(info.page_size, 16);
    assert_eq!(info.app_pages, 4);
    assert!(!info.app_valid);
    t.done();
}

#[test]
fn echo_roundtrip_and_mismatch() {
    let data = [0xAA, 0xBB, 0xCC];
    let mut t = MockTransport::new(vec![
        (enc(CMD_ECHO, &data), Reply::Bytes(enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0xAA, 0xBB, 0xCC]))),
        (enc(CMD_ECHO, &data), Reply::Bytes(enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0xAA, 0xBB, 0x00]))),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    s.echo(&data).unwrap();
    assert_eq!(s.echo(&data), Err(Error::BadFrame)); // echoed bytes differ
    t.done();
}

#[test]
fn echo_rejects_oversize_payload_before_touching_the_wire() {
    let mut t = MockTransport::new(vec![]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    let too_long = [0u8; 17]; // ECHO_MAX is 16
    assert_eq!(s.echo(&too_long), Err(Error::PayloadTooLarge { len: 17 }));
    assert_eq!(t.requests_seen, 0);
}

#[test]
fn boot_ok_and_no_app() {
    let mut t = MockTransport::new(vec![
        (enc(CMD_BOOT, &[]), rsp_st(CMD_BOOT, ST_OK)),
        (enc(CMD_BOOT, &[]), rsp_st(CMD_BOOT, ST_NO_APP)),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    s.boot().unwrap();
    assert_eq!(s.boot(), Err(Error::Device(ST_NO_APP)));
    t.done();
}

// ---------------------------------------------------------------------------
// flash happy path: exact frames on the wire, 4 x 16 device
// ---------------------------------------------------------------------------

#[test]
fn flash_happy_path_exact_wire_bytes() {
    // 20 bytes: page 0 full of 0x11, page 1 starts 22 33 44 55 then pads.
    let mut data = vec![0x11u8; 16];
    data.extend_from_slice(&[0x22, 0x33, 0x44, 0x55]);
    let img = Image::from_bin(&data, 16, 4).unwrap();

    let page0 = [0x11u8; 16];
    let mut page1 = [0xFFu8; 16];
    page1[..4].copy_from_slice(&[0x22, 0x33, 0x44, 0x55]);
    // Page 2 is all-0xFF -> skipped. Page 3 is the footer page; with a
    // 16-byte page the footer IS the page.
    let crc = image::crc32(&data);
    let mut page3 = *b"OTAU\0\0\0\0\0\0\0\0\xFF\xFF\xFF\xFF";
    page3[4..8].copy_from_slice(&20u32.to_le_bytes());
    page3[8..12].copy_from_slice(&crc.to_le_bytes());

    // The erase frame carries the "ERAS" magic; assert its raw bytes.
    let erase = enc(CMD_ERASE_APP, b"ERAS");
    let erase_body = [0x02, 0x04, 0x45, 0x52, 0x41, 0x53];
    assert_eq!(erase[..6], erase_body);
    assert_eq!(erase[6], frame::crc8(&erase_body));

    let mut t = MockTransport::new(vec![
        (enc(CMD_INFO, &[]), info_rsp(1, 16, 4)),
        (erase, rsp_st(CMD_ERASE_APP, ST_OK)),
        (write_req(0, &page0), rsp_st(CMD_WRITE_PAGE, ST_OK)),
        (write_req(1, &page1), rsp_st(CMD_WRITE_PAGE, ST_OK)),
        (write_req(3, &page3), rsp_st(CMD_WRITE_PAGE, ST_OK)),
        (verify_req(20, crc), rsp_st(CMD_VERIFY, ST_OK)),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    let mut progress = Vec::new();
    s.flash(&img, &mut |done, total| progress.push((done, total))).unwrap();
    assert_eq!(progress, vec![(0, 3), (1, 3), (2, 3), (3, 3)]);
    t.done();
}

// ---------------------------------------------------------------------------
// retries
// ---------------------------------------------------------------------------

#[test]
fn flash_retries_corrupt_write_response_with_identical_request() {
    // Empty image on a 1 x 16 device: only the footer page gets written.
    let img = Image::from_bin(&[], 16, 1).unwrap();
    let crc = image::crc32(&[]);
    let mut footer = *b"OTAU\0\0\0\0\0\0\0\0\xFF\xFF\xFF\xFF";
    footer[4..8].copy_from_slice(&0u32.to_le_bytes());
    footer[8..12].copy_from_slice(&crc.to_le_bytes());

    let mut corrupt = enc(CMD_WRITE_PAGE | RSP_FLAG, &[ST_OK]);
    *corrupt.last_mut().unwrap() ^= 0x5A; // CRC-8 now wrong

    let wreq = write_req(0, &footer);
    let mut t = MockTransport::new(vec![
        (enc(CMD_INFO, &[]), info_rsp(1, 16, 1)),
        (enc(CMD_ERASE_APP, b"ERAS"), rsp_st(CMD_ERASE_APP, ST_OK)),
        (wreq.clone(), Reply::Bytes(corrupt)),
        (wreq, rsp_st(CMD_WRITE_PAGE, ST_OK)), // byte-identical retry
        (verify_req(0, crc), rsp_st(CMD_VERIFY, ST_OK)),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    s.flash(&img, &mut |_, _| {}).unwrap();
    t.done();
}

#[test]
fn st_bad_frame_is_retried_then_succeeds() {
    let req = enc(CMD_ECHO, &[0x42]);
    let ok = Reply::Bytes(enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x42]));
    let mut t = MockTransport::new(vec![
        (req.clone(), rsp_st(CMD_ECHO, ST_BAD_FRAME)),
        (req.clone(), rsp_st(CMD_ECHO, ST_BAD_FRAME)),
        (req, ok),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    s.echo(&[0x42]).unwrap();
    t.done();
}

#[test]
fn retries_are_bounded_transport_error() {
    // 1 initial attempt + 3 retries = exactly 4 requests, then the error.
    let req = enc(CMD_BOOT, &[]);
    let mut t = MockTransport::new(vec![
        (req.clone(), Reply::Err),
        (req.clone(), Reply::Err),
        (req.clone(), Reply::Err),
        (req, Reply::Err),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    assert_eq!(s.boot(), Err(Error::Transport(MockErr)));
    assert_eq!(t.requests_seen, 4);
    t.done();
}

#[test]
fn retries_are_bounded_st_bad_frame() {
    let req = enc(CMD_BOOT, &[]);
    let script = (0..4).map(|_| (req.clone(), rsp_st(CMD_BOOT, ST_BAD_FRAME))).collect();
    let mut t = MockTransport::new(script);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    assert_eq!(s.boot(), Err(Error::Device(ST_BAD_FRAME)));
    assert_eq!(t.requests_seen, 4);
    t.done();
}

// ---------------------------------------------------------------------------
// gates and aborts
// ---------------------------------------------------------------------------

#[test]
fn flash_aborts_on_protocol_version_mismatch() {
    let data = [0x11u8; 4];
    let img = Image::from_bin(&data, 16, 4).unwrap();
    let mut t = MockTransport::new(vec![(enc(CMD_INFO, &[]), info_rsp(9, 16, 4))]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    assert_eq!(
        s.flash(&img, &mut |_, _| {}),
        Err(Error::ProtocolVersion { device: 9 })
    );
    assert_eq!(t.requests_seen, 1); // nothing after INFO — no erase
    t.done();
}

#[test]
fn flash_aborts_on_geometry_mismatch() {
    let data = [0x11u8; 4];
    let img = Image::from_bin(&data, 16, 4).unwrap(); // built for 16 x 4
    let mut t = MockTransport::new(vec![(enc(CMD_INFO, &[]), info_rsp(1, 32, 4))]);
    let mut buf = [0u8; 128];
    let mut s = Session::new(&mut t, &mut buf);
    assert_eq!(
        s.flash(&img, &mut |_, _| {}),
        Err(Error::BadGeometry { page_size: 32, app_pages: 4 })
    );
    assert_eq!(t.requests_seen, 1);
    t.done();
}

#[test]
fn flash_device_error_on_verify_is_final_and_nothing_follows() {
    let img = Image::from_bin(&[], 16, 1).unwrap();
    let crc = image::crc32(&[]);
    let mut footer = *b"OTAU\0\0\0\0\0\0\0\0\xFF\xFF\xFF\xFF";
    footer[8..12].copy_from_slice(&crc.to_le_bytes());

    let mut t = MockTransport::new(vec![
        (enc(CMD_INFO, &[]), info_rsp(1, 16, 1)),
        (enc(CMD_ERASE_APP, b"ERAS"), rsp_st(CMD_ERASE_APP, ST_OK)),
        (write_req(0, &footer), rsp_st(CMD_WRITE_PAGE, ST_OK)),
        (verify_req(0, crc), rsp_st(CMD_VERIFY, ST_BAD_CRC)),
    ]);
    let mut buf = [0u8; 64];
    let mut s = Session::new(&mut t, &mut buf);
    assert_eq!(s.flash(&img, &mut |_, _| {}), Err(Error::Device(ST_BAD_CRC)));
    assert_eq!(t.requests_seen, 4); // BAD_CRC is final: no retry, no BOOT
    t.done();
}

// ---------------------------------------------------------------------------
// misuse resistance
// ---------------------------------------------------------------------------

#[test]
fn too_small_frame_buf_is_a_typed_error_not_a_panic() {
    let mut t = MockTransport::new(vec![]);
    let mut tiny = [0u8; 8]; // INFO needs 3 (req) + 15 (rsp) = 18
    let mut s = Session::new(&mut t, &mut tiny);
    assert_eq!(s.info(), Err(Error::BufferTooSmall { needed: 18 }));
    assert_eq!(t.requests_seen, 0);
}
