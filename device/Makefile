# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Adrian Erlacher

CC        := cc
CFLAGS    := -std=c11 -O1 -g -Wall -Wextra -Werror -pedantic -Wconversion -Iinclude
BUILD     := build
CORE_SRC  := $(wildcard core/*.c)
# Transport-pump skeletons are portable C (their whole point) and must stay
# warning-clean under this host lane; linking them into every test binary
# enforces that continuously. They define no port_* or main symbols.
SKEL_SRC  := $(wildcard ports/skeletons/*.c)
TEST_SRC  := $(wildcard test/test_*.c)
TEST_BINS := $(patsubst test/%.c,$(BUILD)/%,$(TEST_SRC))

.PHONY: all test prove prove-wp clean
all: test

# Every test binary links all of core/, and core/update.c calls the port
# contract — so the fake port double (test-only; must not live in core/)
# is linked into every test binary to satisfy those symbols.
$(BUILD)/%: test/%.c test/fake_port.c $(CORE_SRC) $(SKEL_SRC)
	@mkdir -p $(BUILD)
	$(CC) $(CFLAGS) -Itest -Iports/skeletons $^ -o $@

test: $(TEST_BINS)
	@fail=0; for t in $(TEST_BINS); do echo "== $$t =="; $$t || fail=1; done; exit $$fail

# prove = both proof legs: CBMC (bounded) always; Frama-C/WP (deductive,
# docker) when the pinned container is available, else a notice + skip.
# prove-wp runs just the WP leg (e.g. right after editing ACSL).
prove:
	$(MAKE) -C proofs
prove-wp:
	$(MAKE) -C proofs wp

clean:
	rm -rf $(BUILD); $(MAKE) -C proofs clean 2>/dev/null || true
