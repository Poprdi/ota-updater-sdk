//! Host-side stream framing: the byte-exact mirror of the device's
//! `link_stream.c` receive machine, shared by every byte-stream transport
//! (UART, SPI, bit-banged GPIO). I2C is transactional and does not use it.
//!
//! Wire binding (`device/include/updater/link.h`): a [`SYNC`] byte
//! (`0x7E`), then the standard frame verbatim (`CMD LEN payload CRC8`).
//! Parsing is length-driven after sync acquisition, so `0x7E` bytes inside
//! a frame are harmless; hunting only happens between frames. A frame that
//! cannot fit the buffer, declares an impossible LEN, or fails validation
//! is dropped silently and the machine re-hunts — the request/response
//! protocol's retry discipline owns loss recovery, exactly as on the
//! device.
//!
//! The scanner is sans-I/O on purpose: transports of any shape (blocking
//! reads, byte-at-a-time SPI exchanges, bit-banged pins) push received
//! bytes one at a time and act on the returned [`Scan`]. That keeps the
//! sync-hunt/LEN-completion logic in exactly one place, `no_std` and
//! panic-free, with the same Kani coverage discipline as the rest of this
//! crate.
//!
//! SPI note: the device shifts out `0x00` while idle or busy, and the
//! slave-side one-byte lag means at least one stale byte precedes the sync
//! on MISO. Both are ordinary hunt fodder here — no special handling.

use crate::frame::{self, FRAME_OVERHEAD, PAYLOAD_MAX};

/// Stream sync byte (`UPD_LINK_SYNC`): precedes every frame on the wire.
pub const SYNC: u8 = 0x7E;

/// Outcome of feeding one received byte to [`RxScanner::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    /// Byte discarded while hunting for [`SYNC`].
    Hunt,
    /// Byte consumed; a frame is being assembled (or was just started).
    Frame,
    /// A complete, CRC-valid frame now occupies `buf[..len]`.
    Done {
        /// Frame length in bytes (`LEN + FRAME_OVERHEAD`).
        len: usize,
    },
    /// A declared frame was dropped — it could not fit `buf`, declared an
    /// impossible LEN, or completed but failed validation. Hunting resumed.
    Dropped,
}

/// Incremental receive scanner: push bytes as they arrive, get told when a
/// valid frame is complete.
///
/// Frame bytes are written to the front of the caller's buffer, which must
/// be the **same** buffer for every push of one frame (handing in a
/// shorter one mid-frame resolves as [`Scan::Dropped`], never a panic) and
/// at least [`FRAME_OVERHEAD`] bytes long, or no sync is ever accepted.
/// Size it for the largest response expected plus slack for hunting
/// through garbage — the session's receive window already satisfies this.
///
/// The state machine and its drop rules mirror `link_stream.c` case by
/// case; see the module docs for the shared wire binding.
#[derive(Debug, Clone, Default)]
pub struct RxScanner {
    /// Bytes assembled since the last sync.
    n: usize,
    /// Sync seen; accumulating length-driven.
    in_frame: bool,
}

impl RxScanner {
    /// A fresh scanner, hunting for sync.
    #[must_use]
    pub const fn new() -> Self {
        Self { n: 0, in_frame: false }
    }

    /// Abandon any partial frame and hunt for the next sync.
    pub fn reset(&mut self) {
        self.n = 0;
        self.in_frame = false;
    }

    /// Is a frame currently being assembled? (Diagnostic — transports use
    /// it to distinguish a torn response from pure silence.)
    #[must_use]
    pub const fn in_frame(&self) -> bool {
        self.in_frame
    }

    /// Feed one received byte; frame bytes land at the front of `buf`.
    ///
    /// Returns [`Scan::Done`] exactly when `buf[..len]` holds a complete
    /// frame accepted by [`frame::decode`]. Total: no input, state or
    /// buffer can make it panic.
    pub fn push(&mut self, byte: u8, buf: &mut [u8]) -> Scan {
        if !self.in_frame {
            // A buffer below the 3-byte frame minimum can never complete a
            // frame; refusing sync keeps every later write in bounds.
            if byte == SYNC && buf.len() >= FRAME_OVERHEAD {
                self.in_frame = true;
                self.n = 0;
                return Scan::Frame;
            }
            return Scan::Hunt;
        }

        let Some(slot) = buf.get_mut(self.n) else {
            // Unreachable while the caller keeps one buffer per frame (the
            // invariant below bounds n); a swapped-in shorter buffer lands
            // here and resolves as a drop, keeping push total.
            self.reset();
            return Scan::Dropped;
        };
        *slot = byte;
        // n < buf.len() (get_mut succeeded), so this cannot wrap.
        self.n = self.n.wrapping_add(1);

        if self.n < 2 {
            return Scan::Frame; // LEN not known yet
        }
        let Some(&len_byte) = buf.get(1) else {
            self.reset(); // unreachable: n >= 2 implies buf.len() >= 2
            return Scan::Dropped;
        };
        let declared = usize::from(len_byte);
        // <= 255 + 3: cannot wrap.
        let total = declared.wrapping_add(FRAME_OVERHEAD);
        // LEN above PAYLOAD_MAX can never validate (the device's u8 buffer
        // drops it structurally — total > 255 >= buf_len; a big host
        // buffer must drop it deliberately to keep the accept sets equal).
        if declared > PAYLOAD_MAX || total > buf.len() {
            self.reset();
            return Scan::Dropped;
        }
        // Invariant on every Frame return below: n < total <= buf.len(),
        // so the next push's get_mut succeeds.
        if self.n < total {
            return Scan::Frame;
        }

        // Frame complete, valid or not: hunting resumes either way.
        self.reset();
        let Some(raw) = buf.get(..total) else {
            return Scan::Dropped; // unreachable: total <= buf.len()
        };
        if frame::decode(raw).is_ok() {
            Scan::Done { len: total }
        } else {
            // CRC/shape failure: drop silently — retry owns recovery, a
            // stream-level ACK would duplicate the CRC layer.
            Scan::Dropped
        }
    }
}

