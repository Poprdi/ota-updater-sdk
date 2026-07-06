// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Adversarial campaigns through the REAL `updater_core::Session` driving
//! the REAL C core over simulated flash — the no-brick property and the
//! full-update happy path, demonstrated end to end.

use std::convert::Infallible;

use conformance::{Sim, APP_PAGES, PAGE_SIZE, REGION};
use updater_core::frame::{
    self, CMD_ERASE_APP, CMD_VERIFY, CMD_WRITE_PAGE, ERASE_MAGIC, RSP_FLAG,
    ST_BAD_CRC, ST_NO_APP, ST_OK, ST_OUT_OF_RANGE,
};
use updater_core::image::Image;
use updater_core::{Error, Session};

/// Deterministic pseudo-random bytes (xorshift32, fixed seed): the campaign
/// image must be reproducible run to run.
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

/// The campaign app: 3000 pseudo-random bytes (seed constant per the brief).
fn new_app() -> Vec<u8> {
    prng_bytes(0x2A2A_2A2A, 3000)
}

/// A different, older app for power-cut scenarios that must start from a
/// bootable device.
fn old_app() -> Vec<u8> {
    prng_bytes(0x0BAD_F00D, 2500)
}

fn image(data: &[u8]) -> Image<'_> {
    Image::from_bin(data, PAGE_SIZE as u16, APP_PAGES as u16).expect("valid geometry")
}

fn try_flash(sim: &Sim, img: &Image<'_>) -> Result<(), Error<Infallible>> {
    let mut buf = [0u8; 320];
    Session::new(sim.transport(), &mut buf).flash(img, &mut |_, _| {})
}

fn flash_ok(sim: &Sim, img: &Image<'_>) {
    try_flash(sim, img).expect("clean flash must succeed");
}

fn try_boot(sim: &Sim) -> Result<(), Error<Infallible>> {
    let mut buf = [0u8; 320];
    Session::new(sim.transport(), &mut buf).boot()
}

/// What flash must hold after `img` is fully written: every page as the
/// shipped `page_into` renders it (data, 0xFF padding, footer overlay).
fn expected_flash(img: &Image<'_>) -> Vec<u8> {
    let mut out = vec![0u8; REGION];
    for p in 0..APP_PAGES {
        let page = u16::try_from(p).unwrap();
        img.page_into(page, &mut out[p * PAGE_SIZE..][..PAGE_SIZE]).unwrap();
    }
    out
}

/// Raw exchange helper (shipped codec, no session): returns the response
/// payload, asserting the response frame is valid and command-matched.
fn raw(sim: &Sim, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = [0u8; 256];
    let n = frame::encode(cmd, payload, &mut buf).expect("request encodes");
    let rsp = sim.request(&buf[..n]);
    let f = frame::decode(&rsp).expect("response decodes");
    assert_eq!(f.cmd, cmd | RSP_FLAG);
    f.payload.to_vec()
}

#[test]
fn full_update_campaign() {
    let sim = Sim::acquire();
    let data = new_app();
    let img = image(&data);

    let mut buf = [0u8; 320];
    let mut session = Session::new(sim.transport(), &mut buf);

    let info = session.info().unwrap();
    assert!(!info.app_valid, "blank flash cannot hold a valid app");

    // Flash with progress accounting: (0, total) up front, then one bump
    // per written page, ending at (total, total).
    let mut calls: Vec<(u16, u16)> = Vec::new();
    session.flash(&img, &mut |done, total| calls.push((done, total))).unwrap();
    let total = u16::try_from(img.pages().count()).unwrap();
    assert_eq!(calls.first(), Some(&(0, total)));
    assert_eq!(calls.last(), Some(&(total, total)));
    assert_eq!(calls.len(), usize::from(total) + 1);

    // The C side's flash is byte-identical to what the Rust image layer
    // says the device must hold.
    assert_eq!(sim.flash_snapshot(), expected_flash(&img));

    let info = session.info().unwrap();
    assert!(info.app_valid, "flashed + verified image must satisfy the boot gate");

    session.boot().expect("boot must be accepted");
    assert!(sim.jumped(), "BOOT gate must fire after the OK reply");
}

