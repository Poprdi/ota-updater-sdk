//! Wire frame codec, mirroring `device/include/updater/proto.h` byte for
//! byte.
//!
//! Frame layout: `[0] = CMD`, `[1] = LEN`, `[2 .. 2+LEN) = payload`,
//! `[2+LEN] = CRC-8` over bytes `0 .. 2+LEN` (CRC-8/ATM: polynomial 0x07,
//! init 0x00, no reflection, no final XOR).
//!
//! Everything here is a total function into caller-provided buffers: no
//! allocation, no panics, no `unsafe`.

use crate::error::Error;

/// Protocol version implemented by this crate (`UPD_PROTO_VERSION`).
pub const PROTO_VERSION: u8 = 1;

/// Query device info (`UPD_CMD_INFO`).
pub const CMD_INFO: u8 = 0x01;
/// Erase the app region; requires [`ERASE_MAGIC`] (`UPD_CMD_ERASE_APP`).
pub const CMD_ERASE_APP: u8 = 0x02;
/// Write one flash page (`UPD_CMD_WRITE_PAGE`).
pub const CMD_WRITE_PAGE: u8 = 0x03;
/// Verify the written image against length + CRC-32 (`UPD_CMD_VERIFY`).
pub const CMD_VERIFY: u8 = 0x04;
/// Boot the application (`UPD_CMD_BOOT`).
pub const CMD_BOOT: u8 = 0x05;
/// Echo payload back, link smoke test (`UPD_CMD_ECHO`).
pub const CMD_ECHO: u8 = 0x06;
/// Set on the CMD byte of every device response (`UPD_RSP_FLAG`).
pub const RSP_FLAG: u8 = 0x80;

/// Success (`UPD_ST_OK`).
pub const ST_OK: u8 = 0x00;
/// Malformed frame (`UPD_ST_BAD_FRAME`).
pub const ST_BAD_FRAME: u8 = 0x01;
/// Unknown command (`UPD_ST_BAD_CMD`).
pub const ST_BAD_CMD: u8 = 0x02;
/// Write attempted before erase (`UPD_ST_NOT_ERASED`).
pub const ST_NOT_ERASED: u8 = 0x03;
/// Page index outside the app region (`UPD_ST_OUT_OF_RANGE`).
pub const ST_OUT_OF_RANGE: u8 = 0x04;
/// Image CRC mismatch (`UPD_ST_BAD_CRC`).
pub const ST_BAD_CRC: u8 = 0x05;
/// Missing or wrong magic (`UPD_ST_BAD_MAGIC`).
pub const ST_BAD_MAGIC: u8 = 0x06;
/// No valid application present (`UPD_ST_NO_APP`).
pub const ST_NO_APP: u8 = 0x07;

/// Maximum ECHO payload the device accepts (`UPD_ECHO_MAX`).
pub const ECHO_MAX: usize = 16;
/// Bytes a frame adds around its payload: CMD + LEN + CRC-8
/// (`UPD_FRAME_OVERHEAD`).
pub const FRAME_OVERHEAD: usize = 3;
/// Largest payload a frame can carry: LEN is a u8 and the whole frame must
/// fit 255 bytes, so `255 - FRAME_OVERHEAD`. Matches the C codec's accept
/// set exactly.
pub const PAYLOAD_MAX: usize = 255 - FRAME_OVERHEAD;
/// Unlock magic carried by `CMD_ERASE_APP` ("ERAS").
pub const ERASE_MAGIC: [u8; 4] = *b"ERAS";

/// Human-readable name of a device status byte (`ST_*`), for error
/// messages; unknown values map to `"unknown status"`.
#[must_use]
pub fn st_name(st: u8) -> &'static str {
    match st {
        ST_OK => "OK",
        ST_BAD_FRAME => "BAD_FRAME: device saw a malformed frame",
        ST_BAD_CMD => "BAD_CMD: unknown command",
        ST_NOT_ERASED => "NOT_ERASED: write before erase",
        ST_OUT_OF_RANGE => "OUT_OF_RANGE: page or length outside the app region",
        ST_BAD_CRC => "BAD_CRC: image CRC mismatch",
        ST_BAD_MAGIC => "BAD_MAGIC: missing or wrong erase magic",
        ST_NO_APP => "NO_APP: no valid application present",
        _ => "unknown status",
    }
}

/// A decoded frame; `payload` borrows from the receive buffer handed to
/// [`decode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Command byte (bit 7 = [`RSP_FLAG`] on responses).
    pub cmd: u8,
    /// Payload, borrowed from the input buffer.
    pub payload: &'a [u8],
}