/// Kani model-checking harnesses. Compiled only under `cargo kani`
/// (`cfg(kani)`), never in normal builds, tests or clippy runs; proof text
/// may therefore panic — a panic here *is* the failed assertion.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Buffer bound: 8 bytes reaches every structural branch — sync
    /// refusal below FRAME_OVERHEAD, the LEN-fit drop, completion, and the
    /// defensive out-of-bounds arm (forged n). Larger buffers only add
    /// uniform per-byte copies.
    const BUF_MAX: usize = 8;

    /// `push` never panics for ANY scanner state (including states no
    /// legal call sequence produces — the buffer-swap case), any byte and
    /// any buffer up to `BUF_MAX`.
    #[kani::proof]
    #[kani::unwind(80)]
    fn push_total_any_state() {
        let mut sc = RxScanner { n: kani::any(), in_frame: kani::any() };
        let mut buf: [u8; BUF_MAX] = kani::any();
        let blen: usize = kani::any();
        kani::assume(blen <= BUF_MAX);
        let byte: u8 = kani::any();
        let Some(window) = buf.get_mut(..blen) else { return };
        let _ = sc.push(byte, window); // property: no panic
    }

    /// Whatever `push` reports Done for is exactly a frame `decode`
    /// accepts, its extent fixed by the LEN byte, and the scanner is back
    /// to hunting.
    #[kani::proof]
    #[kani::unwind(80)]
    fn done_is_decodable() {
        let mut sc = RxScanner { n: kani::any(), in_frame: kani::any() };
        let mut buf: [u8; BUF_MAX] = kani::any();
        let blen: usize = kani::any();
        kani::assume(blen <= BUF_MAX);
        let byte: u8 = kani::any();
        let Some(window) = buf.get_mut(..blen) else { return };

        if let Scan::Done { len } = sc.push(byte, window) {
            let Some(raw) = window.get(..len) else {
                panic!("Done extends past the buffer")
            };
            let Ok(f) = frame::decode(raw) else {
                panic!("Done must imply a decodable frame")
            };
            assert!(len == f.payload.len() + FRAME_OVERHEAD);
            assert!(!sc.in_frame());
        }
    }

    /// End-to-end accept: from a fresh scanner, a non-sync garbage prefix
    /// followed by SYNC + a well-formed encoded frame is accepted byte for
    /// byte, and nothing before the final byte completes.
    #[kani::proof]
    #[kani::unwind(80)]
    fn garbage_then_frame_roundtrip() {
        const PL_MAX: usize = 3;
        let payload: [u8; PL_MAX] = kani::any();
        let n: usize = kani::any();
        kani::assume(n <= PL_MAX);
        let Some(pl) = payload.get(..n) else { return };
        let cmd: u8 = kani::any();

        let mut wire = [0u8; PL_MAX + FRAME_OVERHEAD];
        let Ok(flen) = frame::encode(cmd, pl, &mut wire) else {
            panic!("encode must succeed for n <= PAYLOAD_MAX")
        };

        let mut sc = RxScanner::new();
        let mut buf = [0u8; PL_MAX + FRAME_OVERHEAD];

        // Bounded garbage prefix, no sync bytes in it.
        let garbage: [u8; 2] = kani::any();
        let glen: usize = kani::any();
        kani::assume(glen <= 2);
        let mut i = 0;
        while i < glen {
            kani::assume(garbage[i] != SYNC);
            assert!(matches!(sc.push(garbage[i], &mut buf), Scan::Hunt));
            i += 1;
        }

        assert!(matches!(sc.push(SYNC, &mut buf), Scan::Frame));
        let mut j = 0;
        while j < flen {
            match sc.push(wire[j], &mut buf) {
                Scan::Frame => assert!(j + 1 < flen, "premature staying in frame"),
                Scan::Done { len } => {
                    assert!(j + 1 == flen, "completion must land on the last byte");
                    assert!(len == flen);
                    assert!(buf[..len] == wire[..flen]);
                }
                _ => panic!("frame bytes must be consumed as frame bytes"),
            }
            j += 1;
        }
    }
}
