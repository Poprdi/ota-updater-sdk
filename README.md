# updater-sdk

A project-agnostic OTA firmware update SDK for microcontrollers behind a
master CPU. Two halves bound by one enforced wire contract:

- **Device:** a formally verified, portable C11 bootloader core plus thin
  audited per-MCU ports (shipped: AVR64EA28 over I2C and UART; RP2350 /
  Pico 2 W over UART).
- **Host:** `no_std`, zero-alloc Rust — `updater-core` has zero
  dependencies, `updater-eh` adds only the `embedded-hal`/`embedded-io`
  trait crates — so the identical update code runs
  on a Raspberry Pi, a Pico 2 W or an ESP32 — plus `updater-cli` for
  Linux (I2C, UART, SPI, bit-banged GPIO backends and a debug-adapter
  `install` orchestrator).

Design center: a failed update must never brick a board. The bootloader
cannot write its own region; a torn image fails its own CRC and the
device stays reachable; every command is idempotent so retries are
always safe.

## Assurance

| Layer | Technology | Result (current tree) |
|---|---|---|
| Device core (C11) | ACSL contracts + CBMC 6.8.0 bounded model checking, 4 harnesses (RTE, flash-write confinement, jump gating, stream link) | 2,253 checks, 0 failed. Proven: no UB for all inputs; no reachable flash write/erase outside the app region; jump-to-app reachable only through a passed full-image CRC |
| Host core (Rust) | `#![forbid(unsafe_code)]`, `no_std`, zero deps + Kani model checking | 14 harnesses, 0 failures: frame codec, session validation and stream scanner panic-free for all inputs within modeled bounds |
| The contract between them | Conformance harness: the real Rust client drives the real C core (host-compiled, ASan/UBSan) | 3,584-cell exhaustive FSM table, power-cut sweep at all 57 flash operations (erases + writes) of an update, golden wire vectors, transactional + stream paths — all green |
| The ports (unproven by construction) | Kept thin + audited: every register access tabulated with datasheet citation and risk in a per-port `PORT_AUDIT.md`; disassembly checks; hard size gates | 3 ports shipped, 3 audits |
| Hardware | Live Pico 2 W over UART | Validated on silicon: install, INFO/ECHO, full flash + device VERIFY, BOOT, app-requested re-entry, and a real torn-update event (power pulled mid-flash): boot refused, one-command recovery. The validated build predates the interrupt-handoff fix (rp2350 PORT_AUDIT M6, disassembly-verified); on-board reinstall + app-heartbeat re-check and the POR auto-boot leg are pending the board's next service |

Honest scope: the proofs guarantee the specified properties of the core;
the spec itself and the port boundary remain human-reviewed territory —
that boundary is exactly what the PORT_AUDIT files and the hardware legs
cover. Frama-C/WP deductive proofs are the documented upgrade path when
the tool is present (`TOOLING.md` ladder); today's device-core lane is
CBMC against the same ACSL-annotated sources.

## Documentation

| Document | Contents |
|---|---|
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | Normative wire spec: frames, commands, footer, entry model, transport bindings, golden vectors |
| [docs/INTEGRATION.md](docs/INTEGRATION.md) | Datasheet: capabilities/limits, per-target recipes (RPi, Pico/ESP32 hosts, RP2350/AVR-EA devices, app stub), gotchas |
| [docs/PORTING.md](docs/PORTING.md) | New-MCU port procedure: the 9-function contract, main-loop checklist, PORT_AUDIT template |
| [TOOLING.md](TOOLING.md) | Proof toolchain inventory + prover fallback ladder |

Design history: the original design spec and implementation plan are not
shipped in-tree; they live in the maintainers' session archives.

## Build, test, prove

```sh
make all      # device core + host workspace
make test     # device unit tests; host tests + clippy -D warnings +
              # no_std thumbv8m build; conformance suite + sanitizer runner
make prove    # CBMC on the device core (4 harnesses) + cargo kani on updater-core
```

Port binaries build in their own lanes (cross-toolchains required):

```sh
make -C device/ports/avr_ea_twi     # avr-gcc; 4 KiB size gate + .init3 objdump gate
make -C device/ports/avr_ea_uart
cd device/ports/rp2350_uart && cmake -S . -B build -G Ninja \
    -DCMAKE_BUILD_TYPE=MinSizeRel && ninja -C build   # pico-sdk 2.1.1
```

## Quickstart

Flash an app from a Linux box (details and other targets:
[docs/INTEGRATION.md](docs/INTEGRATION.md)):

```sh
cd host && cargo build --release -p updater-cli
updater-cli --transport uart --dev /dev/ttyACM0 info
updater-cli --transport uart --dev /dev/ttyACM0 flash app.bin
updater-cli --transport uart --dev /dev/ttyACM0 boot
```

## License

Dual-licensed under MIT OR Apache-2.0, at your option — see LICENSE-MIT and LICENSE-APACHE.