#[test]
fn power_cut_sweep_every_flash_op() {
    let sim = Sim::acquire();
    let old_data = old_app();
    let new_data = new_app();
    let old = image(&old_data);
    let new = image(&new_data);

    // Calibrate: ops of one clean update from a device that already runs
    // the old app. Every page op counts — app_pages erases plus one write
    // per yielded page — so the sweep provably covers the erase phase too.
    sim.reset(false);
    flash_ok(&sim, &old);
    sim.reset(true); // reboot into the updater with the old app in flash
    flash_ok(&sim, &new);
    let total = sim.flash_ops();
    let writes = u32::try_from(new.pages().count()).unwrap();
    assert_eq!(total, APP_PAGES as u32 + writes, "op census: erases + writes");

    for n in 1..=total {
        // Device runs the old app; an update tears at flash op n.
        sim.reset(false);
        flash_ok(&sim, &old);
        sim.reset(true);
        sim.powercut_after(n);
        let res = try_flash(&sim, &new);
        assert!(res.is_err(), "update cut at op {n} must report an error");
        assert!(sim.powercut_hit(), "op index {n} must actually be reached");

        // Power back on, flash preserved exactly as the cut left it. The
        // boot gate must refuse: the cut lands MID-op, so no op index
        // leaves a valid image — erase cuts destroy the old image before
        // the new footer lands (erase runs first, ascending, and any torn
        // page fails the old CRC), and the final footer write itself tears
        // with the footer bytes (last 16 of the page) still 0xFF. The
        // brief's "unless the cut fell after the final footer write" branch
        // is therefore empty under mid-op tear semantics: one op later is
        // no cut at all, which is the clean-flash case already covered.
        sim.reset(true);
        let refused = try_boot(&sim);
        assert!(
            matches!(refused, Err(Error::Device(st)) if st == ST_NO_APP),
            "boot after cut at op {n} must be refused with ST_NO_APP, got {refused:?}"
        );
        assert!(!sim.jumped(), "gate must not fire on a torn image (op {n})");

        // Recovery: the device is alive and reflashable — the no-brick
        // property. A clean re-flash must fully recover.
        flash_ok(&sim, &new);
        try_boot(&sim).expect("boot after recovery re-flash");
        assert!(sim.jumped(), "recovered image must boot (op {n})");
    }
}

#[test]
fn corrupt_byte_boot_refused() {
    let sim = Sim::acquire();
    let data = new_app();
    let img = image(&data);
    flash_ok(&sim, &img);

    // One flipped bit inside the measured [0, len) range, post-verify.
    sim.flash_xor(1500, 0x40);

    // VERIFY sees it (spec: recompute CRC-32 over [0, length))...
    let mut vp = [0u8; 8];
    vp[..4].copy_from_slice(&img.len().to_le_bytes());
    vp[4..].copy_from_slice(&img.crc32().to_le_bytes());
    assert_eq!(raw(&sim, CMD_VERIFY, &vp), [ST_BAD_CRC]);

    // ...and the boot gate independently refuses.
    let refused = try_boot(&sim);
    assert!(matches!(refused, Err(Error::Device(st)) if st == ST_NO_APP));
    assert!(!sim.jumped());
}

