# Device proof lane — bounded model checking of the C core with CBMC.
# Prover probe order (TOOLING.md ladder): Kani-bundled CBMC -> system CBMC;
# when Frama-C lands it slots in as the full deductive leg (ACSL/WP) without
# restructuring — see README.md.
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
SRC := ../core/crc8.c ../core/crc32.c ../core/proto.c ../core/update.c

.PHONY: all clean
all:
ifeq ($(CBMC),)
	@echo "no prover available: install kani (bundles cbmc) or frama-c — see TOOLING.md"; exit 1
else
	$(CBMC) $(CHECKS) $(UNWIND) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_rte.c $(SRC)
	$(CBMC) $(CHECKS) $(UNWIND) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_confinement.c $(SRC)
	$(CBMC) $(CHECKS) $(UNWIND) $(SOLVER) -I../include -DPROOF_SMALL_MODEL harness_boot_gate.c $(SRC)
endif
clean:
	@true
