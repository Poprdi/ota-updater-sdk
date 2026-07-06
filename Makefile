# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Adrian Erlacher
#
# all: build everything buildable on this machine (device core host-lib, host workspace)
# test: device unit tests + host tests + conformance (incl. sanitizer runner)
# prove: every prover available on this machine; fails if none can run
SUBDIRS_BUILD = device host
.PHONY: all test prove clean
all:
	$(MAKE) -C device
	cd host && cargo build --workspace --all-features
test:
	$(MAKE) -C device test
	cd host && cargo test --workspace --all-features
	cd host && cargo clippy --workspace --all-targets -- -D warnings
	cd host && cargo build -p updater-core -p updater-eh --target thumbv8m.main-none-eabihf  # structural no_std proof
	cd conformance && cargo test
	$(MAKE) -C conformance/casan run
prove:
	$(MAKE) -C device prove
	cd host && cargo kani -p updater-core
clean:
	$(MAKE) -C device clean; cd host && cargo clean; cd conformance && cargo clean; $(MAKE) -C conformance/casan clean
