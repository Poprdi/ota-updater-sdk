// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Adrian Erlacher

//! Compiles the REAL device core (device/core/*.c — the same files the
//! AVR ports link, link_stream.c included for the stream path) plus the
//! simulated port into this crate. Flags mirror device/Makefile so the
//! conformance build is as strict as the device build; assert() stays
//! live (no NDEBUG) — the sim's confinement belt.

fn main() {
    let core = ["crc8.c", "crc32.c", "proto.c", "update.c", "link_stream.c"];

    let mut build = cc::Build::new();
    for f in core {
        let path = format!("../device/core/{f}");
        println!("cargo:rerun-if-changed={path}");
        build.file(path);
    }
    println!("cargo:rerun-if-changed=sim/sim_port.c");
    println!("cargo:rerun-if-changed=sim/sim_port.h");
    for h in ["crc8.h", "crc32.h", "link.h", "port.h", "proto.h", "update.h"] {
        println!("cargo:rerun-if-changed=../device/include/updater/{h}");
    }
    build
        .file("sim/sim_port.c")
        .include("../device/include")
        .include("sim")
        .flag("-std=c11")
        .flag("-Wall")
        .flag("-Wextra")
        .flag("-Werror")
        .flag("-pedantic")
        .flag("-Wconversion")
        .compile("updater_sim");
}
