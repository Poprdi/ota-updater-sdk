//! Host-side stream framing scanner: the byte-exact mirror of the device's
//! `link_stream.c` receive machine (sync hunt, LEN-driven completion,
//! silent drop + re-hunt). These tests pin the accept set against the same
//! scenarios the device-side `test_link.c` pins.

use updater_core::frame::{self, CMD_ECHO, CMD_INFO, RSP_FLAG, ST_OK};
use updater_core::stream::{RxScanner, Scan, SYNC};

/// Encode a frame the way the device's builder would.
fn enc(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 300];
    let n = frame::encode(cmd, payload, &mut buf).unwrap();
    buf[..n].to_vec()
}

/// Push every byte, returning the outcome of the last push.
fn feed(scanner: &mut RxScanner, bytes: &[u8], buf: &mut [u8]) -> Scan {
    let mut last = Scan::Hunt;
    for &b in bytes {
        last = scanner.push(b, buf);
    }
    last
}

#[test]
fn hunts_until_sync_then_assembles_len_driven() {
    let rsp = enc(CMD_INFO | RSP_FLAG, &[ST_OK, 1, 2, 3]);
    let mut wire = vec![0xA5, 0x13, 0x37]; // pre-sync garbage
    wire.push(SYNC);
    wire.extend_from_slice(&rsp);

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    let last = feed(&mut sc, &wire, &mut buf);
    assert_eq!(last, Scan::Done { len: rsp.len() });
    assert_eq!(&buf[..rsp.len()], &rsp[..]);
    let f = frame::decode(&buf[..rsp.len()]).unwrap();
    assert_eq!(f.cmd, CMD_INFO | RSP_FLAG);
    assert_eq!(f.payload, [ST_OK, 1, 2, 3]);
}

#[test]
fn spi_busy_zeros_and_lag_byte_are_discarded() {
    // The SPI slave shifts 0x00 while busy, and the one-byte-lag contract
    // means at least one stale byte precedes the sync. All of it is hunt
    // fodder.
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x42]);
    let mut wire = vec![0x00; 40]; // busy run
    wire.push(0xEE); // stale staked byte (lag)
    wire.push(SYNC);
    wire.extend_from_slice(&rsp);

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    let mut done = None;
    for &b in &wire {
        match sc.push(b, &mut buf) {
            Scan::Done { len } => done = Some(len),
            Scan::Dropped => panic!("nothing to drop in this stream"),
            _ => {}
        }
    }
    assert_eq!(done, Some(rsp.len()));
    assert_eq!(&buf[..rsp.len()], &rsp[..]);
}

#[test]
fn sync_byte_inside_payload_is_frame_data() {
    // Parsing is length-driven after acquisition: a 0x7E payload byte must
    // not restart the hunt.
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, SYNC, SYNC, 0x01]);
    let mut wire = vec![SYNC];
    wire.extend_from_slice(&rsp);

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    assert_eq!(feed(&mut sc, &wire, &mut buf), Scan::Done { len: rsp.len() });
    let f = frame::decode(&buf[..rsp.len()]).unwrap();
    assert_eq!(f.payload, [ST_OK, SYNC, SYNC, 0x01]);
}

#[test]
fn torn_frame_stays_in_frame_until_completed() {
    let rsp = enc(CMD_INFO | RSP_FLAG, &[ST_OK, 9, 9, 9]);
    let (head, tail) = rsp.split_at(3);

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    assert_eq!(sc.push(SYNC, &mut buf), Scan::Frame);
    assert_eq!(feed(&mut sc, head, &mut buf), Scan::Frame);
    assert!(sc.in_frame(), "torn frame: scanner must keep assembling");
    assert_eq!(feed(&mut sc, tail, &mut buf), Scan::Done { len: rsp.len() });
    assert!(!sc.in_frame());
}

#[test]
fn completion_is_exactly_at_len_plus_overhead() {
    let rsp = enc(CMD_ECHO | RSP_FLAG, &[ST_OK]);
    assert_eq!(rsp.len(), 4);

    let mut buf = [0u8; 16];
    let mut sc = RxScanner::new();
    sc.push(SYNC, &mut buf);
    // Every byte but the last reports Frame; the last completes.
    for &b in &rsp[..rsp.len() - 1] {
        assert_eq!(sc.push(b, &mut buf), Scan::Frame);
    }
    assert_eq!(sc.push(rsp[3], &mut buf), Scan::Done { len: 4 });
}

#[test]
fn crc_corrupt_frame_dropped_then_resync_accepts_retry() {
    let good = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x11]);
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 0xFF;

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    sc.push(SYNC, &mut buf);
    assert_eq!(feed(&mut sc, &bad, &mut buf), Scan::Dropped);
    assert!(!sc.in_frame(), "drop must resume hunting");

    sc.push(SYNC, &mut buf);
    assert_eq!(feed(&mut sc, &good, &mut buf), Scan::Done { len: good.len() });
    assert_eq!(&buf[..good.len()], &good[..]);
}