#[test]
fn out_of_order_and_duplicate_writes_still_verify() {
    let sim = Sim::acquire();
    let data = new_app();
    let img = image(&data);

    assert_eq!(raw(&sim, CMD_ERASE_APP, &ERASE_MAGIC), [ST_OK]);

    // Footer page first, then every data page in DESCENDING order, then
    // duplicates: page order must not matter (idempotent, order-free).
    let mut order: Vec<u16> = vec![img.footer_page_index()];
    let mut pages: Vec<u16> = img.pages().collect();
    pages.reverse();
    order.extend(&pages);
    order.push(pages[pages.len() - 1]); // duplicate first data page
    order.push(img.footer_page_index()); // duplicate footer page

    for index in order {
        let mut payload = vec![0u8; 2 + PAGE_SIZE];
        payload[..2].copy_from_slice(&index.to_le_bytes());
        img.page_into(index, &mut payload[2..]).unwrap();
        assert_eq!(raw(&sim, CMD_WRITE_PAGE, &payload), [ST_OK], "page {index}");
    }

    let mut vp = [0u8; 8];
    vp[..4].copy_from_slice(&img.len().to_le_bytes());
    vp[4..].copy_from_slice(&img.crc32().to_le_bytes());
    assert_eq!(raw(&sim, CMD_VERIFY, &vp), [ST_OK], "image must verify");

    assert_eq!(sim.flash_snapshot(), expected_flash(&img));
    try_boot(&sim).expect("out-of-order image must boot");
    assert!(sim.jumped());
}

#[test]
fn out_of_range_boundary_pinned_through_ffi() {
    // Wellformed (framing-valid) requests whose PARAMETERS are out of range
    // must be answered with ST_OUT_OF_RANGE by the real C core — a core
    // that dropped only the range checks would still pass every other
    // campaign here, so the fence is pinned explicitly, from both sides.
    let sim = Sim::acquire();

    assert_eq!(raw(&sim, CMD_ERASE_APP, &ERASE_MAGIC), [ST_OK]);
    let erased = sim.flash_snapshot();

    // WRITE_PAGE idx = APP_PAGES (32, LE [32, 0]): first index past the
    // app region. Wellformed frame, full 128-byte payload.
    let mut payload = vec![0xA5u8; 2 + PAGE_SIZE];
    payload[..2].copy_from_slice(&u16::try_from(APP_PAGES).unwrap().to_le_bytes());
    assert_eq!(raw(&sim, CMD_WRITE_PAGE, &payload), [ST_OUT_OF_RANGE]);
    assert_eq!(sim.flash_snapshot(), erased, "rejected write must not touch flash");

    // VERIFY length = region - 15: one past the region-16 bound (the last
    // 16 bytes are the footer, never part of the measured image). Any CRC —
    // the length gate must fire before the CRC is even computed.
    let mut vp = [0u8; 8];
    vp[..4].copy_from_slice(&u32::try_from(REGION - 15).unwrap().to_le_bytes());
    vp[4..].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
    assert_eq!(raw(&sim, CMD_VERIFY, &vp), [ST_OUT_OF_RANGE]);

    // Boundary companion: idx = APP_PAGES - 1 (31) is the last valid page
    // and must be accepted — the fence sits exactly at APP_PAGES.
    payload[..2].copy_from_slice(&u16::try_from(APP_PAGES - 1).unwrap().to_le_bytes());
    assert_eq!(raw(&sim, CMD_WRITE_PAGE, &payload), [ST_OK]);
    let snap = sim.flash_snapshot();
    assert_eq!(&snap[(APP_PAGES - 1) * PAGE_SIZE..], vec![0xA5u8; PAGE_SIZE]);
}

#[test]
fn footer_tampering_refused_per_field() {
    // Each footer field sabotaged separately, from a fresh valid image:
    // magic (+0), length (+4), crc32 (+8). All must yield ST_NO_APP.
    let footer_base = REGION - 16;
    for (name, offset) in [("magic", 0usize), ("length", 4), ("crc32", 8)] {
        let sim = Sim::acquire();
        let data = new_app();
        let img = image(&data);
        flash_ok(&sim, &img);

        sim.flash_xor(footer_base + offset, 0x01);
        let refused = try_boot(&sim);
        assert!(
            matches!(refused, Err(Error::Device(st)) if st == ST_NO_APP),
            "tampered footer {name} must refuse boot, got {refused:?}"
        );
        assert!(!sim.jumped(), "gate fired despite tampered {name}");
    }
}
