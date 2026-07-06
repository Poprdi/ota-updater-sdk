# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Adrian Erlacher
#
# Device proof lane — two legs (TOOLING.md ladder):
#   cbmc: bounded model checking of the C core (Kani-bundled CBMC -> system
#         CBMC probe order).
#   wp:   Frama-C/WP + Alt-Ergo deductive proofs (unbounded), via the pinned
#         framac/frama-c container — single driver device/proofs/run-wp.sh,
#         the same file CI executes. Skips with a notice when docker or the
#         image is absent: `make prove` runs every prover available on THIS
#         machine (root Makefile contract), and the CI frama-c-proofs job
#         runs the leg unconditionally, so a local skip never silently
#         drops the property.
CBMC := $(shell ls $$HOME/.kani/kani-*/bin/cbmc 2>/dev/null | head -1)
ifeq ($(CBMC),)
CBMC := $(shell command -v cbmc)
endif
# kissat ships in the same Kani bundle as cbmc; the boot-gate harness needs
# it — CBMC's built-in SAT back-end does not finish the CRC32-determinism
# equivalence (two 48-byte CRC sweeps over the same nondet flash) in
# reasonable time, kissat solves it in seconds. Falls back to the built-in
# solver if no kissat sits next to the chosen cbmc.
KISSAT := $(wildcard $(dir $(CBMC))kissat)
ifneq ($(KISSAT),)
SOLVER := --external-sat-solver $(KISSAT)
endif
CHECKS := --bounds-check --pointer-check --conversion-check \
          --div-by-zero-check --signed-overflow-check --unsigned-overflow-check \
          --unwinding-assertions
# PROOF_SMALL_MODEL (page_size <= 16, app_pages <= 4) keeps the model small;
# the properties are quantified over the info struct the harness makes
# nondet within bounds. --unwind 70 dominates every loop bound reachable
# under that model (largest: 64-byte flash fill / 48-byte CRC32 sweep).
UNWIND := --unwind 70
# The link harness bounds its model to stream/buf <= 10 (see harness_link.c),
# so 12 dominates every loop it reaches (largest: the 11-iteration pump).
# Reusing 70 would cube the pump x parse x CRC-8 nesting into a formula the
# solver chews on for minutes; 12 keeps the lane fast and
# --unwinding-assertions still proves the dominance claim.
UNWIND_LINK := --unwind 12
SRC := ../core/crc8.c ../core/crc32.c ../core/proto.c ../core/update.c
# link harness draws only codec + link: update.c would drag the port
# contract in for code the harness never reaches
LINK_SRC := ../core/crc8.c ../core/proto.c ../core/link_stream.c

# Pinned proof container (see run-wp.sh header for the version rationale).
FRAMAC_IMAGE := framac/frama-c:32.1

.PHONY: all cbmc wp clean
all: cbmc wp

cbmc:
ifeq ($(CBMC),)
	@echo "no prover available: install kani (bundles cbmc) or frama-c — see TOOLING.md"; exit 1
else
	$(CBMC) $(CHECKS) $(UNWIND) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_rte.c $(SRC)
	$(CBMC) $(CHECKS) $(UNWIND) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_confinement.c $(SRC)
	$(CBMC) $(CHECKS) $(UNWIND) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_boot_gate.c $(SRC)
	$(CBMC) $(CHECKS) $(UNWIND_LINK) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_link.c $(LINK_SRC)
endif

wp:
	@if command -v docker >/dev/null 2>&1 \
	    && docker image inspect $(FRAMAC_IMAGE) >/dev/null 2>&1; then \
		echo "== Frama-C/WP leg ($(FRAMAC_IMAGE)) =="; \
		docker run --rm -v "$$(cd ../.. && pwd):/work" -w /work \
		    $(FRAMAC_IMAGE) device/proofs/run-wp.sh; \
	else \
		echo "notice: Frama-C/WP leg SKIPPED — docker or the $(FRAMAC_IMAGE) image is absent."; \
		echo "        docker pull $(FRAMAC_IMAGE)   # ~1 GB, once; CI always runs this leg"; \
	fi

clean:
	rm -f wp.log
