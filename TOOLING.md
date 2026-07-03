# Proof Toolchain Inventory

Recorded 2026-07-03 on Ubuntu 26.04 LTS (x86_64), user-local installs only (no sudo).

## What is installed

| Tool | Status | Version | Location |
|------|--------|---------|----------|
| Kani (`cargo kani`) | **installed** | kani-verifier 0.67.0 (`cargo-kani 0.67.0`) | `~/.cargo/bin/cargo-kani`, bundle in `~/.kani/kani-0.67.0/` |
| CBMC (bundled with Kani) | **installed** | 6.8.0 (cbmc-6.8.0) | `~/.kani/kani-0.67.0/bin/cbmc` |
| kissat SAT solver (bundled) | installed | 4.0.1 | `~/.kani/kani-0.67.0/bin/kissat` |
| Kani pinned Rust toolchain | installed | nightly-2025-11-21 (rustc 1.93.0-nightly) | via rustup, pulled by `cargo kani setup` |
| CBMC (system-wide) | **absent** | — | `command -v cbmc` empty |
| Frama-C | **absent** | — | `command -v frama-c` empty |

Install method: `cargo install kani-verifier --locked && cargo kani setup`
(setup downloaded the prebuilt `kani-0.67.0-x86_64-unknown-linux-gnu.tar.gz` bundle and
completed `[5/5] Successfully completed Kani first-time setup.`).

Other bundled binaries in `~/.kani/kani-0.67.0/bin/`: `goto-analyzer`, `goto-cc`,
`goto-instrument`, `kani-compiler`, `kani-cov`, `kani-driver`.

Note: `cargo kani ...` resolves the subcommand from `~/.cargo/bin` even when that
directory is not on `PATH`; invoking the bare `kani`/`cbmc` binaries directly requires
adding `~/.cargo/bin` respectively `~/.kani/kani-0.67.0/bin` to `PATH`.

## Consequence for `make prove` today

- **Rust host (`host/`):** fully provable today — `cargo kani` runs the Kani harnesses
  against `updater-core` using the bundled CBMC 6.8.0 backend.
- **C device core (`device/`):** the Frama-C/WP deductive leg **cannot run** (Frama-C
  absent). The CBMC leg **can run** by pointing the device proof driver at the bundled
  binary `~/.kani/kani-0.67.0/bin/cbmc` (bounded model checking of the assertions in
  `device/proofs/`).
- When Frama-C is installed later (e.g. via opam, still no sudo required), `make prove`
  additionally discharges the full ACSL/WP deductive proofs for the C core; nothing in
  the Makefiles needs restructuring — the driver just detects the prover.

## Fallback ladder (normative)

Frama-C/WP (full deductive proofs, preferred) → CBMC (bounded model checking of the
same properties via the assertions in `device/proofs/`) → build fails listing missing
provers.