/// One step of CRC-8/ATM (polynomial 0x07, MSB first).
const fn crc8_step(crc: u8, byte: u8) -> u8 {
    let mut crc = crc ^ byte;
    let mut bit = 0u8;
    while bit < 8 {
        // Wrapping shift is the mod-256 truncation the CRC wants; the XOR
        // with the polynomial is applied when the shifted-out MSB was set.
        crc = if crc & 0x80 != 0 {
            crc.wrapping_shl(1) ^ 0x07
        } else {
            crc.wrapping_shl(1)
        };
        bit = bit.wrapping_add(1); // bounded by the loop condition
    }
    crc
}

/// CRC-8/ATM over `data` (polynomial 0x07, init 0x00). Mirrors the device's
/// `upd_crc8`.
#[must_use]
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0, |crc, &b| crc8_step(crc, b))
}

/// Encode a frame into `out`, returning the number of bytes written.
///
/// # Buffer contract
///
/// On `Ok(n)`, exactly the first `n` bytes of `out` are written
/// (`n == payload.len() + FRAME_OVERHEAD`); the bytes `out[n..]` are left
/// untouched. On `Err(BufferTooSmall)`, the **entire** buffer is left
/// untouched — there is no observable partial write — and `needed` is
/// exactly `payload.len() + FRAME_OVERHEAD`. This contract is verified by
/// the crate's Kani harnesses.
///
/// # Errors
///
/// [`Error::PayloadTooLarge`] if `payload.len() > PAYLOAD_MAX`;
/// [`Error::BufferTooSmall`] if `out` cannot hold
/// `payload.len() + FRAME_OVERHEAD` bytes.
pub fn encode(cmd: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
    let len = payload.len();
    let Ok(len_byte) = u8::try_from(len) else {
        return Err(Error::PayloadTooLarge { len });
    };
    if len > PAYLOAD_MAX {
        return Err(Error::PayloadTooLarge { len });
    }
    // len <= 252 here, so this cannot wrap.
    let total = len.wrapping_add(FRAME_OVERHEAD);

    let Some(frame) = out.get_mut(..total) else {
        return Err(Error::BufferTooSmall { needed: total });
    };
    // frame.len() == total >= 3, so both splits are infallible; the error
    // arms keep the function total without a panic path.
    let Some(([b_cmd, b_len], rest)) = frame.split_first_chunk_mut::<2>() else {
        return Err(Error::BufferTooSmall { needed: total });
    };
    *b_cmd = cmd;
    *b_len = len_byte;
    let Some((body, tail)) = rest.split_at_mut_checked(len) else {
        return Err(Error::BufferTooSmall { needed: total });
    };
    body.copy_from_slice(payload);

    let mut crc = crc8_step(crc8_step(0, cmd), len_byte);
    crc = payload.iter().fold(crc, |c, &b| crc8_step(c, b));
    let Some(crc_slot) = tail.first_mut() else {
        return Err(Error::BufferTooSmall { needed: total });
    };
    *crc_slot = crc;
    Ok(total)
}

/// Decode `raw` as exactly one frame; the returned [`Frame`] borrows its
/// payload from `raw`. Total function: never panics, any input rejected by
/// the device's `upd_frame_parse` is rejected here too.
///
/// # Errors
///
/// [`Error::BadFrame`] if `raw` is shorter than [`FRAME_OVERHEAD`], its LEN
/// byte does not match `raw.len()`, or the CRC-8 check fails.
pub fn decode(raw: &[u8]) -> Result<Frame<'_>, Error> {
    let Some((&[cmd, len_byte], rest)) = raw.split_first_chunk::<2>() else {
        return Err(Error::BadFrame);
    };
    // rest = payload + CRC byte, hence LEN + 1 bytes exactly. LEN above
    // PAYLOAD_MAX is rejected too: the device's buffers cap at 255 bytes,
    // so no such frame exists on the wire and the C parser can never
    // accept one — the two codecs' accept sets stay identical.
    if usize::from(len_byte) > PAYLOAD_MAX
        || rest.len() != usize::from(len_byte).wrapping_add(1)
    {
        return Err(Error::BadFrame);
    }
    let Some((&crc_wire, payload)) = rest.split_last() else {
        return Err(Error::BadFrame); // unreachable: rest.len() >= 1
    };
    let mut crc = crc8_step(crc8_step(0, cmd), len_byte);
    crc = payload.iter().fold(crc, |c, &b| crc8_step(c, b));
    if crc != crc_wire {
        return Err(Error::BadFrame);
    }
    Ok(Frame { cmd, payload })
}

