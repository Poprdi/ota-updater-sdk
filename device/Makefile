CC        := cc
CFLAGS    := -std=c11 -O1 -g -Wall -Wextra -Werror -pedantic -Wconversion -Iinclude
BUILD     := build
CORE_SRC  := $(wildcard core/*.c)
TEST_SRC  := $(wildcard test/test_*.c)
TEST_BINS := $(patsubst test/%.c,$(BUILD)/%,$(TEST_SRC))

.PHONY: all test prove clean
all: test

# Every test binary links all of core/, and core/update.c calls the port
# contract — so the fake port double (test-only; must not live in core/)
# is linked into every test binary to satisfy those symbols.
$(BUILD)/%: test/%.c test/fake_port.c $(CORE_SRC)
	@mkdir -p $(BUILD)
	$(CC) $(CFLAGS) -Itest $^ -o $@

test: $(TEST_BINS)
	@fail=0; for t in $(TEST_BINS); do echo "== $$t =="; $$t || fail=1; done; exit $$fail

prove:
	$(MAKE) -C proofs

clean:
	rm -rf $(BUILD); $(MAKE) -C proofs clean 2>/dev/null || true
