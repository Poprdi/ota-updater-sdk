//! Exhaustive FSM enumeration: both session pre-states × all 256 command
//! bytes × 7 frame variants, straight through the C parse→handle→build
//! path. This is the full alphabet, not a sample: every cell's response
//! must be a valid frame, its status must match the spec table
//! (§Wire protocol v1; device/core/update.c is the reference), and flash
//! must be bit-identical before and after — none of these stimuli is a
//! legal flash-touching operation.

use conformance::Sim;
use updater_core::frame::{
    self, CMD_BOOT, CMD_ECHO, CMD_ERASE_APP, CMD_INFO, CMD_VERIFY, CMD_WRITE_PAGE,
    ERASE_MAGIC, RSP_FLAG, ST_BAD_CMD, ST_BAD_FRAME, ST_BAD_MAGIC, ST_NOT_ERASED,
    ST_NO_APP, ST_OK,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Fresh boot: `upd_init` just ran, no ERASE_APP this session.
    Fresh,
    /// After a successful ERASE_APP in this session.
    Erased,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// Valid frame, empty payload.
    WellformedEmpty,
    /// Valid frame, maximum payload (252 bytes of 0xA5).
    WellformedMax,
    /// Wellformed-empty frame with its CRC byte inverted.
    BadCrc,
    /// LEN byte inconsistent with the buffer length (LEN=5 in a 4-byte
    /// buffer, CRC computed over what is present so LEN is the only fault).
    LenMismatch,
    /// Wellformed-empty frame truncated to 0, 1 or 2 bytes.
    Truncated(usize),
}

const VARIANTS: [Variant; 7] = [
    Variant::WellformedEmpty,
    Variant::WellformedMax,
    Variant::BadCrc,
    Variant::LenMismatch,
    Variant::Truncated(0),
    Variant::Truncated(1),
    Variant::Truncated(2),
];

/// Does this variant survive `upd_frame_parse`?
fn parseable(v: Variant) -> bool {
    matches!(v, Variant::WellformedEmpty | Variant::WellformedMax)
}

/// Build the stimulus bytes for (cmd, variant) with the Rust encoder.
fn stimulus(cmd: u8, v: Variant) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let wellformed = |payload: &[u8], buf: &mut [u8; 256]| {
        let n = frame::encode(cmd, payload, buf).expect("stimulus encodes");
        buf[..n].to_vec()
    };
    match v {
        Variant::WellformedEmpty => wellformed(&[], &mut buf),
        Variant::WellformedMax => wellformed(&[0xA5; 252], &mut buf),
        Variant::BadCrc => {
            let mut f = wellformed(&[], &mut buf);
            *f.last_mut().unwrap() ^= 0xFF;
            f
        }
        Variant::LenMismatch => {
            let body = [cmd, 5, 0xAA];
            let mut f = body.to_vec();
            f.push(frame::crc8(&body)); // CRC fine; LEN says 8-byte frame
            f
        }
        Variant::Truncated(k) => wellformed(&[], &mut buf)[..k].to_vec(),
    }
}

/// The spec table: expected status byte for (state, cmd, variant).
///
/// Derivation (spec §Wire protocol v1, reference device/core/update.c):
/// unparseable input never reaches a handler → ST_BAD_FRAME. Parseable:
/// INFO ignores its payload → OK; ERASE_APP demands exactly the 4-byte
/// magic → BAD_MAGIC for both variants; WRITE_PAGE demands a prior erase
/// (NOT_ERASED) and then exactly page_size+2 payload (BAD_FRAME — neither
/// 0 nor 252 is 130); VERIFY demands exactly 8 (BAD_FRAME); BOOT checks
/// only the footer, and flash is blank 0xFF in both states → NO_APP; ECHO
/// caps at 16 (empty → OK, 252 → BAD_FRAME); anything else → BAD_CMD.
fn expected_status(state: State, cmd: u8, v: Variant) -> u8 {
    if !parseable(v) {
        return ST_BAD_FRAME;
    }
    match cmd {
        CMD_INFO => ST_OK,
        CMD_ERASE_APP => ST_BAD_MAGIC,
        CMD_WRITE_PAGE => match state {
            State::Fresh => ST_NOT_ERASED,
            State::Erased => ST_BAD_FRAME,
        },
        CMD_VERIFY => ST_BAD_FRAME,
        CMD_BOOT => ST_NO_APP,
        CMD_ECHO => match v {
            Variant::WellformedEmpty => ST_OK,
            _ => ST_BAD_FRAME,
        },
        _ => ST_BAD_CMD,
    }
}

