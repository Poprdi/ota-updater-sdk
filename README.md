# OTA Updater SDK

A formally verified, transport-agnostic OTA update SDK for microcontrollers
behind a master CPU.

[![CI](https://github.com/Poprdi/ota-updater-sdk/actions/workflows/ci.yml/badge.svg)](https://github.com/Poprdi/ota-updater-sdk/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

The device side is a portable C11 bootloader core whose safety properties are
machine-proven — 534 Frama-C/WP goals and 2,166 CBMC checks, zero unproven —
behind thin, audited per-MCU ports (shipped: AVR64EA28 over I2C and UART,
RP2350 / Pico 2 W over UART). The host side is `no_std`, zero-alloc Rust:
`updater-core` has zero dependencies, `updater-eh` adds only the
`embedded-hal`/`embedded-io` trait crates, and `updater-cli` drives it all
from Linux — the identical update code runs on a Raspberry Pi, a Pico 2 W or
an ESP32. A conformance harness runs the real Rust client against the real C
core on every push, and the stack is validated on silicon, including a real
torn-update power cut with one-command recovery.

Design center: a failed update must never brick a board. The bootloader
cannot write its own region; a torn image fails its own CRC and the device
stays reachable; every command is idempotent so retries are always safe.

## Quickstart

Flash an app from a Linux box:

```sh
cd host && cargo build --release -p updater-cli
./target/release/updater-cli --transport uart --dev /dev/ttyACM0 flash app.bin
```

`boot` starts the app, `info`/`echo` check liveness; other transports
(I2C, SPI, bit-banged GPIO), other hosts (Pico 2 W, ESP32) and the
debug-adapter `install` orchestrator are covered in
[docs/INTEGRATION.md](docs/INTEGRATION.md).

## Architecture

```
          host (Rust, no_std)                     device (C11)
  ┌───────────────────────────────┐      ┌────────────────────────────────┐
  │ updater-cli    Linux front end│      │ port — thin, audited:          │
  │ updater-eh     embedded-hal / │ wire │   avr_ea_twi · avr_ea_uart ·   │
  │                embedded-io    │ v1   │   rp2350_uart · yours          │
  │       │                       │◄────►│       │ 9-function contract    │
  │ updater-core   frame codec +  │ I2C  │ core — formally verified FSM,  │
  │                update session │ UART │   codec, CRC, boot gate        │
  └───────────────────────────────┘ SPI  └────────────────────────────────┘
                                    GPIO
```

One enforced wire contract binds the halves: the conformance suite drives the
shipped Rust client against the real C core over a simulated device, and the
golden vectors in [docs/PROTOCOL.md](docs/PROTOCOL.md) pin the bytes.

## Assurance

| Layer | Technology | Result (current tree) |
|---|---|---|
| Device core (C11) — deductive | Frama-C/WP 32.1 + Alt-Ergo 2.5.4 (pinned container): ACSL contracts over all 5 core units and the 9-function port contract, run on every push by [CI](.github/workflows/ci.yml) (`frama-c-proofs` job; full log uploaded as the `wp-report` artifact) | 534 goals, 0 unproven — **unbounded**: UB-freedom (RTE), flash confinement at every call site, `upd_handle`'s reply contract, the codec behaviors and the stream-link state invariant hold for the full `uint16_t` geometry range, not just a bounded model. Scope notes: [device/proofs/README.md](device/proofs/README.md) |
| Device core (C11) — bounded | ACSL contracts + CBMC 6.8.0 bounded model checking, 4 harnesses (RTE, flash-write confinement, jump gating, stream link) | 2,166 checks, 0 failed. Proven exhaustively over the small model: no UB for all inputs; no reachable flash write/erase outside the app region; jump-to-app reachable only through a passed full-image CRC; plus the strict wrap/narrowing arithmetic lints the WP gate scopes out (defined behavior) |
| Host core (Rust) | `#![forbid(unsafe_code)]`, `no_std`, zero deps + Kani model checking | 14 harnesses, 0 failures: frame codec, session validation and stream scanner panic-free for all inputs within modeled bounds |
| The contract between them | Conformance harness: the real Rust client drives the real C core (host-compiled, ASan/UBSan) | 3,584-cell exhaustive FSM table, power-cut sweep at all 57 flash operations (erases + writes) of an update, golden wire vectors, transactional + stream paths — all green |
| The ports (unproven by construction) | Kept thin + audited: every register access tabulated with datasheet citation and risk in a per-port `PORT_AUDIT.md`; disassembly checks; hard size gates | 3 ports shipped, 3 audits |
| Hardware | Live Pico 2 W over UART | Validated on silicon: install, INFO/ECHO, full flash + device VERIFY, BOOT, app-requested re-entry, and a real torn-update event (power pulled mid-flash): boot refused, one-command recovery. The validated build predates the interrupt-handoff fix (rp2350 PORT_AUDIT M6, disassembly-verified); on-board reinstall + app-heartbeat re-check and the POR auto-boot leg are pending the board's next service |

Honest scope: the proofs guarantee the specified properties of the core;
the spec itself and the port boundary remain human-reviewed territory —
that boundary is exactly what the PORT_AUDIT files and the hardware legs
cover. The WP and CBMC lanes verify the same ACSL-annotated sources and
split the claim set deliberately (unbounded UB-freedom + contracts vs
exhaustive small-model + strict arithmetic lints); the split and every
scoped-out goal are documented in
[device/proofs/README.md](device/proofs/README.md).

## Documentation

Start with the document that matches your role:

| You are | Read | Contents |
|---|---|---|
| Integrating the SDK into a product | [docs/INTEGRATION.md](docs/INTEGRATION.md) | Datasheet: capabilities/limits, per-target recipes (RPi, Pico/ESP32 hosts, RP2350/AVR-EA devices, app stub), gotchas |
| Porting the bootloader to a new MCU | [docs/PORTING.md](docs/PORTING.md) | The 9-function port contract, main-loop checklist, PORT_AUDIT template |
| Implementing the wire yourself | [docs/PROTOCOL.md](docs/PROTOCOL.md) | Normative wire spec: frames, commands, footer, entry model, transport bindings, golden vectors |
| Reproducing the proofs | [TOOLING.md](TOOLING.md), [device/proofs/README.md](device/proofs/README.md) | Proof toolchain inventory, prover fallback ladder, proof-lane scope |

Design history: the original design spec and implementation plan are not
shipped in-tree; they live in the maintainers' session archives.

## Build, test, prove

```sh
make all      # device core + host workspace
make test     # device unit tests; host tests + clippy -D warnings +
              # no_std thumbv8m build; conformance suite + sanitizer runner
make prove    # device core: CBMC (4 harnesses) + Frama-C/WP in docker
              # (framac/frama-c:32.1; skipped with a notice if absent);
              # host: cargo kani on updater-core
```

CI runs the Frama-C/WP proofs and the fast gates on every push and pull
request ([.github/workflows/ci.yml](.github/workflows/ci.yml)).

Port binaries build in their own lanes (cross-toolchains required):

```sh
make -C device/ports/avr_ea_twi     # avr-gcc; 4 KiB size gate + .init3 objdump gate
make -C device/ports/avr_ea_uart
cd device/ports/rp2350_uart && cmake -S . -B build -G Ninja \
    -DCMAKE_BUILD_TYPE=MinSizeRel && ninja -C build   # pico-sdk 2.1.1
```

## Project status: complete

This SDK is finished and stable by design. The wire protocol is frozen at
v1, the proofs cover the shipped core, and the conformance suite pins both
sides of the contract. Maintenance is limited to defect fixes that preserve
that contract; there is no feature roadmap and no contribution pipeline —
stability is the feature. If you need different behavior, please fork
rather than wait: the dual license exists so you can, and the conformance
harness and [docs/PORTING.md](docs/PORTING.md) travel with your fork as its
safety net.

Security: report vulnerabilities privately via
[GitHub security advisories](https://github.com/Poprdi/ota-updater-sdk/security/advisories/new).

## License

Copyright (c) 2026 Adrian Erlacher.

Dual-licensed under MIT OR Apache-2.0, at your option — see
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). This
copyright covers all first-party files in this repository, documentation
included. `device/ports/rp2350_uart/pico_sdk_import.cmake` is upstream
Raspberry Pi code and keeps its own BSD-3-Clause header.
