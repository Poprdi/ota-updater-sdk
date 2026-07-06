//! Golden byte exchanges: literal wire bytes through the C side, validated
//! by the Rust codec in both directions. If either codec drifts from the
//! spec's §Wire protocol v1, these literals catch it.

use conformance::Sim;
use updater_core::frame::{
    self, CMD_ECHO, CMD_INFO, PROTO_VERSION, RSP_FLAG, ST_OK,
};
use updater_core::Session;

/// Pad `raw` to a fixed-length-read buffer with 0xFF filler, the way a real
/// I2C master sees it, and require `decode_padded` to agree with `decode`.
fn assert_padded_agrees(raw: &[u8]) {
    let mut padded = vec![0xFFu8; 259];
    padded[..raw.len()].copy_from_slice(raw);
    let a = frame::decode(raw).expect("exact response must decode");
    let b = frame::decode_padded(&padded).expect("padded response must decode");
    assert_eq!(a.cmd, b.cmd);
    assert_eq!(a.payload, b.payload);
}

#[test]
fn info_golden_exchange() {
    let sim = Sim::acquire();

    // The INFO request is the spec's smallest frame: 01 00 15. Pin that the
    // Rust encoder produces exactly these bytes, then drive the C side with
    // the literal (not the encoder output) so the two stay independent.
    const INFO_REQ: [u8; 3] = [0x01, 0x00, 0x15];
    let mut enc = [0u8; 8];
    let n = frame::encode(CMD_INFO, &[], &mut enc).unwrap();
    assert_eq!(&enc[..n], &INFO_REQ);

    let raw = sim.request(&INFO_REQ);
    // Response payload: ST, proto, bl_ver, id[4], page_size LE, app_pages
    // LE, app_valid — 12 bytes, so the whole frame is 15.
    assert_eq!(raw.len(), 15);
    let rsp = frame::decode(&raw).expect("INFO response must decode (CRC-8 valid)");
    assert_eq!(rsp.cmd, CMD_INFO | RSP_FLAG);
    assert_eq!(
        rsp.payload,
        [
            ST_OK,
            PROTO_VERSION,
            1,                        // bl_version
            b'S', b'I', b'M', b'0',   // device_id
            128, 0,                   // page_size LE
            32, 0,                    // app_pages LE
            0,                        // app_valid: blank flash
        ]
    );
    assert_padded_agrees(&raw);

    // Byte-identical cross-check the other way: the Rust encoder, given the
    // same cmd/payload, must reproduce the C builder's frame exactly
    // (including the CRC byte).
    let mut expect = [0u8; 32];
    let n = frame::encode(rsp.cmd, rsp.payload, &mut expect).unwrap();
    assert_eq!(&expect[..n], &raw[..]);
}

#[test]
fn echo_golden_exchange() {
    let sim = Sim::acquire();

    // ECHO DE AD BE EF. Request literal: CRC-8(06 04 DE AD BE EF) = B3
    // (SMBus parameters, see PROTOCOL.md section 2).
    const ECHO_REQ: [u8; 7] = [0x06, 0x04, 0xDE, 0xAD, 0xBE, 0xEF, 0xB3];
    let mut enc = [0u8; 8];
    let n = frame::encode(CMD_ECHO, &[0xDE, 0xAD, 0xBE, 0xEF], &mut enc).unwrap();
    assert_eq!(&enc[..n], &ECHO_REQ, "Rust encoder drifted from the golden request");

    let raw = sim.request(&ECHO_REQ);
    let rsp = frame::decode(&raw).expect("ECHO response must decode");
    assert_eq!(rsp.cmd, CMD_ECHO | RSP_FLAG);
    assert_eq!(rsp.payload, [ST_OK, 0xDE, 0xAD, 0xBE, 0xEF]);
    assert_padded_agrees(&raw);

    // And the C builder's bytes must equal the Rust encoder's for the same
    // response — the CRC byte crosses implementations both directions.
    let mut expect = [0u8; 16];
    let n = frame::encode(rsp.cmd, rsp.payload, &mut expect).unwrap();
    assert_eq!(&expect[..n], &raw[..]);
}

#[test]
fn session_golden_over_padded_transport() {
    // The same two exchanges through the shipped Session + SimTransport:
    // every response arrives 0xFF-padded to the fixed read length, so this
    // pins the decode_padded contract end to end.
    let sim = Sim::acquire();
    let mut buf = [0u8; 320];
    let mut session = Session::new(sim.transport(), &mut buf);

    let info = session.info().expect("INFO through the real session");
    assert_eq!(info.proto, PROTO_VERSION);
    assert_eq!(info.bl_version, 1);
    assert_eq!(info.device_id, *b"SIM0");
    assert_eq!(info.page_size, 128);
    assert_eq!(info.app_pages, 32);
    assert!(!info.app_valid);

    session
        .echo(&[0xDE, 0xAD, 0xBE, 0xEF])
        .expect("ECHO through the real session");
    session.echo(&[]).expect("empty ECHO");
    session.echo(&[0x55; 16]).expect("max-length ECHO");
}
