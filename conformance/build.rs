//! Compiles the REAL device core (device/core/*.c — the same four files the
//! AVR port links) plus the simulated port into this crate. Flags mirror
//! device/Makefile so the conformance build is as strict as the device
//! build; assert() stays live (no NDEBUG) — the sim's confinement belt.

fn main() {
    let core = ["crc8.c", "crc32.c", "proto.c", "update.c"];

    let mut build = cc::Build::new();
    for f in core {
        let path = format!("../device/core/{f}");
        println!("cargo:rerun-if-changed={path}");
        build.file(path);
    }
    println!("cargo:rerun-if-changed=sim/sim_port.c");
    println!("cargo:rerun-if-changed=sim/sim_port.h");
    println!("cargo:rerun-if-changed=../device/include/updater");
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
