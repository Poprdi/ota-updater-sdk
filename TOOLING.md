# Proof Toolchain Inventory

Recorded 2026-07-03 on Ubuntu 26.04 LTS (x86_64), user-local installs only (no sudo).
Frama-C leg added 2026-07-06 (containerized — still no sudo, no opam tree to maintain).

## What is installed

| Tool | Status | Version | Location |
|------|--------|---------|----------|
| Kani (`cargo kani`) | **installed** | kani-verifier 0.67.0 (`cargo-kani 0.67.0`) | `~/.cargo/bin/cargo-kani`, bundle in `~/.kani/kani-0.67.0/` |
| CBMC (bundled with Kani) | **installed** | 6.8.0 (cbmc-6.8.0) | `~/.kani/kani-0.67.0/bin/cbmc` |
| kissat SAT solver (bundled) | installed | 4.0.1 | `~/.kani/kani-0.67.0/bin/kissat` |
| Kani pinned Rust toolchain | installed | nightly-2025-11-21 (rustc 1.93.0-nightly) | via rustup, pulled by `cargo kani setup` |
| Frama-C + Alt-Ergo (containerized) | **installed** | Frama-C 32.1 (Germanium), Alt-Ergo 2.5.4 | Docker image `framac/frama-c:32.1` (Debian 12; official upstream image, version-pinned) |
| CBMC (system-wide) | **absent** | — | `command -v cbmc` empty |
| Frama-C (native) | **absent** | — | `command -v frama-c` empty — the container IS the install; a native opam Frama-C works too if it matches 32.1/Alt-Ergo 2.5.4 |

Install methods:
- Kani: `cargo install kani-verifier --locked && cargo kani setup`
  (setup downloaded the prebuilt `kani-0.67.0-x86_64-unknown-linux-gnu.tar.gz` bundle and
  completed `[5/5] Successfully completed Kani first-time setup.`).
- Frama-C: `docker pull framac/frama-c:32.1` (~1 GB, once). Pinned to the
  release tag — `32.1` was current stable at adoption; the `dev` tags float
  and would let the meaning of "proven" drift between runs.

Other bundled binaries in `~/.kani/kani-0.67.0/bin/`: `goto-analyzer`, `goto-cc`,
`goto-instrument`, `kani-compiler`, `kani-cov`, `kani-driver`.

Note: `cargo kani ...` resolves the subcommand from `~/.cargo/bin` even when that
directory is not on `PATH`; invoking the bare `kani`/`cbmc` binaries directly requires
adding `~/.cargo/bin` respectively `~/.kani/kani-0.67.0/bin` to `PATH`.

## Consequence for `make prove` today

- **Rust host (`host/`):** `cargo kani` runs the Kani harnesses against
  `updater-core` using the bundled CBMC 6.8.0 backend.
- **C device core (`device/`):** BOTH legs run.
  - CBMC (bounded): `make -C device prove` probes the Kani-bundled binary
    (`device/proofs/Makefile`) — 4 harnesses, small-model exhaustive.
  - Frama-C/WP (deductive, unbounded): the same target runs
    `device/proofs/run-wp.sh` in the pinned container when docker + the
    image are present, and skips with a notice otherwise (CI always runs
    it — see below). One-liner, identical to what CI executes:

    ```sh
    docker run --rm -v "$PWD:/work" -w /work framac/frama-c:32.1 \
        device/proofs/run-wp.sh          # or: make -C device prove-wp
    ```

## CI (`.github/workflows/ci.yml`)

Every push / pull request to master runs:
- `frama-c-proofs`: `run-wp.sh` in `framac/frama-c:32.1` — fails on any
  unproven WP goal, uploads the full log as the `wp-report` artifact;
- `fast-gates`: device unit tests, both AVR bootloader builds (size +
  .init3 gates), host workspace tests + clippy + thumbv8m no_std builds,
  conformance suite.
The CBMC/Kani lanes and the rp2350 build stay local-only for now (toolchain
download weight); the workflow header documents the extension path for each.

## Ladder (normative)

For the C core the two legs are complementary, and `make -C device prove`
runs whatever is present, in this order of preference:

1. **Frama-C/WP + Alt-Ergo** (containerized, pinned): unbounded deductive
   proofs of the ACSL contracts + UB-freedom (RTE). Runs in CI on every
   push, so it is never silently absent from the project even when skipped
   locally.
2. **CBMC** (Kani bundle or system): bounded, exhaustive over the small
   model; also carries the strict non-UB arithmetic lints
   (`--conversion-check`, `--unsigned-overflow-check`) that are scoped out
   of the WP gate (defined behavior; see `device/proofs/README.md`).
3. Neither present: the target fails listing the missing provers.