#[test]
fn declared_frame_too_big_for_buffer_dropped_at_len_byte() {
    // Mirrors the device: an over-long declared frame is dropped the moment
    // LEN arrives, before any overflow is possible.
    let mut buf = [0u8; 8]; // fits LEN <= 5 only
    let mut sc = RxScanner::new();
    sc.push(SYNC, &mut buf);
    assert_eq!(sc.push(CMD_ECHO | RSP_FLAG, &mut buf), Scan::Frame);
    assert_eq!(sc.push(6, &mut buf), Scan::Dropped); // total 9 > 8
    assert!(!sc.in_frame());
    // The next byte is hunted, not stored.
    assert_eq!(sc.push(0xAB, &mut buf), Scan::Hunt);
}

#[test]
fn len_above_payload_max_dropped_even_in_a_big_buffer() {
    // LEN 253..=255 can never be a valid frame (PAYLOAD_MAX = 252); the
    // device's u8 buffer drops them structurally, a big host buffer must
    // drop them deliberately.
    for len_byte in 253..=255u16 {
        let mut buf = [0u8; 512];
        let mut sc = RxScanner::new();
        sc.push(SYNC, &mut buf);
        sc.push(0x81, &mut buf);
        #[allow(clippy::cast_possible_truncation)]
        let scan = sc.push(len_byte as u8, &mut buf);
        assert_eq!(scan, Scan::Dropped, "LEN {len_byte} must drop");
    }
}

#[test]
fn buffer_below_frame_minimum_never_syncs() {
    let mut buf = [0u8; 2];
    let mut sc = RxScanner::new();
    assert_eq!(sc.push(SYNC, &mut buf), Scan::Hunt);
    assert!(!sc.in_frame());
}

#[test]
fn back_to_back_frames_both_complete() {
    let a = enc(CMD_INFO | RSP_FLAG, &[ST_OK]);
    let b = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x55]);
    let mut wire = vec![SYNC];
    wire.extend_from_slice(&a);
    wire.push(SYNC);
    wire.extend_from_slice(&b);

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    let mut frames = Vec::new();
    for &byte in &wire {
        if let Scan::Done { len } = sc.push(byte, &mut buf) {
            frames.push(buf[..len].to_vec());
        }
    }
    assert_eq!(frames, vec![a, b]);
}

#[test]
fn garbage_containing_sync_recovers_via_drop_and_retry() {
    // A 0x7E inside pre-frame garbage falsely enters a frame; the machine
    // must eventually drop and accept a retransmission — same recovery
    // story as the device (session retry owns loss).
    let good = enc(CMD_ECHO | RSP_FLAG, &[ST_OK, 0x77]);
    let mut wire = vec![SYNC, 0x99, 0x01, 0xFE, 0xFE]; // false frame, bad CRC
    wire.push(SYNC);
    wire.extend_from_slice(&good); // the retry

    let mut buf = [0u8; 64];
    let mut sc = RxScanner::new();
    let mut done = None;
    let mut dropped = 0;
    for &byte in &wire {
        match sc.push(byte, &mut buf) {
            Scan::Done { len } => done = Some(buf[..len].to_vec()),
            Scan::Dropped => dropped += 1,
            _ => {}
        }
    }
    assert_eq!(dropped, 1, "the false frame must be dropped exactly once");
    assert_eq!(done.as_deref(), Some(&good[..]));
}

#[test]
fn reset_abandons_a_partial_frame() {
    let mut buf = [0u8; 16];
    let mut sc = RxScanner::new();
    sc.push(SYNC, &mut buf);
    sc.push(0x81, &mut buf);
    assert!(sc.in_frame());
    sc.reset();
    assert!(!sc.in_frame());
    // After reset the scanner hunts again.
    assert_eq!(sc.push(0x81, &mut buf), Scan::Hunt);
}

#[test]
fn shrunken_buffer_between_pushes_drops_instead_of_panicking() {
    // The scanner's invariant assumes the same buffer across a frame; a
    // caller who swaps in a shorter one must get a Drop, never a panic.
    let mut big = [0u8; 64];
    let mut sc = RxScanner::new();
    // Assemble 4 bytes of a declared-10-byte frame in the big buffer.
    for b in [SYNC, 0x81, 10, 0xAA, 0xBB] {
        sc.push(b, &mut big);
    }
    assert!(sc.in_frame());
    // Next write index (4) is out of bounds for the swapped-in buffer:
    // must resolve as a Drop, never a panic.
    let mut tiny = [0u8; 3];
    assert_eq!(sc.push(0xCC, &mut tiny), Scan::Dropped);
    assert!(!sc.in_frame());
}

#[test]
fn max_length_frame_completes() {
    let payload = [0x5Au8; frame::PAYLOAD_MAX];
    let big = enc(0x83, &payload);
    assert_eq!(big.len(), 255);

    let mut buf = [0u8; 255];
    let mut sc = RxScanner::new();
    sc.push(SYNC, &mut buf);
    assert_eq!(feed(&mut sc, &big, &mut buf), Scan::Done { len: 255 });
    assert_eq!(&buf[..], &big[..]);
}