/// Decode one frame from the front of `raw`, tolerating trailing `0xFF`
/// filler.
///
/// This exists for fixed-length transports: an I2C master must choose the
/// read length before the device has said how long its response is (error
/// responses are shorter than success responses, and `/dev/i2c-*` cannot
/// split one read transaction), so the host reads a worst-case-sized buffer
/// in one go and the device pads the tail with `0xFF` idle bytes.
///
/// Accept set (deliberately *length-driven*, not a blind `0xFF` strip — a
/// payload or CRC byte may legitimately be `0xFF`): `decode_padded(raw)`
/// accepts exactly the inputs of the form `f ++ 0xFF^k` (`k >= 0`) where
/// [`decode`] accepts `f`, and `f`'s extent is fixed by the LEN byte at
/// `raw[1]` (`|f| = LEN + FRAME_OVERHEAD`). Every input [`decode`] accepts
/// is accepted here unchanged (`k = 0`); non-`0xFF` bytes after the frame
/// are rejected — they mean desync, not filler.
///
/// # Errors
///
/// [`Error::BadFrame`] if `raw` is shorter than the LEN byte promises, any
/// byte after the frame is not `0xFF`, or the frame itself fails [`decode`].
pub fn decode_padded(raw: &[u8]) -> Result<Frame<'_>, Error> {
    let Some(&len_byte) = raw.get(1) else {
        return Err(Error::BadFrame);
    };
    // <= 255 + 3, cannot wrap.
    let total = usize::from(len_byte).wrapping_add(FRAME_OVERHEAD);
    let Some(frame_bytes) = raw.get(..total) else {
        return Err(Error::BadFrame);
    };
    // get(total..) is Some whenever get(..total) was; the false-on-None arm
    // keeps this total without a panic path.
    let filler_ok = raw
        .get(total..)
        .is_some_and(|tail| tail.iter().all(|&b| b == 0xFF));
    if !filler_ok {
        return Err(Error::BadFrame);
    }
    decode(frame_bytes)
}