/// Put the sim into `state` from scratch (flash wiped to 0xFF).
fn enter_state(sim: &Sim, state: State) {
    sim.reset(false);
    if state == State::Erased {
        let mut req = [0u8; 8];
        let n = frame::encode(CMD_ERASE_APP, &ERASE_MAGIC, &mut req).unwrap();
        let raw = sim.request(&req[..n]);
        let rsp = frame::decode(&raw).expect("ERASE setup response decodes");
        assert_eq!(rsp.payload, [ST_OK], "ERASE_APP setup must succeed");
    }
}

fn sweep(state: State) {
    let sim = Sim::acquire();
    for cmd_wide in 0u16..=255 {
        let cmd = u8::try_from(cmd_wide).unwrap();
        for v in VARIANTS {
            // Fresh cell every time: no stimulus may leak state into the
            // next cell's expectation.
            enter_state(&sim, state);
            let before = sim.flash_snapshot();

            let stim = stimulus(cmd, v);
            let raw = sim.request(&stim);

            // (a) The response is always one valid frame, both under exact
            // decode and under fixed-length-read 0xFF padding.
            assert!(!raw.is_empty(), "device must answer ({state:?} {cmd:#04x} {v:?})");
            let rsp = frame::decode(&raw)
                .unwrap_or_else(|e| panic!("undecodable response ({state:?} {cmd:#04x} {v:?}): {e:?}"));
            let mut padded = vec![0xFFu8; 259];
            padded[..raw.len()].copy_from_slice(&raw);
            let padded_rsp = frame::decode_padded(&padded).expect("padded decode agrees");
            assert_eq!((padded_rsp.cmd, padded_rsp.payload), (rsp.cmd, rsp.payload));

            // (b) Status per the spec table. For parseable stimuli the
            // response command is pinned to cmd|0x80; for unparseable ones
            // the spec fixes only the ST_BAD_FRAME status (asserting a CMD
            // byte there would overconstrain the reference).
            let expect = expected_status(state, cmd, v);
            let st = *rsp.payload.first().expect("status byte always present");
            assert_eq!(
                st, expect,
                "status mismatch at ({state:?}, {cmd:#04x}, {v:?}): got {st:#04x}"
            );
            if parseable(v) {
                assert_eq!(rsp.cmd, cmd | RSP_FLAG);
            }
            // Error replies are status-only; OK replies carry the spec's
            // payload (INFO: 11 more bytes; ECHO: the echoed bytes).
            if st == ST_OK {
                match cmd {
                    CMD_INFO => assert_eq!(rsp.payload.len(), 12),
                    CMD_ECHO => assert_eq!(rsp.payload, [ST_OK]),
                    _ => unreachable!("only INFO/ECHO can be OK in this sweep"),
                }
            } else {
                assert_eq!(rsp.payload.len(), 1, "error replies are status-only");
            }

            // (c) No stimulus in this enumeration is a legal flash-touching
            // (state, cmd) pair — ERASE_APP never carries the magic here and
            // WRITE_PAGE never carries a 130-byte payload — so flash must be
            // bit-identical.
            assert_eq!(
                before,
                sim.flash_snapshot(),
                "flash touched at ({state:?}, {cmd:#04x}, {v:?})"
            );
        }
    }
}

#[test]
fn fsm_exhaustive_fresh() {
    sweep(State::Fresh);
}

#[test]
fn fsm_exhaustive_erased() {
    sweep(State::Erased);
}
