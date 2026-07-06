#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Adrian Erlacher
#
# Frama-C/WP deductive proof leg — device core, unbounded.
#
# Single source of truth: CI (.github/workflows/ci.yml, frama-c-proofs job)
# and the local lane (`make -C device prove-wp`, which wraps this script in
# the pinned container) both execute THIS file, so a passing local run is
# the CI run.
#
# Environment: Frama-C 32.1 (Germanium) + Alt-Ergo 2.5.4, i.e. the
# framac/frama-c:32.1 Docker image (Debian 12). Run it anywhere frama-c and
# alt-ergo of those versions are on PATH; the pinned container is simply
# the reproducible way to get them:
#
#   docker run --rm -v "$PWD:/work" -w /work framac/frama-c:32.1 \
#       device/proofs/run-wp.sh
#
# What is proven (see device/proofs/README.md, "Frama-C/WP leg"):
#   - absence of UNDEFINED BEHAVIOR (-wp-rte: memory access, signed
#     overflow, division, shift-width guards) over all five core units,
#     for unbounded inputs and the full uint16_t geometry range;
#   - every ACSL contract and loop annotation in core/ + the port contract
#     in include/updater/port.h: codec behaviors, session handler
#     contracts, the Invariant 1 confinement asserts at the flash call
#     sites, the link pump state invariant.
#
# Deliberately NOT enabled here: -warn-unsigned-overflow and the
# -warn-*-downcast lints. Unsigned wrap and unsigned narrowing are DEFINED
# C behavior, not runtime errors; the SDK's "compute wide, mask before any
# narrowing" discipline is a lint the CBMC lane already enforces
# exhaustively (--conversion-check --unsigned-overflow-check over the full
# u8 codec ranges, device/proofs/Makefile). WP's Alt-Ergo back end models
# lsl/lxor/lor as near-uninterpreted integer functions (cbits.mlw carries
# no upper-bound axioms), so those non-UB lint goals are structurally
# out of its reach — keeping them here would force rewriting shift/xor
# idioms purely to please the prover. Scope split is documented in
# device/proofs/README.md.
#
# Exit status: 0 iff EVERY goal is proven. Frama-C itself exits 0 with
# unproven goals left, so the gate below parses the WP summary; a missing
# summary is also a failure (a crash must not look like a pass).
set -euo pipefail

cd "$(dirname "$0")/../.."   # repo root, wherever the checkout lives

SRC="device/core/crc8.c
     device/core/crc32.c
     device/core/proto.c
     device/core/update.c
     device/core/link_stream.c"

LOG="${WP_LOG:-device/proofs/wp.log}"

# The image puts frama-c/alt-ergo on PATH via its `opam exec --`
# entrypoint for the `opam` user. GitHub Actions container jobs bypass the
# entrypoint and run steps as root, so resolve the opam switch bin
# directory ourselves when needed.
if ! command -v frama-c >/dev/null 2>&1; then
    for d in /home/opam/.opam/*/bin; do
        if [ -x "${d}/frama-c" ]; then
            export PATH="${d}:${PATH}"
            break
        fi
    done
fi

# The image also pre-detects Why3 provers only for the `opam` user. Under
# another user/HOME (Actions: root, HOME=/github/home), detect once.
if ! [ -f "${HOME}/.why3.conf" ]; then
    mkdir -p "${HOME}"
    why3 config detect
fi

# -wp-par: Alt-Ergo processes; nproc is fine both locally and on runners.
frama-c \
    -cpp-extra-args="-Idevice/include" \
    -wp -wp-rte \
    -wp-prover alt-ergo \
    -wp-timeout 30 \
    -wp-par "$(nproc)" \
    ${SRC} 2>&1 | tee "${LOG}"

# Gate: the "[wp] Proved goals: N / M" summary must exist and N == M.
awk '
    /\[wp\] Proved goals:/ {
        seen = 1
        # line: "[wp] Proved goals:  183 / 183"
        n = $(NF - 2); m = $NF
        if (n != m) {
            printf "run-wp.sh: FAIL — %d of %d goals unproven\n", m - n, m
            exit 1
        }
        printf "run-wp.sh: OK — all %d goals proven\n", m
    }
    END { if (!seen) { print "run-wp.sh: FAIL — no WP summary found (Frama-C error?)"; exit 1 } }
' "${LOG}"