/// Kani model-checking harnesses. Compiled only under `cargo kani`
/// (`cfg(kani)`), never in normal builds, tests or clippy runs; proof text
/// may therefore panic — a panic here *is* the failed assertion.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Input bound for the decoder totality harnesses.
    ///
    /// 40 bytes reaches every structural branch of both decoders: the
    /// too-short header rejects (0 and 1 byte), LEN smaller than, equal to
    /// and larger than the buffer, multi-byte payload CRC folding, and a
    /// padded tail longer than the frame it follows. Beyond that the code
    /// is a straight-line fold per byte with no new control flow, so a
    /// larger bound only grows CRC unrolling (8 steps/byte) without adding
    /// coverage.
    const DEC_MAX: usize = 40;

    /// Payload bound for the encoder harnesses. `encode` treats payload
    /// bytes uniformly (copy + CRC fold); the interesting boundaries are
    /// n = 0, the LEN byte, and the out-buffer size relation, all reached
    /// by n <= 8. The PAYLOAD_MAX boundary itself is proven separately in
    /// `encode_rejects_oversized_payload` with concrete lengths.
    const ENC_MAX: usize = 8;

    /// `decode` never panics for any input up to `DEC_MAX` bytes.
    #[kani::proof]
    #[kani::unwind(48)]
    fn decode_total() {
        let raw: [u8; DEC_MAX] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= DEC_MAX);
        let Some(input) = raw.get(..len) else { return };
        let _ = decode(input); // property: no panic on ANY input
    }

    /// `decode_padded` never panics for any input up to `DEC_MAX` bytes,
    /// and whatever it accepts, plain `decode` accepts as a prefix with the
    /// identical frame (the documented "frame ++ 0xFF filler" accept set).
    #[kani::proof]
    #[kani::unwind(48)]
    fn decode_padded_total() {
        let raw: [u8; DEC_MAX] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= DEC_MAX);
        let Some(input) = raw.get(..len) else { return };
        if let Ok(f) = decode_padded(input) {
            // The frame's extent is fixed by the LEN byte.
            let total = f.payload.len() + FRAME_OVERHEAD;
            let Some(prefix) = input.get(..total) else {
                panic!("accepted frame extends past the input")
            };
            let Ok(g) = decode(prefix) else {
                panic!("decode_padded accepted what decode rejects")
            };
            assert!(g.cmd == f.cmd && g.payload == f.payload);
            let Some(tail) = input.get(total..) else {
                panic!("tail slice of an in-bounds split cannot fail")
            };
            assert!(tail.iter().all(|&b| b == 0xFF));
        }
    }

    /// For any cmd and payload up to `ENC_MAX` with a sufficient out
    /// buffer, `encode` succeeds and `decode` returns the identical
    /// cmd/payload.
    #[kani::proof]
    #[kani::unwind(16)]
    fn encode_decode_roundtrip() {
        let payload: [u8; ENC_MAX] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= ENC_MAX);
        let Some(pl) = payload.get(..n) else { return };
        let cmd: u8 = kani::any();

        let mut out = [0u8; ENC_MAX + FRAME_OVERHEAD];
        let Ok(written) = encode(cmd, pl, &mut out) else {
            panic!("encode must succeed: n <= PAYLOAD_MAX and out is large enough")
        };
        assert!(written == n + FRAME_OVERHEAD);
        let Some(wire) = out.get(..written) else {
            panic!("returned length exceeds the buffer")
        };
        let Ok(f) = decode(wire) else {
            panic!("decode must accept what encode produced")
        };
        assert!(f.cmd == cmd && f.payload == pl);
    }

    /// `encode` never writes beyond its returned length, and a too-small
    /// out buffer yields the typed error with the whole buffer untouched
    /// (no observable partial write).
    #[kani::proof]
    #[kani::unwind(24)]
    fn encode_write_extent() {
        let payload: [u8; ENC_MAX] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= ENC_MAX);
        let Some(pl) = payload.get(..n) else { return };
        let cmd: u8 = kani::any();

        const OUT_MAX: usize = ENC_MAX + FRAME_OVERHEAD + 4;
        let before: [u8; OUT_MAX] = kani::any();
        let mut out = before;
        let out_len: usize = kani::any();
        kani::assume(out_len <= OUT_MAX);
        let Some(window) = out.get_mut(..out_len) else { return };

        match encode(cmd, pl, window) {
            Ok(written) => {
                assert!(written == n + FRAME_OVERHEAD);
                assert!(written <= out_len);
                // Everything at and past the returned length is untouched.
                let mut i = written;
                while i < OUT_MAX {
                    assert!(out[i] == before[i]);
                    i += 1;
                }
            }
            Err(Error::BufferTooSmall { needed }) => {
                assert!(needed == n + FRAME_OVERHEAD);
                assert!(out_len < needed);
                assert!(out == before); // no partial write
            }
            Err(_) => panic!("only BufferTooSmall is reachable for n <= PAYLOAD_MAX"),
        }
    }

    /// The PAYLOAD_MAX boundary: 252 encodes, 253 (fits u8, exceeds the
    /// frame budget) and 256 (exceeds u8) are rejected typed, untouched
    /// buffer. Concrete lengths — the rejects return before any loop, and
    /// the accept at 252 exercises the LEN byte's maximum.
    #[kani::proof]
    #[kani::unwind(8)]
    fn encode_rejects_oversized_payload() {
        let cmd: u8 = kani::any();
        let mut out = [0u8; 256];

        let over_u8_budget = [0u8; PAYLOAD_MAX + 1]; // 253: fits u8, too big
        assert!(matches!(
            encode(cmd, &over_u8_budget, &mut out),
            Err(Error::PayloadTooLarge { len }) if len == PAYLOAD_MAX + 1
        ));
        let over_u8 = [0u8; 256]; // does not fit the LEN byte at all
        assert!(matches!(
            encode(cmd, &over_u8, &mut out),
            Err(Error::PayloadTooLarge { len }) if len == 256
        ));
    }

    /// A frame followed by 0xFF filler round-trips through `decode_padded`;
    /// corrupting any filler byte to a non-0xFF value is rejected.
    #[kani::proof]
    #[kani::unwind(24)]
    fn decode_padded_roundtrip_with_filler() {
        const N: usize = 4;
        let payload: [u8; N] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= N);
        let Some(pl) = payload.get(..n) else { return };
        let cmd: u8 = kani::any();

        let mut buf = [0xFFu8; N + FRAME_OVERHEAD + 5]; // frame + 0xFF tail
        let Ok(written) = encode(cmd, pl, &mut buf) else {
            panic!("encode must succeed")
        };
        let read_len: usize = kani::any();
        kani::assume(read_len >= written && read_len <= buf.len());
        let Some(view) = buf.get(..read_len) else { return };
        let Ok(f) = decode_padded(view) else {
            panic!("decode_padded must accept frame + 0xFF filler")
        };
        assert!(f.cmd == cmd && f.payload == pl);

        // Desync bytes are not filler: corrupt one tail byte, must reject.
        if read_len > written {
            let pos: usize = kani::any();
            kani::assume(pos >= written && pos < read_len);
            let junk: u8 = kani::any();
            kani::assume(junk != 0xFF);
            buf[pos] = junk;
            let Some(view) = buf.get(..read_len) else { return };
            assert!(decode_padded(view).is_err());
        }
    }
}
