//! FFI to the simulated device (`sim/sim_port.c` + the real `device/core`)
//! and the safe wrappers the conformance tests use.
//!
//! # Safety model
//!
//! The C sim is a block of static state, so all access must be serialized.
//! [`Sim::acquire`] takes a process-wide mutex and is the ONLY way to reach
//! the FFI: every entry point is a method on the guard, so the borrow
//! checker + mutex together make racing the sim impossible from safe code.
//! `cargo test` runs test *threads* in parallel within a binary (serialized
//! here) and test *binaries* as separate processes (each gets its own copy
//! of the C statics) — both cases are sound.
//!
//! This crate is dev-only; the `unsafe` below is permitted here and nowhere
//! else in the SDK. The shipped crates remain `#![forbid(unsafe_code)]`.

use std::convert::Infallible;
use std::sync::{Mutex, MutexGuard};

use updater_core::Transport;

/// Flash page size of the simulated device (matches device test fixtures).
pub const PAGE_SIZE: usize = 128;
/// App-region page count of the simulated device.
pub const APP_PAGES: usize = 32;
/// App-region size in bytes.
pub const REGION: usize = PAGE_SIZE * APP_PAGES;
/// `sim_request` contract: the response buffer must hold at least this.
const RESP_CAP: usize = 259;

extern "C" {
    fn sim_reset(preserve_flash: bool);
    fn sim_powercut_after(flash_ops: u32);
    fn sim_powercut_hit() -> bool;
    fn sim_flash_ops() -> u32;
    fn sim_request(frame: *const u8, len: u16, resp: *mut u8) -> u16;
    fn sim_jumped() -> bool;
    fn sim_flash() -> *mut u8;
}

static SIM_MUTEX: Mutex<()> = Mutex::new(());

/// Exclusive handle to the simulated device. All sim access goes through
/// methods on this guard; holding it serializes the C static state.
pub struct Sim {
    _guard: MutexGuard<'static, ()>,
}

impl Sim {
    /// Lock the sim and hand it over factory-fresh: flash wiped to 0xFF,
    /// session re-initialized, no power cut armed.
    ///
    /// A previous test panicking while holding the lock poisons the mutex;
    /// the state is recovered here because acquisition always resets.
    pub fn acquire() -> Self {
        let guard = SIM_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let sim = Self { _guard: guard };
        sim.reset(false);
        sim
    }

    /// Reboot the device; `preserve_flash` keeps flash contents (power-loss
    /// recovery scenario), `false` wipes to factory-fresh 0xFF.
    pub fn reset(&self, preserve_flash: bool) {
        unsafe { sim_reset(preserve_flash) }
    }

    /// Arm a power cut on the n-th flash op from now (0 disarms).
    pub fn powercut_after(&self, flash_ops: u32) {
        unsafe { sim_powercut_after(flash_ops) }
    }

    /// Did the armed cut fire since the last reset?
    pub fn powercut_hit(&self) -> bool {
        unsafe { sim_powercut_hit() }
    }

    /// Flash ops (erases + writes) since the last reset.
    pub fn flash_ops(&self) -> u32 {
        unsafe { sim_flash_ops() }
    }

    /// Has the BOOT gate fired since the last reset?
    pub fn jumped(&self) -> bool {
        unsafe { sim_jumped() }
    }

    /// One raw request/response cycle through the C parse→handle→build
    /// path, returning exactly the bytes the device produced (empty when
    /// the device is dead or has jumped). No padding — transports add that.
    pub fn request(&self, frame: &[u8]) -> Vec<u8> {
        let mut resp = [0u8; RESP_CAP];
        let len = u16::try_from(frame.len()).expect("request frames are < 64 KiB");
        let n = unsafe { sim_request(frame.as_ptr(), len, resp.as_mut_ptr()) };
        resp[..usize::from(n)].to_vec()
    }

    /// Copy of the whole app-region flash.
    pub fn flash_snapshot(&self) -> Vec<u8> {
        unsafe { std::slice::from_raw_parts(sim_flash(), REGION) }.to_vec()
    }

    /// XOR one flash byte in place — corruption injection.
    ///
    /// # Panics
    ///
    /// If `offset` is outside the app region.
    pub fn flash_xor(&self, offset: usize, mask: u8) {
        assert!(offset < REGION, "corruption offset out of range");
        unsafe {
            let flash = std::slice::from_raw_parts_mut(sim_flash(), REGION);
            flash[offset] ^= mask;
        }
    }

    /// A [`Transport`] over this sim, borrowing the handle. A shared borrow
    /// suffices: every sim entry point takes `&self` (the C side owns the
    /// mutation) and the mutex in [`Sim::acquire`] already guarantees the
    /// process-wide exclusivity that makes those calls sound.
    pub fn transport(&self) -> SimTransport<'_> {
        SimTransport { sim: self }
    }
}

/// The loopback transport: models a fixed-length I2C master read. The
/// device's response goes to the front of `rsp` and the ENTIRE remainder is
/// filled with `0xFF` idle bytes, returning `rsp.len()` — so every
/// conformance exchange goes through `decode_padded`, pinning the
/// device-side 0xFF-filler contract the stream transports rely on.
pub struct SimTransport<'a> {
    sim: &'a Sim,
}

impl Transport for SimTransport<'_> {
    type Err = Infallible;

    fn request(&mut self, req: &[u8], rsp: &mut [u8]) -> Result<usize, Infallible> {
        let raw = self.sim.request(req);
        // An I2C read clocks exactly rsp.len() bytes no matter how long the
        // device's answer is: truncate if oversized, pad with 0xFF idle
        // bytes otherwise. A dead device reads as all-0xFF (pulled-up bus).
        let n = raw.len().min(rsp.len());
        rsp[..n].copy_from_slice(&raw[..n]);
        for b in &mut rsp[n..] {
            *b = 0xFF;
        }
        Ok(rsp.len())
    }
}
