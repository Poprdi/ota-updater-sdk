// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Stream-path conformance: the host's `updater_core::stream` scanner
//! against the REAL device `link_stream.c`, both directions — requests
//! enter through `link_poll`'s sync hunt, responses leave through
//! `link_send` and are scanned by the host. The same campaigns that pin
//! the transactional path run here through the byte-stream path.

use conformance::{Sim, APP_PAGES, PAGE_SIZE, REGION};
use updater_core::frame::{self, CMD_ECHO, CMD_INFO, PROTO_VERSION, RSP_FLAG, ST_OK};
use updater_core::image::Image;
use updater_core::stream::SYNC;
use updater_core::Session;

/// Deterministic campaign image (same generator as campaigns.rs).
fn prng_bytes(mut seed: u32, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        out.push((seed & 0xFF) as u8);
    }
    out
}

fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

/// `0x7E` + frame: the stream binding.
fn stream(frame_bytes: &[u8]) -> Vec<u8> {
    let mut s = vec![SYNC];
    s.extend_from_slice(frame_bytes);
    s
}

#[test]
fn info_golden_exchange_over_the_stream() {
    let sim = Sim::acquire();

    // The spec's smallest frame, hand-framed: 7E 01 00 15.
    let raw = sim.request_stream(&[SYNC, 0x01, 0x00, 0x15]);
    // The device's link_send emits exactly sync + the response frame.
    assert_eq!(raw.first(), Some(&SYNC), "response must start with the sync byte");
    let rsp = frame::decode(&raw[1..]).expect("response frame must decode");
    assert_eq!(rsp.cmd, CMD_INFO | RSP_FLAG);
    assert_eq!(
        rsp.payload,
        [ST_OK, PROTO_VERSION, 1, b'S', b'I', b'M', b'0', 128, 0, 32, 0, 0]
    );

    // Byte-identical cross-check: the Rust encoder reproduces the C
    // builder's frame, and the whole stream is sync + that frame.
    assert_eq!(raw, stream(&enc(rsp.cmd, rsp.payload)));
}

#[test]
fn device_hunts_sync_through_leading_garbage() {
    let sim = Sim::acquire();
    let mut wire = vec![0x00, 0xA5, 0x42]; // pre-sync garbage on the device's RX
    wire.extend_from_slice(&stream(&enc(CMD_ECHO, &[0xDE, 0xAD])));
    let raw = sim.request_stream(&wire);
    let rsp = frame::decode(&raw[1..]).expect("device must resync and answer");
    assert_eq!(rsp.payload, [ST_OK, 0xDE, 0xAD]);
}

#[test]
fn corrupt_request_is_silently_dropped_then_retry_succeeds() {
    let sim = Sim::acquire();
    let good = enc(CMD_ECHO, &[0x77]);
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 0xFF;

    // Stream semantics: a CRC-corrupt frame yields NO reply (unlike the
    // transactional path's ST_BAD_FRAME) — the link drops it silently and
    // the host's retry owns recovery.
    assert!(
        sim.request_stream(&stream(&bad)).is_empty(),
        "link must drop a corrupt frame without a reply"
    );
    let raw = sim.request_stream(&stream(&good));
    let rsp = frame::decode(&raw[1..]).expect("retry must be answered");
    assert_eq!(rsp.payload, [ST_OK, 0x77]);
}

#[test]
fn request_torn_across_pump_calls_completes() {
    let sim = Sim::acquire();
    let wire = stream(&enc(CMD_ECHO, &[0x11, 0x22]));
    let (head, tail) = wire.split_at(3);

    assert!(sim.request_stream(head).is_empty(), "half a frame gets no reply yet");
    let raw = sim.request_stream(tail);
    let rsp = frame::decode(&raw[1..]).expect("completed frame must be answered");
    assert_eq!(rsp.payload, [ST_OK, 0x11, 0x22]);
}

#[test]
fn two_requests_in_one_pump_get_two_replies() {
    let sim = Sim::acquire();
    let mut wire = stream(&enc(CMD_ECHO, &[0x01]));
    wire.extend_from_slice(&stream(&enc(CMD_ECHO, &[0x02])));
    let raw = sim.request_stream(&wire);

    let first_len = 1 + enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x01]).len();
    let (a, b) = raw.split_at(first_len);
    assert_eq!(a, stream(&enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x01])));
    assert_eq!(b, stream(&enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x02])));
}

#[test]
fn session_golden_over_the_stream_transport() {
    // INFO + ECHO through the shipped Session + the stream transport with
    // one SPI-style lag byte prepended to every response scan.
    let sim = Sim::acquire();
    let mut buf = [0u8; 320];
    let mut session = Session::new(sim.stream_transport(1), &mut buf);

    let info = session.info().expect("INFO through the stream path");
    assert_eq!(info.proto, PROTO_VERSION);
    assert_eq!(info.device_id, *b"SIM0");
    assert_eq!(info.page_size, 128);
    assert_eq!(info.app_pages, 32);

    session.echo(&[0xDE, 0xAD, 0xBE, 0xEF]).expect("ECHO through the stream path");
    session.echo(&[]).expect("empty ECHO");
    session.echo(&[0x55; 16]).expect("max-length ECHO");
}

#[test]
fn full_update_campaign_over_the_stream_transport() {
    let sim = Sim::acquire();
    let data = prng_bytes(0x2A2A_2A2A, 3000);
    let img = Image::from_bin(&data, PAGE_SIZE as u16, APP_PAGES as u16).unwrap();

    let mut buf = [0u8; 320];
    let mut session = Session::new(sim.stream_transport(1), &mut buf);

    let info = session.info().unwrap();
    assert!(!info.app_valid, "blank flash cannot hold a valid app");

    let mut calls: Vec<(u16, u16)> = Vec::new();
    session.flash(&img, &mut |done, total| calls.push((done, total))).unwrap();
    let total = u16::try_from(img.pages().count()).unwrap();
    assert_eq!(calls.first(), Some(&(0, total)));
    assert_eq!(calls.last(), Some(&(total, total)));

    // Flash contents byte-identical to the image layer's rendering — the
    // stream path must corrupt nothing.
    let mut expected = vec![0u8; REGION];
    for p in 0..APP_PAGES {
        let page = u16::try_from(p).unwrap();
        img.page_into(page, &mut expected[p * PAGE_SIZE..][..PAGE_SIZE]).unwrap();
    }
    assert_eq!(sim.flash_snapshot(), expected);

    let info = session.info().unwrap();
    assert!(info.app_valid, "flashed + verified image must satisfy the boot gate");

    session.boot().expect("boot must be accepted over the stream");
    assert!(sim.jumped(), "BOOT reply must be emitted before the jump");
}
